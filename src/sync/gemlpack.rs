//! The `gemlpack` transfer pack format (Phase 6; STORAGE.md §10).
//!
//! A deterministic, content-verified, resumable byte format for exchanging
//! Gemel objects between repositories. Distinct from the Git-carried
//! exchange pack (`GXPK`): this format carries **full canonical envelopes**
//! (including source blobs) for native synchronization; `GXPK` is a
//! Git-constrained projection.
//!
//! Layout (little-endian):
//!
//! ```text
//! MAGIC          "GMLP"           4 bytes
//! FORMAT_VERSION 0x01             u8
//! OBJECT_COUNT                    u64
//! TOTAL_BYTES                     u64   (envelope bytes only)
//! RECORD 1..N
//!     object_id                   33 bytes (family + BLAKE3 digest)
//!     envelope_length             u64
//!     canonical_envelope_bytes    [envelope_length]
//! ```
//!
//! Invariants:
//! - Records appear in ascending canonical `Gid` byte order.
//! - The advertised id must equal `BLAKE3(envelope)` and the envelope's
//!   decoded family must equal the id's family.
//! - Duplicate ids are rejected.
//! - Every bound is enforced during decode (before allocation).
//! - A record that fails verification makes the whole pack invalid: the
//!   receiver never activates refs over a partially-verified pack.

use crate::gid::Gid;
use crate::store::Error;

/// The pack magic.
pub const MAGIC: &[u8; 4] = b"GMLP";
/// The format version.
pub const FORMAT_VERSION: u8 = 1;

/// One transferred object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRecord {
    pub id: Gid,
    pub envelope: Vec<u8>,
}

/// The identity of the pack bytes themselves (used for resumed transfer and
/// caching; not a Gemel object identity).
pub fn pack_id(bytes: &[u8]) -> [u8; 32] {
    crate::hash::blake3_256(bytes)
}

/// Encodes records deterministically: ascending id order, no duplicates.
pub fn encode_pack(records: &[PackRecord]) -> Result<Vec<u8>, Error> {
    let mut sorted: Vec<&PackRecord> = records.iter().collect();
    sorted.sort_by_key(|r| r.id.to_bytes());
    let mut out = Vec::with_capacity(16 + sorted.len() * 48);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&(sorted.len() as u64).to_le_bytes());
    let total: u64 = sorted.iter().map(|r| r.envelope.len() as u64).sum();
    out.extend_from_slice(&total.to_le_bytes());
    let mut prev: Option<[u8; 33]> = None;
    for r in &sorted {
        if prev == Some(r.id.to_bytes()) {
            return Err(Error::Invalid(format!(
                "gemlpack: duplicate object id {}",
                r.id
            )));
        }
        prev = Some(r.id.to_bytes());
        out.extend_from_slice(&r.id.to_bytes());
        out.extend_from_slice(&(r.envelope.len() as u64).to_le_bytes());
        out.extend_from_slice(&r.envelope);
    }
    Ok(out)
}

/// The limits applied during decode.
#[derive(Debug, Clone, Copy)]
pub struct PackLimits {
    pub max_pack_bytes: u64,
    pub max_objects: usize,
    pub max_object_bytes: u64,
}

impl Default for PackLimits {
    fn default() -> Self {
        PackLimits {
            max_pack_bytes: 4 << 30, // 4 GiB per pack
            max_objects: 10_000_000,
            max_object_bytes: 1 << 30, // 1 GiB per object
        }
    }
}

/// Decodes and verifies a pack. Every record's identity is checked against
/// its envelope bytes; a single failure rejects the whole pack.
pub fn decode_pack(bytes: &[u8], limits: &PackLimits) -> Result<Vec<PackRecord>, Error> {
    if bytes.len() < 16 {
        return Err(Error::Invalid("gemlpack: truncated header".into()));
    }
    if &bytes[0..4] != MAGIC {
        return Err(Error::Invalid("gemlpack: bad magic".into()));
    }
    if bytes[4] != FORMAT_VERSION {
        return Err(Error::Invalid(format!(
            "gemlpack: unsupported format version {}",
            bytes[4]
        )));
    }
    if (bytes.len() as u64) > limits.max_pack_bytes {
        return Err(Error::Limit {
            kind: "gemlpack pack bytes",
            limit: limits.max_pack_bytes,
            found: bytes.len() as u64,
        });
    }
    let count = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
    if count as usize > limits.max_objects {
        return Err(Error::Limit {
            kind: "gemlpack object count",
            limit: limits.max_objects as u64,
            found: count,
        });
    }
    let mut pos = 21usize;
    let mut out = Vec::with_capacity(count as usize);
    let mut prev: Option<[u8; 33]> = None;
    for _ in 0..count {
        if bytes.len().saturating_sub(pos) < 33 + 8 {
            return Err(Error::Invalid("gemlpack: truncated record header".into()));
        }
        let id_bytes: [u8; 33] = bytes[pos..pos + 33].try_into().unwrap();
        pos += 33;
        let len = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        pos += 8;
        if len > limits.max_object_bytes {
            return Err(Error::Limit {
                kind: "gemlpack object bytes",
                limit: limits.max_object_bytes,
                found: len,
            });
        }
        let end = pos
            .checked_add(len as usize)
            .ok_or_else(|| Error::Invalid("gemlpack: object length overflow".into()))?;
        if end > bytes.len() {
            return Err(Error::Invalid("gemlpack: truncated object envelope".into()));
        }
        let id = Gid::from_bytes(&id_bytes)
            .ok_or_else(|| Error::Invalid("gemlpack: invalid object id".into()))?;
        if prev == Some(id_bytes) {
            return Err(Error::Invalid(format!(
                "gemlpack: duplicate object id {id}"
            )));
        }
        prev = Some(id_bytes);
        let envelope = &bytes[pos..end];
        pos = end;
        // Verify the advertised identity against the exact envelope bytes.
        if crate::hash::blake3_256(envelope) != *id.digest() {
            return Err(Error::Invalid(format!(
                "gemlpack: object id/body mismatch for {id}"
            )));
        }
        // The decoded family must match the id's family.
        match crate::decode::decode_object(envelope, &crate::limits::Limits::default()) {
            Ok(obj) if obj.family == id.family() => {}
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "gemlpack: family mismatch for {id}"
                )))
            }
            Err(e) => {
                return Err(Error::Invalid(format!(
                    "gemlpack: invalid canonical envelope for {id}: {e}"
                )))
            }
        }
        out.push(PackRecord {
            id,
            envelope: envelope.to_vec(),
        });
    }
    if pos != bytes.len() {
        return Err(Error::Invalid(format!(
            "gemlpack: trailing bytes after {} objects",
            count
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::Family;

    fn blob(byte: u8) -> PackRecord {
        let obj = crate::value::Object::blob(vec![byte]);
        let envelope =
            crate::encode::encode_object(&obj, &crate::limits::Limits::default()).unwrap();
        let id = Gid::new(Family::Blob, crate::hash::blake3_256(&envelope));
        PackRecord { id, envelope }
    }

    #[test]
    fn roundtrip_and_identity_verification() {
        let recs = vec![blob(1), blob(2), blob(3)];
        let bytes = encode_pack(&recs).unwrap();
        assert_eq!(&bytes[0..4], MAGIC);
        let decoded = decode_pack(&bytes, &PackLimits::default()).unwrap();
        assert_eq!(decoded.len(), 3);
        // Compare as sorted multisets: the pack re-orders ascending by id.
        let mut orig: Vec<PackRecord> = recs.clone();
        orig.sort_by_key(|r| r.id.to_bytes());
        for (a, b) in orig.iter().zip(decoded.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.envelope, b.envelope);
        }
    }

    #[test]
    fn deterministic_order_and_duplicate_rejection() {
        let recs = vec![blob(3), blob(1), blob(2)];
        let bytes = encode_pack(&recs).unwrap();
        // Records are re-ordered ascending by id.
        let decoded = decode_pack(&bytes, &PackLimits::default()).unwrap();
        let ids: Vec<[u8; 33]> = decoded.iter().map(|r| r.id.to_bytes()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        // Duplicate ids are rejected.
        let dup = vec![blob(1), blob(1)];
        assert!(encode_pack(&dup).is_err());
    }

    #[test]
    fn corruption_is_rejected_whole_pack() {
        let bytes = encode_pack(&[blob(1), blob(2)]).unwrap();
        // Flip a byte inside the second envelope.
        let mut corrupt = bytes.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0x01;
        assert!(decode_pack(&corrupt, &PackLimits::default()).is_err());
        // Truncation is rejected.
        assert!(decode_pack(&bytes[..bytes.len() - 5], &PackLimits::default()).is_err());
        // Bad magic is rejected.
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert!(decode_pack(&bad, &PackLimits::default()).is_err());
    }

    #[test]
    fn bounds_are_enforced() {
        let bytes = encode_pack(&[blob(1)]).unwrap();
        let tight = PackLimits {
            max_pack_bytes: 10,
            ..Default::default()
        };
        assert!(decode_pack(&bytes, &tight).is_err());
    }
}
