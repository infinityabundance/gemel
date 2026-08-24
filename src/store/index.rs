//! The disposable derived index (STORAGE.md §5).
//!
//! A SQLite database (`index/gemel.db`, WAL mode) accelerates queries. It is
//! **never** the source of truth: canonical objects + refs are. Corruption is
//! repaired by rebuild, never by history loss. The index schema version is
//! recorded in `meta`; a stale index is flagged and rebuilt by
//! `fsck --rebuild-index`.

use crate::decode::decode_object;
use crate::gid::Gid;
use crate::hash::gid_from_envelope;
use crate::store::objects;
use crate::store::refs;
use crate::store::{Error, Repo};
use crate::value::{Body, Object, Value};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// The derived-index schema version.
pub const INDEX_SCHEMA_VERSION: i64 = 1;

const META_SCHEMA_KEY: &str = "schema_version";
const META_STALE_KEY: &str = "stale";

/// The on-disk index database path.
pub fn db_path(meta: &Path) -> PathBuf {
    meta.join("index").join("gemel.db")
}

fn open_conn(repo: &Repo) -> Result<Connection, Error> {
    let path = db_path(repo.meta_dir());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path).map_err(|e| Error::Index(e.to_string()))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| Error::Index(e.to_string()))?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| Error::Index(e.to_string()))?;
    ensure_schema(&conn)?;
    // Version check: a freshly created database records the schema and starts
    // clean; a pre-existing database with an older/missing schema version is
    // flagged stale (same connection; `fsck --rebuild-index` restores). The
    // stored version is TEXT (exact integer projection), so read it as a
    // string and parse.
    let stored_text: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_SCHEMA_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| Error::Index(e.to_string()))?;
    let stored = stored_text.as_deref().and_then(|s| s.parse::<i64>().ok());
    match stored {
        None => {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![META_SCHEMA_KEY, INDEX_SCHEMA_VERSION.to_string()],
            )
            .map_err(|e| Error::Index(e.to_string()))?;
            conn.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![META_STALE_KEY, "0"],
            )
            .map_err(|e| Error::Index(e.to_string()))?;
        }
        Some(v) if v != INDEX_SCHEMA_VERSION => {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                params![META_STALE_KEY, "1"],
            )
            .map_err(|e| Error::Index(e.to_string()))?;
        }
        Some(_) => {}
    }
    Ok(conn)
}

fn ensure_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS objects(
             id TEXT PRIMARY KEY,
             family INTEGER,
             schemever INTEGER,
             size INTEGER
         );
         CREATE TABLE IF NOT EXISTS edges(
             from_id TEXT,
             to_id TEXT,
             kind TEXT,
             ordinal INTEGER
         );
         CREATE INDEX IF NOT EXISTS edges_from ON edges(from_id);
         CREATE INDEX IF NOT EXISTS edges_to ON edges(to_id);
         CREATE TABLE IF NOT EXISTS refs(
             name TEXT PRIMARY KEY,
             gid TEXT
         );
         CREATE TABLE IF NOT EXISTS subjects(
             gid TEXT,
             subject TEXT,
             kind TEXT
         );
         CREATE TABLE IF NOT EXISTS claim_index(
             claim TEXT,
             subject TEXT
         );
         CREATE TABLE IF NOT EXISTS meta(
             key TEXT PRIMARY KEY,
             value TEXT
         );",
    )
    .map_err(|e| Error::Index(e.to_string()))?;
    Ok(())
}

/// Best-effort index update after an object insert (failures mark stale).
pub fn note_insert(repo: &Repo, obj: &Object, id: Gid, size: u64) -> Result<(), Error> {
    let conn = open_conn(repo)?;
    conn.execute(
        "INSERT OR IGNORE INTO objects(id, family, schemever, size) VALUES (?1, ?2, ?3, ?4)",
        params![
            id.to_string(),
            obj.family.code() as i64,
            obj.schemever as i64,
            size as i64
        ],
    )
    .map_err(|e| Error::Index(e.to_string()))?;
    conn.execute(
        "DELETE FROM edges WHERE from_id = ?1",
        params![id.to_string()],
    )
    .map_err(|e| Error::Index(e.to_string()))?;
    for (kind, to, ordinal) in edges_of(obj) {
        conn.execute(
            "INSERT INTO edges(from_id, to_id, kind, ordinal) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), to.to_string(), kind, ordinal as i64],
        )
        .map_err(|e| Error::Index(e.to_string()))?;
    }
    Ok(())
}

/// Best-effort index update after a ref transaction.
pub fn note_refs(repo: &Repo, txn: &refs::RefTransaction) -> Result<(), Error> {
    let conn = open_conn(repo)?;
    for op in &txn.ops {
        match op.new {
            Some(gid) => {
                conn.execute(
                    "INSERT OR REPLACE INTO refs(name, gid) VALUES (?1, ?2)",
                    params![op.name, gid.to_string()],
                )
                .map_err(|e| Error::Index(e.to_string()))?;
            }
            None => {
                conn.execute("DELETE FROM refs WHERE name = ?1", params![op.name])
                    .map_err(|e| Error::Index(e.to_string()))?;
            }
        }
    }
    Ok(())
}

/// Marks the index stale (best-effort; never fails the caller).
pub fn mark_stale(repo: &Repo) {
    let _ = (|| -> Result<(), Error> {
        let conn = open_conn(repo)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
            params![META_STALE_KEY, "1"],
        )
        .map_err(|e| Error::Index(e.to_string()))?;
        Ok(())
    })();
}

/// Opens a connection for read-only queries (index may be stale; callers
/// should treat results as derived acceleration).
pub fn open_for_query(repo: &Repo) -> Result<Connection, Error> {
    open_conn(repo)
}

/// Whether the index is flagged stale.
pub fn is_stale(repo: &Repo) -> Result<bool, Error> {
    let conn = open_conn(repo)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_STALE_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| Error::Index(e.to_string()))?;
    Ok(v.as_deref() == Some("1"))
}

/// The ref mirror from the index.
pub fn refs_mirror(repo: &Repo) -> Result<Vec<(String, Gid)>, Error> {
    let conn = open_conn(repo)?;
    let mut stmt = conn
        .prepare("SELECT name, gid FROM refs ORDER BY name")
        .map_err(|e| Error::Index(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| Error::Index(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        let (name, gid_text) = r.map_err(|e| Error::Index(e.to_string()))?;
        let gid = gid_text
            .parse::<Gid>()
            .map_err(|e| Error::Index(e.to_string()))?;
        out.push((name, gid));
    }
    Ok(out)
}

/// Indexed objects as (id text, family code).
pub fn indexed_objects(repo: &Repo) -> Result<Vec<(String, i64)>, Error> {
    let conn = open_conn(repo)?;
    let mut stmt = conn
        .prepare("SELECT id, family FROM objects ORDER BY id")
        .map_err(|e| Error::Index(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| Error::Index(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Index(e.to_string()))?);
    }
    Ok(out)
}

/// Extracts the graph edges of an object: (kind, target, ordinal) per GID
/// value, using schema field names as kinds (OBJECT_MODEL.md §7.1).
pub fn edges_of(obj: &Object) -> Vec<(String, Gid, usize)> {
    let mut out = Vec::new();
    let schema = crate::spec::schema_for(obj.family);
    if let Body::Fields(fields) = &obj.body {
        for field in fields {
            let kind = schema.field(field.tag).map(|s| s.name).unwrap_or("?");
            collect_edges(&field.value, kind, &mut out);
        }
    }
    out
}

fn collect_edges(value: &Value, kind: &str, out: &mut Vec<(String, Gid, usize)>) {
    match value {
        Value::Gid(g) => out.push((kind.to_string(), *g, 0)),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::Gid(g) => out.push((kind.to_string(), *g, i)),
                    Value::Record(fields) => {
                        for f in fields {
                            collect_edges(&f.value, kind, out);
                        }
                    }
                    other => collect_edges(other, kind, out),
                }
            }
        }
        Value::Record(fields) => {
            for f in fields {
                collect_edges(&f.value, kind, out);
            }
        }
        _ => {}
    }
}

/// Rebuilds the index from canonical objects and refs (caller holds the
/// writer lock). Writes to a fresh database and renames it into place.
pub fn rebuild(repo: &Repo) -> Result<(), Error> {
    let target = db_path(repo.meta_dir());
    let tmp = target.with_extension("db.rebuild");
    let _ = std::fs::remove_file(&tmp);

    let conn = Connection::open(&tmp).map_err(|e| Error::Index(e.to_string()))?;
    conn.pragma_update(None, "journal_mode", "OFF")
        .map_err(|e| Error::Index(e.to_string()))?;
    ensure_schema(&conn)?;

    let limits = repo.limits();
    let mut indexed = 0usize;
    for path in objects::scan(repo.meta_dir())? {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let obj = match decode_object(&bytes, &limits) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let id = match gid_from_envelope(&bytes) {
            Some(id) => id,
            None => continue,
        };
        if id.digest() != &crate::hash::object_id_bytes(&bytes) {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO objects(id, family, schemever, size) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                obj.family.code() as i64,
                obj.schemever as i64,
                bytes.len() as i64
            ],
        )
        .map_err(|e| Error::Index(e.to_string()))?;
        for (kind, to, ordinal) in edges_of(&obj) {
            conn.execute(
                "INSERT INTO edges(from_id, to_id, kind, ordinal) VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), to.to_string(), kind, ordinal as i64],
            )
            .map_err(|e| Error::Index(e.to_string()))?;
        }
        indexed += 1;
    }

    for (name, gid) in refs::all(repo.meta_dir())? {
        conn.execute(
            "INSERT OR REPLACE INTO refs(name, gid) VALUES (?1, ?2)",
            params![name, gid.to_string()],
        )
        .map_err(|e| Error::Index(e.to_string()))?;
    }

    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        params![META_SCHEMA_KEY, INDEX_SCHEMA_VERSION.to_string()],
    )
    .map_err(|e| Error::Index(e.to_string()))?;
    conn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
        params![META_STALE_KEY, "0"],
    )
    .map_err(|e| Error::Index(e.to_string()))?;
    drop(conn);

    // Publish the rebuilt database atomically. The previous database and its
    // WAL sidecars must be removed first: a leftover `-wal`/`-shm` from an
    // earlier incarnation would be recovered into the fresh file (SQLite
    // validates the WAL against the database header and would reject it,
    // leaving the index unreadable).
    let _ = std::fs::remove_file(&target);
    for ext in ["-wal", "-shm"] {
        let side = format!("{}{}", target.display(), ext);
        let _ = std::fs::remove_file(&side);
    }
    std::fs::rename(&tmp, &target).map_err(|e| Error::Index(e.to_string()))?;
    let _ = indexed;
    Ok(())
}

/// Removes the index database entirely (used by fsck --repair fallback).
pub fn remove(repo: &Repo) -> Result<(), Error> {
    let path = db_path(repo.meta_dir());
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // WAL sidecars.
    for ext in ["-wal", "-shm"] {
        let side = format!("{}{}", path.display(), ext);
        let _ = std::fs::remove_file(&side);
    }
    Ok(())
}
