//! The lightweight agent protocol (Phase 7; brief §15.4, AGENT_PROTOCOL.md
//! §10).
//!
//! A bounded, line-delimited JSON session over stdin/stdout so agents query
//! Gemel without scraping terminal prose. The protocol is a **session
//! framing around the existing query layer** — there is no parallel
//! ontology: every request routes to the same library queries the CLI uses.
//!
//! Request (one JSON object per line):
//!
//! ```json
//! {"id": 1, "query": "why", "params": {"subject": "decode_name"}}
//! ```
//!
//! Response (one JSON object per line):
//!
//! ```json
//! {"id": 1, "schema": "gemel.agent.v1", "result": {...},
//!  "omitted": [], "uncertainty": []}
//! ```
//!
//! Errors carry a stable code, never prose the agent must parse:
//!
//! ```json
//! {"id": 1, "schema": "gemel.agent.v1",
//!  "error": {"code": "invalid_request", "message": "..."}}
//! ```
//!
//! Bounds: a request line is at most 64 KiB; a response is at most 4 MiB.
//! The session is stateless per request (the `id` field correlates
//! responses); EOF ends the session. Nothing is executed during a session.

use crate::gid::Gid;
use crate::store::{Error, Repo};
use serde_json::{json, Value};

/// The protocol envelope schema.
pub const AGENT_SCHEMA: &str = "gemel.agent.v1";
/// Maximum request line length.
pub const MAX_REQUEST_LINE: usize = 64 * 1024;
/// Maximum response size.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// A parsed agent request.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub id: u64,
    pub query: String,
    pub params: serde_json::Map<String, Value>,
}

/// Parses one request line. The `id` is required; unknown fields are
/// rejected (fail closed), never ignored.
pub fn parse_request(line: &str) -> Result<AgentRequest, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "request must be a JSON object".to_string())?;
    let id = obj
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "request requires a numeric id".to_string())?;
    let query = obj
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "request requires a query string".to_string())?
        .to_string();
    let params = match obj.get("params") {
        None => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err("params must be an object".to_string()),
    };
    Ok(AgentRequest { id, query, params })
}

fn param_str(params: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn param_u64(params: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64())
}

/// Routes one request to the query layer. Returns the `result` payload.
pub fn route(repo: &Repo, req: &AgentRequest) -> Result<Value, String> {
    match req.query.as_str() {
        "status" => route_status(repo),
        "next" => route_next(repo),
        "why" => {
            let subject = param_str(&req.params, "subject")
                .ok_or_else(|| "why requires params.subject".to_string())?;
            route_why(repo, &subject)
        }
        "semantic" => {
            let subject = param_str(&req.params, "subject");
            route_semantic(repo, subject.as_deref())
        }
        "claims" => route_claims(repo, &req.params),
        "evidence" => route_evidence(repo, &req.params),
        "residuals" => route_residuals(repo, &req.params),
        "attempts" => {
            let subject = param_str(&req.params, "subject")
                .ok_or_else(|| "attempts requires params.subject".to_string())?;
            route_attempts(repo, &subject)
        }
        "context" => route_context(repo, &req.params),
        "log" => route_log(repo, &req.params),
        "index" => route_index(repo, &req.params),
        other => Err(format!("unknown query {other:?}")),
    }
}

/// Runs the session: reads request lines from stdin, writes response lines
/// to stdout. EOF or a zero-length request ends the session.
pub fn run_session(repo: &Repo) -> Result<(), Error> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_REQUEST_LINE {
            let resp = error_response(0, "limit_exceeded", "request line too long");
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
            continue;
        }
        let req = match parse_request(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = error_response(0, "invalid_request", &e);
                writeln!(stdout, "{resp}")?;
                stdout.flush()?;
                continue;
            }
        };
        let resp = match route(repo, &req) {
            Ok(result) => {
                let mut envelope = serde_json::Map::new();
                envelope.insert("id".into(), json!(req.id));
                envelope.insert("schema".into(), json!(AGENT_SCHEMA));
                envelope.insert("result".into(), result);
                envelope.insert("omitted".into(), json!([]));
                envelope.insert("uncertainty".into(), json!([]));
                serde_json::to_string(&Value::Object(envelope)).unwrap_or_else(|_| {
                    error_response(req.id, "internal", "response serialization failed")
                })
            }
            Err(e) => error_response(req.id, "query_failed", &e),
        };
        if resp.len() > MAX_RESPONSE_BYTES {
            let resp = error_response(req.id, "limit_exceeded", "response too large");
            writeln!(stdout, "{resp}")?;
        } else {
            writeln!(stdout, "{resp}")?;
        }
        stdout.flush()?;
    }
    Ok(())
}

fn error_response(id: u64, code: &str, message: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "schema": AGENT_SCHEMA,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".to_string())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn route_status(repo: &Repo) -> Result<Value, String> {
    // Automatic exchange ingestion mirrors `gemel status` (idempotent).
    let _ = crate::exchange::ingest::ingest(repo).map_err(|e| e.to_string());
    let st = crate::query::status(repo).map_err(|e| e.to_string())?;
    let exchange = exchange_block(repo);
    Ok(json!({
        "trajectory": st.trajectory,
        "intent": st.intent.map(|g| g.to_string()),
        "state": st.state.map(|g| g.to_string()),
        "changed": st.changed.iter().map(|(p, s)| json!({
            "path": p, "status": format!("{:?}", s),
        })).collect::<Vec<_>>(),
        "claims": st.claims.iter().map(|c| json!({
            "id": c.gid.to_string(), "predicate": c.predicate, "status": c.status.as_str(),
        })).collect::<Vec<_>>(),
        "residuals": st.residuals.iter().map(|r| json!({
            "id": r.gid.to_string(), "summary": r.summary,
            "severity": r.severity, "disposition": r.disposition,
        })).collect::<Vec<_>>(),
        "semantic": st.semantic_entities.map(|n| json!({ "entities": n })),
        "readiness": st.readiness.as_str(),
        "exchange": exchange,
    }))
}

fn exchange_block(repo: &Repo) -> Value {
    let frontiers = crate::exchange::discover_frontiers(repo.meta_dir()).unwrap_or_default();
    if frontiers.is_empty() {
        return json!({ "detected": false });
    }
    let active = crate::exchange::export::read_active_frontier(repo.meta_dir())
        .ok()
        .flatten();
    let source_match = crate::exchange::export::working_tree_files(repo)
        .ok()
        .and_then(|files| crate::exchange::export::content_state_identity(repo, &files).ok())
        .map(|content_id| {
            frontiers
                .iter()
                .any(|(f, _, _)| f.source_state == content_id)
        })
        .unwrap_or(false);
    json!({
        "detected": true,
        "frontier": active.map(|a| crate::hex::encode(&a)),
        "source_match": source_match,
        "coverage": { "canonical_metadata": "complete", "deep_evidence": "partial" },
    })
}

fn route_next(repo: &Repo) -> Result<Value, String> {
    let plan = crate::query::next_plan(repo).map_err(|e| e.to_string())?;
    Ok(json!({
        "intent": plan.intent.map(|g| g.to_string()),
        "trajectory": plan.trajectory.as_ref().map(|(n, g)| json!({ "name": n, "id": g.to_string() })),
        "state": plan.state.map(|g| g.to_string()),
        "recommendations": plan.recommendations.iter().map(|r| json!({
            "kind": r.kind,
            "subject": r.subject,
            "refs": r.refs.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "rationale": r.rationale,
            "certainty": r.certainty,
        })).collect::<Vec<_>>(),
        "uncertainty": plan.uncertainty,
    }))
}

fn route_why(repo: &Repo, subject: &str) -> Result<Value, String> {
    let report = crate::query::why(repo, subject).map_err(|e| e.to_string())?;
    let introduced = report.introduced_by.as_ref().map(|n| {
        json!({
            "change": { "id": n.change.to_string(), "name": n.change_name, "summary": n.summary },
            "intent": n.intent.map(|g| json!({
                "id": g.to_string(), "summary": n.intent_summary,
            })),
            "claim": n.claim.as_ref().map(|c| json!({
                "id": c.id.to_string(), "predicate": c.predicate, "status": c.status.as_str(),
            })),
            "evidence": n.evidence.iter().map(|e| json!({
                "id": e.id.to_string(), "kind": e.kind, "subject": e.subject, "outcome": e.outcome,
            })).collect::<Vec<_>>(),
            "residuals": n.residuals.iter().map(|r| json!({
                "id": r.id.to_string(), "summary": r.summary,
                "severity": r.severity, "disposition": r.disposition,
            })).collect::<Vec<_>>(),
        })
    });
    Ok(json!({
        "subject": report.subject,
        "semantic": report.semantic.as_ref().map(entity_json),
        "introduced_by": introduced,
        "last_modified": report.last_modified.map(|g| g.to_string()),
        "previous_approaches": report.previous_approaches.iter().map(attempt_json).collect::<Vec<_>>(),
        "uncertainty": report.uncertainty,
    }))
}

fn entity_json(e: &crate::semantic::EntityInfo) -> Value {
    json!({
        "id": e.id.map(|g| g.to_string()),
        "kind": e.kind,
        "name": e.name,
        "module_path": e.module_path,
        "full_path": e.full_path(),
        "file_path": e.file_path,
        "start_line": e.start_line,
        "end_line": e.end_line,
        "signature": e.signature,
        "visibility": e.visibility,
        "lineage": e.lineage.as_ref().map(|(from, evidence, certainty)| json!({
            "from": from.to_string(), "evidence": evidence, "certainty": certainty,
        })),
        "state": e.state.to_string(),
    })
}

fn attempt_json(a: &crate::query::AttemptSummary) -> Value {
    json!({
        "trajectory": a.trajectory.to_string(),
        "name": a.name,
        "intent": a.intent.map(|g| g.to_string()),
        "outcome": a.outcome,
        "termination_reason": a.termination_reason,
        "evidence": a.evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "residuals": a.residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "handoff": a.handoff_summary,
        "touched_subject": a.touched_subject,
    })
}

fn route_semantic(repo: &Repo, subject: Option<&str>) -> Result<Value, String> {
    match subject {
        Some(subject) => {
            let resolved =
                crate::semantic::resolve_subject(repo, subject).map_err(|e| e.to_string())?;
            match resolved.entity {
                Some(e) => Ok(json!({ "entity": entity_json(&e), "aliases": resolved.aliases })),
                None => Err("no semantic entity matches the subject".to_string()),
            }
        }
        None => {
            let entities = crate::semantic::current_entities(repo)
                .map_err(|e| e.to_string())?
                .unwrap_or_default();
            let list: Vec<Value> = entities
                .iter()
                .filter_map(|(gid, _)| crate::semantic::entity_info(repo, gid).ok())
                .take(100)
                .map(|e| entity_json(&e))
                .collect();
            Ok(json!({ "entities": list }))
        }
    }
}

fn route_claims(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let status = match param_str(params, "status") {
        None => None,
        Some(s) => Some(match s.as_str() {
            "supported" => crate::query::ClaimStatus::Supported,
            "contradicted" => crate::query::ClaimStatus::Contradicted,
            "partially_supported" => crate::query::ClaimStatus::PartiallySupported,
            "unverified" => crate::query::ClaimStatus::Unverified,
            "stale" => crate::query::ClaimStatus::Stale,
            "superseded" => crate::query::ClaimStatus::Superseded,
            other => return Err(format!("unknown claim status {other:?}")),
        }),
    };
    let limit = param_u64(params, "limit").unwrap_or(100).min(1000) as usize;
    let (rows, _) = crate::query::claims(
        repo,
        &crate::query::ClaimsFilter {
            subject: param_str(params, "subject"),
            status,
            limit,
            cursor: None,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "claims": rows.iter().map(|r| json!({
            "id": r.gid.to_string(),
            "predicate": r.predicate,
            "predicate_kind": r.predicate_kind,
            "subject": r.subject,
            "status": r.status.as_str(),
            "supporting": r.supporting.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "contradicting": r.contradicting.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "change": r.change.map(|g| g.to_string()),
            "trajectory": r.trajectory,
        })).collect::<Vec<_>>(),
    }))
}

fn route_evidence(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    match (param_str(params, "id"), param_str(params, "subject")) {
        (Some(id), None) => {
            let row = crate::query::evidence_show(repo, &id).map_err(|e| e.to_string())?;
            Ok(json!({ "evidence": evidence_json(&row) }))
        }
        (None, Some(subject)) => {
            let rows =
                crate::query::evidence_for_subject(repo, &subject).map_err(|e| e.to_string())?;
            Ok(json!({
                "subject": subject,
                "evidence": rows.iter().map(evidence_json).collect::<Vec<_>>(),
            }))
        }
        _ => Err("provide params.id or params.subject".to_string()),
    }
}

fn evidence_json(row: &crate::query::EvidenceRow) -> Value {
    json!({
        "id": row.gid.to_string(),
        "kind": row.kind,
        "subject": row.subject,
        "outcome": row.outcome,
        "evaluated_state": row.evaluated_state.map(|g| g.to_string()),
        "freshness": row.freshness.as_str(),
        "producer": row.producer.map(|g| g.to_string()),
    })
}

fn route_residuals(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let limit = param_u64(params, "limit").unwrap_or(100).min(1000) as usize;
    let (rows, _) = crate::query::residuals(
        repo,
        &crate::query::ResidualsFilter {
            subject: param_str(params, "subject"),
            disposition: param_str(params, "disposition"),
            limit,
            cursor: None,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "residuals": rows.iter().map(|r| json!({
            "id": r.gid.to_string(),
            "summary": r.summary,
            "classification": r.classification,
            "severity": r.severity,
            "disposition": r.disposition,
            "persistence": r.persistence,
            "origin_evidence": r.origin_evidence.map(|g| g.to_string()),
            "affected_claims": r.affected_claims.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "affected_changes": r.affected_changes.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

fn route_attempts(repo: &Repo, subject: &str) -> Result<Value, String> {
    let rows = crate::query::attempts(repo, subject).map_err(|e| e.to_string())?;
    Ok(json!({
        "subject": subject,
        "attempts": rows.iter().map(attempt_json).collect::<Vec<_>>(),
    }))
}

fn route_context(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let subject = param_str(params, "subject").ok_or("context requires params.subject")?;
    let budget = param_u64(params, "budget").unwrap_or(4096).min(1_000_000) as usize;
    let flags =
        crate::query::IncludeFlags::parse(param_str(params, "include").as_deref().unwrap_or(""))
            .map_err(|e| e.to_string())?;
    let bundle = crate::query::context_bundle(
        repo,
        &subject,
        param_str(params, "for_intent").as_deref(),
        budget,
        flags,
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({
        "subject": subject,
        "intent": bundle.intent.map(|g| g.to_string()),
        "budget": {
            "tokens": bundle.budget_tokens,
            "consumed": bundle.consumed,
            "remaining": bundle.budget_tokens.saturating_sub(bundle.consumed),
        },
        "bundle": {
            "objects": bundle.items.iter().map(|i| json!({
                "id": i.id.to_string(),
                "family": i.family.short(),
                "level": i.level,
                "summary": i.summary,
            })).collect::<Vec<_>>(),
            "deduplicated": bundle.deduplicated,
        },
        "omitted": bundle.omitted,
        "next": { "expand": bundle.next_expand },
    }))
}

fn route_log(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let limit = param_u64(params, "limit").unwrap_or(100).min(1000) as usize;
    let entries = crate::query::log(repo, limit).map_err(|e| e.to_string())?;
    Ok(json!({
        "changes": entries.iter().map(|e| json!({
            "id": e.change.to_string(),
            "name": e.name,
            "summary": e.summary,
            "input_state": e.input_state.map(|g| g.to_string()),
            "resulting_state": e.resulting_state.map(|g| g.to_string()),
            "trajectory": e.trajectory,
            "operations": e.operations.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

fn route_index(repo: &Repo, params: &serde_json::Map<String, Value>) -> Result<Value, String> {
    let gid: Gid = match param_str(params, "state") {
        Some(s) => crate::query::resolve_state(repo, &s).map_err(|e| e.to_string())?,
        None => crate::query::head_state(repo)
            .map_err(|e| e.to_string())?
            .ok_or("no head state to index")?,
    };
    let producer =
        crate::defaults::automation_producer_object_at(crate::semantic::INDEXER_PRODUCER_NAME, 0);
    let producer_gid =
        crate::content::object_identity(repo, &producer).map_err(|e| e.to_string())?;
    let out = crate::semantic::index_state(repo, &gid, &producer_gid).map_err(|e| e.to_string())?;
    Ok(json!({
        "state": gid.to_string(),
        "index": out.index.to_string(),
        "entities": out.entities,
        "files": out.files,
        "new": out.new_entities,
        "modified": out.modified_entities,
        "moved": out.moved_entities,
        "lineage_links": out.lineage_links,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InitOptions;
    use crate::workflow::{self, BeginOptions, FinishOptions};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("gemel-proto-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(root: &Path) -> Repo {
        std::fs::write(root.join("src"), "").ok();
        let repo = Repo::init(root, &InitOptions::default()).unwrap();
        workflow::begin_change(
            &repo,
            &BeginOptions {
                intent_summary: Some("demo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        workflow::finish_change(
            &repo,
            &FinishOptions {
                summary: "first change".into(),
                ..Default::default()
            },
        )
        .unwrap();
        repo
    }

    #[test]
    fn request_parsing_is_strict() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request("[]").is_err());
        assert!(parse_request(r#"{"query":"status"}"#).is_err()); // no id
        assert!(parse_request(r#"{"id":1,"params":[]}"#).is_err()); // bad params
        let ok = parse_request(r#"{"id":7,"query":"why","params":{"subject":"x"}}"#).unwrap();
        assert_eq!(ok.id, 7);
        assert_eq!(ok.query, "why");
        assert_eq!(ok.params["subject"], "x");
    }

    #[test]
    fn routes_return_structured_results() {
        let root = temp_root("routes");
        let repo = seed(&root);
        let req = AgentRequest {
            id: 1,
            query: "status".into(),
            params: serde_json::Map::new(),
        };
        let out = route(&repo, &req).unwrap();
        assert_eq!(out["trajectory"], "T1");
        assert_eq!(out["readiness"], "READY");
        // next derives recommendations without any model.
        let req = AgentRequest {
            id: 2,
            query: "next".into(),
            params: serde_json::Map::new(),
        };
        let out = route(&repo, &req).unwrap();
        let recs = out["recommendations"].as_array().unwrap();
        assert!(!recs.is_empty());
        // log works.
        let req = AgentRequest {
            id: 3,
            query: "log".into(),
            params: serde_json::Map::new(),
        };
        let out = route(&repo, &req).unwrap();
        assert_eq!(out["changes"].as_array().unwrap().len(), 1);
        // unknown query fails with a stable error.
        let req = AgentRequest {
            id: 4,
            query: "frobnicate".into(),
            params: serde_json::Map::new(),
        };
        assert!(route(&repo, &req).is_err());
    }

    #[test]
    fn response_envelope_is_deterministic() {
        let root = temp_root("envelope");
        seed(&root);
        let err = error_response(9, "test", "boom");
        let v: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(v["id"], 9);
        assert_eq!(v["schema"], AGENT_SCHEMA);
        assert_eq!(v["error"]["code"], "test");
    }
}
