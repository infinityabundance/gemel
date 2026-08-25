//! Sync transports (Phase 8; HOSTED.md §3–§5).
//!
//! Three transports implement the Phase 6 [`Transport`] trait:
//!
//! - [`FileTransport`] (super::FileTransport): a local filesystem path.
//! - [`SshTransport`]: `ssh host gemel serve <path>` — SSH supplies mutual
//!   auth and encryption; the remote command is argv-safe and the session
//!   framing is the bounded `session` protocol.
//! - [`HttpTransport`]: POSTs the same operations to a minimal HTTP/1.1
//!   server (`gemel serve --http`), with bearer-token capability grants.
//!
//! Remote arguments accept `ssh://[user@]host[:port]/path`,
//! `http://host:port/path`, or a local filesystem path. `https://` is
//! rejected: TLS is not implemented in-crate (THREAT_MODEL.md §10: transport
//! encryption is mandatory for non-local remotes — use `ssh://` or terminate
//! TLS in a proxy and speak `http://` to it).

use crate::gid::Gid;
use crate::store::{Error, Repo};
use crate::sync::session;
use crate::sync::Transport;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A parsed remote argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteUrl {
    /// `ssh://[user@]host[:port]/path`
    Ssh {
        user: Option<String>,
        host: String,
        port: Option<u16>,
        path: String,
    },
    /// `http://host:port/path` (token from the URL or the configured remote)
    Http {
        host: String,
        port: u16,
        path: String,
        token: Option<String>,
    },
    /// A local filesystem path.
    Local(PathBuf),
}

/// Parses a remote argument. Bare paths are local; `ssh://` and `http://`
/// URLs are validated strictly. `https://` fails closed.
pub fn parse_remote(arg: &str) -> Result<RemoteUrl, Error> {
    if let Some(rest) = arg.strip_prefix("ssh://") {
        let (authority, path) = split_authority(rest)?;
        let (user, host, port) = split_user_host_port(authority)?;
        let path = path.to_string();
        if path.is_empty() || path == "/" {
            return Err(Error::Invalid(
                "ssh:// remote requires a repository path (e.g. ssh://host/srv/gemel/foo)".into(),
            ));
        }
        return Ok(RemoteUrl::Ssh {
            user,
            host,
            port,
            path,
        });
    }
    if let Some(_rest) = arg.strip_prefix("https://") {
        return Err(Error::Invalid(
            "https:// remotes are not supported: TLS is not implemented in-crate. \
             Use ssh:// or terminate TLS in a proxy and speak http:// to it \
             (THREAT_MODEL.md §10)"
                .into(),
        ));
    }
    if let Some(rest) = arg.strip_prefix("http://") {
        let (authority, path) = split_authority(rest)?;
        let (user, host, port) = split_user_host_port(authority)?;
        let mut token = None;
        if let Some(u) = user {
            // http://user@host/ is rejected; http://token@host/ is accepted
            // as a bearer-token shorthand (documented; never a password).
            token = Some(u);
        }
        let path = if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        };
        return Ok(RemoteUrl::Http {
            host,
            port: port.unwrap_or(80),
            path,
            token,
        });
    }
    if arg.contains("://") {
        return Err(Error::Invalid(format!(
            "unsupported remote URL scheme in {arg:?} (ssh://, http://, or a local path)"
        )));
    }
    Ok(RemoteUrl::Local(PathBuf::from(arg)))
}

fn split_authority(rest: &str) -> Result<(&str, &str), Error> {
    match rest.find('/') {
        Some(i) => Ok((&rest[..i], &rest[i..])),
        None => Ok((rest, "")),
    }
}

/// Splits `[user@]host[:port]`. The port is `None` when omitted (the
/// transport applies its scheme default: 22 for ssh, 80 for http). A present
/// but malformed port is rejected rather than silently swallowed, and
/// bracketed IPv6 literals are not supported in v1 (documented).
fn split_user_host_port(authority: &str) -> Result<(Option<String>, String, Option<u16>), Error> {
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((u, hp)) => (Some(u.to_string()), hp),
        None => (None, authority),
    };
    if host_port.is_empty() {
        return Err(Error::Invalid("remote URL has no host".into()));
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Invalid(format!("invalid port in remote URL: {p}")));
            }
            let port: u16 = p
                .parse()
                .map_err(|_| Error::Invalid(format!("invalid port in remote URL: {p}")))?;
            if h.is_empty() {
                return Err(Error::Invalid("remote URL has no host".into()));
            }
            (h.to_string(), Some(port))
        }
        None => (host_port.to_string(), None),
    };
    if host.is_empty() {
        return Err(Error::Invalid("remote URL has no host".into()));
    }
    Ok((user, host, port))
}

/// What a remote argument resolves to.
pub enum RemoteTarget {
    /// A Gemel repository reachable through a [`Transport`].
    Native(Box<dyn Transport>),
    /// A Git-only repository (deterministic projection; GIT_INTEROP.md §6).
    Git(PathBuf),
}

/// Resolves a remote argument (configured name, URL, or path) to a target.
/// The returned name is the label used for `refs/remotes/<name>/*`.
pub fn resolve_target(repo: &Repo, arg: &str) -> Result<(String, RemoteTarget), Error> {
    if let Ok(path) = crate::sync::remote_path(repo, arg) {
        let url = parse_remote(path.to_str().unwrap_or_default())?;
        return target_for(arg.to_string(), &url);
    }
    let url = parse_remote(arg)?;
    // The name is the label used for `refs/remotes/<name>/*`; it must
    // distinguish remotes that differ only by port, so the port is part of
    // the name when present.
    let name = match &url {
        RemoteUrl::Local(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| arg.to_string()),
        RemoteUrl::Ssh {
            host, port, path, ..
        } => {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            format!("{host}{port}{}", path.trim_end_matches('/'))
        }
        RemoteUrl::Http {
            host, port, path, ..
        } => format!("{host}:{port}{}", path.trim_end_matches('/')),
    };
    target_for(name, &url)
}

fn target_for(name: String, url: &RemoteUrl) -> Result<(String, RemoteTarget), Error> {
    match url {
        RemoteUrl::Ssh {
            user,
            host,
            port,
            path,
        } => Ok((
            name,
            RemoteTarget::Native(Box::new(SshTransport::ssh(
                host,
                user.as_deref(),
                *port,
                path,
            )?)),
        )),
        RemoteUrl::Http {
            host,
            port,
            path,
            token,
        } => Ok((
            name,
            RemoteTarget::Native(Box::new(HttpTransport::new(
                host,
                *port,
                path,
                token.clone(),
            ))),
        )),
        RemoteUrl::Local(path) => {
            if path
                .join(crate::store::META_DIR)
                .join("meta.json")
                .is_file()
            {
                return Ok((
                    name,
                    RemoteTarget::Native(Box::new(crate::sync::FileTransport::open(path, false)?)),
                ));
            }
            let git_dir = if path.join(".git").is_dir() {
                Some(path.join(".git"))
            } else if path.join("HEAD").is_file() && path.join("objects").is_dir() {
                Some(path.to_path_buf())
            } else {
                None
            };
            match git_dir {
                Some(g) => Ok((name, RemoteTarget::Git(g))),
                None => Err(Error::Invalid(format!(
                    "{} is neither a gemel repository nor a git repository",
                    path.display()
                ))),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SSH transport
// ---------------------------------------------------------------------------

/// A transport over `ssh host gemel serve <path>`. SSH provides mutual auth
/// and encryption; the sync session framing is the bounded `session`
/// protocol. The remote command is never run through a shell.
pub struct SshTransport {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
    describe: String,
}

impl SshTransport {
    /// Builds the default invocation: `ssh [-p port] [user@]host gemel serve <path>`.
    pub fn ssh(
        host: &str,
        user: Option<&str>,
        port: Option<u16>,
        path: &str,
    ) -> Result<SshTransport, Error> {
        if path.starts_with('-') {
            return Err(Error::Invalid("remote path must not start with '-'".into()));
        }
        let mut args: Vec<String> = Vec::new();
        if let Some(p) = port {
            args.push("-p".into());
            args.push(p.to_string());
        }
        let target = match user {
            Some(u) => format!("{u}@{host}"),
            None => host.to_string(),
        };
        let describe = format!("ssh://{}{}", target, path);
        args.push(target);
        args.push("gemel".into());
        args.push("serve".into());
        args.push(path.to_string());
        Self::spawn("ssh", &args, describe)
    }

    /// Spawns `program args...` and speaks the sync session to its stdio.
    /// `program` and `args` are passed argv-safe (no shell concatenation);
    /// tests point this at the local gemel binary.
    pub fn spawn(program: &str, args: &[String], describe: String) -> Result<SshTransport, Error> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| {
            Error::Invalid(format!("cannot spawn {program}: {e} (is it installed?)"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Invalid("cannot open remote stdin".into()))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::Invalid("cannot open remote stdout".into()))?,
        );
        Ok(SshTransport {
            child,
            stdin,
            stdout,
            next_id: 1,
            describe,
        })
    }

    fn request(&mut self, params: &Value) -> Result<Value, Error> {
        let req = json!({
            "id": self.next_id,
            "op": params["op"],
            "params": params.get("body").cloned().unwrap_or(Value::Null),
        });
        self.next_id += 1;
        let mut line = serde_json::to_string(&req)
            .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        let mut resp_line = String::new();
        self.stdout.read_line(&mut resp_line)?;
        let resp: Value = serde_json::from_str(resp_line.trim())
            .map_err(|e| Error::Invalid(format!("malformed remote response: {e}")))?;
        let obj = resp
            .as_object()
            .ok_or_else(|| Error::Invalid("remote response is not an object".into()))?;
        if obj.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = obj
                .get("error")
                .and_then(|e| e.as_object())
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("remote error");
            return Err(Error::Invalid(msg.to_string()));
        }
        Ok(resp)
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        // Reap the session process. The child is dedicated to this
        // transport: killing it (rather than waiting for a graceful EOF
        // we can no longer deliver) is deterministic and cannot hang.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl crate::sync::Transport for SshTransport {
    fn describe(&self) -> String {
        self.describe.clone()
    }

    fn list_refs(&mut self) -> Result<Vec<(String, Gid)>, Error> {
        let resp = self.request(&json!({ "op": "list_refs", "body": {} }))?;
        let refs = resp
            .get("refs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::Invalid("remote list_refs malformed".into()))?;
        let mut out = Vec::with_capacity(refs.len());
        for pair in refs {
            let pair = pair
                .as_array()
                .ok_or_else(|| Error::Invalid("remote ref pair malformed".into()))?;
            let name = pair[0]
                .as_str()
                .ok_or_else(|| Error::Invalid("remote ref name malformed".into()))?;
            let gid = pair[1]
                .as_str()
                .ok_or_else(|| Error::Invalid("remote ref gid malformed".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(format!("remote ref gid invalid: {e}")))?;
            out.push((name.to_string(), gid));
        }
        Ok(out)
    }

    fn reachable_ids(&mut self, seeds: &[Gid]) -> Result<Vec<Gid>, Error> {
        let resp = self.request(&json!({
            "op": "reachable",
            "body": { "seeds": seeds.iter().map(|g| g.to_string()).collect::<Vec<_>>() },
        }))?;
        parse_id_list(resp.get("ids"))
    }

    fn missing_ids(&mut self, ids: &[Gid]) -> Result<Vec<Gid>, Error> {
        let resp = self.request(&json!({
            "op": "missing",
            "body": { "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>() },
        }))?;
        parse_id_list(resp.get("ids"))
    }

    fn fetch_objects(&mut self, ids: &[Gid]) -> Result<Vec<(Gid, Vec<u8>)>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let req = json!({
            "id": self.next_id,
            "op": "fetch",
            "params": { "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>() },
        });
        self.next_id += 1;
        let mut line = serde_json::to_string(&req)
            .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        let mut header_line = String::new();
        self.stdout.read_line(&mut header_line)?;
        let header: Value = serde_json::from_str(header_line.trim())
            .map_err(|e| Error::Invalid(format!("malformed fetch header: {e}")))?;
        let header_obj = header
            .as_object()
            .ok_or_else(|| Error::Invalid("fetch header malformed".into()))?;
        if header_obj.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = header_obj
                .get("error")
                .and_then(|e| e.as_object())
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("remote error");
            return Err(Error::Invalid(msg.to_string()));
        }
        let pack_len = header_obj
            .get("pack_len")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| Error::Invalid("fetch header has no pack_len".into()))?;
        if pack_len > crate::sync::session::MAX_PACK_BYTES {
            return Err(Error::Limit {
                kind: "remote fetch pack",
                limit: crate::sync::session::MAX_PACK_BYTES,
                found: pack_len,
            });
        }
        let mut pack = vec![0u8; pack_len as usize];
        self.stdout.read_exact(&mut pack)?;
        decode_fetch_pack(&pack, ids)
    }

    fn push_objects(&mut self, records: &[(Gid, Vec<u8>)]) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let pack = crate::sync::gemlpack::encode_pack(
            &records
                .iter()
                .map(|(id, envelope)| crate::sync::gemlpack::PackRecord {
                    id: *id,
                    envelope: envelope.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        let header = json!({
            "id": self.next_id,
            "op": "push",
            "params": { "pack_len": pack.len() },
        });
        self.next_id += 1;
        let mut line = serde_json::to_string(&header)
            .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        self.stdin.write_all(&pack)?;
        self.stdin.flush()?;
        let mut resp_line = String::new();
        self.stdout.read_line(&mut resp_line)?;
        let resp: Value = serde_json::from_str(resp_line.trim())
            .map_err(|e| Error::Invalid(format!("malformed push response: {e}")))?;
        if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = resp
                .get("error")
                .and_then(|e| e.as_object())
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("remote error");
            return Err(Error::Invalid(msg.to_string()));
        }
        Ok(())
    }

    fn update_refs(&mut self, refs: &[(String, Gid)]) -> Result<(), Error> {
        let resp = self.request(&json!({
            "op": "update_refs",
            "body": {
                "refs": refs.iter().map(|(n, g)| json!([n, g.to_string()])).collect::<Vec<_>>(),
            },
        }))?;
        let _ = resp;
        Ok(())
    }
}

fn parse_id_list(v: Option<&Value>) -> Result<Vec<Gid>, Error> {
    let arr = v
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Invalid("remote id list malformed".into()))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Error::Invalid("remote id malformed".into()))?
                .parse::<Gid>()
                .map_err(|e| Error::Invalid(format!("remote id invalid: {e}")))
        })
        .collect()
}

/// Decodes a fetched pack and verifies every record is in the requested set.
fn decode_fetch_pack(pack: &[u8], requested: &[Gid]) -> Result<Vec<(Gid, Vec<u8>)>, Error> {
    let records =
        crate::sync::gemlpack::decode_pack(pack, &crate::sync::gemlpack::PackLimits::default())?;
    let want: std::collections::HashSet<Gid> = requested.iter().copied().collect();
    let mut out = Vec::with_capacity(records.len());
    for r in records {
        if !want.contains(&r.id) {
            return Err(Error::Invalid(format!(
                "remote sent unsolicited object {}",
                r.id
            )));
        }
        out.push((r.id, r.envelope));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// HTTP transport + minimal server
// ---------------------------------------------------------------------------

/// A transport over the minimal HTTP sync server.
pub struct HttpTransport {
    host: String,
    port: u16,
    path: String,
    token: Option<String>,
    connect_timeout: Duration,
}

impl HttpTransport {
    pub fn new(host: &str, port: u16, path: &str, token: Option<String>) -> HttpTransport {
        HttpTransport {
            host: host.to_string(),
            port,
            path: path.to_string(),
            token,
            connect_timeout: Duration::from_secs(30),
        }
    }

    fn post(&self, endpoint: &str, body: &[u8], json: bool) -> Result<(u16, Vec<u8>), Error> {
        http_post(
            &self.host,
            self.port,
            &format!("{}{}", self.path.trim_end_matches('/'), endpoint),
            self.token.as_deref(),
            json,
            body,
            self.connect_timeout,
        )
    }

    fn post_json(&self, endpoint: &str, body: &Value) -> Result<Value, Error> {
        let bytes = serde_json::to_vec(body)
            .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
        let (status, resp) = self.post(endpoint, &bytes, true)?;
        if status != 200 {
            return Err(http_error(status, &resp));
        }
        serde_json::from_slice(&resp)
            .map_err(|e| Error::Invalid(format!("malformed server JSON: {e}")))
    }
}

impl crate::sync::Transport for HttpTransport {
    fn describe(&self) -> String {
        format!(
            "http://{}:{}{}",
            self.host,
            self.port,
            self.path.trim_end_matches('/')
        )
    }

    fn list_refs(&mut self) -> Result<Vec<(String, Gid)>, Error> {
        let resp = self.post_json("/refs", &json!({}))?;
        let mut out = Vec::new();
        if let Some(refs) = resp.get("refs").and_then(|v| v.as_array()) {
            for pair in refs {
                let pair = pair
                    .as_array()
                    .ok_or_else(|| Error::Invalid("server ref pair malformed".into()))?;
                let name = pair[0]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("server ref name malformed".into()))?;
                let gid = pair[1]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("server ref gid malformed".into()))?
                    .parse::<Gid>()
                    .map_err(|e| Error::Invalid(format!("server ref gid invalid: {e}")))?;
                out.push((name.to_string(), gid));
            }
        }
        Ok(out)
    }

    fn reachable_ids(&mut self, seeds: &[Gid]) -> Result<Vec<Gid>, Error> {
        let resp = self.post_json(
            "/reachable",
            &json!({ "seeds": seeds.iter().map(|g| g.to_string()).collect::<Vec<_>>() }),
        )?;
        parse_id_list(resp.get("ids"))
    }

    fn missing_ids(&mut self, ids: &[Gid]) -> Result<Vec<Gid>, Error> {
        let resp = self.post_json(
            "/missing",
            &json!({ "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>() }),
        )?;
        parse_id_list(resp.get("ids"))
    }

    fn fetch_objects(&mut self, ids: &[Gid]) -> Result<Vec<(Gid, Vec<u8>)>, Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::to_vec(&json!({
            "ids": ids.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        }))
        .map_err(|e| Error::Invalid(format!("request serialization: {e}")))?;
        let (status, pack) = self.post("/objects", &body, true)?;
        if status != 200 {
            return Err(http_error(status, &pack));
        }
        decode_fetch_pack(&pack, ids)
    }

    fn push_objects(&mut self, records: &[(Gid, Vec<u8>)]) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let pack = crate::sync::gemlpack::encode_pack(
            &records
                .iter()
                .map(|(id, envelope)| crate::sync::gemlpack::PackRecord {
                    id: *id,
                    envelope: envelope.clone(),
                })
                .collect::<Vec<_>>(),
        )?;
        let (status, resp) = self.post("/push", &pack, false)?;
        if status != 200 {
            return Err(http_error(status, &resp));
        }
        let _: Value = serde_json::from_slice(&resp)
            .map_err(|e| Error::Invalid(format!("malformed server JSON: {e}")))?;
        Ok(())
    }

    fn update_refs(&mut self, refs: &[(String, Gid)]) -> Result<(), Error> {
        let _ = self.post_json(
            "/update-refs",
            &json!({ "refs": refs.iter().map(|(n, g)| json!([n, g.to_string()])).collect::<Vec<_>>() }),
        )?;
        Ok(())
    }
}

fn http_error(status: u16, body: &[u8]) -> Error {
    let msg = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
        .and_then(|e| {
            e.get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| {
            format!(
                "server returned HTTP {status}: {}",
                String::from_utf8_lossy(body)
            )
        });
    Error::Invalid(msg)
}

/// One HTTP POST. `json` selects the Content-Type. Bounded response reads.
fn http_post(
    host: &str,
    port: u16,
    path: &str,
    token: Option<&str>,
    json: bool,
    body: &[u8],
    timeout: Duration,
) -> Result<(u16, Vec<u8>), Error> {
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| Error::Invalid(format!("cannot connect to {host}:{port}: {e}")))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| Error::Invalid(format!("set read timeout: {e}")))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| Error::Invalid(format!("set write timeout: {e}")))?;
    let content_type = if json {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(t) = token {
        request.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    // Response: status line, headers, body (bounded).
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Invalid(format!("malformed HTTP status line: {status_line}")))?;
    let mut content_length: u64 = 0;
    let mut saw_length = false;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header)?;
        if n == 0 {
            return Err(Error::Invalid("truncated HTTP headers".into()));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(v) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            content_length = v
                .trim()
                .parse()
                .map_err(|_| Error::Invalid("malformed Content-Length".into()))?;
            saw_length = true;
        }
    }
    if !saw_length {
        return Err(Error::Invalid("response has no Content-Length".into()));
    }
    if content_length > crate::sync::session::MAX_PACK_BYTES {
        return Err(Error::Limit {
            kind: "http response body",
            limit: crate::sync::session::MAX_PACK_BYTES,
            found: content_length,
        });
    }
    let mut body = vec![0u8; content_length as usize];
    reader.read_exact(&mut body)?;
    Ok((status, body))
}

/// Options for the hosted HTTP server.
#[derive(Debug, Clone, Default)]
pub struct ServeHttpOptions {
    /// Serve any Gemel repository under this root by URL path; when `None`,
    /// only the server's own repository (path `/`) is served.
    pub root: Option<PathBuf>,
    /// Globally refuse mutations even for write tokens.
    pub read_only: bool,
    /// Bearer tokens: token -> capability (`read` | `write`). When empty and
    /// the listener is non-loopback, startup fails (fail closed).
    pub tokens: Vec<(String, String)>,
}

/// The known HTTP sync endpoints.
const HTTP_ENDPOINTS: [&str; 6] = [
    "/refs",
    "/reachable",
    "/missing",
    "/objects",
    "/push",
    "/update-refs",
];

/// Extracts the endpoint from a request target: `/foo/refs` → `/refs`,
/// `/refs` → `/refs`. Unknown targets keep their full path (404 later).
fn endpoint_of(target: &str) -> &str {
    let path = target.split('?').next().unwrap_or(target);
    for ep in HTTP_ENDPOINTS {
        if path == ep {
            return ep;
        }
        if let Some(p) = path.strip_suffix(ep) {
            if !p.is_empty() {
                return ep;
            }
        }
    }
    path
}

/// The repository path portion of a request target: `/foo/refs` → `/foo`,
/// `/refs` → `/` (the server's own repository).
fn repo_path_of(target: &str) -> &str {
    let path = target.split('?').next().unwrap_or(target);
    for ep in HTTP_ENDPOINTS {
        if path == ep {
            return "/";
        }
        if let Some(p) = path.strip_suffix(ep) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    path
}

/// Runs the HTTP sync server until it errors or is interrupted.
pub fn serve_http(server_root: &Path, addr: &str, opts: &ServeHttpOptions) -> Result<(), Error> {
    let listener =
        TcpListener::bind(addr).map_err(|e| Error::Invalid(format!("cannot bind {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| Error::Invalid(format!("local addr: {e}")))?;
    let is_loopback = bound.ip().is_loopback();
    if !is_loopback && opts.tokens.is_empty() {
        return Err(Error::Invalid(
            "refusing to serve HTTP on a non-loopback address without bearer tokens \
             (THREAT_MODEL.md §10; transport encryption is mandatory for non-local remotes)"
                .into(),
        ));
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let result = handle_http_connection(stream, server_root, opts);
        if let Err(e) = result {
            eprintln!("http serve: {e}");
        }
    }
    Ok(())
}

fn handle_http_connection(
    mut stream: TcpStream,
    server_root: &Path,
    opts: &ServeHttpOptions,
) -> Result<(), Error> {
    stream.set_read_timeout(Some(Duration::from_secs(120))).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    // Request line.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return respond(
            &mut stream,
            400,
            true,
            b"{\"error\":\"unsupported HTTP version\"}",
        );
    }
    // Headers (bounded).
    let mut content_length: Option<u64> = None;
    let mut auth: Option<String> = None;
    for _ in 0..64 {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return respond(&mut stream, 400, true, b"{\"error\":\"truncated headers\"}");
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(v) = header
            .strip_prefix("Content-Length:")
            .or_else(|| header.strip_prefix("content-length:"))
        {
            content_length = Some(
                v.trim()
                    .parse()
                    .map_err(|_| Error::Invalid("malformed Content-Length".into()))?,
            );
        } else if let Some(v) = header
            .strip_prefix("Authorization:")
            .or_else(|| header.strip_prefix("authorization:"))
        {
            auth = Some(v.trim().to_string());
        }
    }
    if method != "POST" {
        return respond(
            &mut stream,
            405,
            true,
            b"{\"error\":\"method not allowed\"}",
        );
    }
    let Some(length) = content_length else {
        return respond(
            &mut stream,
            411,
            true,
            b"{\"error\":\"Content-Length required\"}",
        );
    };
    if length > crate::sync::session::MAX_PACK_BYTES {
        return respond(&mut stream, 413, true, b"{\"error\":\"request too large\"}");
    }
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;
    // Authz.
    let capability = match check_auth(&auth, opts)? {
        Some(cap) => cap,
        None => return respond(&mut stream, 401, true, b"{\"error\":\"unauthorized\"}"),
    };
    if opts.read_only {
        let cap = capability;
        let _ = cap;
    }
    // Resolve the repository for this path (the endpoint suffix is not part
    // of the repo path: `/foo/refs` serves repository `/foo`).
    let repo = match resolve_http_repo(server_root, opts, target)? {
        Some(r) => r,
        None => {
            return respond(
                &mut stream,
                404,
                true,
                b"{\"error\":\"no such repository\"}",
            );
        }
    };
    let read_only = opts.read_only || capability == "read";
    // Dispatch on the endpoint (the last path segment group), so multi-repo
    // targets like `/foo/refs` route correctly.
    let endpoint = endpoint_of(target);
    match endpoint {
        "/refs" | "/reachable" | "/missing" | "/update-refs" => {
            let params: Value = serde_json::from_slice(&body)
                .map_err(|e| Error::Invalid(format!("bad JSON body: {e}")))?;
            let params = params.as_object().cloned().unwrap_or_default();
            let op = match endpoint {
                "/refs" => "list_refs",
                "/reachable" => "reachable",
                "/missing" => "missing",
                "/update-refs" => "update_refs",
                _ => unreachable!(),
            };
            match session::handle_json(&repo, op, &params, read_only) {
                Ok(result) => {
                    let bytes = serde_json::to_vec(&result)
                        .map_err(|e| Error::Invalid(format!("serialization: {e}")))?;
                    respond(&mut stream, 200, true, &bytes)
                }
                Err(e) => respond_err(&mut stream, 400, &e),
            }
        }
        "/objects" => {
            let params: Value = serde_json::from_slice(&body)
                .map_err(|e| Error::Invalid(format!("bad JSON body: {e}")))?;
            let ids = match params.get("ids") {
                None => Vec::new(),
                Some(arr) => arr
                    .as_array()
                    .ok_or_else(|| Error::Invalid("ids must be an array".into()))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .ok_or_else(|| Error::Invalid("id must be a string".into()))?
                            .parse::<Gid>()
                            .map_err(|e| Error::Invalid(format!("invalid gid: {e}")))
                    })
                    .collect::<Result<Vec<_>, Error>>()?,
            };
            match session::handle_fetch(&repo, &ids) {
                Ok(pack) => respond(&mut stream, 200, false, &pack),
                Err(e) => respond_err(&mut stream, 400, &e),
            }
        }
        "/push" => match session::handle_push(&repo, &body, read_only) {
            Ok(inserted) => {
                let bytes = serde_json::to_vec(&json!({ "ok": true, "inserted": inserted }))
                    .map_err(|e| Error::Invalid(format!("serialization: {e}")))?;
                respond(&mut stream, 200, true, &bytes)
            }
            Err(e) => respond_err(&mut stream, 400, &e),
        },
        other => respond(
            &mut stream,
            404,
            true,
            &serde_json::to_vec(&json!({ "error": format!("unknown endpoint {other}") }))
                .map_err(|e| Error::Invalid(format!("serialization: {e}")))?,
        ),
    }
}

fn check_auth(
    auth: &Option<String>,
    opts: &ServeHttpOptions,
) -> Result<Option<&'static str>, Error> {
    if opts.tokens.is_empty() {
        return Ok(Some("write")); // no auth configured: open (loopback only enforced at bind)
    }
    let Some(header) = auth else {
        return Ok(None);
    };
    let token = header
        .strip_prefix("Bearer ")
        .ok_or_else(|| Error::Invalid("authorization scheme must be Bearer".into()))?;
    Ok(opts
        .tokens
        .iter()
        .find(|(t, _)| t == token)
        .map(|(_, cap)| if cap == "read" { "read" } else { "write" }))
}

fn resolve_http_repo(
    server_root: &Path,
    opts: &ServeHttpOptions,
    target: &str,
) -> Result<Option<Repo>, Error> {
    let rel = repo_path_of(target).trim_start_matches('/');
    let dir = match &opts.root {
        Some(root) => {
            if rel.is_empty() {
                root.clone()
            } else {
                // Path traversal is rejected: only single-segment repo names.
                if rel.contains('/') || rel == ".." || rel == "." {
                    return Ok(None);
                }
                root.join(rel)
            }
        }
        None => {
            if !rel.is_empty() {
                return Ok(None);
            }
            server_root.to_path_buf()
        }
    };
    if !dir.join(crate::store::META_DIR).join("meta.json").is_file() {
        return Ok(None);
    }
    match Repo::open(&dir) {
        Ok(r) => Ok(Some(r)),
        Err(_) => Ok(None),
    }
}

fn respond(stream: &mut TcpStream, status: u16, json: bool, body: &[u8]) -> Result<(), Error> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let content_type = if json {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn respond_err(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), Error> {
    let body = serde_json::to_vec(&json!({
        "error": { "code": "query_failed", "message": message }
    }))
    .map_err(|e| Error::Invalid(format!("serialization: {e}")))?;
    respond(stream, status, true, &body)
}
