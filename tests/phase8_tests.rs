//! Phase 8 integration courts (SPECIFICATION.md Phase 8; HOSTED.md;
//! THREAT_MODEL.md §10; brief §38).
//!
//! Network transports (SSH + HTTP) implementing the Phase 6 transport trait,
//! capability-scoped auth, hosted sync over ordinary sockets, and the FRF
//! court runner (the *only* execution path, policy-gated — nothing executes
//! during ingestion, status, or sync). Courts prove: the remote URL grammar
//! (including `https://` fail-closed), HTTP roundtrip with identical
//! canonical identities, idempotent re-push, HTTP auth (missing/wrong token
//! 401, read-only capability, read-only server), non-loopback-without-tokens
//! fail-closed, multi-repo `--root` serving with path-traversal rejection,
//! the SSH-equivalent session over the local binary (including read-only
//! refusal), CLI push/pull over an HTTP URL, the court runner's policy
//! matrix (default deny, `--allow`, allowlist, timeout → inconclusive), and
//! policy-gap reporting through `policy --json` with NOT_READY readiness.

#![allow(clippy::result_large_err)]

use gemel::store::{InitOptions, Repo};
use gemel::sync::Transport as _;
use gemel::value::{Field, Object, Value};
use gemel::workflow::{self, BeginOptions, FinishOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-p8-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A repository with an indexed head state (one change, one intent).
fn seed(root: &Path) -> Repo {
    write_file(
        root,
        "src/lib.rs",
        "pub fn greet() -> &'static str { \"hi\" }\n",
    );
    let repo = Repo::init(root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("greeting".into()),
            ..Default::default()
        },
    )
    .unwrap();
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "add greet".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // Index the head state so semantic queries (`why`) work on pulled clones.
    let state = repo
        .read_ref(gemel::store::REF_STATE_HEAD)
        .unwrap()
        .unwrap();
    let producer = gemel::content::object_identity(
        &repo,
        &gemel::defaults::automation_producer_object_at(gemel::semantic::INDEXER_PRODUCER_NAME, 0),
    )
    .unwrap();
    gemel::semantic::index_state(&repo, &state, &producer).unwrap();
    repo
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A spawned CLI child (killed on drop so failed tests cannot leak servers).
struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Polls until the port accepts connections or the child exits early.
fn wait_for_server(child: &mut Child, port: u16) {
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let mut err = String::new();
            let _ = std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err);
            panic!("http server exited early ({status}): {err}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("http server did not accept connections on port {port}");
}

/// Spawns `gemel serve <root> --http 127.0.0.1:<port> [args...]`.
fn spawn_http_server(root: &Path, port: u16, args: &[&str]) -> ServerGuard {
    let bin = env!("CARGO_BIN_EXE_gemel");
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .arg(root)
        .arg("--http")
        .arg(format!("127.0.0.1:{port}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    for a in args {
        cmd.arg(a);
    }
    let child = cmd
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gemel serve");
    ServerGuard(child)
}

/// A transport to a spawned HTTP server.
fn http_transport(
    port: u16,
    path: &str,
    token: Option<&str>,
) -> gemel::sync::transports::HttpTransport {
    gemel::sync::transports::HttpTransport::new("127.0.0.1", port, path, token.map(String::from))
}

/// Inserts an evidence object carrying a reproduction command — the FRF
/// court's input (court.rs reads field 0x05).
fn insert_court_evidence(repo: &Repo, command: &str, subject: &str) -> gemel::gid::Gid {
    let producer = gemel::defaults::automation_producer_object_at("court-seeder", 0);
    let producer_gid = repo.insert_object(&producer).unwrap();
    let obj = Object::fields(
        gemel::family::Family::Evidence,
        vec![
            Field::new(0x01, Value::Gid(producer_gid)),
            Field::new(0x02, Value::Str("test_result".into())),
            Field::new(0x03, Value::Str(subject.into())),
            Field::new(0x05, Value::Str(command.into())),
            Field::new(
                0x0D,
                Value::Record(vec![Field::new(0x01, Value::Str("inconclusive".into()))]),
            ),
            Field::new(0x10, Value::I(gemel::store::now_ms())),
        ],
    );
    repo.insert_object(&obj).unwrap()
}

/// Replaces the repository config (the mandatory field is 0x04
/// `execution_policy`; 0x08 is the required-verification matrix).
fn write_config(repo: &Repo, fields: Vec<Field>) {
    let cfg = Object::fields(gemel::family::Family::Config, fields);
    let gid = repo.insert_object(&cfg).unwrap();
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction {
            ops: vec![gemel::store::refs::RefOp::set(
                gemel::store::REF_CONFIG,
                gid,
            )],
        })
        .unwrap();
        Ok::<(), gemel::store::Error>(())
    })
    .unwrap();
}

fn execution_policy(repo: &Repo) -> String {
    let Some(cfg) = repo.read_ref(gemel::store::REF_CONFIG).unwrap() else {
        return "never_auto_execute".into();
    };
    let obj = repo.load(&cfg).unwrap();
    obj.field_sequence()
        .and_then(|fs| gemel::query::str_field(fs, 0x04))
        .unwrap_or("never_auto_execute")
        .to_string()
}

// ---------------------------------------------------------------------------
// Remote URL grammar (HOSTED.md §3)
// ---------------------------------------------------------------------------

#[test]
fn remote_url_grammar_ssh_http_local() {
    use gemel::sync::transports::{parse_remote, RemoteUrl};
    // ssh: host, user, optional port, repository path required.
    let ssh = parse_remote("ssh://git@example.com/srv/gemel/foo").unwrap();
    match ssh {
        RemoteUrl::Ssh {
            user,
            host,
            port,
            path,
        } => {
            assert_eq!(user.as_deref(), Some("git"));
            assert_eq!(host, "example.com");
            assert_eq!(port, None); // scheme default (22), applied by ssh(1)
            assert_eq!(path, "/srv/gemel/foo");
        }
        other => panic!("expected Ssh, got {other:?}"),
    }
    let ssh = parse_remote("ssh://example.com:2222/repo").unwrap();
    match ssh {
        RemoteUrl::Ssh {
            user,
            host,
            port,
            path,
        } => {
            assert_eq!(user, None);
            assert_eq!(host, "example.com");
            assert_eq!(port, Some(2222));
            assert_eq!(path, "/repo");
        }
        other => panic!("expected Ssh, got {other:?}"),
    }
    // http: host, optional port (default 80), token via userinfo.
    let http = parse_remote("http://sekret@127.0.0.1:8033/gemel/foo").unwrap();
    match http {
        RemoteUrl::Http {
            host,
            port,
            path,
            token,
        } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 8033);
            assert_eq!(path, "/gemel/foo");
            assert_eq!(token.as_deref(), Some("sekret"));
        }
        other => panic!("expected Http, got {other:?}"),
    }
    let http = parse_remote("http://example.com/repo").unwrap();
    match http {
        RemoteUrl::Http {
            port, path, token, ..
        } => {
            assert_eq!(port, 80);
            assert_eq!(path, "/repo");
            assert_eq!(token, None);
        }
        other => panic!("expected Http, got {other:?}"),
    }
    // Local paths are untouched (relative and absolute).
    match parse_remote("/tmp/foo").unwrap() {
        RemoteUrl::Local(p) => assert_eq!(p, Path::new("/tmp/foo")),
        other => panic!("expected Local, got {other:?}"),
    }
    match parse_remote("./sibling").unwrap() {
        RemoteUrl::Local(p) => assert_eq!(p, Path::new("./sibling")),
        other => panic!("expected Local, got {other:?}"),
    }
}

#[test]
fn remote_url_fails_closed() {
    use gemel::sync::transports::parse_remote;
    // https:// is rejected: TLS is not implemented in-crate.
    let e = parse_remote("https://example.com/repo").unwrap_err();
    assert!(
        e.to_string().contains("https"),
        "https rejection must be explicit: {e}"
    );
    // Bad ports are rejected, not silently swallowed.
    for bad in [
        "ssh://host:abc/repo",
        "ssh://host:/repo",
        "ssh://host:99999/repo",
        "http://host:abc/repo",
    ] {
        assert!(
            parse_remote(bad).is_err(),
            "{bad} must be rejected as malformed"
        );
    }
    // Missing hosts and empty ssh paths fail.
    assert!(parse_remote("ssh:///repo").is_err());
    assert!(parse_remote("ssh://host").is_err());
    assert!(parse_remote("ssh://host/").is_err());
    assert!(parse_remote("http://:80/repo").is_err());
    // Unknown schemes are rejected with the supported set named.
    let e = parse_remote("ftp://host/repo").unwrap_err();
    assert!(e.to_string().contains("ssh://"));
}

// ---------------------------------------------------------------------------
// HTTP transport roundtrip (HOSTED.md §5)
// ---------------------------------------------------------------------------

#[test]
fn http_roundtrip_identical_ids_and_idempotence() {
    let a = temp_root("http-a");
    let server = temp_root("http-srv");
    let c = temp_root("http-c");
    let repo_a = seed(&a);
    let repo_server = Repo::init(&server, &InitOptions::default()).unwrap();

    let port = free_port();
    let mut guard = spawn_http_server(&server, port, &["--token", "sekret write"]);
    wait_for_server(&mut guard.0, port);

    // Push A → server with a write token.
    let mut push_t = http_transport(port, "/", Some("sekret"));
    let pushed = gemel::sync::push(&repo_a, "origin", &mut push_t).unwrap();
    assert!(pushed.transferred > 0);
    assert!(pushed.missing_on_remote > 0);
    // The server now holds A's canonical identities.
    let server_head = repo_server.read_ref(gemel::store::REF_HEAD).unwrap();
    assert_eq!(
        server_head,
        repo_a.read_ref(gemel::store::REF_HEAD).unwrap()
    );

    // Idempotent re-push transfers nothing.
    let again = gemel::sync::push(&repo_a, "origin", &mut push_t).unwrap();
    assert_eq!(again.transferred, 0);
    assert_eq!(again.missing_on_remote, 0);

    // Pull server → fresh C: identical canonical identities.
    let repo_c = Repo::init(&c, &InitOptions::default()).unwrap();
    let mut pull_t = http_transport(port, "/", Some("sekret"));
    let pulled = gemel::sync::pull(&repo_c, "origin", &mut pull_t).unwrap();
    assert!(pulled.fast_forwarded);
    assert!(pulled.fetch.transferred > 0);
    assert_eq!(
        repo_c.read_ref(gemel::store::REF_HEAD).unwrap(),
        repo_a.read_ref(gemel::store::REF_HEAD).unwrap()
    );
    assert_eq!(
        repo_c.read_ref(gemel::store::REF_STATE_HEAD).unwrap(),
        repo_a.read_ref(gemel::store::REF_STATE_HEAD).unwrap()
    );
    // Semantic knowledge travels over HTTP too.
    let why = gemel::query::why(&repo_c, "greet").unwrap();
    assert!(why.introduced_by.is_some());
}

#[test]
fn http_auth_capabilities_fail_closed() {
    let a = temp_root("auth-a");
    let server = temp_root("auth-srv");
    let repo_a = seed(&a);
    let _ = Repo::init(&server, &InitOptions::default()).unwrap();

    let port = free_port();
    let mut guard = spawn_http_server(
        &server,
        port,
        &["--token", "ro read", "--token", "rw write"],
    );
    wait_for_server(&mut guard.0, port);

    // No token → 401.
    let mut no_token = http_transport(port, "/", None);
    let e = no_token.list_refs().unwrap_err();
    assert!(
        e.to_string().contains("unauthorized"),
        "missing token must be rejected: {e}"
    );
    // Wrong token → 401.
    let mut bad = http_transport(port, "/", Some("nope"));
    let e = bad.list_refs().unwrap_err();
    assert!(
        e.to_string().contains("unauthorized"),
        "wrong token must be rejected: {e}"
    );
    // Read-only token: fetch works, push refused.
    let mut ro = http_transport(port, "/", Some("ro"));
    let refs = ro.list_refs().unwrap();
    assert!(!refs.is_empty());
    let e = gemel::sync::push(&repo_a, "origin", &mut ro).unwrap_err();
    assert!(
        e.to_string().contains("read-only"),
        "read-capability token must not mutate: {e}"
    );
    // Write token: push succeeds.
    let mut rw = http_transport(port, "/", Some("rw"));
    let out = gemel::sync::push(&repo_a, "origin", &mut rw).unwrap();
    assert!(out.transferred > 0);
}

#[test]
fn http_read_only_server_refuses_mutation() {
    let a = temp_root("ro-a");
    let server = temp_root("ro-srv");
    let repo_a = seed(&a);
    let _ = Repo::init(&server, &InitOptions::default()).unwrap();

    let port = free_port();
    let mut guard = spawn_http_server(&server, port, &["--read-only", "--token", "sekret write"]);
    wait_for_server(&mut guard.0, port);

    let mut t = http_transport(port, "/", Some("sekret"));
    // Reads work.
    assert!(!t.list_refs().unwrap().is_empty());
    // Mutations are refused even with a write token.
    let e = gemel::sync::push(&repo_a, "origin", &mut t).unwrap_err();
    assert!(
        e.to_string().contains("read-only"),
        "read-only server must refuse push: {e}"
    );
}

#[test]
fn http_non_loopback_without_tokens_fails_closed() {
    let root = temp_root("nl");
    let _ = seed(&root);
    let opts = gemel::sync::transports::ServeHttpOptions::default();
    let e = gemel::sync::transports::serve_http(&root, "0.0.0.0:0", &opts).unwrap_err();
    assert!(
        e.to_string().contains("refusing to serve"),
        "non-loopback without tokens must fail at startup: {e}"
    );
}

#[test]
fn http_multi_repo_root_serving_and_traversal_rejection() {
    let root = temp_root("root");
    let repo1_dir = root.join("repo1");
    let repo2_dir = root.join("repo2");
    let _ = seed(&repo1_dir);
    let _ = Repo::init(&repo2_dir, &InitOptions::default()).unwrap();
    // A non-repository directory that must not resolve.
    let _ = std::fs::create_dir_all(root.join("plain"));
    // The client is a *different* repository pushing into repo1 by URL path.
    let client_dir = temp_root("root-client");
    let repo_client = seed(&client_dir);

    let port = free_port();
    // Start with --root from a non-repository cwd to prove multi-repo
    // serving needs no local repository of its own.
    let bin = env!("CARGO_BIN_EXE_gemel");
    let mut cmd = Command::new(bin);
    cmd.current_dir(&root)
        .arg("serve")
        .arg("--root")
        .arg(&root)
        .arg("--http")
        .arg(format!("127.0.0.1:{port}"))
        .args(["--token", "sekret write"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut guard = ServerGuard(cmd.spawn().expect("spawn gemel serve --root"));
    wait_for_server(&mut guard.0, port);

    // Push the client into repo1 by URL path.
    let mut t1 = http_transport(port, "/repo1", Some("sekret"));
    let out = gemel::sync::push(&repo_client, "origin", &mut t1).unwrap();
    assert!(out.transferred > 0);
    let head = repo_client.read_ref(gemel::store::REF_HEAD).unwrap();
    let server_head = Repo::open(&repo1_dir)
        .unwrap()
        .read_ref(gemel::store::REF_HEAD)
        .unwrap();
    assert_eq!(head, server_head);

    // repo2 is reachable as its own repository.
    let mut t2 = http_transport(port, "/repo2", Some("sekret"));
    assert!(t2.list_refs().is_ok());

    // Unknown repositories and traversal attempts 404 (fail closed).
    for bad in ["/nope", "/../etc", "/..", "/repo1/../repo2", "/a/b"] {
        let mut t = http_transport(port, bad, Some("sekret"));
        let e = t.list_refs().unwrap_err();
        assert!(
            e.to_string().contains("no such repository") || e.to_string().contains("404"),
            "{bad} must be rejected: {e}"
        );
    }
}

// ---------------------------------------------------------------------------
// SSH-equivalent session transport (HOSTED.md §4)
// ---------------------------------------------------------------------------

#[test]
fn ssh_transport_roundtrip_via_local_binary() {
    let a = temp_root("ssh-a");
    let c = temp_root("ssh-c");
    let repo_a = seed(&a);
    let bin = env!("CARGO_BIN_EXE_gemel").to_string();

    // `SshTransport::spawn` speaks the bounded stdio session to any program;
    // here it is the local gemel binary (`gemel serve <path>`), the exact
    // command the real ssh transport runs remotely.
    let mut server = gemel::sync::transports::SshTransport::spawn(
        &bin,
        &["serve".into(), a.to_string_lossy().into_owned()],
        "test-ssh".into(),
    )
    .unwrap();
    // Push a fresh repo into the served repository.
    let repo_b = Repo::init(&temp_root("ssh-b"), &InitOptions::default()).unwrap();
    let pushed = gemel::sync::push(&repo_b, "origin", &mut server).unwrap();
    assert!(pushed.transferred > 0);
    let _ = repo_b;
    // Pull the served repository into a fresh clone: identical identities.
    let repo_c = Repo::init(&c, &InitOptions::default()).unwrap();
    let mut pull = gemel::sync::transports::SshTransport::spawn(
        &bin,
        &["serve".into(), a.to_string_lossy().into_owned()],
        "test-ssh".into(),
    )
    .unwrap();
    let pulled = gemel::sync::pull(&repo_c, "origin", &mut pull).unwrap();
    assert!(pulled.fast_forwarded);
    assert_eq!(
        repo_c.read_ref(gemel::store::REF_HEAD).unwrap(),
        repo_a.read_ref(gemel::store::REF_HEAD).unwrap()
    );
}

#[test]
fn ssh_read_only_refuses_mutation() {
    let a = temp_root("ssh-ro");
    let repo_a = seed(&a);
    let bin = env!("CARGO_BIN_EXE_gemel").to_string();
    let mut server = gemel::sync::transports::SshTransport::spawn(
        &bin,
        &[
            "serve".into(),
            a.to_string_lossy().into_owned(),
            "--read-only".into(),
        ],
        "test-ssh-ro".into(),
    )
    .unwrap();
    assert!(!server.list_refs().unwrap().is_empty());
    let e = gemel::sync::push(&repo_a, "origin", &mut server).unwrap_err();
    assert!(
        e.to_string().contains("read-only"),
        "read-only session must refuse push: {e}"
    );
}

// ---------------------------------------------------------------------------
// CLI push/pull over an HTTP URL (HOSTED.md §5)
// ---------------------------------------------------------------------------

#[test]
fn cli_push_pull_http_url_end_to_end() {
    let a = temp_root("cli-a");
    let server = temp_root("cli-srv");
    let c = temp_root("cli-c");
    let repo_a = seed(&a);
    let _ = Repo::init(&server, &InitOptions::default()).unwrap();

    let port = free_port();
    let mut guard = spawn_http_server(&server, port, &["--token", "sekret write"]);
    wait_for_server(&mut guard.0, port);
    let bin = env!("CARGO_BIN_EXE_gemel");
    let url = format!("http://sekret@127.0.0.1:{port}/");

    // `gemel push <http-url>` — the token rides in the URL userinfo.
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&a)
        .args(["push", &url, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert!(v["result"]["transferred"].as_u64().unwrap() > 0);

    // `gemel pull <http-url>` on a fresh clone restores the context.
    let _ = Repo::init(&c, &InitOptions::default()).unwrap();
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&c)
        .args(["pull", &url, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repo_c = Repo::open(&c).unwrap();
    assert_eq!(
        repo_c.read_ref(gemel::store::REF_HEAD).unwrap(),
        repo_a.read_ref(gemel::store::REF_HEAD).unwrap()
    );
    // The name derived from the URL is used for tracking refs.
    let all: Vec<String> = repo_c
        .all_refs()
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let tracked = gemel::sync::tracked_refs(&repo_c, &format!("127.0.0.1:{port}")).unwrap();
    assert!(
        tracked
            .iter()
            .any(|(n, _)| { n.starts_with(&format!("refs/remotes/127.0.0.1:{port}/")) }),
        "no tracking refs under the URL-derived name; refs: {all:?}"
    );
}

// ---------------------------------------------------------------------------
// FRF court runner (HOSTED.md §7; brief §38)
// ---------------------------------------------------------------------------

#[test]
fn court_default_policy_denies_execution_and_status_never_executes() {
    let root = temp_root("court-deny");
    let repo = seed(&root);
    let ev = insert_court_evidence(&repo, "touch executed-marker", "src/lib.rs");
    assert_eq!(execution_policy(&repo), "never_auto_execute");

    // The default policy denies re-execution.
    let e = gemel::court::run_court(&repo, &ev, false, 60).unwrap_err();
    assert!(
        e.to_string().contains("POLICY_DENIED"),
        "default policy must deny: {e}"
    );
    // `--allow` does not override never_auto_execute.
    let e = gemel::court::run_court(&repo, &ev, true, 60).unwrap_err();
    assert!(e.to_string().contains("POLICY_DENIED"));

    // status / protocol never execute: the closure is unchanged and no
    // marker file appears.
    let head = repo.read_ref(gemel::store::REF_HEAD).unwrap().unwrap();
    let before = gemel::sync::reachable_ids(&repo, &[head]).unwrap();
    let bin = env!("CARGO_BIN_EXE_gemel");
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let after = gemel::sync::reachable_ids(&repo, &[head]).unwrap();
    assert_eq!(before, after, "status must not add objects");
    assert!(
        !root.join("executed-marker").exists(),
        "status must never execute recorded commands"
    );
}

#[test]
fn court_policy_gated_allow_publishes_fresh_evidence() {
    let root = temp_root("court-gated");
    let repo = seed(&root);
    write_config(
        &repo,
        vec![Field::new(0x04, Value::Str("policy_gated".into()))],
    );

    // Without --allow the explicit action is still required.
    let ev = insert_court_evidence(&repo, "true", "src/lib.rs");
    let e = gemel::court::run_court(&repo, &ev, false, 60).unwrap_err();
    assert!(e.to_string().contains("POLICY_DENIED"));

    // With --allow a trivial command runs and publishes a fresh observation.
    let head = repo
        .read_ref(gemel::store::REF_STATE_HEAD)
        .unwrap()
        .unwrap();
    let out = gemel::court::run_court(&repo, &ev, true, 60).unwrap();
    assert_eq!(out.outcome, "pass");
    assert_eq!(out.exit_code, Some(0));
    // Fresh evidence: court-runner producer, evaluated against the head
    // state, reproduction record marks it replayable and policy-required.
    let obj = repo.load(&out.evidence).unwrap();
    assert_eq!(obj.family, gemel::family::Family::Evidence);
    let fs = obj.field_sequence().unwrap();
    let producer = gemel::query::gid_field(fs, 0x01).unwrap();
    let pobj = repo.load(&producer).unwrap();
    let pfs = pobj.field_sequence().unwrap();
    assert_eq!(gemel::query::str_field(pfs, 0x02).unwrap(), "court-runner");
    assert_eq!(gemel::query::str_field(fs, 0x05).unwrap(), "true");
    assert_eq!(
        gemel::query::gid_field(fs, 0x11).unwrap(),
        head,
        "court evidence must be evaluated against the head state"
    );
    let repro = gemel::query::record_field(fs, 0x0F).unwrap();
    assert_eq!(
        gemel::query::value_at(repro, 0x01).unwrap(),
        &Value::B(true),
        "court observation must be replayable"
    );
    assert_ne!(
        out.evidence, ev,
        "a court publishes new evidence, never rewrites"
    );

    // A failing command records the observed failure, not a rewrite.
    let ev_fail = insert_court_evidence(&repo, "exit 3", "src/lib.rs");
    let out = gemel::court::run_court(&repo, &ev_fail, true, 60).unwrap();
    assert_eq!(out.outcome, "fail");
    assert_eq!(out.exit_code, Some(3));
    let obj = repo.load(&out.evidence).unwrap();
    let fs = obj.field_sequence().unwrap();
    let result = gemel::query::record_field(fs, 0x0D).unwrap();
    assert_eq!(gemel::query::str_field(result, 0x01).unwrap(), "fail");
    assert_eq!(gemel::query::value_at(result, 0x03).unwrap(), &Value::I(3));
}

#[test]
fn court_allowlist_permits_matching_commands_only() {
    let root = temp_root("court-allow");
    let repo = seed(&root);
    write_config(
        &repo,
        vec![Field::new(0x04, Value::Str("allowlist".into()))],
    );
    // Write the allowlist: the literal command `true`, and any `printf*`.
    gemel::court::write_allowlist(&repo, &["true", "printf*"]).unwrap();

    let ok = insert_court_evidence(&repo, "true", "src/lib.rs");
    assert_eq!(
        gemel::court::run_court(&repo, &ok, false, 60)
            .unwrap()
            .outcome,
        "pass"
    );
    let pr = insert_court_evidence(&repo, "printf hi", "src/lib.rs");
    assert_eq!(
        gemel::court::run_court(&repo, &pr, false, 60)
            .unwrap()
            .outcome,
        "pass"
    );
    // A command outside the allowlist is denied even with --allow.
    let denied = insert_court_evidence(&repo, "ls /tmp", "src/lib.rs");
    let e = gemel::court::run_court(&repo, &denied, true, 60).unwrap_err();
    assert!(e.to_string().contains("POLICY_DENIED"));
}

#[test]
fn court_timeout_is_inconclusive() {
    let root = temp_root("court-to");
    let repo = seed(&root);
    write_config(
        &repo,
        vec![Field::new(0x04, Value::Str("policy_gated".into()))],
    );
    let ev = insert_court_evidence(&repo, "sleep 5", "src/lib.rs");
    let out = gemel::court::run_court(&repo, &ev, true, 1).unwrap();
    assert_eq!(out.outcome, "inconclusive");
    assert_eq!(out.exit_code, None);
    assert!(out.detail.contains("timed out"));
}

#[test]
fn court_cli_end_to_end() {
    let root = temp_root("court-cli");
    let repo = seed(&root);
    write_config(
        &repo,
        vec![Field::new(0x04, Value::Str("policy_gated".into()))],
    );
    let ev = insert_court_evidence(&repo, "exit 7", "src/lib.rs");
    let bin = env!("CARGO_BIN_EXE_gemel");
    // Denied without --allow (structured error on stderr, exit code 2).
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["court", &ev.to_string(), "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("POLICY_DENIED"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Allowed: structured observation.
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["court", &ev.to_string(), "--allow", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "court failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["result"]["outcome"], "fail");
    assert_eq!(v["result"]["exit_code"], 7);
    // The published evidence is queryable through the ordinary surface.
    let eid: gemel::gid::Gid = v["result"]["evidence"].as_str().unwrap().parse().unwrap();
    let shown = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["show", &eid.to_string(), "--json"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&shown.stdout)).unwrap();
    assert_eq!(v["result"]["family"], "evidence");
}

// ---------------------------------------------------------------------------
// Policy through the CLI (extension of Phase 7; HOSTED.md §7)
// ---------------------------------------------------------------------------

#[test]
fn policy_gaps_reported_and_readiness_not_ready() {
    let root = temp_root("pol-cli");
    let repo = seed(&root);
    // Configure a matrix requiring correctness on linux/x86_64; no evidence
    // has an environment, so the gap exists.
    let entry = Value::Record(vec![
        Field::new(0x01, Value::Str("correctness".into())),
        Field::new(
            0x02,
            Value::Array(vec![Value::Record(vec![
                Field::new(0x01, Value::Str("linux".into())),
                Field::new(0x02, Value::Str("x86_64".into())),
            ])]),
        ),
    ]);
    write_config(
        &repo,
        vec![
            Field::new(0x04, Value::Str("never_auto_execute".into())),
            Field::new(
                0x08,
                Value::Record(vec![Field::new(0x01, Value::Array(vec![entry]))]),
            ),
        ],
    );
    let bin = env!("CARGO_BIN_EXE_gemel");
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["policy", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let gaps = v["result"]["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0]["kind"], "correctness");
    // Readiness is NOT_READY while required verification is missing.
    let out = Command::new(bin)
        .arg("--repo")
        .arg(&root)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["result"]["readiness"], "NOT_READY");
}
