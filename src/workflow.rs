//! The change workflow (SPECIFICATION.md Phase 1; brief §42).
//!
//! `gemel change begin` opens a pending change (workspace metadata); `gemel
//! change finish` computes the resulting state from the working tree,
//! synthesizes operations from the delta, records claims/evidence/residuals,
//! creates the Change, and advances the Trajectory — all under one journaled
//! ref transaction. Human names (`I<n>`, `T<n>`, `C<n>`, `S<n>`) are
//! registered in the ref namespace; identities remain content-addressed.

use crate::content;
use crate::family::Family;
use crate::gid::Gid;
use crate::ignore::Ignore;
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::{
    now_ms, Error, Repo, REF_CHECKPOINTS, REF_HEAD, REF_NAMES, REF_STATE_HEAD, REF_TRAJECTORIES,
};
use crate::value::{Field, Object, Value};
use std::path::PathBuf;

/// The default workspace id (Phase 1 has one workspace per repository).
pub const DEFAULT_WORKSPACE: &str = "default";

/// The pending-change record schema.
pub const PENDING_SCHEMA: &str = "gemel.pending.v1";

fn f(tag: u8, value: Value) -> Field {
    Field::new(tag, value)
}
fn arr(vals: Vec<Value>) -> Value {
    Value::Array(vals)
}
fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

// ---------------------------------------------------------------------------
// Workspace metadata
// ---------------------------------------------------------------------------
//
// Phase 3 adds multiple concurrent workspaces (brief §34): named workspaces
// keep their own pending change and materialized-state records, and agents
// may snapshot from separate working directories (`--worktree`), so they
// never serialize merely to avoid filesystem collisions. The default
// workspace keeps Phase 1/2 behavior.

/// The workspace metadata directory.
pub fn workspace_dir(repo: &Repo) -> PathBuf {
    workspace_named_dir(repo, DEFAULT_WORKSPACE)
}

/// The metadata directory of a named workspace.
pub fn workspace_named_dir(repo: &Repo, workspace: &str) -> PathBuf {
    if workspace.is_empty()
        || workspace == "."
        || workspace == ".."
        || workspace.contains('/')
        || workspace.contains('\\')
        || workspace.contains('\0')
    {
        panic!("invalid workspace name {workspace:?}");
    }
    repo.meta_dir().join("worktrees").join(workspace)
}

/// The state the default workspace is currently materialized from, if any.
pub fn workspace_state(repo: &Repo) -> Result<Option<Gid>, Error> {
    workspace_named_state(repo, DEFAULT_WORKSPACE)
}

/// The state a named workspace is currently materialized from, if any.
pub fn workspace_named_state(repo: &Repo, workspace: &str) -> Result<Option<Gid>, Error> {
    let path = workspace_named_dir(repo, workspace).join("state.ref");
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .trim()
            .parse::<Gid>()
            .map(Some)
            .map_err(|e| Error::RefCorrupt {
                name: format!("worktree {workspace} state.ref"),
                detail: e.to_string(),
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Updates the default workspace's materialized state (caller holds the
/// writer lock).
pub fn set_workspace_state(repo: &Repo, gid: Gid) -> Result<(), Error> {
    set_workspace_named_state(repo, DEFAULT_WORKSPACE, gid)
}

/// Updates a named workspace's materialized state (caller holds the writer
/// lock).
pub fn set_workspace_named_state(repo: &Repo, workspace: &str, gid: Gid) -> Result<(), Error> {
    let dir = workspace_named_dir(repo, workspace);
    std::fs::create_dir_all(&dir)?;
    crate::store::objects::write_atomic(
        &dir.join("state.ref"),
        &format!("{}\n", gid).into_bytes(),
    )?;
    Ok(())
}

/// The pending change record, if any (default workspace).
pub fn read_pending(repo: &Repo) -> Result<Option<serde_json::Value>, Error> {
    read_pending_named(repo, DEFAULT_WORKSPACE)
}

/// The pending change record of a named workspace, if any.
pub fn read_pending_named(
    repo: &Repo,
    workspace: &str,
) -> Result<Option<serde_json::Value>, Error> {
    let path = workspace_named_dir(repo, workspace).join("pending.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Error::Invalid(format!("pending.json: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_pending_named(
    repo: &Repo,
    workspace: &str,
    value: &serde_json::Value,
) -> Result<(), Error> {
    let dir = workspace_named_dir(repo, workspace);
    std::fs::create_dir_all(&dir)?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::Invalid(e.to_string()))?;
    bytes.push(b'\n');
    crate::store::objects::write_atomic(&dir.join("pending.json"), &bytes)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Names and counters
// ---------------------------------------------------------------------------

/// Advances the named counter for `kind` and returns the next human name
/// (`I<n>`/`T<n>`/`C<n>`/`S<n>`/`K<n>`/`Re<n>`). Callers hold the writer lock.
pub fn next_name(repo: &Repo, kind: &str) -> Result<String, Error> {
    let mut meta = repo.read_meta()?;
    let n = meta["counters"][kind].as_u64().unwrap_or(0) + 1;
    meta["counters"][kind] = serde_json::json!(n);
    repo.write_meta(&meta)?;
    let prefix = match kind {
        "intent" => "I",
        "trajectory" => "T",
        "change" => "C",
        "state" => "S",
        "checkpoint" => "K",
        "reconciliation" => "Re",
        _ => return Err(Error::Invalid(format!("unknown counter {kind}"))),
    };
    Ok(format!("{prefix}{n}"))
}

/// Registers a human name under a namespace (caller holds the writer lock).
fn register_name(repo: &Repo, namespace: &str, name: &str, gid: Gid) -> Result<(), Error> {
    let ops = vec![RefOp::set(&format!("{namespace}/{name}"), gid)];
    repo.apply_refs_unlocked(&RefTransaction { ops })
}

/// The name registered for `gid` in a namespace, if any.
pub fn name_in_namespace(repo: &Repo, namespace: &str, gid: &Gid) -> Result<Option<String>, Error> {
    let prefix = format!("{namespace}/");
    for (name, target) in repo.all_refs()? {
        if let Some(short) = name.strip_prefix(&prefix) {
            if &target == gid {
                return Ok(Some(short.to_string()));
            }
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// change begin
// ---------------------------------------------------------------------------

/// Options for `change begin`.
#[derive(Debug, Clone, Default)]
pub struct BeginOptions {
    /// Explicit input state (default: workspace state, then head state).
    pub from_state: Option<Gid>,
    /// Existing intent to pursue.
    pub intent: Option<Gid>,
    /// Creates a new intent with this summary.
    pub intent_summary: Option<String>,
    /// Producer identity (default: repository default producer).
    pub producer: Option<Gid>,
    /// Named workspace (default: `default`).
    pub workspace: Option<String>,
    /// Working directory the change will be finished from (default:
    /// repository root).
    pub worktree: Option<PathBuf>,
}

/// The outcome of `change begin`.
#[derive(Debug, Clone)]
pub struct BeginOutcome {
    pub input_state: Option<Gid>,
    pub intent: Option<Gid>,
    pub intent_name: Option<String>,
    pub producer: Gid,
    pub started_at: i64,
}

/// Opens a pending change in a (named) workspace.
pub fn begin_change(repo: &Repo, opts: &BeginOptions) -> Result<BeginOutcome, Error> {
    let workspace = opts
        .workspace
        .clone()
        .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string());
    repo.with_write_lock(|| {
        if read_pending_named(repo, &workspace)?.is_some() {
            return Err(Error::PendingChangeAlreadyExists);
        }
        let input_state = match opts.from_state {
            Some(g) => Some(g),
            None => match workspace_named_state(repo, &workspace)? {
                Some(g) => Some(g),
                None => repo.read_ref(REF_STATE_HEAD)?,
            },
        };
        let producer = match opts.producer {
            Some(g) => g,
            None => repo.read_meta()?["default_producer"]
                .as_str()
                .ok_or_else(|| Error::Invalid("meta.json has no default_producer".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        };
        // Intent: explicit, or create from the summary.
        let (intent, intent_name) = match (opts.intent, &opts.intent_summary) {
            (Some(g), _) => (Some(g), None),
            (None, Some(summary)) => {
                let obj = Object::fields(
                    Family::Intent,
                    vec![
                        f(0x01, s(summary)),
                        f(0x0B, Value::Gid(producer)),
                        f(0x0C, Value::I(now_ms())),
                    ],
                );
                let gid = repo.insert_object(&obj)?;
                let name = next_name(repo, "intent")?;
                register_name(repo, REF_NAMES, &name, gid)?;
                (Some(gid), Some(name))
            }
            (None, None) => (None, None),
        };
        let pending = serde_json::json!({
            "schema": PENDING_SCHEMA,
            "input_state": input_state.map(|g| g.to_string()),
            "intent": intent.map(|g| g.to_string()),
            "producer": producer.to_string(),
            "workspace": workspace,
            "worktree": opts.worktree.clone().map(|p| p.display().to_string()),
            "started_at": now_ms(),
        });
        write_pending_named(repo, &workspace, &pending)?;
        Ok(BeginOutcome {
            input_state,
            intent,
            intent_name,
            producer,
            started_at: pending["started_at"].as_i64().unwrap_or(0),
        })
    })
}

// ---------------------------------------------------------------------------
// change finish
// ---------------------------------------------------------------------------

/// A basic claim specification for `change finish`.
#[derive(Debug, Clone)]
pub struct ClaimSpec {
    pub subject: Option<String>,
    pub predicate: String,
    pub kind: String,
}

/// A basic evidence specification for `change finish`.
#[derive(Debug, Clone)]
pub struct EvidenceSpec {
    pub subject: Option<String>,
    pub outcome: String,
    pub kind: String,
}

/// A basic residual specification for `change finish`.
#[derive(Debug, Clone)]
pub struct ResidualSpec {
    pub summary: String,
    pub classification: String,
    pub severity: String,
}

/// Options for `change finish`.
#[derive(Debug, Clone, Default)]
pub struct FinishOptions {
    pub summary: String,
    pub claims: Vec<ClaimSpec>,
    pub evidence: Vec<EvidenceSpec>,
    pub residuals: Vec<ResidualSpec>,
    /// Named workspace (default: `default`).
    pub workspace: Option<String>,
    /// Working directory to snapshot (default: repository root).
    pub worktree: Option<PathBuf>,
}

/// The outcome of `change finish`.
#[derive(Debug, Clone)]
pub struct FinishOutcome {
    pub change: Gid,
    pub change_name: String,
    pub trajectory: Gid,
    pub trajectory_name: String,
    pub state: Gid,
    pub state_name: String,
    pub operations: Vec<Gid>,
    pub claims: Vec<Gid>,
    pub evidence: Vec<Gid>,
    pub residuals: Vec<Gid>,
    pub is_new_trajectory: bool,
}

/// Finishes the pending change (SPECIFICATION.md Phase 1 demo:
/// State S0 → Intent I1 → Trajectory T1 → Change C1 → State S1).
pub fn finish_change(repo: &Repo, opts: &FinishOptions) -> Result<FinishOutcome, Error> {
    let workspace = opts
        .workspace
        .clone()
        .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string());
    // Fast-fail when nothing is pending in this workspace.
    if read_pending_named(repo, &workspace)?.is_none() {
        return Err(Error::NoPendingChange);
    }

    // Build the resulting state from the working tree (lock-free inserts).
    // The snapshot root is the change's worktree (separate agent
    // directories are first-class, brief §34).
    let worktree = opts
        .worktree
        .clone()
        .unwrap_or_else(|| repo.root().to_path_buf());
    let ignore = Ignore::from_root(&worktree);
    let snapshot = content::build_state(repo, &worktree, &ignore)?;
    let resulting_state = snapshot.state;

    repo.with_write_lock(|| {
        let pending = read_pending_named(repo, &workspace)?
            .ok_or_else(|| Error::Invalid("pending change disappeared mid-finish".into()))?;
        let input_state: Option<Gid> = pending["input_state"]
            .as_str()
            .map(|s| s.parse::<Gid>())
            .transpose()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let intent: Option<Gid> = pending["intent"]
            .as_str()
            .map(|s| s.parse::<Gid>())
            .transpose()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let producer: Gid = pending["producer"]
            .as_str()
            .ok_or_else(|| Error::Invalid("pending has no producer".into()))?
            .parse::<Gid>()
            .map_err(|e| Error::Invalid(e.to_string()))?;

        // File-level delta and operations.
        let operations = match &input_state {
            Some(base) => {
                let deltas = content::diff_states(repo, base, &resulting_state)?;
                content::synthesize_operations(repo, &deltas, &producer)?
            }
            None => {
                // Initial change from an empty base: synthesize creates for
                // every working-tree file from the snapshot we just built.
                let st = repo.load(&resulting_state)?;
                let tree = st
                    .field_sequence()
                    .and_then(|fs| fs.iter().find(|f| f.tag == 0x01))
                    .and_then(|f| match &f.value {
                        Value::Gid(g) => Some(*g),
                        _ => None,
                    })
                    .ok_or_else(|| Error::Invalid("state has no root_tree".into()))?;
                let files = content::flatten_tree(repo, &tree)?;
                synthesize_creates(repo, files, &producer)?
            }
        };

        // Claims, evidence, residuals (basic support).
        let mut claim_ids = Vec::new();
        let mut evidence_ids = Vec::new();
        for spec in &opts.evidence {
            // Field tags in strict ascending order (0x01, 0x02, [0x03], 0x0D,
            // 0x10).
            let mut fields = vec![f(0x01, Value::Gid(producer)), f(0x02, s(&spec.kind))];
            if let Some(subject) = &spec.subject {
                fields.push(f(0x03, s(subject)));
            }
            fields.push(f(0x0D, Value::Record(vec![f(0x01, s(&spec.outcome))])));
            fields.push(f(0x10, Value::I(now_ms())));
            evidence_ids.push(repo.insert_object(&Object::fields(Family::Evidence, fields))?);
        }
        for spec in &opts.claims {
            // Field tags in strict ascending order ([0x01], 0x03, 0x04, 0x07,
            // [0x08], 0x0E).
            let mut fields = Vec::new();
            if let Some(subject) = &spec.subject {
                fields.push(f(0x01, s(subject)));
            }
            fields.push(f(0x03, s(&spec.predicate)));
            fields.push(f(0x04, s(&spec.kind)));
            fields.push(f(0x07, Value::Gid(producer)));
            // Basic linking: a claim links to evidence with the same
            // subject produced by this change.
            if let Some(subject) = &spec.subject {
                let matched: Vec<Value> = opts
                    .evidence
                    .iter()
                    .zip(evidence_ids.iter())
                    .filter(|(e, _)| e.subject.as_deref() == Some(subject.as_str()))
                    .map(|(_, id)| Value::Gid(*id))
                    .collect();
                if !matched.is_empty() {
                    fields.push(f(0x08, arr(matched)));
                }
            }
            fields.push(f(0x0E, Value::I(now_ms())));
            claim_ids.push(repo.insert_object(&Object::fields(Family::Claim, fields))?);
        }
        let mut residual_ids = Vec::new();
        for spec in &opts.residuals {
            // Field tags in strict ascending order (0x02, 0x03, 0x04, [0x06],
            // [0x08], 0x0C).
            let mut fields = vec![
                f(0x02, s(&spec.summary)),
                f(0x03, s(&spec.classification)),
                f(0x04, s(&spec.severity)),
            ];
            if let (Some(last_evidence), Some(last_claim)) = (evidence_ids.last(), claim_ids.last())
            {
                fields.push(f(0x06, arr(vec![Value::Gid(*last_claim)])));
                fields.push(f(0x08, Value::Gid(*last_evidence)));
            }
            fields.push(f(0x0C, Value::I(now_ms())));
            residual_ids.push(repo.insert_object(&Object::fields(Family::Residual, fields))?);
        }

        // Causal parent: chain off head when the change's input equals the
        // head's resulting state.
        let mut causal_parents = Vec::new();
        if let Some(head) = repo.read_ref(REF_HEAD)? {
            if let Ok(head_obj) = repo.load(&head) {
                if let Some(Value::Gid(hrs)) = head_obj
                    .field_sequence()
                    .and_then(|fs| fs.iter().find(|f| f.tag == 0x05).map(|f| &f.value))
                {
                    if Some(*hrs) == input_state {
                        causal_parents.push(Value::Gid(head));
                    }
                }
            }
        }

        // Trajectory: continue the most recent trajectory with the same
        // intent when it is still open (no outcome); a closed trajectory is
        // terminal — new work on the same intent starts a fresh attempt
        // (brief §7: rejected attempts are preserved, not extended).
        let meta = repo.read_meta()?;
        let last_t: u64 = meta["counters"]["trajectory"].as_u64().unwrap_or(0);
        let (trajectory_previous, base_state, is_new) = if last_t > 0 {
            let latest = repo.read_ref(&format!("{REF_TRAJECTORIES}/T{last_t}"))?;
            match latest {
                Some(gid) => {
                    let obj = repo.load(&gid)?;
                    let traj_intent = obj
                        .field_sequence()
                        .and_then(|fs| fs.iter().find(|f| f.tag == 0x02))
                        .and_then(|f| match &f.value {
                            Value::Gid(g) => Some(*g),
                            _ => None,
                        });
                    let closed = obj
                        .field_sequence()
                        .and_then(|fs| fs.iter().find(|f| f.tag == 0x0A))
                        .is_some();
                    let same_intent = traj_intent == intent;
                    if same_intent && !closed {
                        let base = obj
                            .field_sequence()
                            .and_then(|fs| fs.iter().find(|f| f.tag == 0x03))
                            .and_then(|f| match &f.value {
                                Value::Gid(g) => Some(*g),
                                _ => None,
                            });
                        (Some(gid), base, false)
                    } else {
                        (None, input_state, true)
                    }
                }
                None => (None, input_state, true),
            }
        } else {
            (None, input_state, true)
        };

        // The Change object. Field tags in strict ascending order:
        // 0x01, [0x02], [0x03], [0x04], 0x05, 0x06, [0x0C], [0x0D], [0x0E],
        // [0x11], 0x15.
        let mut change_fields = vec![f(
            0x01,
            s(if opts.summary.is_empty() {
                "change"
            } else {
                opts.summary.as_str()
            }),
        )];
        if let Some(intent) = intent {
            change_fields.push(f(0x02, Value::Gid(intent)));
        }
        if let Some(input) = input_state {
            change_fields.push(f(0x03, Value::Gid(input)));
        }
        if !operations.is_empty() {
            change_fields.push(f(
                0x04,
                arr(operations.iter().copied().map(Value::Gid).collect()),
            ));
        }
        change_fields.push(f(0x05, Value::Gid(resulting_state)));
        change_fields.push(f(0x06, Value::Gid(producer)));
        if !claim_ids.is_empty() {
            change_fields.push(f(
                0x0C,
                arr(claim_ids.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !evidence_ids.is_empty() {
            change_fields.push(f(
                0x0D,
                arr(evidence_ids.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !residual_ids.is_empty() {
            change_fields.push(f(
                0x0E,
                arr(residual_ids.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !causal_parents.is_empty() {
            change_fields.push(f(0x11, arr(causal_parents)));
        }
        change_fields.push(f(0x15, Value::I(now_ms())));
        let change = repo.insert_object(&Object::fields(Family::Change, change_fields))?;

        // The Trajectory object. Field tags in strict ascending order:
        // [0x01], [0x02], [0x03], 0x04, 0x06, [0x08], [0x09], 0x0D, 0x0E.
        let mut traj_fields = Vec::new();
        if let Some(prev) = trajectory_previous {
            traj_fields.push(f(0x01, Value::Gid(prev)));
        }
        if let Some(intent) = intent {
            traj_fields.push(f(0x02, Value::Gid(intent)));
        }
        if let Some(base) = base_state {
            traj_fields.push(f(0x03, Value::Gid(base)));
        }
        traj_fields.push(f(0x04, Value::Gid(producer)));
        traj_fields.push(f(0x06, arr(vec![Value::Gid(change)])));
        if !evidence_ids.is_empty() {
            traj_fields.push(f(
                0x08,
                arr(evidence_ids.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !residual_ids.is_empty() {
            traj_fields.push(f(
                0x09,
                arr(residual_ids.iter().copied().map(Value::Gid).collect()),
            ));
        }
        traj_fields.push(f(0x0D, Value::I(now_ms())));
        traj_fields.push(f(0x0E, Value::I(now_ms())));
        let trajectory = repo.insert_object(&Object::fields(Family::Trajectory, traj_fields))?;

        // Names + counters + refs, one journaled transaction.
        let change_name = next_name(repo, "change")?;
        let state_name = next_name(repo, "state")?;
        let trajectory_name = if is_new {
            next_name(repo, "trajectory")?
        } else {
            format!("T{last_t}")
        };
        let ops = vec![
            RefOp::set(REF_HEAD, change),
            RefOp::set(REF_STATE_HEAD, resulting_state),
            RefOp::set(&format!("{REF_NAMES}/{change_name}"), change),
            RefOp::set(&format!("{REF_NAMES}/{state_name}"), resulting_state),
            RefOp::set(&format!("{REF_TRAJECTORIES}/{trajectory_name}"), trajectory),
            RefOp::set(&format!("{REF_TRAJECTORIES}/current"), trajectory),
        ];
        repo.apply_refs_unlocked(&RefTransaction { ops })?;

        // Workspace now materializes the resulting state.
        set_workspace_named_state(repo, &workspace, resulting_state)?;

        // Clear the pending change.
        let pending_path = workspace_named_dir(repo, &workspace).join("pending.json");
        match std::fs::remove_file(&pending_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        Ok(FinishOutcome {
            change,
            change_name,
            trajectory,
            trajectory_name,
            state: resulting_state,
            state_name,
            operations,
            claims: claim_ids,
            evidence: evidence_ids,
            residuals: residual_ids,
            is_new_trajectory: is_new,
        })
    })
}

/// Synthesizes create operations for a set of (path, (mode, blob)) files
/// (used for the initial change from an empty base).
fn synthesize_creates(
    repo: &Repo,
    files: std::collections::HashMap<String, (u64, Gid)>,
    producer: &Gid,
) -> Result<Vec<Gid>, Error> {
    let ts = Value::I(now_ms());
    let mut files: Vec<(String, (u64, Gid))> = files.into_iter().collect();
    files.sort();
    let mut out = Vec::new();
    for (path, (_mode, blob)) in files {
        let fields = vec![
            f(0x01, s("create_file")),
            f(0x02, s(&path)),
            f(0x05, arr(vec![Value::Gid(blob)])),
            f(0x06, Value::Record(vec![f(0x01, s("ok"))])),
            f(0x07, Value::Gid(*producer)),
            f(0x09, ts.clone()),
            f(0x0A, ts.clone()),
            f(0x11, Value::Gid(blob)),
        ];
        out.push(repo.insert_object(&Object::fields(Family::Operation, fields))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Phase 2 — checkpoint, trajectory close, residual resolve
// ---------------------------------------------------------------------------

/// Options for `gemel checkpoint`.
#[derive(Debug, Clone, Default)]
pub struct CheckpointOptions {
    /// Human summary (default: machine-generated from the intent).
    pub summary: Option<String>,
    /// Producer identity (default: repository default producer).
    pub producer: Option<Gid>,
}

/// The outcome of `gemel checkpoint`.
#[derive(Debug, Clone)]
pub struct CheckpointOutcome {
    pub checkpoint: Gid,
    pub name: String,
    pub plan: crate::query::CheckpointPlan,
}

/// Creates a checkpoint: a continuation boundary assembled from structured
/// repository state (AGENT_PROTOCOL.md §9.2). Object family `checkpoint`
/// (0x14); fields in strict ascending tag order.
pub fn create_checkpoint(
    repo: &Repo,
    opts: &CheckpointOptions,
) -> Result<CheckpointOutcome, Error> {
    repo.with_write_lock(|| {
        let plan = crate::query::checkpoint_plan(repo)?;
        let producer = match opts.producer {
            Some(g) => g,
            None => repo.read_meta()?["default_producer"]
                .as_str()
                .ok_or_else(|| Error::Invalid("meta.json has no default_producer".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        };
        let summary = opts.summary.clone().unwrap_or(plan.summary.clone());
        let previous = repo.read_ref(&format!("{REF_CHECKPOINTS}/current"))?;
        let mut fields = Vec::new();
        if let Some(prev) = previous {
            fields.push(f(0x01, Value::Gid(prev)));
        }
        fields.push(f(0x02, s(&summary)));
        if let Some(intent) = plan.intent {
            fields.push(f(0x03, Value::Gid(intent)));
        }
        if let Some((_, traj)) = &plan.trajectory {
            fields.push(f(0x04, Value::Gid(*traj)));
        }
        if let Some(state) = plan.state {
            fields.push(f(0x05, Value::Gid(state)));
        }
        if !plan.open_claims.is_empty() {
            fields.push(f(
                0x06,
                arr(plan.open_claims.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !plan.unresolved_residuals.is_empty() {
            fields.push(f(
                0x07,
                arr(plan
                    .unresolved_residuals
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.important_evidence.is_empty() {
            fields.push(f(
                0x08,
                arr(plan
                    .important_evidence
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.recent_decisions.is_empty() {
            fields.push(f(
                0x09,
                arr(plan
                    .recent_decisions
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.relevant_attempts.is_empty() {
            fields.push(f(
                0x0A,
                arr(plan
                    .relevant_attempts
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.continuation_scope.is_empty() {
            fields.push(f(
                0x0B,
                arr(plan
                    .continuation_scope
                    .iter()
                    .map(|s| Value::Str(s.clone()))
                    .collect()),
            ));
        }
        fields.push(f(0x0C, Value::Gid(producer)));
        fields.push(f(0x0D, Value::I(now_ms())));
        let checkpoint = repo.insert_object(&Object::fields(Family::Checkpoint, fields))?;
        let name = next_name(repo, "checkpoint")?;
        let ops = vec![
            RefOp::set(&format!("{REF_CHECKPOINTS}/{name}"), checkpoint),
            RefOp::set(&format!("{REF_CHECKPOINTS}/current"), checkpoint),
        ];
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(CheckpointOutcome {
            checkpoint,
            name,
            plan,
        })
    })
}

/// Options for `gemel trajectory close`.
#[derive(Debug, Clone)]
pub struct CloseTrajectoryOptions {
    /// Trajectory name or identity.
    pub trajectory: String,
    /// Outcome (`completed` `abandoned` `superseded` `rejected`
    /// `inconclusive` `interrupted`).
    pub outcome: String,
    /// Termination reason.
    pub reason: Option<String>,
    /// Producer identity (default: repository default producer).
    pub producer: Option<Gid>,
}

/// The outcome of `gemel trajectory close`.
#[derive(Debug, Clone)]
pub struct CloseTrajectoryOutcome {
    pub previous: Gid,
    pub version: Gid,
    pub name: String,
}

/// Publishes a new trajectory version with an outcome and termination reason
/// (append-chained; the alternative remains canonical). The full change
/// sequence stays the concatenation of `added_changes` across the chain.
pub fn close_trajectory(
    repo: &Repo,
    opts: &CloseTrajectoryOptions,
) -> Result<CloseTrajectoryOutcome, Error> {
    repo.with_write_lock(|| {
        let previous = repo.resolve(&opts.trajectory)?;
        let prev_obj = repo.load(&previous)?;
        if prev_obj.family != Family::Trajectory {
            return Err(Error::Invalid(format!(
                "{} is not a trajectory",
                opts.trajectory
            )));
        }
        let pfs = prev_obj.field_sequence().unwrap_or(&[]);
        let producer = match opts.producer {
            Some(g) => g,
            None => repo.read_meta()?["default_producer"]
                .as_str()
                .ok_or_else(|| Error::Invalid("meta.json has no default_producer".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        };
        let mut fields = vec![f(0x01, Value::Gid(previous))];
        if let Some(intent) = crate::query::gid_field(pfs, 0x02) {
            fields.push(f(0x02, Value::Gid(intent)));
        }
        if let Some(base) = crate::query::gid_field(pfs, 0x03) {
            fields.push(f(0x03, Value::Gid(base)));
        }
        fields.push(f(0x04, Value::Gid(producer)));
        fields.push(f(0x0A, s(&opts.outcome)));
        if let Some(reason) = &opts.reason {
            fields.push(f(0x0B, s(reason)));
        }
        fields.push(f(0x0D, Value::I(now_ms())));
        fields.push(f(0x0E, Value::I(now_ms())));
        let version = repo.insert_object(&Object::fields(Family::Trajectory, fields))?;
        // Re-point the name at the newest version.
        let name = crate::workflow::name_in_namespace(repo, REF_TRAJECTORIES, &previous)?
            .ok_or_else(|| Error::Invalid("trajectory has no name ref".into()))?;
        let ops = vec![RefOp::set(&format!("{REF_TRAJECTORIES}/{name}"), version)];
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(CloseTrajectoryOutcome {
            previous,
            version,
            name,
        })
    })
}

/// Options for `gemel residual resolve`.
#[derive(Debug, Clone)]
pub struct ResolveResidualOptions {
    /// Residual name or identity.
    pub residual: String,
    /// Disposition: `open` `acknowledged` `resolved` `superseded`
    /// `irrelevant`.
    pub disposition: String,
    /// Rationale.
    pub reason: Option<String>,
    /// Producer identity (default: repository default producer).
    pub producer: Option<Gid>,
}

/// The outcome of `gemel residual resolve`.
#[derive(Debug, Clone)]
pub struct ResolveResidualOutcome {
    pub previous: Gid,
    pub version: Gid,
}

/// Publishes a new residual version with a disposition event. The original
/// version stays referenced by its change; the derived disposition comes from
/// the latest chain version (OBJECT_MODEL.md §6.12, §8.3).
pub fn resolve_residual(
    repo: &Repo,
    opts: &ResolveResidualOptions,
) -> Result<ResolveResidualOutcome, Error> {
    repo.with_write_lock(|| {
        let base = repo.resolve(&opts.residual)?;
        let latest = crate::query::chain_latest(repo, &base)?;
        let obj = repo.load(&latest)?;
        if obj.family != Family::Residual {
            return Err(Error::Invalid(format!(
                "{} is not a residual",
                opts.residual
            )));
        }
        let fields = obj.field_sequence().unwrap_or(&[]);
        let producer = match opts.producer {
            Some(g) => g,
            None => repo.read_meta()?["default_producer"]
                .as_str()
                .ok_or_else(|| Error::Invalid("meta.json has no default_producer".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        };
        // Carry the semantic content forward; append the disposition event.
        let mut out = vec![f(0x01, Value::Gid(latest))];
        if let Some(summary) = crate::query::str_field(fields, 0x02) {
            out.push(f(0x02, s(summary)));
        }
        if let Some(class) = crate::query::str_field(fields, 0x03) {
            out.push(f(0x03, s(class)));
        }
        if let Some(sev) = crate::query::str_field(fields, 0x04) {
            out.push(f(0x04, s(sev)));
        }
        let claims = crate::query::gid_list(fields, 0x06);
        if !claims.is_empty() {
            out.push(f(
                0x06,
                arr(claims.iter().copied().map(Value::Gid).collect()),
            ));
        }
        let changes = crate::query::gid_list(fields, 0x07);
        if !changes.is_empty() {
            out.push(f(
                0x07,
                arr(changes.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if let Some(origin) = crate::query::gid_field(fields, 0x08) {
            out.push(f(0x08, Value::Gid(origin)));
        }
        let mut event = vec![f(0x01, s(&opts.disposition)), f(0x02, Value::Gid(producer))];
        if let Some(reason) = &opts.reason {
            event.push(f(0x05, s(reason)));
        }
        event.push(f(0x06, Value::I(now_ms())));
        out.push(f(0x0A, Value::Record(event)));
        out.push(f(0x0C, Value::I(now_ms())));
        let version = repo.insert_object(&Object::fields(Family::Residual, out))?;
        Ok(ResolveResidualOutcome {
            previous: latest,
            version,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::fresh_repo;

    #[test]
    fn counters_and_names() {
        let (repo, _) = fresh_repo("names2");
        repo.with_write_lock(|| {
            assert_eq!(next_name(&repo, "change").unwrap(), "C1");
            assert_eq!(next_name(&repo, "change").unwrap(), "C2");
            assert_eq!(next_name(&repo, "intent").unwrap(), "I1");
            assert_eq!(next_name(&repo, "trajectory").unwrap(), "T1");
            assert_eq!(next_name(&repo, "state").unwrap(), "S1");
            Ok(())
        })
        .unwrap();
    }
}
