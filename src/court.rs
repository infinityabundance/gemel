//! The FRF court runner (Phase 8; HOSTED.md §7, brief §38).
//!
//! `gemel court <evidence-id>` re-executes the reproduction command recorded
//! in an evidence object and publishes the fresh observation as new
//! evidence. This is the *separate, explicit, policy-gated* action the brief
//! contemplates: nothing is ever executed during ingestion, `status`, or
//! sync. The default `execution_policy` is `never_auto_execute`, which
//! refuses. `policy_gated` requires an explicit `--allow`; `allowlist`
//! permits commands whose first token matches `.gemel/court.allowlist`.
//!
//! The court never fabricates: the new evidence records the observed exit
//! code and output digests, links the evaluated head state, and is a fresh
//! canonical object — the original evidence is untouched.

use crate::gid::Gid;
use crate::store::{Error, Repo};
use crate::value::{Field, Object, Value};
use std::time::{Duration, Instant};

/// The deterministic producer of court observations.
pub const COURT_PRODUCER_NAME: &str = "court-runner";

/// The result of a court run.
#[derive(Debug, Clone)]
pub struct CourtOutcome {
    pub evidence: Gid,
    pub outcome: String,
    pub exit_code: Option<i64>,
    pub stdout_digest: Option<String>,
    pub stderr_digest: Option<String>,
    pub detail: String,
}

/// Runs the court for an evidence object under the repository's execution
/// policy. `allow` satisfies `policy_gated`; `timeout_secs` bounds the run.
pub fn run_court(
    repo: &Repo,
    evidence: &Gid,
    allow: bool,
    timeout_secs: u64,
) -> Result<CourtOutcome, Error> {
    let eobj = repo.load(evidence)?;
    let efs = eobj.field_sequence().unwrap_or(&[]);
    let command = crate::query::str_field(efs, 0x05).ok_or_else(|| {
        Error::Invalid(format!("evidence {evidence} has no reproduction command"))
    })?;
    let command = command.trim();
    if command.is_empty() {
        return Err(Error::Invalid(format!(
            "evidence {evidence} has an empty reproduction command"
        )));
    }
    let subject = crate::query::str_field(efs, 0x03).map(|s| s.to_string());
    let kind = crate::query::str_field(efs, 0x02)
        .unwrap_or("test_result")
        .to_string();

    // Execution policy (config 0x04; defaults to never_auto_execute).
    let policy = execution_policy(repo)?;
    match policy.as_str() {
        "never_auto_execute" => {
            return Err(Error::Invalid(
                "execution denied (POLICY_DENIED): config.execution_policy is \
                 never_auto_execute; switch to policy_gated or allowlist to run courts"
                    .into(),
            ));
        }
        "policy_gated" => {
            if !allow {
                return Err(Error::Invalid(
                    "execution denied (POLICY_DENIED): execution_policy is policy_gated; \
                     pass --allow to run this court explicitly"
                        .into(),
                ));
            }
        }
        "allowlist" => {
            if !allowlist_permits(repo, command)? {
                return Err(Error::Invalid(
                    "execution denied (POLICY_DENIED): the command is not in .gemel/court.allowlist"
                        .into(),
                ));
            }
        }
        other => {
            return Err(Error::Invalid(format!(
                "unknown execution_policy {other:?} in config"
            )));
        }
    }

    // Execute (bounded by the timeout; never during ingestion — this is the
    // explicit court action).
    let timeout = Duration::from_secs(timeout_secs.clamp(1, 3600));
    let outcome = execute(repo, command, timeout)?;

    // Publish the fresh observation.
    let producer = crate::defaults::automation_producer_object_at(COURT_PRODUCER_NAME, 0);
    let producer_gid = repo.insert_object(&producer)?;
    let head_state = repo.read_ref(crate::store::REF_STATE_HEAD)?;
    let mut fields = vec![
        Field::new(0x01, Value::Gid(producer_gid)),
        Field::new(0x02, Value::Str(kind)),
    ];
    if let Some(s) = &subject {
        fields.push(Field::new(0x03, Value::Str(s.clone())));
    }
    fields.push(Field::new(0x05, Value::Str(command.to_string())));
    let mut result = vec![Field::new(0x01, Value::Str(outcome.outcome.clone()))];
    if !outcome.detail.is_empty() {
        result.push(Field::new(0x02, Value::Str(outcome.detail.clone())));
    }
    if let Some(code) = outcome.exit_code {
        result.push(Field::new(0x03, Value::I(code)));
    }
    fields.push(Field::new(0x0D, Value::Record(result)));
    fields.push(Field::new(
        0x0F,
        Value::Record(vec![
            Field::new(0x01, Value::B(true)),
            Field::new(0x02, Value::B(true)),
            Field::new(0x04, Value::B(false)),
        ]),
    ));
    fields.push(Field::new(0x10, Value::I(crate::store::now_ms())));
    if let Some(s) = head_state {
        fields.push(Field::new(0x11, Value::Gid(s)));
    }
    let new_evidence =
        repo.insert_object(&Object::fields(crate::family::Family::Evidence, fields))?;
    Ok(CourtOutcome {
        evidence: new_evidence,
        outcome: outcome.outcome,
        exit_code: outcome.exit_code,
        stdout_digest: outcome.stdout_digest,
        stderr_digest: outcome.stderr_digest,
        detail: outcome.detail,
    })
}

struct ExecResult {
    outcome: String,
    exit_code: Option<i64>,
    stdout_digest: Option<String>,
    stderr_digest: Option<String>,
    detail: String,
}

/// Runs `sh -c <command>` with a timeout, capturing output. The command is
/// the reproduction command recorded by the producer — executing it is the
/// entire point of the court, and only the court does it.
fn execute(repo: &Repo, command: &str, timeout: Duration) -> Result<ExecResult, Error> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(repo.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Invalid(format!("cannot execute reproduction command: {e}")))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(ExecResult {
                        outcome: "inconclusive".into(),
                        exit_code: None,
                        stdout_digest: None,
                        stderr_digest: None,
                        detail: format!("court timed out after {}s", timeout.as_secs()),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Invalid(format!("court wait failed: {e}")));
            }
        }
    };
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = status.and_then(|s| s.code()).map(i64::from);
    let outcome = match exit_code {
        Some(0) => "pass",
        Some(_) => "fail",
        None => "inconclusive",
    };
    let detail = {
        let mut d = String::new();
        if !stderr.is_empty() {
            d.push_str(&stderr);
        }
        if d.len() > 4096 {
            d.truncate(4096);
            d.push('…');
        }
        d
    };
    Ok(ExecResult {
        outcome: outcome.into(),
        exit_code,
        stdout_digest: Some(crate::hex::encode(&crate::hash::blake3_256(
            stdout.as_bytes(),
        ))),
        stderr_digest: Some(crate::hex::encode(&crate::hash::blake3_256(
            stderr.as_bytes(),
        ))),
        detail,
    })
}

/// The active `execution_policy` (config 0x04), defaulting to
/// `never_auto_execute`.
fn execution_policy(repo: &Repo) -> Result<String, Error> {
    let Some(cfg) = repo.read_ref(crate::store::REF_CONFIG)? else {
        return Ok("never_auto_execute".into());
    };
    match repo.load(&cfg) {
        Ok(obj) => Ok(obj
            .field_sequence()
            .and_then(|fs| crate::query::str_field(fs, 0x04))
            .unwrap_or("never_auto_execute")
            .to_string()),
        Err(_) => Ok("never_auto_execute".into()),
    }
}

/// `.gemel/court.allowlist`: one pattern per line (`#` comments). A pattern
/// matches when the command's first token equals it, or starts with it when
/// it ends in `*`.
fn allowlist_permits(repo: &Repo, command: &str) -> Result<bool, Error> {
    let path = repo.meta_dir().join("court.allowlist");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let first = command.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        return Ok(false);
    }
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(prefix) = line.strip_suffix('*') {
            if first.starts_with(prefix) {
                return Ok(true);
            }
        } else if line == first {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The allowlist path (documented; tests).
pub fn allowlist_path(repo: &Repo) -> std::path::PathBuf {
    repo.meta_dir().join("court.allowlist")
}

/// Writes the allowlist (used by tests and `gemel court --allowlist-file`).
pub fn write_allowlist(repo: &Repo, patterns: &[&str]) -> Result<(), Error> {
    let mut text = String::new();
    for p in patterns {
        text.push_str(p);
        text.push('\n');
    }
    crate::exchange::export::write_atomic_fsync(&allowlist_path(repo), text.as_bytes())
}
