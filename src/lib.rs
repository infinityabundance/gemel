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
//! - the executable golden fixtures ([`golden`]);
//! - the repository store, content layer, workflow, query surface, ignore
//!   matching, and default builders ([`store`], [`content`], [`workflow`],
//!   [`query`], [`ignore`], [`defaults`]).
//!
//! The store error type intentionally carries rich payloads (paths, gids,
//! strings, tombstones) for precise diagnostics; boxing every variant would
//! obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

pub mod consts;
pub mod content;
pub mod decode;
pub mod defaults;
pub mod encode;
pub mod error;
pub mod family;
pub mod gid;
pub mod golden;
pub mod hash;
pub mod hex;
pub mod ignore;
pub mod json;
pub mod limits;
pub mod query;
pub mod spec;
pub mod store;
pub mod validate;
pub mod value;
pub mod varint;
pub mod workflow;

pub use error::ObjectError;
pub use value::{Body, Field, Object, Value};

#[cfg(test)]
mod tests;
