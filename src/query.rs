//! Query layer: log, show, status, and derived statuses
//! (OBJECT_MODEL.md §8, AGENT_PROTOCOL.md §5).
//!
//! Derived statuses (claim status, residual disposition, persistence,
//! readiness) are computed from the canonical graph, never stored.

use crate::family::Family;
use crate::gid::Gid;
use crate::store::{Error, Repo, REF_HEAD, REF_STATE_HEAD, REF_TRAJECTORIES};
use crate::value::{Field, Object, Value};

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
        let trajectory = current_trajectory_name(repo)?;
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

/// Whether some claim supersedes `claim` (via the derived index).
fn is_superseded(repo: &Repo, claim: &Gid) -> Result<bool, Error> {
    if let Ok(conn) = crate::store::index::open_for_query(repo) {
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
        return Ok(false);
    }
    Ok(false)
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

fn int_field(fields: &[Field], tag: u8) -> Option<i64> {
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
