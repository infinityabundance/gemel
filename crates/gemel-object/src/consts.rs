//! Canonical encoding constants (OBJECT_MODEL.md §1.4).

/// Envelope magic: "GEML".
pub const MAGIC: [u8; 4] = *b"GEML";

/// Encoding version of the GCE primitive grammar (Phase 0: 1).
pub const ENC_VERSION: u8 = 1;

/// Reserved flags byte; must be 0x00 in encver 1.
pub const FLAGS_ZERO: u8 = 0x00;

/// Mandatory-schema tag range: 0x01..=0x7F (unknown => fail closed).
pub const TAG_MAX_SCHEMA: u8 = 0x7F;
/// Extension tag range: 0x80..=0xEF (retained verbatim where permitted).
pub const TAG_MIN_EXTENSION: u8 = 0x80;
pub const TAG_MAX_EXTENSION: u8 = 0xEF;
// Reserved tags: 0x00 and 0xF0..=0xFF.

/// Maximum number of fields in a record (structural, implied by the tag range).
pub const MAX_FIELDS_PER_RECORD: usize = TAG_MAX_EXTENSION as usize;
