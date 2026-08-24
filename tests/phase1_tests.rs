//! Phase 1 integration tests (SPECIFICATION.md §10, §42).
//!
//! These tests prove the Phase 1 exit criteria: `init`, `status`, `snapshot`,
//! `change begin/finish`, `log`, `show`, `diff`, `fsck` operate on a local
//! store, and the demonstration `State S0 → Intent I1 → Trajectory T1 →
//! Change C1 → State S1` works with exact content-addressed reconstruction.

// Test closures return the rich store error type; boxed variants would
// obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use gemel::content;
use gemel::ignore::Ignore;
use gemel::query;
use gemel::store::fsck::FsckOptions;
use gemel::store::{InitOptions, Repo, REF_HEAD, REF_NAMES, REF_STATE_HEAD};
use gemel::workflow::{self, BeginOptions, ClaimSpec, EvidenceSpec, FinishOptions, ResidualSpec};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p1-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn read_dir_tree(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    fn walk(dir: &Path, prefix: &str, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".gemel" {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &rel, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    walk(root, "", &mut out);
    out
}

// ---------------------------------------------------------------------------
// The Phase 1 demonstration
// ---------------------------------------------------------------------------

#[test]
fn demo_s0_to_s1_exact_reconstruction() {
    let root = temp_root("demo");
    write_file(&root, "src/name.rs", "pub fn decode(name: &[u8]) -> String {\n    String::from_utf8_lossy(name).into_owned()\n}\n");
    write_file(&root, "src/lib.rs", "mod name;\n");
    write_file(&root, "README.md", "# gemel\n");

    let repo = Repo::init(&root, &InitOptions::default()).unwrap();

    // State S0: snapshot the initial tree and register the name manually
    // (the counter stays at 0 so the first finished change names its
    // resulting state S1).
    let ignore = Ignore::from_root(&root);
    let snap0 = content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        let ops = vec![gemel::store::refs::RefOp::set(
            &format!("{REF_NAMES}/S0"),
            snap0.state,
        )];
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction { ops })
            .unwrap();
        workflow::set_workspace_state(&repo, snap0.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    let s0 = snap0.state;
    let tree_before = read_dir_tree(&root);

    // Intent I1 + change begin.
    let begun = workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some(
                "Implement pointer-loop detection matching upstream behavior".into(),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(begun.intent_name.as_deref(), Some("I1"));
    assert_eq!(begun.input_state, Some(s0));

    // Modify the working tree.
    write_file(&root, "src/name.rs", "pub fn decode(name: &[u8]) -> String {\n    reject_loops(name);\n    String::from_utf8_lossy(name).into_owned()\n}\n\nfn reject_loops(name: &[u8]) {\n    // pointer loops > 16 rejected\n}\n");
    write_file(
        &root,
        "src/ptr.rs",
        "pub fn check(b: &[u8]) -> bool { b.len() > 16 }\n",
    );
    std::fs::remove_file(root.join("README.md")).unwrap();

    // Change C1, trajectory T1, state S1.
    let finished = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "Add pointer-loop rejection".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(finished.change_name, "C1");
    assert_eq!(finished.trajectory_name, "T1");
    assert_eq!(finished.state_name, "S1");
    assert!(finished.is_new_trajectory);
    let tree_after = read_dir_tree(&root);

    // Names resolve.
    let c1 = repo.resolve("C1").unwrap();
    let t1 = repo.resolve("T1").unwrap();
    let i1 = repo.resolve("I1").unwrap();
    let s1 = repo.resolve("S1").unwrap();
    assert_eq!(c1, finished.change);
    assert_eq!(t1, finished.trajectory);
    assert_eq!(s1, finished.state);
    assert_eq!(repo.resolve("S0").unwrap(), s0);

    // Refs.
    assert_eq!(repo.read_ref(REF_HEAD).unwrap(), Some(c1));
    assert_eq!(repo.read_ref(REF_STATE_HEAD).unwrap(), Some(s1));
    assert_eq!(workflow::workspace_state(&repo).unwrap(), Some(s1));

    // The change object.
    let change = repo.load(&c1).unwrap();
    let fs = change.field_sequence().unwrap();
    assert_eq!(
        query::str_field(fs, 0x01).unwrap(),
        "Add pointer-loop rejection"
    );
    assert_eq!(query::gid_field(fs, 0x02), Some(i1));
    assert_eq!(query::gid_field(fs, 0x03), Some(s0));
    assert_eq!(query::gid_field(fs, 0x05), Some(s1));
    assert_eq!(query::gid_list(fs, 0x04).len(), 3); // modify, create, delete

    // The trajectory object chains.
    let traj = repo.load(&t1).unwrap();
    let tfs = traj.field_sequence().unwrap();
    assert_eq!(query::gid_field(tfs, 0x02), Some(i1));
    assert_eq!(query::gid_field(tfs, 0x03), Some(s0));
    assert!(query::gid_field(tfs, 0x01).is_none()); // first version

    // log.
    let entries = query::log(&repo, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].change, c1);
    assert_eq!(entries[0].name.as_deref(), Some("C1"));
    assert_eq!(entries[0].input_state, Some(s0));
    assert_eq!(entries[0].resulting_state, Some(s1));
    assert_eq!(entries[0].trajectory.as_deref(), Some("T1"));

    // diff S0 → S1.
    let deltas = content::diff_states(&repo, &s0, &s1).unwrap();
    let kinds: Vec<&str> = deltas
        .iter()
        .map(|d| match d.kind {
            content::DeltaKind::Created => "create",
            content::DeltaKind::Modified => "modify",
            content::DeltaKind::Deleted => "delete",
            content::DeltaKind::Renamed { .. } => "rename",
        })
        .collect();
    assert!(kinds.contains(&"modify"));
    assert!(kinds.contains(&"create"));
    assert!(kinds.contains(&"delete"));

    // EXACT CONTENT-ADDRESSED RECONSTRUCTION.
    let dir_s0 = temp_root("recon-s0");
    let dir_s1 = temp_root("recon-s1");
    content::materialize(&repo, &s0, &dir_s0).unwrap();
    content::materialize(&repo, &s1, &dir_s1).unwrap();
    assert_eq!(
        read_dir_tree(&dir_s0),
        tree_before,
        "S0 must reconstruct byte-exact"
    );
    assert_eq!(
        read_dir_tree(&dir_s1),
        tree_after,
        "S1 must reconstruct byte-exact"
    );

    // Status.
    let st = query::status(&repo).unwrap();
    assert_eq!(st.trajectory.as_deref(), Some("T1"));
    assert_eq!(st.intent, Some(i1));
    assert!(st.changed.is_empty(), "working tree matches S1");
    assert_eq!(st.readiness, query::Readiness::Ready);

    // fsck is clean.
    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "fsck problems: {:?}", report.problems);
}

#[test]
fn second_change_continues_trajectory() {
    let root = temp_root("cont");
    write_file(&root, "a.txt", "one\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = Ignore::from_root(&root);
    let snap = content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        let mut meta = repo.read_meta().unwrap();
        let n = meta["counters"]["state"].as_u64().unwrap() + 1;
        meta["counters"]["state"] = serde_json::json!(n);
        repo.write_meta(&meta).unwrap();
        let ops = vec![gemel::store::refs::RefOp::set(
            &format!("{REF_NAMES}/S{n}"),
            snap.state,
        )];
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction { ops })
            .unwrap();
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();

    let first = workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Build feature X".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.txt", "one\nplus\n");
    let f1 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "first".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(f1.trajectory_name, "T1");
    assert!(f1.is_new_trajectory);
    let _ = first;

    // Second change with the same intent continues T1.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "b.txt", "new file\n");
    let f2 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "second".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        f2.trajectory_name, "T1",
        "same intent continues the trajectory"
    );
    assert!(!f2.is_new_trajectory);

    let t1 = repo.resolve("T1").unwrap();
    let traj = repo.load(&t1).unwrap();
    let tfs = traj.field_sequence().unwrap();
    let prev = query::gid_field(tfs, 0x01).expect("chained previous");
    assert_eq!(
        prev, f1.trajectory,
        "trajectory chains to the previous version"
    );

    // Different intent starts a new trajectory.
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Unrelated work".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "c.txt", "c\n");
    let f3 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "third".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(f3.trajectory_name, "T2");
    assert!(f3.is_new_trajectory);

    // log shows all three changes (newest first).
    let entries = query::log(&repo, 10).unwrap();
    assert_eq!(entries.len(), 3);
}

#[test]
fn claims_evidence_residuals_flow() {
    let root = temp_root("cer");
    write_file(&root, "parser.rs", "fn decode() {}\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Parser compatibility".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "parser.rs", "fn decode() { /* loops rejected */ }\n");

    let finished = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "parser rewrite".into(),
            claims: vec![
                ClaimSpec {
                    subject: Some("parser::decode".into()),
                    predicate: "parser accepts all valid inputs".into(),
                    kind: "correctness".into(),
                },
                ClaimSpec {
                    subject: Some("parser::decode".into()),
                    predicate: "parser matches upstream on all inputs".into(),
                    kind: "compatibility".into(),
                },
            ],
            evidence: vec![
                EvidenceSpec {
                    subject: Some("parser::decode".into()),
                    outcome: "pass".into(),
                    kind: "court_receipt".into(),
                },
                EvidenceSpec {
                    subject: Some("parser::decode".into()),
                    outcome: "mismatch".into(),
                    kind: "oracle_comparison".into(),
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
    assert_eq!(finished.claims.len(), 2);
    assert_eq!(finished.evidence.len(), 2);
    assert_eq!(finished.residuals.len(), 1);

    // Derived claim statuses: pass evidence + fail evidence on the same
    // subject → PARTIALLY_SUPPORTED for both (basic subject-matched linking).
    let (status, supporting, contradicting) =
        query::claim_status(&repo, &finished.claims[0]).unwrap();
    assert_eq!(status, query::ClaimStatus::PartiallySupported);
    assert_eq!(supporting.len(), 1);
    assert_eq!(contradicting.len(), 1);
    let (status2, _, _) = query::claim_status(&repo, &finished.claims[1]).unwrap();
    assert_eq!(status2, query::ClaimStatus::PartiallySupported);

    // Residual: open disposition, persistence counts descendants.
    let res = finished.residuals[0];
    assert_eq!(query::residual_disposition(&repo, &res).unwrap(), "open");
    assert_eq!(query::residual_persistence(&repo, &res).unwrap(), 0);

    // Status surfaces the residual and readiness.
    let st = query::status(&repo).unwrap();
    assert_eq!(st.residuals.len(), 1);
    assert_eq!(st.residuals[0].disposition, "open");
    assert_eq!(
        st.readiness,
        query::Readiness::NotReady,
        "contradicted claims"
    );

    // show a claim renders its derived status.
    let (_, obj, _) = query::show(&repo, &finished.claims[0].to_string()).unwrap();
    assert_eq!(obj.family, gemel::family::Family::Claim);
}

#[test]
fn snapshot_and_status_detect_working_tree_changes() {
    let root = temp_root("status");
    write_file(&root, "a.txt", "one\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = Ignore::from_root(&root);
    let snap = content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        let mut meta = repo.read_meta().unwrap();
        let n = meta["counters"]["state"].as_u64().unwrap() + 1;
        meta["counters"]["state"] = serde_json::json!(n);
        repo.write_meta(&meta).unwrap();
        let ops = vec![gemel::store::refs::RefOp::set(
            &format!("{REF_NAMES}/S{n}"),
            snap.state,
        )];
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction { ops })
            .unwrap();
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();

    // Clean.
    let st = query::status(&repo).unwrap();
    assert!(st.changed.is_empty());

    // Modify + add + delete.
    write_file(&root, "a.txt", "changed\n");
    write_file(&root, "b.txt", "new\n");
    std::fs::remove_file(root.join("a.txt")).unwrap();
    std::fs::write(root.join("a.txt"), "changed\n").unwrap();
    let st = query::status(&repo).unwrap();
    let paths: Vec<&str> = st.changed.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&"a.txt"), "modified: {paths:?}");
    assert!(paths.contains(&"b.txt"), "added: {paths:?}");
    assert_eq!(st.changed.len(), 2);
}

#[test]
fn ignore_rules_exclude_paths() {
    let root = temp_root("ignore");
    write_file(&root, "keep.rs", "pub fn f() {}\n");
    write_file(&root, "skip.log", "noise\n");
    write_file(&root, "build/out.o", "binary");
    write_file(&root, "src/mod.rs", "mod x;\n");
    std::fs::write(root.join(".gitignore"), "*.log\nbuild/\n").unwrap();

    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = Ignore::from_root(&root);
    let snap = content::build_state(&repo, &root, &ignore).unwrap();
    assert_eq!(snap.files, 2, "keep.rs and src/mod.rs only");
}

// ---------------------------------------------------------------------------
// fsck
// ---------------------------------------------------------------------------

fn seed_simple_repo(root: &Path) -> Repo {
    write_file(root, "a.txt", "alpha\n");
    let repo = Repo::init(root, &InitOptions::default()).unwrap();
    workflow::begin_change(&repo, &BeginOptions::default()).unwrap();
    write_file(root, "b.txt", "beta\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "seed".into(),
            ..Default::default()
        },
    )
    .unwrap();
    repo
}

#[test]
fn fsck_detects_corruption_and_missing_objects() {
    let root = temp_root("fsck-corrupt");
    let repo = seed_simple_repo(&root);

    // Corrupt an object file.
    let head = repo.read_ref(REF_HEAD).unwrap().unwrap();
    let path = gemel::store::objects::object_path(repo.meta_dir(), &head);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[10] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert_eq!(
        report.exit_code(),
        2,
        "corruption must be detected: {:?}",
        report.problems
    );
    assert!(report
        .problems
        .iter()
        .any(|p| p.code == "corrupt-object" || p.code == "invalid-object"));

    // Repair is impossible for content corruption: still exit 2.
    let report = repo
        .fsck(&FsckOptions {
            repair: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report.exit_code(), 2);
}

#[test]
fn fsck_index_rebuild_and_drift() {
    let root = temp_root("fsck-index");
    let repo = seed_simple_repo(&root);

    // Delete the index database: derived data, must not affect correctness.
    gemel::store::index::remove(&repo).unwrap();
    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(
        report.exit_code() == 2,
        "drift detected: {:?}",
        report.problems
    );

    let report = repo
        .fsck(&FsckOptions {
            rebuild_index: true,
            ..Default::default()
        })
        .unwrap();
    assert!(report.repairs.iter().any(|r| r.contains("index")));
    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "after rebuild: {:?}", report.problems);

    // Rebuilding again performs maintenance work (exit 1) and must not
    // introduce drift; a follow-up plain run is fully clean.
    let report2 = repo
        .fsck(&FsckOptions {
            rebuild_index: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(report2.exit_code(), 1);
    assert!(report2.repairs.iter().any(|r| r.contains("index")));
    let report3 = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(
        report3.is_clean(),
        "after second rebuild: {:?}",
        report3.problems
    );
}

#[test]
fn fsck_journal_recovery() {
    let root = temp_root("fsck-journal");
    let repo = seed_simple_repo(&root);
    let head = repo.read_ref(REF_HEAD).unwrap().unwrap();

    // Simulate an interrupted ref transaction (journal with no commit marker).
    let journal = gemel::store::refs::journal_path(repo.meta_dir());
    let fake = format!(
        "{{\"op\":\"set\",\"ref\":\"{REF_HEAD}\",\"new\":\"{}\",\"prev\":null}}\n",
        head
    );
    std::fs::write(&journal, fake).unwrap();

    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(report
        .problems
        .iter()
        .any(|p| p.code == "interrupted-transaction"));

    let report = repo
        .fsck(&FsckOptions {
            repair: true,
            ..Default::default()
        })
        .unwrap();
    assert!(report.repairs.iter().any(|r| r.contains("journal")));
    // Repair work reports exit 1; a follow-up run must be fully clean.
    assert_eq!(report.exit_code(), 1);
    let report = repo.fsck(&FsckOptions::default()).unwrap();
    assert!(report.is_clean(), "after repair: {:?}", report.problems);
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

fn cli(repo_root: &Path, args: &[&str]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_gemel");
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("--repo").arg(repo_root);
    cmd.args(args);
    let out = cmd.output().expect("run gemel");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cli_end_to_end_flow() {
    let root = temp_root("cli");
    write_file(&root, "file.txt", "hello\n");
    let (code, out, err) = cli(&root, &["init"]);
    assert_eq!(code, 0, "init failed: {err}");
    assert!(out.contains("initialized"));

    let (code, out, _) = cli(&root, &["snapshot", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"name\": \"S1\""));
    assert!(out.contains("gemel.query.v1"));

    let (code, out, err) = cli(
        &root,
        &["change", "begin", "--intent-summary", "CLI demo intent"],
    );
    assert_eq!(code, 0, "begin failed: {err}");
    assert!(out.contains("intent: I1") || out.contains("I1"));

    write_file(&root, "file.txt", "hello\nworld\n");
    let (code, out, err) = cli(
        &root,
        &["change", "finish", "--summary", "CLI change", "--json"],
    );
    assert_eq!(code, 0, "finish failed: {err}");
    assert!(out.contains("\"change_name\": \"C1\""));
    assert!(out.contains("\"trajectory_name\": \"T1\""));
    assert!(out.contains("\"state_name\": \"S2\""));

    let (code, out, _) = cli(&root, &["log", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("CLI change"));

    let (code, out, _) = cli(&root, &["show", "C1"]);
    assert_eq!(code, 0);
    assert!(out.contains("CLI change"));

    let (code, out, _) = cli(&root, &["diff", "S1", "S2", "--stat"]);
    assert_eq!(code, 0);
    assert!(out.contains("~ file.txt"));

    let (code, out, _) = cli(&root, &["status", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"readiness\": \"READY\""));

    let (code, out, _) = cli(&root, &["fsck"]);
    assert_eq!(code, 0);
    assert!(out.contains("clean"));

    let (code, out, _) = cli(&root, &["show", "I1", "--json"]);
    assert_eq!(code, 0);
    assert!(out.contains("\"family\": \"intent\""));
}

#[test]
fn cli_error_paths() {
    let root = temp_root("cli-err");
    // change finish without init → repository error.
    let (code, _, err) = cli(&root, &["change", "finish"]);
    assert_eq!(code, 2);
    assert!(err.contains("not a gemel repository"));

    let _ = Repo::init(&root, &InitOptions::default()).unwrap();
    // finish without begin.
    let (code, _, err) = cli(&root, &["change", "finish"]);
    assert_eq!(code, 2);
    assert!(err.contains("no change in progress"));

    // fsck on a fresh repo is clean.
    let (code, out, _) = cli(&root, &["fsck"]);
    assert_eq!(code, 0);
    assert!(out.contains("clean"));
}

#[test]
fn checkout_reconstructs_states() {
    let root = temp_root("checkout");
    write_file(&root, "v1.txt", "version one\n");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(&repo, &BeginOptions::default()).unwrap();
    std::fs::remove_file(root.join("v1.txt")).unwrap();
    write_file(&root, "v2.txt", "version two\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "v2".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let s1 = repo.resolve("S1").unwrap();

    let target = temp_root("checkout-target");
    let (code, out, err) = cli(
        &root,
        &[
            "checkout",
            &s1.to_string(),
            "--dir",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "checkout failed: {err}");
    assert!(out.contains("checked out"));
    let files: Vec<String> = read_dir_tree(&target).keys().cloned().collect();
    assert_eq!(files, vec!["v2.txt"]);
}
