//! Phase 3 integration tests (SPECIFICATION.md §44, OBJECT_MODEL.md §6.17,
//! AGENT_PROTOCOL.md §5.10).
//!
//! These tests prove the Phase 3 exit criteria: `gemel reconcile` using
//! textual changes, path changes, explicit Claims, Evidence, Residuals;
//! producing a Reconciliation object (adopted / rejected / unresolved /
//! verification required / resulting State); concurrent agents working from
//! the same base State; uncertainty exposed, never invented.

// Test closures return the rich store error type; boxed variants would
// obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use gemel::reconcile::{self, ReconcileInput, ReconcileOptions};
use gemel::store::{InitOptions, Repo, REF_HEAD, REF_STATE_HEAD};
use gemel::value::{Field, Value};
use gemel::workflow::{self, BeginOptions, ClaimSpec, EvidenceSpec, FinishOptions, ResidualSpec};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p3-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Seeds a repository at a common base state S1: two files, snapshot only
/// (no changes yet). Returns the repo and the base state.
fn seed_base(root: &Path) -> (Repo, gemel::gid::Gid) {
    write_file(root, "a.txt", "a1\n");
    write_file(root, "b.txt", "b1\n");
    let repo = Repo::init(root, &InitOptions::default()).unwrap();
    let ignore = gemel::ignore::Ignore::from_root(root);
    let snap = gemel::content::build_state(&repo, root, &ignore).unwrap();
    repo.with_write_lock(|| {
        let ops = vec![gemel::store::refs::RefOp::set(
            &format!("{}/S1", gemel::store::REF_NAMES),
            snap.state,
        )];
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction { ops })
            .unwrap();
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    (repo, snap.state)
}

/// Concurrent agent A: a change touching `a.txt` from the base state, in its
/// own workspace and working directory (brief §34: no filesystem
/// serialization between agents).
fn agent_a_change(repo: &Repo, _root: &Path, base: &gemel::gid::Gid) -> PathBuf {
    let wa = temp_root("wa");
    gemel::content::materialize(repo, base, &wa).unwrap();
    workflow::begin_change(
        repo,
        &BeginOptions {
            from_state: Some(*base),
            intent_summary: Some("Agent A work".into()),
            workspace: Some("wa".into()),
            worktree: Some(wa.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&wa, "a.txt", "a2 from A\n");
    workflow::finish_change(
        repo,
        &FinishOptions {
            summary: "A modifies a.txt".into(),
            claims: vec![ClaimSpec {
                subject: Some("a.txt".into()),
                predicate: "a.txt is stable under A".into(),
                kind: "correctness".into(),
            }],
            workspace: Some("wa".into()),
            worktree: Some(wa.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    wa
}

/// Concurrent agent B: a change touching `b.txt` from the base state, in its
/// own workspace and working directory.
fn agent_b_change(repo: &Repo, _root: &Path, base: &gemel::gid::Gid) -> PathBuf {
    let wb = temp_root("wb");
    gemel::content::materialize(repo, base, &wb).unwrap();
    workflow::begin_change(
        repo,
        &BeginOptions {
            from_state: Some(*base),
            intent_summary: Some("Agent B work".into()),
            workspace: Some("wb".into()),
            worktree: Some(wb.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&wb, "b.txt", "b2 from B\n");
    workflow::finish_change(
        repo,
        &FinishOptions {
            summary: "B modifies b.txt".into(),
            workspace: Some("wb".into()),
            worktree: Some(wb.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    wb
}

fn input(repo: &Repo, name: &str) -> ReconcileInput {
    ReconcileInput {
        name: name.to_string(),
        gid: repo.resolve(name).unwrap(),
    }
}

fn state_content(repo: &Repo, state: &gemel::gid::Gid, path: &str) -> String {
    let files = gemel::content::state_files(repo, state).unwrap();
    let (_, blob) = files.get(path).expect("path in state");
    let obj = repo.load(blob).unwrap();
    String::from_utf8_lossy(obj.blob_bytes().unwrap_or(&[])).into_owned()
}

#[test]
fn concurrent_agents_same_base_no_conflict() {
    let root = temp_root("concurrent");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);

    // Both trajectories derive from the same base state.
    assert_eq!(
        gemel::query::trajectory_detail(&repo, "T1")
            .unwrap()
            .base_state,
        Some(base)
    );
    assert_eq!(
        gemel::query::trajectory_detail(&repo, "T2")
            .unwrap()
            .base_state,
        Some(base)
    );

    // Reconcile: disjoint paths → adopt everything, no conflicts.
    let plan = reconcile::analyze(&repo, &[input(&repo, "T1"), input(&repo, "T2")]).unwrap();
    assert!(
        plan.textual_conflicts.is_empty(),
        "{:?}",
        plan.textual_conflicts
    );
    assert_eq!(plan.adopted.len(), 2);
    assert!(plan.rejected.is_empty());
    // Merged file map contains both changes (the plan's state identity is
    // in-memory; the file map is the ground truth).
    let read_blob = |repo: &Repo, g: &gemel::gid::Gid| {
        String::from_utf8_lossy(repo.load(g).unwrap().blob_bytes().unwrap_or(&[])).into_owned()
    };
    assert_eq!(
        read_blob(&repo, &plan.merged_files.get("a.txt").unwrap().1),
        "a2 from A\n"
    );
    assert_eq!(
        read_blob(&repo, &plan.merged_files.get("b.txt").unwrap().1),
        "b2 from B\n"
    );
    // The retained claim from A is present; it is unverified (no evidence).
    assert_eq!(plan.claims_retained.len(), 1);
    assert_eq!(plan.verification_required, plan.claims_retained);
}

#[test]
fn textual_conflict_first_input_wins_and_is_recorded() {
    let root = temp_root("conflict");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    // Agent C also modifies a.txt from the same base, in its own workspace.
    let wc = temp_root("wc");
    gemel::content::materialize(&repo, &base, &wc).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            from_state: Some(base),
            intent_summary: Some("Agent C work".into()),
            workspace: Some("wc".into()),
            worktree: Some(wc.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&wc, "a.txt", "a2 from C\n");
    let c_change = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "C modifies a.txt".into(),
            evidence: vec![EvidenceSpec {
                subject: Some("a.txt".into()),
                outcome: "pass".into(),
                kind: "test_result".into(),
            }],
            workspace: Some("wc".into()),
            worktree: Some(wc.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let plan = reconcile::analyze(&repo, &[input(&repo, "T1"), input(&repo, "T2")]).unwrap();
    assert_eq!(plan.textual_conflicts.len(), 1);
    assert_eq!(plan.textual_conflicts[0].path, "a.txt");
    assert_eq!(plan.textual_conflicts[0].changes.len(), 2);
    // First input (T1) wins the path: A's change adopted, C's rejected.
    assert_eq!(plan.adopted.len(), 1);
    assert_eq!(plan.rejected, vec![c_change.change]);
    assert!(plan
        .interactions
        .iter()
        .any(|i| i.kind == "textual" && i.certainty == "observed"));
    // Resulting state carries the adopted version.
    let a = plan.merged_files.get("a.txt").unwrap().1;
    let content =
        String::from_utf8_lossy(repo.load(&a).unwrap().blob_bytes().unwrap_or(&[])).into_owned();
    assert_eq!(content, "a2 from A\n");
    assert_eq!(
        state_content(&repo, &plan.resulting_state, "a.txt"),
        "a2 from A\n"
    );
    assert!(plan.rationale.contains("first-input-trajectory-wins"));
    // C's claim about a.txt is invalidated (subject touched by adopted work).
    assert_eq!(plan.claims_invalidated.len(), 0); // C declared no claim
                                                  // C's evidence is not retained.
    assert!(plan.evidence_retained.is_empty());
}

#[test]
fn reconciliation_object_records_the_decision() {
    let root = temp_root("object");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);

    let out = reconcile::reconcile(
        &repo,
        &[input(&repo, "T1"), input(&repo, "T2")],
        &ReconcileOptions::default(),
    )
    .unwrap();
    assert_eq!(out.reconciliation_name, "Re1");
    assert!(out.plan.textual_conflicts.is_empty());

    let obj = repo.load(&out.reconciliation).unwrap();
    assert_eq!(obj.family, gemel::family::Family::Reconciliation);
    let fs = obj.field_sequence().unwrap();
    // input_trajectories (0x03) lists both inputs.
    let inputs = gemel::query::gid_list(fs, 0x03);
    assert_eq!(inputs.len(), 2);
    // adopted (0x05) = both changes; rejected (0x06) absent.
    assert_eq!(gemel::query::gid_list(fs, 0x05).len(), 2);
    assert!(gemel::query::gid_list(fs, 0x06).is_empty());
    // resulting_state (0x0E) is the merged state; resulting_change (0x0F).
    assert_eq!(gemel::query::gid_field(fs, 0x0E), Some(out.state));
    assert_eq!(gemel::query::gid_field(fs, 0x0F), Some(out.change));
    // rationale (0x10) is present.
    let rationale = gemel::query::str_field(fs, 0x10).unwrap();
    assert!(rationale.contains("per-path first-input-trajectory-wins"));

    // The resulting change has the merged state and causal parents.
    let cobj = repo.load(&out.change).unwrap();
    let cfs = cobj.field_sequence().unwrap();
    assert_eq!(gemel::query::gid_field(cfs, 0x05), Some(out.state));
    assert_eq!(gemel::query::gid_list(cfs, 0x11).len(), 2);
    // Names resolve.
    assert_eq!(repo.resolve("Re1").unwrap(), out.reconciliation);
    assert_eq!(repo.resolve(&out.state_name).unwrap(), out.state);
    assert_eq!(repo.resolve(&out.change_name).unwrap(), out.change);
    // Without --apply the head is untouched: it still points at the last
    // agent finish (B's change), not the reconciliation's change.
    assert_ne!(repo.read_ref(REF_HEAD).unwrap(), Some(out.change));
    assert_eq!(
        repo.read_ref(REF_STATE_HEAD).unwrap(),
        Some(
            gemel::query::gid_field(
                repo.load(&repo.read_ref(REF_HEAD).unwrap().unwrap())
                    .unwrap()
                    .field_sequence()
                    .unwrap(),
                0x05
            )
            .unwrap()
        )
    );
    let _ = base;
}

#[test]
fn plan_is_read_only_and_deterministic() {
    let root = temp_root("plan");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);
    let inputs = [input(&repo, "T1"), input(&repo, "T2")];

    let p1 = reconcile::analyze(&repo, &inputs).unwrap();
    let p2 = reconcile::analyze(&repo, &inputs).unwrap();
    assert_eq!(p1.resulting_state, p2.resulting_state);
    assert_eq!(p1.adopted, p2.adopted);
    assert_eq!(p1.rationale, p2.rationale);
    // The plan publishes nothing: refs are exactly as the agent finishes
    // left them (head = B's change, state/head = B's resulting state).
    let head_before = repo.read_ref(REF_HEAD).unwrap();
    let state_before = repo.read_ref(REF_STATE_HEAD).unwrap();
    assert!(head_before.is_some());
    // The planned resulting state identity matches the executed one.
    let out = reconcile::reconcile(&repo, &inputs, &ReconcileOptions::default()).unwrap();
    assert_eq!(out.plan.resulting_state, p1.resulting_state);
    assert_eq!(out.state, p1.resulting_state);
    // Reconcile without --apply leaves refs untouched.
    assert_eq!(repo.read_ref(REF_HEAD).unwrap(), head_before);
    assert_eq!(repo.read_ref(REF_STATE_HEAD).unwrap(), state_before);
}

#[test]
fn apply_advances_head_and_workspace() {
    let root = temp_root("apply");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);

    let out = reconcile::reconcile(
        &repo,
        &[input(&repo, "T1"), input(&repo, "T2")],
        &ReconcileOptions {
            apply: true,
            producer: None,
        },
    )
    .unwrap();
    assert_eq!(repo.read_ref(REF_HEAD).unwrap(), Some(out.change));
    assert_eq!(repo.read_ref(REF_STATE_HEAD).unwrap(), Some(out.state));
    assert_eq!(workflow::workspace_state(&repo).unwrap(), Some(out.state));
    // Materialize the merged state into the default working tree; status is
    // then clean (workspace matches head state).
    gemel::content::materialize(&repo, &out.state, &root).unwrap();
    let st = gemel::query::status(&repo).unwrap();
    assert!(
        st.changed.is_empty(),
        "workspace matches merged state: {:?}",
        st.changed
    );
}

#[test]
fn base_mismatch_is_refused() {
    let root = temp_root("mismatch");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    // The sequential change explicitly bases on A's resulting state.
    let a_head = repo.read_ref(REF_HEAD).unwrap().unwrap();
    let a_result =
        gemel::query::gid_field(repo.load(&a_head).unwrap().field_sequence().unwrap(), 0x05)
            .unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            from_state: Some(a_result),
            intent_summary: Some("sequential".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "b.txt", "b2\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "sequential change".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // T1 bases on S1; T2 bases on A's result → refuse, fail closed.
    let err = reconcile::analyze(&repo, &[input(&repo, "T1"), input(&repo, "T2")]).unwrap_err();
    assert!(
        err.to_string().contains("do not share a base state"),
        "unexpected error: {err}"
    );
}

#[test]
fn residual_carry_forward_and_resolution() {
    let root = temp_root("residuals");
    let (repo, base) = seed_base(&root);
    // Agent A leaves an open residual; Agent B resolves one. Each works in
    // its own workspace (brief §34).
    let wa = temp_root("ra");
    gemel::content::materialize(&repo, &base, &wa).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            from_state: Some(base),
            intent_summary: Some("A".into()),
            workspace: Some("ra".into()),
            worktree: Some(wa.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&wa, "a.txt", "a2\n");
    let fa = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "A".into(),
            residuals: vec![ResidualSpec {
                summary: "FreeBSD still diverges".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
            }],
            workspace: Some("ra".into()),
            worktree: Some(wa.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let wb = temp_root("rb");
    gemel::content::materialize(&repo, &base, &wb).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            from_state: Some(base),
            intent_summary: Some("B".into()),
            workspace: Some("rb".into()),
            worktree: Some(wb.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&wb, "b.txt", "b2\n");
    let fb = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "B".into(),
            residuals: vec![ResidualSpec {
                summary: "fixed on FreeBSD".into(),
                severity: "medium".into(),
                classification: "platform_divergence".into(),
            }],
            workspace: Some("rb".into()),
            worktree: Some(wb.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    workflow::resolve_residual(
        &repo,
        &workflow::ResolveResidualOptions {
            residual: fb.residuals[0].to_string(),
            disposition: "resolved".into(),
            reason: Some("court now matches".into()),
            producer: None,
        },
    )
    .unwrap();

    let plan = reconcile::analyze(&repo, &[input(&repo, "T1"), input(&repo, "T2")]).unwrap();
    assert_eq!(plan.unresolved_residuals, vec![fa.residuals[0]]);
    assert_eq!(plan.resolved_residuals.len(), 1);
    // The resulting change carries the open residual forward.
    let out = reconcile::reconcile(
        &repo,
        &[input(&repo, "T1"), input(&repo, "T2")],
        &ReconcileOptions::default(),
    )
    .unwrap();
    let cobj = repo.load(&out.change).unwrap();
    let cfs = cobj.field_sequence().unwrap();
    assert!(gemel::query::gid_list(cfs, 0x0E).contains(&fa.residuals[0]));
}

#[test]
fn cli_reconcile_plan_and_execute() {
    let root = temp_root("cli-recon");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);
    let bin = env!("CARGO_BIN_EXE_gemel");
    let run = |args: &[&str]| -> (i32, serde_json::Value, String) {
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--repo").arg(&root);
        cmd.args(args);
        let out = cmd.output().expect("run gemel");
        (
            out.status.code().unwrap_or(-1),
            serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, plan, err) = run(&["reconcile", "T1", "T2", "--plan", "--json"]);
    assert_eq!(code, 0, "plan failed: {err}");
    assert_eq!(plan["schema"], "gemel.query.v1");
    assert_eq!(plan["result"]["mode"], "plan");
    assert!(plan["result"]["textual_conflicts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(plan["result"]["adopted"].as_array().unwrap().len(), 2);

    let (code, out, err) = run(&["reconcile", "T1", "T2", "--apply", "--json"]);
    assert_eq!(code, 0, "reconcile failed: {err}");
    assert_eq!(out["result"]["reconciliation_name"], "Re1");
    assert_eq!(out["result"]["applied"], serde_json::Value::Bool(true));
    assert_eq!(out["result"]["mode"], "executed");
    // Head advanced; fsck stays clean (canonical + derived consistent).
    let (code, _, _) = run(&["fsck"]);
    assert_eq!(code, 0, "fsck must be clean after reconcile");
    let _ = repo;
}

// ---------------------------------------------------------------------------
// Schema-order regression: the reconciliation object must encode with
// strictly ascending tags (the fail-closed encoder rejects out-of-order).
// ---------------------------------------------------------------------------

#[test]
fn reconciliation_object_encodes_with_ascending_tags() {
    let root = temp_root("tags");
    let (repo, base) = seed_base(&root);
    agent_a_change(&repo, &root, &base);
    agent_b_change(&repo, &root, &base);
    let out = reconcile::reconcile(
        &repo,
        &[input(&repo, "T1"), input(&repo, "T2")],
        &ReconcileOptions::default(),
    )
    .unwrap();
    let obj = repo.load(&out.reconciliation).unwrap();
    let fs = obj.field_sequence().unwrap();
    let mut prev = 0u8;
    for field in fs {
        assert!(
            field.tag > prev,
            "reconciliation tags must be ascending: {field:?}"
        );
        prev = field.tag;
    }
    // The interaction record shape round-trips.
    let interactions = gemel::query::value_at(fs, 0x09);
    assert!(interactions.is_none() || matches!(interactions, Some(Value::Array(_))));
    let _ = Field::new(0, Value::B(false)); // Field is in scope
}
