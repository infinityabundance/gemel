//! Mutable refs and the journaled ref transaction (STORAGE.md §4).
//!
//! Refs are single-line text files (`<textual gid>\n`) under `refs/`, updated
//! atomically (temp + rename). Multi-ref transactions are journaled: entries
//! are appended and fsynced before any ref file is touched, then a commit
//! marker is appended, then the ref files are applied. Recovery rolls back
//! uncommitted transactions and truncates the journal (STORAGE.md §4.2).

use crate::gid::Gid;
use crate::store::lock;
use crate::store::objects::write_atomic;
use crate::store::Error;
use std::path::Path;

/// One ref mutation in a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefOp {
    pub name: String,
    pub new: Option<Gid>,
}

impl RefOp {
    /// Sets a ref to `gid`.
    pub fn set(name: &str, gid: Gid) -> RefOp {
        RefOp {
            name: name.to_string(),
            new: Some(gid),
        }
    }

    /// Deletes a ref.
    pub fn delete(name: &str) -> RefOp {
        RefOp {
            name: name.to_string(),
            new: None,
        }
    }
}

/// A journaled, atomic ref transaction.
#[derive(Debug, Clone, Default)]
pub struct RefTransaction {
    pub ops: Vec<RefOp>,
}

/// Validates a ref name (INVARIANTS REF-01): `refs/...`, no traversal, no
/// empty/`.`/`..` segments, printable UTF-8 without control characters.
pub fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || !name.starts_with("refs/") {
        return Err(Error::RefNameInvalid(name.to_string()));
    }
    if name.contains('\0') || name.contains('\\') {
        return Err(Error::RefNameInvalid(name.to_string()));
    }
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(Error::RefNameInvalid(name.to_string()));
        }
        if seg.chars().any(|c| c.is_control()) {
            return Err(Error::RefNameInvalid(name.to_string()));
        }
    }
    Ok(())
}

/// The on-disk path of a ref file.
pub fn ref_path(meta: &Path, name: &str) -> std::io::Result<std::path::PathBuf> {
    validate_name(name)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
    Ok(meta.join("refs").join(name.trim_start_matches("refs/")))
}

/// Reads a ref; absent refs yield `None`.
pub fn read(meta: &Path, name: &str) -> Result<Option<Gid>, Error> {
    let path = ref_path(meta, name)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let trimmed = text.trim();
            trimmed
                .parse::<Gid>()
                .map(Some)
                .map_err(|e| Error::RefCorrupt {
                    name: name.to_string(),
                    detail: e.to_string(),
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Lists all refs as (name, gid), sorted by name.
pub fn all(meta: &Path) -> Result<Vec<(String, Gid)>, Error> {
    let mut out = Vec::new();
    walk_refs_dir(meta, &meta.join("refs"), "", &mut out)?;
    out.sort();
    Ok(out)
}

fn walk_refs_dir(
    meta: &Path,
    dir: &Path,
    rel: &str,
    out: &mut Vec<(String, Gid)>,
) -> Result<(), Error> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        let child_rel = if rel.is_empty() {
            fname.clone()
        } else {
            format!("{rel}/{fname}")
        };
        let child_path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk_refs_dir(meta, &child_path, &child_rel, out)?;
        } else {
            let name = format!("refs/{child_rel}");
            if let Some(gid) = read(meta, &name)? {
                out.push((name, gid));
            }
        }
    }
    Ok(())
}

/// The journal path.
pub fn journal_path(meta: &Path) -> std::path::PathBuf {
    meta.join("journal").join("journal.log")
}

/// Applies a ref transaction with journaling (acquires the writer lock).
pub fn apply(meta: &Path, txn: &RefTransaction) -> Result<(), Error> {
    let lock_path = meta.join("lock");
    lock::with_write_lock(&lock_path, || apply_unlocked(meta, txn))
}

/// Applies a ref transaction; the caller holds the writer lock.
pub fn apply_unlocked(meta: &Path, txn: &RefTransaction) -> Result<(), Error> {
    if txn.ops.is_empty() {
        return Ok(());
    }
    // Validate everything up front.
    for op in &txn.ops {
        validate_name(&op.name)?;
        if let Some(gid) = op.new {
            if gid.family().code() == 0 {
                return Err(Error::Invalid("ref target has invalid family".into()));
            }
        }
    }

    let journal = journal_path(meta);
    // Append the journal (entries + commit marker) and fsync.
    let mut body = String::new();
    for op in &txn.ops {
        let prev = read(meta, &op.name)?;
        let entry = serde_json::json!({
            "op": "set",
            "ref": op.name,
            "new": op.new.map(|g| g.to_string()),
            "prev": prev.map(|g| g.to_string()),
        });
        body.push_str(&serde_json::to_string(&entry).map_err(|e| Error::Invalid(e.to_string()))?);
        body.push('\n');
    }
    body.push_str("{\"op\":\"commit\"}\n");
    write_atomic(&journal, body.as_bytes())?;

    // Apply the ref file updates (each atomic).
    for op in &txn.ops {
        let path = ref_path(meta, &op.name)?;
        match op.new {
            Some(gid) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_atomic(&path, &format!("{gid}\n").into_bytes())?;
            }
            None => match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            },
        }
    }

    // Truncate the journal after a clean commit.
    write_atomic(&journal, b"")?;
    Ok(())
}

/// Recovers from interrupted transactions: rolls back uncommitted
/// transactions and truncates the journal. Returns true when recovery
/// performed work. Acquires the writer lock.
pub fn recover(meta: &Path) -> Result<bool, Error> {
    let lock_path = meta.join("lock");
    lock::with_write_lock(&lock_path, || recover_unlocked(meta))
}

/// Recovery with the writer lock held by the caller.
pub fn recover_unlocked(meta: &Path) -> Result<bool, Error> {
    let journal = journal_path(meta);
    let content = match std::fs::read_to_string(&journal) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if content.trim().is_empty() {
        return Ok(false);
    }
    let mut committed = false;
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| Error::RefCorrupt {
            name: "journal".into(),
            detail: e.to_string(),
        })?;
        if v["op"] == "commit" {
            committed = true;
            break;
        }
        let name = v["ref"].as_str().ok_or_else(|| Error::RefCorrupt {
            name: "journal".into(),
            detail: "missing ref".into(),
        })?;
        let prev = v["prev"]
            .as_str()
            .map(|s| s.parse::<Gid>())
            .transpose()
            .map_err(|e| Error::RefCorrupt {
                name: name.to_string(),
                detail: e.to_string(),
            })?;
        entries.push((name.to_string(), prev));
    }
    let did_work = !committed && !entries.is_empty();
    if did_work {
        // Roll back each ref to its previous value.
        for (name, prev) in entries {
            let path = ref_path(meta, &name)?;
            match prev {
                Some(gid) => {
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    write_atomic(&path, &format!("{gid}\n").into_bytes())?;
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    write_atomic(&journal, b"")?;
    Ok(did_work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{fresh_repo, temp_root};
    use crate::store::REF_HEAD;

    #[test]
    fn ref_roundtrip() {
        let (repo, root) = fresh_repo("refs");
        let obj = crate::value::Object::blob(b"x".to_vec());
        let gid = repo.insert_object(&obj).unwrap();
        repo.write_refs(&RefTransaction {
            ops: vec![RefOp::set(REF_HEAD, gid)],
        })
        .unwrap();
        assert_eq!(repo.read_ref(REF_HEAD).unwrap(), Some(gid));
        repo.write_refs(&RefTransaction {
            ops: vec![RefOp::delete(REF_HEAD)],
        })
        .unwrap();
        assert_eq!(repo.read_ref(REF_HEAD).unwrap(), None);
        let _ = root;
    }

    #[test]
    fn invalid_names_rejected() {
        let (repo, _) = fresh_repo("refnames");
        for bad in [
            "../etc",
            "refs/..",
            "refs/a/../b",
            "refs/",
            "refs//x",
            "names/x",
        ] {
            assert!(
                repo.write_refs(&RefTransaction {
                    ops: vec![RefOp::set(
                        bad,
                        Gid::new(crate::family::Family::Blob, [0u8; 32])
                    )]
                })
                .is_err(),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn recovery_rolls_back_uncommitted() {
        let (repo, root) = fresh_repo("recover");
        let obj = crate::value::Object::blob(b"y".to_vec());
        let gid = repo.insert_object(&obj).unwrap();
        // Simulate an interrupted transaction: journal written, ref not applied.
        let journal = journal_path(repo.meta_dir());
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let fake = format!(
            "{{\"op\":\"set\",\"ref\":\"{REF_HEAD}\",\"new\":\"{}\",\"prev\":null}}\n",
            gid
        );
        std::fs::write(&journal, fake).unwrap();
        assert_eq!(repo.read_ref(REF_HEAD).unwrap(), None);
        assert!(recover(repo.meta_dir()).unwrap());
        // Recovery rolled back (prev was null → ref stays absent) and truncated.
        assert_eq!(repo.read_ref(REF_HEAD).unwrap(), None);
        assert!(std::fs::read_to_string(&journal).unwrap().is_empty());
        let _ = root;
    }

    #[test]
    fn resolve_names() {
        let (repo, _) = fresh_repo("resolve");
        let obj = crate::value::Object::blob(b"z".to_vec());
        let gid = repo.insert_object(&obj).unwrap();
        repo.write_refs(&RefTransaction {
            ops: vec![RefOp::set("refs/names/hello", gid)],
        })
        .unwrap();
        assert_eq!(repo.resolve("hello").unwrap(), gid);
        assert_eq!(repo.resolve(&gid.to_string()).unwrap(), gid);
        assert!(repo.resolve("nope").is_err());
        let _ = temp_root("unused");
    }
}
