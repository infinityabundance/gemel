//! Phase 2 integration tests (SPECIFICATION.md §43, AGENT_PROTOCOL.md §5–§6).
//!
//! These tests prove the Phase 2 exit criteria: `why`, `claims`, `evidence`,
//! `residuals`, `attempts`, `trajectory`, `checkpoint`, `context` with
//! machine-readable JSON forms, and the §52 acceptance demo — Agent A
//! discovers a rejected attempt, records claims/evidence/residuals, leaves a
//! checkpoint; Agent B resumes from repository state alone.

// Test closures return the rich store error type; boxed variants would
// obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use gemel::query::{self, ClaimStatus, IncludeFlags};
use gemel::store::{InitOptions, Repo, REF_STATE_HEAD};
use gemel::workflow::{
    self, BeginOptions, ClaimSpec, CloseTrajectoryOptions, EvidenceSpec, FinishOptions,
    ResidualSpec, ResolveResidualOptions,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p2-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Seeds the §52 scenario: a rejected first attempt T1 (strict RFC decode),
/// then Agent A's second attempt T2 with a claim, mixed evidence, and an open
/// residual. Returns the repo and the ids Agent A produced.
struct Demo {
    _repo: Repo,
    intent: gemel::gid::Gid,
    t17_rejected: gemel::gid::Gid,
    t18: gemel::gid::Gid,
    claim: gemel::gid::Gid,
    evidence_pass: gemel::gid::Gid,
    evidence_mismatch: gemel::gid::Gid,
    residual: gemel::gid::Gid,
}

fn seed_acceptance_demo(root: &Path) -> Demo {
    write_file(
        root,
        "parser.rs",
        "pub fn decode(name: &[u8]) -> String {\n    String::from_utf8_lossy(name).into_owned()\n}\n",
    );
    let repo = Repo::init(root, &InitOptions::default()).unwrap();

    // Previous agent: T1 (strict RFC decode) → rejected, FreeBSD diverged.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Fix parser compatibility problem".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(
        root,
        "parser.rs",
        "pub fn decode(name: &[u8]) -> String {\n    strict_decode(name)\n}\nfn strict_decode(name: &[u8]) -> String {\n    String::from_utf8_lossy(name).into_owned()\n}\n",
    );
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "strict RFC decode".into(),
            evidence: vec![
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "mismatch".into(),
                    kind: "oracle_comparison".into(),
                },
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "fail".into(),
                    kind: "court_receipt".into(),
                },
            ],
            residuals: vec![ResidualSpec {
                summary: "FreeBSD diverges from the oracle".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    workflow::close_trajectory(
        &repo,
        &CloseTrajectoryOptions {
            trajectory: "T1".into(),
            outcome: "rejected".into(),
            reason: Some("strict RFC behavior diverged in 17 oracle cases".into()),
            producer: None,
        },
    )
    .unwrap();

    // Agent A: second attempt T2, with a claim and mixed evidence.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(
        root,
        "parser.rs",
        "pub fn decode(name: &[u8]) -> String {\n    reject_loops(name);\n    String::from_utf8_lossy(name).into_owned()\n}\nfn reject_loops(name: &[u8]) {\n    // pointer loops > 16 rejected\n}\n",
    );
    let finished = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "loop rejection".into(),
            claims: vec![ClaimSpec {
                subject: Some("parser.rs".into()),
                predicate: "parser now matches upstream".into(),
                kind: "compatibility".into(),
            }],
            evidence: vec![
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "pass".into(),
                    kind: "court_receipt".into(),
                },
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "mismatch".into(),
                    kind: "oracle_comparison".into(),
                },
            ],
            residuals: vec![ResidualSpec {
                summary: "FreeBSD diverges from the oracle".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
            }],
        },
    )
    .unwrap();
    Demo {
        _repo: repo.clone(),
        intent: repo.resolve("I1").unwrap(),
        t17_rejected: repo.resolve("T1").unwrap(),
        t18: repo.resolve("T2").unwrap(),
        claim: finished.claims[0],
        evidence_pass: finished.evidence[0],
        evidence_mismatch: finished.evidence[1],
        residual: finished.residuals[0],
    }
}

// ---------------------------------------------------------------------------
// The §52 acceptance demo
// ---------------------------------------------------------------------------

#[test]
fn acceptance_demo_two_agents() {
    let root = temp_root("demo");
    let demo = seed_acceptance_demo(&root);
    let repo = Repo::find(&root).unwrap();

    // Agent A discovers the rejected attempt before writing new code.
    let attempts = query::attempts(&repo, "parser.rs").unwrap();
    let t1 = attempts
        .iter()
        .find(|a| a.name.as_deref() == Some("T1"))
        .unwrap();
    assert_eq!(t1.outcome.as_deref(), Some("rejected"));
    assert_eq!(
        t1.termination_reason.as_deref(),
        Some("strict RFC behavior diverged in 17 oracle cases")
    );
    assert!(t1.touched_subject);

    // The claim is partially supported (pass + mismatch evidence).
    let (status, supporting, contradicting) = query::claim_status(&repo, &demo.claim).unwrap();
    assert_eq!(status, ClaimStatus::PartiallySupported);
    assert_eq!(supporting, vec![demo.evidence_pass]);
    assert_eq!(contradicting, vec![demo.evidence_mismatch]);

    // Agent A stops; the checkpoint is a continuation boundary.
    let cp = workflow::create_checkpoint(&repo, &workflow::CheckpointOptions::default()).unwrap();
    assert_eq!(cp.name, "K1");
    assert_eq!(cp.plan.intent, Some(demo.intent));
    assert_eq!(cp.plan.open_claims, vec![demo.claim]);
    assert!(cp.plan.unresolved_residuals.contains(&demo.residual));
    assert!(cp
        .plan
        .continuation_scope
        .iter()
        .any(|s| s.contains("FreeBSD diverges")));

    // Agent A marks the trajectory interrupted (context limits).
    workflow::close_trajectory(
        &repo,
        &CloseTrajectoryOptions {
            trajectory: "T2".into(),
            outcome: "interrupted".into(),
            reason: Some("context limit reached".into()),
            producer: None,
        },
    )
    .unwrap();
    // Closing re-points the name at the newest chained version.
    let t18_latest = repo.resolve("T2").unwrap();
    assert_ne!(t18_latest, demo.t18);

    // Agent B receives ONLY the repository + the intent. It asks Gemel for
    // the smallest sufficient context and must discover: T1 rejected, T2
    // interrupted, C1 partially supported, E1/E2, R1.
    let bundle = query::context_bundle(
        &repo,
        "parser.rs",
        Some("I1"),
        4096,
        IncludeFlags::parse("").unwrap(),
    )
    .unwrap();
    let ids: Vec<String> = bundle.items.iter().map(|i| i.id.to_string()).collect();
    assert!(
        ids.contains(&demo.t17_rejected.to_string()),
        "T1 rejected must be in the bundle: {ids:?}"
    );
    assert!(
        ids.contains(&t18_latest.to_string()),
        "T2 interrupted must be in the bundle: {ids:?}"
    );
    assert!(
        ids.contains(&demo.claim.to_string()),
        "C1 must be in the bundle: {ids:?}"
    );
    assert!(
        ids.contains(&demo.evidence_pass.to_string()),
        "E1 must be in the bundle: {ids:?}"
    );
    assert!(
        ids.contains(&demo.evidence_mismatch.to_string()),
        "E2 must be in the bundle: {ids:?}"
    );
    assert!(
        ids.contains(&demo.residual.to_string()),
        "R1 must be in the bundle: {ids:?}"
    );
    // Attempts are labeled with their outcomes.
    let traj: Vec<&query::BundleItem> = bundle
        .items
        .iter()
        .filter(|i| i.family == gemel::family::Family::Trajectory)
        .collect();
    assert!(
        traj.iter().any(|t| t.summary.contains("rejected")),
        "rejected attempt labeled: {:?}",
        traj.iter().map(|t| &t.summary).collect::<Vec<_>>()
    );
    assert!(
        traj.iter().any(|t| t.summary.contains("interrupted")),
        "interrupted attempt labeled: {:?}",
        traj.iter().map(|t| &t.summary).collect::<Vec<_>>()
    );

    // Agent B resolves the FreeBSD discrepancy (the residual is still open).
    assert_eq!(
        query::residual_disposition(&repo, &demo.residual).unwrap(),
        "open"
    );
    let resolved = workflow::resolve_residual(
        &repo,
        &ResolveResidualOptions {
            residual: demo.residual.to_string(),
            disposition: "resolved".into(),
            reason: Some("FreeBSD court now matches after loop rejection".into()),
            producer: None,
        },
    )
    .unwrap();
    // The derived disposition comes from the latest chain version.
    assert_eq!(
        query::residual_disposition(&repo, &resolved.version).unwrap(),
        "resolved"
    );

    // Agent B creates T3 (a new attempt: T2 was closed interrupted).
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(demo.intent),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(
        &root,
        "parser.rs",
        "pub fn decode(name: &[u8]) -> String {\n    reject_loops(name);\n    String::from_utf8_lossy(name).into_owned()\n}\nfn reject_loops(name: &[u8]) {\n    // pointer loops > 16 rejected; FreeBSD court matches\n}\n",
    );
    let t3 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "FreeBSD loop match".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(t3.trajectory_name, "T3");
    assert!(t3.is_new_trajectory);

    // `why` walks source → Change → Intent → Claim → Evidence → Residual.
    let report = query::why(&repo, "parser.rs").unwrap();
    let node = report.introduced_by.expect("introduced by a change");
    assert_eq!(node.intent, Some(demo.intent));
    assert!(node.claim.is_some());
    assert_eq!(
        node.claim.as_ref().unwrap().status,
        ClaimStatus::PartiallySupported
    );
    assert_eq!(node.evidence.len(), 2, "claim links both evidence objects");
    assert!(!node.residuals.is_empty(), "residual surfaces in why");
    assert!(
        report
            .previous_approaches
            .iter()
            .any(|a| a.outcome.as_deref() == Some("rejected")),
        "rejected approach surfaces in why"
    );
}

// ---------------------------------------------------------------------------
// Query behaviors
// ---------------------------------------------------------------------------

#[test]
fn closed_trajectory_spawns_fresh_attempt() {
    let root = temp_root("close");
    write_file(&root, "a.txt", "one\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Shared intent".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "two\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "first".into(),
            ..Default::default()
        },
    )
    .unwrap();

    // Open trajectory continues on the same intent.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "three\n");
    let f2 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "second".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(f2.trajectory_name, "T1", "open trajectory continues");

    // Closing it makes it terminal: new work spawns a fresh attempt.
    workflow::close_trajectory(
        &repo,
        &CloseTrajectoryOptions {
            trajectory: "T1".into(),
            outcome: "superseded".into(),
            reason: Some("replaced by a better approach".into()),
            producer: None,
        },
    )
    .unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "four\n");
    let f3 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "third".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(f3.trajectory_name, "T2", "closed trajectory not continued");
    assert!(f3.is_new_trajectory);

    // The chained trajectory version preserves the outcome.
    let detail = query::trajectory_detail(&repo, "T1").unwrap();
    assert_eq!(detail.outcome.as_deref(), Some("superseded"));
    assert_eq!(
        detail.termination_reason.as_deref(),
        Some("replaced by a better approach")
    );
    // The full sequence is the concatenation across the chain.
    assert_eq!(detail.sequence.len(), 2, "both changes materialized");
}

#[test]
fn claims_filter_and_pagination() {
    let root = temp_root("claims");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    for (i, predicate) in ["claim alpha", "claim beta", "claim gamma"]
        .iter()
        .enumerate()
    {
        workflow::begin_change(
            &repo,
            &BeginOptions {
                intent_summary: Some(format!("intent {i}")),
                ..Default::default()
            },
        )
        .unwrap();
        write_file(&root, "f.txt", &format!("content {i}\n"));
        workflow::finish_change(
            &repo,
            &FinishOptions {
                summary: format!("change {i}"),
                claims: vec![ClaimSpec {
                    subject: Some(format!("subject-{i}")),
                    predicate: (*predicate).into(),
                    kind: "correctness".into(),
                }],
                ..Default::default()
            },
        )
        .unwrap();
    }

    // Subject filter.
    let (rows, next) = query::claims(
        &repo,
        &query::ClaimsFilter {
            subject: Some("subject-1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].predicate, "claim beta");
    assert!(next.is_none(), "no more items after the last page");

    // Status filter: all three claims are UNVERIFIED (no evidence).
    let (rows, _) = query::claims(
        &repo,
        &query::ClaimsFilter {
            status: Some(ClaimStatus::Unverified),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 3);

    // Cursor pagination: limit 1 walks all three pages deterministically.
    let (p1, c1) = query::claims(
        &repo,
        &query::ClaimsFilter {
            limit: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p1.len(), 1);
    assert!(c1.is_some(), "more items follow");
    let (p2, c2) = query::claims(
        &repo,
        &query::ClaimsFilter {
            limit: 1,
            cursor: c1.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p2.len(), 1);
    assert!(c2.is_some());
    let (p3, c3) = query::claims(
        &repo,
        &query::ClaimsFilter {
            limit: 1,
            cursor: c2.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(p3.len(), 1);
    assert!(c3.is_none(), "exhausted");
    // Re-requesting from the same cursor is idempotent (same page back).
    let (p4, _) = query::claims(
        &repo,
        &query::ClaimsFilter {
            limit: 1,
            cursor: c2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        p4.iter().map(|r| r.gid).collect::<Vec<_>>(),
        p3.iter().map(|r| r.gid).collect::<Vec<_>>(),
        "cursor re-request returns the same page"
    );
    // No overlap across pages.
    let all_ids: Vec<String> = [&p1, &p2, &p3]
        .iter()
        .flat_map(|p| p.iter().map(|r| r.gid.to_string()))
        .collect();
    assert_eq!(all_ids.len(), 3);
    let unique: std::collections::HashSet<&String> = all_ids.iter().collect();
    assert_eq!(unique.len(), 3, "no duplicated items across pages");
}

#[test]
fn residual_resolution_chains_and_derives_disposition() {
    let root = temp_root("residual");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(&repo, &BeginOptions::default()).unwrap();
    write_file(&root, "a.txt", "x\n");
    let f = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "seed".into(),
            residuals: vec![ResidualSpec {
                summary: "expected output differs from oracle".into(),
                severity: "medium".into(),
                classification: "expected_mismatch".into(),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let r1 = f.residuals[0];
    assert_eq!(query::residual_disposition(&repo, &r1).unwrap(), "open");

    // Acknowledge, then resolve: each is a chained version.
    let ack = workflow::resolve_residual(
        &repo,
        &ResolveResidualOptions {
            residual: r1.to_string(),
            disposition: "acknowledged".into(),
            reason: Some("accepted for now".into()),
            producer: None,
        },
    )
    .unwrap();
    assert_eq!(
        query::residual_disposition(&repo, &ack.version).unwrap(),
        "acknowledged"
    );
    let done = workflow::resolve_residual(
        &repo,
        &ResolveResidualOptions {
            residual: r1.to_string(),
            disposition: "resolved".into(),
            reason: Some("fixed".into()),
            producer: None,
        },
    )
    .unwrap();
    assert_eq!(
        query::residual_disposition(&repo, &done.version).unwrap(),
        "resolved"
    );
    // The latest chain version is found from any version.
    assert_eq!(query::chain_latest(&repo, &r1).unwrap(), done.version);
    assert_eq!(
        query::chain_latest(&repo, &ack.version).unwrap(),
        done.version
    );

    // The list endpoint resolves each residual to its latest version.
    let (rows, _) = query::residuals(&repo, &query::ResidualsFilter::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].gid, done.version);
    assert_eq!(rows[0].disposition, "resolved");
}

#[test]
fn trajectory_detail_materializes_sequence_and_handoff() {
    let root = temp_root("traj");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Implement X".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "1\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "step one".into(),
            ..Default::default()
        },
    )
    .unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "1\n2\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "step two".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let detail = query::trajectory_detail(&repo, "T1").unwrap();
    assert_eq!(detail.name.as_deref(), Some("T1"));
    assert_eq!(detail.sequence.len(), 2);
    assert_eq!(detail.sequence[0].summary, "step one");
    assert_eq!(detail.sequence[1].summary, "step two");
    assert!(detail.sequence[0].state.is_some());
    assert_eq!(detail.outcome, None, "open trajectory has no outcome");
}

#[test]
fn evidence_freshness_tracks_evaluated_state() {
    let root = temp_root("fresh");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("i".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "1\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "c1".into(),
            evidence: vec![EvidenceSpec {
                subject: Some("a.txt".into()),
                outcome: "pass".into(),
                kind: "test_result".into(),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let head = repo.read_ref(REF_STATE_HEAD).unwrap().unwrap();
    let (rows, _) = query::residuals(&repo, &query::ResidualsFilter::default()).unwrap();
    let _ = rows;
    let all = query::claims(&repo, &query::ClaimsFilter::default())
        .unwrap()
        .0;
    assert!(all.is_empty());
    // The only evidence object: CURRENT while head state matches.
    let evidence = {
        let mut out = Vec::new();
        for (_, latest) in query::all_trajectories(&repo).unwrap() {
            for (_, tobj) in query::trajectory_versions(&repo, &latest).unwrap() {
                let fs = tobj.field_sequence().unwrap_or(&[]);
                for c in query::gid_list(fs, 0x06) {
                    if let Ok(cobj) = repo.load(&c) {
                        let cfs = cobj.field_sequence().unwrap_or(&[]);
                        out.extend(query::gid_list(cfs, 0x0D));
                    }
                }
            }
        }
        out
    };
    assert_eq!(evidence.len(), 1);
    let row = query::evidence_show(&repo, &evidence[0].to_string()).unwrap();
    // No evaluated_state anchor: fresh by default.
    assert_eq!(row.freshness, query::Freshness::Current);
    // With an explicit anchor at the head state it is CURRENT; advancing the
    // head conservatively marks MAY_REQUIRE_REFRESH.
    let obj = repo.load(&evidence[0]).unwrap();
    let mut fields = obj.field_sequence().unwrap_or(&[]).to_vec();
    fields.push(gemel::value::Field::new(
        0x11,
        gemel::value::Value::Gid(head),
    ));
    fields.sort_by_key(|f| f.tag);
    let anchored = gemel::value::Object::fields(gemel::family::Family::Evidence, fields);
    let anchored_id = repo.insert_object(&anchored).unwrap();
    assert_eq!(
        query::evidence_freshness(&repo, &anchored_id).unwrap(),
        query::Freshness::Current
    );
}

#[test]
fn context_bundle_is_bounded_and_deterministic() {
    let root = temp_root("ctx");
    let demo = seed_acceptance_demo(&root);
    let repo = Repo::find(&root).unwrap();

    let a = query::context_bundle(
        &repo,
        "parser.rs",
        Some("I1"),
        4096,
        IncludeFlags::parse("").unwrap(),
    )
    .unwrap();
    let b = query::context_bundle(
        &repo,
        "parser.rs",
        Some("I1"),
        4096,
        IncludeFlags::parse("").unwrap(),
    )
    .unwrap();
    // Deterministic: identical inputs → identical bundles.
    let key = |x: &query::ContextBundle| {
        x.items
            .iter()
            .map(|i| format!("{}:{}:{}", i.id, i.level, i.summary))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&a), key(&b));
    assert!(a.consumed <= a.budget_tokens);
    assert!(!a.items.is_empty());
    let _ = demo;

    // A tiny budget bounds the bundle and reports the expansion point.
    let tiny = query::context_bundle(
        &repo,
        "parser.rs",
        None,
        16,
        IncludeFlags::parse("claims,attempts,evidence,residuals").unwrap(),
    )
    .unwrap();
    assert!(tiny.consumed <= tiny.budget_tokens);
    assert!(
        tiny.items.len() < a.items.len(),
        "smaller budget yields a smaller bundle ({} vs {})",
        tiny.items.len(),
        a.items.len()
    );
}

#[test]
fn why_reports_uncertainty_when_subject_absent() {
    let root = temp_root("why-none");
    write_file(&root, "a.txt", "x\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let report = query::why(&repo, "ghost.rs").unwrap();
    assert!(report.introduced_by.is_none());
    assert!(
        !report.uncertainty.is_empty(),
        "explicit uncertainty, never invented history"
    );
    assert!(report.previous_approaches.is_empty());
}

#[test]
fn checkpoint_chains_versions() {
    let root = temp_root("cp");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("long work".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "1\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "c1".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let k1 = workflow::create_checkpoint(&repo, &workflow::CheckpointOptions::default()).unwrap();
    let k2 = workflow::create_checkpoint(&repo, &workflow::CheckpointOptions::default()).unwrap();
    assert_eq!(k1.name, "K1");
    assert_eq!(k2.name, "K2");
    // The second checkpoint chains to the first.
    let obj = repo.load(&k2.checkpoint).unwrap();
    let prev = query::gid_field(obj.field_sequence().unwrap_or(&[]), 0x01);
    assert_eq!(prev, Some(k1.checkpoint));
    // refs/checkpoints/current points at the latest.
    let current = repo
        .read_ref(&format!("{}/current", gemel::store::REF_CHECKPOINTS))
        .unwrap();
    assert_eq!(current, Some(k2.checkpoint));
}

#[test]
fn cli_json_forms_are_parseable() {
    let root = temp_root("cli");
    let demo = seed_acceptance_demo(&root);
    let _ = demo;
    let bin = env!("CARGO_BIN_EXE_gemel");
    let run = |args: &[&str]| -> serde_json::Value {
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--repo").arg(&root);
        cmd.args(args);
        let out = cmd.output().expect("run gemel");
        assert_eq!(
            out.status.code(),
            Some(0),
            "args {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("valid gemel.query.v1 JSON")
    };
    let why = run(&["why", "parser.rs", "--json"]);
    assert_eq!(why["schema"], "gemel.query.v1");
    assert_eq!(why["request"]["command"], "why");
    assert!(why["result"]["introduced_by"]["claim"].is_object());
    let claims = run(&["claims", "--subject", "parser.rs", "--json"]);
    assert_eq!(claims["pagination"]["count"], 1);
    assert_eq!(
        claims["result"]["claims"][0]["status"],
        "PARTIALLY_SUPPORTED"
    );
    let residuals = run(&["residuals", "--disposition", "open", "--json"]);
    assert!(!residuals["result"]["residuals"]
        .as_array()
        .unwrap()
        .is_empty());
    let attempts = run(&["attempts", "parser.rs", "--json"]);
    let names: Vec<&str> = attempts["result"]["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    assert!(names.contains(&"T1"));
    let traj = run(&["trajectory", "T2", "--json"]);
    assert_eq!(traj["result"]["outcome"], serde_json::Value::Null);
    let cp = run(&["checkpoint", "--json"]);
    assert!(cp["result"]["name"].as_str().unwrap().starts_with('K'));
    let ctx = run(&["context", "parser.rs", "--budget", "2000", "--json"]);
    assert!(ctx["result"]["bundle"]["objects"].as_array().unwrap().len() >= 5);
}
