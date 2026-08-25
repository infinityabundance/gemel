//! Phase 7 integration tests (SPECIFICATION.md Phase 7; AGENT_PROTOCOL.md
//! §10; brief §15.4, §57, §11).
//!
//! The lightweight agent protocol (bounded, line-delimited JSON sessions
//! over the existing query layer), the `next` recommendation plan (derived
//! purely from durable engineering state — never fake intelligence), and
//! repository policy (the required-verification matrix driving readiness).

#![allow(clippy::result_large_err)]

use gemel::store::{InitOptions, Repo};
use gemel::workflow::{self, BeginOptions, ClaimSpec, EvidenceSpec, FinishOptions, ResidualSpec};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p7-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn seed(root: &Path) -> Repo {
    write_file(root, "src/lib.rs", "pub fn f() {}\n");
    Repo::init(root, &InitOptions::default()).unwrap()
}

fn make_change(repo: &Repo, root: &Path, summary: &str) {
    workflow::begin_change(
        repo,
        &BeginOptions {
            intent_summary: Some(summary.into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(root, "src/lib.rs", "pub fn f() { /* changed */ }\n");
    workflow::finish_change(
        repo,
        &FinishOptions {
            summary: summary.into(),
            ..Default::default()
        },
    )
    .unwrap();
}

/// Runs the protocol binary with the given request lines; returns the parsed
/// response lines in order.
fn run_protocol(root: &Path, requests: &str) -> Vec<serde_json::Value> {
    let bin = env!("CARGO_BIN_EXE_gemel");
    let mut child = Command::new(bin)
        .arg("--repo")
        .arg(root)
        .arg("protocol")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn gemel protocol");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(requests.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("response line is JSON"))
        .collect()
}

// ---------------------------------------------------------------------------
// Courts
// ---------------------------------------------------------------------------

#[test]
fn protocol_session_routes_queries_and_errors() {
    let root = temp_root("session");
    let repo = seed(&root);
    make_change(&repo, &root, "first");
    let responses = run_protocol(
        &root,
        "{\"id\":1,\"query\":\"status\"}\n\
         {\"id\":2,\"query\":\"log\"}\n\
         {\"id\":3,\"query\":\"next\"}\n\
         {\"id\":4,\"query\":\"frobnicate\"}\n\
         {\"id\":5,\"query\":\"why\",\"params\":{\"subject\":\"src/lib.rs\"}}\n",
    );
    assert_eq!(responses.len(), 5);
    for r in &responses {
        assert_eq!(r["schema"], "gemel.agent.v1");
    }
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["readiness"], "READY");
    assert_eq!(
        responses[1]["result"]["changes"].as_array().unwrap().len(),
        1
    );
    assert_eq!(responses[3]["error"]["code"], "query_failed");
    assert_eq!(responses[4]["result"]["subject"], "src/lib.rs");
    // All results are deterministic across sessions.
    let again = run_protocol(&root, "{\"id\":1,\"query\":\"status\"}\n");
    assert_eq!(responses[0]["result"], again[0]["result"]);
}

#[test]
fn protocol_rejects_malformed_and_oversized_requests() {
    let root = temp_root("bounds");
    let repo = seed(&root);
    make_change(&repo, &root, "first");
    let mut huge = "{\"id\":1,\"query\":\"x\",\"params\":{\"pad\":\"".to_string();
    huge.push_str(&"a".repeat(128 * 1024));
    huge.push_str("\"}}}\n");
    let responses = run_protocol(&root, "not json\n{\"query\":\"status\"}\n");
    assert_eq!(responses[0]["error"]["code"], "invalid_request");
    assert_eq!(responses[1]["error"]["code"], "invalid_request");
    // The oversized line is rejected with a limit code (the session continues).
    let bin = env!("CARGO_BIN_EXE_gemel");
    let mut child = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .arg("protocol")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(huge.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let line = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(v["error"]["code"], "limit_exceeded");
}

#[test]
fn next_derives_honest_recommendations() {
    let root = temp_root("next");
    let repo = seed(&root);
    // Fresh repository: begin a change.
    let plan = gemel::query::next_plan(&repo).unwrap();
    assert!(plan.recommendations.iter().any(|r| r.kind == "continue"));
    assert!(plan
        .recommendations
        .iter()
        .any(|r| r.certainty == "observed"));
    // After a change with an open residual: resolve + index.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("with residual".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "src/lib.rs", "pub fn f() { /* v2 */ }\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "change with residual".into(),
            residuals: vec![ResidualSpec {
                summary: "FreeBSD divergence".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let plan = gemel::query::next_plan(&repo).unwrap();
    assert!(
        plan.recommendations
            .iter()
            .any(|r| r.kind == "resolve" && r.rationale.contains("FreeBSD divergence")),
        "expected resolve recommendation: {:?}",
        plan.recommendations
    );
    // Unindexed head state: index recommendation.
    assert!(
        plan.recommendations.iter().any(|r| r.kind == "index"),
        "expected index recommendation: {:?}",
        plan.recommendations
    );
    // Deterministic: identical inputs, identical plan.
    let again = gemel::query::next_plan(&repo).unwrap();
    assert_eq!(
        plan.recommendations
            .iter()
            .map(|r| (r.kind.clone(), r.subject.clone(), r.rationale.clone()))
            .collect::<Vec<_>>(),
        again
            .recommendations
            .iter()
            .map(|r| (r.kind.clone(), r.subject.clone(), r.rationale.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn next_flags_blocked_claims_for_verification() {
    let root = temp_root("blocked");
    let repo = seed(&root);
    // A contradicted claim: one supporting and one contradicting evidence.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("claims".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "src/lib.rs", "pub fn f() { /* v3 */ }\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "change with claims".into(),
            evidence: vec![
                EvidenceSpec {
                    subject: Some("src/lib.rs".into()),
                    outcome: "pass".into(),
                    kind: "test_result".into(),
                },
                EvidenceSpec {
                    subject: Some("src/lib.rs".into()),
                    outcome: "fail".into(),
                    kind: "test_result".into(),
                },
            ],
            claims: vec![
                ClaimSpec {
                    subject: Some("src/lib.rs".into()),
                    predicate: "parser is stable".into(),
                    kind: "correctness".into(),
                    evidence: vec![0],
                },
                ClaimSpec {
                    subject: Some("src/lib.rs".into()),
                    predicate: "parser is stable".into(),
                    kind: "correctness".into(),
                    evidence: vec![1],
                },
            ],
            ..Default::default()
        },
    )
    .unwrap();
    let plan = gemel::query::next_plan(&repo).unwrap();
    assert!(
        plan.recommendations.iter().any(|r| r.kind == "verify"),
        "blocked claims must surface a verify recommendation: {:?}",
        plan.recommendations
    );
}

#[test]
fn policy_matrix_drives_readiness() {
    let root = temp_root("policy");
    let repo = seed(&root);
    // No matrix configured: no gaps, READY after a change.
    make_change(&repo, &root, "first");
    let st = gemel::query::status(&repo).unwrap();
    assert!(gemel::query::required_verification(&repo)
        .unwrap()
        .is_empty());
    assert!(gemel::query::required_verification_gaps(&repo)
        .unwrap()
        .is_empty());
    assert_eq!(st.readiness.as_str(), "READY");
    // Configure a matrix requiring correctness on linux/x86_64 and
    // freebsd/x86_64 (no evidence has an environment → both are gaps).
    use gemel::value::{Field, Object, Value};
    let platform = |platform: &str, arch: &str| {
        Value::Record(vec![
            Field::new(0x01, Value::Str(platform.into())),
            Field::new(0x02, Value::Str(arch.into())),
        ])
    };
    let entry = Value::Record(vec![
        Field::new(0x01, Value::Str("correctness".into())),
        Field::new(
            0x02,
            Value::Array(vec![
                platform("linux", "x86_64"),
                platform("freebsd", "x86_64"),
            ]),
        ),
    ]);
    let matrix = Object::fields(
        gemel::family::Family::Config,
        vec![
            Field::new(0x04, Value::Str("never_auto_execute".into())), // mandatory execution_policy
            Field::new(
                0x08,
                Value::Record(vec![Field::new(0x01, Value::Array(vec![entry]))]),
            ),
        ],
    );
    let cfg_gid = repo.insert_object(&matrix).unwrap();
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction {
            ops: vec![gemel::store::refs::RefOp::set(
                gemel::store::REF_CONFIG,
                cfg_gid,
            )],
        })
        .unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    let matrix = gemel::query::required_verification(&repo).unwrap();
    assert_eq!(
        matrix,
        vec![
            (
                "correctness".to_string(),
                "freebsd".to_string(),
                "x86_64".to_string()
            ),
            (
                "correctness".to_string(),
                "linux".to_string(),
                "x86_64".to_string()
            ),
        ]
    );
    let gaps = gemel::query::required_verification_gaps(&repo).unwrap();
    assert_eq!(gaps.len(), 2);
    // Readiness is NOT_READY: required verification is missing.
    let st = gemel::query::status(&repo).unwrap();
    assert_eq!(st.readiness.as_str(), "NOT_READY");
}

#[test]
fn protocol_exposes_next_and_policy() {
    let root = temp_root("proto-next");
    let repo = seed(&root);
    make_change(&repo, &root, "first");
    let responses = run_protocol(
        &root,
        "{\"id\":1,\"query\":\"next\"}\n\
         {\"id\":2,\"query\":\"policy\"}\n",
    );
    // `policy` is exposed through the CLI; the protocol covers `next` plus
    // the query surface (status/log/why/semantic/claims/evidence/residuals/
    // attempts/context/index).
    assert!(responses[0]["result"]["recommendations"].is_array());
    let bin = env!("CARGO_BIN_EXE_gemel");
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["policy", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert!(v["result"]["required_verification"].is_array());
}
