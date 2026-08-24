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
        /// Named workspace whose materialized-state record to update
        /// (default: `default`).
        #[arg(long)]
        workspace: Option<String>,
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
    /// Causal blame: why does this subject exist? (Phase 2)
    Why {
        /// Canonical path, entity name, or identity.
        subject: String,
        #[arg(long)]
        json: bool,
    },
    /// List claims (filtered, paginated).
    Claims {
        /// Only claims about this subject.
        #[arg(long)]
        subject: Option<String>,
        /// Only claims with this derived status.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one evidence object, or list evidence for a subject.
    Evidence {
        /// Evidence identity (omit with --subject).
        id: Option<String>,
        /// List evidence about this subject.
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List residuals (filtered, paginated).
    Residuals {
        /// Only residuals affecting this subject.
        #[arg(long)]
        subject: Option<String>,
        /// Only residuals with this disposition.
        #[arg(long)]
        disposition: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Trajectories relevant to a subject (attempts).
    Attempts {
        subject: String,
        #[arg(long)]
        json: bool,
    },
    /// Show or close a trajectory.
    Trajectory {
        /// Trajectory name or identity.
        id: Option<String>,
        /// Close with this outcome (publishes a chained version).
        #[arg(long)]
        close: Option<String>,
        /// Termination reason (with --close).
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Manage a residual.
    #[command(subcommand)]
    Residual(ResidualCmd),
    /// Create a continuation checkpoint.
    Checkpoint {
        /// Human summary (default: machine-generated).
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Smallest sufficient context for a subject.
    Context {
        /// Canonical path, entity name, or identity.
        subject: String,
        /// Intent to pursue (affects relevant attempts).
        #[arg(long)]
        for_intent: Option<String>,
        /// Token budget.
        #[arg(long, default_value_t = 4096)]
        budget: usize,
        /// Comma-separated categories: claims,residuals,attempts,evidence.
        #[arg(long)]
        include: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Reconcile trajectories into a chosen direction.
    Reconcile {
        /// Input trajectories (2+).
        #[arg(required = true, num_args = 2..)]
        trajectories: Vec<String>,
        /// Dry-run: analyze only, publish nothing.
        #[arg(long)]
        plan: bool,
        /// Advance head, state/head, and the workspace to the result.
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ResidualCmd {
    /// Publish a residual disposition event (open/acknowledged/resolved/
    /// superseded/irrelevant) as a chained version.
    Resolve {
        /// Residual name or identity.
        id: String,
        #[arg(long)]
        disposition: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
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
        /// Named workspace (default: `default`).
        #[arg(long)]
        workspace: Option<String>,
        /// Working directory the change will be finished from.
        #[arg(long)]
        worktree: Option<PathBuf>,
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
        /// Named workspace (default: `default`).
        #[arg(long)]
        workspace: Option<String>,
        /// Working directory to snapshot (default: repository root).
        #[arg(long)]
        worktree: Option<PathBuf>,
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
                workspace,
                worktree,
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
                        workspace: workspace.clone(),
                        worktree: worktree.clone(),
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
                            "workspace": workspace,
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
                    if let Some(w) = workspace {
                        println!("  workspace: {w}");
                    }
                }
                Ok(0)
            }
            ChangeCmd::Finish {
                summary,
                claim,
                evidence,
                residual,
                workspace,
                worktree,
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
                        workspace: workspace.clone(),
                        worktree: worktree.clone(),
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
        Command::Checkout {
            state,
            dir,
            workspace,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let gid = gemel::query::resolve_state(&repo, state)?;
            let target = dir.clone().unwrap_or_else(|| repo.root().to_path_buf());
            gemel::content::materialize(&repo, &gid, &target)?;
            repo.with_write_lock(|| {
                match workspace {
                    Some(w) => workflow::set_workspace_named_state(&repo, w, gid)?,
                    None => workflow::set_workspace_state(&repo, gid)?,
                }
                Ok(())
            })?;
            if *json {
                print_json(
                    "checkout",
                    json!({ "state": gid.to_string(), "dir": target.display().to_string() }),
                );
            } else {
                println!("checked out {gid} into {}", target.display());
                if let Some(w) = workspace {
                    println!("  workspace: {w}");
                }
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
        Command::Why { subject, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let report = gemel::query::why(&repo, subject)?;
            if *json {
                print_json("why", why_json(&report));
            } else {
                render_why(&repo, &report)?;
            }
            Ok(0)
        }
        Command::Claims {
            subject,
            status,
            limit,
            cursor,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let want = match status.as_deref() {
                None => None,
                Some(s) => Some(parse_claim_status(s)?),
            };
            let (rows, next) = gemel::query::claims(
                &repo,
                &gemel::query::ClaimsFilter {
                    subject: subject.clone(),
                    status: want,
                    limit: *limit,
                    cursor: cursor.clone(),
                },
            )?;
            if *json {
                let claims: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.gid.to_string(),
                            "predicate": r.predicate,
                            "predicate_kind": r.predicate_kind,
                            "subject": r.subject,
                            "status": r.status.as_str(),
                            "supporting": r.supporting.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "contradicting": r.contradicting.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "scope": r.scope,
                            "change": r.change.map(|g| g.to_string()),
                            "trajectory": r.trajectory,
                        })
                    })
                    .collect();
                print_json_paged(
                    "claims",
                    json!({ "claims": claims }),
                    rows.len(),
                    next.as_deref(),
                );
            } else {
                for r in &rows {
                    println!("{}  [{}] {}", r.gid, r.status.as_str(), r.predicate);
                    if let Some(s) = &r.subject {
                        println!("    subject: {s}");
                    }
                    if !r.supporting.is_empty() {
                        println!("    supporting: {}", ids(&r.supporting));
                    }
                    if !r.contradicting.is_empty() {
                        println!("    contradicting: {}", ids(&r.contradicting));
                    }
                }
                if let Some(n) = next {
                    println!("next: --cursor {n}");
                }
                if rows.is_empty() {
                    println!("no claims");
                }
            }
            Ok(0)
        }
        Command::Evidence { id, subject, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            match (id, subject) {
                (Some(id), None) => {
                    let row = gemel::query::evidence_show(&repo, id)?;
                    if *json {
                        print_json("evidence", evidence_json(&row));
                    } else {
                        render_evidence(&row);
                    }
                }
                (None, Some(subject)) => {
                    let rows = gemel::query::evidence_for_subject(&repo, subject)?;
                    if *json {
                        let list: Vec<serde_json::Value> = rows.iter().map(evidence_json).collect();
                        print_json("evidence", json!({ "subject": subject, "evidence": list }));
                    } else {
                        for r in &rows {
                            render_evidence(r);
                        }
                        if rows.is_empty() {
                            println!("no evidence for {subject:?}");
                        }
                    }
                }
                _ => return Err(Error::Invalid("provide either <id> or --subject".into())),
            }
            Ok(0)
        }
        Command::Residuals {
            subject,
            disposition,
            limit,
            cursor,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let (rows, next) = gemel::query::residuals(
                &repo,
                &gemel::query::ResidualsFilter {
                    subject: subject.clone(),
                    disposition: disposition.clone(),
                    limit: *limit,
                    cursor: cursor.clone(),
                },
            )?;
            if *json {
                let residuals: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.gid.to_string(),
                            "summary": r.summary,
                            "classification": r.classification,
                            "severity": r.severity,
                            "disposition": r.disposition,
                            "persistence": { "descendant_changes": r.persistence },
                            "origin_evidence": r.origin_evidence.map(|g| g.to_string()),
                            "affected_claims": r.affected_claims.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "affected_changes": r.affected_changes.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                print_json_paged(
                    "residuals",
                    json!({ "residuals": residuals }),
                    rows.len(),
                    next.as_deref(),
                );
            } else {
                for r in &rows {
                    println!(
                        "{} [{}] {}",
                        r.disposition,
                        r.severity.as_deref().unwrap_or("medium"),
                        r.summary
                    );
                    if let Some(c) = &r.classification {
                        println!("    class: {c}");
                    }
                    println!("    persistence: {} descendant change(s)", r.persistence);
                }
                if let Some(n) = next {
                    println!("next: --cursor {n}");
                }
                if rows.is_empty() {
                    println!("no residuals");
                }
            }
            Ok(0)
        }
        Command::Attempts { subject, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let rows = gemel::query::attempts(&repo, subject)?;
            if *json {
                let attempts: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|a| {
                        json!({
                            "trajectory": a.trajectory.to_string(),
                            "name": a.name,
                            "intent": a.intent.map(|g| g.to_string()),
                            "outcome": a.outcome,
                            "termination_reason": a.termination_reason,
                            "evidence": a.evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "residuals": a.residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "handoff": a.handoff_summary,
                            "touched_subject": a.touched_subject,
                        })
                    })
                    .collect();
                print_json(
                    "attempts",
                    json!({ "subject": subject, "attempts": attempts }),
                );
            } else {
                for a in &rows {
                    let name = a.name.clone().unwrap_or_else(|| a.trajectory.to_string());
                    println!(
                        "{}  {}",
                        name,
                        a.outcome.clone().unwrap_or_else(|| "incomplete".into())
                    );
                    if let Some(r) = &a.termination_reason {
                        println!("    reason: {r}");
                    }
                    if !a.evidence.is_empty() {
                        println!("    evidence: {}", ids(&a.evidence));
                    }
                    if !a.residuals.is_empty() {
                        println!("    residuals: {}", ids(&a.residuals));
                    }
                }
                if rows.is_empty() {
                    println!("no previous attempts for {subject:?}");
                }
            }
            Ok(0)
        }
        Command::Trajectory {
            id,
            close,
            reason,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            match (id, close) {
                (Some(id), None) => {
                    let detail = gemel::query::trajectory_detail(&repo, id)?;
                    if *json {
                        print_json("trajectory", trajectory_json(&detail));
                    } else {
                        render_trajectory(&repo, &detail)?;
                    }
                }
                (Some(id), Some(outcome)) => {
                    let out = workflow::close_trajectory(
                        &repo,
                        &workflow::CloseTrajectoryOptions {
                            trajectory: id.clone(),
                            outcome: outcome.clone(),
                            reason: reason.clone(),
                            producer: None,
                        },
                    )?;
                    if *json {
                        print_json(
                            "trajectory close",
                            json!({
                                "name": out.name,
                                "previous": out.previous.to_string(),
                                "version": out.version.to_string(),
                                "outcome": outcome,
                            }),
                        );
                    } else {
                        println!("{} closed as {outcome} (version {})", out.name, out.version);
                        if let Some(r) = reason {
                            println!("  reason: {r}");
                        }
                    }
                }
                _ => return Err(Error::Invalid("trajectory requires an id".into())),
            }
            Ok(0)
        }
        Command::Residual(ResidualCmd::Resolve {
            id,
            disposition,
            reason,
            json,
        }) => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let out = workflow::resolve_residual(
                &repo,
                &workflow::ResolveResidualOptions {
                    residual: id.clone(),
                    disposition: disposition.clone(),
                    reason: reason.clone(),
                    producer: None,
                },
            )?;
            if *json {
                print_json(
                    "residual resolve",
                    json!({
                        "previous": out.previous.to_string(),
                        "version": out.version.to_string(),
                        "disposition": disposition,
                    }),
                );
            } else {
                println!(
                    "residual {} marked {disposition} (version {})",
                    out.previous, out.version
                );
                if let Some(r) = reason {
                    println!("  reason: {r}");
                }
            }
            Ok(0)
        }
        Command::Checkpoint { summary, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let out = workflow::create_checkpoint(
                &repo,
                &workflow::CheckpointOptions {
                    summary: summary.clone(),
                    producer: None,
                },
            )?;
            if *json {
                print_json(
                    "checkpoint",
                    json!({
                        "id": out.checkpoint.to_string(),
                        "name": out.name,
                        "summary": out.plan.summary,
                        "intent": out.plan.intent.map(|g| g.to_string()),
                        "trajectory": out.plan.trajectory.as_ref().map(|(n, g)| json!({ "name": n, "id": g.to_string() })),
                        "state": out.plan.state.map(|g| g.to_string()),
                        "open_claims": out.plan.open_claims.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        "unresolved_residuals": out.plan.unresolved_residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        "important_evidence": out.plan.important_evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        "recent_decisions": out.plan.recent_decisions.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        "relevant_attempts": out.plan.relevant_attempts.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                        "continuation_scope": out.plan.continuation_scope,
                    }),
                );
            } else {
                println!("checkpoint {} ({})", out.name, out.checkpoint);
                println!("  {}", out.plan.summary);
                if let Some((name, _)) = &out.plan.trajectory {
                    println!("  trajectory: {name}");
                }
                if let Some(state) = out.plan.state {
                    println!("  state: {state}");
                }
                println!(
                    "  open claims: {}  open residuals: {}",
                    out.plan.open_claims.len(),
                    out.plan.unresolved_residuals.len()
                );
                for s in &out.plan.continuation_scope {
                    println!("  next: {s}");
                }
            }
            Ok(0)
        }
        Command::Context {
            subject,
            for_intent,
            budget,
            include,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let flags = gemel::query::IncludeFlags::parse(include.as_deref().unwrap_or(""))?;
            let bundle = gemel::query::context_bundle(
                &repo,
                subject,
                for_intent.as_deref(),
                *budget,
                flags,
            )?;
            if *json {
                print_json(
                    "context",
                    json!({
                        "subject": subject,
                        "intent": bundle.intent.map(|g| g.to_string()),
                        "budget": {
                            "tokens": bundle.budget_tokens,
                            "consumed": bundle.consumed,
                            "remaining": bundle.budget_tokens.saturating_sub(bundle.consumed),
                        },
                        "bundle": {
                            "objects": bundle.items.iter().map(|i| json!({
                                "id": i.id.to_string(),
                                "family": i.family.short(),
                                "level": i.level,
                                "summary": i.summary,
                            })).collect::<Vec<_>>(),
                            "deduplicated": bundle.deduplicated,
                            "expanded": {
                                "claims": bundle.expanded.claims,
                                "residuals": bundle.expanded.residuals,
                                "attempts": bundle.expanded.attempts,
                                "evidence": bundle.expanded.evidence,
                            },
                        },
                        "next": {
                            "expand": bundle.next_expand,
                            "why": if bundle.next_expand.is_empty() { "complete".to_string() } else { "budget".to_string() },
                        },
                        "omitted": bundle.omitted,
                    }),
                );
            } else {
                println!(
                    "context for {subject:?} ({} tokens used of {})",
                    bundle.consumed, bundle.budget_tokens
                );
                for i in &bundle.items {
                    println!(
                        "  L{} {} {}  {}",
                        i.level,
                        i.family.short(),
                        i.id,
                        i.summary
                    );
                }
                if !bundle.next_expand.is_empty() {
                    println!(
                        "budget exhausted; expand next: {}",
                        bundle.next_expand.join(", ")
                    );
                }
                if bundle.items.is_empty() {
                    println!("nothing relevant found");
                }
            }
            Ok(0)
        }
        Command::Reconcile {
            trajectories,
            plan,
            apply,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let inputs: Vec<gemel::reconcile::ReconcileInput> = trajectories
                .iter()
                .map(|t| {
                    let gid = repo.resolve(t)?;
                    let obj = repo.load(&gid)?;
                    if obj.family != gemel::family::Family::Trajectory {
                        return Err(Error::Invalid(format!("{t} is not a trajectory")));
                    }
                    Ok(gemel::reconcile::ReconcileInput {
                        name: t.clone(),
                        gid,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            if *plan {
                let p = gemel::reconcile::analyze(&repo, &inputs)?;
                if *json {
                    print_json("reconcile", reconcile_plan_json(&p, true));
                } else {
                    render_reconcile_plan(&repo, &p)?;
                }
            } else {
                let out = gemel::reconcile::reconcile(
                    &repo,
                    &inputs,
                    &gemel::reconcile::ReconcileOptions {
                        apply: *apply,
                        producer: None,
                    },
                )?;
                if *json {
                    let mut result = reconcile_plan_json(&out.plan, false);
                    if let Some(o) = result.as_object_mut() {
                        o.insert(
                            "reconciliation".into(),
                            json!(out.reconciliation.to_string()),
                        );
                        o.insert("reconciliation_name".into(), json!(out.reconciliation_name));
                        o.insert("change".into(), json!(out.change.to_string()));
                        o.insert("change_name".into(), json!(out.change_name));
                        o.insert("state".into(), json!(out.state.to_string()));
                        o.insert("state_name".into(), json!(out.state_name));
                        o.insert("applied".into(), json!(apply));
                    }
                    print_json("reconcile", result);
                } else {
                    println!(
                        "{}: {} adopted, {} rejected (resulting state {})",
                        out.reconciliation_name,
                        out.plan.adopted.len(),
                        out.plan.rejected.len(),
                        out.state_name
                    );
                    println!("  reconciliation: {}", out.reconciliation);
                    println!("  resulting change: {} ({})", out.change_name, out.change);
                    println!("  resulting state: {}", out.state);
                    for c in &out.plan.textual_conflicts {
                        println!("  conflict: {}", c.path);
                    }
                    if *apply {
                        println!("  applied: head + workspace advanced");
                    }
                }
            }
            Ok(0)
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

/// Prints a paged `gemel.query.v1` envelope (AGENT_PROTOCOL.md §4.1).
fn print_json_paged(
    command: &str,
    result: serde_json::Value,
    count: usize,
    next_cursor: Option<&str>,
) {
    let response = json!({
        "schema": "gemel.query.v1",
        "request": { "command": command },
        "pagination": {
            "has_more": next_cursor.is_some(),
            "next_cursor": next_cursor,
            "count": count,
        },
        "result": result,
        "omitted": [],
        "uncertainty": [],
    });
    println!("{}", serde_json::to_string_pretty(&response).unwrap());
}

/// A comma-separated list of textual gids.
fn ids(gids: &[gemel::gid::Gid]) -> String {
    gids.iter()
        .map(|g| g.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses a claim status flag.
fn parse_claim_status(s: &str) -> Result<gemel::query::ClaimStatus, Error> {
    use gemel::query::ClaimStatus;
    match s.to_ascii_uppercase().as_str() {
        "SUPPORTED" => Ok(ClaimStatus::Supported),
        "PARTIALLY_SUPPORTED" | "PARTIAL" => Ok(ClaimStatus::PartiallySupported),
        "CONTRADICTED" => Ok(ClaimStatus::Contradicted),
        "UNVERIFIED" => Ok(ClaimStatus::Unverified),
        "STALE" => Ok(ClaimStatus::Stale),
        "SUPERSEDED" => Ok(ClaimStatus::Superseded),
        other => Err(Error::Invalid(format!("unknown claim status {other:?}"))),
    }
}

/// The `why` report as JSON (AGENT_PROTOCOL.md §5.2).
fn why_json(report: &gemel::query::WhyReport) -> serde_json::Value {
    let introduced = report.introduced_by.as_ref().map(|n| {
        json!({
            "change": { "id": n.change.to_string(), "name": n.change_name, "summary": n.summary },
            "intent": n.intent.map(|g| json!({
                "id": g.to_string(), "summary": n.intent_summary,
            })),
            "claim": n.claim.as_ref().map(|c| json!({
                "id": c.id.to_string(), "predicate": c.predicate, "status": c.status.as_str(),
            })),
            "evidence": n.evidence.iter().map(|e| json!({
                "id": e.id.to_string(), "kind": e.kind, "subject": e.subject, "outcome": e.outcome,
            })).collect::<Vec<_>>(),
            "residuals": n.residuals.iter().map(|r| json!({
                "id": r.id.to_string(), "summary": r.summary,
                "severity": r.severity, "disposition": r.disposition,
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "subject": report.subject,
        "introduced_by": introduced,
        "last_modified": report.last_modified.map(|g| g.to_string()),
        "previous_approaches": report.previous_approaches.iter().map(attempt_json).collect::<Vec<_>>(),
    })
}

fn attempt_json(a: &gemel::query::AttemptSummary) -> serde_json::Value {
    json!({
        "trajectory": a.trajectory.to_string(),
        "name": a.name,
        "intent": a.intent.map(|g| g.to_string()),
        "outcome": a.outcome,
        "termination_reason": a.termination_reason,
        "evidence": a.evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "residuals": a.residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "handoff": a.handoff_summary,
        "touched_subject": a.touched_subject,
    })
}

/// Human rendering of `why` (brief §14).
fn render_why(repo: &Repo, report: &gemel::query::WhyReport) -> Result<(), Error> {
    println!("subject: {}", report.subject);
    if let Some(n) = &report.introduced_by {
        println!(
            "introduced by {} ({})",
            n.change_name
                .clone()
                .unwrap_or_else(|| n.change.to_string()),
            n.summary
        );
        if let Some(i) = &n.intent {
            let name = repo.name_of(i)?.unwrap_or_else(|| i.to_string());
            println!(
                "  intent: {name} {}",
                n.intent_summary.clone().unwrap_or_default()
            );
        }
        if let Some(c) = &n.claim {
            println!("  claim: {} [{}]", c.predicate, c.status.as_str());
            for e in &n.evidence {
                println!(
                    "    evidence: {} {} {}",
                    e.id,
                    e.kind,
                    e.outcome.clone().unwrap_or_default()
                );
            }
            for r in &n.residuals {
                println!(
                    "    residual: {} [{}] {}",
                    r.disposition,
                    r.severity.clone().unwrap_or_default(),
                    r.summary
                );
            }
        }
    } else {
        println!("  (no change in this repository touches this subject)");
    }
    if !report.previous_approaches.is_empty() {
        println!("previous approaches:");
        for a in &report.previous_approaches {
            println!(
                "  {} {}",
                a.name.clone().unwrap_or_else(|| a.trajectory.to_string()),
                a.outcome.clone().unwrap_or_else(|| "incomplete".into())
            );
            if let Some(r) = &a.termination_reason {
                println!("    reason: {r}");
            }
        }
    }
    for u in &report.uncertainty {
        println!("uncertainty: {u}");
    }
    Ok(())
}

/// An evidence row as JSON (AGENT_PROTOCOL.md §5.4).
fn evidence_json(row: &gemel::query::EvidenceRow) -> serde_json::Value {
    json!({
        "id": row.gid.to_string(),
        "kind": row.kind,
        "subject": row.subject,
        "outcome": row.outcome,
        "evaluated_state": row.evaluated_state.map(|g| g.to_string()),
        "freshness": {
            "status": row.freshness.as_str(),
            "caused_by": Vec::<String>::new(),
        },
        "reproduction": {
            "replayable": row.reproduction_replayable.unwrap_or(false),
            "inputs_present": false,
            "policy_required": false,
        },
        "producer": row.producer.map(|g| g.to_string()),
    })
}

fn render_evidence(row: &gemel::query::EvidenceRow) {
    println!("evidence {}", row.gid);
    println!("  kind: {}", row.kind);
    if let Some(s) = &row.subject {
        println!("  subject: {s}");
    }
    if let Some(o) = &row.outcome {
        println!("  outcome: {o}");
    }
    println!("  freshness: {}", row.freshness.as_str());
    if let Some(s) = row.evaluated_state {
        println!("  evaluated state: {s}");
    }
}

/// A trajectory detail as JSON (AGENT_PROTOCOL.md §5.7).
fn trajectory_json(detail: &gemel::query::TrajectoryDetail) -> serde_json::Value {
    json!({
        "id": detail.gid.to_string(),
        "name": detail.name,
        "intent": detail.intent.map(|g| g.to_string()),
        "base_state": detail.base_state.map(|g| g.to_string()),
        "outcome": detail.outcome,
        "termination_reason": detail.termination_reason,
        "sequence": detail.sequence.iter().map(|c| json!({
            "change": c.change.to_string(),
            "summary": c.summary,
            "state": c.state.map(|g| g.to_string()),
        })).collect::<Vec<_>>(),
        "evidence": detail.evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "residuals": detail.residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "handoff": detail.handoff.as_ref().map(|h| json!({
            "summary": h.summary,
            "completed": h.completed,
            "remaining": h.remaining,
            "open_residuals": h.open_residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "important_evidence": h.important_evidence.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "recommended_objects": h.recommended_objects.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "next_steps": h.next_steps,
        })),
        "created_at": detail.created_at,
    })
}

fn render_trajectory(repo: &Repo, detail: &gemel::query::TrajectoryDetail) -> Result<(), Error> {
    println!(
        "trajectory {} ({})",
        detail
            .name
            .clone()
            .unwrap_or_else(|| detail.gid.to_string()),
        detail.gid
    );
    if let Some(i) = detail.intent {
        let name = repo.name_of(&i)?.unwrap_or_else(|| i.to_string());
        println!("  intent: {name}");
    }
    if let Some(b) = detail.base_state {
        println!("  base state: {b}");
    }
    println!(
        "  outcome: {}",
        detail
            .outcome
            .clone()
            .unwrap_or_else(|| "incomplete".into())
    );
    if let Some(r) = &detail.termination_reason {
        println!("  termination reason: {r}");
    }
    for c in &detail.sequence {
        println!("  {}  {}", c.change, c.summary);
    }
    if !detail.evidence.is_empty() {
        println!("  evidence: {}", ids(&detail.evidence));
    }
    if !detail.residuals.is_empty() {
        println!("  residuals: {}", ids(&detail.residuals));
    }
    if let Some(h) = &detail.handoff {
        if let Some(s) = &h.summary {
            println!("  handoff: {s}");
        }
    }
    Ok(())
}

/// The reconcile plan as JSON (AGENT_PROTOCOL.md §5.10).
fn reconcile_plan_json(p: &gemel::reconcile::ReconcilePlan, is_plan: bool) -> serde_json::Value {
    json!({
        "inputs": p.inputs.iter().map(|i| json!({ "name": i.name, "id": i.gid.to_string() })).collect::<Vec<_>>(),
        "textual_conflicts": p.textual_conflicts.iter().map(|c| json!({
            "path": c.path,
            "changes": c.changes.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "semantic_interactions": p.interactions.iter().map(|i| json!({
            "kind": i.kind,
            "certainty": i.certainty,
            "detail": i.detail,
            "subjects": i.subjects.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "claims": {
            "retained": p.claims_retained.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "invalidated": p.claims_invalidated.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
            "verification_required": p.verification_required.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        },
        "evidence_retained": p.evidence_retained.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "unresolved_residuals": p.unresolved_residuals.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "resulting_state": p.resulting_state.to_string(),
        "adopted": p.adopted.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "rejected": p.rejected.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
        "rationale": p.rationale,
        "mode": if is_plan { "plan" } else { "executed" },
    })
}

fn render_reconcile_plan(repo: &Repo, p: &gemel::reconcile::ReconcilePlan) -> Result<(), Error> {
    let _ = repo;
    println!("reconcile plan (dry-run; nothing published)");
    println!(
        "  inputs: {}",
        p.inputs
            .iter()
            .map(|i| i.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if p.textual_conflicts.is_empty() {
        println!("  textual conflicts: none");
    } else {
        for c in &p.textual_conflicts {
            println!("  textual conflict: {} ({})", c.path, ids(&c.changes));
        }
    }
    for i in &p.interactions {
        println!("  interaction [{}] {}: {}", i.kind, i.certainty, i.detail);
    }
    println!(
        "  adopted: {}  rejected: {}",
        ids(&p.adopted),
        ids(&p.rejected)
    );
    println!(
        "  claims retained: {}  invalidated: {}  verification required: {}",
        p.claims_retained.len(),
        p.claims_invalidated.len(),
        p.verification_required.len()
    );
    println!("  unresolved residuals: {}", p.unresolved_residuals.len());
    println!("  resulting state: {}", p.resulting_state);
    println!("  rationale: {}", p.rationale);
    Ok(())
}

// Helper: parse an exit code from a subcommand result.
