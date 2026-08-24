//! The deterministic JSON projection of canonical objects
//! (OBJECT_MODEL.md §10.3, AGENT_PROTOCOL.md §3).
//!
//! Names in the output are annotations; tags are authoritative. UINT/SINT
//! values are decimal strings (exact); BYTES are lowercase hex; GIDs are
//! textual identities; optional absent fields are omitted; `null` is never
//! emitted for absent optional fields.

use crate::error::ObjectError;
use crate::spec::{schema_for, FamilySchema, Type};
use crate::value::{Body, Field, Object, Value};
use crate::hex;
use serde_json::{json, Value as Json};

/// The canonical JSON projection of an object.
pub fn object_to_json(obj: &Object) -> Result<Json, ObjectError> {
    let schema = schema_for(obj.family);
    let body = match &obj.body {
        Body::Blob(bytes) => json!(hex::encode(bytes)),
        Body::Fields(fields) => Json::Array(
            fields
                .iter()
                .map(|f| field_to_json(f, schema))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    Ok(json!({
        "family": obj.family.short(),
        "schemever": obj.schemever,
        "body": body,
    }))
}

/// The JSON projection of a single field, annotated with the schema name.
pub fn field_to_json(field: &Field, schema: &FamilySchema) -> Result<Json, ObjectError> {
    let ty = schema.field(field.tag).map(|s| s.ty);
    let mut map = serde_json::Map::new();
    map.insert("tag".to_string(), json!(field.tag));
    if let Some(spec) = schema.field(field.tag) {
        map.insert("name".to_string(), json!(spec.name));
    }
    map.insert("value".to_string(), value_to_json(&field.value, ty));
    Ok(Json::Object(map))
}

/// The JSON projection of a value. `ty` (when known) provides nested record
/// names; tags remain authoritative.
pub fn value_to_json(value: &Value, ty: Option<Type>) -> Json {
    match value {
        Value::U(v) => json!(v.to_string()),
        Value::I(v) => json!(v.to_string()),
        Value::B(b) => json!(b),
        Value::Bytes(b) => json!(hex::encode(b)),
        Value::Str(s) => json!(s),
        Value::Gid(g) => json!(g.to_string()),
        Value::Record(fields) => {
            let inner = match ty {
                Some(Type::Record(inner)) => Some(inner),
                _ => None,
            };
            let nested = FamilySchema {
                family: crate::family::Family::Change, // names only; family unused here
                schemever: 1,
                extensions_allowed: true,
                fields: inner.unwrap_or(&[]),
            };
            Json::Array(
                fields
                    .iter()
                    .map(|f| field_to_json(f, &nested))
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_default(),
            )
        }
        Value::Array(items) => {
            let inner_ty = match ty {
                Some(Type::Array(inner)) => Some(*inner),
                _ => None,
            };
            Json::Array(items.iter().map(|it| value_to_json(it, inner_ty)).collect())
        }
        Value::Raw(b) => json!(hex::encode(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Field;
    use crate::family::Family;
    use crate::gid::Gid;

    #[test]
    fn projection_shape() {
        let obj = Object::fields(
            Family::State,
            vec![Field::new(
                0x01,
                Value::Gid(Gid::new(Family::Tree, [0u8; 32])),
            )],
        );
        let j = object_to_json(&obj).unwrap();
        assert_eq!(j["family"], "state");
        assert_eq!(j["schemever"], 1);
        assert_eq!(j["body"][0]["tag"], 1);
        assert_eq!(j["body"][0]["name"], "root_tree");
        assert!(j["body"][0]["value"].as_str().unwrap().starts_with("tree."));
    }
}
