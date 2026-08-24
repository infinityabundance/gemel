//! The fail-closed canonical decoder (OBJECT_MODEL.md §1, THREAT_MODEL.md §4).
//!
//! All parsing is cursor-based over an input buffer, bounded by explicit
//! limits, and rejects any non-canonical or schema-violating byte sequence.

use crate::consts::{ENC_VERSION, FLAGS_ZERO, MAGIC, TAG_MAX_SCHEMA};
use crate::error::ObjectError;
use crate::family::Family;
use crate::gid::Gid;
use crate::limits::Limits;
use crate::spec::{schema_for, FamilySchema, Type};
use crate::validate::{is_valid_canonical_path, validate_family};
use crate::value::{Body, Field, Object, Value};
use crate::varint::{decode_i64, decode_u64};
use std::str;

/// Decodes a canonical envelope into an object.
///
/// Fails closed on any violation of the grammar, schema, or limits.
pub fn decode_object(bytes: &[u8], limits: &Limits) -> Result<Object, ObjectError> {
    if bytes.len() < 9 {
        return Err(ObjectError::LengthMismatch);
    }
    if bytes[0..4] != MAGIC {
        return Err(ObjectError::BadMagic);
    }
    if bytes[4] != ENC_VERSION {
        return Err(ObjectError::UnknownEncodingVersion { found: bytes[4] });
    }
    let family =
        Family::from_code(bytes[5]).ok_or(ObjectError::UnknownFamily { code: bytes[5] })?;
    let schemever = bytes[6];
    if !family.supports_schemever(schemever) {
        return Err(ObjectError::UnknownSchemaVersion {
            family,
            found: schemever,
        });
    }
    if bytes[7] != FLAGS_ZERO {
        return Err(ObjectError::ReservedFlags { found: bytes[7] });
    }

    let mut pos = 8usize;
    let bodylen = decode_u64(bytes, &mut pos)?;
    if bodylen > limits.max_object_bytes {
        return Err(ObjectError::LimitExceeded {
            kind: "object size",
            limit: limits.max_object_bytes,
            found: bodylen,
        });
    }
    let body_start = pos;
    let expected_end = body_start
        .checked_add(bodylen as usize)
        .ok_or(ObjectError::LengthMismatch)?;
    if expected_end > bytes.len() {
        return Err(ObjectError::LengthMismatch);
    }
    if expected_end != bytes.len() {
        return Err(ObjectError::TrailingBytes);
    }
    let body = &bytes[body_start..expected_end];

    let schema = schema_for(family);
    let obj = if family == Family::Blob {
        Object {
            family,
            schemever,
            body: Body::Blob(body.to_vec()),
        }
    } else {
        let mut p = 0usize;
        let fields = decode_record_fields(schema, body, &mut p, body.len(), 1, limits)?;
        if p != body.len() {
            return Err(ObjectError::ValueLengthMismatch);
        }
        Object {
            family,
            schemever,
            body: Body::Fields(fields),
        }
    };
    validate_family(&obj, limits)?;
    Ok(obj)
}

/// Decodes a record body (field sequence) from `buf[*pos..end]`.
pub(crate) fn decode_record_fields(
    schema: &FamilySchema,
    buf: &[u8],
    pos: &mut usize,
    end: usize,
    depth: usize,
    limits: &Limits,
) -> Result<Vec<Field>, ObjectError> {
    if depth > limits.max_record_depth {
        return Err(ObjectError::LimitExceeded {
            kind: "record depth",
            limit: limits.max_record_depth as u64,
            found: depth as u64,
        });
    }
    let mut fields = Vec::new();
    let mut prev: u64 = 0;
    while *pos < end {
        let tag = decode_u64(buf, pos)?;
        if tag == 0 || tag >= 0xF0 {
            return Err(ObjectError::ReservedTag { tag });
        }
        if tag == prev {
            return Err(ObjectError::DuplicateField { tag });
        }
        if tag < prev {
            return Err(ObjectError::UnsortedFields { tag, prev });
        }
        prev = tag;

        let len = decode_u64(buf, pos)?;
        if len > limits.max_object_bytes {
            return Err(ObjectError::LimitExceeded {
                kind: "field size",
                limit: limits.max_object_bytes,
                found: len,
            });
        }
        let value_start = *pos;
        let value_end = value_start
            .checked_add(len as usize)
            .ok_or(ObjectError::LengthMismatch)?;
        if value_end > end {
            return Err(ObjectError::LengthMismatch);
        }

        let value = if tag > TAG_MAX_SCHEMA as u64 {
            if !schema.extensions_allowed {
                return Err(ObjectError::ExtensionNotPermitted {
                    family: schema.family,
                    tag,
                });
            }
            *pos = value_end;
            Value::Raw(buf[value_start..value_end].to_vec())
        } else {
            let spec = schema
                .field(tag as u8)
                .ok_or(ObjectError::UnknownMandatoryField {
                    family: schema.family,
                    tag,
                })?;
            let mut p = value_start;
            let value = decode_value(&spec.ty, buf, &mut p, value_end, depth + 1, schema, limits)?;
            if p != value_end {
                return Err(ObjectError::ValueLengthMismatch);
            }
            *pos = value_end;
            value
        };
        fields.push(Field::new(tag as u8, value));
    }

    for spec in schema.fields {
        if spec.required && !fields.iter().any(|f| f.tag == spec.tag) {
            return Err(ObjectError::MissingRequiredField {
                family: schema.family,
                tag: spec.tag,
                name: spec.name,
            });
        }
    }
    Ok(fields)
}

/// Decodes a value of the given type from `buf[*pos..end]`, advancing `*pos`
/// past the value. The value must be fully contained within the bound.
#[allow(clippy::too_many_arguments)]
fn decode_value(
    ty: &Type,
    buf: &[u8],
    pos: &mut usize,
    end: usize,
    depth: usize,
    parent: &FamilySchema,
    limits: &Limits,
) -> Result<Value, ObjectError> {
    match ty {
        Type::U64 => Ok(Value::U(decode_u64(buf, pos)?)),
        Type::I64 => Ok(Value::I(decode_i64(buf, pos)?)),
        Type::Bool => {
            if end - *pos != 1 {
                return Err(ObjectError::ValueLengthMismatch);
            }
            match buf[*pos] {
                0x00 => {
                    *pos += 1;
                    Ok(Value::B(false))
                }
                0x01 => {
                    *pos += 1;
                    Ok(Value::B(true))
                }
                b => Err(ObjectError::InvalidBoolean { byte: b }),
            }
        }
        Type::Bytes => {
            let len = decode_u64(buf, pos)?;
            if len > limits.max_string_bytes as u64 {
                return Err(ObjectError::LimitExceeded {
                    kind: "bytes size",
                    limit: limits.max_string_bytes as u64,
                    found: len,
                });
            }
            let value_end = pos
                .checked_add(len as usize)
                .ok_or(ObjectError::LengthMismatch)?;
            if value_end > end {
                return Err(ObjectError::LengthMismatch);
            }
            let bytes = buf[*pos..value_end].to_vec();
            *pos = value_end;
            Ok(Value::Bytes(bytes))
        }
        Type::Str | Type::Path | Type::Enum(_) => {
            let len = decode_u64(buf, pos)?;
            if len > limits.max_string_bytes as u64 {
                return Err(ObjectError::LimitExceeded {
                    kind: "string size",
                    limit: limits.max_string_bytes as u64,
                    found: len,
                });
            }
            let value_end = pos
                .checked_add(len as usize)
                .ok_or(ObjectError::LengthMismatch)?;
            if value_end > end {
                return Err(ObjectError::LengthMismatch);
            }
            let s = str::from_utf8(&buf[*pos..value_end]).map_err(|_| ObjectError::InvalidUtf8)?;
            *pos = value_end;
            if let Type::Enum(vals) = ty {
                if !vals.contains(&s) {
                    return Err(ObjectError::InvalidEnumValue {
                        field: "enum",
                        found: s.to_string(),
                    });
                }
            }
            if let Type::Path = ty {
                if !is_valid_canonical_path(s) {
                    return Err(ObjectError::InvalidPath {
                        path: s.to_string(),
                    });
                }
            }
            Ok(Value::Str(s.to_string()))
        }
        Type::Gid(expected) => {
            let g = decode_gid(buf, pos, end, limits)?;
            if g.family() != *expected {
                return Err(ObjectError::FamilyMismatch {
                    expected: *expected,
                    found: g.family(),
                });
            }
            Ok(Value::Gid(g))
        }
        Type::GidAny => Ok(Value::Gid(decode_gid(buf, pos, end, limits)?)),
        Type::Record(inner) => {
            let len = decode_u64(buf, pos)?;
            if len > limits.max_object_bytes {
                return Err(ObjectError::LimitExceeded {
                    kind: "record size",
                    limit: limits.max_object_bytes,
                    found: len,
                });
            }
            let record_end = pos
                .checked_add(len as usize)
                .ok_or(ObjectError::LengthMismatch)?;
            if record_end > end {
                return Err(ObjectError::LengthMismatch);
            }
            let nested = FamilySchema {
                family: parent.family,
                schemever: parent.schemever,
                extensions_allowed: parent.extensions_allowed,
                fields: inner,
            };
            let fields = decode_record_fields(&nested, buf, pos, record_end, depth + 1, limits)?;
            if *pos != record_end {
                return Err(ObjectError::ValueLengthMismatch);
            }
            Ok(Value::Record(fields))
        }
        Type::Array(inner) => {
            let count = decode_u64(buf, pos)?;
            if count > limits.max_array_elements as u64 {
                return Err(ObjectError::LimitExceeded {
                    kind: "array size",
                    limit: limits.max_array_elements as u64,
                    found: count,
                });
            }
            let mut items = Vec::with_capacity(count.min(1_000_000) as usize);
            for _ in 0..count {
                let item = decode_value(inner, buf, pos, end, depth + 1, parent, limits)?;
                items.push(item);
            }
            Ok(Value::Array(items))
        }
    }
}

/// Decodes a 33-byte GID reference.
fn decode_gid(
    buf: &[u8],
    pos: &mut usize,
    end: usize,
    limits: &Limits,
) -> Result<Gid, ObjectError> {
    let len = decode_u64(buf, pos)?;
    if len > limits.max_string_bytes as u64 {
        return Err(ObjectError::LimitExceeded {
            kind: "gid size",
            limit: limits.max_string_bytes as u64,
            found: len,
        });
    }
    if len != 33 {
        return Err(ObjectError::InvalidGid { len: len as usize });
    }
    let value_end = pos.checked_add(33).ok_or(ObjectError::LengthMismatch)?;
    if value_end > end {
        return Err(ObjectError::LengthMismatch);
    }
    let mut bytes = [0u8; 33];
    bytes.copy_from_slice(&buf[*pos..value_end]);
    *pos = value_end;
    Gid::from_bytes(&bytes).ok_or(ObjectError::UnknownFamily { code: bytes[0] })
}
