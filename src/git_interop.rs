//! Deterministic Git interchange (GIT_INTEROP.md, Phase 4).
//!
//! `export-git` projects a Gemel change sequence into an ordinary Git
//! repository (loose objects, deterministic authors/timestamps/messages,
//! `GEMEL-*` trailers carrying stable Gemel identities, and canonical
//! `mapping` objects anchoring the correspondence under `refs/mappings/*`).
//!
//! `import-git` synthesizes Gemel objects from a Git history: synthetic
//! `git_import`/`human` producers, states from Git trees, deterministic
//! operations from tree deltas, first-parent-chain trajectories, and
//! mappings. It never fabricates intent, claims, evidence, or residuals;
//! unavailable provenance is `UNKNOWN` (GIT_INTEROP.md §4.2). Trailers that
//! validate against the repository re-link the original identities; hostile
//! trailer values are ignored (treated as foreign).
//!
//! Determinism is absolute: identical inputs produce identical Git bytes
//! (export) and identical Gemel objects (import). No wall clock is consulted.

use crate::family::Family;
use crate::gid::Gid;
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::REF_MAPPINGS;
use crate::store::{Error, Repo, REF_HEAD};
use crate::value::{Field, Object, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// The export trailer version (GIT_INTEROP.md §3.2).
pub const EXPORT_VERSION: &str = "1";

/// The Git committer identity used by export (the exporting automation).
pub const GEMEL_COMMITTER: &str = "gemel <gemel@local>";
/// The Git author identity for producers whose disclosure does not permit
/// human attribution (GIT_INTEROP.md §3.2: never fabricated).
pub const GEMEL_PRODUCER_AUTHOR: &str = "Gemel Producer <gemel@local>";

/// The ref namespace anchoring import/export mappings lives in the store
/// (see `crate::store::REF_MAPPINGS`) so name resolution searches it
/// (GIT_INTEROP.md §2).
/// Options for `gemel export-git`.
#[derive(Debug, Clone)]
pub struct ExportGitOptions {
    /// The target Git directory (e.g. `<root>/.git` or a bare dir).
    pub git_dir: std::path::PathBuf,
    /// The branch to write (default `main`).
    pub branch: String,
    /// Include `GEMEL-CLAIM` trailers (disclosure permitting; default false).
    pub include_claims: bool,
}

/// The outcome of an export.
#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub commits: usize,
    pub trees: usize,
    pub mappings: usize,
    pub head_oid: String,
    pub branch: String,
}

/// Options for `gemel import-git`.
#[derive(Debug, Clone)]
pub struct ImportGitOptions {
    /// The Git directory to read from.
    pub git_dir: std::path::PathBuf,
    /// The commit-ish to import (default `HEAD`).
    pub head: String,
}

/// The outcome of an import.
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub commits: usize,
    pub changes: usize,
    pub trajectories: usize,
    pub mappings: usize,
    pub relinked: usize,
    pub ignored_trailers: usize,
    pub unknown_producers: usize,
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// The deterministic Git author for a change (GIT_INTEROP.md §3.2).
fn export_author(repo: &Repo, change: &Gid) -> Result<(String, i64), Error> {
    let obj = repo.load(change)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    let producer = crate::query::gid_field(fs, 0x06);
    let created_at = crate::query::int_field(fs, 0x15).unwrap_or(0);
    let author = match producer {
        Some(p) => {
            let pobj = repo.load(&p)?;
            let pfs = pobj.field_sequence().unwrap_or(&[]);
            let kind = crate::query::str_field(pfs, 0x01).unwrap_or("");
            let disclosure = crate::query::str_field(pfs, 0x04).unwrap_or("");
            if kind == "human" && (disclosure == "FULL" || disclosure == "DIGEST_ONLY") {
                let name = crate::query::str_field(pfs, 0x02)
                    .unwrap_or("unknown")
                    .to_string();
                let email = producer_email(repo, &p);
                format!(
                    "{} <{}>",
                    sanitize_identity(&name),
                    email.unwrap_or_else(|| "gemel@local".to_string())
                )
            } else {
                GEMEL_PRODUCER_AUTHOR.to_string()
            }
        }
        None => GEMEL_PRODUCER_AUTHOR.to_string(),
    };
    Ok((author, created_at))
}

/// The email inside a producer's identity record, if any.
fn producer_email(repo: &Repo, producer: &Gid) -> Option<String> {
    let obj = repo.load(producer).ok()?;
    let fs = obj.field_sequence()?;
    for f in fs {
        if f.tag == 0x03 {
            if let Value::Record(ident) = &f.value {
                for g in ident {
                    if g.tag == 0x01 {
                        if let Value::Record(parts) = &g.value {
                            for h in parts {
                                if h.tag == 0x02 {
                                    if let Value::Str(e) = &h.value {
                                        return Some(e.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Git identity fields are restricted to printable ASCII without angle
/// brackets (hostile values must not corrupt the commit header).
fn sanitize_identity(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '<' && *c != '>' && *c != '\n')
        .take(200)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Formats `<name> <email> <ts> +0000` for a commit header.
fn person_line(name_email: &str, ts: i64) -> String {
    format!("{name_email} {ts} +0000")
}

/// Recursively writes Git trees for a flat file map; returns the root oid.
fn write_git_tree(
    repo: &Repo,
    git_dir: &Path,
    files: &BTreeMap<String, (u64, Gid)>,
    prefix: &str,
    tree_count: &mut usize,
) -> Result<crate::git_io::Oid, Error> {
    // Names under this prefix, in git canonical order.
    let prefix_slash = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let mut names: Vec<String> = Vec::new();
    for path in files.keys() {
        let rest = match path.strip_prefix(&prefix_slash) {
            Some(r) => r,
            None => continue,
        };
        if let Some(seg) = rest.split('/').next() {
            if !names.iter().any(|n| n == seg) {
                names.push(seg.to_string());
            }
        }
    }
    let mut entries: Vec<crate::git_io::TreeEntry> = Vec::new();
    for name in names {
        let child = format!("{prefix_slash}{name}");
        let is_dir = files.keys().any(|p| p.starts_with(&format!("{child}/")));
        if is_dir {
            let subtree = write_git_tree(repo, git_dir, files, &child, tree_count)?;
            entries.push(crate::git_io::TreeEntry {
                mode: 0o040000,
                name,
                oid: subtree,
            });
        } else {
            let (mode, blob) = files
                .get(&child)
                .ok_or_else(|| Error::Invalid(format!("missing file entry {child}")))?;
            // Blobs store raw content in the canonical envelope; git wants
            // the raw bytes (blob_bytes extracts them).
            let blob_obj = repo.load(blob)?;
            let bytes: Vec<u8> = blob_obj
                .blob_bytes()
                .ok_or_else(|| Error::Invalid(format!("{blob} is not a blob object")))?
                .to_vec();
            let oid = crate::git_io::hash_object("blob", &bytes);
            crate::git_io::write_loose(git_dir, &oid, "blob", &bytes)?;
            entries.push(crate::git_io::TreeEntry {
                mode: *mode as u32,
                name,
                oid,
            });
        }
    }
    let tree_bytes = crate::git_io::encode_tree(entries);
    let oid = crate::git_io::hash_object("tree", &tree_bytes);
    crate::git_io::write_loose(git_dir, &oid, "tree", &tree_bytes)?;
    *tree_count += 1;
    Ok(oid)
}

/// The ordered trailer lines for a change (deterministic, sorted).
fn export_trailers(repo: &Repo, change: &Gid, include_claims: bool) -> Result<Vec<String>, Error> {
    let obj = repo.load(change)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    let mut out = Vec::new();
    out.push(format!("GEMEL-CHANGE: {change}"));
    if let Some(intent) = crate::query::gid_field(fs, 0x02) {
        out.push(format!("GEMEL-INTENT: {intent}"));
    }
    let trajectory = repo.read_ref(&format!("{}/current", crate::store::REF_TRAJECTORIES))?;
    if let Some(t) = trajectory {
        out.push(format!("GEMEL-TRAJECTORY: {t}"));
    }
    if include_claims {
        let mut claims: Vec<String> = crate::query::gid_list(fs, 0x0C)
            .iter()
            .map(|g| format!("GEMEL-CLAIM: {g}"))
            .collect();
        claims.sort();
        out.extend(claims);
    }
    out.push(format!("GEMEL-EXPORT-VERSION: {EXPORT_VERSION}"));
    Ok(out)
}

/// The loss documentation for one exported commit (GIT_INTEROP.md §3.3).
fn export_loss(created_at: i64, producer_human: bool) -> Vec<String> {
    let mut loss = Vec::new();
    if created_at == 0 {
        loss.push("commit timestamp substituted with epoch sentinel".to_string());
    }
    if !producer_human {
        loss.push("author substituted with Gemel Producer identity".to_string());
    }
    loss.push("agent reasoning omitted".to_string());
    loss.push("evidence payloads omitted".to_string());
    loss.push("residual dispositions omitted".to_string());
    loss.push("verification scope omitted".to_string());
    loss
}

/// The full causal-parent closure of the head, in deterministic topological
/// order (parents before children; DFS post-order over the recorded parent
/// order). Every change a merge commit references is exported.
pub fn head_closure(repo: &Repo) -> Result<Vec<Gid>, Error> {
    let mut order = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let head = repo.read_ref(REF_HEAD)?;
    if let Some(h) = head {
        visit_closure(repo, h, &mut visited, &mut order)?;
    }
    Ok(order)
}

fn visit_closure(
    repo: &Repo,
    gid: Gid,
    visited: &mut std::collections::HashSet<Gid>,
    order: &mut Vec<Gid>,
) -> Result<(), Error> {
    if !visited.insert(gid) {
        return Ok(());
    }
    let obj = repo.load(&gid)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    for p in crate::query::gid_list(fs, 0x11) {
        visit_closure(repo, p, visited, order)?;
    }
    order.push(gid);
    Ok(())
}

/// Projects the head change chain into `opts.git_dir` as Git commits.
/// Deterministic: no wall clock; identical repositories export identical
/// bytes (GIT_INTEROP.md §3.4).
pub fn export_git(repo: &Repo, opts: &ExportGitOptions) -> Result<ExportOutcome, Error> {
    let chain = head_closure(repo)?;
    let git_dir = &opts.git_dir;
    std::fs::create_dir_all(git_dir.join("objects"))?;
    std::fs::create_dir_all(git_dir.join("refs").join("heads"))?;
    // Reuse any already-exported mapping: change gid → git oid (re-exports
    // are append-only and deterministic).
    let mut change_oid: HashMap<Gid, crate::git_io::Oid> = HashMap::new();
    let mut outcome = ExportOutcome {
        commits: 0,
        trees: 0,
        mappings: 0,
        head_oid: String::new(),
        branch: opts.branch.clone(),
    };
    let mapping_producer = repo.insert_object(&crate::defaults::automation_producer_object_at(
        "gemel-export",
        0,
    ))?;
    for change in &chain {
        let obj = repo.load(change)?;
        let fs = obj.field_sequence().unwrap_or(&[]);
        let resulting = crate::query::gid_field(fs, 0x05)
            .ok_or_else(|| Error::Invalid(format!("change {change} has no resulting state")))?;
        let files = crate::content::state_files(repo, &resulting)?;
        let mut tree_count = 0usize;
        let tree_oid = write_git_tree(repo, git_dir, &files, "", &mut tree_count)?;
        outcome.trees += tree_count;
        // Parents: every causal parent in deterministic (gid-sorted) order.
        let mut parents: Vec<crate::git_io::Oid> = crate::query::gid_list(fs, 0x11)
            .iter()
            .filter_map(|p| change_oid.get(p).copied())
            .collect();
        parents.sort();
        // Author + timestamp policy.
        let (author, created_at) = export_author(repo, change)?;
        let producer_human = author != GEMEL_PRODUCER_AUTHOR;
        let trailers = export_trailers(repo, change, opts.include_claims)?;
        let mut message = crate::query::str_field(fs, 0x01)
            .unwrap_or("change")
            .to_string();
        if !trailers.is_empty() {
            message.push_str("\n\n");
            message.push_str(&trailers.join("\n"));
        }
        let commit = crate::git_io::Commit {
            tree: tree_oid,
            parents,
            author: person_line(&author, created_at / 1000),
            committer: person_line(GEMEL_COMMITTER, created_at / 1000),
            message,
        };
        let commit_bytes = crate::git_io::encode_commit(&commit);
        let commit_oid = crate::git_io::hash_object("commit", &commit_bytes);
        crate::git_io::write_loose(git_dir, &commit_oid, "commit", &commit_bytes)?;
        change_oid.insert(*change, commit_oid);
        outcome.commits += 1;
        outcome.head_oid = commit_oid.to_string();
        // Mapping objects (canonical Tier 0): commit and tree.
        let loss = export_loss(created_at, producer_human);
        insert_mapping(
            repo,
            "export",
            "git_commit",
            &commit_oid.to_string(),
            *change,
            &loss,
            &mapping_producer,
        )?;
        insert_mapping(
            repo,
            "export",
            "git_tree",
            &tree_oid.to_string(),
            resulting,
            &loss,
            &mapping_producer,
        )?;
        outcome.mappings += 2;
    }
    if !outcome.head_oid.is_empty() {
        let branch_oid = crate::git_io::Oid::parse(&outcome.head_oid)?;
        crate::git_io::write_branch(git_dir, &opts.branch, &branch_oid)?;
        crate::git_io::write_head(git_dir, &opts.branch)?;
    }
    Ok(outcome)
}

/// Inserts a mapping object and anchors it under `refs/mappings/<origin>/`.
/// Export- and import-created mappings live in distinct sub-namespaces so a
/// re-import never mistakes an export mapping for a prior import.
fn insert_mapping(
    repo: &Repo,
    origin: &str,
    kind: &str,
    from: &str,
    to: Gid,
    loss: &[String],
    producer: &Gid,
) -> Result<Gid, Error> {
    let fields = vec![
        Field::new(0x01, Value::Str(kind.to_string())),
        Field::new(0x02, Value::Str(from.to_string())),
        Field::new(0x03, Value::Gid(to)),
        Field::new(
            0x04,
            Value::Record(vec![
                Field::new(
                    0x01,
                    Value::Array(loss.iter().cloned().map(Value::Str).collect()),
                ),
                Field::new(0x02, Value::Array(vec![])),
                Field::new(0x03, Value::Array(vec![])),
            ]),
        ),
        Field::new(0x05, Value::Gid(*producer)),
        Field::new(0x06, Value::I(0)),
    ];
    let mapping = repo.insert_object(&Object::fields(Family::Mapping, fields))?;
    let ops = vec![RefOp::set(
        &format!("{REF_MAPPINGS}/{origin}/{kind}/{from}"),
        mapping,
    )];
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(())
    })?;
    Ok(mapping)
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Parses `GEMEL-*` trailers from a commit message. Returns (trailers,
/// remainder-without-trailers) — hostile values are reported and ignored.
fn parse_gemel_trailers(message: &str) -> (Vec<(String, String)>, String) {
    let mut out = Vec::new();
    let lines: Vec<&str> = message.lines().collect();
    // Git trailer block: contiguous trailing lines of `Key: value`.
    let mut i = lines.len();
    while i > 0 {
        let line = lines[i - 1].trim_end();
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_uppercase();
            if key.starts_with("GEMEL-") {
                out.push((key, v.trim().to_string()));
                i -= 1;
                continue;
            }
        }
        break;
    }
    let body_end = i;
    // Re-join the message without the trailer block.
    let body = lines[..body_end].join("\n");
    (out, body)
}

/// The gemel change a `GEMEL-CHANGE` trailer links to, if it validates.
fn validated_trailer_gid(repo: &Repo, value: &str) -> Option<Gid> {
    let gid = value.parse::<Gid>().ok()?;
    if gid.family() != Family::Change {
        return None;
    }
    if repo.read_object(&gid).is_err() {
        return None;
    }
    Some(gid)
}

/// The deterministic producer for a Git person line: a `human` producer with
/// the author's identity and the commit's timestamp (never the wall clock),
/// or the `unknown` producer when no identity is reliable.
fn import_producer(
    repo: &Repo,
    person: &str,
    ts: i64,
    cache: &mut HashMap<String, Gid>,
    _unknown: &Gid,
) -> Result<Gid, Error> {
    let name = crate::git_adapter::person_name(person);
    let email = crate::git_adapter::person_email(person);
    let key = format!("{name}|{email:?}");
    if let Some(g) = cache.get(&key) {
        return Ok(*g);
    }
    let obj = crate::defaults::human_producer_object_at(&name, email.as_deref(), ts);
    let gid = repo.insert_object(&obj)?;
    cache.insert(key, gid);
    Ok(gid)
}

/// Builds a Gemel state from a Git tree (via the adapter; handles packed
/// repositories). Gitlinks (submodules) are skipped with a loss note.
fn state_from_git_tree(
    repo: &Repo,
    git_dir: &Path,
    tree: &str,
    loss: &mut Vec<String>,
) -> Result<Gid, Error> {
    let entries = crate::git_adapter::ls_tree_recursive(git_dir, tree)?;
    let mut files: BTreeMap<String, (u64, Gid)> = BTreeMap::new();
    for e in entries {
        match e.mode {
            0o100644 | 0o100755 | 0o120000 => {
                let content = crate::git_adapter::cat_blob(git_dir, &e.oid)?;
                let blob = repo.insert_object(&Object::blob(content))?;
                files.insert(e.path.clone(), (e.mode.into(), blob));
            }
            0o160000 => {
                loss.push(format!(
                    "gitlink {} omitted (submodule content unavailable)",
                    e.path
                ));
            }
            other => {
                return Err(Error::Invalid(format!(
                    "unsupported git tree mode {other:o} at {}",
                    e.path
                )))
            }
        }
    }
    crate::content::build_state_from_files(repo, &files)
}

/// Imports a Git history (topological order) into Gemel. Deterministic:
/// identical Git repositories import to identical Gemel objects
/// (GIT_INTEROP.md §4).
pub fn import_git(repo: &Repo, opts: &ImportGitOptions) -> Result<ImportOutcome, Error> {
    let git_dir = &opts.git_dir;
    let head = if opts.head.is_empty() {
        "HEAD"
    } else {
        &opts.head
    };
    let commits = crate::git_adapter::rev_list_topo_reverse(git_dir, head)?;
    let mut outcome = ImportOutcome {
        commits: commits.len(),
        changes: 0,
        trajectories: 0,
        mappings: 0,
        relinked: 0,
        ignored_trailers: 0,
        unknown_producers: 0,
    };
    let git_import_producer = repo.insert_object(&crate::defaults::git_import_producer_object())?;
    let unknown_producer = repo.insert_object(&crate::defaults::unknown_producer_object())?;
    let mut producer_cache: HashMap<String, Gid> = HashMap::new();
    // oid → imported change gid (for causal parents and mappings).
    let mut change_of: HashMap<String, Gid> = HashMap::new();
    // oid → trajectory gid (first-parent chain grouping).
    let mut trajectory_of: HashMap<String, Gid> = HashMap::new();
    let mut traj_cache: HashMap<Gid, Gid> = HashMap::new();
    let mut created_trajectories = 0usize;
    let mut name_ops: Vec<RefOp> = Vec::new();
    // The final (change, resulting state, trajectory) becomes the imported
    // head (GIT_INTEROP.md §6: the imported history is navigable).
    let mut last_change: Option<Gid> = None;
    let mut last_state: Option<Gid> = None;
    let mut last_trajectory: Option<Gid> = None;

    // Idempotence: a commit whose import mapping already exists was imported
    // before; importing again must not duplicate objects (GIT_INTEROP.md §4).
    for oid in &commits {
        if repo
            .read_ref(&format!("{REF_MAPPINGS}/import/git_commit/{oid}"))?
            .is_some()
        {
            continue;
        }
        let c = crate::git_adapter::cat_commit(git_dir, oid)?;
        // Git timestamps are seconds; Gemel metadata uses milliseconds.
        // Converting once here keeps the unit convention consistent across
        // native changes and imported ones (re-export divides by 1000 once).
        let author_ts = crate::git_adapter::person_timestamp(&c.author);
        let ts_ms = author_ts.saturating_mul(1000);
        let (trailers, body) = parse_gemel_trailers(&c.message);
        // Trailer re-linking (GIT_INTEROP.md §4.3): validated GEMEL-CHANGE
        // identities are recorded in the mapping; hostile values ignored.
        let mut relinked_change: Option<Gid> = None;
        for (k, v) in &trailers {
            if k == "GEMEL-CHANGE" {
                match validated_trailer_gid(repo, v) {
                    Some(g) => {
                        relinked_change = Some(g);
                        outcome.relinked += 1;
                    }
                    None => outcome.ignored_trailers += 1,
                }
            } else if k == "GEMEL-EXPORT-VERSION" && v != EXPORT_VERSION {
                outcome.ignored_trailers += 1;
            }
        }
        // Producer: synthetic human from author identity (foreign Git) or the
        // deterministic git_import producer; unknown when no identity exists.
        let author_name = crate::git_adapter::person_name(&c.author);
        let author_email = crate::git_adapter::person_email(&c.author);
        let producer = if author_name == "unknown" && author_email.is_none() {
            outcome.unknown_producers += 1;
            unknown_producer
        } else {
            import_producer(
                repo,
                &c.author,
                ts_ms,
                &mut producer_cache,
                &unknown_producer,
            )?
        };
        // States.
        let mut loss: Vec<String> = Vec::new();
        let resulting = state_from_git_tree(repo, git_dir, &c.tree, &mut loss)?;
        let input = match c.parents.first() {
            Some(p) => {
                let parent_tree = crate::git_adapter::cat_commit(git_dir, p)?.tree;
                state_from_git_tree(repo, git_dir, &parent_tree, &mut loss)?
            }
            None => crate::content::build_state_from_files(repo, &BTreeMap::new())?,
        };
        // Operations from the deterministic delta.
        let deltas = crate::content::diff_states(repo, &input, &resulting)?;
        let operations = crate::content::synthesize_operations_ts(repo, &deltas, &producer, ts_ms)?;
        // Causal parents (imported first-parent chain order).
        let causal_parents: Vec<Gid> = c
            .parents
            .iter()
            .filter_map(|p| change_of.get(p).copied())
            .collect();
        // The change object (tags mirror workflow::finish_change).
        let mut change_fields = vec![Field::new(
            0x01,
            Value::Str(body.lines().next().unwrap_or("imported change").to_string()),
        )];
        if !causal_parents.is_empty() {
            change_fields.push(Field::new(0x03, Value::Gid(input)));
        }
        if !operations.is_empty() {
            change_fields.push(Field::new(
                0x04,
                Value::Array(operations.iter().copied().map(Value::Gid).collect()),
            ));
        }
        change_fields.push(Field::new(0x05, Value::Gid(resulting)));
        change_fields.push(Field::new(0x06, Value::Gid(producer)));
        if !causal_parents.is_empty() {
            change_fields.push(Field::new(
                0x11,
                Value::Array(causal_parents.iter().copied().map(Value::Gid).collect()),
            ));
        }
        change_fields.push(Field::new(0x15, Value::I(ts_ms)));
        let change = repo.insert_object(&Object::fields(Family::Change, change_fields))?;
        change_of.insert(oid.clone(), change);
        outcome.changes += 1;
        last_change = Some(change);
        last_state = Some(resulting);
        // Names continue the local counters (change/state/trajectory share
        // one namespace with local work; GIT_INTEROP.md §4.1).
        let cname = crate::workflow::next_name(repo, "change")?;
        name_ops.push(RefOp::set(
            &format!("{}/{cname}", crate::store::REF_NAMES),
            change,
        ));
        let sname = crate::workflow::next_name(repo, "state")?;
        name_ops.push(RefOp::set(
            &format!("{}/{sname}", crate::store::REF_NAMES),
            resulting,
        ));

        // Trajectory: first-parent chain grouping (GIT_INTEROP.md §4.1).
        let traj = match c.parents.first().and_then(|p| trajectory_of.get(p)) {
            Some(existing) => *existing,
            None => {
                let tfs = vec![
                    Field::new(0x04, Value::Gid(producer)),
                    Field::new(0x06, Value::Array(vec![Value::Gid(change)])),
                    Field::new(0x0D, Value::I(ts_ms)),
                    Field::new(0x0E, Value::I(ts_ms)),
                ];
                let t = repo.insert_object(&Object::fields(Family::Trajectory, tfs))?;
                let tname = crate::workflow::next_name(repo, "trajectory")?;
                name_ops.push(RefOp::set(
                    &format!("{}/{}", crate::store::REF_TRAJECTORIES, tname),
                    t,
                ));
                created_trajectories += 1;
                traj_cache.insert(t, t);
                t
            }
        };
        trajectory_of.insert(oid.clone(), traj);
        last_trajectory = Some(traj);

        // Mapping: to = the original change when a validated trailer re-links,
        // else the imported change; loss documents the unknowns.
        let mapping_to = relinked_change.unwrap_or(change);
        if relinked_change.is_none() {
            // Foreign provenance Git cannot supply: documented as unknowns,
            // never fabricated (GIT_INTEROP.md §4.2).
            loss.push("intent unknown".to_string());
            loss.push("claims unknown".to_string());
            loss.push("evidence unknown".to_string());
            loss.push("residuals unknown".to_string());
            loss.push("verification scope unknown".to_string());
        }
        insert_mapping(
            repo,
            "import",
            "git_commit",
            oid,
            mapping_to,
            &loss,
            &git_import_producer,
        )?;
        insert_mapping(
            repo,
            "import",
            "git_tree",
            &c.tree,
            resulting,
            &loss,
            &git_import_producer,
        )?;
        outcome.mappings += 2;
    }
    outcome.trajectories = created_trajectories;
    if !name_ops.is_empty() {
        repo.with_write_lock(|| {
            repo.apply_refs_unlocked(&RefTransaction { ops: name_ops })?;
            Ok(())
        })?;
    }
    // Wire the imported history as the repository head (navigable via the
    // ordinary query surface). Only when no local head exists — a repository
    // with local work is never overwritten.
    if repo.read_ref(crate::store::REF_HEAD)?.is_none() {
        let mut ops = Vec::new();
        if let (Some(c), Some(s)) = (last_change, last_state) {
            ops.push(RefOp::set(crate::store::REF_HEAD, c));
            ops.push(RefOp::set(crate::store::REF_STATE_HEAD, s));
        }
        if let Some(t) = last_trajectory {
            ops.push(RefOp::set(
                &format!("{}/current", crate::store::REF_TRAJECTORIES),
                t,
            ));
        }
        if !ops.is_empty() {
            repo.with_write_lock(|| {
                repo.apply_refs_unlocked(&RefTransaction { ops })?;
                Ok(())
            })?;
        }
    }
    let _ = traj_cache;
    Ok(outcome)
}
