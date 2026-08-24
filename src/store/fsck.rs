//! Repository verification (STORAGE.md §8, INVARIANTS.md §12).
//!
//! `fsck` verifies: envelope/hash of every object file, schema validity,
//! reference resolution (missing vs. pruned), acyclicity, ref validity,
//! index consistency, workspace metadata, and journal state. Exit codes:
//! 0 clean, 1 repairs made, 2 corruption found.

use crate::decode::decode_object;
use crate::gid::Gid;
use crate::hash::object_id_bytes;
use crate::store::index;
use crate::store::objects;
use crate::store::refs;
use crate::store::tombstone;
use crate::store::{Error, ReadOutcome, Repo};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Options controlling the fsck run.
#[derive(Debug, Clone, Default)]
pub struct FsckOptions {
    /// Repair rebuildable artifacts (index rebuild, journal recovery).
    pub repair: bool,
    /// Force an index rebuild (implies repair of the index).
    pub rebuild_index: bool,
    /// Verbose output.
    pub verbose: bool,
}

/// A problem found by fsck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub id: Option<Gid>,
}

/// Problem severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// The fsck report.
#[derive(Debug, Clone, Default)]
pub struct FsckReport {
    pub objects_scanned: usize,
    pub objects_ok: usize,
    pub problems: Vec<Problem>,
    pub repairs: Vec<String>,
    pub index_drift: Vec<String>,
    pub journal_recovered: bool,
}

impl FsckReport {
    /// The documented exit code (STORAGE.md §8).
    pub fn exit_code(&self) -> u8 {
        let has_errors = self.problems.iter().any(|p| p.severity == Severity::Error);
        if has_errors {
            2
        } else if !self.repairs.is_empty() || self.journal_recovered {
            1
        } else {
            0
        }
    }

    pub fn is_clean(&self) -> bool {
        self.exit_code() == 0
    }
}

/// Runs the full verification.
pub fn run(repo: &Repo, opts: &FsckOptions) -> Result<FsckReport, Error> {
    let mut report = FsckReport::default();

    // -- 1. Object files: envelope, hash, schema --------------------------
    let mut on_disk: HashMap<Gid, u64> = HashMap::new();
    for path in objects::scan(repo.meta_dir())? {
        report.objects_scanned += 1;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "unreadable-object",
                    message: format!("{}: {e}", path.display()),
                    id: None,
                });
                continue;
            }
        };
        let id = match verify_envelope(repo, &bytes) {
            Ok(id) => id,
            Err(problem) => {
                report.problems.push(problem);
                continue;
            }
        };
        // Filename must match the identity digest.
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let hex = crate::hex::encode(id.digest());
        if file_name != format!("{hex}.gce") {
            report.problems.push(Problem {
                severity: Severity::Error,
                code: "filename-mismatch",
                message: format!("{}: file name does not match identity", path.display()),
                id: Some(id),
            });
        }
        report.objects_ok += 1;
        on_disk.insert(id, bytes.len() as u64);
    }

    // Stray files in the objects tree.
    for (code, message) in stray_files(repo.meta_dir()) {
        report.problems.push(Problem {
            severity: Severity::Warning,
            code,
            message,
            id: None,
        });
    }

    // -- 2/3/4. Reachability, references, cycles --------------------------
    let mut reachable: HashMap<Gid, Vec<Gid>> = HashMap::new();
    let mut queue: Vec<Gid> = Vec::new();
    for (name, gid) in refs::all(repo.meta_dir())? {
        queue.push(gid);
        if opts.verbose {
            eprintln!("fsck: ref {name} -> {gid}");
        }
    }
    let mut visited: HashSet<Gid> = HashSet::new();
    while let Some(id) = queue.pop() {
        if !visited.insert(id) {
            continue;
        }
        match repo.read_object(&id) {
            Ok(ReadOutcome::Object(obj)) => {
                let edges: Vec<Gid> = index::edges_of(&obj)
                    .into_iter()
                    .map(|(_, to, _)| to)
                    .collect();
                reachable.insert(id, edges.clone());
                queue.extend(edges);
            }
            Ok(ReadOutcome::Pruned(t)) => {
                report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "pruned-referenced",
                    message: format!(
                        "reference to pruned object {id} (tier {} rule {})",
                        t.policy_tier, t.policy_rule
                    ),
                    id: Some(id),
                });
            }
            Err(Error::ObjectNotFound(_)) => {
                report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "missing-reference",
                    message: format!("reference to missing object {id}"),
                    id: Some(id),
                });
            }
            Err(Error::ObjectCorrupt { detail, .. }) => {
                report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "corrupt-object",
                    message: format!("{id}: {detail}"),
                    id: Some(id),
                });
            }
            Err(e) => return Err(e),
        }
    }

    // Cycle detection (three-color DFS over the reachable graph).
    if let Some(cycle) = find_cycle(&reachable) {
        report.problems.push(Problem {
            severity: Severity::Error,
            code: "cycle",
            message: format!("object graph contains a cycle at {cycle}"),
            id: Some(cycle),
        });
    }

    // -- 5. Refs -----------------------------------------------------------
    for (name, _gid) in refs::all(repo.meta_dir())? {
        if let Err(e) = refs::read(repo.meta_dir(), &name) {
            report.problems.push(Problem {
                severity: Severity::Error,
                code: "corrupt-ref",
                message: format!("{name}: {e}"),
                id: None,
            });
        }
    }

    // -- 6. Index consistency ---------------------------------------------
    let index_stale = index::is_stale(repo).unwrap_or(false);
    if index_stale {
        report.index_drift.push("index flagged stale".into());
    }
    match (index::refs_mirror(repo), refs::all(repo.meta_dir())) {
        (Ok(mirror), Ok(actual)) => {
            let mirror_set: HashMap<String, String> = mirror
                .into_iter()
                .map(|(n, g)| (n, g.to_string()))
                .collect();
            let actual_set: HashMap<String, String> = actual
                .into_iter()
                .map(|(n, g)| (n, g.to_string()))
                .collect();
            if mirror_set != actual_set {
                report
                    .index_drift
                    .push("index refs mirror differs from on-disk refs".into());
            }
        }
        _ => {
            report.index_drift.push("index unreadable".into());
        }
    }
    match index::indexed_objects(repo) {
        Ok(indexed) => {
            let indexed_set: HashSet<String> = indexed.into_iter().map(|(id, _)| id).collect();
            let disk_set: HashSet<String> = on_disk.keys().map(|g| g.to_string()).collect();
            if indexed_set != disk_set {
                report.index_drift.push(format!(
                    "index objects differ from disk ({} indexed, {} on disk)",
                    indexed_set.len(),
                    disk_set.len()
                ));
            }
        }
        Err(_) => {
            report.index_drift.push("index unreadable".into());
        }
    }
    if !report.index_drift.is_empty() {
        report.problems.push(Problem {
            severity: Severity::Error,
            code: "index-inconsistent",
            message: format!(
                "derived index inconsistent ({} drift items)",
                report.index_drift.len()
            ),
            id: None,
        });
    }

    // -- 7. Workspace metadata --------------------------------------------
    check_workspaces(repo, &mut report);

    // -- 8. Journal --------------------------------------------------------
    if let Ok(content) = std::fs::read_to_string(refs::journal_path(repo.meta_dir())) {
        if !content.trim().is_empty() {
            report.problems.push(Problem {
                severity: Severity::Error,
                code: "interrupted-transaction",
                message: "journal contains an interrupted transaction".into(),
                id: None,
            });
        }
    }

    // -- Repair (derived artifacts only) ----------------------------------
    if opts.repair || opts.rebuild_index {
        if opts.rebuild_index || !report.index_drift.is_empty() {
            match repo.with_write_lock(|| index::rebuild(repo)) {
                Ok(()) => {
                    report.repairs.push("rebuilt derived index".into());
                    report.problems.retain(|p| p.code != "index-inconsistent");
                    report.index_drift.clear();
                }
                Err(e) => report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "repair-failed",
                    message: format!("index rebuild failed: {e}"),
                    id: None,
                }),
            }
        }
        let journal_has_content = std::fs::read_to_string(refs::journal_path(repo.meta_dir()))
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        if journal_has_content {
            // Recover under the writer lock. Rolling back an interrupted
            // transaction changes the canonical ref set, so the derived index
            // must be rebuilt to stay consistent.
            let recovered = repo.with_write_lock(|| {
                let did = refs::recover_unlocked(repo.meta_dir())?;
                if did {
                    index::rebuild(repo)?;
                }
                Ok(did)
            });
            match recovered {
                Ok(true) => {
                    report
                        .repairs
                        .push("recovered interrupted journal transaction".into());
                    report
                        .problems
                        .retain(|p| p.code != "interrupted-transaction");
                }
                Ok(false) => {}
                Err(e) => report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "repair-failed",
                    message: format!("journal recovery failed: {e}"),
                    id: None,
                }),
            }
        }
    }

    Ok(report)
}

fn verify_envelope(repo: &Repo, bytes: &[u8]) -> Result<Gid, Problem> {
    let limits = repo.limits();
    let obj = decode_object(bytes, &limits).map_err(|e| Problem {
        severity: Severity::Error,
        code: "invalid-object",
        message: format!("decode failed: {e}"),
        id: None,
    })?;
    let digest = object_id_bytes(bytes);
    let id = Gid::new(obj.family, digest);
    Ok(id)
}

fn stray_files(meta: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let objects_dir = meta.join("objects");
    if let Ok(shards) = std::fs::read_dir(&objects_dir) {
        for shard in shards.flatten() {
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(shard.path()) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(".tmp-") {
                        out.push((
                            "stale-temp-file",
                            format!("stale temp file: {}", entry.path().display()),
                        ));
                    } else if !name.ends_with(".gce") && !name.ends_with(".tomb") {
                        out.push((
                            "stray-file",
                            format!("stray file: {}", entry.path().display()),
                        ));
                    }
                }
            }
        }
    }
    out
}

fn check_workspaces(repo: &Repo, report: &mut FsckReport) {
    let worktrees = repo.meta_dir().join("worktrees");
    let entries = match std::fs::read_dir(&worktrees) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let state_ref = dir.join("state.ref");
        if let Ok(text) = std::fs::read_to_string(&state_ref) {
            match text.trim().parse::<Gid>() {
                Ok(gid) => {
                    if !tombstone::exists(repo.meta_dir(), &gid).unwrap_or(false)
                        && !objects::exists(repo.meta_dir(), &gid).unwrap_or(false)
                    {
                        report.problems.push(Problem {
                            severity: Severity::Error,
                            code: "workspace-state-missing",
                            message: format!(
                                "workspace {} state.ref resolves to missing {gid}",
                                entry.file_name().to_string_lossy()
                            ),
                            id: Some(gid),
                        });
                    }
                }
                Err(e) => report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "workspace-state-corrupt",
                    message: format!(
                        "workspace {} state.ref unparseable: {e}",
                        entry.file_name().to_string_lossy()
                    ),
                    id: None,
                }),
            }
        }
        let pending = dir.join("pending.json");
        if let Ok(text) = std::fs::read_to_string(&pending) {
            if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                report.problems.push(Problem {
                    severity: Severity::Error,
                    code: "pending-corrupt",
                    message: format!(
                        "workspace {} pending.json unparseable",
                        entry.file_name().to_string_lossy()
                    ),
                    id: None,
                });
            }
        }
    }
}

/// Three-color DFS cycle detection; returns a node on a cycle if one exists.
fn find_cycle(graph: &HashMap<Gid, Vec<Gid>>) -> Option<Gid> {
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    let mut color: HashMap<Gid, u8> = HashMap::new();
    for start in graph.keys() {
        if color.get(start).copied().unwrap_or(WHITE) != WHITE {
            continue;
        }
        // Iterative DFS.
        let mut stack: Vec<(Gid, bool)> = vec![(*start, false)];
        while let Some((node, exiting)) = stack.pop() {
            let c = color.get(&node).copied().unwrap_or(WHITE);
            if exiting {
                color.insert(node, BLACK);
                continue;
            }
            if c == GRAY {
                return Some(node); // back edge
            }
            if c == BLACK {
                continue;
            }
            color.insert(node, GRAY);
            stack.push((node, true));
            if let Some(neighbors) = graph.get(&node) {
                for n in neighbors {
                    let nc = color.get(n).copied().unwrap_or(WHITE);
                    if nc == WHITE {
                        stack.push((*n, false));
                    } else if nc == GRAY {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}
