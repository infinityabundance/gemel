//! The canonical encoder (OBJECT_MODEL.md §1).
//!
//! Encoding is deterministic and validates everything: field ordering, tag
//! ranges, required fields, types, enum values, path rules, and limits.

use crate::consts::{ENC_VERSION, FLAGS_ZERO, MAGIC};
use crate::error::ObjectError;
use crate::spec::{schema_for, FamilySchema, Type};
use crate::validate::{is_valid_canonical_path, validate_family};
use crate::value::{Body, Field, Object, Value};
use gemel_core::family::Family;
use gemel_core::limits::Limits;
use gemel_core::varint::{encode_i64, encode_u64};

/// Encodes an object into its canonical envelope bytes.
pub fn encode_object(obj: &Object, limits: &Limits) -> Result<Vec<u8>, ObjectError> {
    let schema = schema_for(obj.family);
    if !obj.family.supports_schemever(obj.schemever) {
        return Err(ObjectError::UnknownSchemaVersion {
            family: obj.family,
            found: obj.schemever,
        });
    }
    validate_family(obj, limits)?;

    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.push(ENC_VERSION);
    out.push(obj.family.code());
    out.push(obj.schemever);
    out.push(FLAGS_ZERO);

    match &obj.body {
        Body::Blob(bytes) => {
            if obj.family != Family::Blob {
                return Err(ObjectError::BodyKindMismatch { family: obj.family });
            }
            let total = 8 + 1 + 8 + bytes.len() as u64; // upper bound for header sizing
            if total > limits.max_object_bytes {
                return Err(ObjectError::LimitExceeded {
                    kind: "object size",
                    limit: limits.max_object_bytes,
                    found: total,
                });
            }
            encode_u64(bytes.len() as u64, &mut out);
            out.extend_from_slice(bytes);
        }
        Body::Fields(fields) => {
            if obj.family == Family::Blob {
                return Err(ObjectError::BodyKindMismatch { family: obj.family });
            }
            let body = encode_record_fields(schema, fields, 1, limits)?;
            let total = 8 + 1 + 8 + body.len() as u64;
            if total > limits.max_object_bytes {
                return Err(ObjectError::LimitExceeded {
                    kind: "object size",
                    limit: limits.max_object_bytes,
                    found: total,
                });
            }
            encode_u64(body.len() as u64, &mut out);
            out.extend_from_slice(&body);
        }
    }
    Ok(out)
}

/// Encodes a record body (field sequence without a length prefix).
pub(crate) fn encode_record_fields(
    schema: &FamilySchema,
    fields: &[Field],
    depth: usize,
    limits: &Limits,
) -> Result<Vec<u8>, ObjectError> {
    if depth > limits.max_record_depth {
        return Err(ObjectError::LimitExceeded {
            kind: "record depth",
            limit: limits.max_record_depth as u64,
            found: depth as u64,
        });
    }
    let mut out = Vec::new();
    let mut prev: u64 = 0;
    for field in fields {
        let tag = field.tag as u64;
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

        let mut value_bytes = Vec::new();
        match &field.value {
            Value::Raw(raw) => {
                if tag <= crate::consts::TAG_MAX_SCHEMA as u64 {
                    return Err(ObjectError::UnexpectedRawValue { tag });
                }
                if !schema.extensions_allowed {
                    return Err(ObjectError::ExtensionNotPermitted {
                        family: schema.family,
                        tag,
                    });
                }
                value_bytes.extend_from_slice(raw);
            }
            _ => {
                if tag >= 0x80 {
                    return Err(ObjectError::ExtensionMustBeRaw { tag });
                }
                let spec = schema
                    .field(field.tag)
                    .ok_or(ObjectError::UnknownMandatoryField {
                        family: schema.family,
                        tag,
                    })?;
                encode_value(
                    &spec.ty,
                    &field.value,
                    field.tag,
                    &mut value_bytes,
                    depth + 1,
                    schema,
                    limits,
                )?;
            }
        }
        encode_u64(tag, &mut out);
        encode_u64(value_bytes.len() as u64, &mut out);
        out.extend_from_slice(&value_bytes);
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
    Ok(out)
}

/// Encodes a value according to its schema type (self-delimiting bytes).
#[allow(clippy::too_many_arguments)]
fn encode_value(
    ty: &Type,
    value: &Value,
    tag: u8,
    out: &mut Vec<u8>,
    depth: usize,
    parent: &FamilySchema,
    limits: &Limits,
) -> Result<(), ObjectError> {
    match (ty, value) {
        (Type::U64, Value::U(v)) => encode_u64(*v, out),
        (Type::I64, Value::I(v)) => encode_i64(*v, out),
        (Type::Bool, Value::B(b)) => out.push(if *b { 0x01 } else { 0x00 }),
        (Type::Bytes, Value::Bytes(b)) => {
            check_len("bytes size", b.len(), limits.max_string_bytes)?;
            encode_u64(b.len() as u64, out);
            out.extend_from_slice(b);
        }
        (Type::Str, Value::Str(s)) => {
            check_len("string size", s.len(), limits.max_string_bytes)?;
            encode_u64(s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        (Type::Path, Value::Str(s)) => {
            if !is_valid_canonical_path(s) {
                return Err(ObjectError::InvalidPath { path: s.clone() });
            }
            check_len("string size", s.len(), limits.max_string_bytes)?;
            encode_u64(s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        (Type::Enum(vals), Value::Str(s)) => {
            if !vals.contains(&s.as_str()) {
                return Err(ObjectError::InvalidEnumValue {
                    field: "enum",
                    found: s.clone(),
                });
            }
            check_len("string size", s.len(), limits.max_string_bytes)?;
            encode_u64(s.len() as u64, out);
            out.extend_from_slice(s.as_bytes());
        }
        (Type::Gid(expected), Value::Gid(g)) => {
            if g.family() != *expected {
                return Err(ObjectError::FamilyMismatch {
                    expected: *expected,
                    found: g.family(),
                });
            }
            encode_gid(g, out);
        }
        (Type::GidAny, Value::Gid(g)) => encode_gid(g, out),
        (Type::Record(inner), Value::Record(fields)) => {
            let nested = FamilySchema {
                family: parent.family,
                schemever: parent.schemever,
                extensions_allowed: parent.extensions_allowed,
                fields: inner,
            };
            let body = encode_record_fields(&nested, fields, depth + 1, limits)?;
            encode_u64(body.len() as u64, out);
            out.extend_from_slice(&body);
        }
        (Type::Array(inner), Value::Array(items)) => {
            check_len("array size", items.len(), limits.max_array_elements)?;
            encode_u64(items.len() as u64, out);
            for item in items {
                encode_value(inner, item, tag, out, depth + 1, parent, limits)?;
            }
        }
        (_, _) => {
            return Err(ObjectError::TypeMismatch {
                tag: tag as u64,
                expected: type_name(ty),
            })
        }
    }
    Ok(())
}

fn encode_gid(g: &gemel_core::gid::Gid, out: &mut Vec<u8>) {
    let bytes = g.to_bytes();
    encode_u64(bytes.len() as u64, out);
    out.extend_from_slice(&bytes);
}

fn check_len(kind: &'static str, found: usize, limit: usize) -> Result<(), ObjectError> {
    if found > limit {
        return Err(ObjectError::LimitExceeded {
            kind,
            limit: limit as u64,
            found: found as u64,
        });
    }
    Ok(())
}

/// A human-readable type name for error messages.
pub fn type_name(ty: &Type) -> &'static str {
    match ty {
        Type::U64 => "uint",
        Type::I64 => "sint",
        Type::Bool => "bool",
        Type::Bytes => "bytes",
        Type::Str => "string",
        Type::Path => "path",
        Type::Enum(_) => "enum string",
        Type::Gid(_) => "gid",
        Type::GidAny => "gid",
        Type::Record(_) => "record",
        Type::Array(_) => "array",
    }
}
