//! Exchange ingestion (EXCHANGE.md §11–§12, §15–§20, §34, §37).
//!
//! Quarantine flow: discover → validate descriptor → validate pack identity →
//! decode → verify ids/schemas → verify coverage → promote objects → publish
//! imported-frontier state. No active ref is updated until the entire
//! relevant frontier passed validation. Idempotent. Never executes anything.

use super::export::{content_state_identity, is_imported, mark_imported, working_tree_files};
use super::{decode_pack, discover_frontiers, ExchangeLimits, Frontier};
use crate::gid::Gid;
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::{
    Error, InitOptions, Repo, REF_CONFIG, REF_HEAD, REF_NAMES, REF_STATE_HEAD, REF_TRAJECTORIES,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// The ref namespace for imported frontiers.
pub const REF_EXCHANGE: &str = "refs/exchange";
/// The ref namespace for imported frontiers' head changes.
pub const REF_EXCHANGE_FRONTIERS: &str = "refs/exchange/frontiers";

/// The status of the source-state binding for a frontier (EXCHANGE.md §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBinding {
    /// The frontier's source_state equals the current source content state.
    Matched,
    /// The frontier is valid but does not describe the current source.
    Diverged,
}

/// The result of a full ingest pass.
#[derive(Debug, Clone)]
pub struct IngestOutcome {
    pub frontiers_found: usize,
    pub frontiers_imported: usize,
    pub frontiers_already_imported: usize,
    pub packs_processed: usize,
    pub objects_promoted: usize,
    pub current_source_state: Option<Gid>,
    pub matching: Vec<String>,
    pub diverged: Vec<String>,
    pub activated: Option<String>,
    /// The Config object carried by the activated frontier (bootstrap adopts
    /// the imported config instead of the init-time default; EXCHANGE.md
    /// §17).
    pub imported_config: Option<String>,
    /// The semantic index of the activated state, when the imported
    /// frontier carried exactly one (Phase 5; refs/semantic re-established
    /// over the imported derived objects).
    pub semantic_index: Option<String>,
}

/// Mutable accumulators threaded through an ingestion pass.
struct IngestAccum {
    total_bytes: u64,
    promoted_config: Option<Gid>,
    processed_packs: HashMap<String, bool>,
    imported_indexes: Vec<(Gid, Gid)>,
    imported_changes: Vec<Gid>,
    imported_states: Vec<Gid>,
    imported_trajectories: Vec<Gid>,
}

/// Quarantine-validates and promotes every pack referenced by a frontier and
/// its parent chain (transitive, deduplicated). Returns the promoted object
/// count. Validation is iterative (bounded reference depth; no recursion),
/// and promotion happens only after the entire frontier closure passed
/// validation (EXCHANGE.md §11, §15, §37).
fn ingest_frontier_objects(
    repo: &Repo,
    meta: &Path,
    root_frontier: &Frontier,
    limits: &ExchangeLimits,
    accum: &mut IngestAccum,
) -> Result<usize, Error> {
    // Iterative DFS over the parent chain; depth is tracked explicitly so a
    // malicious chain fails with a structured limit error, never a stack
    // overflow.
    struct Frame {
        frontier: Box<Frontier>,
        depth: usize,
    }
    let mut stack = vec![Frame {
        frontier: Box::new(root_frontier.clone()),
        depth: 0,
    }];
    let mut pending: Vec<(Gid, Vec<u8>)> = Vec::new();
    while let Some(frame) = stack.pop() {
        if frame.depth > limits.max_reference_depth {
            return Err(Error::Limit {
                kind: "exchange parent frontier depth",
                limit: limits.max_reference_depth as u64,
                found: frame.depth as u64,
            });
        }
        require_supported_schemas(&frame.frontier)?;
        for parent_hex in &frame.frontier.parent_frontiers {
            let parent_id = super::hex_to_digest(parent_hex).ok_or_else(|| {
                Error::Invalid(format!("malformed parent frontier id {parent_hex:?}"))
            })?;
            let parent_path = super::frontier_path(meta, &parent_id);
            if !parent_path.exists() {
                return Err(Error::Invalid(format!(
                    "frontier references missing parent frontier {parent_hex}"
                )));
            }
            let parent_bytes = super::read_frontier_file(&parent_path)?;
            if super::frontier_id(&parent_bytes) != parent_id {
                return Err(Error::Invalid(format!(
                    "parent frontier {parent_hex} fails identity verification"
                )));
            }
            let parent = super::parse_frontier(&parent_bytes, limits)?;
            stack.push(Frame {
                frontier: Box::new(parent),
                depth: frame.depth + 1,
            });
        }
        // This frontier's packs: decode + verify (quarantine) — nothing is
        // promoted until the whole closure passed.
        for pack_hex in &frame.frontier.packs {
            let pack_id = super::hex_to_digest(pack_hex)
                .ok_or_else(|| Error::Invalid(format!("malformed pack id {pack_hex:?}")))?;
            if accum.processed_packs.contains_key(pack_hex) {
                continue;
            }
            let path = super::pack_path(meta, &pack_id);
            let bytes = super::read_pack_file(&path)
                .map_err(|_| Error::Invalid(format!("referenced pack {pack_hex} is missing")))?;
            // Automatic ingestion is bounded (EXCHANGE.md §11, §37): a hostile
            // descriptor cannot force unbounded reads during `gemel status`.
            accum.total_bytes += bytes.len() as u64;
            if accum.total_bytes > limits.max_automatic_ingest_bytes {
                return Err(Error::Limit {
                    kind: "exchange automatic ingest (IMPORT_REQUIRES_EXPLICIT_APPROVAL)",
                    limit: limits.max_automatic_ingest_bytes,
                    found: accum.total_bytes,
                });
            }
            if super::pack_id(&bytes) != pack_id {
                return Err(Error::Invalid(format!(
                    "pack {pack_hex} fails identity verification"
                )));
            }
            let objects = decode_pack(&bytes, limits)?;
            for o in objects {
                pending.push((o.id, o.envelope));
            }
            accum.processed_packs.insert(pack_hex.clone(), true);
        }
    }
    // Promote after the entire frontier closure validated: verify id↔bytes
    // (already done) and detect local-native conflicts. Duplicates across
    // packs are benign content-address dedup (same id ⇒ same bytes, since
    // the id is BLAKE3 of the envelope); the genuinely fatal case — the same
    // id present locally with different bytes — is rejected by insert_bytes
    // as a HashCollision (EXCHANGE.md §42).
    let mut total = 0usize;
    for (id, envelope) in &pending {
        if id.family() == crate::family::Family::Config && accum.promoted_config.is_none() {
            accum.promoted_config = Some(*id);
        }
        // insert_bytes validates, hashes, publishes, and fails on
        // id↔bytes conflicts with local native objects.
        repo.insert_bytes(envelope)?;
        match id.family() {
            crate::family::Family::SemanticIndex => {
                // Remember (state, index) pairs so the activated frontier can
                // re-establish the semantic refs over the imported objects
                // (the index is derived knowledge carried by the frontier;
                // EXCHANGE.md §11, §56).
                if let Ok(obj) = repo.load(id) {
                    if let Some(fs) = obj.field_sequence() {
                        if let Some(state) = crate::query::gid_field(fs, 0x01) {
                            accum.imported_indexes.push((state, *id));
                        }
                    }
                }
            }
            crate::family::Family::Change => accum.imported_changes.push(*id),
            crate::family::Family::State => accum.imported_states.push(*id),
            crate::family::Family::Trajectory => accum.imported_trajectories.push(*id),
            _ => {}
        }
        total += 1;
    }
    Ok(total)
}

/// The full ingestion pass over every valid frontier on disk (EXCHANGE.md
/// §11, §16, §34). Idempotent: already-imported frontiers are skipped.
pub fn ingest(repo: &Repo) -> Result<IngestOutcome, Error> {
    ingest_with_limits(repo, &ExchangeLimits::default())
}

/// As [`ingest`] with explicit resource limits (tests, recovery, explicit
/// high-budget imports).
pub fn ingest_with_limits(repo: &Repo, limits: &ExchangeLimits) -> Result<IngestOutcome, Error> {
    let meta = repo.meta_dir().to_path_buf();
    let frontiers = discover_frontiers(&meta)?;
    let mut outcome = IngestOutcome {
        frontiers_found: frontiers.len(),
        frontiers_imported: 0,
        frontiers_already_imported: 0,
        packs_processed: 0,
        objects_promoted: 0,
        current_source_state: None,
        matching: Vec::new(),
        diverged: Vec::new(),
        activated: None,
        imported_config: None,
        semantic_index: None,
    };
    // (state, index) pairs of SemanticIndex objects promoted from packs,
    // plus the other accumulators threaded through the frontier closure.
    let mut accum = IngestAccum {
        total_bytes: 0,
        promoted_config: None,
        processed_packs: HashMap::new(),
        imported_indexes: Vec::new(),
        imported_changes: Vec::new(),
        imported_states: Vec::new(),
        imported_trajectories: Vec::new(),
    };
    // Current source content state (carrier-backed recovery of current blobs
    // happens inside build_state).
    let files = working_tree_files(repo)?;
    let current = content_state_identity(repo, &files)?;
    outcome.current_source_state = Some(current);

    // Config objects promoted per frontier (bootstrap adopts the imported
    // config on activation).
    let mut frontier_configs: HashMap<String, Gid> = HashMap::new();
    for (frontier, id, _) in &frontiers {
        if is_imported(&meta, id) {
            outcome.frontiers_already_imported += 1;
            if frontier.source_state == current {
                outcome.matching.push(crate::hex::encode(id));
            } else {
                outcome.diverged.push(crate::hex::encode(id));
            }
            continue;
        }
        require_supported_schemas(frontier)?;
        let promoted = ingest_frontier_objects(repo, &meta, frontier, limits, &mut accum)?;
        if let Some(cfg) = accum.promoted_config.take() {
            frontier_configs.insert(crate::hex::encode(id), cfg);
        }
        outcome.packs_processed += accum.processed_packs.len();
        outcome.objects_promoted += promoted;
        // Register the imported frontier ref (head change), restore human
        // names over the immutable identities, then mark local.
        let ops = vec![RefOp::set(
            &format!("{REF_EXCHANGE_FRONTIERS}/{}", crate::hex::encode(id)),
            frontier.head_change,
        )];
        repo.with_write_lock(|| {
            repo.apply_refs_unlocked(&RefTransaction { ops })?;
            restore_imported_names(repo, frontier)?;
            restore_imported_object_names(repo, &accum)?;
            Ok(())
        })?;
        mark_imported(&meta, id)?;
        outcome.frontiers_imported += 1;
        if frontier.source_state == current {
            outcome.matching.push(crate::hex::encode(id));
        } else {
            outcome.diverged.push(crate::hex::encode(id));
        }
    }
    // Activate the matching frontier: establish head/state/trajectory refs
    // (only when the repository has no local head yet — bootstrap semantics;
    // an existing repository with local work is never overwritten,
    // EXCHANGE.md §18).
    let has_local_head = repo.read_ref(REF_HEAD)?.is_some();
    if !has_local_head {
        for (frontier, id, _) in &frontiers {
            if frontier.source_state == current {
                let head = frontier.head_change;
                let cobj = repo.load(&head)?;
                let cfs = cobj.field_sequence().unwrap_or(&[]);
                let state = crate::query::gid_field(cfs, 0x05).unwrap();
                let mut ops = vec![
                    RefOp::set(REF_HEAD, head),
                    RefOp::set(REF_STATE_HEAD, state),
                ];
                if let Some(t) = frontier.trajectory {
                    ops.push(RefOp::set(&format!("{REF_TRAJECTORIES}/current"), t));
                }
                // Bootstrap adopts the config carried by the exchange
                // material (the init-time default was only a placeholder).
                if let Some(cfg) = frontier_configs.get(&crate::hex::encode(id)).copied() {
                    ops.push(RefOp::set(REF_CONFIG, cfg));
                    outcome.imported_config = Some(cfg.to_string());
                }
                repo.with_write_lock(|| {
                    repo.apply_refs_unlocked(&RefTransaction { ops })?;
                    Ok(())
                })?;
                // The imported context becomes the active frontier locally.
                super::export::record_active_frontier(&meta, id)?;
                outcome.activated = Some(crate::hex::encode(id));
                // Re-establish the semantic refs when the imported frontier
                // carried exactly one index for this state. If several
                // distinct indexes claim the same state (divergent derived
                // histories), none is activated: `gemel index` rebuilds
                // deterministically instead of guessing (Phase 5; EXCHANGE.md
                // §42: never prefer one conflicting representation).
                let candidates: BTreeSet<Gid> = accum
                    .imported_indexes
                    .iter()
                    .filter(|(s, _)| *s == state)
                    .map(|(_, i)| *i)
                    .collect();
                if candidates.len() == 1 {
                    let index = *candidates.iter().next().unwrap();
                    let state_hex = crate::hex::encode(state.digest());
                    let sop = vec![
                        RefOp::set(&format!("refs/semantic/state/{state_hex}"), index),
                        RefOp::set("refs/semantic/current", index),
                        RefOp::set("refs/semantic/head", index),
                    ];
                    repo.with_write_lock(|| {
                        repo.apply_refs_unlocked(&RefTransaction { ops: sop })?;
                        Ok(())
                    })?;
                    outcome.semantic_index = Some(index.to_string());
                }
                break; // exactly one active frontier
            }
        }
    }
    Ok(outcome)
}

/// Rejects frontiers whose `required_schemas` mention versions this client
/// cannot interpret (EXCHANGE.md §6, §15): a v1 client supports only the
/// pack/schema version 1. Fail closed on unknown mandatory semantics.
fn require_supported_schemas(frontier: &Frontier) -> Result<(), Error> {
    for v in &frontier.required_schemas {
        if *v != 1 {
            return Err(Error::Invalid(format!(
                "frontier requires unsupported schema version {v}"
            )));
        }
    }
    Ok(())
}

/// Restores deterministic human names for an imported frontier's head change,
/// resulting state, and trajectory (EXCHANGE.md §56). Names are local labels
/// over immutable identities (SPECIFICATION.md §1: naming and identity are
/// separate); they are refs, not objects, so they never travel in packs.
/// Imported names continue the repository's name counters, sharing one
/// namespace with local work. Runs only for newly imported frontiers
/// (idempotent).
fn restore_imported_names(repo: &Repo, frontier: &Frontier) -> Result<(), Error> {
    let head = frontier.head_change;
    // Best effort: if the head change is not present the names are simply
    // not restored (names are local labels over immutable identities, never
    // canonical truth).
    let cobj = match repo.load(&head) {
        Ok(o) => o,
        Err(_) => return Ok(()),
    };
    let cfs = cobj.field_sequence().unwrap_or(&[]);
    let resulting = crate::query::gid_field(cfs, 0x05);
    let mut ops = Vec::new();
    let cname = crate::workflow::next_name(repo, "change")?;
    ops.push(RefOp::set(&format!("{REF_NAMES}/{cname}"), head));
    if let Some(s) = resulting {
        let sname = crate::workflow::next_name(repo, "state")?;
        ops.push(RefOp::set(&format!("{REF_NAMES}/{sname}"), s));
    }
    if let Some(t) = frontier.trajectory {
        let tname = crate::workflow::next_name(repo, "trajectory")?;
        ops.push(RefOp::set(&format!("{REF_TRAJECTORIES}/{tname}"), t));
    }
    repo.apply_refs_unlocked(&RefTransaction { ops })
}

/// Names every imported change/state/trajectory that has no local name yet
/// (deterministic gid order). The frontier descriptor only references the
/// head change/trajectory, so sibling trajectories and their changes would
/// otherwise be orphaned — present in the store but undiscoverable by the
/// trajectory/attempts queries (EXCHANGE.md §7, §31). Names are local labels
/// that continue the repository's counters; object identities are canonical
/// and unaffected.
fn restore_imported_object_names(repo: &Repo, accum: &IngestAccum) -> Result<(), Error> {
    let mut changes: Vec<Gid> = accum.imported_changes.clone();
    let mut states: Vec<Gid> = accum.imported_states.clone();
    let mut trajectories: Vec<Gid> = accum.imported_trajectories.clone();
    changes.sort_by_key(|g| g.to_bytes());
    changes.dedup();
    states.sort_by_key(|g| g.to_bytes());
    states.dedup();
    trajectories.sort_by_key(|g| g.to_bytes());
    trajectories.dedup();
    // Only the latest version of each trajectory chain receives a name: a
    // trajectory referenced as another imported trajectory's `previous` is an
    // intermediate version, reachable through trajectory_versions.
    let mut is_previous: HashSet<Gid> = HashSet::new();
    for g in &trajectories {
        if let Ok(obj) = repo.load(g) {
            if let Some(fs) = obj.field_sequence() {
                if let Some(prev) = crate::query::gid_field(fs, 0x01) {
                    is_previous.insert(prev);
                }
            }
        }
    }
    let mut ops = Vec::new();
    for g in &changes {
        if repo.name_of(g)?.is_none() {
            let name = crate::workflow::next_name(repo, "change")?;
            ops.push(RefOp::set(&format!("{REF_NAMES}/{name}"), *g));
        }
    }
    for g in &states {
        if repo.name_of(g)?.is_none() {
            let name = crate::workflow::next_name(repo, "state")?;
            ops.push(RefOp::set(&format!("{REF_NAMES}/{name}"), *g));
        }
    }
    for g in &trajectories {
        if is_previous.contains(g) {
            continue;
        }
        if repo.name_of(g)?.is_none() {
            let name = crate::workflow::next_name(repo, "trajectory")?;
            ops.push(RefOp::set(&format!("{REF_TRAJECTORIES}/{name}"), *g));
        }
    }
    if ops.is_empty() {
        return Ok(());
    }
    repo.apply_refs_unlocked(&RefTransaction { ops })
}

/// A structured report for `gemel exchange status`.
#[derive(Debug, Clone)]
pub struct ExchangeStatus {
    pub detected: bool,
    pub native_store: bool,
    pub frontiers: Vec<FrontierSummary>,
    pub current_source_state: Option<Gid>,
    pub active: Option<String>,
    pub pending_export: bool,
}

/// One frontier summary.
#[derive(Debug, Clone)]
pub struct FrontierSummary {
    pub id: String,
    pub source_state: Gid,
    pub head_change: Gid,
    pub profile: String,
    pub imported: bool,
    pub binding: SourceBinding,
}

/// Reports the exchange state without mutating anything.
pub fn status(repo: Option<&Repo>, root: &Path) -> Result<ExchangeStatus, Error> {
    let meta = root.join(crate::store::META_DIR);
    let frontiers = discover_frontiers(&meta)?;
    let native = repo.is_some();
    let current = if let Some(repo) = repo {
        working_tree_files(repo)
            .and_then(|files| content_state_identity(repo, &files))
            .ok()
    } else {
        None
    };
    let active = super::export::read_active_frontier(&meta)?;
    let mut summaries = Vec::new();
    for (f, id, _) in &frontiers {
        let binding = match current {
            Some(c) if c == f.source_state => SourceBinding::Matched,
            _ => SourceBinding::Diverged,
        };
        summaries.push(FrontierSummary {
            id: crate::hex::encode(id),
            source_state: f.source_state,
            head_change: f.head_change,
            profile: f.profile.clone(),
            imported: is_imported(&meta, id),
            binding,
        });
    }
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    // Exchange no longer describes the current source when no present
    // frontier's source_state equals the current content state.
    let pending_export = native
        && current.is_some()
        && !frontiers
            .iter()
            .any(|(f, _, _)| Some(f.source_state) == current);
    Ok(ExchangeStatus {
        detected: !summaries.is_empty(),
        native_store: native,
        frontiers: summaries,
        current_source_state: current,
        active: active.map(|a| crate::hex::encode(&a)),
        pending_export,
    })
}

/// Safe bootstrap of a fresh native store over an existing exchange tree
/// (EXCHANGE.md §17, §34): preserves tracked exchange files, creates native
/// metadata without clobbering, installs the local .gitignore, ingests,
/// builds the disposable index, establishes imported refs.
pub fn bootstrap(root: &Path) -> Result<IngestOutcome, Error> {
    let repo = Repo::init(root, &InitOptions::default())?;
    super::export::install_local_gitignore(repo.meta_dir())?;
    ingest(&repo)
}

/// Source-state verification mode (EXCHANGE.md §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    WorkingTree,
    GitIndex,
}

/// Verifies exchange artifacts without activating them (EXCHANGE.md §17,
/// §23, §33): every frontier validates, every referenced pack verifies, and
/// the source binding is reported for the chosen source view.
pub fn verify(root: &Path, mode: VerifyMode) -> Result<VerifyOutcome, Error> {
    let meta = root.join(crate::store::META_DIR);
    let limits = ExchangeLimits::default();
    let frontiers = discover_frontiers(&meta)?;
    if frontiers.is_empty() {
        return Err(Error::Invalid("no exchange frontiers present".into()));
    }
    // Validate every pack of every frontier (transitively), no promotion.
    let mut packs_seen: HashMap<String, bool> = HashMap::new();
    for (frontier, id, _) in &frontiers {
        require_supported_schemas(frontier)?;
        validate_frontier_packs(&meta, frontier, &limits, 0, &mut packs_seen)
            .map_err(|e| Error::Invalid(format!("frontier {}: {e}", crate::hex::encode(id))))?;
    }
    // Source binding: the working tree / git index may be read without a
    // native store (fresh checkout, CI); the pure path hashes in memory.
    let limits_default = crate::limits::Limits::default();
    let ignore = crate::ignore::Ignore::from_root(root);
    let (source_state, staged) = match mode {
        VerifyMode::WorkingTree => {
            let state = match Repo::open(root) {
                Ok(repo) => {
                    let files = working_tree_files(&repo)?;
                    content_state_identity(&repo, &files)?
                }
                Err(crate::store::Error::NotARepository(_)) => {
                    let files =
                        crate::content::pure_working_tree_files(root, &ignore, &limits_default)?;
                    super::export::content_state_identity_with_limits(&files, &limits_default)?
                }
                Err(e) => return Err(e),
            };
            (state, false)
        }
        VerifyMode::GitIndex => {
            let files = crate::git_adapter::staged_files(root)?;
            let map = files
                .iter()
                // The staged tree may carry tracked exchange material
                // (`.gemel/…`) and the root `.gitignore`; those are metadata,
                // never source content, and must not participate in the
                // source-state binding (EXCHANGE.md §10, §23).
                .filter(|(p, _, _)| !p.starts_with(".gemel/") && p.as_str() != ".gitignore")
                .map(|(p, mode, content)| {
                    (p.clone(), (*mode, crate::content::blob_identity(content)))
                })
                .collect::<std::collections::BTreeMap<String, (u64, Gid)>>();
            let state = match Repo::open(root) {
                Ok(repo) => content_state_identity(&repo, &map)?,
                Err(crate::store::Error::NotARepository(_)) => {
                    super::export::content_state_identity_with_limits(&map, &limits_default)?
                }
                Err(e) => return Err(e),
            };
            (state, true)
        }
    };
    let mut matched = Vec::new();
    let mut diverged = Vec::new();
    for (f, id, _) in &frontiers {
        if f.source_state == source_state {
            matched.push(crate::hex::encode(id));
        } else {
            diverged.push(crate::hex::encode(id));
        }
    }
    Ok(VerifyOutcome {
        frontiers_validated: frontiers.len(),
        packs_validated: packs_seen.len(),
        source_state,
        staged,
        matched,
        diverged,
    })
}

fn validate_frontier_packs(
    meta: &Path,
    root_frontier: &Frontier,
    limits: &ExchangeLimits,
    _depth: usize,
    seen: &mut HashMap<String, bool>,
) -> Result<(), Error> {
    // Iterative DFS over the parent chain (bounded depth; no recursion).
    struct Frame {
        frontier: Box<Frontier>,
        depth: usize,
    }
    let mut stack = vec![Frame {
        frontier: Box::new(root_frontier.clone()),
        depth: 0,
    }];
    while let Some(frame) = stack.pop() {
        if frame.depth > limits.max_reference_depth {
            return Err(Error::Limit {
                kind: "exchange parent frontier depth",
                limit: limits.max_reference_depth as u64,
                found: frame.depth as u64,
            });
        }
        require_supported_schemas(&frame.frontier)?;
        for parent_hex in &frame.frontier.parent_frontiers {
            let parent_id = super::hex_to_digest(parent_hex).ok_or_else(|| {
                Error::Invalid(format!("malformed parent frontier id {parent_hex:?}"))
            })?;
            let parent_path = super::frontier_path(meta, &parent_id);
            let parent_bytes = super::read_frontier_file(&parent_path)
                .map_err(|_| Error::Invalid(format!("missing parent frontier {parent_hex}")))?;
            if super::frontier_id(&parent_bytes) != parent_id {
                return Err(Error::Invalid(format!(
                    "parent frontier {parent_hex} identity mismatch"
                )));
            }
            let parent = super::parse_frontier(&parent_bytes, limits)?;
            stack.push(Frame {
                frontier: Box::new(parent),
                depth: frame.depth + 1,
            });
        }
        for pack_hex in &frame.frontier.packs {
            if seen.contains_key(pack_hex) {
                continue;
            }
            let pack_id = super::hex_to_digest(pack_hex)
                .ok_or_else(|| Error::Invalid(format!("malformed pack id {pack_hex:?}")))?;
            let path = super::pack_path(meta, &pack_id);
            let bytes = super::read_pack_file(&path)
                .map_err(|_| Error::Invalid(format!("referenced pack {pack_hex} is missing")))?;
            if super::pack_id(&bytes) != pack_id {
                return Err(Error::Invalid(format!("pack {pack_hex} identity mismatch")));
            }
            decode_pack(&bytes, limits)?;
            seen.insert(pack_hex.clone(), true);
        }
    }
    Ok(())
}

/// The outcome of `gemel exchange verify`.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub frontiers_validated: usize,
    pub packs_validated: usize,
    pub source_state: Gid,
    pub staged: bool,
    pub matched: Vec<String>,
    pub diverged: Vec<String>,
}
