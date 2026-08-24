//! Golden vector verification (OBJECT_MODEL.md §11–§12).
//!
//! For every vector in `golden/manifest.json`: decode the pinned bytes,
//! verify byte-exact round-trip, verify the pinned identity, verify envelope
//! metadata, verify cross-references resolve, and verify the rebuilt fixture
//! reproduces the pinned bytes exactly.

use gemel::hex;
use gemel::limits::Limits;
use gemel::decode::decode_object;
use gemel::encode::encode_object;
use gemel::golden::build_all;
use gemel::hash::object_id_bytes;
use gemel::value::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    match std::env::var("GEMEL_GOLDEN_DIR") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden"),
    }
}

fn read_hex(path: &Path) -> Vec<u8> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    hex::decode(text.trim()).unwrap_or_else(|e| panic!("hex {}: {e:?}", path.display()))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .trim()
        .to_string()
}

fn collect_gids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Gid(g) => out.push(g.to_string()),
        Value::Record(fields) => {
            for f in fields {
                collect_gids(&f.value, out);
            }
        }
        Value::Array(items) => {
            for it in items {
                collect_gids(it, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_vector_roundtrips_pins_identity_and_rebuilds() {
    let dir = golden_dir();
    let manifest_path = dir.join("manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap())
            .unwrap_or_else(|e| panic!("manifest {}: {e}", manifest_path.display()));
    assert_eq!(manifest["schema"], "gemel.golden.v1");
    assert_eq!(manifest["encver"], 1);

    let vectors = manifest["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());

    let built = build_all(&Limits::default()).expect("fixtures build");
    let by_name: HashMap<&str, &gemel::golden::BuiltFixture> =
        built.iter().map(|b| (b.fixture.name, b)).collect();

    let mut families = std::collections::HashSet::new();
    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let bytes = read_hex(&dir.join(v["bytes"].as_str().expect("bytes path")));

        // 1. Decode succeeds.
        let obj = decode_object(&bytes, &Limits::default())
            .unwrap_or_else(|e| panic!("decode {name}: {e}"));

        // 2. Byte-exact round trip.
        let re = encode_object(&obj, &Limits::default())
            .unwrap_or_else(|e| panic!("encode {name}: {e}"));
        assert_eq!(re, bytes, "re-encode of {name} differs from pinned bytes");

        // 3. Identity is pinned.
        let pinned_id = read_text(&dir.join(v["id"].as_str().expect("id path")));
        let computed = format!(
            "{}.{}",
            obj.family.short(),
            hex::encode(&object_id_bytes(&bytes))
        );
        assert_eq!(computed, pinned_id, "identity of {name}");

        // 4. Envelope metadata matches the manifest.
        assert_eq!(obj.family.short(), v["family"].as_str().expect("family"));
        assert_eq!(obj.schemever, v["schemever"].as_u64().unwrap() as u8);

        // 5. Rebuilt fixture reproduces the pinned bytes.
        let fixture = by_name
            .get(name)
            .unwrap_or_else(|| panic!("fixture {name}"));
        let rebuilt = encode_object(&fixture.object, &Limits::default()).expect("rebuild encodes");
        assert_eq!(rebuilt, bytes, "rebuilt {name} differs from pinned bytes");

        // 6. Cross-references resolve to pinned identities.
        let mut gids = Vec::new();
        match &obj.body {
            gemel::value::Body::Blob(_) => {}
            gemel::value::Body::Fields(fields) => {
                for f in fields {
                    collect_gids(&f.value, &mut gids);
                }
            }
        }
        if let Some(refs) = v.get("references").and_then(|r| r.as_object()) {
            for (_, id) in refs {
                let id = id.as_str().expect("reference id");
                assert!(
                    gids.contains(&id.to_string()),
                    "{name} should reference {id}"
                );
            }
        }
        families.insert(obj.family);
    }

    // 7. Every family is covered.
    assert_eq!(families.len(), 22, "all 22 families must be represented");
    for family in gemel::family::Family::ALL {
        assert!(families.contains(&family), "missing vector for {family}");
    }
}

#[test]
fn manifest_is_well_formed() {
    let dir = golden_dir();
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let vectors = manifest["vectors"].as_array().unwrap();
    let mut names = std::collections::HashSet::new();
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        assert!(
            names.insert(name.to_string()),
            "duplicate vector name {name}"
        );
        assert!(
            dir.join(v["bytes"].as_str().unwrap()).exists(),
            "missing {}",
            v["bytes"]
        );
        assert!(
            dir.join(v["id"].as_str().unwrap()).exists(),
            "missing {}",
            v["id"]
        );
    }
}
