//! Unit tests: determinism, round-trips, the fail-closed negative catalog,
//! limits, extension retention, and family validation.

use crate::decode::decode_object;
use crate::encode::encode_object;
use crate::error::ObjectError;
use crate::family::Family;
use crate::gid::Gid;
use crate::golden::build_all;
use crate::limits::Limits;
use crate::spec::{FamilySchema, FieldSpec, Type};
use crate::value::{Body, Field, Object, Value};
use crate::varint::encode_u64;

const LIMITS: Limits = Limits {
    max_object_bytes: 1 << 30,
    max_record_depth: 64,
    max_array_elements: 1_000_000,
    max_string_bytes: 16 << 20,
    max_refs_per_object: 100_000,
};

// ---------------------------------------------------------------------------
// Byte-crafting helpers (bypass the encoder for negative fixtures).
// ---------------------------------------------------------------------------

fn env(family: u8, schemever: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0x47, 0x45, 0x4D, 0x4C, 0x01, family, schemever, 0x00];
    let mut lb = Vec::new();
    encode_u64(body.len() as u64, &mut lb);
    out.extend(lb);
    out.extend(body);
    out
}

fn fld(tag: u8, val: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_u64(tag as u64, &mut out);
    encode_u64(val.len() as u64, &mut out);
    out.extend(val);
    out
}

fn u(v: u64) -> Vec<u8> {
    let mut o = Vec::new();
    encode_u64(v, &mut o);
    o
}

fn str_(s: &str) -> Vec<u8> {
    let mut o = Vec::new();
    encode_u64(s.len() as u64, &mut o);
    o.extend(s.as_bytes());
    o
}

fn gidb(family: u8) -> Vec<u8> {
    let mut o = vec![33, family];
    o.extend([0u8; 32]);
    o
}

fn recb(inner: &[Vec<u8>]) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    for f in inner {
        body.extend(f);
    }
    let mut o = Vec::new();
    encode_u64(body.len() as u64, &mut o);
    o.extend(body);
    o
}

fn arrb(count: u64, items: &[Vec<u8>]) -> Vec<u8> {
    let mut o = Vec::new();
    encode_u64(count, &mut o);
    for it in items {
        o.extend(it);
    }
    o
}

fn change_body(fields: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for f in fields {
        body.extend(f);
    }
    body
}

// ---------------------------------------------------------------------------
// Positive: determinism and round-trips
// ---------------------------------------------------------------------------

#[test]
fn all_fixtures_roundtrip_byte_exactly() {
    let built = build_all(&LIMITS).unwrap();
    assert!(built.len() >= 22, "every family should be covered");
    for b in &built {
        let bytes = encode_object(&b.object, &LIMITS).unwrap();
        let decoded = decode_object(&bytes, &LIMITS)
            .unwrap_or_else(|e| panic!("decode {}: {e}", b.fixture.name));
        assert_eq!(
            decoded, b.object,
            "decoded object differs for {}",
            b.fixture.name
        );
        let re = encode_object(&decoded, &LIMITS).unwrap();
        assert_eq!(re, bytes, "re-encode differs for {}", b.fixture.name);
    }
}

#[test]
fn encoding_is_deterministic() {
    let a = build_all(&LIMITS).unwrap();
    let b = build_all(&LIMITS).unwrap();
    for (x, y) in a.iter().zip(b.iter()) {
        let ex = encode_object(&x.object, &LIMITS).unwrap();
        let ey = encode_object(&y.object, &LIMITS).unwrap();
        assert_eq!(ex, ey, "nondeterministic encoding for {}", x.fixture.name);
    }
}

#[test]
fn distinct_objects_have_distinct_identities() {
    let built = build_all(&LIMITS).unwrap();
    let mut ids = std::collections::HashSet::new();
    for b in &built {
        assert!(ids.insert(b.gid), "duplicate identity: {}", b.fixture.name);
    }
    assert_ne!(built[0].gid, built[1].gid); // blob-empty vs blob-hello
}

// ---------------------------------------------------------------------------
// Negative: the fail-closed catalog
// ---------------------------------------------------------------------------

fn expect_err(bytes: &[u8], expected: ObjectError) {
    match decode_object(bytes, &LIMITS) {
        Ok(obj) => panic!("expected error {expected:?}, got object {:?}", obj.family),
        Err(e) => assert_eq!(e, expected, "wrong error for {}", expected),
    }
}

#[test]
fn bad_magic() {
    let mut bytes = env(0x01, 1, b"hi");
    bytes[0] = 0x00;
    expect_err(&bytes, ObjectError::BadMagic);
}

#[test]
fn bad_encver() {
    let mut bytes = env(0x01, 1, b"hi");
    bytes[4] = 0x02;
    expect_err(&bytes, ObjectError::UnknownEncodingVersion { found: 2 });
}

#[test]
fn bad_family() {
    expect_err(
        &env(0x17, 1, &[]),
        ObjectError::UnknownFamily { code: 0x17 },
    );
}

#[test]
fn bad_flags() {
    let mut bytes = env(0x01, 1, b"hi");
    bytes[7] = 0x01;
    expect_err(&bytes, ObjectError::ReservedFlags { found: 0x01 });
}

#[test]
fn bad_schemever() {
    expect_err(
        &env(0x07, 2, &[]),
        ObjectError::UnknownSchemaVersion {
            family: Family::Change,
            found: 2,
        },
    );
}

#[test]
fn noncanonical_bodylen() {
    let mut bytes = vec![0x47, 0x45, 0x4D, 0x4C, 0x01, 0x01, 0x01, 0x00];
    bytes.extend([0x80u8, 0x00]);
    expect_err(&bytes, ObjectError::NonCanonicalInteger);
}

#[test]
fn trailing_bytes() {
    let mut bytes = env(0x01, 1, b"hi");
    bytes.push(0x00);
    expect_err(&bytes, ObjectError::TrailingBytes);
}

#[test]
fn truncation() {
    let mut bytes = env(0x01, 1, b"hi");
    bytes.truncate(bytes.len() - 1);
    expect_err(&bytes, ObjectError::LengthMismatch);
}

#[test]
fn unsorted_fields() {
    let body = change_body(&[fld(0x02, &gidb(0x06)), fld(0x01, &str_("x"))]);
    expect_err(
        &env(0x07, 1, &body),
        ObjectError::UnsortedFields { tag: 1, prev: 2 },
    );
}

#[test]
fn duplicate_field() {
    let body = change_body(&[fld(0x01, &str_("x")), fld(0x01, &str_("y"))]);
    expect_err(&env(0x07, 1, &body), ObjectError::DuplicateField { tag: 1 });
}

#[test]
fn reserved_tags() {
    let body = change_body(&[fld(0xF1, &str_("x"))]);
    expect_err(&env(0x07, 1, &body), ObjectError::ReservedTag { tag: 0xF1 });
    let body = change_body(&[fld(0x00, &str_("x"))]);
    expect_err(&env(0x07, 1, &body), ObjectError::ReservedTag { tag: 0 });
}

#[test]
fn unknown_mandatory_field() {
    let body = change_body(&[fld(0x50, &str_("x"))]);
    expect_err(
        &env(0x07, 1, &body),
        ObjectError::UnknownMandatoryField {
            family: Family::Change,
            tag: 0x50,
        },
    );
}

#[test]
fn invalid_utf8() {
    let body = change_body(&[fld(0x01, &[0x02, 0xC3, 0x28])]);
    expect_err(&env(0x07, 1, &body), ObjectError::InvalidUtf8);
}

#[test]
fn gid_wrong_length() {
    let mut value = vec![32, 0x06];
    value.extend([0u8; 32]);
    let body = change_body(&[fld(0x02, &value)]);
    expect_err(&env(0x07, 1, &body), ObjectError::InvalidGid { len: 32 });
}

#[test]
fn gid_family_mismatch() {
    let body = change_body(&[fld(0x02, &gidb(0x01))]); // blob gid in an intent field
    expect_err(
        &env(0x07, 1, &body),
        ObjectError::FamilyMismatch {
            expected: Family::Intent,
            found: Family::Blob,
        },
    );
}

#[test]
fn missing_required_field() {
    expect_err(
        &env(0x07, 1, &[]),
        ObjectError::MissingRequiredField {
            family: Family::Change,
            tag: 0x01,
            name: "summary",
        },
    );
}

#[test]
fn invalid_enum_value() {
    let body = change_body(&[fld(0x01, &str_("frobnicate"))]);
    expect_err(
        &env(0x04, 1, &body),
        ObjectError::InvalidEnumValue {
            field: "enum",
            found: "frobnicate".into(),
        },
    );
}

#[test]
fn invalid_bool() {
    let repro = recb(&[fld(0x01, &[0x02])]);
    let body = change_body(&[
        fld(0x01, &gidb(0x0E)),
        fld(0x02, &str_("test_result")),
        fld(0x0F, &repro),
    ]);
    expect_err(
        &env(0x0B, 1, &body),
        ObjectError::InvalidBoolean { byte: 0x02 },
    );
}

#[test]
fn invalid_path() {
    let body = change_body(&[
        fld(0x01, &str_("create_file")),
        fld(0x02, &str_("/etc/passwd")),
    ]);
    expect_err(
        &env(0x04, 1, &body),
        ObjectError::InvalidPath {
            path: "/etc/passwd".into(),
        },
    );
}

#[test]
fn tree_mode_invalid() {
    let entry = recb(&[
        fld(0x01, &str_("a")),
        fld(0x02, &u(420)),
        fld(0x03, &gidb(0x01)),
    ]);
    let body = change_body(&[fld(0x01, &arrb(1, &[entry]))]);
    expect_err(
        &env(0x02, 1, &body),
        ObjectError::InvalidTreeMode { mode: 420 },
    );
}

#[test]
fn tree_entries_unsorted() {
    let b = recb(&[
        fld(0x01, &str_("b")),
        fld(0x02, &u(0o100644)),
        fld(0x03, &gidb(0x01)),
    ]);
    let a = recb(&[
        fld(0x01, &str_("a")),
        fld(0x02, &u(0o100644)),
        fld(0x03, &gidb(0x01)),
    ]);
    let body = change_body(&[fld(0x01, &arrb(2, &[b, a]))]);
    expect_err(
        &env(0x02, 1, &body),
        ObjectError::InvalidTreeOrder { name: "a".into() },
    );
}

#[test]
fn tree_name_invalid() {
    let entry = recb(&[
        fld(0x01, &str_("a/b")),
        fld(0x02, &u(0o100644)),
        fld(0x03, &gidb(0x01)),
    ]);
    let body = change_body(&[fld(0x01, &arrb(1, &[entry]))]);
    expect_err(
        &env(0x02, 1, &body),
        ObjectError::InvalidTreeName { name: "a/b".into() },
    );
}

#[test]
fn tree_target_family_mismatch() {
    let entry = recb(&[
        fld(0x01, &str_("a")),
        fld(0x02, &u(0o100644)),
        fld(0x03, &gidb(0x02)),
    ]);
    let body = change_body(&[fld(0x01, &arrb(1, &[entry]))]);
    expect_err(
        &env(0x02, 1, &body),
        ObjectError::InvalidTreeTargetFamily {
            name: "a".into(),
            mode: 0o100644,
        },
    );
}

#[test]
fn operation_undeclared_tag() {
    let body = change_body(&[fld(0x01, &str_("create_file")), fld(0x12, &u(0))]);
    expect_err(
        &env(0x04, 1, &body),
        ObjectError::UndeclaredOperationTag {
            op_type: "create_file".into(),
            tag: 0x12,
        },
    );
}

#[test]
fn mapping_fabricated_nonempty() {
    let loss = recb(&[fld(0x03, &arrb(1, &[str_("x")]))]);
    let body = change_body(&[
        fld(0x01, &str_("git_commit")),
        fld(0x02, &str_("abc")),
        fld(0x03, &gidb(0x07)),
        fld(0x04, &loss),
    ]);
    expect_err(&env(0x16, 1, &body), ObjectError::MappingFabricatedNonEmpty);
}

#[test]
fn claim_empty_predicate() {
    let body = change_body(&[fld(0x03, &str_("")), fld(0x07, &gidb(0x0E))]);
    expect_err(&env(0x0A, 1, &body), ObjectError::EmptyPredicate);
}

// ---------------------------------------------------------------------------
// Negative: limits
// ---------------------------------------------------------------------------

#[test]
fn depth_bomb() {
    static RECURSIVE: [FieldSpec; 1] = [FieldSpec::new(
        0x01,
        "next",
        Type::Record(&RECURSIVE),
        false,
    )];
    let schema = FamilySchema {
        family: Family::Change,
        schemever: 1,
        extensions_allowed: true,
        fields: &RECURSIVE,
    };
    // 10 levels decode fine. `v` is a length-prefixed record value; decode
    // its body via decode_record_fields (which expects a field sequence).
    let mut v = recb(&[]);
    for _ in 1..10 {
        v = recb(&[fld(0x01, &v)]);
    }
    let mut p = 0;
    let bodylen = crate::varint::decode_u64(&v, &mut p).unwrap() as usize;
    assert_eq!(bodylen, v.len() - p);
    let body = &v[p..];
    match crate::decode::decode_record_fields(&schema, body, &mut 0, body.len(), 1, &LIMITS) {
        Ok(_) => {}
        Err(e) => panic!("10-level decode failed: {e:?}"),
    }
    // 70 levels exceed the depth limit.
    let mut v = recb(&[]);
    for _ in 1..70 {
        v = recb(&[fld(0x01, &v)]);
    }
    let mut p = 0;
    crate::varint::decode_u64(&v, &mut p).unwrap();
    let body = &v[p..];
    match crate::decode::decode_record_fields(&schema, body, &mut 0, body.len(), 1, &LIMITS) {
        Err(ObjectError::LimitExceeded { kind, .. }) => assert_eq!(kind, "record depth"),
        other => panic!("expected depth limit error, got {other:?}"),
    }
}

#[test]
fn array_limit() {
    let small = Limits {
        max_array_elements: 3,
        ..LIMITS
    };
    let body = change_body(&[
        fld(0x01, &str_("create_file")),
        fld(0x04, &arrb(4, &vec![gidb(0x01); 4])),
    ]);
    let bytes = env(0x04, 1, &body);
    match decode_object(&bytes, &small) {
        Err(ObjectError::LimitExceeded { kind, .. }) => assert_eq!(kind, "array size"),
        other => panic!("expected array limit error, got {other:?}"),
    }
}

#[test]
fn string_limit() {
    let small = Limits {
        max_string_bytes: 4,
        ..LIMITS
    };
    let body = change_body(&[fld(0x01, &str_("hello"))]);
    let bytes = env(0x07, 1, &body);
    match decode_object(&bytes, &small) {
        Err(ObjectError::LimitExceeded { kind, .. }) => assert_eq!(kind, "string size"),
        other => panic!("expected string limit error, got {other:?}"),
    }
}

#[test]
fn object_size_limit() {
    let small = Limits {
        max_object_bytes: 16,
        ..LIMITS
    };
    let bytes = env(0x01, 1, &[0u8; 17]);
    match decode_object(&bytes, &small) {
        Err(ObjectError::LimitExceeded { kind, .. }) => assert_eq!(kind, "object size"),
        other => panic!("expected object size limit error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Negative: encoder-side misuse
// ---------------------------------------------------------------------------

#[test]
fn raw_value_misuse() {
    // Raw payload under a schema tag.
    let obj = Object::fields(
        Family::Change,
        vec![Field::new(0x01, Value::Raw(vec![0x01]))],
    );
    assert_eq!(
        encode_object(&obj, &LIMITS).unwrap_err(),
        ObjectError::UnexpectedRawValue { tag: 1 }
    );
    // Typed value under an extension tag.
    let obj = Object::fields(
        Family::Change,
        vec![Field::new(0x80, Value::Str("x".into()))],
    );
    assert_eq!(
        encode_object(&obj, &LIMITS).unwrap_err(),
        ObjectError::ExtensionMustBeRaw { tag: 0x80 }
    );
}

#[test]
fn body_kind_mismatch() {
    let blob_as_fields = Object {
        family: Family::Blob,
        schemever: 1,
        body: Body::Fields(vec![]),
    };
    assert_eq!(
        encode_object(&blob_as_fields, &LIMITS).unwrap_err(),
        ObjectError::BodyKindMismatch {
            family: Family::Blob
        }
    );
    let tree_as_blob = Object {
        family: Family::Tree,
        schemever: 1,
        body: Body::Blob(vec![]),
    };
    assert_eq!(
        encode_object(&tree_as_blob, &LIMITS).unwrap_err(),
        ObjectError::BodyKindMismatch {
            family: Family::Tree
        }
    );
}

// ---------------------------------------------------------------------------
// Extension retention
// ---------------------------------------------------------------------------

#[test]
fn extension_fields_retain_losslessly() {
    let built = build_all(&LIMITS).unwrap();
    let ext = built
        .iter()
        .find(|b| b.fixture.name == "extension-change")
        .unwrap();
    let bytes = encode_object(&ext.object, &LIMITS).unwrap();
    let decoded = decode_object(&bytes, &LIMITS).unwrap();
    let fields = decoded.field_sequence().unwrap();
    let raw = fields
        .iter()
        .find(|f| f.tag == 0x80)
        .expect("extension tag present");
    assert_eq!(raw.value, Value::Raw(vec![0x03, 0x68, 0x69, 0x21]));
    // Byte-exact round trip through decode → encode.
    let re = encode_object(&decoded, &LIMITS).unwrap();
    assert_eq!(re, bytes);
}

// ---------------------------------------------------------------------------
// Family validation on the encode path
// ---------------------------------------------------------------------------

#[test]
fn encode_rejects_invalid_tree() {
    let entry = crate::value::Field::new(
        0x01,
        Value::Array(vec![Value::Record(vec![
            Field::new(0x01, Value::Str("a".into())),
            Field::new(0x02, Value::U(420)),
            Field::new(0x03, Value::Gid(Gid::new(Family::Blob, [0u8; 32]))),
        ])]),
    );
    let obj = Object::fields(Family::Tree, vec![entry]);
    assert!(matches!(
        encode_object(&obj, &LIMITS),
        Err(ObjectError::InvalidTreeMode { .. })
    ));
}

#[test]
fn family_table_matches_schemas() {
    let schemas = crate::spec::all_schemas();
    assert_eq!(schemas.len(), Family::ALL.len());
    let schema_families: Vec<Family> = schemas.iter().map(|s| s.family).collect();
    for f in Family::ALL {
        assert!(schema_families.contains(&f), "missing schema for {f}");
    }
}
