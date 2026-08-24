//! Family-specific object validation (INVARIANTS.md §4) and path rules.

use crate::error::ObjectError;
use crate::spec::{op_kind_tags, schema_for};
use crate::value::{Body, Field, Object, Value};
use gemel_core::family::Family;
use gemel_core::limits::Limits;

/// Tree modes (OBJECT_MODEL.md §6.2).
pub const MODE_FILE: u64 = 0o100644;
pub const MODE_EXEC: u64 = 0o100755;
pub const MODE_SYMLINK: u64 = 0o120000;
pub const MODE_DIR: u64 = 0o040000;

/// Validates a canonical path (OBJECT_MODEL.md §1.6).
pub fn is_valid_canonical_path(p: &str) -> bool {
    if p.is_empty() {
        return false;
    }
    if p.starts_with('/') || p.ends_with('/') {
        return false;
    }
    if p.contains('\\') || p.contains('\0') {
        return false;
    }
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return false;
        }
    }
    true
}

/// Validates a tree entry name: a single segment (no separators).
pub fn is_valid_tree_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
}

/// Runs the family-specific validation pass over an object.
///
/// Called by both the encoder and the decoder, so invalid objects can neither
/// be written nor accepted from hostile input.
pub fn validate_family(obj: &Object, limits: &Limits) -> Result<(), ObjectError> {
    match obj.family {
        Family::Blob => {}
        Family::Tree => validate_tree(obj)?,
        Family::Operation => validate_operation(obj)?,
        Family::Claim => validate_claim(obj)?,
        Family::Mapping => validate_mapping(obj)?,
        _ => {}
    }
    let refs = match &obj.body {
        Body::Fields(fields) => count_refs_fields(fields),
        Body::Blob(_) => 0,
    };
    if refs > limits.max_refs_per_object {
        return Err(ObjectError::RefCountExceeded {
            limit: limits.max_refs_per_object,
            found: refs,
        });
    }
    Ok(())
}

fn validate_tree(obj: &Object) -> Result<(), ObjectError> {
    let fields = obj
        .field_sequence()
        .ok_or(ObjectError::BodyKindMismatch { family: obj.family })?;
    let entries = find_array(fields, 0x01).ok_or(ObjectError::MissingRequiredField {
        family: obj.family,
        tag: 0x01,
        name: "entries",
    })?;

    let mut prev_name: Option<&[u8]> = None;
    for entry in entries {
        let record = match entry {
            Value::Record(fields) => fields,
            _ => {
                return Err(ObjectError::TypeMismatch {
                    tag: 0x01,
                    expected: "record",
                })
            }
        };
        let name = find_str(record, 0x01).ok_or(ObjectError::MissingRequiredField {
            family: obj.family,
            tag: 0x01,
            name: "name",
        })?;
        if !is_valid_tree_name(name) {
            return Err(ObjectError::InvalidTreeName {
                name: name.to_string(),
            });
        }
        let mode = match find_value(record, 0x02) {
            Some(Value::U(m)) => *m,
            _ => {
                return Err(ObjectError::MissingRequiredField {
                    family: obj.family,
                    tag: 0x02,
                    name: "mode",
                })
            }
        };
        if !matches!(mode, MODE_FILE | MODE_EXEC | MODE_SYMLINK | MODE_DIR) {
            return Err(ObjectError::InvalidTreeMode { mode });
        }
        let target = match find_value(record, 0x03) {
            Some(Value::Gid(g)) => *g,
            _ => {
                return Err(ObjectError::MissingRequiredField {
                    family: obj.family,
                    tag: 0x03,
                    name: "target",
                })
            }
        };
        let expected_family = if mode == MODE_DIR {
            Family::Tree
        } else {
            Family::Blob
        };
        if target.family() != expected_family {
            return Err(ObjectError::InvalidTreeTargetFamily {
                name: name.to_string(),
                mode,
            });
        }
        if let Some(prev) = prev_name {
            if name.as_bytes() <= prev {
                return Err(ObjectError::InvalidTreeOrder {
                    name: name.to_string(),
                });
            }
        }
        prev_name = Some(name.as_bytes());
    }
    Ok(())
}

fn validate_operation(obj: &Object) -> Result<(), ObjectError> {
    let fields = obj
        .field_sequence()
        .ok_or(ObjectError::BodyKindMismatch { family: obj.family })?;
    let op_type = match find_value(fields, 0x01) {
        Some(Value::Str(s)) => s.as_str(),
        _ => {
            return Err(ObjectError::MissingRequiredField {
                family: obj.family,
                tag: 0x01,
                name: "op_type",
            })
        }
    };
    let declared = op_kind_tags(op_type);
    if let Some(allowed) = declared {
        for f in fields {
            if f.tag >= 0x11 && !allowed.contains(&f.tag) {
                return Err(ObjectError::UndeclaredOperationTag {
                    op_type: op_type.to_string(),
                    tag: f.tag as u64,
                });
            }
        }
    }
    Ok(())
}

fn validate_claim(obj: &Object) -> Result<(), ObjectError> {
    let fields = obj
        .field_sequence()
        .ok_or(ObjectError::BodyKindMismatch { family: obj.family })?;
    if let Some(Value::Str(predicate)) = find_value(fields, 0x03) {
        if predicate.is_empty() {
            return Err(ObjectError::EmptyPredicate);
        }
    }
    Ok(())
}

fn validate_mapping(obj: &Object) -> Result<(), ObjectError> {
    let fields = obj
        .field_sequence()
        .ok_or(ObjectError::BodyKindMismatch { family: obj.family })?;
    if let Some(Value::Record(loss)) = find_value(fields, 0x04) {
        if let Some(Value::Array(fabricated)) = find_value(loss, 0x03) {
            if !fabricated.is_empty() {
                return Err(ObjectError::MappingFabricatedNonEmpty);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_value(fields: &[Field], tag: u8) -> Option<&Value> {
    fields.iter().find(|f| f.tag == tag).map(|f| &f.value)
}

fn find_str(fields: &[Field], tag: u8) -> Option<&str> {
    match find_value(fields, tag) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

fn find_array(fields: &[Field], tag: u8) -> Option<&[Value]> {
    match find_value(fields, tag) {
        Some(Value::Array(items)) => Some(items),
        _ => None,
    }
}

fn count_refs_fields(fields: &[Field]) -> usize {
    fields.iter().map(|f| count_refs(&f.value)).sum()
}

fn count_refs(value: &Value) -> usize {
    match value {
        Value::Gid(_) => 1,
        Value::Record(fields) => count_refs_fields(fields),
        Value::Array(items) => items.iter().map(count_refs).sum(),
        _ => 0,
    }
}

/// Whether a schema exists for the family (used by tests and store layers).
pub fn is_known_family(family: Family) -> bool {
    schema_for(family).family == family
}
