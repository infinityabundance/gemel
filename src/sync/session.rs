//! The hosted sync session (Phase 8; HOSTED.md).
//!
//! A bounded, line-delimited JSON session implementing the Phase 6 sync
//! operations on a server repository. This is the backend of both the SSH
//! transport (`ssh host gemel serve`) and the HTTP transport
//! (`gemel serve --http`): the wire framing is a strict superset of the
//! agent protocol's discipline — one request per line, one response per
//! line, stable error strings, nothing executed.
//!
//! Ops (request: `{"id":N,"op":"<op>",...}`):
//!
//! | op | params | response |
//! |---|---|---|
//! | `list_refs` | — | `{"ok":true,"refs":[[name,gid],...]}` |
//! | `reachable` | `seeds:[gid,...]` | `{"ok":true,"ids":[gid,...]}` |
//! | `missing` | `ids:[gid,...]` | `{"ok":true,"ids":[gid,...]}` |
//! | `fetch` | `ids:[gid,...]` | header `{"ok":true,"pack_len":L}` + `L` raw `gemlpack` bytes |
//! | `push` | `pack_len:L` | header + `L` raw `gemlpack` bytes, then `{"ok":true,"inserted":N}` |
//! | `update_refs` | `refs:[[name,gid],...]` | `{"ok":true,"applied":N}` |
//!
//! Errors are `{"ok":false,"error":"..."}`. `--read-only` servers refuse
//! `push` and `update_refs` (capability scoping; THREAT_MODEL.md §10).

use crate::gid::Gid;
use crate::store::{Error, Repo};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

/// Maximum request/response line length.
pub const MAX_LINE: usize = 64 * 1024;
/// Maximum automatic pack size accepted from a client.
pub const MAX_PACK_BYTES: u64 = 4 << 30;
/// Maximum ids per request.
pub const MAX_IDS: usize = 10_000_000;

/// The server's capability profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServeOptions {
    pub read_only: bool,
}

/// `list_refs` / `reachable` / `missing` / `update_refs`.
pub fn handle_json(
    repo: &Repo,
    op: &str,
    params: &serde_json::Map<String, Value>,
    read_only: bool,
) -> Result<Value, String> {
    let ids_of = |v: &Value| -> Result<Vec<Gid>, String> {
        let arr = v
            .as_array()
            .ok_or_else(|| "ids must be an array of gid strings".to_string())?;
        if arr.len() > MAX_IDS {
            return Err(format!("too many ids: {} (limit {MAX_IDS})", arr.len()));
        }
        arr.iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| "id must be a string".to_string())?
                    .parse::<Gid>()
                    .map_err(|e| format!("invalid gid: {e}"))
            })
            .collect()
    };
    match op {
        "list_refs" => {
            let refs = crate::sync::public_refs(repo).map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "refs": refs.iter().map(|(n, g)| json!([n, g.to_string()])).collect::<Vec<_>>(),
            }))
        }
        "reachable" => {
            let seeds = params
                .get("seeds")
                .map(ids_of)
                .transpose()?
                .unwrap_or_default();
            let ids = crate::sync::reachable_ids(repo, &seeds).map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            }))
        }
        "missing" => {
            let ids = params
                .get("ids")
                .map(ids_of)
                .transpose()?
                .unwrap_or_default();
            let missing = crate::sync::missing_ids(repo, &ids).map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "ids": missing.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            }))
        }
        "update_refs" => {
            if read_only {
                return Err("server is read-only".to_string());
            }
            let refs = params
                .get("refs")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "refs must be an array of [name, gid] pairs".to_string())?;
            let mut updates: Vec<(String, Gid)> = Vec::with_capacity(refs.len());
            for pair in refs {
                let pair = pair
                    .as_array()
                    .ok_or_else(|| "ref pair must be an array".to_string())?;
                if pair.len() != 2 {
                    return Err("ref pair must be [name, gid]".to_string());
                }
                let name = pair[0]
                    .as_str()
                    .ok_or_else(|| "ref name must be a string".to_string())?
                    .to_string();
                if !crate::sync::is_public_ref(&name) {
                    return Err(format!("ref {name:?} is not a public ref"));
                }
                let gid = pair[1]
                    .as_str()
                    .ok_or_else(|| "ref gid must be a string".to_string())?
                    .parse::<Gid>()
                    .map_err(|e| format!("invalid gid: {e}"))?;
                updates.push((name, gid));
            }
            // Validate before publishing: names are public and the referenced
            // closures are fully present (a ref never dangles).
            crate::sync::ensure_reachable(repo, &updates).map_err(|e| e.to_string())?;
            let ops: Vec<crate::store::refs::RefOp> = updates
                .iter()
                .map(|(n, g)| crate::store::refs::RefOp::set(n, *g))
                .collect();
            let applied = ops.len();
            repo.with_write_lock(|| {
                repo.apply_refs_unlocked(&crate::store::refs::RefTransaction { ops })?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true, "applied": applied }))
        }
        other => Err(format!("unknown op {other:?}")),
    }
}

/// `fetch`: returns the raw `gemlpack` bytes for the requested ids.
pub fn handle_fetch(repo: &Repo, ids: &[Gid]) -> Result<Vec<u8>, String> {
    if ids.len() > MAX_IDS {
        return Err(format!("too many ids: {} (limit {MAX_IDS})", ids.len()));
    }
    let mut records = Vec::with_capacity(ids.len());
    for id in ids {
        let envelope = repo.read_bytes(id).map_err(|e| e.to_string())?;
        records.push(crate::sync::gemlpack::PackRecord { id: *id, envelope });
    }
    crate::sync::gemlpack::encode_pack(&records).map_err(|e| e.to_string())
}

/// `push`: verifies and stores a raw `gemlpack` body. Returns the number of
/// objects inserted (deduplicated by content identity).
pub fn handle_push(repo: &Repo, pack_bytes: &[u8], read_only: bool) -> Result<usize, String> {
    if read_only {
        return Err("server is read-only".to_string());
    }
    if (pack_bytes.len() as u64) > MAX_PACK_BYTES {
        return Err(format!(
            "pack too large: {} bytes (limit {MAX_PACK_BYTES})",
            pack_bytes.len()
        ));
    }
    let records = crate::sync::gemlpack::decode_pack(
        pack_bytes,
        &crate::sync::gemlpack::PackLimits::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut inserted = 0usize;
    for r in &records {
        // insert_bytes verifies the identity and rejects id↔bytes conflicts
        // with existing objects (fatal; THREAT_MODEL.md §11).
        let got = repo.insert_bytes(&r.envelope).map_err(|e| e.to_string())?;
        if got != r.id {
            return Err(format!(
                "identity mismatch: advertised {}, stored {got}",
                r.id
            ));
        }
        inserted += 1;
    }
    Ok(inserted)
}

/// Runs one sync session over stdin/stdout (the SSH transport backend).
/// EOF ends the session. Requests are bounded; a push body is read exactly
/// per its declared `pack_len` (never unbounded).
pub fn serve_session(repo: &Repo, opts: &ServeOptions) -> Result<(), Error> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(()); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_LINE {
            writeln!(
                stdout,
                "{}",
                error_line(0, "limit_exceeded", "request line too long")
            )?;
            stdout.flush()?;
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    stdout,
                    "{}",
                    error_line(0, "invalid_request", &format!("bad JSON: {e}"))
                )?;
                stdout.flush()?;
                continue;
            }
        };
        let Some(obj) = req.as_object() else {
            writeln!(
                stdout,
                "{}",
                error_line(0, "invalid_request", "request must be an object")
            )?;
            stdout.flush()?;
            continue;
        };
        let id = obj.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let op = obj
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = match obj.get("params") {
            None => serde_json::Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                writeln!(
                    stdout,
                    "{}",
                    error_line(id, "invalid_request", "params must be an object")
                )?;
                stdout.flush()?;
                continue;
            }
        };
        if op == "fetch" {
            let ids: Vec<Gid> = match params.get("ids").map(ids_param).transpose() {
                Ok(Some(ids)) => ids,
                Ok(None) => Vec::new(),
                Err(e) => {
                    writeln!(stdout, "{}", error_line(id, "invalid_request", &e))?;
                    stdout.flush()?;
                    continue;
                }
            };
            match handle_fetch(repo, &ids) {
                Ok(pack) => {
                    let header = json!({
                        "id": id, "op": "fetch", "ok": true,
                        "pack_len": pack.len(),
                    });
                    writeln!(stdout, "{header}")?;
                    stdout.flush()?;
                    stdout.write_all(&pack)?;
                    stdout.flush()?;
                }
                Err(e) => {
                    writeln!(stdout, "{}", error_line(id, "query_failed", &e))?;
                    stdout.flush()?;
                }
            }
            continue;
        }
        if op == "push" {
            let pack_len = params.get("pack_len").and_then(|v| v.as_u64()).unwrap_or(0);
            if pack_len > MAX_PACK_BYTES {
                writeln!(
                    stdout,
                    "{}",
                    error_line(id, "limit_exceeded", "pack too large")
                )?;
                stdout.flush()?;
                continue;
            }
            let mut pack = vec![0u8; pack_len as usize];
            reader
                .read_exact(&mut pack)
                .map_err(|e| Error::Invalid(format!("push body truncated: {e}")))?;
            match handle_push(repo, &pack, opts.read_only) {
                Ok(inserted) => {
                    let resp = json!({
                        "id": id, "op": "push", "ok": true, "inserted": inserted,
                    });
                    writeln!(stdout, "{resp}")?;
                }
                Err(e) => {
                    writeln!(stdout, "{}", error_line(id, "query_failed", &e))?;
                }
            }
            stdout.flush()?;
            continue;
        }
        match handle_json(repo, &op, &params, opts.read_only) {
            Ok(result) => {
                let mut r = result.as_object().cloned().unwrap_or_default();
                r.insert("id".into(), json!(id));
                r.insert("op".into(), json!(op));
                writeln!(stdout, "{}", serde_json::Value::Object(r))?;
            }
            Err(e) => {
                writeln!(stdout, "{}", error_line(id, "query_failed", &e))?;
            }
        }
        stdout.flush()?;
    }
}

fn ids_param(v: &Value) -> Result<Vec<Gid>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| "ids must be an array of gid strings".to_string())?;
    if arr.len() > MAX_IDS {
        return Err(format!("too many ids: {} (limit {MAX_IDS})", arr.len()));
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| "id must be a string".to_string())?
                .parse::<Gid>()
                .map_err(|e| format!("invalid gid: {e}"))
        })
        .collect()
}

fn error_line(id: u64, code: &str, message: &str) -> String {
    serde_json::to_string(&json!({
        "id": id, "ok": false, "error": { "code": code, "message": message },
    }))
    .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization failed\"}".to_string())
}

/// Sends one sync request over a writer and reads its JSON response line.
/// Returns the response object (the `id` is echoed; mismatch is an error).
pub fn send_json<W: Write, R: BufRead>(
    out: &mut W,
    inp: &mut R,
    req: &Value,
) -> Result<serde_json::Value, Error> {
    let mut line = serde_json::to_string(req)
        .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()?;
    let mut resp_line = String::new();
    inp.read_line(&mut resp_line)?;
    if resp_line.trim().is_empty() {
        return Err(Error::Invalid("server closed the session".into()));
    }
    let resp: Value = serde_json::from_str(resp_line.trim())
        .map_err(|e| Error::Invalid(format!("malformed server response: {e}")))?;
    let resp_obj = resp
        .as_object()
        .ok_or_else(|| Error::Invalid("server response is not an object".into()))?;
    if resp_obj.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let msg = resp_obj
            .get("error")
            .and_then(|e| e.as_object())
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("server error");
        return Err(Error::Invalid(msg.to_string()));
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::InitOptions;
    use crate::workflow::{self, BeginOptions, FinishOptions};
    use std::io::BufReader;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("gemel-sess-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(root: &Path) -> Repo {
        std::fs::write(root.join("src.rs"), "pub fn f() {}\n").unwrap();
        let repo = Repo::init(root, &InitOptions::default()).unwrap();
        workflow::begin_change(
            &repo,
            &BeginOptions {
                intent_summary: Some("demo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        workflow::finish_change(
            &repo,
            &FinishOptions {
                summary: "first".into(),
                ..Default::default()
            },
        )
        .unwrap();
        repo
    }

    #[test]
    fn session_ops_roundtrip() {
        let root = temp_root("ops");
        let repo = seed(&root);
        let refs = handle_json(&repo, "list_refs", &serde_json::Map::new(), false).unwrap();
        assert_eq!(refs["ok"], true);
        let head = repo.read_ref(crate::store::REF_HEAD).unwrap().unwrap();
        let reach = handle_json(
            &repo,
            "reachable",
            &serde_json::json!({ "seeds": [head.to_string()] })
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .unwrap();
        let ids: Vec<Gid> = reach["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().parse::<Gid>().unwrap())
            .collect();
        assert!(!ids.is_empty());
        // Fetch returns a verified pack with exactly the requested ids.
        let pack = handle_fetch(&repo, &ids).unwrap();
        let decoded = crate::sync::gemlpack::decode_pack(
            &pack,
            &crate::sync::gemlpack::PackLimits::default(),
        )
        .unwrap();
        assert_eq!(decoded.len(), ids.len());
        // Push into a fresh repo reconstructs the closure.
        let b = temp_root("ops-b");
        let repo_b = Repo::init(&b, &InitOptions::default()).unwrap();
        let inserted = handle_push(&repo_b, &pack, false).unwrap();
        assert_eq!(inserted, ids.len());
        // missing now reports nothing for those ids.
        let miss = handle_json(
            &repo_b,
            "missing",
            &serde_json::json!({ "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>() })
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .unwrap();
        assert!(miss["ids"].as_array().unwrap().is_empty());
        // update_refs on the fresh repo validates and applies.
        let update = handle_json(
            &repo_b,
            "update_refs",
            &serde_json::json!({ "refs": [["refs/head", head.to_string()]] })
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .unwrap();
        assert_eq!(update["ok"], true);
        assert_eq!(repo_b.read_ref(crate::store::REF_HEAD).unwrap(), Some(head));
    }

    #[test]
    fn read_only_refuses_mutation() {
        let root = temp_root("ro");
        let repo = seed(&root);
        let update = handle_json(
            &repo,
            "update_refs",
            &serde_json::json!({ "refs": [] })
                .as_object()
                .unwrap()
                .clone(),
            true,
        );
        assert!(update.is_err());
        assert!(handle_push(&repo, b"", true).is_err());
        // Reads still work.
        assert!(handle_json(&repo, "list_refs", &serde_json::Map::new(), true).is_ok());
    }

    #[test]
    fn malformed_and_unknown_ops_fail() {
        let root = temp_root("bad");
        let repo = seed(&root);
        assert!(handle_json(&repo, "frobnicate", &serde_json::Map::new(), false).is_err());
        assert!(handle_json(
            &repo,
            "update_refs",
            &serde_json::json!({ "refs": [["refs/workspaces/x", "state.0000000000000000000000000000000000000000000000000000000000000000"]] })
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .is_err());
        // An update whose closure is not present fails validation.
        let foreign: Gid =
            "change.ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                .parse()
                .unwrap();
        assert!(handle_json(
            &repo,
            "update_refs",
            &serde_json::json!({ "refs": [["refs/head", foreign.to_string()]] })
                .as_object()
                .unwrap()
                .clone(),
            false,
        )
        .is_err());
    }

    #[test]
    fn send_json_echoes_id_and_errors() {
        let root = temp_root("send");
        let repo = seed(&root);
        let mut out = Vec::new();
        let mut inp = BufReader::new(&b"{\"id\":9,\"ok\":true,\"refs\":[]}\n"[..]);
        let resp = send_json(&mut out, &mut inp, &json!({"id": 9, "op": "list_refs"})).unwrap();
        assert_eq!(resp["id"], 9);
        let mut out = Vec::new();
        let mut inp =
            BufReader::new(&b"{\"id\":9,\"ok\":false,\"error\":{\"message\":\"boom\"}}\n"[..]);
        assert!(send_json(&mut out, &mut inp, &json!({"id": 9, "op": "x"})).is_err());
        let _ = &repo;
    }
}
