//! Phase 1.5 integration courts (EXCHANGE.md §33, §43–§53, §59, §66).
//!
//! These courts use real Git repositories end to end (`git init` / `add` /
//! `commit` / `push` / `clone` / `clone --depth=1` / `merge`). Only files
//! carried by Git cross repository boundaries: no bytes from a repository's
//! native `.gemel/objects` are ever copied by the tests.
//!
//! Exit criteria proven here:
//!   - native identity round-trips through Git (transport court)
//!   - shallow clones recover the engineering frontier
//!   - branch merges union immutable artifacts without semantic byte merges
//!   - Git-only source changes fail closed (stale context, never fabricated)
//!   - every corruption class fails deterministically without panicking
//!   - idempotence: repeated status changes nothing
//!   - deterministic export across paths/machines
//!   - exchange non-interference: exchange files never change source State
//!   - Git status cleanliness after bootstrap
//!   - index independence: derived index is acceleration, never an oracle
//!   - incremental export reuses packs
//!   - golden fixtures pin descriptor/pack bytes and digests

#![allow(clippy::result_large_err)]

use gemel::store::{InitOptions, Repo};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-ex-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn git_config(root: &Path) {
    git(root, &["config", "user.email", "court@example.com"]);
    git(root, &["config", "user.name", "Gemel Court"]);
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

/// Runs the gemel binary with `--repo <root>`. Returns (exit code, parsed
/// JSON stdout, raw stderr).
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
    let parsed = if stdout.trim().is_empty() {
        Value::Null
    } else if stdout.trim_start().starts_with('{') {
        serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("gemel output is not JSON ({e}): {stdout}");
        })
    } else {
        Value::Null // human-readable output
    };
    (code, parsed, stderr)
}

/// The `result` block of a JSON command (fail on nonzero exit).
fn result_of(root: &Path, args: &[&str]) -> Value {
    let (code, v, err) = gemel_cli(root, args);
    assert_eq!(code, 0, "gemel {} failed: {err}", args.join(" "));
    v["result"].clone()
}

fn commit_all(root: &Path, msg: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", msg]);
}

/// A repository with one finished change (intent, claim, evidence, residual)
/// inside a real Git worktree; the change auto-exports a frontier which is
/// then committed.
fn make_exported_repo(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    git(&root, &["init", "-q", "-b", "main"]);
    git_config(&root);
    let (code, v, err) = gemel_cli(&root, &["init", "--json"]);
    assert_eq!(code, 0, "init failed: {err}");
    let _ = v;
    write_file(&root, "src.rs", "pub fn parse() {}\n");
    let (code, _, err) = gemel_cli(
        &root,
        &[
            "change",
            "begin",
            "--intent-summary",
            "Fix parser compatibility",
        ],
    );
    assert_eq!(code, 0, "begin failed: {err}");
    write_file(
        &root,
        "src.rs",
        "pub fn parse() { loop_detect(); }\nfn loop_detect() {}\n",
    );
    let (code, v, err) = gemel_cli(
        &root,
        &[
            "change",
            "finish",
            "--json",
            "--summary",
            "implemented pointer-loop detection",
            "--claim",
            "parser|accepts all valid RFC inputs|behavior",
            "--evidence",
            "parser|pass|test_result",
            "--residual",
            "FreeBSD divergence|high|platform_divergence",
        ],
    );
    assert_eq!(code, 0, "finish failed: {err}");
    assert_eq!(
        v["result"]["exchange"]["exported"], true,
        "change finish must auto-export inside a Git worktree (EXCHANGE.md §21)"
    );
    commit_all(&root, "implement parser fix");
    root
}

/// Clones `remote` into a fresh directory named `clone`.
fn clone_repo(remote: &Path, clone: &Path, extra: &[&str]) {
    let mut args: Vec<&str> = vec!["clone", "-q"];
    args.extend_from_slice(extra);
    let out = Command::new("git")
        .arg("-C")
        .arg(remote.parent().unwrap())
        .args(&args)
        .arg(remote)
        .arg(clone)
        .output()
        .expect("git clone");
    assert!(
        out.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The published exchange artifacts of a repository: relative path -> bytes
/// (frontier descriptors and packs only; temporaries are not artifacts).
fn exchange_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let base = root.join(".gemel").join("exchange");
    let mut out = BTreeMap::new();
    if base.is_dir() {
        walk_dir(&base, "", &mut out);
    }
    out
}

fn walk_dir(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, &rel, out);
        } else if path.is_file() && (name.ends_with(".gxf") || name.ends_with(".gxp")) {
            out.insert(rel, std::fs::read(&path).unwrap());
        }
    }
}

/// The number of native object files (`.gemel/objects/**/*.gce`).
fn object_count(root: &Path) -> usize {
    let base = root.join(".gemel").join("objects");
    let mut n = 0;
    if base.is_dir() {
        walk_count(&base, &mut n);
    }
    n
}

fn walk_count(dir: &Path, n: &mut usize) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            walk_count(&entry.path(), n);
        } else if entry.file_name().to_string_lossy().ends_with(".gce") {
            *n += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// §43 transport court + §67 acceptance demonstration
// ---------------------------------------------------------------------------

#[test]
fn git_carried_roundtrip_reconstructs_canonical_ids() {
    let repo_a = make_exported_repo("rt-a");
    let remote = temp_root("rt-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);

    let clone = temp_root("rt-clone");
    clone_repo(&remote, &clone, &[]);

    // The fresh machine restores the engineering context with one command.
    let st = result_of(&clone, &["status", "--json"]);
    assert_eq!(st["exchange"]["detected"], true);
    assert_eq!(st["exchange"]["source_match"], true);
    assert_eq!(st["exchange"]["bootstrapped"], true);
    let traj_b = st["trajectory"]
        .as_str()
        .expect("trajectory name")
        .to_string();
    let state_b = st["state"].as_str().expect("state id").to_string();
    let claims_b = st["claims"].clone();
    let residuals_b = st["residuals"].clone();
    assert_eq!(claims_b.as_array().unwrap().len(), 1, "claim must survive");
    assert_eq!(
        residuals_b.as_array().unwrap().len(),
        1,
        "residual must survive"
    );
    assert_eq!(
        st["readiness"].as_str().unwrap(),
        "READY_WITH_RESIDUALS",
        "readiness reflects the open residual"
    );

    // Same canonical ids as the original repository.
    let st_a = result_of(&repo_a, &["status", "--json"]);
    assert_eq!(st_a["trajectory"].as_str().unwrap(), traj_b);
    assert_eq!(st_a["state"].as_str().unwrap(), state_b);
    assert_eq!(st_a["claims"], claims_b);
    assert_eq!(st_a["residuals"], residuals_b);

    // Names were restored as local labels over the immutable identities.
    let log_b = result_of(&clone, &["log", "--json"]);
    let log_a = result_of(&repo_a, &["log", "--json"]);
    assert_eq!(log_b, log_a, "imported log equals original log");

    // show works on imported objects; intent is queryable by name.
    let (code, v, _) = gemel_cli(&clone, &["show", &state_b, "--json"]);
    assert_eq!(code, 0, "show of imported state failed: {v}");
    let intents = result_of(&clone, &["claims", "--json"]);
    assert_eq!(intents["claims"].as_array().unwrap().len(), 1);

    // Negative knowledge survived transport: the open residual is visible.
    let res = result_of(&clone, &["residuals", "--json"]);
    let list = res["residuals"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0]["summary"].as_str().unwrap().contains("FreeBSD"));
}

// ---------------------------------------------------------------------------
// §44 shallow clone court
// ---------------------------------------------------------------------------

#[test]
fn shallow_clone_recovers_frontier() {
    let repo_a = make_exported_repo("shallow-a");
    let remote = temp_root("shallow-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);

    let clone = temp_root("shallow-clone");
    clone_repo(&remote, &clone, &["--depth=1"]);
    // Prove the clone really is shallow (one commit of history).
    let depth = git(&clone, &["rev-list", "--count", "HEAD"]);
    assert_eq!(depth.trim(), "1");

    let st = result_of(&clone, &["status", "--json"]);
    assert_eq!(st["exchange"]["detected"], true);
    assert_eq!(st["exchange"]["source_match"], true);
    assert_eq!(st["trajectory"].as_str(), Some("T1"));
    assert_eq!(st["claims"].as_array().unwrap().len(), 1);
    assert_eq!(st["residuals"].as_array().unwrap().len(), 1);
    assert_eq!(
        st["readiness"].as_str().unwrap(),
        "READY_WITH_RESIDUALS",
        "readiness reflects the open residual"
    );

    // Same state identity as the original.
    let st_a = result_of(&repo_a, &["status", "--json"]);
    assert_eq!(
        st["state"].as_str().unwrap(),
        st_a["state"].as_str().unwrap()
    );
}

// ---------------------------------------------------------------------------
// §46 Git-only mutation court
// ---------------------------------------------------------------------------

#[test]
fn git_only_source_change_marks_context_stale() {
    let repo_a = make_exported_repo("stale-a");
    // A developer edits source with Git only (no Gemel exchange update).
    write_file(&repo_a, "src.rs", "pub fn parse() { GIT_ONLY_CHANGE; }\n");
    commit_all(&repo_a, "git-only source edit");
    let remote = temp_root("stale-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);

    let clone = temp_root("stale-clone");
    clone_repo(&remote, &clone, &[]);

    let st = result_of(&clone, &["status", "--json"]);
    // The old context imported successfully but is provably stale.
    assert_eq!(st["exchange"]["detected"], true);
    assert_eq!(st["exchange"]["source_match"], false);
    // No activation: no frontier describes the current source.
    assert_eq!(st["trajectory"], Value::Null);
    // Old claims are NOT presented as current truth.
    assert_eq!(st["claims"].as_array().unwrap().len(), 0);
    // Readiness must not claim ready.
    assert_ne!(st["readiness"].as_str().unwrap(), "READY");

    // The historical context remains queryable with an explicit mismatch.
    let es = result_of(&clone, &["exchange", "status", "--json"]);
    assert_eq!(es["detected"], true);
    let frontiers = es["frontiers"].as_array().unwrap();
    assert_eq!(frontiers.len(), 1);
    assert_eq!(frontiers[0]["binding"], "diverged");
    assert_eq!(es["pending_export"], true);
    // The imported frontier head change is still reachable by gid.
    let head = frontiers[0]["head_change"].as_str().unwrap();
    let (code, v, _) = gemel_cli(&clone, &["show", head, "--json"]);
    assert_eq!(
        code, 0,
        "imported historical change must remain readable: {v}"
    );
}

/// The §67 final act: a frontier that matched at clone time must flip to
/// STALE the moment the source diverges through Git only — readiness and the
/// current-context claim must carry the mismatch.
#[test]
fn activated_context_becomes_stale_after_git_only_edit() {
    let repo_a = make_exported_repo("act-stale-a");
    let remote = temp_root("act-stale-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("act-stale-clone");
    clone_repo(&remote, &clone, &[]);

    // At clone time the frontier matches and the context activates.
    let st = result_of(&clone, &["status", "--json"]);
    assert_eq!(st["exchange"]["source_match"], true);
    assert_eq!(st["exchange"].get("context"), None);
    assert_eq!(st["trajectory"], "T1");

    // A developer edits source with Git only (no Gemel exchange update).
    write_file(&clone, "src.rs", "pub fn parse() { GIT_ONLY_CHANGE; }\n");
    commit_all(&clone, "git-only edit");

    // The imported context is now provably historical: status carries the
    // mismatch instead of pretending the old evidence verifies the new
    // source.
    let st = result_of(&clone, &["status", "--json"]);
    assert_eq!(st["exchange"]["detected"], true);
    assert_eq!(st["exchange"]["source_match"], false);
    assert_eq!(st["exchange"]["context"], "STALE");
    assert_eq!(st["readiness"], "NOT_READY");
    // The imported base remains the head (a later `change begin` would diff
    // the git-only edit into a recorded change), but it is not presented as
    // verifying the current source.
    assert_eq!(st["state"].as_str().is_some(), true);
}

// ---------------------------------------------------------------------------
// §45 branch merge court
// ---------------------------------------------------------------------------

#[test]
fn branch_merge_unions_immutable_artifacts() {
    let root = temp_root("merge-root");
    git(&root, &["init", "-q", "-b", "main"]);
    git_config(&root);
    let (code, _, err) = gemel_cli(&root, &["init"]);
    assert_eq!(code, 0, "init failed: {err}");
    write_file(&root, "base.rs", "pub fn base() {}\n");
    commit_all(&root, "base");

    // Branch A: its own trajectory + change + export.
    git(&root, &["checkout", "-q", "-b", "branch-a"]);
    let (code, _, err) = gemel_cli(&root, &["change", "begin", "--intent-summary", "Feature A"]);
    assert_eq!(code, 0, "{err}");
    write_file(&root, "a.rs", "pub fn a() {}\n");
    let (code, v, err) = gemel_cli(
        &root,
        &[
            "change",
            "finish",
            "--json",
            "--summary",
            "feature A",
            "--claim",
            "a.rs|A works|behavior",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["result"]["exchange"]["exported"], true);
    commit_all(&root, "feature A");

    // Branch B from main: independent trajectory + change + export.
    git(&root, &["checkout", "-q", "main"]);
    git(&root, &["checkout", "-q", "-b", "branch-b"]);
    let (code, _, err) = gemel_cli(&root, &["change", "begin", "--intent-summary", "Feature B"]);
    assert_eq!(code, 0, "{err}");
    write_file(&root, "b.rs", "pub fn b() {}\n");
    let (code, v, err) = gemel_cli(
        &root,
        &[
            "change",
            "finish",
            "--json",
            "--summary",
            "feature B",
            "--claim",
            "b.rs|B works|behavior",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["result"]["exchange"]["exported"], true);
    commit_all(&root, "feature B");

    // Merge both into main: immutable exchange artifacts union cleanly.
    git(&root, &["checkout", "-q", "main"]);
    git(&root, &["merge", "-q", "--no-edit", "branch-a"]);
    git(&root, &["merge", "-q", "--no-edit", "branch-b"]);

    let remote = temp_root("merge-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&root, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("merge-clone");
    clone_repo(&remote, &clone, &[]);

    // Both historical frontiers are importable; neither claims the merged
    // source, and Gemel says so explicitly. (`gemel status` performs the
    // automatic import; `exchange status` is read-only.)
    let st = result_of(&clone, &["status", "--json"]);
    assert_eq!(st["exchange"]["source_match"], false);
    assert_eq!(st["trajectory"], Value::Null);
    let es = result_of(&clone, &["exchange", "status", "--json"]);
    assert_eq!(es["detected"], true);
    let frontiers = es["frontiers"].as_array().unwrap();
    assert_eq!(frontiers.len(), 2, "both branch frontiers must be present");
    for f in frontiers {
        assert_eq!(f["binding"], "diverged");
        assert_eq!(f["imported"], true, "both knowledge sets imported");
    }
    // Both head changes remain individually readable (union of knowledge).
    for f in frontiers {
        let head = f["head_change"].as_str().unwrap();
        let (code, _, _) = gemel_cli(&clone, &["show", head, "--json"]);
        assert_eq!(code, 0);
    }
}

// ---------------------------------------------------------------------------
// §47 corruption courts
// ---------------------------------------------------------------------------

/// Builds a fresh clone of a valid exported repository (no native store yet).
fn fresh_clone_of_exported(tag: &str) -> PathBuf {
    let repo_a = make_exported_repo(tag);
    let remote = temp_root(&format!("{tag}-remote"));
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root(&format!("{tag}-clone"));
    clone_repo(&remote, &clone, &[]);
    clone
}

/// A corrupted clone must make `gemel exchange verify` fail deterministically
/// with a nonzero exit and no panic.
fn assert_verify_fails(clone: &Path, what: &str) {
    let (code, v, err) = gemel_cli(clone, &["exchange", "verify", "--json"]);
    assert_ne!(code, 0, "{what}: verify must fail");
    assert!(
        !err.contains("panic"),
        "{what}: no panic, got stderr: {err}"
    );
    let _ = v;
}

fn one_pack_path(clone: &Path) -> PathBuf {
    let packs = clone.join(".gemel/exchange/v1/packs");
    let mut found = None;
    for shard in std::fs::read_dir(&packs).unwrap() {
        let shard = shard.unwrap();
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".gxp") {
                found = Some(entry.path());
            }
        }
    }
    found.expect("at least one pack")
}

fn one_frontier_path(clone: &Path) -> PathBuf {
    let frontiers = clone.join(".gemel/exchange/v1/frontiers");
    let mut found = None;
    for shard in std::fs::read_dir(&frontiers).unwrap() {
        let shard = shard.unwrap();
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".gxf") {
                found = Some(entry.path());
            }
        }
    }
    found.expect("at least one frontier")
}

#[test]
fn corruption_modified_pack_byte_fails_verify() {
    let clone = fresh_clone_of_exported("corrupt-byte");
    let path = one_pack_path(&clone);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip a byte in the middle of the pack body.
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x5A;
    std::fs::write(&path, bytes).unwrap();
    assert_verify_fails(&clone, "modified pack byte");
}

#[test]
fn corruption_truncated_pack_fails_verify() {
    let clone = fresh_clone_of_exported("corrupt-trunc");
    let path = one_pack_path(&clone);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert_verify_fails(&clone, "truncated pack");
}

#[test]
fn corruption_missing_referenced_pack_fails_verify() {
    let clone = fresh_clone_of_exported("corrupt-missing");
    std::fs::remove_file(one_pack_path(&clone)).unwrap();
    assert_verify_fails(&clone, "missing referenced pack");
}

#[test]
fn corruption_renamed_pack_fails_verify() {
    let clone = fresh_clone_of_exported("corrupt-rename");
    let path = one_pack_path(&clone);
    // Renaming to a content-addressed name that does not match the bytes
    // makes the pack unreachable at its advertised identity.
    let renamed =
        path.with_file_name("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef.gxp");
    std::fs::rename(&path, &renamed).unwrap();
    assert_verify_fails(&clone, "renamed pack (filename/hash mismatch)");
}

#[test]
fn corruption_malformed_frontier_is_skipped_or_rejected() {
    let clone = fresh_clone_of_exported("corrupt-frontier");
    let path = one_frontier_path(&clone);
    std::fs::write(&path, b"{ not valid json !!!").unwrap();
    // The descriptor fails identity verification, so no valid frontier
    // remains; verify must fail closed (never "activate" garbage).
    assert_verify_fails(&clone, "malformed frontier");
}

#[test]
fn corruption_partial_publication_fails_verify() {
    let clone = fresh_clone_of_exported("corrupt-partial");
    let path = one_pack_path(&clone);
    // A half-written pack: tmp file present, final file absent.
    std::fs::remove_file(&path).unwrap();
    write_file(&clone, "x.tmp", "partial");
    assert_verify_fails(&clone, "partial publication");
}

#[test]
fn corruption_unsupported_mandatory_schema_fails_ingest() {
    // Craft a frontier that requires an unsupported schema version and prove
    // both ingest and verify reject it structurally.
    let clone = fresh_clone_of_exported("corrupt-schema");
    // Bootstrap a native store over the exchange material (Repo::init
    // completes an exchange-only .gemel without clobbering it).
    let repo = Repo::init(&clone, &InitOptions::default()).unwrap();
    let frontier_path = one_frontier_path(&clone);
    let bytes = std::fs::read(&frontier_path).unwrap();
    let mut f: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    f["required_schemas"] = serde_json::json!([2]);
    let new_bytes = serde_json::to_vec(&f).unwrap();
    // The id changes with the bytes; the old file no longer matches its
    // advertised identity, so discover skips it. Instead, publish it at its
    // correct new identity so ingest encounters an unsupported schema.
    let id = gemel::exchange::frontier_id(&new_bytes);
    let path = gemel::exchange::frontier_path(&clone.join(".gemel"), &id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &new_bytes).unwrap();
    let out = gemel::exchange::ingest::ingest(&repo);
    let msg = format!("{out:?}");
    assert!(
        out.is_err(),
        "unsupported mandatory schema must fail ingest, got {msg}"
    );
    assert!(msg.contains("unsupported schema version"), "{msg}");
}

#[test]
fn corruption_conflicting_object_identity_is_fatal() {
    // §42: the same ObjectId with different bytes is a fatal integrity
    // violation — never "pick one". Within valid packs a duplicate id
    // implies identical bytes (id = BLAKE3(envelope)), so the conflict only
    // arises against a local object file whose bytes were corrupted.
    let (repo, root) = {
        let r = temp_root("corrupt-dup");
        let repo = Repo::init(&r, &InitOptions::default()).unwrap();
        (repo, r)
    };
    // A canonical object, then a pack carrying its correct envelope.
    let b1 = gemel::value::Object::blob(b"alpha".to_vec());
    let id1 = repo.insert_object(&b1).unwrap();
    let env1 = repo.read_bytes(&id1).unwrap();
    // Corrupt the object file on disk: the path keeps its name (the id) but
    // the bytes no longer hash to it.
    let mut path = root.join(".gemel/objects");
    let hex = gemel::hex::encode(id1.digest());
    path = path.join(&hex[0..2]).join(format!("{hex}.gce"));
    let mut corrupt = env1.clone();
    let mid = corrupt.len() / 2;
    corrupt[mid] ^= 0xFF;
    std::fs::write(&path, &corrupt).unwrap();

    let meta = root.join(".gemel");
    let (pack, pid) = gemel::exchange::encode_pack(&[gemel::exchange::PackObject {
        id: id1,
        envelope: env1,
    }])
    .unwrap();
    let p1_path = gemel::exchange::pack_path(&meta, &pid);
    std::fs::create_dir_all(p1_path.parent().unwrap()).unwrap();
    std::fs::write(p1_path, &pack).unwrap();
    let f = gemel::exchange::Frontier {
        schema: gemel::exchange::FRONTIER_SCHEMA.into(),
        source_state: gemel::gid::Gid::new(gemel::family::Family::State, [0u8; 32]),
        head_change: gemel::gid::Gid::new(gemel::family::Family::Change, [0u8; 32]),
        trajectory: None,
        intent: None,
        parent_frontiers: vec![],
        packs: vec![gemel::hex::encode(&pid)],
        profile: "frontier".into(),
        coverage: gemel::exchange::Coverage::default(),
        required_schemas: vec![1],
    };
    let fbytes = gemel::exchange::encode_frontier(&f).unwrap();
    let fid = gemel::exchange::frontier_id(&fbytes);
    let fpath = gemel::exchange::frontier_path(&meta, &fid);
    std::fs::create_dir_all(fpath.parent().unwrap()).unwrap();
    std::fs::write(fpath, &fbytes).unwrap();
    // Ingest must fail loudly on the identity conflict, never pick a side.
    let err = gemel::exchange::ingest::ingest(&repo).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("collision") || msg.to_lowercase().contains("conflict"),
        "{msg}"
    );
}

#[test]
fn corruption_object_id_body_mismatch_is_rejected() {
    // Hand-craft a pack whose advertised object id does not match its body.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"GXPK");
    bytes.push(1);
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.push(gemel::family::Family::Blob.code());
    bytes.extend_from_slice(&[0xAB; 32]); // wrong digest on purpose
    bytes.extend_from_slice(&5u64.to_le_bytes());
    bytes.extend_from_slice(b"hello");
    bytes.extend_from_slice(b"GXPK-END");
    let err = gemel::exchange::decode_pack(&bytes, &gemel::exchange::ExchangeLimits::default())
        .unwrap_err();
    assert!(format!("{err}").contains("id/body mismatch"));
}

#[test]
fn corruption_excessive_object_length_is_rejected() {
    // A pack whose object length field exceeds the available body.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"GXPK");
    bytes.push(1);
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.push(gemel::family::Family::Blob.code());
    bytes.extend_from_slice(&[0xAB; 32]);
    bytes.extend_from_slice(&u64::MAX.to_le_bytes()); // absurd length
    bytes.extend_from_slice(b"tiny");
    bytes.extend_from_slice(b"GXPK-END");
    let err = gemel::exchange::decode_pack(&bytes, &gemel::exchange::ExchangeLimits::default())
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("truncated") || msg.contains("overflow"),
        "{msg}"
    );
}

#[test]
fn corruption_pack_size_limit_is_enforced() {
    let (repo, _root) = {
        let r = temp_root("corrupt-limit");
        let repo = Repo::init(&r, &InitOptions::default()).unwrap();
        (repo, r)
    };
    let b = gemel::value::Object::blob(b"x".to_vec());
    let id = repo.insert_object(&b).unwrap();
    let (pack, _) = gemel::exchange::encode_pack(&[gemel::exchange::PackObject {
        id,
        envelope: repo.read_bytes(&id).unwrap(),
    }])
    .unwrap();
    let limits = gemel::exchange::ExchangeLimits {
        max_pack_bytes: 10, // far smaller than the pack
        ..gemel::exchange::ExchangeLimits::default()
    };
    let err = gemel::exchange::decode_pack(&pack, &limits).unwrap_err();
    assert!(format!("{err}").contains("exchange pack"), "{err:?}");
}

#[test]
fn corruption_illegal_symlink_is_ignored_not_followed() {
    #[cfg(unix)]
    {
        let clone = fresh_clone_of_exported("corrupt-link");
        let frontiers = clone.join(".gemel/exchange/v1/frontiers");
        let shard = frontiers.join("zz");
        std::fs::create_dir_all(&shard).unwrap();
        // A symlink that looks like a frontier name pointing at an arbitrary
        // file; discovery must never follow it.
        std::fs::write(clone.join("victim.txt"), "not a frontier").unwrap();
        std::os::unix::fs::symlink(
            clone.join("victim.txt"),
            shard.join("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.gxf"),
        )
        .unwrap();
        let meta = clone.join(".gemel");
        let found = gemel::exchange::discover_frontiers(&meta).unwrap();
        assert_eq!(found.len(), 1, "symlinked frontier must be ignored");
        // Same for packs: a symlinked pack is rejected on read.
        let pack = one_pack_path(&clone);
        let bytes = std::fs::read(&pack).unwrap();
        let pid = gemel::exchange::pack_id(&bytes);
        let target = gemel::exchange::pack_path(&meta, &pid);
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&pack, &target).unwrap();
        let err = gemel::exchange::read_pack_file(&target).unwrap_err();
        assert!(format!("{err}").contains("regular file"));
    }
}

#[test]
fn corruption_malformed_hex_path_is_ignored() {
    let clone = fresh_clone_of_exported("corrupt-hex");
    let frontiers = clone.join(".gemel/exchange/v1/frontiers");
    let shard = frontiers.join("zz");
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz.gxf"),
        b"{}",
    )
    .unwrap();
    let meta = clone.join(".gemel");
    let found = gemel::exchange::discover_frontiers(&meta).unwrap();
    assert_eq!(found.len(), 1, "malformed-hex shard must be ignored");
}

#[test]
fn corruption_reference_fanout_depth_is_bounded() {
    // A malicious chain of parent frontiers deeper than the protocol limit
    // must fail with a structured limit error, not a stack overflow.
    let (repo, root) = {
        let r = temp_root("corrupt-depth");
        let repo = Repo::init(&r, &InitOptions::default()).unwrap();
        (repo, r)
    };
    let meta = root.join(".gemel");
    let depth_limit = gemel::exchange::ExchangeLimits::default().max_reference_depth;
    // Chain: F_n references F_{n-1} as parent.
    // Pre-mark every frontier except the deepest as locally imported so the
    // ingest pass processes exactly one deep chain (the property under test
    // is the bounded depth, not the O(n²) full-closure walk).
    let imported_dir = meta.join("exchange-state").join("imported");
    std::fs::create_dir_all(&imported_dir).unwrap();
    let mut chain: Vec<[u8; 32]> = Vec::new();
    let mut prev: Option<String> = None;
    let n = depth_limit + 5;
    for _ in 0..n {
        let f = gemel::exchange::Frontier {
            schema: gemel::exchange::FRONTIER_SCHEMA.into(),
            source_state: gemel::gid::Gid::new(gemel::family::Family::State, [0u8; 32]),
            head_change: gemel::gid::Gid::new(gemel::family::Family::Change, [0u8; 32]),
            trajectory: None,
            intent: None,
            parent_frontiers: prev.into_iter().collect(),
            packs: vec![],
            profile: "frontier".into(),
            coverage: gemel::exchange::Coverage::default(),
            required_schemas: vec![1],
        };
        let bytes = gemel::exchange::encode_frontier(&f).unwrap();
        let fid = gemel::exchange::frontier_id(&bytes);
        let path = gemel::exchange::frontier_path(&meta, &fid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &bytes).unwrap();
        prev = Some(gemel::hex::encode(&fid));
        chain.push(fid);
    }
    let deepest = chain.last().copied().unwrap();
    for fid in chain {
        if fid != deepest {
            std::fs::write(imported_dir.join(gemel::hex::encode(&fid)), b"imported\n").unwrap();
        }
    }
    let err = gemel::exchange::ingest::ingest(&repo).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("parent frontier depth"), "{msg}");
    let _ = deepest;
}

#[test]
fn corruption_automatic_ingest_byte_limit_is_enforced() {
    let (repo, root) = {
        let r = temp_root("corrupt-bytes");
        let repo = Repo::init(&r, &InitOptions::default()).unwrap();
        (repo, r)
    };
    let b = gemel::value::Object::blob(vec![b'x'; 4096]);
    let id = repo.insert_object(&b).unwrap();
    let (pack, pid) = gemel::exchange::encode_pack(&[gemel::exchange::PackObject {
        id,
        envelope: repo.read_bytes(&id).unwrap(),
    }])
    .unwrap();
    let meta = root.join(".gemel");
    let path = gemel::exchange::pack_path(&meta, &pid);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &pack).unwrap();
    let f = gemel::exchange::Frontier {
        schema: gemel::exchange::FRONTIER_SCHEMA.into(),
        source_state: gemel::gid::Gid::new(gemel::family::Family::State, [0u8; 32]),
        head_change: gemel::gid::Gid::new(gemel::family::Family::Change, [0u8; 32]),
        trajectory: None,
        intent: None,
        parent_frontiers: vec![],
        packs: vec![gemel::hex::encode(&pid)],
        profile: "frontier".into(),
        coverage: gemel::exchange::Coverage::default(),
        required_schemas: vec![1],
    };
    let fbytes = gemel::exchange::encode_frontier(&f).unwrap();
    let fid = gemel::exchange::frontier_id(&fbytes);
    let fpath = gemel::exchange::frontier_path(&meta, &fid);
    std::fs::create_dir_all(fpath.parent().unwrap()).unwrap();
    std::fs::write(fpath, &fbytes).unwrap();
    let err = gemel::exchange::ingest::ingest_with_limits(
        &repo,
        &gemel::exchange::ExchangeLimits {
            // A deliberately tiny automatic-ingest budget: the pack alone
            // exceeds it, so the frontier cannot force unbounded ingestion.
            max_automatic_ingest_bytes: 512,
            ..gemel::exchange::ExchangeLimits::default()
        },
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("IMPORT_REQUIRES_EXPLICIT_APPROVAL"), "{msg}");
}

// ---------------------------------------------------------------------------
// §48 idempotence court
// ---------------------------------------------------------------------------

#[test]
fn repeated_status_is_idempotent() {
    let repo_a = make_exported_repo("idem-a");
    let remote = temp_root("idem-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("idem-clone");
    clone_repo(&remote, &clone, &[]);

    // Warm-up: the first call bootstraps the native store (a one-time
    // transition, not semantic state). Semantic results are compared after.
    let _ = result_of(&clone, &["status", "--json"]);
    let first = result_of(&clone, &["status", "--json"]);
    assert_eq!(first["exchange"]["bootstrapped"], false);
    let objects_before = object_count(&clone);
    let exchange_before = exchange_files(&clone);
    let state_before = {
        // content hash of the working tree source (excluding .gemel)
        let mut h = blake3::Hasher::new();
        for (p, bytes) in exchange_files(&clone) {
            let _ = p;
            h.update(&bytes);
        }
        h.finalize().to_hex().to_string()
    };
    let _ = state_before;
    for _ in 0..4 {
        let again = result_of(&clone, &["status", "--json"]);
        assert_eq!(again, first, "status must be semantically identical");
    }
    assert_eq!(
        object_count(&clone),
        objects_before,
        "no new native objects"
    );
    assert_eq!(
        exchange_files(&clone),
        exchange_before,
        "no exchange files rewritten"
    );
}

// ---------------------------------------------------------------------------
// §49 deterministic export court
// ---------------------------------------------------------------------------

#[test]
fn export_is_deterministic_across_machines_and_paths() {
    let repo_a = make_exported_repo("det-a");
    let remote = temp_root("det-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);

    // Two clones at different paths ingest the same canonical objects.
    let clone1 = temp_root("det-clone1");
    clone_repo(&remote, &clone1, &[]);
    let clone2 = temp_root("det-clone2");
    clone_repo(&remote, &clone2, &[]);
    result_of(&clone1, &["status", "--json"]);
    result_of(&clone2, &["status", "--json"]);

    // Re-export from each: the frontier and packs must be byte-identical to
    // the original export and to each other (pure function of canonical
    // state — path, pid, and creation order cannot matter).
    result_of(&clone1, &["exchange", "export", "--json"]);
    result_of(&clone2, &["exchange", "export", "--json"]);
    let a = exchange_files(&repo_a);
    let b1 = exchange_files(&clone1);
    let b2 = exchange_files(&clone2);
    assert_eq!(a, b1, "clone1 export must equal original export");
    assert_eq!(b1, b2, "exports across different paths must be identical");

    // Also identical for the portable profile.
    result_of(
        &repo_a,
        &["exchange", "export", "--profile", "portable", "--json"],
    );
    result_of(
        &clone1,
        &["exchange", "export", "--profile", "portable", "--json"],
    );
    let a_p = exchange_files(&repo_a);
    let b_p = exchange_files(&clone1);
    assert_eq!(a_p, b_p, "portable exports must be identical");
}

// ---------------------------------------------------------------------------
// §50 exchange non-interference court
// ---------------------------------------------------------------------------

#[test]
fn exchange_files_never_change_source_state() {
    let root = make_exported_repo("ni-a");
    let repo = Repo::open(&root).unwrap();
    let before = gemel::exchange::export::content_state_identity(
        &repo,
        &gemel::exchange::export::working_tree_files(&repo).unwrap(),
    )
    .unwrap();
    // Add more exchange material: a portable export plus a copied frontier.
    result_of(
        &root,
        &["exchange", "export", "--profile", "portable", "--json"],
    );
    let after = gemel::exchange::export::content_state_identity(
        &repo,
        &gemel::exchange::export::working_tree_files(&repo).unwrap(),
    )
    .unwrap();
    assert_eq!(
        before, after,
        "changing .gemel/exchange/** must never change the source State"
    );

    // A captured snapshot contains no .gemel entries at all.
    let snap = result_of(&root, &["snapshot", "--json"]);
    let state = snap["state"].as_str().unwrap();
    let files = gemel::content::state_files(&repo, &state.parse().unwrap()).unwrap();
    assert!(
        files.keys().all(|p| !p.starts_with(".gemel")),
        "captured state must never contain exchange metadata"
    );
}

// ---------------------------------------------------------------------------
// §51 Git status cleanliness court
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_keeps_git_status_clean() {
    let repo_a = make_exported_repo("clean-a");
    let remote = temp_root("clean-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("clean-clone");
    clone_repo(&remote, &clone, &[]);

    // After bootstrap, git must not report the native store as untracked.
    result_of(&clone, &["status", "--json"]);
    let porcelain = git(&clone, &["status", "--porcelain"]);
    assert_eq!(
        porcelain.trim(),
        "",
        "git status must stay clean after bootstrap, got:\n{porcelain}"
    );
    // The tracked file list contains the exchange material but no native
    // store files.
    let tracked = git(&clone, &["ls-files"]);
    assert!(tracked.contains(".gemel/exchange/v1/"), "{tracked}");
    assert!(tracked.contains(".gemel/.gitignore"), "{tracked}");
    assert!(!tracked.contains(".gemel/objects"), "{tracked}");
    assert!(!tracked.contains(".gemel/index"), "{tracked}");
    assert!(!tracked.contains(".gemel/refs"), "{tracked}");
}

// ---------------------------------------------------------------------------
// §40 index independence court
// ---------------------------------------------------------------------------

#[test]
fn deleting_the_derived_index_never_changes_answers() {
    let repo_a = make_exported_repo("idx-a");
    let remote = temp_root("idx-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("idx-clone");
    clone_repo(&remote, &clone, &[]);

    let queries = |root: &Path| -> Value {
        serde_json::json!({
            "status": result_of(root, &["status", "--json"]),
            "log": result_of(root, &["log", "--json"]),
            "claims": result_of(root, &["claims", "--json"]),
            "residuals": result_of(root, &["residuals", "--json"]),
            "evidence": result_of(root, &["evidence", "--subject", "src.rs", "--json"]),
            "why": result_of(root, &["why", "src.rs", "--json"]),
        })
    };

    // Warm-up: the first status call bootstraps the native store (a one-time
    // transition marker, not semantic state).
    let _ = result_of(&clone, &["status", "--json"]);
    let with_index = queries(&clone);
    let db = clone.join(".gemel/index/gemel.db");
    assert!(db.exists(), "index must exist after ingestion");
    std::fs::remove_file(&db).unwrap();
    let without_index = queries(&clone);
    assert_eq!(
        with_index, without_index,
        "deleting the derived index must not change semantic answers"
    );

    // Rebuilding restores the accelerator with identical answers.
    {
        let repo = Repo::open(&clone).unwrap();
        repo.rebuild_index().unwrap();
    }
    let rebuilt = queries(&clone);
    assert_eq!(with_index, rebuilt);
}

// ---------------------------------------------------------------------------
// §52 incremental export court
// ---------------------------------------------------------------------------

#[test]
fn incremental_export_reuses_packs() {
    let root = temp_root("incr");
    git(&root, &["init", "-q", "-b", "main"]);
    git_config(&root);
    gemel_cli(&root, &["init"]);
    write_file(&root, "src.rs", "pub fn a() {}\n");
    gemel_cli(&root, &["change", "begin", "--intent-summary", "First"]);
    write_file(&root, "src.rs", "pub fn a() { b(); }\nfn b() {}\n");
    gemel_cli(
        &root,
        &[
            "change",
            "finish",
            "--json",
            "--summary",
            "first change",
            "--claim",
            "src.rs|works|behavior",
        ],
    );
    commit_all(&root, "first");
    let packs_1 = result_of(&root, &["exchange", "export", "--json"]);
    let pack_count_1 = exchange_files(&root)
        .values()
        .filter(|b| b.starts_with(b"GXPK"))
        .count();

    // A second change must not regenerate the first change's packs.
    gemel_cli(&root, &["change", "begin", "--intent-summary", "Second"]);
    write_file(
        &root,
        "src.rs",
        "pub fn a() { b(); c(); }\nfn b() {}\nfn c() {}\n",
    );
    let (code, v, err) = gemel_cli(
        &root,
        &[
            "change",
            "finish",
            "--json",
            "--summary",
            "second change",
            "--claim",
            "src.rs|still works|behavior",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(v["result"]["exchange"]["exported"], true);
    let out = result_of(&root, &["exchange", "export", "--json"]);
    assert!(
        out["packs_reused"].as_u64().unwrap() >= pack_count_1 as u64,
        "existing packs must be reused: {out}"
    );
    let _ = (packs_1, out);
}

// ---------------------------------------------------------------------------
// §59 golden fixtures
// ---------------------------------------------------------------------------

#[test]
fn golden_frontier_descriptor_bytes_and_digest() {
    // A full frontier descriptor with every field populated. Fixed ids, no
    // timestamps, no hostnames, no git commit ids (EXCHANGE.md §8).
    let f = gemel::exchange::Frontier {
        schema: gemel::exchange::FRONTIER_SCHEMA.into(),
        source_state: "state.0000000000000000000000000000000000000000000000000000000000000001"
            .parse()
            .unwrap(),
        head_change: "change.0000000000000000000000000000000000000000000000000000000000000002"
            .parse()
            .unwrap(),
        trajectory: Some(
            "trajectory.0000000000000000000000000000000000000000000000000000000000000003"
                .parse()
                .unwrap(),
        ),
        intent: Some(
            "intent.0000000000000000000000000000000000000000000000000000000000000004"
                .parse()
                .unwrap(),
        ),
        parent_frontiers: vec![
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ],
        packs: vec![
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into(),
        ],
        profile: "frontier".into(),
        coverage: gemel::exchange::Coverage {
            canonical_metadata: "complete".into(),
            source_content: "carrier-backed".into(),
            evidence_receipts: "complete".into(),
            evidence_payloads: "partial".into(),
            conversations: "omitted".into(),
            forensic_traces: "omitted".into(),
        },
        required_schemas: vec![1],
    };
    let bytes = gemel::exchange::encode_frontier(&f).unwrap();
    let digest = gemel::exchange::frontier_id(&bytes);
    assert_eq!(
        gemel::hex::encode(&digest),
        "02d9452325f484381497013dc8022cd9616c5195d57e99423b8495f2e2e70892",
        "frontier descriptor digest is pinned (protocol change requires \
         version consideration, EXCHANGE.md §59)"
    );
    // The descriptor bytes are canonical JSON, sorted keys, no timestamps.
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("now") && !text.contains("timestamp"));
    assert!(text.contains("gemel.exchange.frontier.v1"));
    // Round-trips byte-exactly and preserves the profile (regression: the
    // profile was once read from inside the coverage object).
    let parsed =
        gemel::exchange::parse_frontier(&bytes, &gemel::exchange::ExchangeLimits::default())
            .unwrap();
    assert_eq!(parsed.profile, "frontier");
    assert_eq!(
        gemel::exchange::encode_frontier(&parsed).unwrap(),
        bytes,
        "parse(encode(x)) must equal x"
    );
    let portable = gemel::exchange::Frontier {
        profile: "portable".into(),
        ..f
    };
    let pbytes = gemel::exchange::encode_frontier(&portable).unwrap();
    let pparsed =
        gemel::exchange::parse_frontier(&pbytes, &gemel::exchange::ExchangeLimits::default())
            .unwrap();
    assert_eq!(
        pparsed.profile, "portable",
        "portable profile must survive a round-trip"
    );
}

#[test]
fn golden_pack_bytes_and_digest() {
    // A pack of two blobs: fully deterministic content (no timestamps).
    let b1 = gemel::value::Object::blob(b"hello\n".to_vec());
    let env1 = {
        let mut env = Vec::new();
        env.extend_from_slice(b"GEML");
        env.push(1);
        env.push(1);
        env.push(1);
        env.push(0);
        let mut len = Vec::new();
        gemel::varint::encode_u64(b"hello\n".len() as u64, &mut len);
        env.extend_from_slice(&len);
        env.extend_from_slice(b"hello\n");
        env
    };
    let id1 = gemel::content::blob_identity(b"hello\n");
    let id2 = gemel::content::blob_identity(b"world\n");
    let env2 = {
        let mut env = Vec::new();
        env.extend_from_slice(b"GEML");
        env.push(1);
        env.push(1);
        env.push(1);
        env.push(0);
        let mut len = Vec::new();
        gemel::varint::encode_u64(b"world\n".len() as u64, &mut len);
        env.extend_from_slice(&len);
        env.extend_from_slice(b"world\n");
        env
    };
    assert_eq!(env1.len(), 15);
    let (pack, pid) = gemel::exchange::encode_pack(&[
        gemel::exchange::PackObject {
            id: id1,
            envelope: env1,
        },
        gemel::exchange::PackObject {
            id: id2,
            envelope: env2,
        },
    ])
    .unwrap();
    let _ = b1;
    assert_eq!(
        gemel::hex::encode(&pid),
        "b83316a4caf9455f03cda3c766435e14b373e30e8157d63d38e9180ac53527b3",
        "pack digest is pinned (EXCHANGE.md §59)"
    );
    // The pack parses back to the two blob identities.
    let decoded =
        gemel::exchange::decode_pack(&pack, &gemel::exchange::ExchangeLimits::default()).unwrap();
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].id, id1);
    assert_eq!(decoded[1].id, id2);
    // Object identities are pinned as native identities.
    assert_eq!(
        id1.to_string(),
        "blob.808efa7d6f43730dfac9dd2758b9de14f53065fcf5cc8748be8a020e1598f8c1"
    );
    assert_eq!(
        id2.to_string(),
        "blob.db7c7d9a6e6d99ee3fb14b13137b4183038d39c9e73bf031005a175b52aa0ba5"
    );
}

// ---------------------------------------------------------------------------
// §17/§33 verify without a native store + coverage propagation
// ---------------------------------------------------------------------------

#[test]
fn verify_works_on_fresh_checkout_without_native_store() {
    let repo_a = make_exported_repo("verify-fresh");
    let remote = temp_root("verify-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("verify-clone");
    clone_repo(&remote, &clone, &[]);
    // No `gemel status` has run: no native store exists.
    assert!(!clone.join(".gemel").is_dir() || !clone.join(".gemel/objects").is_dir());

    let out = result_of(&clone, &["exchange", "verify", "--json"]);
    assert!(out["frontiers_validated"].as_u64().unwrap() >= 1);
    assert!(out["packs_validated"].as_u64().unwrap() >= 1);
    assert_eq!(out["staged"], false);
    assert_eq!(out["matched"].as_array().unwrap().len(), 1);

    // Git-index verification against the staged tree.
    let out = result_of(&clone, &["exchange", "verify", "--git-index", "--json"]);
    assert_eq!(out["staged"], true);
    assert_eq!(out["matched"].as_array().unwrap().len(), 1);

    // The frontier carries explicit coverage; the client reports what it
    // does NOT carry rather than pretending absence means absence.
    let es = result_of(&clone, &["exchange", "status", "--json"]);
    assert_eq!(es["detected"], true);
    assert_eq!(es["native_store"], false);
}

// ---------------------------------------------------------------------------
// §41 fsck integration
// ---------------------------------------------------------------------------

#[test]
fn fsck_reports_exchange_omitted_blobs_separately_from_corruption() {
    let repo_a = make_exported_repo("fsck-a");
    let remote = temp_root("fsck-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("fsck-clone");
    clone_repo(&remote, &clone, &[]);
    let st = result_of(&clone, &["status", "--json"]);
    let _ = st;
    let (code, v, _) = gemel_cli(&clone, &["fsck", "--json"]);
    assert_eq!(code, 0, "native store must be clean: {v}");
    let res = &v["result"];
    assert_eq!(res["exchange"]["frontiers"], 1);
    assert_eq!(res["exchange"]["imported"], 1);
    // A genuinely corrupt non-blob object is still an error.
    let (repo, root) = {
        let r = temp_root("fsck-corrupt");
        let repo = Repo::init(&r, &InitOptions::default()).unwrap();
        (repo, r)
    };
    // Corrupt an object file on disk.
    let mut n = 0;
    let objs = root.join(".gemel/objects");
    let mut target = None;
    for shard in std::fs::read_dir(&objs).unwrap() {
        let shard = shard.unwrap();
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".gce") && n == 0 {
                target = Some(entry.path());
                n += 1;
            }
        }
    }
    let t = target.expect("an object file");
    let mut bytes = std::fs::read(&t).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&t, bytes).unwrap();
    let report = repo
        .fsck(&gemel::store::fsck::FsckOptions::default())
        .unwrap();
    assert!(
        report.problems.iter().any(|p| p.code == "corrupt-object"
            || p.code == "unreadable-object"
            || p.code == "filename-mismatch"),
        "real corruption must still be reported: {:?}",
        report.problems
    );
}

// ---------------------------------------------------------------------------
// §35/§36 atomic publication + interrupted export recovery
// ---------------------------------------------------------------------------

#[test]
fn interrupted_export_temporaries_are_cleaned_and_export_recovers() {
    let root = temp_root("atomic");
    git(&root, &["init", "-q", "-b", "main"]);
    git_config(&root);
    gemel_cli(&root, &["init"]);
    write_file(&root, "src.rs", "pub fn a() {}\n");
    gemel_cli(&root, &["change", "begin", "--intent-summary", "Atomic"]);
    write_file(&root, "src.rs", "pub fn a() { b(); }\nfn b() {}\n");
    let (code, v, err) = gemel_cli(
        &root,
        &["change", "finish", "--json", "--summary", "atomic change"],
    );
    assert_eq!(code, 0, "{err}");
    let frontier = &v["result"]["exchange"]["exported"];
    assert_eq!(frontier, true);
    // Simulate a crash: an abandoned temporary next to a published pack.
    let packs = root.join(".gemel/exchange/v1/packs");
    let mut tmp = None;
    for shard in std::fs::read_dir(&packs).unwrap() {
        let shard = shard.unwrap();
        for entry in std::fs::read_dir(shard.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".gxp") {
                tmp = Some(entry.path().with_extension("gxp.tmp-99999"));
            }
        }
    }
    // The write path places temporaries inside the shards next to their
    // target (never at the packs root).
    let t = tmp.unwrap();
    std::fs::write(&t, b"abandoned partial pack").unwrap();
    // Re-export with no state change: idempotent, no new bytes.
    let before = exchange_files(&root);
    let out = result_of(&root, &["exchange", "export", "--json"]);
    assert!(out["packs_reused"].as_u64().unwrap() >= 1);
    assert_eq!(
        exchange_files(&root),
        before,
        "idempotent export adds nothing"
    );
    assert!(!t.exists(), "abandoned temp must be cleaned");
}

// ---------------------------------------------------------------------------
// §62/§63: exchange files are reviewable; CI verify of committed state
// ---------------------------------------------------------------------------

#[test]
fn ci_verify_of_committed_state_passes() {
    let repo_a = make_exported_repo("ci-a");
    let remote = temp_root("ci-remote");
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        &repo_a,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo_a, &["push", "-q", "-u", "origin", "HEAD"]);
    let clone = temp_root("ci-clone");
    clone_repo(&remote, &clone, &[]);
    // CI-style check against the checked-out (committed) tree, no native
    // store, no bootstrap needed.
    let out = result_of(&clone, &["exchange", "verify", "--git-index", "--json"]);
    assert_eq!(out["matched"].as_array().unwrap().len(), 1);
    assert_eq!(out["diverged"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------------
// §3: the local .gemel/.gitignore preserves a clean git status at init
// ---------------------------------------------------------------------------

#[test]
fn init_installs_exchange_gitignore_and_keeps_native_store_invisible() {
    let root = temp_root("gitignore");
    git(&root, &["init", "-q", "-b", "main"]);
    git_config(&root);
    gemel_cli(&root, &["init"]);
    let gi = std::fs::read_to_string(root.join(".gemel/.gitignore")).unwrap();
    assert_eq!(gi, "*\n!.gitignore\n!exchange/\n!exchange/**\n");
    // The native store is not tracked; the gitignore file itself is.
    commit_all(&root, "init");
    let tracked = git(&root, &["ls-files"]);
    assert!(tracked.contains(".gemel/.gitignore"));
    assert!(!tracked.contains(".gemel/objects"));
    assert!(!tracked.contains(".gemel/meta.json"));
    // git status is clean.
    let porcelain = git(&root, &["status", "--porcelain"]);
    assert_eq!(porcelain.trim(), "", "{porcelain}");
}
