//! Object identity hashing (OBJECT_MODEL.md §2).
//!
//! `ObjectId = BLAKE3-256(canonical envelope bytes)`.

use crate::error::ObjectError;
use crate::value::Object;
use gemel_core::gid::Gid;

/// The BLAKE3-256 identity digest of canonical envelope bytes.
pub fn object_id_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Derives the identity of an encoded object (family from the envelope).
pub fn gid_from_envelope(bytes: &[u8]) -> Option<Gid> {
    let family = gemel_core::family::Family::from_code(*bytes.get(5)?)?;
    Some(Gid::new(family, object_id_bytes(bytes)))
}

/// Derives the identity of an object (encodes it first).
pub fn object_id(obj: &Object, limits: &gemel_core::limits::Limits) -> Result<Gid, ObjectError> {
    let bytes = crate::encode::encode_object(obj, limits)?;
    Ok(Gid::new(obj.family, object_id_bytes(&bytes)))
}
