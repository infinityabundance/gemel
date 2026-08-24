//! Golden vector generator (OBJECT_MODEL.md §10.4, §12).
//!
//! Builds the fixture set, encodes it, and pins the canonical bytes and
//! identities under `golden/vectors/`. Refuses to overwrite changed vectors
//! unless `--force` is passed: golden vectors are regenerated only as part of
//! a deliberate protocol change.

use gemel_core::hex;
use gemel_core::limits::Limits;
use gemel_object::encode::encode_object;
use gemel_object::golden::build_all;
use gemel_object::json::object_to_json;
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let force = args.iter().any(|a| a == "--force");
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../golden"));
    let vectors_dir = dir.join("vectors");

    let built = match build_all(&Limits::default()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("fixture build failed: {e}");
            std::process::exit(2);
        }
    };

    // Encode everything first (deterministic).
    let mut entries = Vec::new();
    for b in &built {
        let bytes = encode_object(&b.object, &Limits::default()).expect("fixture encodes");
        let id_text = b.gid.to_string();
        let references: serde_json::Map<String, serde_json::Value> = b
            .refs
            .iter()
            .map(|(name, gid)| ((*name).to_string(), json!(gid.to_string())))
            .collect();
        let constructed_from = object_to_json(&b.object).expect("json projection");
        entries.push(GoldenEntry {
            name: b.fixture.name,
            family: b.object.family.short(),
            schemever: b.object.schemever,
            bytes,
            id_text,
            description: b.fixture.description,
            references,
            constructed_from,
        });
    }

    // Compare against pinned files; refuse to overwrite without --force.
    let mut changed: Vec<&str> = Vec::new();
    for e in &entries {
        let hex_path = vectors_dir.join(format!("{}.gce.hex", e.name));
        let id_path = vectors_dir.join(format!("{}.id", e.name));
        if let (Ok(old_hex), Ok(old_id)) = (
            std::fs::read_to_string(&hex_path),
            std::fs::read_to_string(&id_path),
        ) {
            let want_hex = format!("{}\n", hex::encode(&e.bytes));
            let want_id = format!("{}\n", e.id_text);
            if old_hex != want_hex || old_id != want_id {
                changed.push(e.name);
            }
        } else if hex_path.exists() != id_path.exists() {
            changed.push(e.name);
        }
    }
    if !changed.is_empty() {
        if !force {
            eprintln!(
                "golden vectors would change: {}; refusing (protocol change requires --force):",
                changed.join(", ")
            );
            std::process::exit(1);
        }
        eprintln!("overwriting changed golden vectors: {}", changed.join(", "));
    } else if dir.join("manifest.json").exists() {
        println!("golden vectors are up to date ({})", entries.len());
        return;
    }

    std::fs::create_dir_all(&vectors_dir).expect("create vectors dir");

    let manifest = json!({
        "schema": "gemel.golden.v1",
        "generator": "gemel-object golden-gen",
        "generated_at": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "encver": 1,
        "vectors": entries.iter().map(|e| {
            json!({
                "name": e.name,
                "family": e.family,
                "schemever": e.schemever,
                "bytes": format!("vectors/{}.gce.hex", e.name),
                "id": format!("vectors/{}.id", e.name),
                "description": e.description,
                "references": e.references,
                "constructed_from": e.constructed_from,
            })
        }).collect::<Vec<_>>(),
    });

    for e in &entries {
        let hex_path = vectors_dir.join(format!("{}.gce.hex", e.name));
        let id_path = vectors_dir.join(format!("{}.id", e.name));
        std::fs::write(&hex_path, format!("{}\n", hex::encode(&e.bytes))).expect("write hex");
        std::fs::write(&id_path, format!("{}\n", e.id_text)).expect("write id");
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("manifest json");
    manifest_bytes.push(b'\n');
    std::fs::write(dir.join("manifest.json"), manifest_bytes).expect("write manifest");

    println!(
        "wrote {} golden vectors to {}",
        entries.len(),
        dir.display()
    );
}

struct GoldenEntry {
    name: &'static str,
    family: &'static str,
    schemever: u8,
    bytes: Vec<u8>,
    id_text: String,
    description: &'static str,
    references: serde_json::Map<String, serde_json::Value>,
    constructed_from: serde_json::Value,
}
