//! Tombstones (STORAGE.md §7.6).
//!
//! When retention policy prunes a Tier 1–3 blob that remains referenced by
//! canonical objects, a tombstone is created *before* the blob is unlinked.
//! Reading a tombstoned object yields `Pruned` with the tombstone — never a
//! fabricated object. `fsck` distinguishes `missing` (corruption) from
//! `pruned` (policy).

use crate::family::Family;
use crate::gid::Gid;
use crate::store::objects::tombstone_path;
use std::path::Path;

/// The tombstone record (schema `gemel.tombstone.v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    pub id: Gid,
    pub family: Family,
    pub size: u64,
    pub pruned_at: i64,
    pub policy_tier: u8,
    pub policy_rule: String,
    pub archive_remote: Option<String>,
}

/// Writes a tombstone (atomic; the blob must still be present or already
/// archived — ordering is the GC's responsibility, STORAGE.md §7.4).
pub fn write(meta: &Path, t: &Tombstone) -> Result<(), crate::store::Error> {
    let value = serde_json::json!({
        "schema": "gemel.tombstone.v1",
        "id": t.id.to_string(),
        "family": t.family.short(),
        "size": t.size,
        "pruned_at": t.pruned_at,
        "policy": { "tier": t.policy_tier, "rule": t.policy_rule },
        "archive": t.archive_remote.as_ref().map(|r| {
            serde_json::json!({ "remote": r })
        }),
    });
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|e| crate::store::Error::Invalid(e.to_string()))?;
    bytes.push(b'\n');
    let path = tombstone_path(meta, &t.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::store::objects::write_atomic(&path, &bytes)?;
    Ok(())
}

/// Reads the tombstone for `id`, if any.
pub fn read(meta: &Path, id: &Gid) -> Result<Option<Tombstone>, crate::store::Error> {
    let path = tombstone_path(meta, id);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| crate::store::Error::Invalid(format!("tombstone {}: {e}", id)))?;
    if v["schema"] != "gemel.tombstone.v1" {
        return Err(crate::store::Error::Invalid(format!(
            "tombstone {}: unknown schema",
            id
        )));
    }
    let family = Family::parse_short(v["family"].as_str().unwrap_or(""))
        .ok_or_else(|| crate::store::Error::Invalid(format!("tombstone {}: bad family", id)))?;
    Ok(Some(Tombstone {
        id: *id,
        family,
        size: v["size"].as_u64().unwrap_or(0),
        pruned_at: v["pruned_at"].as_i64().unwrap_or(0),
        policy_tier: v["policy"]["tier"].as_u64().unwrap_or(0) as u8,
        policy_rule: v["policy"]["rule"].as_str().unwrap_or("").to_string(),
        archive_remote: v["archive"]["remote"].as_str().map(|s| s.to_string()),
    }))
}

/// Whether a tombstone exists for `id`.
pub fn exists(meta: &Path, id: &Gid) -> Result<bool, crate::store::Error> {
    Ok(std::fs::metadata(tombstone_path(meta, id)).is_ok())
}

/// Removes a tombstone (used when restoring a pruned object).
pub fn remove(meta: &Path, id: &Gid) -> Result<(), crate::store::Error> {
    match std::fs::remove_file(tombstone_path(meta, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::temp_root;

    #[test]
    fn tombstone_roundtrip() {
        let root = temp_root("tomb");
        let meta = root.join(".gemel");
        let id = Gid::new(Family::Blob, [7u8; 32]);
        let t = Tombstone {
            id,
            family: Family::Blob,
            size: 42,
            pruned_at: 1700000000000,
            policy_tier: 1,
            policy_rule: "prune_after_days".into(),
            archive_remote: Some("s3://bucket/key".into()),
        };
        write(&meta, &t).unwrap();
        let back = read(&meta, &id).unwrap().expect("tombstone present");
        assert_eq!(back, t);
        assert!(exists(&meta, &id).unwrap());
        remove(&meta, &id).unwrap();
        assert!(read(&meta, &id).unwrap().is_none());
    }
}
