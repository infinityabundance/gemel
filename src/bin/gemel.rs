//! The `gemel` command-line interface (SPECIFICATION.md Phase 1).
//!
//! Commands: init, status, snapshot, change begin/finish, log, show, diff,
//! checkout, fsck. Every command supports machine-readable JSON output
//! (AGENT_PROTOCOL.md §2–§3).

// The CLI returns the rich store error type; boxed variants would obscure
// construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use clap::{Parser, Subcommand};
use gemel::gid::Gid;
use gemel::store::{Error, InitOptions, Repo, REF_STATE_HEAD};
use gemel::value::Object;
use gemel::workflow;
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "gemel",
    version,
    about = "Evidence-native version control for agentic software development."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Repository root (default: discovered upward from the current directory).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a repository.
    Init {
        /// Directory to initialize (default: current directory).
        path: Option<PathBuf>,
        #[arg(long)]
        author_name: Option<String>,
        #[arg(long)]
        author_email: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show repository status.
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
    /// Record the working tree as a state.
    Snapshot {
        /// Optional label (default: auto name S<n>).
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create and finish changes.
    #[command(subcommand)]
    Change(ChangeCmd),
    /// List changes (optionally expanded to episodes/operations).
    Log {
        #[arg(long)]
        operations: bool,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Show a single object by identity or name.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Diff two states (default: working tree vs head).
    Diff {
        /// State A (default: working tree).
        a: Option<String>,
        /// State B (default: head state).
        b: Option<String>,
        #[arg(long)]
        stat: bool,
        #[arg(long, default_value_t = 3)]
        context: usize,
        #[arg(long)]
        json: bool,
    },
    /// Materialize a state into a directory.
    Checkout {
        state: String,
        /// Target directory (default: repository root).
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Verify repository integrity.
    Fsck {
        #[arg(long)]
        repair: bool,
        #[arg(long)]
        rebuild_index: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum ChangeCmd {
    /// Begin a change.
    Begin {
        /// Input state (default: workspace state, then head).
        #[arg(long)]
        from: Option<String>,
        /// Existing intent identity/name.
        #[arg(long)]
        intent: Option<String>,
        /// Create a new intent with this summary.
        #[arg(long)]
        intent_summary: Option<String>,
        /// Producer identity/name (default: repository default).
        #[arg(long)]
        producer: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Finish the pending change.
    Finish {
        #[arg(long, default_value = "change")]
        summary: String,
        /// Claim: subject|predicate|kind
        #[arg(long)]
        claim: Vec<String>,
        /// Evidence: subject|outcome|kind
        #[arg(long)]
        evidence: Vec<String>,
        /// Residual: summary|severity|classification
        #[arg(long)]
        residual: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    reset_sigpipe();
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Restores the default SIGPIPE disposition so piping output into `head` /
/// `grep` terminates quietly instead of panicking on EPIPE (Rust ignores
/// SIGPIPE by default). `signal(2)` lives in libc, which Rust links on Unix.
#[cfg(unix)]
fn reset_sigpipe() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

fn run(cli: &Cli) -> Result<u8, Error> {
    match &cli.command {
        Command::Init {
            path,
            author_name,
            author_email,
            json,
        } => {
            let root = path
                .clone()
                .or_else(|| cli.repo.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
            let repo = Repo::init(
                &root,
                &InitOptions {
                    author_name: author_name.clone(),
                    author_email: author_email.clone(),
                },
            )?;
            let config = repo.read_ref(gemel::store::REF_CONFIG)?.unwrap();
            let producer = repo.read_meta()?["default_producer"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if *json {
                print_json(
                    "init",
                    json!({ "repository": root.display().to_string(), "config": config.to_string(), "default_producer": producer }),
                );
            } else {
                println!("initialized repository at {}", root.display());
                println!("  config: {}", config);
                println!("  default producer: {}", producer);
            }
            Ok(0)
        }
        Command::Status { json, verbose } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let st = gemel::query::status(&repo)?;
            if *json {
                let result = json!({
                    "trajectory": st.trajectory,
                    "intent": st.intent.map(|g| g.to_string()),
                    "state": st.state.map(|g| g.to_string()),
                    "changed": st.changed.iter().map(|(p, s)| json!({
                        "path": p,
                        "status": format!("{:?}", s),
                    })).collect::<Vec<_>>(),
                    "claims": st.claims.iter().map(|c| json!({
                        "id": c.gid.to_string(),
                        "predicate": c.predicate,
                        "status": c.status.as_str(),
                    })).collect::<Vec<_>>(),
                    "residuals": st.residuals.iter().map(|r| json!({
                        "id": r.gid.to_string(),
                        "summary": r.summary,
                        "severity": r.severity,
                        "disposition": r.disposition,
                    })).collect::<Vec<_>>(),
                    "readiness": st.readiness.as_str(),
                });
                print_json("status", result);
            } else {
                println!(
                    "{}  {}",
                    st.trajectory.as_deref().unwrap_or("(no trajectory)"),
                    st.intent.map(|g| g.to_string()).unwrap_or_default()
                );
                if let Some(state) = &st.state {
                    println!("state: {}", state);
                }
                let (added, modified, deleted) =
                    st.changed
                        .iter()
                        .fold((0, 0, 0), |(a, m, d), (_, s)| match s {
                            gemel::content::PathStatus::Added => (a + 1, m, d),
                            gemel::content::PathStatus::Modified => (a, m + 1, d),
                            gemel::content::PathStatus::Deleted => (a, m, d + 1),
                            gemel::content::PathStatus::TypeChanged => (a, m + 1, d),
                        });
                if st.changed.is_empty() {
                    println!("working tree clean");
                } else {
                    println!(
                        "{} file(s) changed: +{} ~{} -{}",
                        st.changed.len(),
                        added,
                        modified,
                        deleted
                    );
                    if *verbose {
                        for (p, s) in &st.changed {
                            println!("  {:?} {p}", s);
                        }
                    }
                }
                let supported = st
                    .claims
                    .iter()
                    .filter(|c| c.status == gemel::query::ClaimStatus::Supported)
                    .count();
                let total = st.claims.len();
                println!("claims: {supported}/{total} supported");
                for r in &st.residuals {
                    println!("residual: {} [{}] {}", r.disposition, r.severity, r.summary);
                }
                println!("readiness: {}", st.readiness.as_str());
            }
            Ok(0)
        }
        Command::Snapshot { label, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let ignore = gemel::ignore::Ignore::from_root(repo.root());
            let snap = gemel::content::build_state(&repo, repo.root(), &ignore)?;
            repo.with_write_lock(|| {
                let name = match label {
                    Some(l) => l.clone(),
                    None => {
                        let mut meta = repo.read_meta()?;
                        let n = meta["counters"]["state"].as_u64().unwrap_or(0) + 1;
                        meta["counters"]["state"] = json!(n);
                        repo.write_meta(&meta)?;
                        format!("S{n}")
                    }
                };
                let ops = vec![gemel::store::refs::RefOp::set(
                    &format!("{}/{}", gemel::store::REF_NAMES, name),
                    snap.state,
                )];
                repo.apply_refs_unlocked(&gemel::store::refs::RefTransaction { ops })?;
                workflow::set_workspace_state(&repo, snap.state)?;
                if *json {
                    print_json("snapshot", json!({ "state": snap.state.to_string(), "name": name, "files": snap.files, "bytes": snap.bytes }));
                } else {
                    println!("snapshot {}: {} files, {} bytes", name, snap.files, snap.bytes);
                    println!("  {}", snap.state);
                }
                Ok(())
            })
            .map(|_| 0)
        }
        Command::Change(cmd) => match cmd {
            ChangeCmd::Begin {
                from,
                intent,
                intent_summary,
                producer,
                json,
            } => {
                let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
                let out = workflow::begin_change(
                    &repo,
                    &workflow::BeginOptions {
                        from_state: from.as_deref().map(|s| repo.resolve(s)).transpose()?,
                        intent: intent.as_deref().map(|s| repo.resolve(s)).transpose()?,
                        intent_summary: intent_summary.clone(),
                        producer: producer.as_deref().map(|s| repo.resolve(s)).transpose()?,
                    },
                )?;
                if *json {
                    print_json(
                        "change begin",
                        json!({
                            "input_state": out.input_state.map(|g| g.to_string()),
                            "intent": out.intent.map(|g| g.to_string()),
                            "intent_name": out.intent_name,
                            "producer": out.producer.to_string(),
                        }),
                    );
                } else {
                    println!("change begun");
                    if let Some(s) = &out.intent_name {
                        println!("  intent: {s}");
                    }
                    if let Some(i) = out.intent {
                        println!("  intent: {i}");
                    }
                    if let Some(s) = out.input_state {
                        println!("  input state: {s}");
                    }
                }
                Ok(0)
            }
            ChangeCmd::Finish {
                summary,
                claim,
                evidence,
                residual,
                json,
            } => {
                let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
                let parse3 = |specs: &[String]| -> Result<Vec<Vec<String>>, Error> {
                    specs
                        .iter()
                        .map(|s| {
                            let parts: Vec<String> = s.split('|').map(|p| p.to_string()).collect();
                            if parts.is_empty() || parts[0].is_empty() {
                                return Err(Error::Invalid(format!("malformed spec {s:?}")));
                            }
                            Ok(parts)
                        })
                        .collect()
                };
                let claims = parse3(claim)?
                    .into_iter()
                    .map(|p| workflow::ClaimSpec {
                        subject: p.first().filter(|s| !s.is_empty()).cloned(),
                        predicate: p.get(1).cloned().unwrap_or_default(),
                        kind: p.get(2).cloned().unwrap_or_else(|| "other".into()),
                    })
                    .collect();
                let evidence = parse3(evidence)?
                    .into_iter()
                    .map(|p| workflow::EvidenceSpec {
                        subject: p.first().filter(|s| !s.is_empty()).cloned(),
                        outcome: p.get(1).cloned().unwrap_or_else(|| "pass".into()),
                        kind: p.get(2).cloned().unwrap_or_else(|| "test_result".into()),
                    })
                    .collect();
                let residuals = parse3(residual)?
                    .into_iter()
                    .map(|p| workflow::ResidualSpec {
                        summary: p.first().cloned().unwrap_or_default(),
                        severity: p.get(1).cloned().unwrap_or_else(|| "medium".into()),
                        classification: p.get(2).cloned().unwrap_or_else(|| "other".into()),
                    })
                    .collect();
                let out = workflow::finish_change(
                    &repo,
                    &workflow::FinishOptions {
                        summary: summary.clone(),
                        claims,
                        evidence,
                        residuals,
                    },
                )?;
                if *json {
                    print_json(
                        "change finish",
                        json!({
                            "change": out.change.to_string(),
                            "change_name": out.change_name,
                            "trajectory": out.trajectory.to_string(),
                            "trajectory_name": out.trajectory_name,
                            "state": out.state.to_string(),
                            "state_name": out.state_name,
                            "operations": out.operations.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "claims": out.claims.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "evidence": out.evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "residuals": out.residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "new_trajectory": out.is_new_trajectory,
                        }),
                    );
                } else {
                    println!(
                        "{} (change {}) on {} ({}), state {} ({})",
                        summary,
                        out.change_name,
                        out.trajectory_name,
                        out.trajectory,
                        out.state_name,
                        out.state
                    );
                    if !out.operations.is_empty() {
                        println!("  {} operation(s)", out.operations.len());
                    }
                }
                Ok(0)
            }
        },
        Command::Log {
            operations,
            json,
            limit,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let entries = gemel::query::log(&repo, *limit)?;
            if *json {
                let result = json!({
                    "changes": entries.iter().map(|e| json!({
                        "id": e.change.to_string(),
                        "name": e.name,
                        "summary": e.summary,
                        "input_state": e.input_state.map(|g| g.to_string()),
                        "resulting_state": e.resulting_state.map(|g| g.to_string()),
                        "trajectory": e.trajectory,
                        "operations": e.operations.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                });
                print_json("log", result);
            } else {
                for e in &entries {
                    let name = e.name.clone().unwrap_or_else(|| e.change.to_string());
                    println!("{}  {}", name, e.summary);
                    match (e.input_state, e.resulting_state) {
                        (Some(a), Some(b)) => println!("    state {a} -> {b}"),
                        (None, Some(b)) => println!("    state (initial) -> {b}"),
                        _ => {}
                    }
                    if *operations {
                        for op in &e.operations {
                            let obj = repo.load(op)?;
                            let fs = obj.field_sequence().unwrap_or(&[]);
                            let kind = gemel::query::str_field(fs, 0x01).unwrap_or("operation");
                            let path = gemel::query::str_field(fs, 0x02).unwrap_or("");
                            println!("      {kind} {path}");
                        }
                    }
                }
                if entries.is_empty() {
                    println!("no changes");
                }
            }
            Ok(0)
        }
        Command::Show { id, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let (gid, obj, name) = gemel::query::show(&repo, id)?;
            if *json {
                let mut result = gemel::json::object_to_json(&obj).map_err(Error::Object)?;
                if let Some(o) = result.as_object_mut() {
                    o.insert("id".into(), json!(gid.to_string()));
                    if let Some(n) = &name {
                        o.insert("name".into(), json!(n));
                    }
                }
                print_json("show", result);
            } else {
                render_human(&repo, &gid, &obj, name.as_deref())?;
            }
            Ok(0)
        }
        Command::Diff {
            a,
            b,
            stat,
            context,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            match (a, b) {
                (Some(a), Some(b)) => {
                    let sa = gemel::query::resolve_state(&repo, a)?;
                    let sb = gemel::query::resolve_state(&repo, b)?;
                    render_state_diff(&repo, &sa, &sb, *stat, *context, *json)
                }
                (None, Some(b)) => {
                    let sb = gemel::query::resolve_state(&repo, b)?;
                    let sa = repo.read_ref(REF_STATE_HEAD)?.unwrap_or(sb);
                    render_state_diff(&repo, &sa, &sb, *stat, *context, *json)
                }
                _ => {
                    // Working tree vs head state.
                    let base = repo.read_ref(REF_STATE_HEAD)?;
                    let ignore = gemel::ignore::Ignore::from_root(repo.root());
                    let deltas = gemel::content::working_tree_delta(&repo, base.as_ref(), &ignore)?;
                    if *json {
                        let result = json!({
                            "textual": {
                                "files": {
                                    "added": deltas.iter().filter(|(_, s)| *s == gemel::content::PathStatus::Added).map(|(p, _)| p).collect::<Vec<_>>(),
                                    "deleted": deltas.iter().filter(|(_, s)| *s == gemel::content::PathStatus::Deleted).map(|(p, _)| p).collect::<Vec<_>>(),
                                    "changed": deltas.iter().filter(|(_, s)| *s != gemel::content::PathStatus::Added && *s != gemel::content::PathStatus::Deleted).map(|(p, _)| p).collect::<Vec<_>>(),
                                }
                            }
                        });
                        print_json("diff", result);
                    } else if *stat {
                        let (a, m, d) =
                            deltas.iter().fold((0, 0, 0), |(a, m, d), (_, s)| match s {
                                gemel::content::PathStatus::Added => (a + 1, m, d),
                                gemel::content::PathStatus::Deleted => (a, m, d + 1),
                                _ => (a, m + 1, d),
                            });
                        println!("+{a} ~{m} -{d}");
                        for (p, s) in &deltas {
                            println!("  {:?} {p}", s);
                        }
                    } else {
                        for (p, s) in &deltas {
                            println!("{:?} {p}", s);
                        }
                    }
                    Ok(0)
                }
            }
        }
        Command::Checkout { state, dir, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let gid = gemel::query::resolve_state(&repo, state)?;
            let target = dir.clone().unwrap_or_else(|| repo.root().to_path_buf());
            gemel::content::materialize(&repo, &gid, &target)?;
            repo.with_write_lock(|| {
                workflow::set_workspace_state(&repo, gid)?;
                Ok(())
            })?;
            if *json {
                print_json(
                    "checkout",
                    json!({ "state": gid.to_string(), "dir": target.display().to_string() }),
                );
            } else {
                println!("checked out {gid} into {}", target.display());
            }
            Ok(0)
        }
        Command::Fsck {
            repair,
            rebuild_index,
            json,
            verbose,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let report = repo.fsck(&gemel::store::fsck::FsckOptions {
                repair: *repair,
                rebuild_index: *rebuild_index,
                verbose: *verbose,
            })?;
            if *json {
                let result = json!({
                    "objects_scanned": report.objects_scanned,
                    "objects_ok": report.objects_ok,
                    "problems": report.problems.iter().map(|p| json!({
                        "severity": p.severity.as_str(),
                        "code": p.code,
                        "message": p.message,
                        "id": p.id.map(|g| g.to_string()),
                    })).collect::<Vec<_>>(),
                    "repairs": report.repairs,
                    "journal_recovered": report.journal_recovered,
                    "exit_code": report.exit_code(),
                });
                print_json("fsck", result);
            } else {
                println!(
                    "objects: {}/{} verified",
                    report.objects_ok, report.objects_scanned
                );
                for p in &report.problems {
                    println!("{} [{}] {}", p.severity.as_str(), p.code, p.message);
                }
                for r in &report.repairs {
                    println!("repaired: {r}");
                }
                if report.is_clean() {
                    println!("repository clean");
                }
            }
            Ok(report.exit_code())
        }
    }
}

/// Renders a human-readable view of an object by family.
fn render_human(repo: &Repo, gid: &Gid, obj: &Object, name: Option<&str>) -> Result<(), Error> {
    let fs = obj.field_sequence().unwrap_or(&[]);
    let sfield = |tag: u8| gemel::query::str_field(fs, tag).unwrap_or("").to_string();
    let gfield = |tag: u8| gemel::query::gid_field(fs, tag);
    let glist = |tag: u8| gemel::query::gid_list(fs, tag);
    match obj.family {
        gemel::family::Family::Blob => {
            let bytes = obj.blob_bytes().unwrap_or(&[]);
            let preview: String = bytes
                .iter()
                .take(64)
                .map(|b| {
                    if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            println!("blob {} ({} bytes)", gid, bytes.len());
            println!("  preview: {preview}");
        }
        gemel::family::Family::Tree => {
            println!("tree {}", gid);
            if let Some(gemel::value::Value::Array(items)) = gemel::query::value_at(fs, 0x01) {
                for item in items {
                    if let gemel::value::Value::Record(ef) = item {
                        let ename = gemel::query::str_field(ef, 0x01).unwrap_or("");
                        let emode = gemel::query::u64_field(ef, 0x02).unwrap_or(0);
                        let etarget = gemel::query::gid_field(ef, 0x03);
                        let kind = match emode {
                            0o040000 => "dir",
                            0o120000 => "link",
                            0o100755 => "exec",
                            _ => "file",
                        };
                        println!(
                            "  {kind:4} {ename:24} {}",
                            etarget.map(|g| g.to_string()).unwrap_or_default()
                        );
                    }
                }
            }
        }
        gemel::family::Family::State => {
            println!("state {}", gid);
            if let Some(t) = gfield(0x01) {
                println!("  root tree: {t}");
            }
        }
        gemel::family::Family::Operation => {
            println!("operation {}", gid);
            println!("  type: {}", sfield(0x01));
            if !sfield(0x02).is_empty() {
                println!("  path: {}", sfield(0x02));
            }
            if let Some(r) = gemel::query::record_field(fs, 0x06) {
                if let Some(status) = gemel::query::str_field(r, 0x01) {
                    println!("  result: {status}");
                }
            }
        }
        gemel::family::Family::Episode => {
            println!("episode {}", gid);
            println!("  summary: {}", sfield(0x09));
            if !sfield(0x0A).is_empty() {
                println!("  outcome: {}", sfield(0x0A));
            }
        }
        gemel::family::Family::Intent => {
            println!("intent {}", gid);
            println!("  summary: {}", sfield(0x01));
            if !sfield(0x02).is_empty() {
                println!("  description: {}", sfield(0x02));
            }
        }
        gemel::family::Family::Change => {
            println!("change {}", gid);
            if let Some(n) = name {
                println!("  name: {n}");
            }
            println!("  summary: {}", sfield(0x01));
            if let Some(i) = gfield(0x02) {
                println!("  intent: {i}");
            }
            if let Some(a) = gfield(0x03) {
                println!("  input state: {a}");
            }
            if let Some(b) = gfield(0x05) {
                println!("  resulting state: {b}");
            }
            println!("  operations: {}", glist(0x04).len());
            println!("  claims: {}", glist(0x0C).len());
            println!("  evidence: {}", glist(0x0D).len());
            println!("  residuals: {}", glist(0x0E).len());
        }
        gemel::family::Family::Case => {
            println!("case {}", gid);
            println!("  summary: {}", sfield(0x02));
            if !sfield(0x05).is_empty() {
                println!("  status: {}", sfield(0x05));
            }
        }
        gemel::family::Family::Trajectory => {
            println!("trajectory {}", gid);
            if let Some(n) = name {
                println!("  name: {n}");
            }
            if let Some(i) = gfield(0x02) {
                println!("  intent: {i}");
            }
            if let Some(b) = gfield(0x03) {
                println!("  base state: {b}");
            }
            if !sfield(0x0A).is_empty() {
                println!("  outcome: {}", sfield(0x0A));
            }
            println!("  changes (this version): {}", glist(0x06).len());
        }
        gemel::family::Family::Claim => {
            println!("claim {}", gid);
            println!("  predicate: {}", sfield(0x03));
            if !sfield(0x04).is_empty() {
                println!("  kind: {}", sfield(0x04));
            }
            if !sfield(0x01).is_empty() {
                println!("  subject: {}", sfield(0x01));
            }
            let (status, supporting, contradicting) = gemel::query::claim_status(repo, gid)?;
            println!("  status: {}", status.as_str());
            println!(
                "  supporting: {} contradicting: {}",
                supporting.len(),
                contradicting.len()
            );
        }
        gemel::family::Family::Evidence => {
            println!("evidence {}", gid);
            println!("  kind: {}", sfield(0x02));
            if !sfield(0x03).is_empty() {
                println!("  subject: {}", sfield(0x03));
            }
            if let Some(r) = gemel::query::record_field(fs, 0x0D) {
                if let Some(o) = gemel::query::str_field(r, 0x01) {
                    println!("  outcome: {o}");
                }
            }
        }
        gemel::family::Family::Residual => {
            println!("residual {}", gid);
            println!("  summary: {}", sfield(0x02));
            println!("  severity: {}", sfield(0x04));
            println!(
                "  disposition: {}",
                gemel::query::residual_disposition(repo, gid)?
            );
            println!(
                "  persistence: {} descendant change(s)",
                gemel::query::residual_persistence(repo, gid)?
            );
        }
        gemel::family::Family::Verification => {
            println!("verification {}", gid);
            if !sfield(0x01).is_empty() {
                println!("  subject: {}", sfield(0x01));
            }
            println!("  result: {}", sfield(0x07));
        }
        gemel::family::Family::Producer => {
            println!("producer {}", gid);
            println!("  kind: {}", sfield(0x01));
            println!("  name: {}", sfield(0x02));
            println!("  disclosure: {}", sfield(0x04));
        }
        gemel::family::Family::AgentRun => {
            println!("agentrun {}", gid);
            println!("  model: {} {}", sfield(0x02), sfield(0x03));
            println!("  harness: {}", sfield(0x04));
        }
        gemel::family::Family::Environment => {
            println!("environment {}", gid);
            println!("  arch: {}", sfield(0x02));
            if let Some(os) = gemel::query::record_field(fs, 0x01) {
                let family = gemel::query::str_field(os, 0x01).unwrap_or("");
                println!("  os: {family}");
            }
        }
        gemel::family::Family::Reconciliation => {
            println!("reconciliation {}", gid);
            println!("  summary: {}", sfield(0x01));
            println!(
                "  adopted: {} rejected: {}",
                glist(0x05).len(),
                glist(0x06).len()
            );
        }
        gemel::family::Family::Release => {
            println!("release {}", gid);
            println!("  name: {}", sfield(0x01));
            if let Some(v) = gfield(0x03) {
                println!("  state: {v}");
            }
        }
        gemel::family::Family::ContextManifest => {
            println!("context-manifest {}", gid);
            println!("  source objects: {}", glist(0x01).len());
        }
        gemel::family::Family::Checkpoint => {
            println!("checkpoint {}", gid);
            println!("  summary: {}", sfield(0x02));
        }
        gemel::family::Family::Config => {
            println!("config {}", gid);
            println!("  execution_policy: {}", sfield(0x04));
            println!("  disclosure_default: {}", sfield(0x05));
        }
        gemel::family::Family::Mapping => {
            println!("mapping {}", gid);
            println!("  kind: {}", sfield(0x01));
            println!("  from: {}", sfield(0x02));
            if let Some(to) = gfield(0x03) {
                println!("  to: {to}");
            }
        }
    }
    Ok(())
}

/// Renders a diff between two states.
fn render_state_diff(
    repo: &Repo,
    a: &gemel::gid::Gid,
    b: &gemel::gid::Gid,
    stat: bool,
    context: usize,
    json: bool,
) -> Result<u8, Error> {
    let deltas = gemel::content::diff_states(repo, a, b)?;
    if json {
        let result = json!({
            "textual": {
                "files": {
                    "added": deltas.iter().filter(|d| d.kind == gemel::content::DeltaKind::Created).map(|d| d.path.clone()).collect::<Vec<_>>(),
                    "deleted": deltas.iter().filter(|d| d.kind == gemel::content::DeltaKind::Deleted).map(|d| d.path.clone()).collect::<Vec<_>>(),
                    "changed": deltas.iter().filter(|d| d.kind == gemel::content::DeltaKind::Modified).map(|d| d.path.clone()).collect::<Vec<_>>(),
                    "renamed": deltas.iter().filter_map(|d| match &d.kind { gemel::content::DeltaKind::Renamed { from } => Some(json!({"from": from, "to": d.path})), _ => None }).collect::<Vec<_>>(),
                }
            }
        });
        print_json("diff", result);
        return Ok(0);
    }
    if stat {
        let (created, modified, deleted, renamed) =
            deltas
                .iter()
                .fold((0, 0, 0, 0), |(c, m, d, r), x| match x.kind {
                    gemel::content::DeltaKind::Created => (c + 1, m, d, r),
                    gemel::content::DeltaKind::Modified => (c, m + 1, d, r),
                    gemel::content::DeltaKind::Deleted => (c, m, d + 1, r),
                    gemel::content::DeltaKind::Renamed { .. } => (c, m, d, r + 1),
                });
        println!("+{created} ~{modified} -{deleted} >{renamed}");
        for d in &deltas {
            match &d.kind {
                gemel::content::DeltaKind::Renamed { from } => {
                    println!("  > {} -> {}", from, d.path)
                }
                _ => println!("  {} {}", delta_symbol(&d.kind), d.path),
            }
        }
        return Ok(0);
    }
    for d in &deltas {
        match &d.kind {
            gemel::content::DeltaKind::Created => {
                println!("+ {}", d.path);
            }
            gemel::content::DeltaKind::Deleted => {
                println!("- {}", d.path);
            }
            gemel::content::DeltaKind::Renamed { from } => {
                println!("> {from} -> {}", d.path);
            }
            gemel::content::DeltaKind::Modified => {
                let a_content = blob_text(repo, &d.old_blob.unwrap())?;
                let b_content = blob_text(repo, &d.new_blob.unwrap())?;
                let a_lines = gemel::content::split_lines(&a_content);
                let b_lines = gemel::content::split_lines(&b_content);
                let diff = gemel::content::unified_diff(
                    &format!("a/{}", d.path),
                    &format!("b/{}", d.path),
                    &a_lines,
                    &b_lines,
                    context,
                );
                print!("{diff}");
            }
        }
    }
    Ok(0)
}

fn delta_symbol(kind: &gemel::content::DeltaKind) -> &'static str {
    match kind {
        gemel::content::DeltaKind::Created => "+",
        gemel::content::DeltaKind::Deleted => "-",
        gemel::content::DeltaKind::Modified => "~",
        gemel::content::DeltaKind::Renamed { .. } => ">",
    }
}

/// Reads a blob as text (lossy for binary content).
fn blob_text(repo: &Repo, blob: &gemel::gid::Gid) -> Result<String, Error> {
    let obj = repo.load(blob)?;
    let bytes = obj.blob_bytes().unwrap_or(&[]);
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Prints a `gemel.query.v1` response envelope.
fn print_json(command: &str, result: serde_json::Value) {
    let response = json!({
        "schema": "gemel.query.v1",
        "request": { "command": command },
        "pagination": { "has_more": false, "next_cursor": null, "count": 1 },
        "result": result,
        "omitted": [],
        "uncertainty": [],
    });
    println!("{}", serde_json::to_string_pretty(&response).unwrap());
}

// Helper: parse an exit code from a subcommand result.
