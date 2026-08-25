//! Phase 5 integration tests (SPECIFICATION.md Phase 5; OBJECT_MODEL.md
//! §6.23–§6.24; brief §22–§24, §13).
//!
//! Semantic entities are *derived facts*: deterministic scanner output
//! published as canonical `semantic-entity` objects per state, grouped by a
//! canonical `semantic-index` object. Identity is content-addressed;
//! unchanged entities deduplicate; changed/moved entities link by **explicit
//! lineage** with a documented evidence string and certainty — a permanent
//! semantic identity is never silently inferred from heuristics.
//!
//! Courts cover: extraction correctness, index determinism and dedup,
//! lineage on edit (observed) vs move (possible), file-movement survival via
//! aliases, `gemel diff --semantic`, trait/test extraction, nested modules,
//! non-Rust files, Cargo.toml features/dependencies, exchange export of the
//! semantic graph without dangling references, and fsck integrity.

// Test closures return the rich store error type; boxed variants would
// obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use gemel::semantic::{self, EntityInfo};
use gemel::store::{fsck, InitOptions, Repo};
use gemel::workflow::{self, BeginOptions, FinishOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p5-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// The deterministic indexer producer identity (published during indexing).
fn indexer_gid(repo: &Repo) -> gemel::gid::Gid {
    gemel::content::object_identity(
        repo,
        &gemel::defaults::automation_producer_object_at(semantic::INDEXER_PRODUCER_NAME, 0),
    )
    .unwrap()
}

/// Seeds a repo with a Rust crate and a snapshot-only base state S1.
fn seed_crate(root: &Path) -> (Repo, gemel::gid::Gid) {
    write_file(
        root,
        "src/parser.rs",
        "pub fn decode_name(data: &[u8]) -> String {\n    String::from_utf8_lossy(data).to_string()\n}\n\npub struct Name;\n\nimpl Name {\n    pub fn new() -> Self { Name }\n}\n\n#[test]\nfn roundtrip() {\n    assert_eq!(decode_name(b\"abc\"), \"abc\");\n}\n",
    );
    write_file(root, "src/lib.rs", "pub mod parser;\n");
    write_file(
        root,
        "Cargo.toml",
        "[package]\nname = \"p5demo\"\nversion = \"0.1.0\"\n\n[features]\ndefault = [\"std\"]\n\n[dependencies]\nserde = { version = \"1\" }\n",
    );
    let repo = Repo::init(root, &InitOptions::default()).unwrap();
    let ignore = gemel::ignore::Ignore::from_root(root);
    let snap = gemel::content::build_state(&repo, root, &ignore).unwrap();
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction {
            ops: vec![gemel::store::refs::RefOp::set(
                &format!("{}/S1", gemel::store::REF_NAMES),
                snap.state,
            )],
        })
        .unwrap();
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    (repo, snap.state)
}

/// Makes a change from `base` by editing `src/parser.rs` (moving `Name` into
/// a nested module and changing `decode_name`'s body) and finishes it,
/// returning the resulting state S2.
fn make_edit_change(repo: &Repo, base: &gemel::gid::Gid, root: &Path) -> gemel::gid::Gid {
    workflow::begin_change(
        repo,
        &BeginOptions {
            from_state: Some(*base),
            intent_summary: Some("restructure parser".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(
        root,
        "src/name/mod.rs",
        "pub struct Name;\n\nimpl Name {\n    pub fn new() -> Self { Name }\n}\n",
    );
    write_file(
        root,
        "src/parser.rs",
        "pub fn decode_name(data: &[u8]) -> String {\n    format!(\"{}!\", String::from_utf8_lossy(data))\n}\n",
    );
    write_file(root, "src/lib.rs", "pub mod name;\npub mod parser;\n");
    let out = workflow::finish_change(
        repo,
        &FinishOptions {
            summary: "move Name into name module; modify decode_name".into(),
            ..Default::default()
        },
    )
    .unwrap();
    out.state
}

fn entity_list(repo: &Repo, state: &gemel::gid::Gid) -> Vec<EntityInfo> {
    let entities = semantic::entities_for_state(repo, state)
        .unwrap()
        .expect("state indexed");
    entities
        .iter()
        .map(|(gid, obj)| {
            let mut info = EntityInfo::from_object(obj);
            info.id = Some(*gid);
            info
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Courts
// ---------------------------------------------------------------------------

#[test]
fn index_determinism_and_dedup() {
    let root = temp_root("determinism");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    let first = semantic::index_state(&repo, &s1, &g).unwrap();
    let second = semantic::index_state(&repo, &s1, &g).unwrap();
    // Identical index identity and identical entity identities.
    assert_eq!(first.index, second.index);
    let e1: Vec<String> = entity_list(&repo, &s1)
        .iter()
        .map(|e| e.id.unwrap().to_string())
        .collect();
    let e2: Vec<String> = entity_list(&repo, &s1)
        .iter()
        .map(|e| e.id.unwrap().to_string())
        .collect();
    assert_eq!(e1, e2);
    // Re-indexing the same state deduplicates: nothing new is published.
    assert_eq!(second.new_entities, 0);
    assert_eq!(second.modified_entities, 0);
}

#[test]
fn extraction_covers_kinds_and_spans() {
    let root = temp_root("kinds");
    let (repo, s1) = seed_crate(&root);
    semantic::index_state(&repo, &s1, &indexer_gid(&repo)).unwrap();
    let entities = entity_list(&repo, &s1);
    let find = |name: &str| entities.iter().find(|e| e.name == name).unwrap();
    // Function with exact span and full signature (pub included).
    let f = find("decode_name");
    assert_eq!(f.kind, "function");
    assert_eq!(f.module_path, "crate::parser");
    assert_eq!(f.file_path, "src/parser.rs");
    assert_eq!(f.start_line, 1);
    assert_eq!(f.end_line, 3);
    assert_eq!(f.visibility, "public");
    assert!(f
        .signature
        .contains("pub fn decode_name(data: &[u8]) -> String"));
    // Struct and impl entities for `Name` (sorted: impl precedes type).
    assert!(entities
        .iter()
        .any(|e| e.kind == "type" && e.name == "Name"));
    assert!(entities
        .iter()
        .any(|e| e.kind == "impl" && e.name == "Name"));
    let test = entities.iter().find(|e| e.name == "roundtrip").unwrap();
    assert_eq!(test.kind, "test");
    assert_eq!(test.module_path, "crate::parser");
    // Cargo.toml: feature + dependency entities.
    assert!(entities
        .iter()
        .any(|e| e.kind == "feature" && e.name == "default"));
    assert!(entities
        .iter()
        .any(|e| e.kind == "dependency" && e.name == "serde"));
    // Module entity.
    assert!(entities
        .iter()
        .any(|e| e.kind == "module" && e.name == "parser"));
}

#[test]
fn trait_impl_and_test_entities() {
    let root = temp_root("traitimpl");
    write_file(
        &root,
        "src/lib.rs",
        "pub struct Parser;\n\nimpl Display for Parser {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { Ok(()) }\n}\n",
    );
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = gemel::ignore::Ignore::from_root(&root);
    let snap = gemel::content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    semantic::index_state(&repo, &snap.state, &indexer_gid(&repo)).unwrap();
    let entities = entity_list(&repo, &snap.state);
    let impl_entity = entities
        .iter()
        .find(|e| e.kind == "impl")
        .expect("impl entity");
    assert_eq!(impl_entity.name, "Display for Parser");
    assert!(impl_entity.signature.contains("impl Display for Parser"));
}

#[test]
fn lineage_observed_on_edit_possible_on_move() {
    let root = temp_root("lineage");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    semantic::index_state(&repo, &s1, &g).unwrap();
    let s2 = make_edit_change(&repo, &s1, &root);
    semantic::index_state(&repo, &s2, &g).unwrap();
    let entities = entity_list(&repo, &s2);
    // decode_name stayed in crate::parser: observed lineage.
    let dn = entities.iter().find(|e| e.name == "decode_name").unwrap();
    let (from, evidence, certainty) = dn.lineage.as_ref().expect("decode_name lineage");
    assert_eq!(certainty, "observed");
    assert_eq!(evidence, "same-name-kind-path");
    assert_eq!(from.family(), gemel::family::Family::SemanticEntity);
    // Name moved from crate::parser to crate::name: possible lineage with a
    // documented similarity evidence string (never a silent merge).
    let name_entities: Vec<&EntityInfo> = entities.iter().filter(|e| e.name == "Name").collect();
    assert!(!name_entities.is_empty());
    for n in &name_entities {
        assert_eq!(n.module_path, "crate::name");
        let (_, evidence, certainty) = n.lineage.as_ref().expect("Name lineage");
        assert_eq!(certainty, "possible");
        assert_eq!(evidence, "similarity:same-name-kind");
    }
}

#[test]
fn file_movement_survival_and_aliases() {
    let root = temp_root("move");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    semantic::index_state(&repo, &s1, &g).unwrap();
    let s2 = make_edit_change(&repo, &s1, &root);
    semantic::index_state(&repo, &s2, &g).unwrap();
    // Bare-name resolution finds the moved entity (current index).
    let resolved = semantic::resolve_subject(&repo, "Name").unwrap();
    let entity = resolved.entity.expect("Name resolves after the move");
    assert_eq!(entity.module_path, "crate::name");
    // Aliases include the current file path AND the lineage chain's old path,
    // so queries about the entity surface work that touched its ancestors.
    assert!(resolved.aliases.contains(&"src/name/mod.rs".to_string()));
    assert!(resolved.aliases.contains(&"src/parser.rs".to_string()));
    assert!(resolved
        .aliases
        .contains(&"crate::parser::Name".to_string()));
    // `why` surfaces the change that created the entity in its old home.
    let why = gemel::query::why(&repo, "Name").unwrap();
    assert!(why.introduced_by.is_some());
    assert_eq!(why.semantic.as_ref().map(|e| e.name.as_str()), Some("Name"));
    // `attempts` on the moved name finds the trajectory.
    let attempts = gemel::query::attempts(&repo, "Name").unwrap();
    assert!(!attempts.is_empty());
}

#[test]
fn diff_semantic_reports_move_modify_add() {
    let root = temp_root("diff");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    semantic::index_state(&repo, &s1, &g).unwrap();
    let s2 = make_edit_change(&repo, &s1, &root);
    semantic::index_state(&repo, &s2, &g).unwrap();
    let d = semantic::semantic_diff(&repo, &s1, &s2).unwrap();
    // Name (struct + impl) moved; decode_name modified; name module added.
    assert_eq!(d.moved.len(), 2);
    for m in &d.moved {
        let before = m.before.as_ref().unwrap();
        let after = m.after.as_ref().unwrap();
        assert_eq!(before.module_path, "crate::parser");
        assert_eq!(after.module_path, "crate::name");
    }
    assert!(d
        .modified
        .iter()
        .any(|m| { m.after.as_ref().map(|i| i.name.as_str()) == Some("decode_name") }));
    assert!(d
        .added
        .iter()
        .any(|a| a.name == "name" && a.kind == "module"));
    // Unchanged Cargo.toml entities deduplicate across states.
    assert!(d.unchanged >= 2);
    // Determinism: identical inputs produce identical outputs.
    let d2 = semantic::semantic_diff(&repo, &s1, &s2).unwrap();
    assert_eq!(d.added.len(), d2.added.len());
    assert_eq!(d.moved.len(), d2.moved.len());
    assert_eq!(d.modified.len(), d2.modified.len());
    assert_eq!(d.removed.len(), d2.removed.len());
    assert_eq!(d.unchanged, d2.unchanged);
}

#[test]
fn nested_modules_get_paths() {
    let root = temp_root("nested");
    write_file(
        &root,
        "src/lib.rs",
        "pub mod outer {\n    pub mod inner {\n        pub fn deep() {}\n    }\n}\n",
    );
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = gemel::ignore::Ignore::from_root(&root);
    let snap = gemel::content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    semantic::index_state(&repo, &snap.state, &indexer_gid(&repo)).unwrap();
    let entities = entity_list(&repo, &snap.state);
    let deep = entities.iter().find(|e| e.name == "deep").unwrap();
    assert_eq!(deep.module_path, "crate::outer::inner");
    let outer = entities.iter().find(|e| e.name == "outer").unwrap();
    assert_eq!(outer.module_path, "crate");
    let inner = entities.iter().find(|e| e.name == "inner").unwrap();
    assert_eq!(inner.module_path, "crate::outer");
}

#[test]
fn non_rust_files_ignored_toml_scanned() {
    let root = temp_root("nonrust");
    write_file(&root, "src/lib.rs", "pub fn only_fn() {}\n");
    write_file(&root, "src/other.py", "def python_fn():\n    pass\n");
    write_file(&root, "README.md", "# no entities here\nfn fake() {}\n");
    write_file(&root, "src/binary.dat", "\u{0}\u{1}\u{2}\u{ff}");
    write_file(
        &root,
        "Cargo.toml",
        "[features]\nfull = [\"std\", \"extra\"]\n\n[dependencies]\nrand = \"0.8\"\n",
    );
    let repo = Repo::init(&root, &InitOptions::default()).unwrap();
    let ignore = gemel::ignore::Ignore::from_root(&root);
    let snap = gemel::content::build_state(&repo, &root, &ignore).unwrap();
    repo.with_write_lock(|| {
        workflow::set_workspace_state(&repo, snap.state).unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    let out = semantic::index_state(&repo, &snap.state, &indexer_gid(&repo)).unwrap();
    let entities = entity_list(&repo, &snap.state);
    // One Rust fn, one feature (`full` with deps), one dependency — nothing
    // from .py/.md/.dat.
    assert_eq!(out.entities, 3);
    assert!(entities.iter().any(|e| e.name == "only_fn"));
    assert!(entities
        .iter()
        .any(|e| e.name == "full" && e.kind == "feature"));
    assert!(entities
        .iter()
        .any(|e| e.name == "rand" && e.kind == "dependency"));
    assert!(!entities
        .iter()
        .any(|e| e.name == "python_fn" || e.name == "fake"));
}

#[test]
fn index_independent_of_disposable_store() {
    // The semantic answers are canonical objects + refs; deleting the derived
    // SQLite index must not change them (brief §2: accelerator, never oracle).
    let root = temp_root("indexindep");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    semantic::index_state(&repo, &s1, &g).unwrap();
    let before = entity_list(&repo, &s1);
    repo.with_write_lock(|| {
        let meta = repo.meta_dir();
        let idx = meta.join("index.db");
        if idx.exists() {
            std::fs::remove_file(&idx).unwrap();
        }
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
    let after = entity_list(&repo, &s1);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(after.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.signature, b.signature);
    }
}

#[test]
fn exchange_export_carries_semantic_objects() {
    // The indexer producer must be published so the exchange BFS over gid
    // edges never sees a dangling reference (EXCHANGE.md §9, §42).
    let root = temp_root("exch");
    let (repo, s1) = seed_crate(&root);
    let g = indexer_gid(&repo);
    semantic::index_state(&repo, &s1, &g).unwrap();
    // Make a change so there is a head to export.
    let s2 = make_edit_change(&repo, &s1, &root);
    semantic::index_state(&repo, &s2, &g).unwrap();
    let out =
        gemel::exchange::export::export(&repo, gemel::exchange::export::Profile::Frontier).unwrap();
    assert!(out.packs_written + out.packs_reused >= 1);
    // The semantic objects travelled: the exported object set contains the
    // semantic-index family.
    let objects = gemel::exchange::export::collect_export_objects(
        &repo,
        gemel::exchange::export::Profile::Frontier,
    )
    .unwrap();
    let families: std::collections::HashSet<gemel::family::Family> =
        objects.iter().map(|(gid, _)| gid.family()).collect();
    assert!(families.contains(&gemel::family::Family::SemanticIndex));
    assert!(families.contains(&gemel::family::Family::SemanticEntity));
    assert!(families.contains(&gemel::family::Family::Producer));
    // fsck must report a clean native store (no missing references).
    let report = fsck::run(
        &repo,
        &fsck::FsckOptions {
            repair: false,
            rebuild_index: false,
            verbose: false,
        },
    )
    .unwrap();
    assert!(
        report
            .problems
            .iter()
            .all(|p| p.code != "missing-reference"),
        "dangling references after indexing: {:?}",
        report.problems
    );
}

#[test]
fn status_reports_semantic_count() {
    let root = temp_root("status");
    let (repo, s1) = seed_crate(&root);
    // Not indexed yet: None, never a false zero.
    let st = gemel::query::status(&repo).unwrap();
    assert!(st.semantic_entities.is_none());
    semantic::index_state(&repo, &s1, &indexer_gid(&repo)).unwrap();
    let st = gemel::query::status(&repo).unwrap();
    assert!(st.semantic_entities.unwrap() >= 5);
}

// ---------------------------------------------------------------------------
// Git-carried semantic context (Phase 5 × Phase 1.5)
// ---------------------------------------------------------------------------

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The Phase 5 strategic court: semantic context travels through ordinary
/// Git and rehydrates on a fresh machine with identical identities
/// (EXCHANGE.md §27, §43, §56; Phase 5 entity context after clone).
#[test]
fn semantic_context_survives_git_clone() {
    // Repository A: crate, change, index, exchange export, git commit.
    let a = temp_root("clone-a");
    let (repo_a, s1) = seed_crate(&a);
    let g = indexer_gid(&repo_a);
    semantic::index_state(&repo_a, &s1, &g).unwrap();
    let s2 = make_edit_change(&repo_a, &s1, &a);
    semantic::index_state(&repo_a, &s2, &g).unwrap();
    let idx_a = semantic::index_for_state(&repo_a, &s2).unwrap().unwrap();
    gemel::exchange::export::export(&repo_a, gemel::exchange::export::Profile::Frontier).unwrap();
    git(&a, &["init", "-q"]);
    git(&a, &["config", "user.email", "court@example.com"]);
    git(&a, &["config", "user.name", "Gemel Court"]);
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-q", "-m", "crate with gemel context"]);
    let remote = temp_root("clone-remote");
    git(
        &a,
        &[
            "clone",
            "--bare",
            "-q",
            a.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    // Repository B: fresh shallow clone, then `gemel status --json` only.
    let b = temp_root("clone-b");
    git(
        a.parent().unwrap(),
        &[
            "clone",
            "--depth=1",
            "-q",
            remote.to_str().unwrap(),
            b.to_str().unwrap(),
        ],
    );
    // The CLI's status auto-bootstraps, ingests, and activates (no manual
    // import step; EXCHANGE.md §34).
    let bin = env!("CARGO_BIN_EXE_gemel");
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&b)
        .args(["status", "--json"])
        .output()
        .expect("run gemel status on clone");
    assert!(
        out.status.success(),
        "gemel status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let status: serde_json::Value =
        serde_json::from_str(&stdout).expect("status is structured JSON");
    // The imported frontier matches the checked-out source.
    assert_eq!(status["result"]["exchange"]["source_match"], true);
    // The imported semantic index re-established refs/semantic for the head
    // state with the exact identity exported from repository A.
    let repo_b = Repo::open(&b).unwrap();
    let idx_b = semantic::index_for_state(&repo_b, &s2).unwrap();
    assert_eq!(
        idx_b,
        Some(idx_a),
        "semantic index identity survives the clone"
    );
    assert_eq!(
        status["result"]["semantic"]["entities"].as_u64().unwrap(),
        7
    );
    // Entity resolution works on the imported knowledge.
    let resolved = semantic::resolve_subject(&repo_b, "decode_name").unwrap();
    assert_eq!(resolved.entity.unwrap().name, "decode_name");
    let why = gemel::query::why(&repo_b, "decode_name").unwrap();
    assert!(why.introduced_by.is_some());
    assert_eq!(
        why.semantic.as_ref().map(|e| e.name.as_str()),
        Some("decode_name")
    );
}
