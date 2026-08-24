//! The Gemel repository (STORAGE.md).
//!
//! A repository is a directory containing a `.gemel/` metadata directory.
//! Canonical state = content-addressed immutable objects + mutable refs; the
//! SQLite index and workspace metadata are derived and disposable.

pub mod fsck;
pub mod index;
pub mod lock;
pub mod objects;
pub mod refs;
pub mod retention;
pub mod tombstone;

use crate::decode::decode_object;
use crate::encode::encode_object;
use crate::error::ObjectError;
use crate::gid::Gid;
use crate::hash::gid_from_envelope;
use crate::limits::Limits;
use crate::value::Object;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The repository metadata directory name.
pub const META_DIR: &str = ".gemel";

/// Ref namespace paths (STORAGE.md §4.1).
pub const REF_HEAD: &str = "refs/head";
pub const REF_STATE_HEAD: &str = "refs/state/head";
pub const REF_CONFIG: &str = "refs/config";
pub const REF_NAMES: &str = "refs/names";
pub const REF_TRAJECTORIES: &str = "refs/trajectories";
/// The namespace anchoring Git interchange mappings (GIT_INTEROP.md §2).
pub const REF_MAPPINGS: &str = "refs/mappings";
pub const REF_CASES: &str = "refs/cases";
pub const REF_RELEASES: &str = "refs/releases";
pub const REF_CHECKPOINTS: &str = "refs/checkpoints";
pub const REF_RECONCILIATIONS: &str = "refs/reconciliations";

/// Errors produced by the repository layer.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Object(ObjectError),
    /// The directory is not a Gemel repository (no `.gemel/`).
    NotARepository(PathBuf),
    /// The repository already exists.
    RepoAlreadyExists(PathBuf),
    /// A ref name violates the naming rules (INVARIANTS REF-01).
    RefNameInvalid(String),
    /// The ref file exists but does not parse.
    RefCorrupt {
        name: String,
        detail: String,
    },
    /// The requested object is not present.
    ObjectNotFound(Gid),
    /// The requested object was pruned by retention policy.
    ObjectPruned {
        id: Gid,
        tombstone: tombstone::Tombstone,
    },
    /// An object file's bytes do not hash to its name.
    ObjectCorrupt {
        id: Gid,
        detail: String,
    },
    /// An object id exists on disk with different bytes.
    HashCollision {
        id: Gid,
    },
    /// A name or identity did not resolve.
    Unresolved(String),
    /// No change is in progress.
    NoPendingChange,
    /// A change is already in progress.
    PendingChangeAlreadyExists,
    /// The repository has no head change.
    NoHead,
    /// Invalid state transition or argument.
    Invalid(String),
    /// Derived-index failure.
    Index(String),
    /// Locking failure.
    Lock(String),
    /// A configured limit was exceeded.
    Limit {
        kind: &'static str,
        limit: u64,
        found: u64,
    },
    /// A path escaped the workspace or violates canonical rules.
    Path(String),
    /// A filesystem entry type is unsupported (e.g. sockets, devices).
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Object(e) => write!(f, "object error: {e}"),
            Error::NotARepository(p) => write!(f, "not a gemel repository: {}", p.display()),
            Error::RepoAlreadyExists(p) => write!(f, "repository already exists: {}", p.display()),
            Error::RefNameInvalid(name) => write!(f, "invalid ref name: {name:?}"),
            Error::RefCorrupt { name, detail } => write!(f, "corrupt ref {name:?}: {detail}"),
            Error::ObjectNotFound(id) => write!(f, "object not found: {id}"),
            Error::ObjectPruned { id, .. } => write!(f, "object pruned by retention policy: {id}"),
            Error::ObjectCorrupt { id, detail } => write!(f, "corrupt object {id}: {detail}"),
            Error::HashCollision { id } => {
                write!(f, "hash collision: object {id} exists with different bytes")
            }
            Error::Unresolved(name) => write!(f, "unresolved name or identity: {name:?}"),
            Error::NoPendingChange => write!(f, "no change in progress (run `gemel change begin`)"),
            Error::PendingChangeAlreadyExists => {
                write!(
                    f,
                    "a change is already in progress (run `gemel change finish`)"
                )
            }
            Error::NoHead => write!(f, "repository has no head change"),
            Error::Invalid(msg) => write!(f, "invalid: {msg}"),
            Error::Index(msg) => write!(f, "index error: {msg}"),
            Error::Lock(msg) => write!(f, "lock error: {msg}"),
            Error::Limit { kind, limit, found } => {
                write!(f, "limit exceeded: {kind} (limit {limit}, found {found})")
            }
            Error::Path(msg) => write!(f, "path error: {msg}"),
            Error::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<ObjectError> for Error {
    fn from(e: ObjectError) -> Self {
        Error::Object(e)
    }
}

/// A Gemel repository handle.
#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
    meta: PathBuf,
}

/// Options for `Repo::init`.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Author name for the initial producer (kind `human`).
    pub author_name: Option<String>,
    /// Author email for the initial producer.
    pub author_email: Option<String>,
}

impl Repo {
    /// Initializes a new repository at `root`, creating `.gemel/` and the
    /// initial canonical objects (default config, default producer).
    ///
    /// A `.gemel/` directory carrying only exchange material (fresh clone
    /// before bootstrap; tracked `.gitignore`/`exchange/`, no `meta.json`) is
    /// completed rather than rejected: native metadata directories are added
    /// without clobbering tracked exchange data (EXCHANGE.md §17).
    pub fn init(root: &Path, opts: &InitOptions) -> Result<Repo, Error> {
        let meta = root.join(META_DIR);
        if meta.join("meta.json").exists() {
            return Err(Error::RepoAlreadyExists(root.to_path_buf()));
        }
        if !meta.exists() {
            std::fs::create_dir_all(&meta)?;
        }
        std::fs::create_dir_all(meta.join("objects"))?;
        std::fs::create_dir_all(meta.join("refs").join("names"))?;
        std::fs::create_dir_all(meta.join("refs").join("trajectories"))?;
        std::fs::create_dir_all(meta.join("refs").join("cases"))?;
        std::fs::create_dir_all(meta.join("refs").join("releases"))?;
        std::fs::create_dir_all(meta.join("journal"))?;
        std::fs::create_dir_all(meta.join("index"))?;
        std::fs::create_dir_all(meta.join("worktrees").join("default"))?;
        std::fs::File::create(meta.join("lock"))?;

        let repo = Repo {
            root: root.to_path_buf(),
            meta: meta.clone(),
        };

        // Default producer (kind human when an author is given, else automation).
        let producer = match &opts.author_name {
            Some(name) => {
                crate::defaults::human_producer_object(name, opts.author_email.as_deref())
            }
            None => crate::defaults::automation_producer_object("gemel"),
        };
        let producer_id = repo.insert_object(&producer)?;

        // Default config object.
        let config = crate::defaults::default_config_object();
        let config_id = repo.insert_object(&config)?;

        // Repository metadata (default producer, name counters).
        let meta_json = serde_json::json!({
            "schema": "gemel.meta.v1",
            "default_producer": producer_id.to_string(),
            "counters": {
                "intent": 0,
                "trajectory": 0,
                "change": 0,
                "state": 0,
                "checkpoint": 0,
                "reconciliation": 0,
            },
        });
        repo.write_meta(&meta_json)?;

        // Wire refs transactionally.
        let txn = refs::RefTransaction {
            ops: vec![refs::RefOp::set(REF_CONFIG, config_id)],
        };
        repo.write_refs(&txn)?;

        Ok(repo)
    }

    /// Opens an existing repository. Performs opportunistic journal recovery
    /// when the write lock is free.
    ///
    /// A `.gemel/` directory that carries only exchange material (a fresh
    /// clone before bootstrap; no `meta.json`) is not a repository yet:
    /// opening it must fail cleanly without touching locks (EXCHANGE.md §17,
    /// §34).
    pub fn open(root: &Path) -> Result<Repo, Error> {
        let meta = root.join(META_DIR);
        if !meta.is_dir() || !meta.join("meta.json").is_file() {
            return Err(Error::NotARepository(root.to_path_buf()));
        }
        let repo = Repo {
            root: root.to_path_buf(),
            meta,
        };
        repo.recover_opportunistic();
        Ok(repo)
    }

    /// Finds the repository root by walking upward from `start`.
    pub fn find(start: &Path) -> Result<Repo, Error> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join(META_DIR).is_dir() {
                return Repo::open(d);
            }
            dir = d.parent();
        }
        Err(Error::NotARepository(start.to_path_buf()))
    }

    /// The repository root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.gemel/` metadata directory.
    pub fn meta_dir(&self) -> &Path {
        &self.meta
    }

    /// The default parse/validation limits (Phase 1 uses the documented
    /// defaults; config-driven limits arrive with retention policy).
    pub fn limits(&self) -> Limits {
        Limits::default()
    }

    /// Resolves a name or textual identity to an object identity.
    ///
    /// Resolution order: exact identity, `refs/names/*`, `refs/trajectories/*`,
    /// `refs/cases/*`, `refs/releases/*`, `refs/reconciliations/*`,
    /// `refs/checkpoints/*`.
    pub fn resolve(&self, name_or_id: &str) -> Result<Gid, Error> {
        if let Ok(gid) = name_or_id.parse::<Gid>() {
            return Ok(gid);
        }
        for ns in [
            REF_NAMES,
            REF_TRAJECTORIES,
            REF_CASES,
            REF_RELEASES,
            REF_RECONCILIATIONS,
            REF_CHECKPOINTS,
            REF_MAPPINGS,
        ] {
            if let Some(gid) = self.read_ref(&format!("{ns}/{name_or_id}"))? {
                return Ok(gid);
            }
        }
        Err(Error::Unresolved(name_or_id.to_string()))
    }

    /// Reads the repository metadata JSON (`.gemel/meta.json`).
    pub fn read_meta(&self) -> Result<serde_json::Value, Error> {
        let path = self.meta.join("meta.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(serde_json::from_str(&text)
                .map_err(|e| Error::Invalid(format!("meta.json: {e}")))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Invalid(
                "repository metadata missing; run fsck".into(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the repository metadata JSON atomically (callers should hold
    /// the writer lock).
    pub fn write_meta(&self, value: &serde_json::Value) -> Result<(), Error> {
        let path = self.meta.join("meta.json");
        let mut bytes =
            serde_json::to_vec_pretty(value).map_err(|e| Error::Invalid(e.to_string()))?;
        bytes.push(b'\n');
        objects::write_atomic(&path, &bytes)?;
        Ok(())
    }

    /// Acquires the exclusive writer lock and runs `f` under it.
    pub fn with_write_lock<T>(&self, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
        lock::with_write_lock(&self.meta.join("lock"), f)
    }

    /// Runs `f` under the writer lock if it is free (non-blocking).
    pub(crate) fn try_with_write_lock<T>(
        &self,
        f: impl FnOnce() -> Result<T, Error>,
    ) -> Option<Result<T, Error>> {
        lock::try_with_write_lock(&self.meta.join("lock"), f)
    }

    /// Runs journal recovery if the write lock is free (best-effort).
    /// When recovery rolls back an interrupted transaction, the canonical ref
    /// set changed; the derived index is rebuilt so it stays consistent.
    fn recover_opportunistic(&self) {
        let _ = self.try_with_write_lock(|| {
            if refs::recover_unlocked(&self.meta)? {
                index::rebuild(self)?;
            }
            Ok::<(), Error>(())
        });
    }

    /// Inserts a canonical object (validate, hash, atomic publish, index).
    pub fn insert_object(&self, obj: &Object) -> Result<Gid, Error> {
        let bytes = encode_object(obj, &self.limits())?;
        self.insert_bytes(&bytes)
    }

    /// Inserts canonical bytes and returns the identity.
    pub fn insert_bytes(&self, bytes: &[u8]) -> Result<Gid, Error> {
        // Validate before publishing (fail closed).
        let decoded = decode_object(bytes, &self.limits())?;
        let id = gid_from_envelope(bytes).expect("validated envelope has a family");
        if decoded.family != id.family() {
            return Err(Error::Invalid("envelope family mismatch".into()));
        }
        objects::insert(&self.meta, id, bytes, &self.limits())?;
        self.index_note_insert(&decoded, id, bytes.len() as u64)?;
        Ok(id)
    }

    /// Reads and decodes an object; distinguishes pruned objects via
    /// tombstones.
    pub fn read_object(&self, id: &Gid) -> Result<ReadOutcome, Error> {
        match objects::read(&self.meta, id, &self.limits()) {
            Ok(bytes) => {
                let obj = decode_object(&bytes, &self.limits())?;
                if obj.family != id.family() {
                    return Err(Error::ObjectCorrupt {
                        id: *id,
                        detail: "decoded family differs from identity".into(),
                    });
                }
                Ok(ReadOutcome::Object(obj))
            }
            Err(objects::ObjectError::NotFound) => match tombstone::read(&self.meta, id)? {
                Some(t) => Ok(ReadOutcome::Pruned(t)),
                None => Err(Error::ObjectNotFound(*id)),
            },
            Err(e) => Err(e.into()),
        }
    }

    /// Reads an object's canonical bytes, verifying the identity hash.
    pub fn read_bytes(&self, id: &Gid) -> Result<Vec<u8>, Error> {
        objects::read(&self.meta, id, &self.limits()).map_err(|e| match e {
            objects::ObjectError::NotFound => Error::ObjectNotFound(*id),
            other => other.into(),
        })
    }

    /// Reads and decodes an object, returning it (fails on pruned objects).
    pub fn load(&self, id: &Gid) -> Result<Object, Error> {
        match self.read_object(id)? {
            ReadOutcome::Object(obj) => Ok(obj),
            ReadOutcome::Pruned(t) => Err(Error::ObjectPruned {
                id: *id,
                tombstone: t,
            }),
        }
    }

    /// Whether an object is present (ignores tombstones).
    pub fn has_object(&self, id: &Gid) -> Result<bool, Error> {
        objects::exists(&self.meta, id).map_err(Into::into)
    }

    // -- refs -------------------------------------------------------------

    /// Reads a ref (absent refs yield `None`).
    pub fn read_ref(&self, name: &str) -> Result<Option<Gid>, Error> {
        refs::read(&self.meta, name)
    }

    /// Applies a journaled ref transaction (acquires the writer lock).
    pub fn write_refs(&self, txn: &refs::RefTransaction) -> Result<(), Error> {
        self.with_write_lock(|| {
            refs::apply_unlocked(&self.meta, txn)?;
            self.index_note_refs(txn)?;
            Ok(())
        })
    }

    /// Applies a journaled ref transaction; the caller holds the writer lock.
    /// The derived index is updated best-effort (INVARIANTS INDEX-01).
    pub fn apply_refs_unlocked(&self, txn: &refs::RefTransaction) -> Result<(), Error> {
        refs::apply_unlocked(&self.meta, txn)?;
        self.index_note_refs(txn)?;
        Ok(())
    }

    /// All refs as (name, gid), sorted by name.
    pub fn all_refs(&self) -> Result<Vec<(String, Gid)>, Error> {
        refs::all(&self.meta)
    }

    /// The human name (ref) pointing at `gid`, if any. Human-facing
    /// namespaces (`refs/names`, then trajectories/cases/releases) are
    /// preferred over structural refs such as `refs/head`.
    pub fn name_of(&self, gid: &Gid) -> Result<Option<String>, Error> {
        let rank = |name: &str| -> u8 {
            if name.starts_with("refs/names/") {
                0
            } else if name.starts_with("refs/trajectories/") {
                1
            } else if name.starts_with("refs/cases/") {
                2
            } else if name.starts_with("refs/releases/") {
                3
            } else {
                4
            }
        };
        let mut best: Option<(u8, String)> = None;
        for (name, g) in refs::all(&self.meta)? {
            if g == *gid {
                let r = rank(&name);
                let short = name.rsplit('/').next().unwrap_or(&name).to_string();
                if best.as_ref().map(|(br, _)| r < *br).unwrap_or(true) {
                    best = Some((r, short));
                }
            }
        }
        Ok(best.map(|(_, n)| n))
    }

    // -- index ------------------------------------------------------------

    fn index_note_insert(&self, obj: &Object, id: Gid, size: u64) -> Result<(), Error> {
        if let Err(e) = index::note_insert(self, obj, id, size) {
            index::mark_stale(self);
            let _ = e;
        }
        Ok(())
    }

    fn index_note_refs(&self, txn: &refs::RefTransaction) -> Result<(), Error> {
        if let Err(e) = index::note_refs(self, txn) {
            index::mark_stale(self);
            let _ = e;
        }
        Ok(())
    }

    /// Rebuilds the derived index from canonical objects and refs.
    pub fn rebuild_index(&self) -> Result<(), Error> {
        self.with_write_lock(|| index::rebuild(self))
    }

    /// A canonical object scan: every object on disk, decoded, deterministic
    /// order. Corrupt objects are skipped (fsck reports them). Used by
    /// derived queries as the always-correct slow path.
    pub fn scan_canonical(&self) -> Vec<(Gid, Object)> {
        index::scan_canonical(self)
    }

    /// Whether the derived index may be consulted as an accelerator (never
    /// as an oracle; INVARIANTS DER-01).
    pub fn index_is_fresh(&self) -> bool {
        index::is_fresh(self)
    }

    /// Runs the full repository verification (STORAGE.md §8).
    pub fn fsck(&self, opts: &fsck::FsckOptions) -> Result<fsck::FsckReport, Error> {
        fsck::run(self, opts)
    }
}

/// Outcome of reading an object.
#[derive(Debug, Clone)]
pub enum ReadOutcome {
    /// The object is present and valid.
    Object(Object),
    /// The object was pruned by retention policy (tombstone present).
    Pruned(tombstone::Tombstone),
}

/// Unix milliseconds since the epoch (metadata timestamps).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Test helpers shared by the store modules.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Creates a unique temporary directory for a test.
    pub fn temp_root(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("gemel-test-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Initializes a fresh repository in a temp directory.
    pub fn fresh_repo(tag: &str) -> (Repo, PathBuf) {
        let root = temp_root(tag);
        let repo = Repo::init(&root, &InitOptions::default()).expect("init");
        (repo, root)
    }
}
