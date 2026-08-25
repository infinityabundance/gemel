//! Native distributed operation (Phase 6; SPECIFICATION.md Phase 6,
//! STORAGE.md §10, GIT_INTEROP.md §6).
//!
//! Gemel synchronization is separate from Git interchange. This module
//! implements the transport-agnostic sync protocol:
//!
//! ```text
//! negotiate (want/missing by content identity)
//!     → transfer verified canonical envelopes (gemlpack)
//!     → atomically publish refs (never over partial verification)
//! ```
//!
//! Content addressing makes negotiation exact and deduplicating by
//! construction: an object is present iff its identity exists locally, so a
//! re-push transfers nothing and a resumed fetch re-negotiates from the new
//! have-set. Refs are the only mutable state exchanged, and every ref update
//! is validated (names, presence of the referenced closure) before it is
//! applied on the receiving side.
//!
//! Security: transports are untrusted. Every envelope is re-verified against
//! its advertised identity before insertion; conflicting identities (same id,
//! different bytes) are fatal (the store rejects them). Nothing is executed
//! during sync. Network authentication/authorization is the transport's
//! concern (THREAT_MODEL.md §10); the protocol carries integrity, never
//! implicit authority.

pub mod gemlpack;

use crate::exchange::export::write_atomic_fsync;
use crate::gid::Gid;
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::{Error, Repo, REF_HEAD, REF_STATE_HEAD};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The ref namespace where fetched remote refs are tracked locally.
pub const REF_REMOTES: &str = "refs/remotes";
/// The remotes configuration file name (inside the metadata directory).
pub const REMOTES_FILE: &str = "remotes.json";

/// Ref namespaces that travel between repositories. Everything else is
/// local bookkeeping (workspaces, exchange-import markers, interchange
/// mappings) and never syncs.
pub fn is_public_ref(name: &str) -> bool {
    name == REF_HEAD
        || name == REF_STATE_HEAD
        || name == crate::store::REF_CONFIG
        || name == "refs/trajectories/current"
        || name.starts_with("refs/names/")
        || name.starts_with("refs/trajectories/")
        || name.starts_with("refs/cases/")
        || name.starts_with("refs/releases/")
        || name.starts_with("refs/checkpoints/")
        || name.starts_with("refs/reconciliations/")
        || name.starts_with("refs/semantic/")
}

/// The public refs of a repository, sorted by name.
pub fn public_refs(repo: &Repo) -> Result<Vec<(String, Gid)>, Error> {
    let mut out: Vec<(String, Gid)> = repo
        .all_refs()?
        .into_iter()
        .filter(|(n, _)| is_public_ref(n))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The reachable object closure of `seeds` (BFS over canonical gid edges),
/// sorted ascending by identity. Blobs are included: native sync carries
/// full content.
pub fn reachable_ids(repo: &Repo, seeds: &[Gid]) -> Result<Vec<Gid>, Error> {
    let mut seen: HashSet<Gid> = HashSet::new();
    let mut queue: Vec<Gid> = seeds.to_vec();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let obj = match repo.load(&id) {
            Ok(o) => o,
            Err(e) => {
                return Err(Error::Invalid(format!(
                    "cannot sync {id}: {e} (incomplete canonical graph)"
                )))
            }
        };
        for (_, to, _) in crate::store::index::edges_of(&obj) {
            queue.push(to);
        }
    }
    let mut out: Vec<Gid> = seen.into_iter().collect();
    out.sort_by_key(|g| g.to_bytes());
    Ok(out)
}

/// The ids of `ids` that the repository lacks.
pub fn missing_ids(repo: &Repo, ids: &[Gid]) -> Result<Vec<Gid>, Error> {
    let mut out = Vec::new();
    for id in ids {
        if !repo.has_object(id)? {
            out.push(*id);
        }
    }
    Ok(out)
}

/// Validates that every object reachable from `refs` is present (used by the
/// receiving side before publishing refs: a ref must never dangle).
pub fn ensure_reachable(repo: &Repo, refs: &[(String, Gid)]) -> Result<(), Error> {
    for (name, gid) in refs {
        let closure = reachable_ids(repo, std::slice::from_ref(gid))?;
        if let Some(missing) = missing_ids(repo, &closure)?.first() {
            return Err(Error::Invalid(format!(
                "cannot publish ref {name}: reachable object {missing} is absent"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// A transport to a remote Gemel repository. Implementations must be safe
/// against hostile remotes: every returned envelope is re-verified by the
/// caller before insertion.
pub trait Transport {
    /// A human description of the remote (for status output).
    fn describe(&self) -> String;
    /// The remote's public refs.
    fn list_refs(&self) -> Result<Vec<(String, Gid)>, Error>;
    /// The remote's reachable closure for `seeds` (ids only).
    fn reachable_ids(&self, seeds: &[Gid]) -> Result<Vec<Gid>, Error>;
    /// Which of `ids` the remote lacks.
    fn missing_ids(&self, ids: &[Gid]) -> Result<Vec<Gid>, Error>;
    /// The envelopes of `ids` (verified by the caller).
    fn fetch_objects(&self, ids: &[Gid]) -> Result<Vec<(Gid, Vec<u8>)>, Error>;
    /// Publishes envelopes on the remote (the remote verifies identities).
    fn push_objects(&self, records: &[(Gid, Vec<u8>)]) -> Result<(), Error>;
    /// Atomically updates the remote's refs (validated on the remote).
    fn update_refs(&self, refs: &[(String, Gid)]) -> Result<(), Error>;
}

/// A transport over a local filesystem path to an initialized Gemel
/// repository (the canonical Phase 6 transport; network transports implement
/// the same trait later).
pub struct FileTransport {
    path: PathBuf,
    remote: Repo,
}

impl FileTransport {
    /// Opens (or, when `init`, initializes) the remote repository at `path`.
    pub fn open(path: &Path, init: bool) -> Result<FileTransport, Error> {
        let path = path.to_path_buf();
        if init {
            if path
                .join(crate::store::META_DIR)
                .join("meta.json")
                .is_file()
            {
                return Err(Error::Invalid(format!(
                    "remote {} is already a gemel repository",
                    path.display()
                )));
            }
            std::fs::create_dir_all(&path)?;
            let repo = Repo::init(&path, &crate::store::InitOptions::default())?;
            let _ = repo;
            // A remote has no working tree to snapshot; the init-time default
            // config and producer are sufficient.
        }
        let remote = Repo::open(&path)?;
        Ok(FileTransport { path, remote })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for FileTransport {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn list_refs(&self) -> Result<Vec<(String, Gid)>, Error> {
        public_refs(&self.remote)
    }

    fn reachable_ids(&self, seeds: &[Gid]) -> Result<Vec<Gid>, Error> {
        reachable_ids(&self.remote, seeds)
    }

    fn missing_ids(&self, ids: &[Gid]) -> Result<Vec<Gid>, Error> {
        missing_ids(&self.remote, ids)
    }

    fn fetch_objects(&self, ids: &[Gid]) -> Result<Vec<(Gid, Vec<u8>)>, Error> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push((*id, self.remote.read_bytes(id)?));
        }
        Ok(out)
    }

    fn push_objects(&self, records: &[(Gid, Vec<u8>)]) -> Result<(), Error> {
        for (id, envelope) in records {
            // insert_bytes verifies the identity and rejects id↔bytes
            // conflicts with objects already on the remote (fatal).
            let got = self.remote.insert_bytes(envelope)?;
            if got != *id {
                return Err(Error::Invalid(format!(
                    "remote identity mismatch: advertised {id}, stored {got}"
                )));
            }
        }
        Ok(())
    }

    fn update_refs(&self, refs: &[(String, Gid)]) -> Result<(), Error> {
        // The remote validates names, presence of the closure, and applies
        // the transaction journaled-atomically.
        ensure_reachable(&self.remote, refs)?;
        let ops: Vec<RefOp> = refs.iter().map(|(n, g)| RefOp::set(n, *g)).collect();
        self.remote.with_write_lock(|| {
            self.remote.apply_refs_unlocked(&RefTransaction { ops })?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Remotes configuration
// ---------------------------------------------------------------------------

/// The remotes configuration (`remotes.json`).
#[derive(Debug, Clone, Default)]
pub struct RemotesConfig {
    pub remotes: Vec<(String, String)>,
}

pub fn read_remotes(repo: &Repo) -> Result<RemotesConfig, Error> {
    let path = repo.meta_dir().join(REMOTES_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(RemotesConfig::default()),
    };
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Error::Invalid(format!("malformed {REMOTES_FILE}: {e}")))?;
    let mut out = RemotesConfig::default();
    if let Some(map) = v.get("remotes").and_then(|r| r.as_object()) {
        let mut entries: Vec<(String, String)> = map
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        out.remotes = entries;
    }
    Ok(out)
}

pub fn write_remotes(repo: &Repo, cfg: &RemotesConfig) -> Result<(), Error> {
    let mut map = serde_json::Map::new();
    for (name, path) in &cfg.remotes {
        map.insert(name.clone(), serde_json::Value::String(path.clone()));
    }
    let doc = serde_json::json!({
        "schema": "gemel.remotes.v1",
        "remotes": map,
    });
    let mut bytes = serde_json::to_vec_pretty(&doc)
        .map_err(|e| Error::Invalid(format!("remotes.json serialization: {e}")))?;
    bytes.push(b'\n');
    write_atomic_fsync(&repo.meta_dir().join(REMOTES_FILE), &bytes)
}

pub fn add_remote(repo: &Repo, name: &str, path: &str) -> Result<(), Error> {
    validate_remote_name(name)?;
    let mut cfg = read_remotes(repo)?;
    cfg.remotes.retain(|(n, _)| n != name);
    cfg.remotes.push((name.to_string(), path.to_string()));
    cfg.remotes.sort_by(|a, b| a.0.cmp(&b.0));
    write_remotes(repo, &cfg)
}

pub fn remove_remote(repo: &Repo, name: &str) -> Result<(), Error> {
    let mut cfg = read_remotes(repo)?;
    let before = cfg.remotes.len();
    cfg.remotes.retain(|(n, _)| n != name);
    if cfg.remotes.len() == before {
        return Err(Error::Invalid(format!("no such remote {name:?}")));
    }
    write_remotes(repo, &cfg)
}

pub fn remote_path(repo: &Repo, name: &str) -> Result<PathBuf, Error> {
    let cfg = read_remotes(repo)?;
    cfg.remotes
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| PathBuf::from(p))
        .ok_or_else(|| Error::Invalid(format!("no such remote {name:?}")))
}

fn validate_remote_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(Error::Invalid(format!("invalid remote name {name:?}")));
    }
    Ok(())
}

/// The local tracking name for a remote ref: `refs/names/C1` under remote
/// `origin` becomes `refs/remotes/origin/names/C1`.
fn tracking_ref(name: &str, remote: &str) -> String {
    format!(
        "{REF_REMOTES}/{remote}/{}",
        name.trim_start_matches("refs/")
    )
}

/// A fetch outcome.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub remote: String,
    pub remote_refs: usize,
    pub wanted: usize,
    pub transferred: usize,
    pub inserted: usize,
    pub refs_written: usize,
}

/// The remote ref names under the tracking namespace.
pub fn tracked_refs(repo: &Repo, remote: &str) -> Result<Vec<(String, Gid)>, Error> {
    let prefix = format!("{REF_REMOTES}/{remote}/");
    let mut out: Vec<(String, Gid)> = repo
        .all_refs()?
        .into_iter()
        .filter(|(n, _)| n.starts_with(&prefix))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Fetches a remote: negotiate by content identity, transfer verified
/// envelopes, publish tracking refs. Idempotent and resumable — a re-fetch
/// transfers nothing.
pub fn fetch(repo: &Repo, remote: &str, transport: &dyn Transport) -> Result<FetchOutcome, Error> {
    let remote_refs = transport.list_refs()?;
    let seeds: Vec<Gid> = remote_refs.iter().map(|(_, g)| *g).collect();
    let remote_closure = transport.reachable_ids(&seeds)?;
    let want: Vec<Gid> = remote_closure
        .into_iter()
        .filter(|id| !repo.has_object(id).unwrap_or(false))
        .collect();
    let objects = transport.fetch_objects(&want)?;
    let mut inserted = 0usize;
    for (id, envelope) in &objects {
        // Re-verify the advertised identity and insert (dedup/conflict-safe).
        let got = repo.insert_bytes(envelope)?;
        if got != *id {
            return Err(Error::Invalid(format!(
                "fetch identity mismatch: advertised {id}, stored {got}"
            )));
        }
        inserted += 1;
    }
    // Publish tracking refs only after the whole transfer verified.
    let ops: Vec<RefOp> = remote_refs
        .iter()
        .map(|(n, g)| RefOp::set(&tracking_ref(n, remote), *g))
        .collect();
    let refs_written = ops.len();
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(())
    })?;
    Ok(FetchOutcome {
        remote: remote.to_string(),
        remote_refs: remote_refs.len(),
        wanted: want.len(),
        transferred: objects.len(),
        inserted,
        refs_written,
    })
}

/// A push outcome.
#[derive(Debug, Clone)]
pub struct PushOutcome {
    pub remote: String,
    pub refs_pushed: usize,
    pub missing_on_remote: usize,
    pub transferred: usize,
}

/// Pushes the local public refs to a remote: compute what the remote lacks
/// by content identity, transfer verified envelopes, then atomically update
/// the remote's public refs (validated on the remote).
pub fn push(repo: &Repo, remote: &str, transport: &dyn Transport) -> Result<PushOutcome, Error> {
    let refs = public_refs(repo)?;
    let seeds: Vec<Gid> = refs.iter().map(|(_, g)| *g).collect();
    let local_closure = reachable_ids(repo, &seeds)?;
    let missing = transport.missing_ids(&local_closure)?;
    let records: Vec<(Gid, Vec<u8>)> = missing
        .iter()
        .map(|id| Ok((*id, repo.read_bytes(id)?)))
        .collect::<Result<_, Error>>()?;
    transport.push_objects(&records)?;
    transport.update_refs(&refs)?;
    Ok(PushOutcome {
        remote: remote.to_string(),
        refs_pushed: refs.len(),
        missing_on_remote: missing.len(),
        transferred: records.len(),
    })
}

/// A pull outcome.
#[derive(Debug, Clone)]
pub struct PullOutcome {
    pub fetch: FetchOutcome,
    pub fast_forwarded: bool,
    pub applied_refs: usize,
}

/// Whether `head` is reachable from `ancestor` by causal-parent edges.
fn is_ancestor(repo: &Repo, ancestor: &Gid, head: &Gid) -> Result<bool, Error> {
    if ancestor == head {
        return Ok(true);
    }
    let mut current = Some(*head);
    let mut depth = 0u32;
    while let Some(gid) = current {
        if depth > 1_000_000 {
            return Err(Error::Limit {
                kind: "causal ancestry walk",
                limit: 1_000_000,
                found: depth as u64,
            });
        }
        if gid == *ancestor {
            return Ok(true);
        }
        let obj = match repo.load(&gid) {
            Ok(o) => o,
            Err(_) => return Ok(false),
        };
        let fields = obj.field_sequence().unwrap_or(&[]);
        current = crate::query::gid_list(fields, 0x11).first().copied();
        depth += 1;
    }
    Ok(false)
}

/// Fetches a remote and fast-forwards the local public refs when the remote
/// head descends from (or equals) the local head. A diverged local head is
/// never silently overwritten: the caller reconciles instead.
pub fn pull(repo: &Repo, remote: &str, transport: &dyn Transport) -> Result<PullOutcome, Error> {
    let fetched = fetch(repo, remote, transport)?;
    let remote_refs = tracked_refs(repo, remote)?;
    let remote_head = remote_refs
        .iter()
        .find(|(n, _)| n == &tracking_ref(REF_HEAD, remote))
        .map(|(_, g)| *g);
    let local_head = repo.read_ref(REF_HEAD)?;
    let fast_forwarded = match (local_head, remote_head) {
        (None, _) => true, // fresh repository adopts the remote context
        (Some(local), Some(remote_head)) => is_ancestor(repo, &local, &remote_head)?,
        (Some(_), None) => false,
    };
    if !fast_forwarded {
        return Err(Error::Invalid(
            "local head has diverged from the remote head; pull only fast-forwards. \
             Fetch the remote, inspect refs/remotes/<name>/*, and use `gemel reconcile` \
             to choose a direction"
                .into(),
        ));
    }
    // Apply the remote's public refs locally (names/trajectories/config/…),
    // without ever touching local-only namespaces.
    let mut ops: Vec<RefOp> = Vec::new();
    for (n, g) in &remote_refs {
        if let Some(remote_name) = n.strip_prefix(&format!("{REF_REMOTES}/{remote}/")) {
            let local_name = format!("refs/{remote_name}");
            if is_public_ref(&local_name) {
                ops.push(RefOp::set(&local_name, *g));
            }
        }
    }
    let applied_refs = ops.len();
    if applied_refs == 0 {
        return Ok(PullOutcome {
            fetch: fetched,
            fast_forwarded,
            applied_refs: 0,
        });
    }
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(())
    })?;
    // The workspace follows the pulled head state (snapshot-time identity;
    // the working tree is left untouched).
    if let Some(state) = repo.read_ref(REF_STATE_HEAD)? {
        let _ = crate::workflow::set_workspace_state(repo, state);
    }
    Ok(PullOutcome {
        fetch: fetched,
        fast_forwarded,
        applied_refs,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InitOptions;
    use crate::workflow::{self, BeginOptions, FinishOptions};

    fn temp_root(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("gemel-sync-{tag}-{}-{n}", std::process::id()));
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
        repo
    }

    #[test]
    fn public_refs_exclude_local_bookkeeping() {
        let root = temp_root("pubrefs");
        let repo = seed(&root);
        let refs = public_refs(&repo).unwrap();
        assert!(refs.iter().any(|(n, _)| n == REF_HEAD));
        assert!(refs.iter().any(|(n, _)| n.starts_with("refs/names/")));
        assert!(refs
            .iter()
            .any(|(n, _)| n.starts_with("refs/trajectories/")));
        assert!(!refs.iter().any(|(n, _)| n.starts_with("refs/remotes/")));
    }

    #[test]
    fn push_then_fetch_roundtrips_identical_ids() {
        let a = temp_root("roundtrip-a");
        let b = temp_root("roundtrip-b");
        let repo_a = seed(&a);
        let repo_b = Repo::init(&b, &InitOptions::default()).unwrap();
        let ta = FileTransport::open(&a, false).unwrap(); // transport TO A
                                                          // B is empty apart from its init-time config: pushing B → A moves at
                                                          // most that config, and a second push is a strict no-op.
        let _ = push(&repo_b, "test", &ta).unwrap();
        let out = push(&repo_b, "test", &ta).unwrap();
        assert_eq!(out.missing_on_remote, 0);
        assert_eq!(out.transferred, 0);
        // Fetch from A into B: everything transfers.
        let fetched = fetch(&repo_b, "origin", &ta).unwrap();
        assert!(fetched.transferred > 0);
        // Second fetch: idempotent, nothing transferred.
        let again = fetch(&repo_b, "origin", &ta).unwrap();
        assert_eq!(again.wanted, 0);
        assert_eq!(again.transferred, 0);
        // B now has A's refs under tracking.
        let tracked = tracked_refs(&repo_b, "origin").unwrap();
        assert!(tracked.iter().any(|(n, g)| {
            n == &tracking_ref(REF_HEAD, "origin")
                && *g == repo_a.read_ref(REF_HEAD).unwrap().unwrap()
        }));
        // Pull fast-forwards a fresh repo and the head states agree.
        let pulled = pull(&repo_b, "origin", &ta).unwrap();
        assert!(pulled.fast_forwarded);
        assert_eq!(
            repo_b.read_ref(REF_HEAD).unwrap(),
            repo_a.read_ref(REF_HEAD).unwrap()
        );
    }

    #[test]
    fn push_transfers_only_what_remote_lacks() {
        let a = temp_root("push-a");
        let b = temp_root("push-b");
        let repo_a = seed(&a);
        let repo_b = Repo::init(&b, &InitOptions::default()).unwrap();
        // B fetches A first.
        let ta = FileTransport::open(&a, false).unwrap(); // transport TO A
        fetch(&repo_b, "origin", &ta).unwrap();
        // B makes its own change.
        write_file(
            &b,
            "src/lib.rs",
            "pub fn greet() -> &'static str { \"hi\" }\npub fn bye() -> &'static str { \"bye\" }\n",
        );
        workflow::begin_change(
            &repo_b,
            &BeginOptions {
                intent_summary: Some("bye".into()),
                ..Default::default()
            },
        )
        .unwrap();
        workflow::finish_change(
            &repo_b,
            &FinishOptions {
                summary: "add bye".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // Push B → A: A gains exactly the new objects.
        let pushed = push(&repo_b, "origin", &ta).unwrap();
        assert!(pushed.transferred > 0);
        // Second push: nothing to transfer.
        let again = push(&repo_b, "origin", &ta).unwrap();
        assert_eq!(again.missing_on_remote, 0);
        assert_eq!(again.transferred, 0);
        // A can now fetch B's head back.
        fetch(&repo_a, "other", &ta).unwrap();
        assert_eq!(
            repo_a.read_ref(REF_HEAD).unwrap(),
            repo_b.read_ref(REF_HEAD).unwrap()
        );
    }

    #[test]
    fn diverged_pull_refuses_without_reconcile() {
        let a = temp_root("div-a");
        let b = temp_root("div-b");
        let repo_a = seed(&a);
        let repo_b = Repo::init(&b, &InitOptions::default()).unwrap();
        let ta = FileTransport::open(&a, false).unwrap(); // transport TO A
        pull(&repo_b, "origin", &ta).unwrap();
        // Both at A's head. B diverges.
        write_file(&b, "src/lib.rs", "pub fn local_only() {}\n");
        workflow::begin_change(
            &repo_b,
            &BeginOptions {
                intent_summary: Some("local".into()),
                ..Default::default()
            },
        )
        .unwrap();
        workflow::finish_change(
            &repo_b,
            &FinishOptions {
                summary: "local divergence".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // A advances independently.
        write_file(&a, "src/lib.rs", "pub fn remote_only() {}\n");
        workflow::begin_change(
            &repo_a,
            &BeginOptions {
                intent_summary: Some("remote".into()),
                ..Default::default()
            },
        )
        .unwrap();
        workflow::finish_change(
            &repo_a,
            &FinishOptions {
                summary: "remote divergence".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // Pull from A into B: B's head is not an ancestor of A's head. The
        // objects have already been fetched by the failed pull (pull fetches
        // first), so a second fetch transfers nothing — the tracking refs are
        // the reconciliation surface.
        let err = pull(&repo_b, "origin", &ta).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("diverged"), "unexpected error: {text}");
        assert!(!tracked_refs(&repo_b, "origin").unwrap().is_empty());
        // The failed pull never moved the local head.
        let local_head = repo_b.read_ref(REF_HEAD).unwrap();
        assert!(local_head.is_some());
        let fetched = fetch(&repo_b, "origin", &ta).unwrap();
        assert_eq!(fetched.transferred, 0);
    }
}
