//! Gemel — evidence-native version control for agentic software development.
//!
//! This crate is the reference implementation of the Gemel Canonical Encoding
//! (GCE) defined in `docs/OBJECT_MODEL.md`. It provides:
//!
//! - the canonical primitives: varint, family table, Gid, hex, limits
//!   ([`varint`], [`family`], [`gid`], [`hex`], [`limits`]);
//! - the canonical value/object model ([`value`]);
//! - the normative per-family schema tables ([`spec`]);
//! - the fail-closed encoder ([`encode`]) and decoder ([`decode`]);
//! - BLAKE3-256 object identity ([`hash`]);
//! - family-specific validation ([`validate`]);
//! - the deterministic JSON projection ([`json`]);
//! - the executable golden fixtures ([`golden`]).

pub mod consts;
pub mod decode;
pub mod encode;
pub mod error;
pub mod family;
pub mod gid;
pub mod golden;
pub mod hash;
pub mod hex;
pub mod json;
pub mod limits;
pub mod spec;
pub mod validate;
pub mod value;
pub mod varint;

pub use error::ObjectError;
pub use value::{Body, Field, Object, Value};

#[cfg(test)]
mod tests;
