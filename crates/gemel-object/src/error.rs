//! Canonical object errors (fail-closed catalog, THREAT_MODEL.md §4).

use gemel_core::family::Family;
use gemel_core::gid::ParseGidError;
use gemel_core::hex::HexError;
use gemel_core::varint::VarintError;
use std::fmt;

/// Every failure mode of the canonical layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectError {
    /// Envelope magic mismatch.
    BadMagic,
    /// Unsupported encoding version byte.
    UnknownEncodingVersion { found: u8 },
    /// Family code not in the family table.
    UnknownFamily { code: u8 },
    /// Schema version not supported for the family.
    UnknownSchemaVersion { family: Family, found: u8 },
    /// Envelope flags byte is not 0x00.
    ReservedFlags { found: u8 },
    /// Body length does not match the actual input length.
    LengthMismatch,
    /// Bytes remain after the declared body.
    TrailingBytes,
    /// Non-minimal LEB128 (redundant leading zero group).
    NonCanonicalInteger,
    /// Integer overflows the target width.
    IntegerOverflow,
    /// BOOL field with a byte other than 0x00/0x01.
    InvalidBoolean { byte: u8 },
    /// STRING field with invalid UTF-8.
    InvalidUtf8,
    /// GID reference with a length other than 33 bytes.
    InvalidGid { len: usize },
    /// Record fields not strictly ascending.
    UnsortedFields { tag: u64, prev: u64 },
    /// Record fields with a repeated tag.
    DuplicateField { tag: u64 },
    /// Tag 0x00 or 0xF0..=0xFF.
    ReservedTag { tag: u64 },
    /// Tag in the mandatory range unknown to the schema (fail closed).
    UnknownMandatoryField { family: Family, tag: u64 },
    /// Extension tag on a family that does not permit extensions.
    ExtensionNotPermitted { family: Family, tag: u64 },
    /// Declared value length does not match the encoded value.
    ValueLengthMismatch,
    /// A required field is absent.
    MissingRequiredField {
        family: Family,
        tag: u8,
        name: &'static str,
    },
    /// A configured limit was exceeded.
    LimitExceeded {
        kind: &'static str,
        limit: u64,
        found: u64,
    },
    /// Invalid canonical path (OBJECT_MODEL.md §1.6).
    InvalidPath { path: String },
    /// Enum field with a value outside the declared set.
    InvalidEnumValue { field: &'static str, found: String },
    /// GID with a family that does not match the schema expectation.
    FamilyMismatch { expected: Family, found: Family },
    /// Value does not match the schema-declared type.
    TypeMismatch { tag: u64, expected: &'static str },
    /// Object body kind does not match the family (blob vs. fields).
    BodyKindMismatch { family: Family },
    /// A schema field carried an opaque extension payload.
    UnexpectedRawValue { tag: u64 },
    /// An extension tag carried a typed value instead of an opaque payload.
    ExtensionMustBeRaw { tag: u64 },
    /// Tree entry mode is not in the declared set.
    InvalidTreeMode { mode: u64 },
    /// Tree entry target family does not match its mode.
    InvalidTreeTargetFamily { name: String, mode: u64 },
    /// Tree entries are not sorted by name bytes (or duplicate names).
    InvalidTreeOrder { name: String },
    /// Tree entry name violates the segment rules.
    InvalidTreeName { name: String },
    /// Operation carries a parameter tag undeclared for its op_type.
    UndeclaredOperationTag { op_type: String, tag: u64 },
    /// Claim predicate is empty.
    EmptyPredicate,
    /// Mapping loss.fabricated is not empty (invariant OBJ-12).
    MappingFabricatedNonEmpty,
    /// Object exceeds the configured reference count.
    RefCountExceeded { limit: usize, found: usize },
    /// A field has no name annotation in the JSON projection.
    UnknownFieldName { tag: u64 },
    /// Primitive-level error.
    Varint(VarintError),
    /// Hex-level error.
    Hex(HexError),
    /// Textual identity parse error.
    GidParse(String),
    /// Identity digest length is not 32 bytes.
    DigestLength { len: usize },
}

impl fmt::Display for ObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectError::BadMagic => write!(f, "bad envelope magic"),
            ObjectError::UnknownEncodingVersion { found } => {
                write!(f, "unknown encoding version {found}")
            }
            ObjectError::UnknownFamily { code } => write!(f, "unknown family code 0x{code:02x}"),
            ObjectError::UnknownSchemaVersion { family, found } => {
                write!(f, "unsupported schema version {found} for family {family}")
            }
            ObjectError::ReservedFlags { found } => {
                write!(f, "reserved envelope flags 0x{found:02x}")
            }
            ObjectError::LengthMismatch => write!(f, "declared length does not match input"),
            ObjectError::TrailingBytes => write!(f, "trailing bytes after object body"),
            ObjectError::NonCanonicalInteger => write!(f, "non-canonical integer encoding"),
            ObjectError::IntegerOverflow => write!(f, "integer overflow"),
            ObjectError::InvalidBoolean { byte } => write!(f, "invalid boolean byte 0x{byte:02x}"),
            ObjectError::InvalidUtf8 => write!(f, "invalid UTF-8 in string field"),
            ObjectError::InvalidGid { len } => write!(f, "invalid gid length {len} (expected 33)"),
            ObjectError::UnsortedFields { tag, prev } => {
                write!(f, "unsorted fields: tag {tag} after {prev}")
            }
            ObjectError::DuplicateField { tag } => write!(f, "duplicate field tag {tag}"),
            ObjectError::ReservedTag { tag } => write!(f, "reserved field tag {tag}"),
            ObjectError::UnknownMandatoryField { family, tag } => {
                write!(f, "unknown mandatory field tag {tag} for family {family}")
            }
            ObjectError::ExtensionNotPermitted { family, tag } => {
                write!(f, "extension tag {tag} not permitted for family {family}")
            }
            ObjectError::ValueLengthMismatch => write!(f, "value length mismatch"),
            ObjectError::MissingRequiredField { family, tag, name } => {
                write!(
                    f,
                    "missing required field {name} (tag {tag}) for family {family}"
                )
            }
            ObjectError::LimitExceeded { kind, limit, found } => {
                write!(f, "limit exceeded: {kind} (limit {limit}, found {found})")
            }
            ObjectError::InvalidPath { path } => write!(f, "invalid canonical path: {path:?}"),
            ObjectError::InvalidEnumValue { field, found } => {
                write!(f, "invalid enum value {found:?} for {field}")
            }
            ObjectError::FamilyMismatch { expected, found } => {
                write!(f, "gid family mismatch: expected {expected}, found {found}")
            }
            ObjectError::TypeMismatch { tag, expected } => {
                write!(f, "value for tag {tag} is not {expected}")
            }
            ObjectError::BodyKindMismatch { family } => {
                write!(f, "object body kind does not match family {family}")
            }
            ObjectError::UnexpectedRawValue { tag } => {
                write!(f, "opaque raw value for schema field tag {tag}")
            }
            ObjectError::ExtensionMustBeRaw { tag } => {
                write!(f, "extension tag {tag} must carry an opaque raw value")
            }
            ObjectError::InvalidTreeMode { mode } => write!(f, "invalid tree entry mode {mode:#o}"),
            ObjectError::InvalidTreeTargetFamily { name, mode } => {
                write!(
                    f,
                    "tree entry {name:?} target family does not match mode {mode:#o}"
                )
            }
            ObjectError::InvalidTreeOrder { name } => {
                write!(f, "tree entries out of order at {name:?}")
            }
            ObjectError::InvalidTreeName { name } => write!(f, "invalid tree entry name {name:?}"),
            ObjectError::UndeclaredOperationTag { op_type, tag } => {
                write!(f, "operation tag {tag} undeclared for op_type {op_type:?}")
            }
            ObjectError::EmptyPredicate => write!(f, "claim predicate must be non-empty"),
            ObjectError::MappingFabricatedNonEmpty => {
                write!(f, "mapping loss.fabricated must be empty")
            }
            ObjectError::RefCountExceeded { limit, found } => {
                write!(
                    f,
                    "object reference count exceeds limit (limit {limit}, found {found})"
                )
            }
            ObjectError::UnknownFieldName { tag } => write!(f, "no field name known for tag {tag}"),
            ObjectError::Varint(e) => write!(f, "varint error: {e}"),
            ObjectError::Hex(e) => write!(f, "hex error: {e:?}"),
            ObjectError::GidParse(e) => write!(f, "gid parse error: {e}"),
            ObjectError::DigestLength { len } => write!(f, "digest length {len} (expected 32)"),
        }
    }
}

impl std::error::Error for ObjectError {}

impl From<VarintError> for ObjectError {
    fn from(e: VarintError) -> Self {
        match e {
            // Surface the documented canonical-integer codes.
            VarintError::NonCanonical => ObjectError::NonCanonicalInteger,
            VarintError::Overflow => ObjectError::IntegerOverflow,
            other => ObjectError::Varint(other),
        }
    }
}

impl From<HexError> for ObjectError {
    fn from(e: HexError) -> Self {
        ObjectError::Hex(e)
    }
}

impl From<ParseGidError> for ObjectError {
    fn from(e: ParseGidError) -> Self {
        ObjectError::GidParse(e.to_string())
    }
}
