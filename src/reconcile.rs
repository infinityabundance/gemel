//! Reconciliation (SPECIFICATION.md Phase 3, OBJECT_MODEL.md §6.17,
//! AGENT_PROTOCOL.md §5.10).
//!
//! Reconciliation replaces merge as the higher-level concept (brief §12):
//! input trajectories are never erased; a reconciliation chooses a resulting
//! engineering direction while recording that alternatives existed. Phase 3
//! detects TEXTUAL interactions (same path touched by different trajectories)
//! with `certainty: observed`, conservative CLAIM interactions with
//! `certainty: possible`, and exposes uncertainty rather than inventing
//! certainty (brief §13).
//!
//! Adoption policy (Phase 3, documented in every rationale): per-path
//! first-input-trajectory-wins. Paths are owned by the first input trajectory
//! that touches them; a change whose touched paths are all owned by its own
//! trajectory is adopted, otherwise rejected. The merged state applies the
//! adopted changes' deltas onto the common base state, in input order.

use crate::content;
use crate::family::Family;
use crate::gid::Gid;
use crate::query::{self, ClaimStatus};
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::{now_ms, Error, Repo, REF_HEAD, REF_NAMES, REF_RECONCILIATIONS, REF_STATE_HEAD};
use crate::value::{Field, Object, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

fn f(tag: u8, value: Value) -> Field {
    Field::new(tag, value)
}
fn arr(vals: Vec<Value>) -> Value {
    Value::Array(vals)
}
fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

/// One input trajectory.
#[derive(Debug, Clone)]
pub struct ReconcileInput {
    pub name: String,
    pub gid: Gid,
}

/// A textual conflict: one path touched by changes from different inputs.
#[derive(Debug, Clone)]
pub struct TextualConflict {
    pub path: String,
    pub changes: Vec<Gid>,
}

/// A semantic/claim interaction considered by the reconciliation.
#[derive(Debug, Clone)]
pub struct Interaction {
    pub kind: String,
    pub certainty: String,
    pub subjects: Vec<Gid>,
    pub severity: Option<String>,
    pub detail: String,
}

/// The full reconciliation analysis (deterministic, and — for `plan` — with
/// the resulting state's identity computed in memory without publishing).
#[derive(Debug, Clone)]
pub struct ReconcilePlan {
    pub inputs: Vec<ReconcileInput>,
    pub base_state: Option<Gid>,
    pub intent: Option<Gid>,
    pub adopted: Vec<Gid>,
    pub rejected: Vec<Gid>,
    pub textual_conflicts: Vec<TextualConflict>,
    pub interactions: Vec<Interaction>,
    pub claims_retained: Vec<Gid>,
    pub claims_invalidated: Vec<Gid>,
    pub evidence_retained: Vec<Gid>,
    pub unresolved_residuals: Vec<Gid>,
    pub resolved_residuals: Vec<Gid>,
    pub verification_required: Vec<Gid>,
    pub merged_files: BTreeMap<String, (u64, Gid)>,
    pub resulting_state: Gid,
    pub rationale: String,
}

/// The outcome of a full reconcile (objects published).
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    pub reconciliation: Gid,
    pub reconciliation_name: String,
    pub change: Gid,
    pub change_name: String,
    pub state: Gid,
    pub state_name: String,
    pub plan: ReconcilePlan,
}

/// Options for `gemel reconcile`.
#[derive(Debug, Clone, Default)]
pub struct ReconcileOptions {
    /// Advance `refs/head`, `refs/state/head`, and the workspace to the
    /// reconciled result.
    pub apply: bool,
    /// Producer identity (default: repository default producer).
    pub producer: Option<Gid>,
}

/// The paths a change touches: operation subject paths, plus rename from/to.
fn change_paths(repo: &Repo, change: &Gid) -> Result<Vec<String>, Error> {
    let obj = repo.load(change)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for op in query::gid_list(fields, 0x04) {
        let op_obj = match repo.load(&op) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let ofs = op_obj.field_sequence().unwrap_or(&[]);
        if let Some(p) = query::str_field(ofs, 0x02) {
            if seen.insert(p.to_string()) {
                paths.push(p.to_string());
            }
        }
        // rename_path: the operation touches both `from` and `to`.
        let rf = query::record_field(ofs, 0x06);
        let _ = rf;
        if let Some(from) = query::str_field(ofs, 0x16) {
            if seen.insert(from.to_string()) {
                paths.push(from.to_string());
            }
        }
        if let Some(to) = query::str_field(ofs, 0x17) {
            if seen.insert(to.to_string()) {
                paths.push(to.to_string());
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Every change of a trajectory (earliest → latest).
fn trajectory_changes(repo: &Repo, latest: &Gid) -> Result<Vec<Gid>, Error> {
    let versions = query::trajectory_versions(repo, latest)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, obj) in versions.iter().rev() {
        let fs = obj.field_sequence().unwrap_or(&[]);
        for change in query::gid_list(fs, 0x06) {
            if seen.insert(change) {
                out.push(change);
            }
        }
    }
    Ok(out)
}

/// Runs the reconciliation analysis. Pure: no objects are published; the
/// resulting state identity is computed in memory.
pub fn analyze(repo: &Repo, inputs: &[ReconcileInput]) -> Result<ReconcilePlan, Error> {
    if inputs.len() < 2 {
        return Err(Error::Invalid(
            "reconcile requires at least two trajectories".into(),
        ));
    }
    // Load every trajectory's change sequence and its base state.
    let mut sequences: Vec<Vec<Gid>> = Vec::new();
    let mut base_state: Option<Gid> = None;
    let mut intent: Option<Gid> = None;
    for input in inputs {
        let detail = query::trajectory_detail(repo, &input.gid.to_string())?;
        match base_state {
            None => base_state = detail.base_state,
            Some(b) if Some(b) != detail.base_state => {
                return Err(Error::Invalid(format!(
                    "trajectories do not share a base state ({} has {:?}, expected {b})",
                    input.name, detail.base_state
                )))
            }
            _ => {}
        }
        if intent.is_none() {
            intent = detail.intent;
        }
        sequences.push(trajectory_changes(repo, &input.gid)?);
    }

    // Path ownership: the first input trajectory (and change) that touches a
    // path.
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut owner_change: HashMap<String, Gid> = HashMap::new();
    let mut change_paths_map: Vec<(Gid, usize, Vec<String>)> = Vec::new();
    for (ti, changes) in sequences.iter().enumerate() {
        for change in changes {
            let paths = change_paths(repo, change)?;
            for p in &paths {
                owner.entry(p.clone()).or_insert(ti);
                owner_change.entry(p.clone()).or_insert(*change);
            }
            change_paths_map.push((*change, ti, paths));
        }
    }

    // Adoption: per-path first-trajectory-wins.
    let mut adopted = Vec::new();
    let mut rejected = Vec::new();
    let mut conflicts: HashMap<String, Vec<Gid>> = HashMap::new();
    let mut interactions: Vec<Interaction> = Vec::new();
    for (change, ti, paths) in &change_paths_map {
        let disputed: Vec<&String> = paths
            .iter()
            .filter(|p| owner.get(*p).copied().unwrap_or(*ti) != *ti)
            .collect();
        if disputed.is_empty() {
            adopted.push(*change);
        } else {
            rejected.push(*change);
            for p in disputed {
                let entry = conflicts.entry(p.clone()).or_default();
                // The conflict lists both the owning (adopted) change and
                // this rejected change.
                if let Some(owner) = owner_change.get(p) {
                    if !entry.contains(owner) {
                        entry.push(*owner);
                    }
                }
                if !entry.contains(change) {
                    entry.push(*change);
                }
                interactions.push(Interaction {
                    kind: "textual".into(),
                    certainty: "observed".into(),
                    subjects: vec![*change],
                    severity: Some("medium".into()),
                    detail: format!("{p} touched by an earlier-adopted trajectory"),
                });
            }
        }
    }
    // Deterministic order: adopted in input order; conflicts sorted by path.
    let mut conflicts_vec: Vec<TextualConflict> = conflicts
        .into_iter()
        .map(|(path, mut changes)| {
            changes.sort_by_key(|a| a.to_string());
            TextualConflict { path, changes }
        })
        .collect();
    conflicts_vec.sort_by(|a, b| a.path.cmp(&b.path));

    // Claims and evidence.
    let mut claims_retained = Vec::new();
    let mut claims_invalidated = Vec::new();
    let mut evidence_retained = Vec::new();
    let mut adopted_subjects: HashSet<String> = HashSet::new();
    for change in &adopted {
        let obj = repo.load(change)?;
        let fields = obj.field_sequence().unwrap_or(&[]);
        for claim in query::gid_list(fields, 0x0C) {
            claims_retained.push(claim);
            if let Ok(cobj) = repo.load(&claim) {
                if let Some(subj) = query::str_field(cobj.field_sequence().unwrap_or(&[]), 0x01) {
                    adopted_subjects.insert(subj.to_string());
                }
            }
        }
        evidence_retained.extend(query::gid_list(fields, 0x0D));
    }
    for change in &rejected {
        let obj = repo.load(change)?;
        let fields = obj.field_sequence().unwrap_or(&[]);
        for claim in query::gid_list(fields, 0x0C) {
            let mut touches_adopted = false;
            if let Ok(cobj) = repo.load(&claim) {
                let subj = query::str_field(cobj.field_sequence().unwrap_or(&[]), 0x01);
                if let Some(subj) = subj {
                    touches_adopted = adopted_subjects.contains(subj);
                }
            }
            if touches_adopted {
                claims_invalidated.push(claim);
            }
        }
    }
    claims_retained.sort_by_key(|a| a.to_string());
    claims_retained.dedup();
    claims_invalidated.sort_by_key(|a| a.to_string());
    claims_invalidated.dedup();
    evidence_retained.sort_by_key(|a| a.to_string());
    evidence_retained.dedup();
    // Claim interactions (certainty: possible).
    for claim in &claims_retained {
        let subj = match repo.load(claim) {
            Ok(o) => {
                query::str_field(o.field_sequence().unwrap_or(&[]), 0x01).map(|x| x.to_string())
            }
            Err(_) => None,
        };
        let Some(subj) = subj else { continue };
        let conflicting: Vec<Gid> = claims_invalidated
            .iter()
            .copied()
            .filter(|c| {
                repo.load(c)
                    .map(|o| {
                        query::str_field(o.field_sequence().unwrap_or(&[]), 0x01)
                            == Some(subj.as_str())
                    })
                    .unwrap_or(false)
            })
            .collect();
        if !conflicting.is_empty() {
            let mut subjects = vec![*claim];
            subjects.extend(conflicting);
            subjects.sort_by_key(|a| a.to_string());
            interactions.push(Interaction {
                kind: "claim".into(),
                certainty: "possible".into(),
                subjects,
                severity: Some("medium".into()),
                detail: format!("claim about {subj} from a rejected change may disagree"),
            });
        }
    }

    // Residuals: latest chain version, disposition split.
    let mut unresolved = Vec::new();
    let mut resolved = Vec::new();
    let mut seen_res = HashSet::new();
    for changes in &sequences {
        for change in changes {
            let obj = match repo.load(change) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let fields = obj.field_sequence().unwrap_or(&[]);
            for res in query::gid_list(fields, 0x0E) {
                let latest = query::chain_latest(repo, &res)?;
                if !seen_res.insert(latest) {
                    continue;
                }
                match query::residual_disposition(repo, &latest)?.as_str() {
                    "open" => unresolved.push(latest),
                    "resolved" => resolved.push(latest),
                    _ => {}
                }
            }
        }
    }
    unresolved.sort_by_key(|a| a.to_string());
    resolved.sort_by_key(|a| a.to_string());

    // Verification required: retained claims not fully supported.
    let mut verification_required = Vec::new();
    for claim in &claims_retained {
        if let Ok((status, _, _)) = query::claim_status(repo, claim) {
            if !matches!(status, ClaimStatus::Supported) {
                verification_required.push(*claim);
            }
        }
    }
    verification_required.sort_by_key(|a| a.to_string());

    // The merged file map: base + adopted deltas in application order.
    let base_files: BTreeMap<String, (u64, Gid)> = match base_state {
        Some(base) => content::state_files(repo, &base)?,
        None => BTreeMap::new(),
    };
    let mut merged = base_files;
    for change in &adopted {
        let obj = repo.load(change)?;
        let fields = obj.field_sequence().unwrap_or(&[]);
        let (input, output) = match (
            query::gid_field(fields, 0x03),
            query::gid_field(fields, 0x05),
        ) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let out_files = content::state_files(repo, &output)?;
        for delta in content::diff_states(repo, &input, &output)? {
            let mode = out_files
                .get(&delta.path)
                .map(|(m, _)| *m)
                .unwrap_or(0o100644);
            match delta.kind {
                content::DeltaKind::Created | content::DeltaKind::Modified => {
                    if let Some(new) = delta.new_blob {
                        merged.insert(delta.path, (mode, new));
                    }
                }
                content::DeltaKind::Deleted => {
                    merged.remove(&delta.path);
                }
                content::DeltaKind::Renamed { from } => {
                    if let (Some(_old), Some(new)) = (delta.old_blob, delta.new_blob) {
                        merged.remove(&from);
                        merged.insert(delta.path, (mode, new));
                    }
                }
            }
        }
    }

    // Resulting state identity (in memory; nothing published).
    let (_, resulting_state) = content::state_identity_from_files(repo, &merged)?;

    // Rationale: deterministic, machine-readable prose of the decision.
    let mut rationale = String::new();
    rationale.push_str(&format!(
        "reconciled {} trajectories from base {}; ",
        inputs.len(),
        base_state
            .map(|g| g.to_string())
            .unwrap_or_else(|| "none".into())
    ));
    rationale.push_str(&format!(
        "adopted {} change(s), rejected {} change(s); ",
        adopted.len(),
        rejected.len()
    ));
    if conflicts_vec.is_empty() {
        rationale.push_str("no textual conflicts; ");
    } else {
        rationale.push_str(&format!(
            "{} textual conflict(s) on {}; ",
            conflicts_vec.len(),
            conflicts_vec
                .iter()
                .map(|c| c.path.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    rationale.push_str("policy: per-path first-input-trajectory-wins (Phase 3); alternatives preserved as rejected changes.");

    Ok(ReconcilePlan {
        inputs: inputs.to_vec(),
        base_state,
        intent,
        adopted,
        rejected,
        textual_conflicts: conflicts_vec,
        interactions,
        claims_retained,
        claims_invalidated,
        evidence_retained,
        unresolved_residuals: unresolved,
        resolved_residuals: resolved,
        verification_required,
        merged_files: merged,
        resulting_state,
        rationale,
    })
}

/// The reconciliation execution: publishes the merged state, the resulting
/// change, and the reconciliation object; registers names; optionally applies
/// the result to `refs/head` and the workspace.
pub fn reconcile(
    repo: &Repo,
    inputs: &[ReconcileInput],
    opts: &ReconcileOptions,
) -> Result<ReconcileOutcome, Error> {
    let plan = analyze(repo, inputs)?;
    repo.with_write_lock(|| {
        let producer = match opts.producer {
            Some(g) => g,
            None => repo.read_meta()?["default_producer"]
                .as_str()
                .ok_or_else(|| Error::Invalid("meta.json has no default_producer".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        };
        // Publish the merged state (blobs exist; trees/state inserted).
        let state = content::build_state_from_files(repo, &plan.merged_files)?;

        // The resulting change embodying the direction.
        let deltas = match plan.base_state {
            Some(base) => content::diff_states(repo, &base, &state)?,
            None => Vec::new(),
        };
        let operations = content::synthesize_operations(repo, &deltas, &producer)?;
        let mut cf = vec![f(
            0x01,
            s(&format!("reconcile: {}", summarize_inputs(&plan.inputs))),
        )];
        if let Some(intent) = plan.intent {
            cf.push(f(0x02, Value::Gid(intent)));
        }
        if let Some(base) = plan.base_state {
            cf.push(f(0x03, Value::Gid(base)));
        }
        if !operations.is_empty() {
            cf.push(f(
                0x04,
                arr(operations.iter().copied().map(Value::Gid).collect()),
            ));
        }
        cf.push(f(0x05, Value::Gid(state)));
        cf.push(f(0x06, Value::Gid(producer)));
        if !plan.claims_retained.is_empty() {
            cf.push(f(
                0x0C,
                arr(plan
                    .claims_retained
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.evidence_retained.is_empty() {
            cf.push(f(
                0x0D,
                arr(plan
                    .evidence_retained
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.unresolved_residuals.is_empty() {
            cf.push(f(
                0x0E,
                arr(plan
                    .unresolved_residuals
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.adopted.is_empty() {
            cf.push(f(
                0x11,
                arr(plan.adopted.iter().copied().map(Value::Gid).collect()),
            ));
        }
        cf.push(f(0x15, Value::I(now_ms())));
        let change = repo.insert_object(&Object::fields(Family::Change, cf))?;

        // The reconciliation object.
        let mut rf = vec![f(
            0x01,
            s(&format!(
                "reconcile {} trajectories: {} adopted, {} rejected",
                plan.inputs.len(),
                plan.adopted.len(),
                plan.rejected.len()
            )),
        )];
        if let Some(intent) = plan.intent {
            rf.push(f(0x02, Value::Gid(intent)));
        }
        rf.push(f(
            0x03,
            arr(plan.inputs.iter().map(|i| Value::Gid(i.gid)).collect()),
        ));
        if let Some(base) = plan.base_state {
            rf.push(f(0x04, arr(vec![Value::Gid(base)])));
        }
        if !plan.adopted.is_empty() {
            rf.push(f(
                0x05,
                arr(plan.adopted.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !plan.rejected.is_empty() {
            rf.push(f(
                0x06,
                arr(plan.rejected.iter().copied().map(Value::Gid).collect()),
            ));
        }
        if !plan.unresolved_residuals.is_empty() {
            rf.push(f(
                0x07,
                arr(plan
                    .unresolved_residuals
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.resolved_residuals.is_empty() {
            rf.push(f(
                0x08,
                arr(plan
                    .resolved_residuals
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.interactions.is_empty() {
            rf.push(f(
                0x09,
                arr(plan.interactions.iter().map(interaction_value).collect()),
            ));
        }
        if !plan.claims_retained.is_empty() {
            rf.push(f(
                0x0A,
                arr(plan
                    .claims_retained
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.claims_invalidated.is_empty() {
            rf.push(f(
                0x0B,
                arr(plan
                    .claims_invalidated
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        if !plan.evidence_retained.is_empty() {
            rf.push(f(
                0x0C,
                arr(plan
                    .evidence_retained
                    .iter()
                    .copied()
                    .map(Value::Gid)
                    .collect()),
            ));
        }
        rf.push(f(0x0E, Value::Gid(state)));
        rf.push(f(0x0F, Value::Gid(change)));
        rf.push(f(0x10, s(&plan.rationale)));
        rf.push(f(0x11, Value::Gid(producer)));
        rf.push(f(0x12, Value::I(now_ms())));
        let reconciliation = repo.insert_object(&Object::fields(Family::Reconciliation, rf))?;

        // Names + (optionally) apply.
        let rec_name = crate::workflow::next_name(repo, "reconciliation")?;
        let state_name = crate::workflow::next_name(repo, "state")?;
        let change_name = crate::workflow::next_name(repo, "change")?;
        let mut ops = vec![
            RefOp::set(&format!("{REF_RECONCILIATIONS}/{rec_name}"), reconciliation),
            RefOp::set(&format!("{REF_NAMES}/{state_name}"), state),
            RefOp::set(&format!("{REF_NAMES}/{change_name}"), change),
        ];
        if opts.apply {
            ops.push(RefOp::set(REF_HEAD, change));
            ops.push(RefOp::set(REF_STATE_HEAD, state));
        }
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        if opts.apply {
            crate::workflow::set_workspace_state(repo, state)?;
        }
        Ok(ReconcileOutcome {
            reconciliation,
            reconciliation_name: rec_name,
            change,
            change_name,
            state,
            state_name,
            plan,
        })
    })
}

fn summarize_inputs(inputs: &[ReconcileInput]) -> String {
    inputs
        .iter()
        .map(|i| i.name.clone())
        .collect::<Vec<_>>()
        .join(" + ")
}

fn interaction_value(i: &Interaction) -> Value {
    let mut fields = vec![f(0x01, s(&i.kind)), f(0x02, s(&i.certainty))];
    if !i.subjects.is_empty() {
        fields.push(f(
            0x03,
            arr(i.subjects.iter().copied().map(Value::Gid).collect()),
        ));
    }
    if let Some(sev) = &i.severity {
        fields.push(f(0x04, s(sev)));
    }
    fields.push(f(0x05, s(&i.detail)));
    Value::Record(fields)
}
