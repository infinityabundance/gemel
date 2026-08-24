//! The canonical value and object model (OBJECT_MODEL.md §1, §6).

use gemel_core::family::Family;
use gemel_core::gid::Gid;

/// A canonical value.
///
/// Values are typed by the schema in which they appear; `Value` preserves the
/// exact canonical content. [`Value::Raw`] carries opaque canonical bytes for
/// extension fields (tags 0x80..=0xEF) so older readers can retain them
/// losslessly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Unsigned integer (canonical LEB128).
    U(u64),
    /// Signed integer (zigzag).
    I(i64),
    /// Boolean (single byte 0x00/0x01).
    B(bool),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// UTF-8 string, byte-identical, never normalized.
    Str(String),
    /// Object reference (33 bytes).
    Gid(Gid),
    /// A canonical field sequence.
    Record(Vec<Field>),
    /// A canonical array.
    Array(Vec<Value>),
    /// Opaque canonical value bytes for an extension field.
    Raw(Vec<u8>),
}

/// A single field: tag plus value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub tag: u8,
    pub value: Value,
}

impl Field {
    pub const fn new(tag: u8, value: Value) -> Field {
        Field { tag, value }
    }
}

/// The object body: raw bytes for `blob`, a field sequence otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Raw bytes (blob family only).
    Blob(Vec<u8>),
    /// A canonical field sequence.
    Fields(Vec<Field>),
}

/// A canonical object: envelope metadata plus body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub family: Family,
    pub schemever: u8,
    pub body: Body,
}

impl Object {
    /// A blob object with the given raw content.
    pub fn blob(bytes: Vec<u8>) -> Object {
        Object {
            family: Family::Blob,
            schemever: 1,
            body: Body::Blob(bytes),
        }
    }

    /// An object with a field-sequence body.
    pub fn fields(family: Family, body: Vec<Field>) -> Object {
        Object {
            family,
            schemever: 1,
            body: Body::Fields(body),
        }
    }

    /// The blob content of a blob object.
    pub fn blob_bytes(&self) -> Option<&[u8]> {
        match &self.body {
            Body::Blob(b) => Some(b),
            Body::Fields(_) => None,
        }
    }

    /// The field sequence of a non-blob object.
    pub fn field_sequence(&self) -> Option<&[Field]> {
        match &self.body {
            Body::Fields(f) => Some(f),
            Body::Blob(_) => None,
        }
    }
}
