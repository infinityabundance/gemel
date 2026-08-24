//! The content layer (STORAGE.md §6, OBJECT_MODEL.md §6.2–§6.4).
//!
//! This module maps between the working tree and canonical content
//! (blobs/trees/states), computes deterministic tree deltas, synthesizes
//! Operation objects from deltas, and provides textual (Myers) diffing.

use crate::family::Family;
use crate::gid::Gid;
use crate::ignore::Ignore;
use crate::store::{Error, Repo};
use crate::value::{Field, Object, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Result of snapshotting a working tree into a canonical state.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub state: Gid,
    pub files: usize,
    pub symlinks: usize,
    pub bytes: u64,
    /// Whether the captured state was observationally coherent: every
    /// entry's (size, mtime) was re-verified after reading, so the captured
    /// bytes correspond to a single observed moment (brief §34, forensic
    /// provenance). `false` means the working tree was mutating during
    /// capture and the state may not have existed at any single instant.
    pub coherent: bool,
    /// Capture attempts before coherence was achieved (or the cap reached).
    pub attempts: u32,
}

/// Maximum capture attempts before recording an incoherent state.
pub const MAX_CAPTURE_ATTEMPTS: u32 = 3;

/// One entry observed during capture (for coherence verification).
#[derive(Debug, Clone)]
struct CapturedEntry {
    path: String,
    is_dir: bool,
    size: u64,
    mtime_ns: i64,
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Snapshots the working tree at `root` into a canonical state (inserts all
/// blobs, trees, and the state object).
///
/// Coherence protocol: entries are recorded with their (size, mtime) as they
/// are read; afterwards every entry is re-statted. A mismatch means the
/// working tree mutated mid-capture — retry up to `MAX_CAPTURE_ATTEMPTS`;
/// if the tree is still unstable, the state is recorded with
/// `capture.coherent = false` rather than silently claiming a coherent
/// observation (brief §34: a State must know whether it was observationally
/// coherent).
pub fn build_state(repo: &Repo, root: &Path, ignore: &Ignore) -> Result<Snapshot, Error> {
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let mut stats = Snapshot {
            state: Gid::new(Family::State, [0u8; 32]),
            files: 0,
            symlinks: 0,
            bytes: 0,
            coherent: false,
            attempts,
        };
        let mut log: Vec<CapturedEntry> = Vec::new();
        let root_tree = build_tree(repo, root, "", ignore, &mut stats, &mut log)?;
        let coherent = verify_capture(root, &log)?;
        if coherent || attempts >= MAX_CAPTURE_ATTEMPTS {
            stats.coherent = coherent;
            // The capture record (state extension 0x80, Raw) is the coherence
            // attestation: whether the bytes correspond to a single observed
            // moment, and how many attempts it took. Canonical JSON with
            // sorted keys — deterministic; identical stable captures
            // deduplicate to the same state identity. No timestamp.
            let capture_json = serde_json::json!({ "coherent": coherent, "attempts": attempts });
            let capture_bytes =
                serde_json::to_vec(&capture_json).map_err(|e| Error::Invalid(e.to_string()))?;
            let state = Object::fields(
                Family::State,
                vec![
                    Field::new(0x01, Value::Gid(root_tree)),
                    Field::new(0x80, Value::Raw(capture_bytes)),
                ],
            );
            stats.state = repo.insert_object(&state)?;
            return Ok(stats);
        }
    }
}

/// Re-stats every captured entry and reports whether (size, mtime, type)
/// still match what was observed during the read walk.
fn verify_capture(root: &Path, log: &[CapturedEntry]) -> Result<bool, Error> {
    for e in log {
        let md = match std::fs::symlink_metadata(root.join(&e.path)) {
            Ok(m) => m,
            Err(_) => return Ok(false), // disappeared mid-capture
        };
        if md.is_dir() != e.is_dir || md.len() != e.size || mtime_ns(&md) != e.mtime_ns {
            return Ok(false);
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Flat file map → state (used by reconciliation merges)
// ---------------------------------------------------------------------------

/// The in-memory identity of an object: canonical bytes → BLAKE3 digest,
/// without publishing. Used by read-only plans (AGENT_PROTOCOL.md §5.10).
pub fn object_identity(repo: &Repo, obj: &Object) -> Result<Gid, Error> {
    let bytes = crate::encode::encode_object(obj, &repo.limits())?;
    let digest = crate::hash::object_id_bytes(&bytes);
    Ok(Gid::new(obj.family, digest))
}

/// Computes the would-be identities of the root tree and state for a flat
/// `path -> (mode, blob)` file map, without publishing any object.
pub fn state_identity_from_files(
    repo: &Repo,
    files: &std::collections::BTreeMap<String, (u64, Gid)>,
) -> Result<(Gid, Gid), Error> {
    let tree = build_tree_from_files(repo, files, "")?;
    let tree_id = object_identity(repo, &tree)?;
    let state = Object::fields(Family::State, vec![Field::new(0x01, Value::Gid(tree_id))]);
    let state_id = object_identity(repo, &state)?;
    Ok((tree_id, state_id))
}

/// Publishes a state from a flat `path -> (mode, blob)` file map
/// (reconciliation merge results). Blobs must already exist; trees and the
/// state object are inserted.
pub fn build_state_from_files(
    repo: &Repo,
    files: &std::collections::BTreeMap<String, (u64, Gid)>,
) -> Result<Gid, Error> {
    let tree = build_tree_from_files(repo, files, "")?;
    let tree_id = repo.insert_object(&tree)?;
    let state = Object::fields(Family::State, vec![Field::new(0x01, Value::Gid(tree_id))]);
    repo.insert_object(&state)
}

/// Recursively builds a tree object (in memory) for the entries under
/// `prefix`. Fail-closed on a path that is both a file and a directory.
fn build_tree_from_files(
    repo: &Repo,
    files: &std::collections::BTreeMap<String, (u64, Gid)>,
    prefix: &str,
) -> Result<Object, Error> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in files.keys() {
        let rest = match prefix {
            "" => path.as_str(),
            p => path
                .strip_prefix(&format!("{p}/"))
                .ok_or_else(|| Error::Path(format!("{path} outside prefix {p}")))?,
        };
        if let Some(seg) = rest.split('/').next() {
            names.insert(seg.to_string());
        }
    }
    let mut entries: Vec<(String, u64, Gid)> = Vec::new();
    for name in names {
        let child_prefix = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let is_dir = files
            .keys()
            .any(|p| p.starts_with(&format!("{child_prefix}/")));
        if is_dir {
            if files.contains_key(&child_prefix) {
                return Err(Error::Path(format!(
                    "{child_prefix} is both a file and a directory"
                )));
            }
            let subtree = build_tree_from_files(repo, files, &child_prefix)?;
            let id = object_identity(repo, &subtree)?;
            entries.push((name, 0o040000, id));
        } else {
            let (mode, blob) = files
                .get(&child_prefix)
                .ok_or_else(|| Error::Path(format!("missing file entry {child_prefix}")))?;
            entries.push((name, *mode, *blob));
        }
    }
    build_tree_object(entries)
}

/// Recursively builds a tree object for `dir` (canonical relative path `rel`),
/// recording every observed entry (path, type, size, mtime) for the capture
/// coherence verification.
fn build_tree(
    repo: &Repo,
    dir: &Path,
    rel: &str,
    ignore: &Ignore,
    stats: &mut Snapshot,
    log: &mut Vec<CapturedEntry>,
) -> Result<Gid, Error> {
    let mut entries: Vec<(String, u64, Gid)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == crate::store::META_DIR {
            continue; // repository metadata is never captured
        }
        let md = entry.metadata()?;
        let is_dir = md.is_dir();
        let full_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if is_meta_path(&full_rel) {
            continue; // repository configuration, never content (STORAGE.md §6)
        }
        if ignore.is_ignored(&full_rel, is_dir) {
            continue;
        }
        log.push(CapturedEntry {
            path: full_rel.clone(),
            is_dir,
            size: md.len(),
            mtime_ns: mtime_ns(&md),
        });
        names.push(name.clone());
        entries.push((name, 0, Gid::new(Family::Blob, [0u8; 32])));
    }
    // Sort by name bytes for canonical order.
    names.sort();
    let mut sorted: Vec<(String, u64, Gid)> = Vec::new();
    for name in &names {
        let entry = std::fs::symlink_metadata(dir.join(name))?;
        let ft = entry.file_type();
        let full_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if ft.is_dir() {
            let subtree = build_tree(repo, &dir.join(name), &full_rel, ignore, stats, log)?;
            sorted.push((name.clone(), 0o040000, subtree));
        } else if ft.is_symlink() {
            let target = std::fs::read_link(dir.join(name))?;
            #[cfg(unix)]
            let bytes: Vec<u8> = {
                use std::os::unix::ffi::OsStrExt;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().as_bytes().to_vec();
            let blob = Object::blob(bytes);
            let blob_id = repo.insert_object(&blob)?;
            stats.symlinks += 1;
            stats.bytes += blob_bytes(&blob).len() as u64;
            sorted.push((name.clone(), 0o120000, blob_id));
        } else if ft.is_file() {
            let bytes = std::fs::read(dir.join(name))?;
            if bytes.len() as u64 > repo.limits().max_object_bytes {
                return Err(Error::Limit {
                    kind: "object size",
                    limit: repo.limits().max_object_bytes,
                    found: bytes.len() as u64,
                });
            }
            let mode = if is_executable(&entry) {
                0o100755
            } else {
                0o100644
            };
            let blob = Object::blob(bytes);
            stats.bytes += blob_bytes(&blob).len() as u64;
            stats.files += 1;
            let blob_id = repo.insert_object(&blob)?;
            sorted.push((name.clone(), mode, blob_id));
        } else {
            return Err(Error::Unsupported(format!(
                "{}: special file type",
                dir.join(name).display()
            )));
        }
    }
    let tree = build_tree_object(sorted)?;
    repo.insert_object(&tree)
}

fn build_tree_object(entries: Vec<(String, u64, Gid)>) -> Result<Object, Error> {
    let mut items = Vec::with_capacity(entries.len());
    let mut prev: Option<String> = None;
    for (name, mode, target) in entries {
        if let Some(p) = &prev {
            if name.as_str() <= p.as_str() {
                return Err(Error::Path(format!(
                    "tree entries out of order at {name:?}"
                )));
            }
        }
        prev = Some(name.clone());
        items.push(Value::Record(vec![
            Field::new(0x01, Value::Str(name)),
            Field::new(0x02, Value::U(mode)),
            Field::new(0x03, Value::Gid(target)),
        ]));
    }
    Ok(Object::fields(
        Family::Tree,
        vec![Field::new(0x01, Value::Array(items))],
    ))
}

/// The flattened file map of a state: `path -> (mode, blob)`, deterministic
/// order.
pub fn state_files(repo: &Repo, state: &Gid) -> Result<BTreeMap<String, (u64, Gid)>, Error> {
    let st = repo.load(state)?;
    if st.family != Family::State {
        return Err(Error::Invalid(format!("{state} is not a state object")));
    }
    let tree =
        find_gid(&st, 0x01).ok_or_else(|| Error::Invalid("state has no root_tree".into()))?;
    let flat = flatten_tree(repo, &tree)?;
    Ok(flat.into_iter().collect())
}

fn blob_bytes(blob: &Object) -> &[u8] {
    blob.blob_bytes().unwrap_or(&[])
}

#[cfg(unix)]
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_md: &std::fs::Metadata) -> bool {
    false
}

// ---------------------------------------------------------------------------
// State → working tree
// ---------------------------------------------------------------------------

/// Materializes `state` into `target` with **overlay** semantics: existing
/// files are overwritten; files not in the state are left untouched.
/// This is the conservative primitive (like git checkout leaving untracked
/// files alone). For exact reconstruction use [`materialize_exact`].
pub fn materialize_overlay(repo: &Repo, state: &Gid, target: &Path) -> Result<(), Error> {
    let st = repo.load(state)?;
    if st.family != Family::State {
        return Err(Error::Invalid(format!("{state} is not a state object")));
    }
    let root_tree =
        find_gid(&st, 0x01).ok_or_else(|| Error::Invalid("state has no root_tree".into()))?;
    if !target.exists() {
        std::fs::create_dir_all(target)?;
    }
    write_tree(repo, &root_tree, target)
}

/// Materializes `state` into `target` with **exact** semantics: after
/// writing the tree, entries not present in the state are removed, so the
/// target's tracked content equals the state. Protected from deletion:
/// repository metadata (`.gemel`), an enclosing Git metadata directory
/// (`.git`), the root `.gitignore` (configuration, never content —
/// STORAGE.md §6), and any path the ignore rules declare non-content
/// (unrecorded work is never silently destroyed). Returns the removed paths.
pub fn materialize_exact(
    repo: &Repo,
    state: &Gid,
    target: &Path,
    ignore: &Ignore,
) -> Result<Vec<String>, Error> {
    materialize_overlay(repo, state, target)?;
    let expected: std::collections::HashSet<String> =
        state_files(repo, state)?.into_keys().collect();
    let mut removed = Vec::new();
    remove_unexpected(repo, target, "", &expected, ignore, &mut removed)?;
    Ok(removed)
}

/// The paths an exact materialization *would* remove, without touching the
/// filesystem. Lets callers gate destruction of unrecorded work before any
/// mutation happens.
pub fn exact_removals(
    repo: &Repo,
    state: &Gid,
    target: &Path,
    ignore: &Ignore,
) -> Result<Vec<String>, Error> {
    let expected: std::collections::HashSet<String> =
        state_files(repo, state)?.into_keys().collect();
    let mut removed = Vec::new();
    collect_removals(target, "", &expected, ignore, &mut removed)?;
    Ok(removed)
}

/// The removal decision for one entry (shared by the exact materializer and
/// its read-only planner).
fn should_remove(
    full_rel: &str,
    is_dir: bool,
    expected: &std::collections::HashSet<String>,
    ignore: &Ignore,
) -> bool {
    // Protected: repository metadata, enclosing Git metadata, and the root
    // .gitignore (configuration, never content).
    if full_rel == crate::store::META_DIR || full_rel == ".git" || is_meta_path(full_rel) {
        return false;
    }
    if expected.contains(full_rel) {
        return false;
    }
    // Protected: ignored paths are declared non-content (unrecorded work /
    // artifacts by policy).
    if ignore.is_ignored(full_rel, is_dir) {
        return false;
    }
    true
}

/// Recursively removes entries not in `expected`, protecting metadata,
/// configuration, and ignored paths (see [`materialize_exact`]).
fn remove_unexpected(
    repo: &Repo,
    dir: &Path,
    rel: &str,
    expected: &std::collections::HashSet<String>,
    ignore: &Ignore,
    removed: &mut Vec<String>,
) -> Result<(), Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let full_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let path = entry.path();
        if expected.contains(&full_rel) {
            if is_dir {
                remove_unexpected(repo, &path, &full_rel, expected, ignore, removed)?;
            }
            continue;
        }
        if should_remove(&full_rel, is_dir, expected, ignore) {
            let _ = repo;
            if is_dir {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            removed.push(full_rel);
        }
    }
    Ok(())
}

/// Read-only variant of [`remove_unexpected`]: records what would be
/// removed without deleting anything.
fn collect_removals(
    dir: &Path,
    rel: &str,
    expected: &std::collections::HashSet<String>,
    ignore: &Ignore,
    removed: &mut Vec<String>,
) -> Result<(), Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let full_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if expected.contains(&full_rel) {
            if is_dir {
                collect_removals(&entry.path(), &full_rel, expected, ignore, removed)?;
            }
            continue;
        }
        if should_remove(&full_rel, is_dir, expected, ignore) {
            removed.push(full_rel);
        }
    }
    Ok(())
}

fn write_tree(repo: &Repo, tree: &Gid, dir: &Path) -> Result<(), Error> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    let t = repo.load(tree)?;
    let tfs = t.field_sequence().unwrap_or(&[]);
    let entries = match find_value(tfs, 0x01) {
        Some(Value::Array(items)) => items,
        _ => return Err(Error::Invalid("tree has no entries".into())),
    };
    for item in entries {
        let record = match item {
            Value::Record(fields) => fields,
            _ => return Err(Error::Invalid("tree entry is not a record".into())),
        };
        let name = match find_value(record, 0x01) {
            Some(Value::Str(s)) => s.clone(),
            _ => return Err(Error::Invalid("tree entry has no name".into())),
        };
        if !crate::validate::is_valid_tree_name(&name) {
            return Err(Error::Path(format!("invalid tree entry name {name:?}")));
        }
        let mode = match find_value(record, 0x02) {
            Some(Value::U(m)) => *m,
            _ => return Err(Error::Invalid("tree entry has no mode".into())),
        };
        let target = match find_value(record, 0x03) {
            Some(Value::Gid(g)) => *g,
            _ => return Err(Error::Invalid("tree entry has no target".into())),
        };
        let child = dir.join(&name);
        match mode {
            0o040000 => write_tree(repo, &target, &child)?,
            0o120000 => {
                let blob = repo.load(&target)?;
                let content = blob.blob_bytes().unwrap_or(&[]);
                write_symlink(content, &child)?;
            }
            0o100644 | 0o100755 => {
                let blob = repo.load(&target)?;
                let content = blob.blob_bytes().unwrap_or(&[]);
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create(true).truncate(true);
                use std::io::Write;
                let mut file = opts.open(&child)?;
                file.write_all(content)?;
                file.sync_all()?;
                set_permissions(&child, if mode == 0o100755 { 0o755 } else { 0o644 })?;
            }
            other => return Err(Error::Invalid(format!("invalid tree mode {other:#o}"))),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_symlink(content: &[u8], path: &Path) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let _ = std::fs::remove_file(path);
    symlink(std::ffi::OsStr::from_bytes(content), path)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_symlink(_content: &[u8], _path: &Path) -> Result<(), Error> {
    Err(Error::Unsupported(
        "symlinks are unsupported on this platform".into(),
    ))
}

#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> Result<(), Error> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Tree deltas and operation synthesis
// ---------------------------------------------------------------------------

/// The kind of a file-level delta between two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaKind {
    Created,
    Deleted,
    Modified,
    Renamed { from: String },
}

/// A file-level delta between two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDelta {
    pub path: String,
    pub kind: DeltaKind,
    pub old_blob: Option<Gid>,
    pub new_blob: Option<Gid>,
}

/// Computes the deterministic file-level delta between two states.
pub fn diff_states(repo: &Repo, a: &Gid, b: &Gid) -> Result<Vec<FileDelta>, Error> {
    let tree_a = load_state_tree(repo, a)?;
    let tree_b = load_state_tree(repo, b)?;
    let files_a = flatten_tree(repo, &tree_a)?;
    let files_b = flatten_tree(repo, &tree_b)?;

    let mut deltas = Vec::new();
    let mut paths: Vec<&String> = files_a.keys().chain(files_b.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        match (files_a.get(path), files_b.get(path)) {
            (Some(ta), Some(tb)) => {
                if ta != tb {
                    deltas.push(FileDelta {
                        path: path.clone(),
                        kind: DeltaKind::Modified,
                        old_blob: Some(ta.1),
                        new_blob: Some(tb.1),
                    });
                }
            }
            (Some(ta), None) => deltas.push(FileDelta {
                path: path.clone(),
                kind: DeltaKind::Deleted,
                old_blob: Some(ta.1),
                new_blob: None,
            }),
            (None, Some(tb)) => deltas.push(FileDelta {
                path: path.clone(),
                kind: DeltaKind::Created,
                old_blob: None,
                new_blob: Some(tb.1),
            }),
            (None, None) => unreachable!(),
        }
    }

    // Conservative rename detection: exact content+mode moves only
    // (GIT_INTEROP.md §4.1). Pair deletes and creates by identical
    // (mode, blob) with distinct paths, deterministically (sorted pairing).
    let deletes: Vec<(String, (u64, Gid))> = deltas
        .iter()
        .filter(|d| d.kind == DeltaKind::Deleted)
        .map(|d| {
            (
                d.path.clone(),
                (mode_of(&files_a, &d.path), d.old_blob.unwrap()),
            )
        })
        .collect();
    let creates: Vec<(String, (u64, Gid))> = deltas
        .iter()
        .filter(|d| d.kind == DeltaKind::Created)
        .map(|d| {
            (
                d.path.clone(),
                (mode_of(&files_b, &d.path), d.new_blob.unwrap()),
            )
        })
        .collect();
    if !deletes.is_empty() && !creates.is_empty() {
        let mut deletes = deletes;
        let mut creates = creates;
        deletes.sort();
        creates.sort();
        let mut di = 0usize;
        let mut ci = 0usize;
        while di < deletes.len() && ci < creates.len() {
            let (dpath, dkey) = &deletes[di];
            let (cpath, ckey) = &creates[ci];
            match dkey.cmp(ckey) {
                std::cmp::Ordering::Less => di += 1,
                std::cmp::Ordering::Greater => ci += 1,
                std::cmp::Ordering::Equal => {
                    if dpath != cpath {
                        deltas.retain(|d| !(d.path == *dpath && d.kind == DeltaKind::Deleted));
                        deltas.retain(|d| !(d.path == *cpath && d.kind == DeltaKind::Created));
                        deltas.push(FileDelta {
                            path: cpath.clone(),
                            kind: DeltaKind::Renamed {
                                from: dpath.clone(),
                            },
                            old_blob: Some(dkey.1),
                            new_blob: Some(ckey.1),
                        });
                    }
                    di += 1;
                    ci += 1;
                }
            }
        }
    }
    deltas.sort_by(|x, y| x.path.cmp(&y.path));
    Ok(deltas)
}

fn mode_of(files: &std::collections::HashMap<String, (u64, Gid)>, path: &str) -> u64 {
    files.get(path).map(|(m, _)| *m).unwrap_or(0)
}

/// Synthesizes Operation objects for a delta and returns their identities.
pub fn synthesize_operations(
    repo: &Repo,
    deltas: &[FileDelta],
    producer: &Gid,
) -> Result<Vec<Gid>, Error> {
    let mut out = Vec::new();
    for d in deltas {
        let mut fields = Vec::new();
        // Field tags must be pushed in strictly ascending order (the encoder
        // rejects out-of-order tags to guarantee deterministic encoding).
        fields.push(Field::new(
            0x01,
            Value::Str(
                match d.kind {
                    DeltaKind::Created => "create_file",
                    DeltaKind::Deleted => "delete_file",
                    DeltaKind::Modified => "write_file",
                    DeltaKind::Renamed { .. } => "rename_path",
                }
                .to_string(),
            ),
        ));
        fields.push(Field::new(0x02, Value::Str(d.path.clone())));
        if let Some(old) = d.old_blob {
            fields.push(Field::new(0x04, Value::Array(vec![Value::Gid(old)])));
        }
        if let Some(new) = d.new_blob {
            fields.push(Field::new(0x05, Value::Array(vec![Value::Gid(new)])));
        }
        fields.push(Field::new(
            0x06,
            Value::Record(vec![Field::new(0x01, Value::Str("ok".into()))]),
        ));
        fields.push(Field::new(0x07, Value::Gid(*producer)));
        fields.push(Field::new(0x09, Value::I(crate::store::now_ms())));
        fields.push(Field::new(0x0A, Value::I(crate::store::now_ms())));
        // Kind-specific field, always the highest tag for this family.
        match &d.kind {
            DeltaKind::Created | DeltaKind::Modified => {
                if let Some(new) = d.new_blob {
                    fields.push(Field::new(0x11, Value::Gid(new)));
                }
            }
            DeltaKind::Renamed { from } => {
                fields.push(Field::new(0x16, Value::Str(from.clone())));
                fields.push(Field::new(0x17, Value::Str(d.path.clone())));
            }
            DeltaKind::Deleted => {}
        }
        let op = Object::fields(Family::Operation, fields);
        out.push(repo.insert_object(&op)?);
    }
    Ok(out)
}

fn load_state_tree(repo: &Repo, state: &Gid) -> Result<Gid, Error> {
    let st = repo.load(state)?;
    if st.family != Family::State {
        return Err(Error::Invalid(format!("{state} is not a state object")));
    }
    find_gid(&st, 0x01).ok_or_else(|| Error::Invalid("state has no root_tree".into()))
}

/// Flattens a tree into `path -> (mode, blob)` for regular files and
/// symlinks (directories are traversed).
pub fn flatten_tree(
    repo: &Repo,
    tree: &Gid,
) -> Result<std::collections::HashMap<String, (u64, Gid)>, Error> {
    let mut out = std::collections::HashMap::new();
    flatten_tree_rec(repo, tree, "", &mut out)?;
    Ok(out)
}

fn flatten_tree_rec(
    repo: &Repo,
    tree: &Gid,
    prefix: &str,
    out: &mut std::collections::HashMap<String, (u64, Gid)>,
) -> Result<(), Error> {
    let t = repo.load(tree)?;
    let tfs = t.field_sequence().unwrap_or(&[]);
    let entries = match find_value(tfs, 0x01) {
        Some(Value::Array(items)) => items,
        _ => return Err(Error::Invalid("tree has no entries".into())),
    };
    for item in entries {
        let record = match item {
            Value::Record(fields) => fields,
            _ => return Err(Error::Invalid("tree entry is not a record".into())),
        };
        let name = match find_value(record, 0x01) {
            Some(Value::Str(s)) => s.clone(),
            _ => return Err(Error::Invalid("tree entry has no name".into())),
        };
        let mode = match find_value(record, 0x02) {
            Some(Value::U(m)) => *m,
            _ => return Err(Error::Invalid("tree entry has no mode".into())),
        };
        let target = match find_value(record, 0x03) {
            Some(Value::Gid(g)) => *g,
            _ => return Err(Error::Invalid("tree entry has no target".into())),
        };
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if mode == 0o040000 {
            flatten_tree_rec(repo, &target, &path, out)?;
        } else {
            out.insert(path, (mode, target));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Working tree delta (status) — no object insertion
// ---------------------------------------------------------------------------

/// The status of one path relative to a base state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStatus {
    Added,
    Modified,
    Deleted,
    TypeChanged,
}

/// Compares the working tree against a base state without inserting objects.
/// Returns paths whose content or mode differs, with their status.
pub fn working_tree_delta(
    repo: &Repo,
    base: Option<&Gid>,
    ignore: &Ignore,
) -> Result<Vec<(String, PathStatus)>, Error> {
    let base_files: std::collections::HashMap<String, (u64, Gid)> = match base {
        Some(state) => {
            let tree = load_state_tree(repo, state)?;
            flatten_tree(repo, &tree)?
        }
        None => std::collections::HashMap::new(),
    };
    let mut current = std::collections::HashMap::new();
    collect_working_files(repo.root(), "", ignore, &mut current)?;

    let mut out = Vec::new();
    let mut paths: Vec<&String> = base_files.keys().chain(current.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        match (base_files.get(path), current.get(path)) {
            (Some((bmode, bblob)), Some((cmode, cdigest))) => {
                if bmode != cmode || !blob_matches(repo, bblob, cdigest) {
                    out.push((path.clone(), PathStatus::Modified));
                }
            }
            (Some(_), None) => out.push((path.clone(), PathStatus::Deleted)),
            (None, Some(_)) => out.push((path.clone(), PathStatus::Added)),
            (None, None) => unreachable!(),
        }
    }
    Ok(out)
}

/// Compares a working-file digest against a base blob identity: the base blob
/// digest is BLAKE3 of the blob envelope, so compute the would-be envelope of
/// the working bytes and compare digests.
fn blob_matches(repo: &Repo, base_blob: &Gid, working_digest: &[u8; 32]) -> bool {
    let _ = repo;
    base_blob.digest() == working_digest
}

/// Paths that are repository configuration rather than content. The root
/// `.gitignore` is respected by `Ignore` but never captured into a state
/// (STORAGE.md §6); nested `.gitignore` files are regular content in Phase 1
/// (they are not consulted as rules).
fn is_meta_path(rel: &str) -> bool {
    rel == ".gitignore"
}

fn collect_working_files(
    dir: &Path,
    rel: &str,
    ignore: &Ignore,
    out: &mut std::collections::HashMap<String, (u64, [u8; 32])>,
) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == crate::store::META_DIR {
            continue;
        }
        let ft = entry.file_type()?;
        let is_dir = ft.is_dir();
        let full_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if is_meta_path(&full_rel) {
            continue; // repository configuration, never content (STORAGE.md §6)
        }
        if ignore.is_ignored(&full_rel, is_dir) {
            continue;
        }
        let path = dir.join(&name);
        if ft.is_dir() {
            collect_working_files(&path, &full_rel, ignore, out)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&path)?;
            #[cfg(unix)]
            let bytes: Vec<u8> = {
                use std::os::unix::ffi::OsStrExt;
                target.as_os_str().as_bytes().to_vec()
            };
            #[cfg(not(unix))]
            let bytes = target.to_string_lossy().as_bytes().to_vec();
            let digest = blob_digest_of(&bytes);
            out.insert(full_rel, (0o120000, digest));
        } else if ft.is_file() {
            let bytes = std::fs::read(&path)?;
            let mode = if is_executable(&entry.metadata()?) {
                0o100755
            } else {
                0o100644
            };
            out.insert(full_rel, (mode, blob_digest_of(&bytes)));
        }
    }
    Ok(())
}

/// BLAKE3 of the would-be blob envelope (GCE): magic, encver 1, family blob,
/// schemever 1, flags 0, bodylen, content.
fn blob_digest_of(content: &[u8]) -> [u8; 32] {
    let mut env = Vec::with_capacity(content.len() + 16);
    env.extend_from_slice(b"GEML");
    env.push(0x01);
    env.push(0x01);
    env.push(0x01);
    env.push(0x00);
    let mut len = Vec::new();
    crate::varint::encode_u64(content.len() as u64, &mut len);
    env.extend_from_slice(&len);
    env.extend_from_slice(content);
    crate::hash::object_id_bytes(&env)
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

fn find_value(fields: &[Field], tag: u8) -> Option<&Value> {
    fields.iter().find(|f| f.tag == tag).map(|f| &f.value)
}

fn find_gid(obj: &Object, tag: u8) -> Option<Gid> {
    let fields = obj.field_sequence()?;
    match find_value(fields, tag) {
        Some(Value::Gid(g)) => Some(*g),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Textual (Myers) diff
// ---------------------------------------------------------------------------

/// An edit run over lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// Computes the minimal edit script between two line lists (linear-space
/// Myers with a bounded small-case fallback).
pub fn myers_diff(a: &[&str], b: &[&str]) -> Vec<Edit> {
    let mut edits = Vec::new();
    // Trim common prefix.
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    if i > 0 {
        edits.push(Edit::Equal(i));
    }
    let a = &a[i..];
    let b = &b[i..];
    // Trim common suffix.
    let mut j = 0;
    while j < a.len() && j < b.len() && a[a.len() - 1 - j] == b[b.len() - 1 - j] {
        j += 1;
    }
    let a = &a[..a.len() - j];
    let b = &b[..b.len() - j];
    diff_rec(a, b, &mut edits);
    if j > 0 {
        edits.push(Edit::Equal(j));
    }
    coalesce(edits)
}

fn diff_rec(a: &[&str], b: &[&str], out: &mut Vec<Edit>) {
    if a.is_empty() {
        if !b.is_empty() {
            out.push(Edit::Insert(b.len()));
        }
        return;
    }
    if b.is_empty() {
        out.push(Edit::Delete(a.len()));
        return;
    }
    // Small cases: trace-based Myers (bounded memory).
    if a.len() * b.len() <= 2500 {
        out.extend(myers_trace(a, b));
        return;
    }
    let (sx, sy, ex, ey, d) = find_middle_snake(a, b);
    if d == 0 {
        // Entirely equal (should not happen after trimming).
        out.push(Edit::Equal(a.len()));
        return;
    }
    diff_rec(&a[..sx], &b[..sy], out);
    if ex > sx {
        out.push(Edit::Equal(ex - sx));
    }
    diff_rec(&a[ex..], &b[ey..], out);
}

/// O(ND) Myers with trace — used for small inputs.
fn myers_trace(a: &[&str], b: &[&str]) -> Vec<Edit> {
    let n = a.len() as i64;
    let m = b.len() as i64;
    let max = (n + m) as usize;
    let offset = max as i64;
    let mut v = vec![0i64; 2 * max + 3];
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut found_d = 0i64;
    'outer: for d in 0..=max as i64 {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let kk = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[kk - 1] < v[kk + 1]) {
                v[kk + 1]
            } else {
                v[kk - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[kk] = x;
            if x >= n && y >= m {
                found_d = d;
                break 'outer;
            }
            k += 2;
        }
    }
    // Backtrack.
    let mut edits: Vec<Edit> = Vec::new();
    let mut x = n;
    let mut y = m;
    for d in (0..=found_d).rev() {
        let vd = &trace[d as usize];
        let k = x - y;
        let kk = (k + offset) as usize;
        let prev_k = if k == -d || (k != d && vd[kk - 1] < vd[kk + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = vd[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            edits.push(Edit::Equal(1));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                edits.push(Edit::Insert(1));
            } else {
                edits.push(Edit::Delete(1));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    edits.reverse();
    coalesce(edits)
}

/// Linear-space middle-snake search (Myers 1986).
/// Returns (snake_start_x, snake_start_y, snake_end_x, snake_end_y, d).
fn find_middle_snake(a: &[&str], b: &[&str]) -> (usize, usize, usize, usize, usize) {
    let n = a.len() as i64;
    let m = b.len() as i64;
    let delta = n - m;
    let odd = delta % 2 != 0;
    let max_d = ((n + m) / 2 + 1) as usize;
    let offset = max_d as i64;
    let mut vf = vec![0i64; 2 * max_d + 3];
    let mut vb = vec![0i64; 2 * max_d + 3];
    vf[offset as usize + 1] = 0;
    vb[offset as usize + 1] = 0;
    for d in 0..=max_d as i64 {
        // Forward (diagonals k in forward coordinates).
        let mut k = -d;
        while k <= d {
            let kk = (k + offset) as usize;
            let mut x = if k == -d || (k != d && vf[kk - 1] < vf[kk + 1]) {
                vf[kk + 1]
            } else {
                vf[kk - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            vf[kk] = x;
            // Odd delta: forward depth d overlaps backward depth d-1.
            // The backward array is indexed by the reversed diagonal
            // k' = delta - k.
            if odd && (k - delta).abs() < d {
                let bkk = ((delta - k) + offset) as usize;
                if vf[kk] + vb[bkk] >= n {
                    let (sx, sy) = snake_start(a, b, x, y);
                    return (sx, sy, x as usize, y as usize, d as usize);
                }
            }
            k += 2;
        }
        // Backward (diagonals k' in reversed coordinates).
        let mut k = -d;
        while k <= d {
            let kk = (k + offset) as usize;
            let mut x = if k == -d || (k != d && vb[kk - 1] < vb[kk + 1]) {
                vb[kk + 1]
            } else {
                vb[kk - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[(n - 1 - x) as usize] == b[(m - 1 - y) as usize] {
                x += 1;
                y += 1;
            }
            vb[kk] = x;
            // Even delta: backward depth d overlaps forward depth d.
            // The forward diagonal for reversed diagonal k is delta - k.
            if !odd && (k - delta).abs() <= d {
                let fkk = ((delta - k) + offset) as usize;
                if vb[kk] + vf[fkk] >= n {
                    let (rsx, rsy) = snake_start_rev(a, b, x, y);
                    return (
                        (n - x) as usize,
                        (m - y) as usize,
                        (n - rsx as i64) as usize,
                        (m - rsy as i64) as usize,
                        d as usize,
                    );
                }
            }
            k += 2;
        }
    }
    // Unreachable for valid inputs; fall back to the whole range.
    (0, 0, a.len(), b.len(), 1)
}

/// Walks back from (x, y) to the start of the equal run ending at (x, y).
fn snake_start(a: &[&str], b: &[&str], mut x: i64, mut y: i64) -> (usize, usize) {
    while x > 0 && y > 0 && a[(x - 1) as usize] == b[(y - 1) as usize] {
        x -= 1;
        y -= 1;
    }
    (x as usize, y as usize)
}

/// Walks back from the reversed position (x, y) to the start of the equal
/// run ending there (reversed coordinates).
fn snake_start_rev(a: &[&str], b: &[&str], mut x: i64, mut y: i64) -> (usize, usize) {
    let n = a.len() as i64;
    let m = b.len() as i64;
    while x > 0 && y > 0 && a[(n - x) as usize] == b[(m - y) as usize] {
        x -= 1;
        y -= 1;
    }
    (x as usize, y as usize)
}

fn coalesce(edits: Vec<Edit>) -> Vec<Edit> {
    let mut out: Vec<Edit> = Vec::new();
    for e in edits {
        match (out.last_mut(), e) {
            (Some(Edit::Equal(n)), Edit::Equal(m)) => *n += m,
            (Some(Edit::Delete(n)), Edit::Delete(m)) => *n += m,
            (Some(Edit::Insert(n)), Edit::Insert(m)) => *n += m,
            _ => out.push(e),
        }
    }
    out
}

/// A unified-diff hunk.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub a_start: usize,
    pub a_count: usize,
    pub b_start: usize,
    pub b_count: usize,
    pub lines: Vec<(char, String)>,
}

/// Converts an edit script into context hunks.
pub fn to_hunks(edits: &[Edit], a: &[&str], b: &[&str], context: usize) -> Vec<Hunk> {
    // Expand edits into per-line ops.
    let mut ops: Vec<(char, String)> = Vec::new();
    let mut ai = 0usize;
    let mut bi = 0usize;
    for e in edits {
        match e {
            Edit::Equal(n) => {
                for _ in 0..*n {
                    ops.push((' ', a[ai].to_string()));
                    ai += 1;
                    bi += 1;
                }
            }
            Edit::Delete(n) => {
                for _ in 0..*n {
                    ops.push(('-', a[ai].to_string()));
                    ai += 1;
                }
            }
            Edit::Insert(n) => {
                for _ in 0..*n {
                    ops.push(('+', b[bi].to_string()));
                    bi += 1;
                }
            }
        }
    }
    // Find changed regions and extend with context.
    let mut hunks = Vec::new();
    let mut i = 0usize;
    while i < ops.len() {
        if ops[i].0 == ' ' {
            i += 1;
            continue;
        }
        // Region start.
        let region_start = i;
        let mut region_end = i;
        while region_end < ops.len() && ops[region_end].0 != ' ' {
            region_end += 1;
        }
        // Context window.
        let start = region_start.saturating_sub(context);
        let mut end = region_end + context;
        if end > ops.len() {
            end = ops.len();
        }
        // Extend into following context until the next change is > 2*context away.
        while end < ops.len() {
            // Find next change after end.
            let mut next_change = end;
            while next_change < ops.len() && ops[next_change].0 == ' ' {
                next_change += 1;
            }
            if next_change >= ops.len() {
                break;
            }
            if next_change - end <= 2 * context {
                // Merge.
                end = next_change + context;
                if end > ops.len() {
                    end = ops.len();
                }
            } else {
                break;
            }
        }
        let a_before = ops[..start].iter().filter(|(c, _)| *c != '+').count();
        let a_count = ops[start..end].iter().filter(|(c, _)| *c != '+').count();
        let b_before = ops[..start].iter().filter(|(c, _)| *c != '-').count();
        let b_count = ops[start..end].iter().filter(|(c, _)| *c != '-').count();
        let a_start = if a_count == 0 { 0 } else { a_before + 1 };
        let b_start = if b_count == 0 { 0 } else { b_before + 1 };
        let lines = ops[start..end].to_vec();
        hunks.push(Hunk {
            a_start,
            a_count,
            b_start,
            b_count,
            lines,
        });
        i = end;
    }
    hunks
}

/// Renders a unified diff of two files.
pub fn unified_diff(
    a_label: &str,
    b_label: &str,
    a: &[&str],
    b: &[&str],
    context: usize,
) -> String {
    let edits = myers_diff(a, b);
    if edits.iter().all(|e| matches!(e, Edit::Equal(_))) {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!("--- {a_label}\n+++ {b_label}\n"));
    for h in to_hunks(&edits, a, b, context) {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.a_start, h.a_count, h.b_start, h.b_count
        ));
        for (c, line) in h.lines {
            out.push(c);
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Splits content into lines (without trailing newline).
pub fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    content.lines().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_records_coherence() {
        let (repo, root) = crate::store::testing::fresh_repo("capture");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        std::fs::write(root.join("sub/b.txt"), "world\n").unwrap();
        let ignore = Ignore::default();
        let snap = build_state(&repo, &root, &ignore).unwrap();
        assert!(snap.coherent, "stable tree captures coherently");
        assert_eq!(snap.attempts, 1);
        assert_eq!(snap.files, 2);
        // The state object carries the capture record (extension 0x80, Raw:
        // canonical JSON).
        let obj = repo.load(&snap.state).unwrap();
        let fields = obj.field_sequence().unwrap();
        match crate::query::value_at(fields, 0x80).unwrap() {
            Value::Raw(bytes) => {
                let record: serde_json::Value =
                    serde_json::from_slice(bytes).expect("capture record is JSON");
                assert_eq!(record["coherent"], true);
                assert_eq!(record["attempts"], 1);
            }
            _ => panic!("capture record must be a raw extension"),
        }
        // Identical stable captures deduplicate to the same state identity.
        let snap2 = build_state(&repo, &root, &ignore).unwrap();
        assert_eq!(snap.state, snap2.state);
        assert!(snap2.coherent);
    }

    #[test]
    fn verify_capture_detects_mutation() {
        let root = crate::store::testing::temp_root("verify-capture");
        std::fs::create_dir_all(root.join("d")).unwrap();
        let p = root.join("d/f.txt");
        std::fs::write(&p, "one\n").unwrap();
        let fmd = std::fs::symlink_metadata(&p).unwrap();
        let dmd = std::fs::symlink_metadata(root.join("d")).unwrap();
        let log = vec![
            CapturedEntry {
                path: "d".into(),
                is_dir: true,
                size: dmd.len(),
                mtime_ns: mtime_ns(&dmd),
            },
            CapturedEntry {
                path: "d/f.txt".into(),
                is_dir: false,
                size: fmd.len(),
                mtime_ns: mtime_ns(&fmd),
            },
        ];
        assert!(verify_capture(&root, &log).unwrap());
        // Mutate the file: size + mtime change → incoherent.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&p, "one\ntwo\n").unwrap();
        assert!(!verify_capture(&root, &log).unwrap());
        // Deleting the entry is also incoherent.
        std::fs::remove_file(&p).unwrap();
        assert!(!verify_capture(&root, &log).unwrap());
    }

    fn diff_str(a: &str, b: &str) -> String {
        let al: Vec<&str> = split_lines(a);
        let bl: Vec<&str> = split_lines(b);
        let edits = myers_diff(&al, &bl);
        let mut out = String::new();
        let mut ai = 0;
        let mut bi = 0;
        for e in &edits {
            match e {
                Edit::Equal(n) => {
                    for _ in 0..*n {
                        out.push_str(&format!("  {}\n", al[ai]));
                        ai += 1;
                        bi += 1;
                    }
                }
                Edit::Delete(n) => {
                    for _ in 0..*n {
                        out.push_str(&format!("- {}\n", al[ai]));
                        ai += 1;
                    }
                }
                Edit::Insert(n) => {
                    for _ in 0..*n {
                        out.push_str(&format!("+ {}\n", bl[bi]));
                        bi += 1;
                    }
                }
            }
        }
        out
    }

    #[test]
    fn myers_basics() {
        assert_eq!(diff_str("a\nb\nc\n", "a\nc\n"), "  a\n- b\n  c\n");
        assert_eq!(diff_str("a\n", "a\nb\n"), "  a\n+ b\n");
        assert_eq!(diff_str("", "x\n"), "+ x\n");
        assert_eq!(diff_str("x\n", ""), "- x\n");
        assert_eq!(diff_str("a\nb\n", "a\nb\n"), "  a\n  b\n");
        // Minimal scripts may order edits differently; the script must
        // transform a into b with 3 edits.
        assert_eq!(
            diff_str("a\nb\nc\n", "x\nb\ny\n"),
            "- a\n+ x\n  b\n- c\n+ y\n"
        );
    }

    #[test]
    fn myers_large_case() {
        // Force the divide-and-conquer path.
        let a: Vec<String> = (0..200).map(|i| format!("line{i}")).collect();
        let mut b = a.clone();
        b.remove(100);
        b.insert(50, "inserted".to_string());
        let al: Vec<&str> = a.iter().map(|s| &s[..]).collect();
        let bl: Vec<&str> = b.iter().map(|s| &s[..]).collect();
        let edits = myers_diff(&al, &bl);
        // Verify the edit script actually transforms a into b.
        let mut ai = 0;
        let mut bi = 0;
        for e in &edits {
            match e {
                Edit::Equal(n) => {
                    for _ in 0..*n {
                        assert_eq!(a[ai], b[bi]);
                        ai += 1;
                        bi += 1;
                    }
                }
                Edit::Delete(n) => ai += n,
                Edit::Insert(n) => bi += n,
            }
        }
        assert_eq!(ai, a.len());
        assert_eq!(bi, b.len());
    }

    #[test]
    fn divide_and_conquer_matches_trace_minimality() {
        // Deterministic pseudo-random inputs sized to force the D&C path
        // (n*m > 2500); assert the D&C edit count equals the O(ND) trace's
        // edit count (both are minimal).
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..8 {
            let a: Vec<String> = (0..110).map(|_| format!("l{}", next() % 7)).collect();
            let b: Vec<String> = (0..100).map(|_| format!("l{}", next() % 7)).collect();
            let al: Vec<&str> = a.iter().map(|s| &s[..]).collect();
            let bl: Vec<&str> = b.iter().map(|s| &s[..]).collect();
            let dc = myers_diff(&al, &bl);
            let tr = myers_trace(&al, &bl);
            let count = |v: &[Edit]| -> usize {
                v.iter()
                    .map(|e| match e {
                        Edit::Delete(n) | Edit::Insert(n) => *n,
                        Edit::Equal(_) => 0,
                    })
                    .sum()
            };
            assert_eq!(count(&dc), count(&tr), "D&C minimality mismatch");
        }
    }

    #[test]
    fn unified_diff_renders() {
        let a = split_lines("a\nb\nc\nd\n");
        let b = split_lines("a\nx\nc\nd\n");
        let u = unified_diff("a.txt", "a.txt", &a, &b, 1);
        assert!(u.contains("--- a.txt"));
        assert!(u.contains("@@ -1,3 +1,3 @@"));
        assert!(u.contains("-b"));
        assert!(u.contains("+x"));
    }
}
