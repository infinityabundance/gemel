//! Query layer: log, show, status, derived statuses, and the Phase 2
//! agent-native surface (why/claims/evidence/residuals/attempts/trajectory/
//! checkpoint/context; AGENT_PROTOCOL.md §5–§6, §9).
//!
//! Derived statuses (claim status, residual disposition, persistence,
//! readiness) are computed from the canonical graph, never stored.

use crate::family::Family;
use crate::gid::Gid;
use crate::store::{Error, Repo, REF_HEAD, REF_STATE_HEAD, REF_TRAJECTORIES};
use crate::value::{Field, Object, Value};
use std::collections::{HashMap, HashSet};

/// One change in `gemel log`.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub change: Gid,
    pub name: Option<String>,
    pub summary: String,
    pub input_state: Option<Gid>,
    pub resulting_state: Option<Gid>,
    pub trajectory: Option<String>,
    pub created_at: Option<i64>,
    pub operations: Vec<Gid>,
    pub claims: Vec<Gid>,
    pub evidence: Vec<Gid>,
    pub residuals: Vec<Gid>,
}

/// Lists changes reachable from `refs/head` following the first causal
/// parent (newest first).
pub fn log(repo: &Repo, limit: usize) -> Result<Vec<LogEntry>, Error> {
    // The trajectory of every change, derived from canonical trajectory
    // objects (a change belongs to the trajectory whose `added_changes`
    // contain it — never to whatever trajectory happens to be selected).
    let mut traj_of: HashMap<Gid, String> = HashMap::new();
    for (name, latest) in all_trajectories(repo)? {
        for (_, tobj) in trajectory_versions(repo, &latest)? {
            let fs = tobj.field_sequence().unwrap_or(&[]);
            for change in gid_list(fs, 0x06) {
                traj_of.entry(change).or_insert_with(|| name.clone());
            }
        }
    }
    let mut out = Vec::new();
    let mut current = repo.read_ref(REF_HEAD)?;
    while let Some(gid) = current {
        if out.len() >= limit {
            break;
        }
        let obj = repo.load(&gid)?;
        let fields = obj.field_sequence().unwrap_or(&[]);
        let summary = str_field(fields, 0x01).unwrap_or("").to_string();
        let input_state = gid_field(fields, 0x03);
        let resulting_state = gid_field(fields, 0x05);
        let created_at = int_field(fields, 0x15);
        let operations = gid_list(fields, 0x04);
        let claims = gid_list(fields, 0x0C);
        let evidence = gid_list(fields, 0x0D);
        let residuals = gid_list(fields, 0x0E);
        let name = crate::workflow::name_in_namespace(repo, "refs/names", &gid)?;
        let trajectory = traj_of.get(&gid).cloned();
        let parents = gid_list(fields, 0x11);
        out.push(LogEntry {
            change: gid,
            name,
            summary,
            input_state,
            resulting_state,
            trajectory,
            created_at,
            operations,
            claims,
            evidence,
            residuals,
        });
        current = parents.first().copied();
    }
    Ok(out)
}

/// The name of the current trajectory (refs/trajectories/current).
pub fn current_trajectory_name(repo: &Repo) -> Result<Option<String>, Error> {
    if let Some(gid) = repo.read_ref(&format!("{REF_TRAJECTORIES}/current"))? {
        return crate::workflow::name_in_namespace(repo, REF_TRAJECTORIES, &gid);
    }
    Ok(None)
}

/// Loads an object by identity or name.
pub fn show(repo: &Repo, name_or_id: &str) -> Result<(Gid, Object, Option<String>), Error> {
    let gid = repo.resolve(name_or_id)?;
    let obj = repo.load(&gid)?;
    let name = repo.name_of(&gid)?;
    Ok((gid, obj, name))
}

// ---------------------------------------------------------------------------
// Derived claim status (OBJECT_MODEL.md §8.1, Phase 1 basic)
// ---------------------------------------------------------------------------

/// The derived status of a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    Superseded,
    Contradicted,
    PartiallySupported,
    Supported,
    Unverified,
    Stale,
}

impl ClaimStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaimStatus::Superseded => "SUPERSEDED",
            ClaimStatus::Contradicted => "CONTRADICTED",
            ClaimStatus::PartiallySupported => "PARTIALLY_SUPPORTED",
            ClaimStatus::Supported => "SUPPORTED",
            ClaimStatus::Unverified => "UNVERIFIED",
            ClaimStatus::Stale => "STALE",
        }
    }
}

/// The derived status of a claim with its evidence split
/// (supporting/contradicting). Staleness requires state ancestry analysis
/// (Phase 2); Phase 1 reports it as unknown.
pub fn claim_status(repo: &Repo, claim: &Gid) -> Result<(ClaimStatus, Vec<Gid>, Vec<Gid>), Error> {
    let obj = repo.load(claim)?;
    if obj.family != Family::Claim {
        return Err(Error::Invalid(format!("{claim} is not a claim")));
    }
    let fields = obj.field_sequence().unwrap_or(&[]);

    // Rule 1: superseded by a newer claim.
    if is_superseded(repo, claim)? {
        return Ok((ClaimStatus::Superseded, Vec::new(), Vec::new()));
    }

    // Rules 2–4: evidence outcomes + residuals.
    let evidence = gid_list(fields, 0x08);
    let residuals = gid_list(fields, 0x09);
    let mut supporting = Vec::new();
    let mut contradicting = Vec::new();
    for ev in &evidence {
        match evidence_outcome(repo, ev)?.as_deref() {
            Some("pass") | Some("match") => supporting.push(*ev),
            Some("fail") | Some("mismatch") => contradicting.push(*ev),
            _ => {}
        }
    }
    let mut open_residual = false;
    for res in &residuals {
        if residual_disposition(repo, res)? == "open" {
            open_residual = true;
        }
    }
    let status = if !supporting.is_empty() && !contradicting.is_empty() {
        // Mixed evidence: some results support, some contradict.
        ClaimStatus::PartiallySupported
    } else if !supporting.is_empty() {
        if open_residual {
            ClaimStatus::PartiallySupported
        } else {
            ClaimStatus::Supported
        }
    } else if !contradicting.is_empty() || open_residual {
        ClaimStatus::Contradicted
    } else {
        ClaimStatus::Unverified
    };
    Ok((status, supporting, contradicting))
}

/// Whether some claim supersedes `claim`. Fast path: the derived index
/// (only when fresh); slow path: a canonical scan of `supersedes` fields.
/// The index never changes the answer — it only accelerates it.
fn is_superseded(repo: &Repo, claim: &Gid) -> Result<bool, Error> {
    let fast = |conn: &rusqlite::Connection| -> Result<bool, Error> {
        let mut stmt = conn
            .prepare("SELECT from_id FROM edges WHERE kind = 'supersedes' AND to_id = ?1 LIMIT 1")
            .map_err(|e| Error::Index(e.to_string()))?;
        let mut rows = stmt
            .query_map([claim.to_string()], |row| row.get::<_, String>(0))
            .map_err(|e| Error::Index(e.to_string()))?;
        if let Some(r) = rows.next() {
            r.map_err(|e| Error::Index(e.to_string()))?;
            return Ok(true);
        }
        Ok(false)
    };
    indexed_or_slow(repo, fast, || {
        Ok(repo.scan_canonical().into_iter().any(|(_, obj)| {
            obj.family == Family::Claim
                && obj.field_sequence().and_then(|fs| gid_field(fs, 0x0B)) == Some(*claim)
        }))
    })
}

/// Residuals explicitly linked to a claim (affected_claims). Fast path: the
/// derived index (only when fresh); slow path: a canonical scan of residual
/// `affected_claims` fields.
pub fn residuals_affecting_claim(repo: &Repo, claim: &Gid) -> Vec<Gid> {
    let fast = |conn: &rusqlite::Connection| -> Result<Vec<Gid>, Error> {
        let mut stmt = conn
            .prepare("SELECT from_id FROM edges WHERE kind = 'affected_claims' AND to_id = ?1")
            .map_err(|e| Error::Index(e.to_string()))?;
        let rows = stmt
            .query_map([claim.to_string()], |row| row.get::<_, String>(0))
            .map_err(|e| Error::Index(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows {
            let text = r.map_err(|e| Error::Index(e.to_string()))?;
            if let Ok(g) = text.parse::<Gid>() {
                out.push(g);
            }
        }
        out.sort_by_key(|a| a.to_string());
        Ok(out)
    };
    let slow = || {
        let mut out: Vec<Gid> = repo
            .scan_canonical()
            .into_iter()
            .filter_map(|(id, obj)| {
                if obj.family == Family::Residual
                    && obj
                        .field_sequence()
                        .map(|fs| gid_list(fs, 0x06).contains(claim))
                        .unwrap_or(false)
                {
                    Some(id)
                } else {
                    None
                }
            })
            .collect();
        out.sort_by_key(|a| a.to_string());
        Ok(out)
    };
    indexed_or_slow(repo, fast, slow).unwrap_or_default()
}

/// The outcome string of an evidence object's result, if present.
pub fn evidence_outcome(repo: &Repo, evidence: &Gid) -> Result<Option<String>, Error> {
    let obj = repo.load(evidence)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let result = record_field(fields, 0x0D);
    Ok(match result {
        Some(rf) => str_field(rf, 0x01).map(|s| s.to_string()),
        None => None,
    })
}

// ---------------------------------------------------------------------------
// Residual derived state (OBJECT_MODEL.md §8.3)
// ---------------------------------------------------------------------------

/// The current disposition of a residual ("open" when no event is recorded).
pub fn residual_disposition(repo: &Repo, residual: &Gid) -> Result<String, Error> {
    let obj = repo.load(residual)?;
    if obj.family != Family::Residual {
        return Err(Error::Invalid(format!("{residual} is not a residual")));
    }
    let fields = obj.field_sequence().unwrap_or(&[]);
    if let Some(event) = record_field(fields, 0x0A) {
        if let Some(d) = str_field(event, 0x01) {
            return Ok(d.to_string());
        }
    }
    Ok("open".to_string())
}

/// The persistence of a residual: the number of descendant changes of its
/// affected changes within the head-reachable subgraph (Phase 1 basic).
pub fn residual_persistence(repo: &Repo, residual: &Gid) -> Result<usize, Error> {
    let obj = repo.load(residual)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let affected = gid_list(fields, 0x07);
    // Build the reverse causal-parent map over head-reachable changes.
    let mut children: std::collections::HashMap<Gid, Vec<Gid>> = std::collections::HashMap::new();
    let mut stack = repo.read_ref(REF_HEAD)?.into_iter().collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    while let Some(c) = stack.pop() {
        if !seen.insert(c) {
            continue;
        }
        if let Ok(o) = repo.load(&c) {
            if let Some(fs) = o.field_sequence() {
                for parent in gid_list(fs, 0x11) {
                    children.entry(parent).or_default().push(c);
                }
                stack.extend(gid_list(fs, 0x11));
            }
        }
    }
    let mut count = 0usize;
    let mut queue: Vec<Gid> = affected.clone();
    let mut visited = std::collections::HashSet::new();
    while let Some(node) = queue.pop() {
        if let Some(kids) = children.get(&node) {
            for k in kids {
                if visited.insert(*k) {
                    count += 1;
                    queue.push(*k);
                }
            }
        }
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// A summarized claim for status output.
#[derive(Debug, Clone)]
pub struct ClaimSummary {
    pub gid: Gid,
    pub predicate: String,
    pub status: ClaimStatus,
}

/// A summarized residual for status output.
#[derive(Debug, Clone)]
pub struct ResidualSummary {
    pub gid: Gid,
    pub summary: String,
    pub severity: String,
    pub disposition: String,
}

/// The repository status (AGENT_PROTOCOL.md §5.1, Phase 1 data).
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub trajectory: Option<String>,
    pub intent: Option<Gid>,
    pub state: Option<Gid>,
    pub changed: Vec<(String, crate::content::PathStatus)>,
    pub claims: Vec<ClaimSummary>,
    pub residuals: Vec<ResidualSummary>,
    pub evidence_count: usize,
    /// The semantic entity count of the head state, when indexed (Phase 5).
    /// `None` means the head state is not indexed (the disposable index is an
    /// accelerator; status never builds it implicitly).
    pub semantic_entities: Option<usize>,
    pub readiness: Readiness,
}

/// The derived readiness verdict (AGENT_PROTOCOL.md §9.3, Phase 1 basic).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Readiness {
    #[default]
    Ready,
    ReadyWithResiduals,
    NotReady,
}

impl Readiness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Readiness::Ready => "READY",
            Readiness::ReadyWithResiduals => "READY_WITH_RESIDUALS",
            Readiness::NotReady => "NOT_READY",
        }
    }
}

/// Computes the repository status.
pub fn status(repo: &Repo) -> Result<Status, Error> {
    let mut st = Status::default();
    let ignore = crate::ignore::Ignore::from_root(repo.root());

    st.trajectory = current_trajectory_name(repo)?;
    let head = repo.read_ref(REF_HEAD)?;
    let base_state = match &head {
        Some(_) => repo.read_ref(REF_STATE_HEAD)?,
        None => crate::workflow::workspace_state(repo)?,
    };
    st.state = base_state;
    st.changed = crate::content::working_tree_delta(repo, base_state.as_ref(), &ignore)?;

    // Semantic index presence (Phase 5): the head state's entity count when
    // an index exists. The index is disposable — absence only means "not
    // indexed", never "no entities".
    st.semantic_entities = match base_state {
        Some(s) => match crate::semantic::index_for_state(repo, &s)? {
            Some(index) => {
                let obj = repo.load(&index)?;
                let fs = obj.field_sequence().unwrap_or(&[]);
                Some(gid_list(fs, 0x02).len())
            }
            None => None,
        },
        None => None,
    };

    let head_obj = match head {
        Some(h) => Some(repo.load(&h)?),
        None => None,
    };
    if let Some(obj) = &head_obj {
        let fields = obj.field_sequence().unwrap_or(&[]);
        st.intent = gid_field(fields, 0x02);
        for claim in gid_list(fields, 0x0C) {
            let (status, _, _) = claim_status(repo, &claim)?;
            let predicate = repo
                .load(&claim)?
                .field_sequence()
                .and_then(|fs| str_field(fs, 0x03))
                .unwrap_or("")
                .to_string();
            st.claims.push(ClaimSummary {
                gid: claim,
                predicate,
                status,
            });
        }
        st.evidence_count = gid_list(fields, 0x0D).len();
        for res in gid_list(fields, 0x0E) {
            let obj = repo.load(&res)?;
            let fs = obj.field_sequence().unwrap_or(&[]);
            st.residuals.push(ResidualSummary {
                gid: res,
                summary: str_field(fs, 0x02).unwrap_or("").to_string(),
                severity: str_field(fs, 0x04).unwrap_or("medium").to_string(),
                disposition: residual_disposition(repo, &res)?,
            });
        }
    }

    // Readiness (AGENT_PROTOCOL.md §9.3).
    let has_blocking = st
        .residuals
        .iter()
        .any(|r| r.disposition == "open" && r.severity == "blocking");
    // Readiness (AGENT_PROTOCOL.md §9.3). Contradicted and partially
    // supported claims both mean evidence disagrees: the change is not ready.
    let has_contradicted = st.claims.iter().any(|c| {
        matches!(
            c.status,
            ClaimStatus::Contradicted | ClaimStatus::PartiallySupported
        )
    });
    let has_open = st.residuals.iter().any(|r| r.disposition == "open");
    let has_unverified = st
        .claims
        .iter()
        .any(|c| c.status == ClaimStatus::Unverified);
    st.readiness = if has_blocking || has_contradicted {
        Readiness::NotReady
    } else if has_open || has_unverified {
        Readiness::ReadyWithResiduals
    } else {
        Readiness::Ready
    };
    Ok(st)
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// A string field value.
pub fn str_field(fields: &[Field], tag: u8) -> Option<&str> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// A gid field value.
pub fn gid_field(fields: &[Field], tag: u8) -> Option<Gid> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::Gid(g)) => Some(*g),
        _ => None,
    }
}

/// An integer (SINT) field value.
pub fn int_field(fields: &[Field], tag: u8) -> Option<i64> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::I(v)) => Some(*v),
        _ => None,
    }
}

/// A list of gid field values.
pub fn gid_list(fields: &[Field], tag: u8) -> Vec<Gid> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Gid(g) => Some(*g),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A record field value.
pub fn record_field(fields: &[Field], tag: u8) -> Option<&[Field]> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::Record(inner)) => Some(inner),
        _ => None,
    }
}

/// A raw value at a field tag (used by renderers).
pub fn value_at(fields: &[Field], tag: u8) -> Option<&Value> {
    fields.iter().find(|f| f.tag == tag).map(|f| &f.value)
}

/// An unsigned integer field value.
pub fn u64_field(fields: &[Field], tag: u8) -> Option<u64> {
    match fields.iter().find(|f| f.tag == tag).map(|f| &f.value) {
        Some(Value::U(v)) => Some(*v),
        _ => None,
    }
}

/// Reads the head state (refs/state/head), if any.
pub fn head_state(repo: &Repo) -> Result<Option<Gid>, Error> {
    repo.read_ref(REF_STATE_HEAD)
}

/// Resolves a state reference by name or identity.
pub fn resolve_state(repo: &Repo, name_or_id: &str) -> Result<Gid, Error> {
    let gid = repo.resolve(name_or_id)?;
    let obj = repo.load(&gid)?;
    if obj.family != Family::State {
        return Err(Error::Invalid(format!("{name_or_id} is not a state")));
    }
    Ok(gid)
}

// ---------------------------------------------------------------------------
// Phase 2 — the agent-native query surface (AGENT_PROTOCOL.md §5–§6, §9)
// ---------------------------------------------------------------------------
//
// Every query returns object references first (progressive disclosure); all
// ordering is deterministic ((created_at desc, gid asc) unless noted); all
// derived values are computed from the canonical graph, never stored.

/// Consults the derived index only when it is fresh; otherwise falls back to
/// the canonical slow path. The index is an accelerator, never an oracle
/// (INVARIANTS DER-01): both paths must produce identical answers — with the
/// index, quickly; without it, slowly. Never differently.
fn indexed_or_slow<T>(
    repo: &Repo,
    fast: impl FnOnce(&rusqlite::Connection) -> Result<T, Error>,
    slow: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    if repo.index_is_fresh() {
        if let Ok(conn) = crate::store::index::open_for_query(repo) {
            if let Ok(v) = fast(&conn) {
                return Ok(v);
            }
        }
    }
    slow()
}

/// The latest version of an append-chained object (trajectory, residual,
/// checkpoint, config, case): follows `previous` edges. Fast path: the
/// derived index (only when fresh). Slow path: a canonical scan of `previous`
/// fields — identical answers, always.
pub fn chain_latest(repo: &Repo, gid: &Gid) -> Result<Gid, Error> {
    let fast = |conn: &rusqlite::Connection| -> Result<Gid, Error> {
        let mut current = *gid;
        let mut guard = 0usize;
        loop {
            guard += 1;
            if guard > 4096 {
                return Err(Error::Limit {
                    kind: "chain depth",
                    limit: 4096,
                    found: guard as u64,
                });
            }
            let next: Option<String> = conn
                .query_row(
                    "SELECT from_id FROM edges WHERE kind = 'previous' AND to_id = ?1 LIMIT 1",
                    rusqlite::params![current.to_string()],
                    |row| row.get(0),
                )
                .ok();
            match next {
                Some(n) => match n.parse::<Gid>() {
                    Ok(g) => current = g,
                    Err(_) => break,
                },
                None => break,
            }
        }
        Ok(current)
    };
    indexed_or_slow(repo, fast, || chain_latest_slow(repo, gid))
}

/// Canonical slow path for [`chain_latest`]: one scan of every object,
/// building the `previous → gid` map.
fn chain_latest_slow(repo: &Repo, gid: &Gid) -> Result<Gid, Error> {
    let mut next_of: HashMap<Gid, Gid> = HashMap::new();
    for (id, obj) in repo.scan_canonical() {
        if let Some(prev) = obj.field_sequence().and_then(|fs| gid_field(fs, 0x01)) {
            next_of.insert(prev, id);
        }
    }
    let mut current = *gid;
    let mut guard = 0usize;
    while let Some(next) = next_of.get(&current) {
        guard += 1;
        if guard > 4096 {
            return Err(Error::Limit {
                kind: "chain depth",
                limit: 4096,
                found: guard as u64,
            });
        }
        current = *next;
    }
    Ok(current)
}

/// All trajectory versions reachable from `refs/trajectories/*` as
/// `(name, latest_gid)` sorted by name.
pub fn all_trajectories(repo: &Repo) -> Result<Vec<(String, Gid)>, Error> {
    let mut out = Vec::new();
    let prefix = format!("{REF_TRAJECTORIES}/");
    for (name, gid) in repo.all_refs()? {
        if let Some(short) = name.strip_prefix(&prefix) {
            if short != "current" {
                out.push((short.to_string(), gid));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Walks a trajectory chain (newest first) collecting every version object.
pub fn trajectory_versions(repo: &Repo, latest: &Gid) -> Result<Vec<(Gid, Object)>, Error> {
    let mut versions = Vec::new();
    let mut seen = HashSet::new();
    let mut current = *latest;
    let mut guard = 0usize;
    while seen.insert(current) {
        guard += 1;
        if guard > 4096 {
            return Err(Error::Limit {
                kind: "trajectory chain",
                limit: 4096,
                found: guard as u64,
            });
        }
        let obj = repo.load(&current)?;
        if obj.family != Family::Trajectory {
            break;
        }
        let prev = obj.field_sequence().and_then(|fs| gid_field(fs, 0x01));
        versions.push((current, obj));
        match prev {
            Some(p) => current = p,
            None => break,
        }
    }
    Ok(versions)
}

/// Whether a change touches `subject`: an operation with matching
/// `subject_path` (or a referenced input/output gid), or a claim with a
/// matching subject. `subject` may be a canonical path, an entity name, or a
/// textual identity.
pub fn change_touches(repo: &Repo, change: &Gid, subject: &str) -> Result<bool, Error> {
    let obj = repo.load(change)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let subject_gid = subject.parse::<Gid>().ok();
    for op in gid_list(fields, 0x04) {
        let op_obj = match repo.load(&op) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let ofs = op_obj.field_sequence().unwrap_or(&[]);
        if str_field(ofs, 0x02) == Some(subject) {
            return Ok(true);
        }
        if let Some(sg) = subject_gid {
            for list in [gid_list(ofs, 0x04), gid_list(ofs, 0x05)] {
                if list.contains(&sg) {
                    return Ok(true);
                }
            }
        }
    }
    for claim in gid_list(fields, 0x0C) {
        let claim_obj = match repo.load(&claim) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let cfs = claim_obj.field_sequence().unwrap_or(&[]);
        if str_field(cfs, 0x01) == Some(subject) {
            return Ok(true);
        }
        if let Some(sg) = subject_gid {
            if gid_list(cfs, 0x02).contains(&sg) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Like [`change_touches`] but true when any of several subject aliases
/// matches (semantic resolution: file paths, module paths, lineage names).
pub fn change_touches_subjects(
    repo: &Repo,
    change: &Gid,
    subjects: &[String],
) -> Result<bool, Error> {
    for s in subjects {
        if change_touches(repo, change, s)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One change that touches a subject, with its context.
#[derive(Debug, Clone)]
pub struct SubjectHit {
    pub change: Gid,
    pub change_name: Option<String>,
    pub summary: String,
    pub created_at: Option<i64>,
    pub operations: Vec<Gid>,
    pub claims: Vec<Gid>,
    pub residuals: Vec<Gid>,
    pub trajectory: Option<(String, Gid)>,
}

/// All changes touching `subject`, deterministically ordered
/// ((created_at desc, gid asc); ties and missing timestamps on gid).
pub fn changes_touching(repo: &Repo, subject: &str) -> Result<Vec<SubjectHit>, Error> {
    changes_touching_subjects(repo, &[subject.to_string()])
}

/// Like [`changes_touching`] but over several subject aliases (semantic
/// resolution: file paths, module paths, and lineage-chain names all count).
/// Deduplicated by change; the result is re-sorted deterministically.
pub fn changes_touching_subjects(
    repo: &Repo,
    subjects: &[String],
) -> Result<Vec<SubjectHit>, Error> {
    let mut all: Vec<SubjectHit> = Vec::new();
    let mut seen: std::collections::HashSet<Gid> = std::collections::HashSet::new();
    for subject in subjects {
        for hit in changes_touching_one(repo, subject)? {
            if seen.insert(hit.change) {
                all.push(hit);
            }
        }
    }
    sort_by_time_desc(&mut all, |h| h.created_at, |h| h.change);
    Ok(all)
}

/// The single-subject walk behind [`changes_touching`].
fn changes_touching_one(repo: &Repo, subject: &str) -> Result<Vec<SubjectHit>, Error> {
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let visit = |repo: &Repo,
                 change: Gid,
                 traj: Option<(String, Gid)>,
                 hits: &mut Vec<SubjectHit>,
                 seen: &mut HashSet<Gid>|
     -> Result<(), Error> {
        if !seen.insert(change) {
            return Ok(());
        }
        if !change_touches(repo, &change, subject)? {
            return Ok(());
        }
        let obj = repo.load(&change)?;
        let fields = obj.field_sequence().unwrap_or(&[]);
        hits.push(SubjectHit {
            change,
            change_name: crate::workflow::name_in_namespace(repo, "refs/names", &change)?,
            summary: str_field(fields, 0x01).unwrap_or("").to_string(),
            created_at: int_field(fields, 0x15),
            operations: gid_list(fields, 0x04),
            claims: gid_list(fields, 0x0C),
            residuals: gid_list(fields, 0x0E),
            trajectory: traj,
        });
        Ok(())
    };
    // Every trajectory (latest version), walking the chain and its changes.
    for (name, latest) in all_trajectories(repo)? {
        let versions = trajectory_versions(repo, &latest)?;
        let mut chain_changes: Vec<Gid> = Vec::new();
        for (_, obj) in &versions {
            let fs = obj.field_sequence().unwrap_or(&[]);
            chain_changes.extend(gid_list(fs, 0x06));
        }
        for change in chain_changes {
            visit(
                repo,
                change,
                Some((name.clone(), latest)),
                &mut hits,
                &mut seen,
            )?;
        }
    }
    // The head causal chain (defense in depth; deduplicated above).
    let mut current = repo.read_ref(REF_HEAD)?;
    while let Some(gid) = current {
        let obj = match repo.load(&gid) {
            Ok(o) => o,
            Err(_) => break,
        };
        let fields = obj.field_sequence().unwrap_or(&[]);
        let parents = gid_list(fields, 0x11);
        visit(repo, gid, None, &mut hits, &mut seen)?;
        current = parents.first().copied();
    }
    sort_by_time_desc(&mut hits, |h| h.created_at, |h| h.change);
    Ok(hits)
}

/// Deterministic (created_at desc, gid asc) sort. Missing timestamps sort
/// as 0 (oldest), then by gid.
fn sort_by_time_desc<T>(
    items: &mut [T],
    time: impl Fn(&T) -> Option<i64>,
    gid: impl Fn(&T) -> Gid,
) {
    items.sort_by(|a, b| {
        let (ta, tb) = (time(a).unwrap_or(0), time(b).unwrap_or(0));
        tb.cmp(&ta)
            .then_with(|| gid(a).to_string().cmp(&gid(b).to_string()))
    });
}

/// Cursor pagination over a deterministically ordered list: `cursor` is the
/// opaque `<created_at>:<gid>` tuple of the last item of the previous page.
/// Ordering is (created_at desc, gid asc), so an item at-or-before the cursor
/// (newer timestamp, or equal timestamp with a lexically smaller-or-equal
/// gid) is skipped. Returns the page, the next cursor, and whether more
/// items follow.
pub fn page_by_time<T>(
    items: Vec<T>,
    limit: usize,
    cursor: Option<&str>,
    time: impl Fn(&T) -> Option<i64>,
    gid: impl Fn(&T) -> Gid,
) -> (Vec<T>, Option<String>) {
    let after: Option<(i64, String)> = cursor.and_then(|c| {
        let (t, g) = c.split_once(':')?;
        Some((t.parse::<i64>().ok()?, g.to_string()))
    });
    let at_or_before = |item: &T, after: &(i64, String)| -> bool {
        let it = time(item).unwrap_or(0);
        let ig = gid(item).to_string();
        it > after.0 || (it == after.0 && ig <= after.1)
    };
    let mut page: Vec<T> = Vec::new();
    let mut has_more = false;
    for item in items {
        if let Some(a) = &after {
            if at_or_before(&item, a) {
                continue;
            }
        }
        if page.len() >= limit {
            has_more = true;
            break;
        }
        page.push(item);
    }
    let next = if has_more {
        page.last()
            .map(|last| format!("{}:{}", time(last).unwrap_or(0), gid(last)))
    } else {
        None
    };
    (page, next)
}

// ---------------------------------------------------------------------------
// claims (AGENT_PROTOCOL.md §5.3)
// ---------------------------------------------------------------------------

/// One claim row in `gemel claims`.
#[derive(Debug, Clone)]
pub struct ClaimRow {
    pub gid: Gid,
    pub predicate: String,
    pub predicate_kind: Option<String>,
    pub subject: Option<String>,
    pub scope: Option<String>,
    pub status: ClaimStatus,
    pub supporting: Vec<Gid>,
    pub contradicting: Vec<Gid>,
    pub change: Option<Gid>,
    pub trajectory: Option<String>,
    pub created_at: Option<i64>,
}

/// Filters for `gemel claims`.
#[derive(Debug, Clone, Default)]
pub struct ClaimsFilter {
    pub subject: Option<String>,
    pub status: Option<ClaimStatus>,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// All claims reachable from trajectory changes, deduplicated, filtered,
/// paginated.
pub fn claims(
    repo: &Repo,
    filter: &ClaimsFilter,
) -> Result<(Vec<ClaimRow>, Option<String>), Error> {
    // Collect every claim referenced by any trajectory change.
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut decl = HashMap::new();
    for (traj_name, latest) in all_trajectories(repo)? {
        for (_, tobj) in trajectory_versions(repo, &latest)? {
            let tfs = tobj.field_sequence().unwrap_or(&[]);
            for change in gid_list(tfs, 0x06) {
                let cobj = match repo.load(&change) {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let cfs = cobj.field_sequence().unwrap_or(&[]);
                for claim in gid_list(cfs, 0x0C) {
                    if seen.insert(claim) {
                        decl.insert(claim, (Some(change), Some(traj_name.clone())));
                    }
                }
            }
        }
    }
    for claim in &seen {
        let obj = match repo.load(claim) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let fields = obj.field_sequence().unwrap_or(&[]);
        let subject = str_field(fields, 0x01).map(|s| s.to_string());
        if let Some(want) = &filter.subject {
            if subject.as_deref() != Some(want.as_str()) {
                continue;
            }
        }
        let (status, supporting, contradicting) = claim_status(repo, claim)?;
        if let Some(want) = filter.status {
            if status != want {
                continue;
            }
        }
        let (change, trajectory) = decl.get(claim).cloned().unwrap_or((None, None));
        rows.push(ClaimRow {
            gid: *claim,
            predicate: str_field(fields, 0x03).unwrap_or("").to_string(),
            predicate_kind: str_field(fields, 0x04).map(|s| s.to_string()),
            subject,
            scope: str_field(fields, 0x05).map(|s| s.to_string()),
            status,
            supporting,
            contradicting,
            change,
            trajectory,
            created_at: int_field(fields, 0x0E),
        });
    }
    sort_by_time_desc(&mut rows, |r| r.created_at, |r| r.gid);
    let limit = if filter.limit == 0 { 100 } else { filter.limit };
    Ok(page_by_time(
        rows,
        limit,
        filter.cursor.as_deref(),
        |r| r.created_at,
        |r| r.gid,
    ))
}

// ---------------------------------------------------------------------------
// evidence (AGENT_PROTOCOL.md §5.4)
// ---------------------------------------------------------------------------

/// The derived freshness of evidence (§OBJECT_MODEL 8.4, Phase 2 basic):
/// `CURRENT` when the evaluated state is the head state; otherwise
/// conservatively `MAY_REQUIRE_REFRESH` (impact analysis is Phase 3+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    Current,
    MayRequireRefresh,
}

impl Freshness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Freshness::Current => "CURRENT",
            Freshness::MayRequireRefresh => "MAY_REQUIRE_REFRESH",
        }
    }
}

/// One evidence row with derived freshness.
#[derive(Debug, Clone)]
pub struct EvidenceRow {
    pub gid: Gid,
    pub kind: String,
    pub subject: Option<String>,
    pub outcome: Option<String>,
    pub evaluated_state: Option<Gid>,
    pub freshness: Freshness,
    pub reproduction_replayable: Option<bool>,
    pub producer: Option<Gid>,
    pub created_at: Option<i64>,
}

/// Derives the freshness of an evidence object.
pub fn evidence_freshness(repo: &Repo, evidence: &Gid) -> Result<Freshness, Error> {
    let obj = repo.load(evidence)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let evaluated = gid_field(fields, 0x11);
    let head_state = repo.read_ref(REF_STATE_HEAD)?;
    Ok(match (evaluated, head_state) {
        (Some(e), Some(h)) if e == h => Freshness::Current,
        (Some(_), Some(_)) => Freshness::MayRequireRefresh,
        _ => Freshness::Current,
    })
}

/// A single evidence object with derived freshness.
pub fn evidence_show(repo: &Repo, name_or_id: &str) -> Result<EvidenceRow, Error> {
    let gid = repo.resolve(name_or_id)?;
    let obj = repo.load(&gid)?;
    if obj.family != Family::Evidence {
        return Err(Error::Invalid(format!("{name_or_id} is not evidence")));
    }
    let fields = obj.field_sequence().unwrap_or(&[]);
    let result = record_field(fields, 0x0D);
    let reproduction = record_field(fields, 0x0F);
    Ok(EvidenceRow {
        gid,
        kind: str_field(fields, 0x02).unwrap_or("").to_string(),
        subject: str_field(fields, 0x03).map(|s| s.to_string()),
        outcome: result
            .and_then(|r| str_field(r, 0x01))
            .map(|s| s.to_string()),
        evaluated_state: gid_field(fields, 0x11),
        freshness: evidence_freshness(repo, &gid)?,
        reproduction_replayable: reproduction.and_then(|r| match value_at(r, 0x01) {
            Some(Value::B(b)) => Some(*b),
            _ => None,
        }),
        producer: gid_field(fields, 0x01),
        created_at: int_field(fields, 0x10),
    })
}

/// Evidence for a subject, deterministically ordered.
pub fn evidence_for_subject(repo: &Repo, subject: &str) -> Result<Vec<EvidenceRow>, Error> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (_, latest) in all_trajectories(repo)? {
        for (_, tobj) in trajectory_versions(repo, &latest)? {
            let tfs = tobj.field_sequence().unwrap_or(&[]);
            for change in gid_list(tfs, 0x06) {
                let cobj = match repo.load(&change) {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let cfs = cobj.field_sequence().unwrap_or(&[]);
                for ev in gid_list(cfs, 0x0D) {
                    if !seen.insert(ev) {
                        continue;
                    }
                    let eobj = match repo.load(&ev) {
                        Ok(o) => o,
                        Err(_) => continue,
                    };
                    let efs = eobj.field_sequence().unwrap_or(&[]);
                    if str_field(efs, 0x03) == Some(subject) {
                        out.push(evidence_row(repo, &ev)?);
                    }
                }
            }
        }
    }
    sort_by_time_desc(&mut out, |e| e.created_at, |e| e.gid);
    Ok(out)
}

fn evidence_row(repo: &Repo, gid: &Gid) -> Result<EvidenceRow, Error> {
    let obj = repo.load(gid)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    let result = record_field(fields, 0x0D);
    let reproduction = record_field(fields, 0x0F);
    Ok(EvidenceRow {
        gid: *gid,
        kind: str_field(fields, 0x02).unwrap_or("").to_string(),
        subject: str_field(fields, 0x03).map(|s| s.to_string()),
        outcome: result
            .and_then(|r| str_field(r, 0x01))
            .map(|s| s.to_string()),
        evaluated_state: gid_field(fields, 0x11),
        freshness: evidence_freshness(repo, gid)?,
        reproduction_replayable: reproduction.and_then(|r| match value_at(r, 0x01) {
            Some(Value::B(b)) => Some(*b),
            _ => None,
        }),
        producer: gid_field(fields, 0x01),
        created_at: int_field(fields, 0x10),
    })
}

// ---------------------------------------------------------------------------
// residuals (AGENT_PROTOCOL.md §5.5)
// ---------------------------------------------------------------------------

/// One residual row (latest chain version) with derived state.
#[derive(Debug, Clone)]
pub struct ResidualRow {
    pub gid: Gid,
    pub summary: String,
    pub classification: Option<String>,
    pub severity: Option<String>,
    pub disposition: String,
    pub persistence: usize,
    pub origin_evidence: Option<Gid>,
    pub affected_claims: Vec<Gid>,
    pub affected_changes: Vec<Gid>,
    pub created_at: Option<i64>,
}

/// Filters for `gemel residuals`.
#[derive(Debug, Clone, Default)]
pub struct ResidualsFilter {
    pub subject: Option<String>,
    pub disposition: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// All residuals reachable from trajectory changes (latest chain version
/// each), filtered, paginated, deterministic order.
pub fn residuals(
    repo: &Repo,
    filter: &ResidualsFilter,
) -> Result<(Vec<ResidualRow>, Option<String>), Error> {
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for (_, latest) in all_trajectories(repo)? {
        for (_, tobj) in trajectory_versions(repo, &latest)? {
            let tfs = tobj.field_sequence().unwrap_or(&[]);
            for change in gid_list(tfs, 0x06) {
                let cobj = match repo.load(&change) {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                let cfs = cobj.field_sequence().unwrap_or(&[]);
                for res in gid_list(cfs, 0x0E) {
                    let latest = chain_latest(repo, &res)?;
                    if !seen.insert(latest) {
                        continue;
                    }
                    let row = residual_row(repo, &latest)?;
                    if let Some(want) = &filter.subject {
                        let touches = subject_affects_residual(repo, want, &latest)?;
                        if !touches {
                            continue;
                        }
                    }
                    if let Some(want) = &filter.disposition {
                        if &row.disposition != want {
                            continue;
                        }
                    }
                    rows.push(row);
                }
            }
        }
    }
    sort_by_time_desc(&mut rows, |r| r.created_at, |r| r.gid);
    let limit = if filter.limit == 0 { 100 } else { filter.limit };
    Ok(page_by_time(
        rows,
        limit,
        filter.cursor.as_deref(),
        |r| r.created_at,
        |r| r.gid,
    ))
}

fn residual_row(repo: &Repo, gid: &Gid) -> Result<ResidualRow, Error> {
    let obj = repo.load(gid)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    Ok(ResidualRow {
        gid: *gid,
        summary: str_field(fields, 0x02).unwrap_or("").to_string(),
        classification: str_field(fields, 0x03).map(|s| s.to_string()),
        severity: str_field(fields, 0x04).map(|s| s.to_string()),
        disposition: residual_disposition(repo, gid)?,
        persistence: residual_persistence(repo, gid)?,
        origin_evidence: gid_field(fields, 0x08),
        affected_claims: gid_list(fields, 0x06),
        affected_changes: gid_list(fields, 0x07),
        created_at: int_field(fields, 0x0C),
    })
}

/// Whether a residual (latest version) affects `subject`: an affected claim
/// about the subject, or an affected change touching the subject.
pub fn subject_affects_residual(repo: &Repo, subject: &str, residual: &Gid) -> Result<bool, Error> {
    let obj = repo.load(residual)?;
    let fields = obj.field_sequence().unwrap_or(&[]);
    for claim in gid_list(fields, 0x06) {
        if let Ok(cobj) = repo.load(&claim) {
            let cfs = cobj.field_sequence().unwrap_or(&[]);
            if str_field(cfs, 0x01) == Some(subject) {
                return Ok(true);
            }
        }
    }
    for change in gid_list(fields, 0x07) {
        if change_touches(repo, &change, subject)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// attempts (AGENT_PROTOCOL.md §5.6)
// ---------------------------------------------------------------------------

/// One attempt (trajectory) relevant to a subject.
#[derive(Debug, Clone)]
pub struct AttemptSummary {
    pub trajectory: Gid,
    pub name: Option<String>,
    pub intent: Option<Gid>,
    pub outcome: Option<String>,
    pub termination_reason: Option<String>,
    pub evidence: Vec<Gid>,
    pub residuals: Vec<Gid>,
    pub handoff_summary: Option<String>,
    pub touched_subject: bool,
    pub created_at: Option<i64>,
}

/// Trajectories whose changes touch the subject, plus trajectories sharing
/// the intent of the touching changes. Touching attempts first; deterministic.
/// Phase 5: subject aliases (semantic entity paths and lineage ancestors) are
/// matched too, so a moved entity surfaces attempts against its earlier home.
pub fn attempts(repo: &Repo, subject: &str) -> Result<Vec<AttemptSummary>, Error> {
    let resolved = crate::semantic::resolve_subject(repo, subject)?;
    let mut subjects = vec![subject.to_string()];
    subjects.extend(resolved.aliases.iter().cloned());
    let subjects = subjects;
    let mut out = Vec::new();
    let mut shared_intents: HashSet<Gid> = HashSet::new();
    for hit in changes_touching_subjects(repo, &subjects)? {
        if let Some(t) = &hit.trajectory {
            if let Some(intent) = trajectory_intent(repo, &t.1)? {
                shared_intents.insert(intent);
            }
        }
    }
    for (name, latest) in all_trajectories(repo)? {
        let versions = trajectory_versions(repo, &latest)?;
        let mut touched = false;
        let mut evidence = Vec::new();
        let mut residuals = Vec::new();
        let mut ev_seen = HashSet::new();
        let mut res_seen = HashSet::new();
        for (vid, vobj) in &versions {
            let vfs = vobj.field_sequence().unwrap_or(&[]);
            for change in gid_list(vfs, 0x06) {
                if change_touches_subjects(repo, &change, &subjects)? {
                    touched = true;
                }
            }
            for ev in gid_list(vfs, 0x08) {
                if ev_seen.insert(ev) {
                    evidence.push(ev);
                }
            }
            for res in gid_list(vfs, 0x09) {
                if res_seen.insert(res) {
                    residuals.push(res);
                }
            }
            let _ = vid;
        }
        let latest_obj = &versions[0].1;
        let lfs = latest_obj.field_sequence().unwrap_or(&[]);
        let intent = gid_field(lfs, 0x02);
        let shares_intent = intent.map(|i| shared_intents.contains(&i)).unwrap_or(false);
        if !touched && !shares_intent {
            continue;
        }
        let handoff = record_field(lfs, 0x0C);
        out.push(AttemptSummary {
            trajectory: latest,
            name: Some(name),
            intent,
            outcome: str_field(lfs, 0x0A).map(|s| s.to_string()),
            termination_reason: str_field(lfs, 0x0B).map(|s| s.to_string()),
            evidence,
            residuals,
            handoff_summary: handoff
                .and_then(|h| str_field(h, 0x01))
                .map(|s| s.to_string()),
            touched_subject: touched,
            created_at: int_field(lfs, 0x0D),
        });
    }
    // Touching attempts first; then (created_at desc, gid asc).
    out.sort_by(|a, b| {
        b.touched_subject
            .cmp(&a.touched_subject)
            .then_with(|| {
                let (ta, tb) = (a.created_at.unwrap_or(0), b.created_at.unwrap_or(0));
                tb.cmp(&ta)
            })
            .then_with(|| a.trajectory.to_string().cmp(&b.trajectory.to_string()))
    });
    Ok(out)
}

fn trajectory_intent(repo: &Repo, latest: &Gid) -> Result<Option<Gid>, Error> {
    let obj = repo.load(latest)?;
    Ok(obj.field_sequence().and_then(|fs| gid_field(fs, 0x02)))
}

// ---------------------------------------------------------------------------
// trajectory (AGENT_PROTOCOL.md §5.7)
// ---------------------------------------------------------------------------

/// One change in a trajectory's sequence.
#[derive(Debug, Clone)]
pub struct TrajectoryChange {
    pub change: Gid,
    pub summary: String,
    pub state: Option<Gid>,
    pub created_at: Option<i64>,
}

/// A trajectory with its full materialized sequence.
#[derive(Debug, Clone)]
pub struct TrajectoryDetail {
    pub gid: Gid,
    pub name: Option<String>,
    pub intent: Option<Gid>,
    pub base_state: Option<Gid>,
    pub outcome: Option<String>,
    pub termination_reason: Option<String>,
    pub sequence: Vec<TrajectoryChange>,
    pub evidence: Vec<Gid>,
    pub residuals: Vec<Gid>,
    pub handoff: Option<HandoffDetail>,
    pub created_at: Option<i64>,
}

/// The structured handoff record (OBJECT_MODEL.md §6.9).
#[derive(Debug, Clone, Default)]
pub struct HandoffDetail {
    pub summary: Option<String>,
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub open_residuals: Vec<Gid>,
    pub important_evidence: Vec<Gid>,
    pub recommended_objects: Vec<Gid>,
    pub next_steps: Vec<String>,
}

/// Materializes a trajectory (by name or identity) with its change sequence
/// (earliest → latest), accumulated evidence/residuals, and handoff.
pub fn trajectory_detail(repo: &Repo, name_or_id: &str) -> Result<TrajectoryDetail, Error> {
    let gid = repo.resolve(name_or_id)?;
    let obj = repo.load(&gid)?;
    if obj.family != Family::Trajectory {
        return Err(Error::Invalid(format!("{name_or_id} is not a trajectory")));
    }
    let versions = trajectory_versions(repo, &gid)?;
    let name = crate::workflow::name_in_namespace(repo, REF_TRAJECTORIES, &gid)?;
    let latest_fields = versions[0].1.field_sequence().unwrap_or(&[]);
    let mut sequence: Vec<TrajectoryChange> = Vec::new();
    let mut evidence = Vec::new();
    let mut residuals = Vec::new();
    let mut ev_seen = HashSet::new();
    let mut res_seen = HashSet::new();
    let mut ch_seen = HashSet::new();
    // Chain versions are newest → oldest; the sequence is earliest → latest.
    for (_, vobj) in versions.iter().rev() {
        let vfs = vobj.field_sequence().unwrap_or(&[]);
        for change in gid_list(vfs, 0x06) {
            if !ch_seen.insert(change) {
                continue;
            }
            let cobj = match repo.load(&change) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let cfs = cobj.field_sequence().unwrap_or(&[]);
            sequence.push(TrajectoryChange {
                change,
                summary: str_field(cfs, 0x01).unwrap_or("").to_string(),
                state: gid_field(cfs, 0x05),
                created_at: int_field(cfs, 0x15),
            });
        }
        for ev in gid_list(vfs, 0x08) {
            if ev_seen.insert(ev) {
                evidence.push(ev);
            }
        }
        for res in gid_list(vfs, 0x09) {
            if res_seen.insert(res) {
                residuals.push(res);
            }
        }
    }
    let handoff = record_field(latest_fields, 0x0C).map(|h| HandoffDetail {
        summary: str_field(h, 0x01).map(|s| s.to_string()),
        completed: str_list(h, 0x02),
        remaining: str_list(h, 0x03),
        open_residuals: gid_list_in(h, 0x04),
        important_evidence: gid_list_in(h, 0x05),
        recommended_objects: gid_list_in(h, 0x06),
        next_steps: str_list(h, 0x07),
    });
    Ok(TrajectoryDetail {
        gid,
        name,
        intent: gid_field(latest_fields, 0x02),
        base_state: gid_field(latest_fields, 0x03),
        outcome: str_field(latest_fields, 0x0A).map(|s| s.to_string()),
        termination_reason: str_field(latest_fields, 0x0B).map(|s| s.to_string()),
        sequence,
        evidence,
        residuals,
        handoff,
        created_at: int_field(latest_fields, 0x0D),
    })
}

/// String list values of a field inside a record.
fn str_list(fields: &[Field], tag: u8) -> Vec<String> {
    match value_at(fields, tag) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// GID list values of a field inside a record.
fn gid_list_in(fields: &[Field], tag: u8) -> Vec<Gid> {
    match value_at(fields, tag) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Gid(g) => Some(*g),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// why (AGENT_PROTOCOL.md §5.2)
// ---------------------------------------------------------------------------

/// One evidence detail inside a `why` chain.
#[derive(Debug, Clone)]
pub struct WhyEvidence {
    pub id: Gid,
    pub kind: String,
    pub subject: Option<String>,
    pub outcome: Option<String>,
}

/// One residual detail inside a `why` chain.
#[derive(Debug, Clone)]
pub struct WhyResidual {
    pub id: Gid,
    pub summary: String,
    pub severity: Option<String>,
    pub disposition: String,
}

/// The `introduced_by` node of a `why` traversal.
#[derive(Debug, Clone)]
pub struct WhyNode {
    pub change: Gid,
    pub change_name: Option<String>,
    pub summary: String,
    pub created_at: Option<i64>,
    pub intent: Option<Gid>,
    pub intent_summary: Option<String>,
    pub claim: Option<WhyClaim>,
    pub evidence: Vec<WhyEvidence>,
    pub residuals: Vec<WhyResidual>,
}

/// The claim inside a `why` node.
#[derive(Debug, Clone)]
pub struct WhyClaim {
    pub id: Gid,
    pub predicate: String,
    pub status: ClaimStatus,
}

/// The full `why` report.
#[derive(Debug, Clone)]
pub struct WhyReport {
    pub subject: String,
    /// The resolved semantic entity (Phase 5), when the subject resolves to
    /// one. Explicit lineage is preserved; never silently inferred.
    pub semantic: Option<crate::semantic::EntityInfo>,
    pub introduced_by: Option<WhyNode>,
    pub last_modified: Option<Gid>,
    pub previous_approaches: Vec<AttemptSummary>,
    pub uncertainty: Vec<String>,
}

/// Causal blame (brief §14): subject → Change → Intent → Claim → Evidence →
/// Residual → (decision). Phase 2: no reconciliation decision nodes yet.
/// Phase 5: when the subject resolves to a semantic entity, the walk uses the
/// entity's aliases (file path, module path, lineage ancestors) so moves and
/// renames surface the work that touched the entity across its history.
pub fn why(repo: &Repo, subject: &str) -> Result<WhyReport, Error> {
    let resolved = crate::semantic::resolve_subject(repo, subject)?;
    let mut subjects = vec![subject.to_string()];
    subjects.extend(resolved.aliases.iter().cloned());
    let hits = changes_touching_subjects(repo, &subjects)?;
    let mut report = WhyReport {
        subject: subject.to_string(),
        semantic: resolved.entity,
        introduced_by: None,
        last_modified: None,
        previous_approaches: Vec::new(),
        uncertainty: Vec::new(),
    };
    if hits.is_empty() {
        report
            .uncertainty
            .push(format!("no change in this repository touches {subject:?}"));
        return Ok(report);
    }
    // introduced_by = the earliest touching change (first appearance);
    // last_modified = the newest.
    let mut ordered = hits.clone();
    ordered.sort_by(|a, b| {
        let (ta, tb) = (a.created_at.unwrap_or(0), b.created_at.unwrap_or(0));
        ta.cmp(&tb)
            .then_with(|| a.change.to_string().cmp(&b.change.to_string()))
    });
    let first = &ordered[0];
    let last = ordered.last().unwrap();
    report.last_modified = Some(last.change);

    // The claim: from the introducing change's own claims about the subject,
    // else the latest claim about the subject anywhere.
    let mut claim: Option<Gid> = None;
    for c in &first.claims {
        if claim_subject_is(repo, c, subject)? {
            claim = Some(*c);
            break;
        }
    }
    if claim.is_none() {
        let mut all = claims(
            repo,
            &ClaimsFilter {
                subject: Some(subject.to_string()),
                ..Default::default()
            },
        )?
        .0;
        all.sort_by_key(|r| {
            (
                std::cmp::Reverse(r.created_at.unwrap_or(0)),
                r.gid.to_string(),
            )
        });
        claim = all.first().map(|r| r.gid);
    }

    let mut node = WhyNode {
        change: first.change,
        change_name: first.change_name.clone(),
        summary: first.summary.clone(),
        created_at: first.created_at,
        intent: None,
        intent_summary: None,
        claim: None,
        evidence: Vec::new(),
        residuals: Vec::new(),
    };
    let intent = {
        let cobj = repo.load(&first.change)?;
        let cfs = cobj.field_sequence().unwrap_or(&[]);
        gid_field(cfs, 0x02)
    };
    node.intent = intent;
    if let Some(i) = intent {
        if let Ok(iobj) = repo.load(&i) {
            let ifs = iobj.field_sequence().unwrap_or(&[]);
            node.intent_summary = str_field(ifs, 0x01).map(|s| s.to_string());
        }
    }
    if let Some(c) = claim {
        if let Ok(cobj) = repo.load(&c) {
            let cfs = cobj.field_sequence().unwrap_or(&[]);
            let (status, _, _) = claim_status(repo, &c)?;
            node.claim = Some(WhyClaim {
                id: c,
                predicate: str_field(cfs, 0x03).unwrap_or("").to_string(),
                status,
            });
            for ev in gid_list(cfs, 0x08) {
                if let Ok(eobj) = repo.load(&ev) {
                    let efs = eobj.field_sequence().unwrap_or(&[]);
                    let result = record_field(efs, 0x0D);
                    node.evidence.push(WhyEvidence {
                        id: ev,
                        kind: str_field(efs, 0x02).unwrap_or("").to_string(),
                        subject: str_field(efs, 0x03).map(|s| s.to_string()),
                        outcome: result
                            .and_then(|r| str_field(r, 0x01))
                            .map(|s| s.to_string()),
                    });
                }
            }
            // Residuals relevant to the claim: those explicitly linked to it
            // (affected_claims), then the introducing change's own residuals.
            let mut res_seen: HashSet<Gid> =
                residuals_affecting_claim(repo, &c).into_iter().collect();
            for res in first.residuals.iter().copied() {
                let _ = res_seen.insert(res);
            }
            let mut residual_ids: Vec<Gid> = res_seen.into_iter().collect();
            residual_ids.sort_by_key(|a| a.to_string());
            for res in residual_ids {
                let latest = chain_latest(repo, &res)?;
                if let Ok(robj) = repo.load(&latest) {
                    let rfs = robj.field_sequence().unwrap_or(&[]);
                    node.residuals.push(WhyResidual {
                        id: latest,
                        summary: str_field(rfs, 0x02).unwrap_or("").to_string(),
                        severity: str_field(rfs, 0x04).map(|s| s.to_string()),
                        disposition: residual_disposition(repo, &latest)?,
                    });
                }
            }
        }
    }
    report.introduced_by = Some(node);
    report.previous_approaches = attempts(repo, subject)?;
    Ok(report)
}

fn claim_subject_is(repo: &Repo, claim: &Gid, subject: &str) -> Result<bool, Error> {
    let obj = repo.load(claim)?;
    Ok(obj.field_sequence().and_then(|fs| str_field(fs, 0x01)) == Some(subject))
}

// ---------------------------------------------------------------------------
// checkpoint plan (AGENT_PROTOCOL.md §9.2)
// ---------------------------------------------------------------------------

/// The machine-generated checkpoint plan: every field of a checkpoint object
/// derived from structured repository state (never prose reconstruction).
#[derive(Debug, Clone)]
pub struct CheckpointPlan {
    pub summary: String,
    pub intent: Option<Gid>,
    pub trajectory: Option<(String, Gid)>,
    pub state: Option<Gid>,
    pub open_claims: Vec<Gid>,
    pub unresolved_residuals: Vec<Gid>,
    pub important_evidence: Vec<Gid>,
    pub recent_decisions: Vec<Gid>,
    pub relevant_attempts: Vec<Gid>,
    pub continuation_scope: Vec<String>,
}

/// Assembles the checkpoint plan from the current repository state.
pub fn checkpoint_plan(repo: &Repo) -> Result<CheckpointPlan, Error> {
    let trajectory = repo.read_ref(&format!("{REF_TRAJECTORIES}/current"))?;
    let mut plan = CheckpointPlan {
        summary: String::new(),
        intent: None,
        trajectory: None,
        state: repo.read_ref(REF_STATE_HEAD)?,
        open_claims: Vec::new(),
        unresolved_residuals: Vec::new(),
        important_evidence: Vec::new(),
        recent_decisions: Vec::new(),
        relevant_attempts: Vec::new(),
        continuation_scope: Vec::new(),
    };
    let mut intent: Option<Gid> = None;
    if let Some(tg) = trajectory {
        let name = crate::workflow::name_in_namespace(repo, REF_TRAJECTORIES, &tg)?;
        plan.trajectory = Some((name.unwrap_or_else(|| tg.to_string()), tg));
        let detail = trajectory_detail(repo, &tg.to_string())?;
        intent = detail.intent;
        plan.important_evidence = detail.evidence.into_iter().take(16).collect();
        // Open residuals from the trajectory's accumulated set.
        let mut open = Vec::new();
        for res in detail.residuals {
            let latest = chain_latest(repo, &res)?;
            if residual_disposition(repo, &latest)? == "open" {
                open.push(latest);
            }
        }
        open.sort_by_key(|a| a.to_string());
        plan.unresolved_residuals = open.into_iter().take(16).collect();
        // Relevant attempts: trajectories sharing the intent.
        if let Some(i) = intent {
            let mut sharing = Vec::new();
            for (_, latest) in all_trajectories(repo)? {
                if latest == tg {
                    continue;
                }
                if trajectory_intent(repo, &latest)? == Some(i) {
                    sharing.push(latest);
                }
            }
            sharing.sort_by_key(|a| a.to_string());
            plan.relevant_attempts = sharing.into_iter().take(8).collect();
        }
        // Continuation scope: unresolved residual classes + unverified claims.
        for res in &plan.unresolved_residuals {
            if let Ok(robj) = repo.load(res) {
                let rfs = robj.field_sequence().unwrap_or(&[]);
                let class = str_field(rfs, 0x03).unwrap_or("residual").to_string();
                let summ = str_field(rfs, 0x02).unwrap_or("").to_string();
                plan.continuation_scope
                    .push(format!("resolve {class}: {summ}"));
            }
        }
    }
    if intent.is_none() {
        if let Some(head) = repo.read_ref(REF_HEAD)? {
            if let Ok(hobj) = repo.load(&head) {
                let hfs = hobj.field_sequence().unwrap_or(&[]);
                intent = gid_field(hfs, 0x02);
            }
        }
    }
    plan.intent = intent;
    if let Some(i) = intent {
        if let Ok(iobj) = repo.load(&i) {
            let ifs = iobj.field_sequence().unwrap_or(&[]);
            let summary = str_field(ifs, 0x01).unwrap_or("").to_string();
            plan.summary = format!("continue: {summary}");
        }
    }
    if plan.summary.is_empty() {
        plan.summary = "continue current work".to_string();
    }
    // Open claims: head-chain claims not fully supported.
    let mut open_claims = Vec::new();
    let mut current = repo.read_ref(REF_HEAD)?;
    let mut guard = 0usize;
    while let Some(gid) = current {
        guard += 1;
        if guard > 64 {
            break;
        }
        let obj = match repo.load(&gid) {
            Ok(o) => o,
            Err(_) => break,
        };
        let fields = obj.field_sequence().unwrap_or(&[]);
        for claim in gid_list(fields, 0x0C) {
            let (status, _, _) = claim_status(repo, &claim)?;
            if !matches!(status, ClaimStatus::Supported | ClaimStatus::Superseded) {
                open_claims.push(claim);
            }
        }
        current = gid_list(fields, 0x11).first().copied();
    }
    open_claims.sort_by_key(|a| a.to_string());
    open_claims.dedup();
    plan.open_claims = open_claims.into_iter().take(16).collect();
    for claim in &plan.open_claims {
        if let Ok(cobj) = repo.load(claim) {
            let cfs = cobj.field_sequence().unwrap_or(&[]);
            let predicate = str_field(cfs, 0x03).unwrap_or("").to_string();
            if !predicate.is_empty() {
                plan.continuation_scope.push(format!("verify: {predicate}"));
            }
        }
    }
    // Recent decisions: head causal chain.
    let mut decisions = Vec::new();
    let mut current = repo.read_ref(REF_HEAD)?;
    let mut guard = 0usize;
    while let Some(gid) = current {
        guard += 1;
        if guard > 8 {
            break;
        }
        decisions.push(gid);
        let obj = match repo.load(&gid) {
            Ok(o) => o,
            Err(_) => break,
        };
        current = obj
            .field_sequence()
            .and_then(|fs| gid_list(fs, 0x11).first().copied());
    }
    plan.recent_decisions = decisions;
    plan.continuation_scope.sort();
    plan.continuation_scope.dedup();
    plan.continuation_scope.truncate(16);
    Ok(plan)
}

// ---------------------------------------------------------------------------
// context bundles (AGENT_PROTOCOL.md §6)
// ---------------------------------------------------------------------------

/// What to include in a context bundle.
#[derive(Debug, Clone, Copy, Default)]
pub struct IncludeFlags {
    pub claims: bool,
    pub residuals: bool,
    pub attempts: bool,
    pub evidence: bool,
}

impl IncludeFlags {
    /// Parses a comma-separated `--include` list; empty means all.
    pub fn parse(spec: &str) -> Result<IncludeFlags, Error> {
        if spec.trim().is_empty() {
            return Ok(IncludeFlags {
                claims: true,
                residuals: true,
                attempts: true,
                evidence: true,
            });
        }
        let mut f = IncludeFlags::default();
        for part in spec.split(',') {
            match part.trim() {
                "claims" => f.claims = true,
                "residuals" => f.residuals = true,
                "attempts" => f.attempts = true,
                "evidence" => f.evidence = true,
                other => {
                    return Err(Error::Invalid(format!(
                        "unknown include category {other:?} (claims|residuals|attempts|evidence)"
                    )))
                }
            }
        }
        Ok(f)
    }
}

/// One object in a context bundle.
#[derive(Debug, Clone)]
pub struct BundleItem {
    pub id: Gid,
    pub family: Family,
    pub level: u8,
    pub summary: String,
}

/// The context bundle (AGENT_PROTOCOL.md §6.4).
#[derive(Debug, Clone)]
pub struct ContextBundle {
    pub subject: String,
    pub intent: Option<Gid>,
    pub budget_tokens: usize,
    pub consumed: usize,
    pub items: Vec<BundleItem>,
    pub deduplicated: usize,
    pub expanded: ExpandedCounts,
    pub next_expand: Vec<String>,
    pub omitted: Vec<String>,
}

/// Counts of expanded categories in a bundle.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExpandedCounts {
    pub claims: usize,
    pub residuals: usize,
    pub attempts: usize,
    pub evidence: usize,
}

/// Deterministic token estimate for an item at a level: the textual gid plus
/// the summary, at roughly 4 bytes per token, plus a fixed envelope.
fn item_tokens(item: &BundleItem) -> usize {
    let base = item.id.to_string().len() + item.summary.len() + 16;
    (base / 4).max(8)
}

/// Builds the smallest sufficient context bundle for a subject
/// (AGENT_PROTOCOL.md §6). Phases: changes+claims → residuals (open first) →
/// attempts → evidence. Deterministic; bounded by `budget_tokens`.
pub fn context_bundle(
    repo: &Repo,
    subject: &str,
    for_intent: Option<&str>,
    budget_tokens: usize,
    include: IncludeFlags,
) -> Result<ContextBundle, Error> {
    let mut bundle = ContextBundle {
        subject: subject.to_string(),
        intent: for_intent.map(|s| repo.resolve(s)).transpose()?,
        budget_tokens: budget_tokens.max(64),
        consumed: 0,
        items: Vec::new(),
        deduplicated: 0,
        expanded: ExpandedCounts::default(),
        next_expand: Vec::new(),
        omitted: Vec::new(),
    };
    let budget = bundle.budget_tokens;
    let mut seen: HashSet<Gid> = HashSet::new();
    let push = |bundle: &mut ContextBundle, item: BundleItem, seen: &mut HashSet<Gid>| -> bool {
        if !seen.insert(item.id) {
            bundle.deduplicated += 1;
            return true;
        }
        let cost = item_tokens(&item);
        if bundle.consumed + cost > budget {
            bundle
                .next_expand
                .push(format!("{}:{}", item.family.short(), item.id));
            bundle
                .omitted
                .push(format!("{}:{}", item.family.short(), item.id));
            return false;
        }
        bundle.consumed += cost;
        bundle.items.push(item);
        true
    };

    // Phase 1: changes touching the subject (L1) + claims (L2).
    let hits = changes_touching(repo, subject)?;
    for hit in &hits {
        let family = Family::Change;
        if !push(
            &mut bundle,
            BundleItem {
                id: hit.change,
                family,
                level: 1,
                summary: hit.summary.clone(),
            },
            &mut seen,
        ) {
            return Ok(bundle);
        }
    }
    if include.claims {
        let all = claims(
            repo,
            &ClaimsFilter {
                subject: Some(subject.to_string()),
                ..Default::default()
            },
        )?
        .0;
        for row in &all {
            if !push(
                &mut bundle,
                BundleItem {
                    id: row.gid,
                    family: Family::Claim,
                    level: 2,
                    summary: row.predicate.clone(),
                },
                &mut seen,
            ) {
                return Ok(bundle);
            }
            bundle.expanded.claims += 1;
        }
    }

    // Phase 2: residuals affecting the subject, open first.
    if include.residuals {
        let all = residuals(
            repo,
            &ResidualsFilter {
                subject: Some(subject.to_string()),
                ..Default::default()
            },
        )?
        .0;
        let mut sorted = all;
        sorted.sort_by(|a, b| {
            let oa = (a.disposition == "open") as u8;
            let ob = (b.disposition == "open") as u8;
            ob.cmp(&oa)
                .then_with(|| b.created_at.unwrap_or(0).cmp(&a.created_at.unwrap_or(0)))
                .then_with(|| a.gid.to_string().cmp(&b.gid.to_string()))
        });
        for row in &sorted {
            if !push(
                &mut bundle,
                BundleItem {
                    id: row.gid,
                    family: Family::Residual,
                    level: 2,
                    summary: format!(
                        "{} [{}] {}",
                        row.disposition,
                        row.severity.as_deref().unwrap_or_default(),
                        row.summary
                    ),
                },
                &mut seen,
            ) {
                return Ok(bundle);
            }
            bundle.expanded.residuals += 1;
        }
    }

    // Phase 3: previous attempts.
    if include.attempts {
        for attempt in attempts(repo, subject)? {
            let outcome = attempt
                .outcome
                .clone()
                .unwrap_or_else(|| "incomplete".into());
            if !push(
                &mut bundle,
                BundleItem {
                    id: attempt.trajectory,
                    family: Family::Trajectory,
                    level: 1,
                    summary: format!(
                        "{}: {}",
                        outcome,
                        attempt.termination_reason.clone().unwrap_or_default()
                    ),
                },
                &mut seen,
            ) {
                return Ok(bundle);
            }
            bundle.expanded.attempts += 1;
        }
    }

    // Phase 4: evidence for the included claims.
    if include.evidence {
        let mut ev_seen = HashSet::new();
        let claim_rows = claims(
            repo,
            &ClaimsFilter {
                subject: Some(subject.to_string()),
                ..Default::default()
            },
        )?
        .0;
        for row in &claim_rows {
            if let Ok(cobj) = repo.load(&row.gid) {
                let cfs = cobj.field_sequence().unwrap_or(&[]);
                for ev in gid_list(cfs, 0x08) {
                    if !ev_seen.insert(ev) {
                        continue;
                    }
                    let eobj = match repo.load(&ev) {
                        Ok(o) => o,
                        Err(_) => continue,
                    };
                    let efs = eobj.field_sequence().unwrap_or(&[]);
                    let result = record_field(efs, 0x0D);
                    let outcome = result
                        .and_then(|r| str_field(r, 0x01))
                        .unwrap_or("")
                        .to_string();
                    if !push(
                        &mut bundle,
                        BundleItem {
                            id: ev,
                            family: Family::Evidence,
                            level: 1,
                            summary: format!("{}: {}", str_field(efs, 0x02).unwrap_or(""), outcome),
                        },
                        &mut seen,
                    ) {
                        return Ok(bundle);
                    }
                    bundle.expanded.evidence += 1;
                }
            }
        }
    }

    // Phase 5: context manifests of relevant agent runs (Phase 2: none are
    // created yet; the expansion point is reserved).
    bundle.next_expand.sort();
    bundle.next_expand.dedup();
    bundle.omitted.sort();
    bundle.omitted.dedup();
    Ok(bundle)
}
