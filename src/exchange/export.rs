//! Exchange export (EXCHANGE.md §4–§9, §21, §25).
//!
//! Deterministic, append-only publication: packs first, frontier last.
//! Identical native state + profile ⇒ byte-identical exchange files.

use super::{encode_frontier, encode_pack, pack_id, pack_path, Coverage, Frontier};
use crate::family::Family;
use crate::gid::Gid;
use crate::store::{Error, Repo, REF_HEAD, REF_TRAJECTORIES};
use crate::value::Object;
use std::collections::HashSet;
use std::path::Path;

/// Export profiles (EXCHANGE.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Frontier,
    Portable,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Profile, Error> {
        match s {
            "frontier" => Ok(Profile::Frontier),
            "portable" => Ok(Profile::Portable),
            other => Err(Error::Invalid(format!(
                "unknown exchange profile {other:?} (frontier|portable)"
            ))),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Profile::Frontier => "frontier",
            Profile::Portable => "portable",
        }
    }
}

/// The outcome of an export.
#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub packs_written: usize,
    pub packs_reused: usize,
    pub objects: usize,
    pub frontier: String,
    pub frontier_id: [u8; 32],
    pub source_state: Gid,
}

/// Collects the objects for a profile: a deterministic BFS over gid edges
/// from the trajectory/head/config seeds. Blobs are included only for the
/// `portable` profile (the `frontier` profile treats source payloads as
/// carrier-backed; EXCHANGE.md §4, §13).
pub fn collect_export_objects(repo: &Repo, profile: Profile) -> Result<Vec<(Gid, Vec<u8>)>, Error> {
    let include_blobs = profile == Profile::Portable;
    let mut seen: HashSet<Gid> = HashSet::new();
    let mut out: Vec<(Gid, Vec<u8>)> = Vec::new();
    let mut queue: Vec<Gid> = Vec::new();
    // Seeds: every trajectory (latest version), head, config.
    let prefix = format!("{REF_TRAJECTORIES}/");
    for (name, gid) in repo.all_refs()? {
        if name.starts_with(&prefix) && name != format!("{REF_TRAJECTORIES}/current") {
            queue.push(gid);
        }
    }
    if let Some(h) = repo.read_ref(REF_HEAD)? {
        queue.push(h);
    }
    if let Some(c) = repo.read_ref(crate::store::REF_CONFIG)? {
        queue.push(c);
    }
    // The semantic index of the head state (Phase 5): the frontier profile
    // carries the current semantic knowledge graph so a fresh agent inherits
    // entity context after an ordinary clone (EXCHANGE.md §11, §56).
    if let Some(i) = repo.read_ref(crate::semantic::REF_SEMANTIC_HEAD)? {
        queue.push(i);
    }
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let obj = match repo.load(&id) {
            Ok(o) => o,
            Err(e) => {
                return Err(Error::Invalid(format!(
                    "cannot export {id}: {e} (incomplete canonical graph)"
                )))
            }
        };
        if obj.family == Family::Blob {
            if include_blobs {
                let bytes = repo.read_bytes(&id)?;
                out.push((id, bytes));
            }
            continue; // blobs have no gid children
        }
        let bytes = repo.read_bytes(&id)?;
        out.push((id, bytes));
        for (_, to, _) in crate::store::index::edges_of(&obj) {
            queue.push(to);
        }
    }
    out.sort_by_key(|a| a.0);
    Ok(out)
}

/// One encoded pack artifact: (exact bytes, pack id).
pub type PackArtifact = (Vec<u8>, [u8; 32]);

/// Partitions objects into packs deterministically (EXCHANGE.md §7):
/// sorted by id, greedily appended until the next object would exceed the
/// protocol target size. Returns packs as (bytes, id) in id order.
pub fn build_packs(objects: &[(Gid, Vec<u8>)]) -> Result<Vec<PackArtifact>, Error> {
    let mut packs = Vec::new();
    let mut current: Vec<super::PackObject> = Vec::new();
    let mut current_bytes = 0u64;
    let flush = |current: &mut Vec<super::PackObject>,
                 packs: &mut Vec<PackArtifact>|
     -> Result<(), Error> {
        if current.is_empty() {
            return Ok(());
        }
        let (bytes, id) = encode_pack(current)?;
        packs.push((bytes, id));
        current.clear();
        Ok(())
    };
    for (id, envelope) in objects {
        let size = 41u64 + envelope.len() as u64;
        if !current.is_empty() && current_bytes + size > super::TARGET_PACK_BYTES {
            flush(&mut current, &mut packs)?;
            current_bytes = 0;
        }
        current.push(super::PackObject {
            id: *id,
            envelope: envelope.clone(),
        });
        current_bytes += size;
    }
    flush(&mut current, &mut packs)?;
    packs.sort_by_key(|a| a.1);
    Ok(packs)
}

/// Writes a file atomically with fsync (EXCHANGE.md §9): temp → fsync →
/// verify identity → atomic rename.
pub fn write_atomic_fsync(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The pure-content State identity for a flat file map: the would-be state
/// object over the root tree, excluding capture metadata (EXCHANGE.md §8:
/// source binding is content identity, stable across captures and machines).
pub fn content_state_identity(
    repo: &Repo,
    files: &std::collections::BTreeMap<String, (u64, Gid)>,
) -> Result<Gid, Error> {
    content_state_identity_with_limits(files, &repo.limits())
}

/// As [`content_state_identity`] with explicit limits (no repository needed;
/// used by verification against a checkout without a native store).
pub fn content_state_identity_with_limits(
    files: &std::collections::BTreeMap<String, (u64, Gid)>,
    limits: &crate::limits::Limits,
) -> Result<Gid, Error> {
    let (tree_id, _) = crate::content::state_identity_from_files_with_limits(files, limits)?;
    let state = Object::fields(
        Family::State,
        vec![crate::value::Field::new(
            0x01,
            crate::value::Value::Gid(tree_id),
        )],
    );
    crate::content::object_identity_with_limits(&state, limits)
}

/// The pure-content State identity of a canonical state object (its tree,
/// without capture metadata).
pub fn content_identity_of_state(repo: &Repo, state: &Gid) -> Result<Gid, Error> {
    let files = crate::content::state_files(repo, state)?;
    content_state_identity(repo, &files)
}

/// Writes the `.gemel/.gitignore` that keeps the local native store invisible
/// to Git while allowing the exchange namespace to be tracked (EXCHANGE.md
/// §3). Never clobbers tracked data.
pub const LOCAL_GITIGNORE: &str = "*\n!.gitignore\n!exchange/\n!exchange/**\n";

pub fn install_local_gitignore(meta: &Path) -> Result<(), Error> {
    let path = meta.join(".gitignore");
    if path.exists() {
        // Already installed (or user-managed); do not clobber.
        return Ok(());
    }
    write_atomic_fsync(&path, LOCAL_GITIGNORE.as_bytes())
}

/// The frontier for the current repository state.
fn build_frontier(
    repo: &Repo,
    profile: Profile,
    packs: &[(Vec<u8>, [u8; 32])],
) -> Result<(Frontier, Vec<u8>, [u8; 32]), Error> {
    let head = repo
        .read_ref(REF_HEAD)?
        .ok_or_else(|| Error::Invalid("no head change to export".into()))?;
    let head_obj = repo.load(&head)?;
    let hfs = head_obj.field_sequence().unwrap_or(&[]);
    let resulting = crate::query::gid_field(hfs, 0x05)
        .ok_or_else(|| Error::Invalid("head change has no resulting state".into()))?;
    let source_state = content_identity_of_state(repo, &resulting)?;
    let trajectory = repo.read_ref(&format!("{REF_TRAJECTORIES}/current"))?;
    let intent = crate::query::gid_field(hfs, 0x02);
    // Parent frontier: the previously active one (lineage; append-only).
    let parent = read_active_frontier(repo.meta_dir())?;
    let coverage = Coverage {
        source_content: if profile == Profile::Portable {
            "complete".into()
        } else {
            "carrier-backed".into()
        },
        evidence_payloads: "partial".into(),
        ..Coverage::default()
    };
    let f = Frontier {
        schema: super::FRONTIER_SCHEMA.to_string(),
        source_state,
        head_change: head,
        trajectory,
        intent,
        parent_frontiers: parent.into_iter().map(|p| crate::hex::encode(&p)).collect(),
        packs: packs.iter().map(|(_, id)| crate::hex::encode(id)).collect(),
        profile: profile.as_str().to_string(),
        coverage,
        required_schemas: vec![1],
    };
    let bytes = encode_frontier(&f)?;
    let id = super::frontier_id(&bytes);
    Ok((f, bytes, id))
}

/// Runs an append-only exchange export (EXCHANGE.md §9, §21, §25):
/// packs first (reusing verified existing packs), frontier last.
pub fn export(repo: &Repo, profile: Profile) -> Result<ExportOutcome, Error> {
    let meta = repo.meta_dir().to_path_buf();
    // Interrupted-export recovery (EXCHANGE.md §36): remove abandoned
    // temporaries before continuing; never touches published artifacts.
    let _ = clean_temporaries(&meta);
    let objects = collect_export_objects(repo, profile)?;
    let packs = build_packs(&objects)?;
    let mut written = 0usize;
    let mut reused = 0usize;
    let mut pack_ids = Vec::with_capacity(packs.len());
    for (bytes, id) in &packs {
        let path = pack_path(&meta, id);
        if path.exists() {
            // Reuse the verified existing pack (append-only, EXCHANGE.md §25).
            let existing = std::fs::read(&path)?;
            if pack_id(&existing) != *id {
                return Err(Error::Invalid(format!(
                    "existing exchange pack at {} does not match its identity",
                    path.display()
                )));
            }
            reused += 1;
        } else {
            // Temp → fsync → verify exact PackId → atomic rename.
            let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
            write_atomic_fsync(&tmp, bytes)?;
            let written_bytes = std::fs::read(&tmp)?;
            if pack_id(&written_bytes) != *id {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::Invalid("pack identity mismatch on write".into()));
            }
            std::fs::rename(&tmp, &path)?;
            written += 1;
        }
        pack_ids.push(*id);
    }
    // Frontier last, only after every referenced pack exists and verifies.
    let (f, fbytes, fid) = build_frontier(repo, profile, &packs)?;
    // Idempotence (EXCHANGE.md §49): an existing frontier that already
    // describes this exact head change, source state, and profile IS the
    // current one; publishing a duplicate lineage entry would make
    // "export twice with unchanged native state" produce byte changes.
    // The existing frontier's id becomes the active one (never the
    // computed-but-unwritten id).
    let published_id = match super::discover_frontiers(&meta)?
        .into_iter()
        .find(|(x, _, _)| {
            x.head_change == f.head_change
                && x.source_state == f.source_state
                && x.profile == f.profile
        }) {
        Some((_, existing_id, _)) => existing_id,
        None => {
            let fpath = super::frontier_path(&meta, &fid);
            if !fpath.exists() {
                let tmp = fpath.with_extension(format!("tmp-{}", std::process::id()));
                write_atomic_fsync(&tmp, &fbytes)?;
                let written_bytes = std::fs::read(&tmp)?;
                if super::frontier_id(&written_bytes) != fid {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(Error::Invalid("frontier identity mismatch on write".into()));
                }
                std::fs::rename(&tmp, &fpath)?;
            }
            fid
        }
    };
    // Track the active frontier locally (not tracked by Git).
    record_active_frontier(&meta, &published_id)?;
    Ok(ExportOutcome {
        packs_written: written,
        packs_reused: reused,
        objects: objects.len(),
        frontier: crate::hex::encode(&published_id),
        frontier_id: published_id,
        source_state: f.source_state,
    })
}

/// The locally tracked active frontier id, if any.
pub fn read_active_frontier(meta: &Path) -> Result<Option<[u8; 32]>, Error> {
    let path = meta.join("exchange-state").join("active");
    match std::fs::read_to_string(&path) {
        Ok(text) => match super::hex_to_digest(text.trim()) {
            Some(d) => Ok(Some(d)),
            None => Ok(None),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Records the locally active exported frontier id (not tracked by Git).
pub(crate) fn record_active_frontier(meta: &Path, id: &[u8; 32]) -> Result<(), Error> {
    let dir = meta.join("exchange-state");
    std::fs::create_dir_all(&dir)?;
    write_atomic_fsync(
        &dir.join("active"),
        &format!("{}\n", crate::hex::encode(id)).into_bytes(),
    )
}

/// Marks a frontier as locally imported (idempotence tracking; EXCHANGE.md
/// §16). Returns false when it was already imported.
pub fn mark_imported(meta: &Path, id: &[u8; 32]) -> Result<bool, Error> {
    let dir = meta.join("exchange-state").join("imported");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(crate::hex::encode(id));
    if path.exists() {
        return Ok(false);
    }
    write_atomic_fsync(&path, b"imported\n")?;
    Ok(true)
}

/// Whether a frontier was already imported locally.
pub fn is_imported(meta: &Path, id: &[u8; 32]) -> bool {
    meta.join("exchange-state")
        .join("imported")
        .join(crate::hex::encode(id))
        .exists()
}

/// Removes abandoned exchange temporaries (EXCHANGE.md §36): files matching
/// `*.tmp-*` under the exchange directories. Safe: they are unreferenced.
pub fn clean_temporaries(meta: &Path) -> Result<usize, Error> {
    let mut removed = 0usize;
    for dir in [super::pack_dir(meta), super::frontier_dir(meta)] {
        let shards = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for shard in shards.flatten() {
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let entries = match std::fs::read_dir(shard.path()) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.contains(".tmp-") || name.starts_with("tmp-") {
                    if let Ok(()) = std::fs::remove_file(entry.path()) {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
}

/// A workspace-aware view of the working tree file map (path → (mode,
/// blob-gid)) computed from a captured snapshot. Uses the capture built by
/// `build_state` (blobs already inserted).
pub fn working_tree_files(
    repo: &Repo,
) -> Result<std::collections::BTreeMap<String, (u64, Gid)>, Error> {
    let ignore = crate::ignore::Ignore::from_root(repo.root());
    let snap = crate::content::build_state(repo, repo.root(), &ignore)?;
    let files = crate::content::state_files(repo, &snap.state)?;
    Ok(files)
}
