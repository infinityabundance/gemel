//! The content-addressed object store (STORAGE.md §3).
//!
//! Objects live at `objects/<2-hex>/<64-hex>.gce`. Publication is
//! write-temp → verify → atomic rename; every read re-hashes the bytes and
//! verifies the identity; corrupt files are never silently accepted.

use crate::gid::Gid;
use crate::hash::object_id_bytes;
use crate::limits::Limits;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Errors specific to object file access.
#[derive(Debug)]
pub enum ObjectError {
    /// No file and no tombstone.
    NotFound,
    /// The file's bytes do not hash to its identity.
    Corrupt { id: Gid, detail: String },
    /// The object id already exists with different bytes.
    HashCollision { id: Gid },
    /// An I/O error.
    Io(std::io::Error),
    /// The object exceeds the size limit.
    Limit { limit: u64, found: u64 },
}

impl From<std::io::Error> for ObjectError {
    fn from(e: std::io::Error) -> Self {
        ObjectError::Io(e)
    }
}

impl From<ObjectError> for crate::store::Error {
    fn from(e: ObjectError) -> Self {
        match e {
            ObjectError::NotFound => {
                crate::store::Error::Invalid("internal: not found mapped by caller".into())
            }
            ObjectError::Corrupt { id, detail } => {
                crate::store::Error::ObjectCorrupt { id, detail }
            }
            ObjectError::HashCollision { id } => crate::store::Error::HashCollision { id },
            ObjectError::Io(e) => crate::store::Error::Io(e),
            ObjectError::Limit { limit, found } => crate::store::Error::Limit {
                kind: "object size",
                limit,
                found,
            },
        }
    }
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The on-disk path of an object's canonical bytes.
pub fn object_path(meta: &Path, id: &Gid) -> PathBuf {
    let hex = crate::hex::encode(id.digest());
    meta.join("objects")
        .join(&hex[0..2])
        .join(format!("{hex}.gce"))
}

/// The on-disk path of an object's tombstone.
pub fn tombstone_path(meta: &Path, id: &Gid) -> PathBuf {
    let hex = crate::hex::encode(id.digest());
    meta.join("objects")
        .join(&hex[0..2])
        .join(format!("{hex}.tomb"))
}

/// Atomically writes `bytes` to `path`: temp file in the same directory,
/// fsync, rename, directory fsync.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let tmp = dir.join(format!(".tmp-{}-{seq}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Ok(dirf) = std::fs::File::open(dir) {
        let _ = dirf.sync_all();
    }
    Ok(())
}

/// Publishes an object's canonical bytes (deduplicating; fail closed on
/// collisions). Callers must validate the bytes before calling.
pub fn insert(meta: &Path, id: Gid, bytes: &[u8], limits: &Limits) -> Result<(), ObjectError> {
    let path = object_path(meta, &id);
    if let Ok(existing) = std::fs::read(&path) {
        if existing == bytes {
            return Ok(()); // deduplicated insert
        }
        return Err(ObjectError::HashCollision { id });
    }
    if bytes.len() as u64 > limits.max_object_bytes {
        return Err(ObjectError::Limit {
            limit: limits.max_object_bytes,
            found: bytes.len() as u64,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(&path, bytes)?;
    Ok(())
}

/// Reads an object's canonical bytes, verifying the identity hash.
pub fn read(meta: &Path, id: &Gid, limits: &Limits) -> Result<Vec<u8>, ObjectError> {
    let path = object_path(meta, id);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ObjectError::NotFound),
        Err(e) => return Err(e.into()),
    };
    if bytes.len() as u64 > limits.max_object_bytes {
        return Err(ObjectError::Limit {
            limit: limits.max_object_bytes,
            found: bytes.len() as u64,
        });
    }
    let actual = object_id_bytes(&bytes);
    if actual != *id.digest() {
        return Err(ObjectError::Corrupt {
            id: *id,
            detail: "bytes do not hash to identity".into(),
        });
    }
    Ok(bytes)
}

/// Whether the object file exists.
pub fn exists(meta: &Path, id: &Gid) -> Result<bool, ObjectError> {
    match std::fs::metadata(object_path(meta, id)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Lists every `.gce` object file on disk (shards included) — used by fsck
/// and the index rebuild. Callers decode and verify each file.
pub fn scan(meta: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut out = Vec::new();
    let objects_dir = meta.join("objects");
    for shard in std::fs::read_dir(&objects_dir)? {
        let shard = shard?;
        if !shard.file_type()?.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(shard.path())? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if name.ends_with(".gce") {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::temp_root;
    use crate::value::Object;

    #[test]
    fn insert_dedup_and_read() {
        let root = temp_root("objects");
        let meta = root.join(".gemel");
        std::fs::create_dir_all(meta.join("objects")).unwrap();
        let obj = Object::blob(b"hello world\n".to_vec());
        let bytes = crate::encode::encode_object(&obj, &Limits::default()).unwrap();
        let id = Gid::new(crate::family::Family::Blob, object_id_bytes(&bytes));

        insert(&meta, id, &bytes, &Limits::default()).unwrap();
        // Deduplicated second insert.
        insert(&meta, id, &bytes, &Limits::default()).unwrap();
        assert_eq!(read(&meta, &id, &Limits::default()).unwrap(), bytes);
        assert!(exists(&meta, &id).unwrap());
    }

    #[test]
    fn corruption_detected() {
        let root = temp_root("corrupt");
        let meta = root.join(".gemel");
        std::fs::create_dir_all(meta.join("objects")).unwrap();
        let bytes = b"GEML\x01\x01\x01\x00\x02hi".to_vec();
        let id = Gid::new(crate::family::Family::Blob, object_id_bytes(&bytes));
        insert(&meta, id, &bytes, &Limits::default()).unwrap();
        // Flip a byte on disk.
        let path = object_path(&meta, &id);
        let mut bad = std::fs::read(&path).unwrap();
        bad[0] ^= 0xff;
        std::fs::write(&path, bad).unwrap();
        match read(&meta, &id, &Limits::default()) {
            Err(ObjectError::Corrupt { .. }) => {}
            other => panic!("expected corrupt, got {other:?}"),
        }
    }

    #[test]
    fn missing_and_collision() {
        let root = temp_root("missing");
        let meta = root.join(".gemel");
        std::fs::create_dir_all(meta.join("objects")).unwrap();
        let bytes = b"GEML\x01\x01\x01\x00\x02hi".to_vec();
        let id = Gid::new(crate::family::Family::Blob, object_id_bytes(&bytes));
        assert!(matches!(
            read(&meta, &id, &Limits::default()),
            Err(ObjectError::NotFound)
        ));
        insert(&meta, id, &bytes, &Limits::default()).unwrap();
        let other = b"GEML\x01\x01\x01\x00\x03abc".to_vec();
        let other_id = Gid::new(crate::family::Family::Blob, object_id_bytes(&other));
        let _ = other_id;
        // Inserting different bytes under the same id must fail closed.
        let mut bad = bytes.clone();
        bad.push(0);
        match insert(&meta, id, &bad, &Limits::default()) {
            Err(ObjectError::HashCollision { .. }) => {}
            other => panic!("expected collision, got {other:?}"),
        }
    }
}
