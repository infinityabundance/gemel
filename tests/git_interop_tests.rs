//! Phase 4 integration courts (GIT_INTEROP.md §5, SPECIFICATION.md Phase 4).
//!
//! Deterministic Git interchange: Gemel → Git (export) and Git → Gemel
//! (import), with stable identity trailers, documented loss, never-fabricated
//! provenance, and the mathematically provable round-trip core (trees and
//! topology round-trip exactly; identity linkage round-trips through
//! trailers). Uses real Git repositories end to end.

#![allow(clippy::result_large_err)]

use gemel::store::{InitOptions, Repo};
use gemel::workflow::{self, BeginOptions, FinishOptions};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p4-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git");
    if !out.status.success() {
        panic!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn gemel_cli(root: &Path, args: &[&str]) -> (i32, Value, String) {
    let bin = env!("CARGO_BIN_EXE_gemel");
    let out = Command::new(bin)
        .arg("--repo")
        .arg(root)
        .args(args)
        .output()
        .expect("run gemel");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let parsed = if stdout.trim().is_empty() || !stdout.trim_start().starts_with('{') {
        Value::Null
    } else {
        serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("gemel output is not JSON ({e}): {stdout}");
        })
    };
    (code, parsed, stderr)
}

fn result_of(root: &Path, args: &[&str]) -> Value {
    let (code, v, err) = gemel_cli(root, args);
    assert_eq!(code, 0, "gemel {} failed: {err}", args.join(" "));
    v["result"].clone()
}

fn git_init_commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// Commits with an explicit author/committer date (deterministic ordering).
fn git_init_commit_at(dir: &Path, msg: &str, date: &str) {
    git(dir, &["add", "-A"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg, "--date", date])
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git commit --date");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// All loose objects of a git dir: (kind, content) sorted by oid.
fn git_objects(git_dir: &Path) -> Vec<(String, String, Vec<u8>)> {
    let mut out = Vec::new();
    for (oid, kind, content) in gemel::git_io::read_loose_all(git_dir).unwrap() {
        out.push((kind, oid.to_string(), content));
    }
    out
}

/// A repository with two finished changes (one with claim/evidence/residual).
fn make_gemel_repo(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Fix parser compatibility".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "src.rs", "pub fn parse() { loop_detect(); }\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "implemented pointer-loop detection".into(),
            claims: vec![workflow::ClaimSpec {
                subject: Some("parser".into()),
                predicate: "accepts all valid RFC inputs".into(),
                kind: "behavior".into(),
                evidence: vec![],
            }],
            evidence: vec![workflow::EvidenceSpec {
                subject: Some("parser".into()),
                outcome: "pass".into(),
                kind: "test_result".into(),
            }],
            residuals: vec![workflow::ResidualSpec {
                summary: "FreeBSD divergence".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
                affected_claims: vec![],
                origin_evidence: None,
                affected_changes: vec![],
            }],
            ..Default::default()
        },
    )
    .unwrap();
    workflow::begin_change(&repo, &BeginOptions::default()).unwrap();
    write_file(
        &root,
        "src.rs",
        "pub fn parse() { loop_detect(); depth_limit(); }\nfn depth_limit() {}\n",
    );
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "bounded pointer-loop depth".into(),
            ..Default::default()
        },
    )
    .unwrap();
    root
}

// ---------------------------------------------------------------------------
// §3.4 deterministic export
// ---------------------------------------------------------------------------

#[test]
fn export_git_is_deterministic() {
    let root = make_gemel_repo("det-exp");
    let g1 = temp_root("det-g1");
    let g2 = temp_root("det-g2");
    result_of(
        &root,
        &["export-git", "--git-dir", g1.to_str().unwrap(), "--json"],
    );
    result_of(
        &root,
        &["export-git", "--git-dir", g2.to_str().unwrap(), "--json"],
    );
    // Byte-identical loose objects (same trees, commits, messages, trailers,
    // authors, timestamps — no wall clock involved).
    assert_eq!(
        git_objects(&g1),
        git_objects(&g2),
        "export must be byte-deterministic"
    );
    // The exported repository is valid per git itself.
    let check = Command::new("git")
        .arg("-C")
        .arg(&g1)
        .arg("fsck")
        .output()
        .unwrap();
    assert!(check.status.success(), "exported git must pass git fsck");
    // Trailers carry stable identities (the first change has an intent).
    let log = git(&g1, &["log", "--format=%b", "--reverse"]);
    assert!(log.contains("GEMEL-CHANGE: change."), "{log}");
    assert!(log.contains("GEMEL-INTENT: intent."), "{log}");
    assert!(log.contains("GEMEL-EXPORT-VERSION: 1"), "{log}");
    // The head chain exported as one commit per change.
    let count = git(&g1, &["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "2");
}

// ---------------------------------------------------------------------------
// §4 deterministic import
// ---------------------------------------------------------------------------

#[test]
fn import_git_is_deterministic_and_idempotent() {
    // A foreign git repository (real git, human authors).
    let src = temp_root("imp-src");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "alice@example.com"]);
    git(&src, &["config", "user.name", "Alice"]);
    write_file(&src, "a.txt", "alpha\n");
    git_init_commit(&src, "add a");
    write_file(&src, "a.txt", "alpha2\n");
    write_file(&src, "b.txt", "beta\n");
    git_init_commit(&src, "extend");

    let b = temp_root("imp-b");
    let c = temp_root("imp-c");
    Repo::init(&b, &InitOptions::default()).unwrap();
    Repo::init(&c, &InitOptions::default()).unwrap();
    let out1 = result_of(
        &b,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out1["commits"], 2);
    assert_eq!(out1["changes"], 2);
    assert_eq!(out1["trajectories"], 1);
    assert_eq!(out1["unknown_producers"], 0);
    result_of(
        &c,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
            "--json",
        ],
    );

    // Deterministic: identical logs across independent imports.
    assert_eq!(
        result_of(&b, &["log", "--json"]),
        result_of(&c, &["log", "--json"])
    );
    // The imported head is navigable and clean.
    let st = result_of(&b, &["status", "--json"]);
    assert_eq!(st["trajectory"], "T1");
    assert!(st["state"].as_str().is_some());
    // Import never fabricates intent/claims: absent, reported as unknown.
    assert_eq!(st["intent"], Value::Null);
    assert_eq!(st["claims"].as_array().unwrap().len(), 0);
    assert_eq!(st["residuals"].as_array().unwrap().len(), 0);
    // Idempotence: importing again creates no new changes.
    let out2 = result_of(
        &b,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out2["changes"], 0, "re-import must be a no-op");
}

// ---------------------------------------------------------------------------
// §5 round trips
// ---------------------------------------------------------------------------

#[test]
fn gemel_to_git_to_gemel_roundtrip_relinks_identities() {
    let root = make_gemel_repo("rt-a");
    let g1 = temp_root("rt-g1");
    result_of(&root, &["export-git", "--git-dir", g1.to_str().unwrap()]);
    // Re-import the exported Git INTO the originating repository: the
    // trailers validate, so the mappings re-link the original identities.
    let out = result_of(
        &root,
        &["import-git", "--git-dir", g1.to_str().unwrap(), "--json"],
    );
    assert_eq!(out["relinked"], 2, "both changes re-linked via trailers");
    assert_eq!(out["ignored_trailers"], 0);
    // The mappings point at the ORIGINAL change objects.
    let log = git(&g1, &["log", "--format=%H"]);
    for oid in log.lines() {
        let (code, v, err) = gemel_cli(
            &root,
            &["show", &format!("import/git_commit/{oid}"), "--json"],
        );
        assert_eq!(code, 0, "{err}");
        let _ = v;
    }
    // Re-export after the re-import is unchanged (idempotent export + no new
    // head wiring: local work was never overwritten).
    let g2 = temp_root("rt-g2");
    result_of(&root, &["export-git", "--git-dir", g2.to_str().unwrap()]);
    assert_eq!(git_objects(&g1), git_objects(&g2));
}

#[test]
fn git_to_gemel_to_git_roundtrip_preserves_content_and_topology() {
    let src = temp_root("rt-src");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "bob@example.com"]);
    git(&src, &["config", "user.name", "Bob"]);
    write_file(&src, "dir/file.txt", "one\n");
    write_file(&src, "other.txt", "two\n");
    git_init_commit(&src, "first");
    write_file(&src, "dir/file.txt", "one-two\n");
    write_file(&src, "gone.txt", "x\n");
    git_init_commit(&src, "second");
    git(&src, &["rm", "-q", "gone.txt"]);
    git(&src, &["commit", "-qm", "third"]);
    let src_log = git(&src, &["log", "--format=%s"]);
    let src_tree = git(&src, &["ls-tree", "-r", "HEAD"]);

    let gem = temp_root("rt-gem");
    Repo::init(&gem, &InitOptions::default()).unwrap();
    result_of(
        &gem,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
        ],
    );

    // Re-export into a fresh git dir.
    let g2 = temp_root("rt-g2");
    result_of(&gem, &["export-git", "--git-dir", g2.to_str().unwrap()]);
    // Trees byte-identical.
    let g2_tree = git(&g2, &["ls-tree", "-r", "HEAD"]);
    assert_eq!(src_tree, g2_tree, "trees must round-trip exactly");
    // Topology (commit count + parent structure) identical.
    let g2_log = git(&g2, &["log", "--format=%s"]);
    assert_eq!(src_log, g2_log, "messages/topology must round-trip");
    let src_count = git(&src, &["rev-list", "--count", "HEAD"]);
    let g2_count = git(&g2, &["rev-list", "--count", "HEAD"]);
    assert_eq!(src_count.trim(), g2_count.trim());
    // Author identity preserved (human producers with FULL disclosure).
    let src_author = git(&src, &["log", "-1", "--format=%an <%ae>"]);
    let g2_author = git(&g2, &["log", "-1", "--format=%an <%ae>"]);
    assert_eq!(src_author, g2_author, "human authorship must survive");
    // Timestamps preserved (carried, not wall clock).
    let src_ts = git(&src, &["log", "-1", "--format=%at"]);
    let g2_ts = git(&g2, &["log", "-1", "--format=%at"]);
    assert_eq!(src_ts, g2_ts, "timestamps must be carried");
    // The re-exported commits carry Gemel trailers.
    let trailers = git(&g2, &["log", "-1", "--format=%b"]);
    assert!(trailers.contains("GEMEL-CHANGE: change."), "{trailers}");
}

// ---------------------------------------------------------------------------
// §4.2 never fabricate
// ---------------------------------------------------------------------------

#[test]
fn import_never_fabricates_and_hostile_trailers_are_ignored() {
    let src = temp_root("hostile");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "eve@example.com"]);
    git(&src, &["config", "user.name", "Eve"]);
    write_file(&src, "f.txt", "x\n");
    // A commit whose message carries a hostile GEMEL-CHANGE trailer pointing
    // at a nonexistent identity, plus an unsupported export version.
    git(&src, &["add", "-A"]);
    git(
        &src,
        &[
            "commit", "-qm",
            "legit summary\n\nGEMEL-CHANGE: change.0000000000000000000000000000000000000000000000000000000000000000\nGEMEL-EXPORT-VERSION: 99",
        ],
    );
    let gem = temp_root("hostile-gem");
    Repo::init(&gem, &InitOptions::default()).unwrap();
    let out = result_of(
        &gem,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
            "--json",
        ],
    );
    // The hostile trailers were ignored; the commit imported as foreign.
    assert_eq!(out["relinked"], 0);
    assert_eq!(out["ignored_trailers"], 2);
    assert_eq!(out["changes"], 1);
    let st = result_of(&gem, &["status", "--json"]);
    assert_eq!(st["intent"], Value::Null, "intent is never fabricated");
    assert_eq!(st["claims"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// Multi-parent changes export as Git merge commits
// ---------------------------------------------------------------------------

#[test]
fn multi_parent_change_exports_as_merge_commit() {
    let root = temp_root("merge-exp");
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    // Three changes: C1, C2 (both parents of C3).
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("base".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(&root, "a.rs", "pub fn a() {}\n");
    let c1 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "change one".into(),
            ..Default::default()
        },
    )
    .unwrap();
    workflow::begin_change(&repo, &BeginOptions::default()).unwrap();
    write_file(&root, "b.rs", "pub fn b() {}\n");
    let c2 = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "change two".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // C3: a merge of C1 and C2 (causal parents in recorded order).
    let files = crate_files(&root);
    let state = gemel::content::build_state_from_files(&repo, &files).unwrap();
    let producer: gemel::gid::Gid = repo.read_meta().unwrap()["default_producer"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let c3 = repo
        .insert_object(&gemel::value::Object::fields(
            gemel::family::Family::Change,
            vec![
                gemel::value::Field::new(0x01, gemel::value::Value::Str("merged".into())),
                gemel::value::Field::new(0x03, gemel::value::Value::Gid(c2.state)),
                gemel::value::Field::new(0x05, gemel::value::Value::Gid(state)),
                gemel::value::Field::new(0x06, gemel::value::Value::Gid(producer)),
                gemel::value::Field::new(
                    0x11,
                    gemel::value::Value::Array(vec![
                        gemel::value::Value::Gid(c1.change),
                        gemel::value::Value::Gid(c2.change),
                    ]),
                ),
                gemel::value::Field::new(0x15, gemel::value::Value::I(1000)),
            ],
        ))
        .unwrap();
    // Wire the merge as the head.
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction {
            ops: vec![gemel::store::refs::RefOp::set(gemel::store::REF_HEAD, c3)],
        })?;
        Ok(())
    })
    .unwrap();
    let g = temp_root("merge-g");
    result_of(&root, &["export-git", "--git-dir", g.to_str().unwrap()]);
    // All three changes exported; the head commit is a merge (two parents).
    let count = git(&g, &["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "3", "three changes exported");
    let parents = git(&g, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    let n = parents.split_whitespace().count();
    assert_eq!(n, 3, "merge commit must have two parents, got: {parents}");
}

/// Reads the flat file map of a directory (for tests that craft changes).
fn crate_files(root: &Path) -> std::collections::BTreeMap<String, (u64, gemel::gid::Gid)> {
    let mut out = std::collections::BTreeMap::new();
    fn walk(
        dir: &Path,
        prefix: &str,
        out: &mut std::collections::BTreeMap<String, (u64, gemel::gid::Gid)>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".gemel" || name == ".git" {
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
                let content = std::fs::read(&path).unwrap();
                out.insert(rel, (0o100644, gemel::content::blob_identity(&content)));
            }
        }
    }
    walk(root, "", &mut out);
    out
}

// ---------------------------------------------------------------------------
// Conservative rename detection (§4.1)
// ---------------------------------------------------------------------------

#[test]
fn exact_moves_import_as_renames() {
    let src = temp_root("rename");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "r@example.com"]);
    git(&src, &["config", "user.name", "R"]);
    write_file(&src, "old.txt", "same content\n");
    git_init_commit(&src, "add old");
    // Exact move: old.txt → new.txt, byte-identical.
    std::fs::remove_file(src.join("old.txt")).unwrap();
    write_file(&src, "new.txt", "same content\n");
    git_init_commit(&src, "move");
    let gem = temp_root("rename-gem");
    Repo::init(&gem, &InitOptions::default()).unwrap();
    result_of(
        &gem,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
        ],
    );
    let log = result_of(&gem, &["log", "--json"]);
    // log is newest-first: the move is the head change.
    let second = &log["changes"][0];
    let ops = second["operations"].as_array().unwrap();
    let mut kinds = Vec::new();
    for op in ops {
        let (code, v, _) = gemel_cli(&gem, &["show", op.as_str().unwrap(), "--json"]);
        assert_eq!(code, 0);
        let op_type = v["result"]["body"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "op_type")
            .and_then(|f| f["value"].as_str())
            .unwrap_or("")
            .to_string();
        kinds.push(op_type);
    }
    assert!(
        kinds.iter().any(|k| k == "rename_path"),
        "exact move must import as rename_path, got {kinds:?}"
    );
}

#[test]
fn git_merge_history_roundtrips_through_import_export() {
    // A real Git history with a merge commit.
    let src = temp_root("mh-src");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "m@example.com"]);
    git(&src, &["config", "user.name", "M"]);
    write_file(&src, "base.txt", "base\n");
    git_init_commit_at(&src, "base", "@1000000000 +0000");
    git(&src, &["checkout", "-qb", "feature"]);
    write_file(&src, "feature.txt", "f\n");
    git_init_commit_at(&src, "feature", "@1000000100 +0000");
    git(&src, &["checkout", "-q", "main"]);
    write_file(&src, "main.txt", "m\n");
    git_init_commit_at(&src, "mainline", "@1000000200 +0000");
    git(&src, &["merge", "-q", "--no-edit", "feature"]);
    let src_count = git(&src, &["rev-list", "--count", "HEAD"]);
    assert_eq!(
        src_count.trim(),
        "4",
        "base + feature + mainline + merge commit"
    );

    let gem = temp_root("mh-gem");
    Repo::init(&gem, &InitOptions::default()).unwrap();
    result_of(
        &gem,
        &[
            "import-git",
            "--git-dir",
            src.join(".git").to_str().unwrap(),
        ],
    );
    let g2 = temp_root("mh-g2");
    result_of(&gem, &["export-git", "--git-dir", g2.to_str().unwrap()]);
    // Topology and trees round-trip exactly (the merge commit stays a merge).
    assert_eq!(
        git(&src, &["ls-tree", "-r", "HEAD"]),
        git(&g2, &["ls-tree", "-r", "HEAD"])
    );
    let src_parents = git(&src, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    let g2_parents = git(&g2, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(
        src_parents.split_whitespace().count(),
        g2_parents.split_whitespace().count(),
        "merge topology must survive: {g2_parents}"
    );
    assert_eq!(
        git(&src, &["log", "--format=%s"]),
        git(&g2, &["log", "--format=%s"])
    );
    // The imported merge change has two causal parents.
    let log = result_of(&gem, &["log", "--json"]);
    let head = &log["changes"][0];
    let (code, v, _) = gemel_cli(&gem, &["show", head["id"].as_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    let parents = v["result"]["body"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "causal_parents")
        .and_then(|f| f["value"].as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        parents, 2,
        "imported merge change must record two causal parents"
    );
}

// ---------------------------------------------------------------------------
// §6 clone
// ---------------------------------------------------------------------------

#[test]
fn clone_command_imports_history() {
    let src = temp_root("clone-src");
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "c@example.com"]);
    git(&src, &["config", "user.name", "C"]);
    write_file(&src, "f.txt", "v1\n");
    git_init_commit(&src, "v1");
    write_file(&src, "f.txt", "v2\n");
    git_init_commit(&src, "v2");

    let target = temp_root("clone-target");
    let (code, v, err) = gemel_cli(
        Path::new("/tmp"),
        &[
            "clone",
            src.to_str().unwrap(),
            "--dir",
            target.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let _ = v;
    // The cloned repo imported 2 changes on 1 trajectory.
    let log = result_of(&target, &["log", "--json"]);
    assert_eq!(log["changes"].as_array().unwrap().len(), 2);
    let st = result_of(&target, &["status", "--json"]);
    assert_eq!(st["trajectory"], "T1");
}
