//! Gemel core primitives.
//!
//! This crate provides the primitive building blocks of the Gemel Canonical
//! Encoding (GCE): canonical variable-length integers, the object family table,
//! object identities (Gid), hex encoding, and resource limits.
//!
//! Everything here is deterministic and platform-independent by construction.

pub mod family;
pub mod gid;
pub mod hex;
pub mod limits;
pub mod varint;
