//! Git-carried exchange rollups (SPECIFICATION.md Phase 1.5, EXCHANGE.md).
//!
//! Ordinary Git transports bytes; Gemel restores meaning. This module
//! implements the deterministic exchange projection: immutable content-
//! addressed packs (`gemel.exchange.pack.v1`), immutable Frontier
//! Descriptors (`gemel.exchange.frontier.v1`), append-only export, and
//! quarantine ingestion with source-state binding.
//!
//! Native objects are authoritative; exchange artifacts are projections.
//! Changing `.gemel/exchange/**` never changes the canonical source State
//! (EXCHANGE.md §10).

pub mod export;
pub mod ingest;

use crate::decode::decode_object;
use crate::gid::Gid;
use crate::hash::object_id_bytes;
use crate::limits::Limits;
use crate::store::Error;
use crate::value::Object;
use std::path::Path;

/// The exchange protocol version namespace (EXCHANGE.md §2, §15).
pub const PROTOCOL_VERSION: &str = "v1";
/// The exchange root relative to the metadata dir.
pub const EXCHANGE_ROOT: &str = "exchange";
/// Frontier descriptor schema.
pub const FRONTIER_SCHEMA: &str = "gemel.exchange.frontier.v1";
/// Pack schema identifier.
pub const PACK_SCHEMA: &str = "gemel.exchange.pack.v1";
/// The deterministic pack size target (protocol constant, EXCHANGE.md §7).
pub const TARGET_PACK_BYTES: u64 = 262_144;

/// Resource limits for exchange ingestion (EXCHANGE.md §11, THREAT_MODEL.md §5).
#[derive(Debug, Clone, Copy)]
pub struct ExchangeLimits {
    pub max_descriptor_bytes: u64,
    pub max_pack_bytes: u64,
    pub max_objects_per_pack: u64,
    pub max_packs_per_frontier: usize,
    pub max_automatic_ingest_bytes: u64,
    pub max_reference_depth: usize,
}

impl Default for ExchangeLimits {
    fn default() -> Self {
        ExchangeLimits {
            max_descriptor_bytes: 1024 * 1024,
            max_pack_bytes: 64 * 1024 * 1024,
            max_objects_per_pack: 1_000_000,
            max_packs_per_frontier: 4096,
            max_automatic_ingest_bytes: 512 * 1024 * 1024,
            max_reference_depth: 4096,
        }
    }
}

/// The exchange directory: `.gemel/exchange/v1`.
pub fn exchange_dir(meta: &Path) -> std::path::PathBuf {
    meta.join(EXCHANGE_ROOT).join(PROTOCOL_VERSION)
}

/// The packs directory.
pub fn pack_dir(meta: &Path) -> std::path::PathBuf {
    exchange_dir(meta).join("packs")
}

/// The frontiers directory.
pub fn frontier_dir(meta: &Path) -> std::path::PathBuf {
    exchange_dir(meta).join("frontiers")
}

/// The content-addressed path of a pack (EXCHANGE.md §6).
pub fn pack_path(meta: &Path, id: &[u8; 32]) -> std::path::PathBuf {
    let hex = crate::hex::encode(id);
    pack_dir(meta)
        .join(&hex[0..2])
        .join(format!("{}.gxp", &hex[2..]))
}

/// The content-addressed path of a frontier descriptor (EXCHANGE.md §8).
pub fn frontier_path(meta: &Path, id: &[u8; 32]) -> std::path::PathBuf {
    let hex = crate::hex::encode(id);
    frontier_dir(meta)
        .join(&hex[0..2])
        .join(format!("{}.gxf", &hex[2..]))
}

/// Reads a pack file with path-safety checks (EXCHANGE.md §39): only regular
/// files are accepted; symlinks and special files are rejected, never
/// followed.
pub fn read_pack_file(path: &Path) -> Result<Vec<u8>, Error> {
    let md = std::fs::symlink_metadata(path)?;
    if !md.file_type().is_file() {
        return Err(Error::Invalid(format!(
            "exchange pack is not a regular file: {}",
            path.display()
        )));
    }
    std::fs::read(path).map_err(|e| e.into())
}

/// Reads a frontier descriptor file with the same path-safety checks.
pub fn read_frontier_file(path: &Path) -> Result<Vec<u8>, Error> {
    let md = std::fs::symlink_metadata(path)?;
    if !md.file_type().is_file() {
        return Err(Error::Invalid(format!(
            "exchange frontier is not a regular file: {}",
            path.display()
        )));
    }
    std::fs::read(path).map_err(|e| e.into())
}

// ---------------------------------------------------------------------------
// Pack format (EXCHANGE.md §6)
// ---------------------------------------------------------------------------

const PACK_MAGIC: &[u8; 4] = b"GXPK";
const PACK_VERSION: u8 = 1;
const PACK_TRAILER: &[u8; 8] = b"GXPK-END";

/// One object inside a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackObject {
    pub id: Gid,
    pub envelope: Vec<u8>,
}

/// Encodes a pack (objects must be sorted ascending by id; duplicates are a
/// protocol violation). Returns (bytes, PackId).
pub fn encode_pack(objects: &[PackObject]) -> Result<(Vec<u8>, [u8; 32]), Error> {
    let mut prev: Option<Gid> = None;
    for o in objects {
        if let Some(p) = prev {
            if o.id <= p {
                return Err(Error::Invalid(
                    "pack objects must be strictly ascending by id".into(),
                ));
            }
        }
        prev = Some(o.id);
        if o.envelope.len() as u64 > u64::from(u32::MAX) {
            return Err(Error::Limit {
                kind: "exchange object length",
                limit: u64::from(u32::MAX),
                found: o.envelope.len() as u64,
            });
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(PACK_MAGIC);
    out.push(PACK_VERSION);
    out.extend_from_slice(&(objects.len() as u64).to_le_bytes());
    for o in objects {
        out.push(o.id.family().code());
        out.extend_from_slice(o.id.digest());
        out.extend_from_slice(&(o.envelope.len() as u64).to_le_bytes());
        out.extend_from_slice(&o.envelope);
    }
    out.extend_from_slice(PACK_TRAILER);
    let id = crate::hash::blake3_256(&out);
    Ok((out, id))
}

/// Decodes and fully validates a pack (EXCHANGE.md §6, §11). Every advertised
/// id must match the envelope bytes; every envelope must decode under the
/// supported schemas; limits apply. Returns the objects in id order.
pub fn decode_pack(bytes: &[u8], limits: &ExchangeLimits) -> Result<Vec<PackObject>, Error> {
    if bytes.len() as u64 > limits.max_pack_bytes {
        return Err(Error::Limit {
            kind: "exchange pack",
            limit: limits.max_pack_bytes,
            found: bytes.len() as u64,
        });
    }
    if bytes.len() < PACK_MAGIC.len() + 1 + 8 + PACK_TRAILER.len() {
        return Err(Error::Invalid("pack too short".into()));
    }
    if &bytes[0..4] != PACK_MAGIC {
        return Err(Error::Invalid("pack magic mismatch".into()));
    }
    if bytes[4] != PACK_VERSION {
        return Err(Error::Invalid(format!(
            "unsupported pack version {}",
            bytes[4]
        )));
    }
    let count = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
    if count > limits.max_objects_per_pack {
        return Err(Error::Limit {
            kind: "exchange objects per pack",
            limit: limits.max_objects_per_pack,
            found: count,
        });
    }
    if bytes.len() < 13 + PACK_TRAILER.len() {
        return Err(Error::Invalid("pack truncated".into()));
    }
    let body_end = bytes.len() - PACK_TRAILER.len();
    if &bytes[body_end..] != PACK_TRAILER {
        return Err(Error::Invalid("pack trailer mismatch".into()));
    }
    let mut cursor = 13usize;
    let mut out = Vec::with_capacity(count as usize);
    let mut prev: Option<Gid> = None;
    for _ in 0..count {
        if cursor + 33 + 8 > body_end {
            return Err(Error::Invalid("pack object header truncated".into()));
        }
        let family = crate::family::Family::from_code(bytes[cursor]).ok_or_else(|| {
            Error::Invalid(format!("pack object family {} unknown", bytes[cursor]))
        })?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[cursor + 1..cursor + 33]);
        let id = Gid::new(family, digest);
        if let Some(p) = prev {
            if id <= p {
                return Err(Error::Invalid(
                    "pack objects out of order or duplicate".into(),
                ));
            }
        }
        prev = Some(id);
        let len = u64::from_le_bytes(bytes[cursor + 33..cursor + 41].try_into().unwrap());
        cursor += 41;
        // Bounded arithmetic: a hostile length field must never overflow.
        let end = match cursor.checked_add(len as usize) {
            Some(e) => e,
            None => return Err(Error::Invalid("pack object length overflow".into())),
        };
        if end > body_end {
            return Err(Error::Invalid("pack object body truncated".into()));
        }
        let envelope = bytes[cursor..end].to_vec();
        cursor = end;
        // Advertised id must equal BLAKE3(envelope) (family + digest).
        let actual_digest = object_id_bytes(&envelope);
        if actual_digest != digest {
            return Err(Error::Invalid(format!(
                "pack object id/body mismatch for {id}"
            )));
        }
        let parsed: Object = decode_object(&envelope, &object_limits())?;
        if parsed.family != family {
            return Err(Error::Invalid(format!(
                "pack object envelope family mismatch for {id}"
            )));
        }
        out.push(PackObject { id, envelope });
    }
    if cursor != body_end {
        return Err(Error::Invalid("pack has trailing bytes".into()));
    }
    Ok(out)
}

/// The canonical object limits used for pack validation (repository default).
fn object_limits() -> Limits {
    Limits::default()
}

/// The identity of a pack from its exact bytes.
pub fn pack_id(bytes: &[u8]) -> [u8; 32] {
    crate::hash::blake3_256(bytes)
}

/// The identity of a frontier descriptor from its exact bytes.
pub fn frontier_id(bytes: &[u8]) -> [u8; 32] {
    crate::hash::blake3_256(bytes)
}

// ---------------------------------------------------------------------------
// Frontier descriptor (EXCHANGE.md §8)
// ---------------------------------------------------------------------------

/// The coverage vocabulary (EXCHANGE.md §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub canonical_metadata: String,
    pub source_content: String,
    pub evidence_receipts: String,
    pub evidence_payloads: String,
    pub conversations: String,
    pub forensic_traces: String,
}

impl Default for Coverage {
    fn default() -> Self {
        Coverage {
            canonical_metadata: "complete".into(),
            source_content: "carrier-backed".into(),
            evidence_receipts: "complete".into(),
            evidence_payloads: "partial".into(),
            conversations: "omitted".into(),
            forensic_traces: "omitted".into(),
        }
    }
}

/// A parsed Frontier Descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontier {
    pub schema: String,
    pub source_state: Gid,
    pub head_change: Gid,
    pub trajectory: Option<Gid>,
    pub intent: Option<Gid>,
    pub parent_frontiers: Vec<String>,
    pub packs: Vec<String>,
    pub profile: String,
    pub coverage: Coverage,
    pub required_schemas: Vec<u8>,
}

/// Canonically encodes a frontier descriptor: JSON with sorted keys (UTF-8),
/// no timestamps, no hostnames, no git commit ids (EXCHANGE.md §8).
pub fn encode_frontier(f: &Frontier) -> Result<Vec<u8>, Error> {
    let value = serde_json::json!({
        "schema": f.schema,
        "source_state": f.source_state.to_string(),
        "head_change": f.head_change.to_string(),
        "trajectory": f.trajectory.map(|g| g.to_string()),
        "intent": f.intent.map(|g| g.to_string()),
        "parent_frontiers": f.parent_frontiers,
        "packs": f.packs,
        "profile": f.profile,
        "coverage": {
            "canonical_metadata": f.coverage.canonical_metadata,
            "source_content": f.coverage.source_content,
            "evidence_receipts": f.coverage.evidence_receipts,
            "evidence_payloads": f.coverage.evidence_payloads,
            "conversations": f.coverage.conversations,
            "forensic_traces": f.coverage.forensic_traces,
        },
        "required_schemas": f.required_schemas,
    });
    let mut bytes = serde_json::to_vec(&value).map_err(|e| Error::Invalid(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parses and validates a frontier descriptor (fail closed; unknown mandatory
/// fields rejected, unknown optional fields preserved is not required for the
/// canonical subset — the descriptor is canonical JSON, so the parsed fields
/// are authoritative).
pub fn parse_frontier(bytes: &[u8], limits: &ExchangeLimits) -> Result<Frontier, Error> {
    if bytes.len() as u64 > limits.max_descriptor_bytes {
        return Err(Error::Limit {
            kind: "exchange descriptor",
            limit: limits.max_descriptor_bytes,
            found: bytes.len() as u64,
        });
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Invalid(format!("frontier: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| Error::Invalid("frontier is not an object".into()))?;
    let schema = obj
        .get("schema")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Invalid("frontier missing schema".into()))?;
    if schema != FRONTIER_SCHEMA {
        return Err(Error::Invalid(format!(
            "unsupported frontier schema {schema:?}"
        )));
    }
    let parse_gid = |k: &str| -> Result<Gid, Error> {
        obj.get(k)
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Invalid(format!("frontier missing {k}")))?
            .parse::<Gid>()
            .map_err(|e| Error::Invalid(e.to_string()))
    };
    let packs = obj
        .get("packs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Invalid("frontier missing packs".into()))?;
    if packs.len() > limits.max_packs_per_frontier {
        return Err(Error::Limit {
            kind: "exchange packs per frontier",
            limit: limits.max_packs_per_frontier as u64,
            found: packs.len() as u64,
        });
    }
    let pack_list: Vec<String> = packs
        .iter()
        .map(|p| {
            p.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| Error::Invalid("pack id not a string".into()))
        })
        .collect::<Result<_, _>>()?;
    for p in &pack_list {
        if p.len() != 64 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid(format!("malformed pack id {p:?}")));
        }
    }
    let parents: Vec<String> = obj
        .get("parent_frontiers")
        .map(|v| {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            x.as_str().map(|s| s.to_string()).ok_or_else(|| {
                                Error::Invalid("parent frontier id not a string".into())
                            })
                        })
                        .collect::<Result<_, _>>()
                })
                .unwrap_or_else(|| Err(Error::Invalid("parent_frontiers not an array".into())))
        })
        .unwrap_or_else(|| Ok(Vec::new()))?;
    let cov = |k: &str, def: &str| -> String {
        obj.get("coverage")
            .and_then(|c| c.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or(def)
            .to_string()
    };
    let required: Vec<u8> = obj
        .get("required_schemas")
        .map(|v| {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u8))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Ok(Frontier {
        schema: schema.to_string(),
        source_state: parse_gid("source_state")?,
        head_change: parse_gid("head_change")?,
        trajectory: obj
            .get("trajectory")
            .and_then(|v| v.as_str())
            .map(|s| s.parse::<Gid>())
            .transpose()
            .map_err(|e| Error::Invalid(e.to_string()))?,
        intent: obj
            .get("intent")
            .and_then(|v| v.as_str())
            .map(|s| s.parse::<Gid>())
            .transpose()
            .map_err(|e| Error::Invalid(e.to_string()))?,
        parent_frontiers: parents,
        packs: pack_list,
        profile: obj
            .get("profile")
            .and_then(|v| v.as_str())
            .unwrap_or("frontier")
            .to_string(),
        coverage: Coverage {
            canonical_metadata: cov("canonical_metadata", "complete"),
            source_content: cov("source_content", "carrier-backed"),
            evidence_receipts: cov("evidence_receipts", "complete"),
            evidence_payloads: cov("evidence_payloads", "partial"),
            conversations: cov("conversations", "omitted"),
            forensic_traces: cov("forensic_traces", "omitted"),
        },
        required_schemas: required,
    })
}

/// One discovered frontier on disk: (parsed frontier, id, exact bytes).
pub type DiscoveredFrontier = (Frontier, [u8; 32], Vec<u8>);

/// Enumerates valid frontier descriptors on disk, verifying each identity.
/// Returns `(frontier, id, bytes)` sorted by id.
pub fn discover_frontiers(meta: &Path) -> Result<Vec<DiscoveredFrontier>, Error> {
    let limits = ExchangeLimits::default();
    let dir = frontier_dir(meta);
    let mut out = Vec::new();
    let shards = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for shard in shards.flatten() {
        if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let prefix = shard.file_name().to_string_lossy().to_string();
        if prefix.len() != 2 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let entries = match std::fs::read_dir(shard.path()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            // Content-addressed name: <62 hex>.gxf (66 characters total).
            if !fname.ends_with(".gxf") || fname.len() != 66 {
                continue;
            }
            // Path safety (EXCHANGE.md §39): only regular files are accepted;
            // symlinks and special files are ignored, never followed.
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let hex = format!("{prefix}{}", &fname[..62]);
            let id = match hex_to_digest(&hex) {
                Some(d) => d,
                None => continue,
            };
            let bytes = match std::fs::read(entry.path()) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if frontier_id(&bytes) != id {
                continue; // filename↔content mismatch: not a valid frontier
            }
            if let Ok(f) = parse_frontier(&bytes, &limits) {
                out.push((f, id, bytes));
            }
        }
    }
    out.sort_by_key(|a| a.1);
    Ok(out)
}

/// Converts a 64-hex digest string to bytes.
pub fn hex_to_digest(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}
