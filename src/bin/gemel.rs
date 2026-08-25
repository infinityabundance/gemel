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
use std::path::{Path, PathBuf};
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
        /// Semantic entity diff (Phase 5): added/removed/modified/moved.
        #[arg(long)]
        semantic: bool,
        #[arg(long, default_value_t = 3)]
        context: usize,
        #[arg(long)]
        json: bool,
    },
    /// Materialize a state into a directory (exact by default: files not in
    /// the state are removed, protecting .gemel/.git/.gitignore and ignored
    /// paths; --overlay leaves extra files untouched).
    Checkout {
        state: String,
        /// Target directory (default: repository root).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Named workspace whose materialized-state record to update
        /// (default: `default`).
        #[arg(long)]
        workspace: Option<String>,
        /// Overlay semantics: overwrite, leave extra files untouched.
        #[arg(long)]
        overlay: bool,
        /// Allow removing unignored unrecorded files at the repository root
        /// (exact checkout refuses without this).
        #[arg(long)]
        force: bool,
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
    /// Build the semantic index of a state (Phase 5; index head by default).
    Index {
        /// State identity/name to index (default: the head state).
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a semantic entity (or list current entities).
    Semantic {
        /// Entity name, `path::name`, `file:line`, or entity identity.
        subject: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
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
    /// Git-carried exchange rollups (Phase 1.5).
    #[command(subcommand)]
    Exchange(ExchangeCmd),
    /// Deterministically project Gemel changes into a Git repository
    /// (Phase 4; GIT_INTEROP.md §3).
    ExportGit {
        /// Target Git directory (default: `<repo>/.git`).
        #[arg(long)]
        git_dir: Option<PathBuf>,
        /// Branch to write (default: main).
        #[arg(long, default_value = "main")]
        branch: String,
        /// Emit GEMEL-CLAIM trailers (default: omit).
        #[arg(long)]
        include_claims: bool,
        #[arg(long)]
        json: bool,
    },
    /// Deterministically import a Git history into Gemel (Phase 4;
    /// GIT_INTEROP.md §4). Never fabricates provenance.
    ImportGit {
        /// Git directory to read (default: `<repo>/.git`).
        #[arg(long)]
        git_dir: Option<PathBuf>,
        /// Commit-ish to import (default: HEAD).
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long)]
        json: bool,
    },
    /// Clone a Git repository and import its history as Gemel work
    /// (Phase 4; GIT_INTEROP.md §6).
    Clone {
        /// Remote URL.
        url: String,
        /// Target directory (default: derived from the URL).
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Manage native sync remotes (Phase 6; STORAGE.md §10).
    Remote {
        #[command(subcommand)]
        cmd: Option<RemoteCmd>,
    },
    /// Fetch a remote's objects and refs into refs/remotes/<name>/*.
    Fetch {
        /// Remote name (or a path).
        remote: String,
        #[arg(long)]
        json: bool,
    },
    /// Push the local public refs to a remote (native sync; a Git-only
    /// remote receives the deterministic projection).
    Push {
        /// Remote name (or a path).
        remote: String,
        #[arg(long)]
        json: bool,
    },
    /// Fetch a remote and fast-forward the local refs (native sync; a
    /// Git-only remote is imported). Never overwrites diverged local work.
    Pull {
        /// Remote name (or a path).
        remote: String,
        #[arg(long)]
        json: bool,
    },
    /// Run the lightweight agent protocol session over stdin/stdout
    /// (Phase 7; brief §15.4).
    Protocol {
        #[arg(long)]
        json: bool,
    },
    /// Derive the next-step plan from durable repository state (Phase 7;
    /// brief §57 — never fake intelligence).
    Next {
        #[arg(long)]
        json: bool,
    },
    /// Show the active policy (required-verification matrix and gaps).
    Policy {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ExchangeCmd {
    /// Discover, validate, and report exchange material.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Generate deterministic missing packs and a Frontier Descriptor
    /// (append-only; frontier last).
    Export {
        #[arg(long, default_value = "frontier")]
        profile: String,
        #[arg(long)]
        json: bool,
    },
    /// Explicitly quarantine-ingest exchange material (normally automatic
    /// via `status`).
    Ingest {
        #[arg(long)]
        json: bool,
    },
    /// Validate exchange artifacts without activating them.
    Verify {
        /// Verify against the working tree (default).
        #[arg(long)]
        working_tree: bool,
        /// Verify against the Git index (staged tree).
        #[arg(long)]
        git_index: bool,
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

#[derive(Subcommand)]
enum RemoteCmd {
    /// Add (or replace) a remote; `--init` initializes a new remote repo.
    Add {
        name: String,
        path: String,
        #[arg(long)]
        init: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove a remote.
    Remove {
        name: String,
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
            // Keep the local native store invisible to Git while allowing
            // exchange material to be tracked (EXCHANGE.md §3).
            let _ = gemel::exchange::export::install_local_gitignore(repo.meta_dir());
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
            // Phase 1.5: discover exchange material and auto-ingest (or
            // bootstrap a fresh native store) before computing status
            // (EXCHANGE.md §34, §17).
            let cwd = std::env::current_dir()?;
            let start = cli.repo.as_deref().unwrap_or(&cwd);
            let (repo, bootstrapped, exchange) = match discover_exchange_root(start)? {
                None => {
                    let repo = Repo::find(start)?;
                    (repo, false, false)
                }
                Some(root)
                    if root
                        .join(gemel::store::META_DIR)
                        .join("meta.json")
                        .is_file() =>
                {
                    let repo = Repo::open(&root)?;
                    let has_exchange =
                        !gemel::exchange::discover_frontiers(repo.meta_dir())?.is_empty();
                    if has_exchange {
                        let _ = gemel::exchange::ingest::ingest(&repo)?;
                    }
                    (repo, false, has_exchange)
                }
                Some(root) => {
                    // Fresh native store over existing exchange material.
                    let out = gemel::exchange::ingest::bootstrap(&root)?;
                    let repo = Repo::open(&root)?;
                    (repo, true, out.frontiers_found > 0)
                }
            };
            let st = gemel::query::status(&repo)?;
            let exchange_block = exchange_json(&repo, exchange, bootstrapped)?;
            // Readiness must carry the source-binding mismatch: an imported
            // context that does not describe the checked-out source is never
            // READY (EXCHANGE.md §19, §56).
            let stale = exchange && !exchange_block["source_match"].as_bool().unwrap_or(false);
            if *json {
                let mut result = json!({
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
                    "semantic": st.semantic_entities.map(|n| json!({ "entities": n })),
                    "readiness": st.readiness.as_str(),
                    "exchange": exchange_block,
                });
                if stale {
                    result["readiness"] = json!("NOT_READY");
                    result["exchange"]["context"] = json!("STALE");
                }
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
                if exchange {
                    println!(
                        "exchange: present{}",
                        if bootstrapped { " (bootstrapped)" } else { "" }
                    );
                    if stale {
                        println!(
                            "exchange: SOURCE_CONTEXT_DIVERGED (imported context is historical)"
                        );
                    }
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
                if let Some(n) = st.semantic_entities {
                    println!("semantic: {n} entities indexed");
                } else if st.state.is_some() {
                    println!("semantic: not indexed (gemel index)");
                }
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
                        // CLI claims declare no evidence links: the
                        // relationship stays unknown until an explicit
                        // `claim link` act (AGENT_PROTOCOL.md §7).
                        evidence: Vec::new(),
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
                        // CLI residuals declare no links: affected claims /
                        // origin evidence stay unknown (explicit unknowns
                        // over fabricated history).
                        affected_claims: Vec::new(),
                        origin_evidence: None,
                        affected_changes: Vec::new(),
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
                // Phase 1.5 (§21): when the repository lives inside a Git
                // worktree, automatically refresh the exchange projection so
                // the change's frontier is ready to be committed with the
                // source. A projection failure warns but never fails the
                // change (the exchange is derived, not primary).
                let mut exchange_exported = false;
                if repo.root().join(".git").exists() {
                    match gemel::exchange::export::export(
                        &repo,
                        gemel::exchange::export::Profile::Frontier,
                    ) {
                        Ok(_) => exchange_exported = true,
                        Err(e) => eprintln!("warning: exchange export failed: {e}"),
                    }
                }
                if *json {
                    let result = json!({
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
                        "exchange": { "exported": exchange_exported },
                    });
                    print_json("change finish", result);
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
                    if exchange_exported {
                        println!("  exchange: updated");
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
            semantic,
            context,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            if *semantic {
                // Default range: the head change's input -> resulting state
                // (what the latest change did semantically).
                let head_change = repo.read_ref(gemel::store::REF_HEAD)?;
                let (h_in, h_out) = match &head_change {
                    Some(h) => {
                        let obj = repo.load(h)?;
                        let fs = obj.field_sequence().unwrap_or(&[]);
                        (
                            gemel::query::gid_field(fs, 0x03),
                            gemel::query::gid_field(fs, 0x05),
                        )
                    }
                    None => (None, None),
                };
                let sa = match a {
                    Some(a) => gemel::query::resolve_state(&repo, a)?,
                    None => h_in.or(repo.read_ref(REF_STATE_HEAD)?).ok_or_else(|| {
                        Error::Invalid(
                            "no input state to diff against; pass explicit states".into(),
                        )
                    })?,
                };
                let sb = match b {
                    Some(b) => gemel::query::resolve_state(&repo, b)?,
                    None => h_out.or(gemel::query::head_state(&repo)?).ok_or_else(|| {
                        Error::Invalid("no head state to diff against; pass explicit states".into())
                    })?,
                };
                let d = gemel::semantic::semantic_diff(&repo, &sa, &sb)?;
                if *json {
                    let info = |i: &gemel::semantic::EntityInfo| {
                        json!({
                            "id": i.id.map(|g| g.to_string()),
                            "kind": i.kind,
                            "name": i.name,
                            "module_path": i.module_path,
                            "file_path": i.file_path,
                            "start_line": i.start_line,
                            "end_line": i.end_line,
                            "signature": i.signature,
                            "visibility": i.visibility,
                            "lineage": i.lineage.as_ref().map(|(f, e, c)| json!({
                                "from": f.to_string(),
                                "evidence": e,
                                "certainty": c,
                            })),
                        })
                    };
                    print_json(
                        "diff",
                        json!({
                            "semantic": {
                                "state_a": sa.to_string(),
                                "state_b": sb.to_string(),
                                "unchanged": d.unchanged,
                                "added": d.added.iter().map(info).collect::<Vec<_>>(),
                                "removed": d.removed.iter().map(info).collect::<Vec<_>>(),
                                "modified": d.modified.iter().map(|e| json!({
                                    "before": e.before.as_ref().map(info),
                                    "after": e.after.as_ref().map(info),
                                })).collect::<Vec<_>>(),
                                "moved": d.moved.iter().map(|e| json!({
                                    "before": e.before.as_ref().map(info),
                                    "after": e.after.as_ref().map(info),
                                })).collect::<Vec<_>>(),
                            }
                        }),
                    );
                } else {
                    println!("semantic diff {} -> {}", sa, sb);
                    if d.unchanged > 0 {
                        println!("unchanged: {} entity(ies)", d.unchanged);
                    }
                    for e in &d.moved {
                        let b = e.before.as_ref().map(|i| i.full_path()).unwrap_or_default();
                        let a = e.after.as_ref().map(|i| i.full_path()).unwrap_or_default();
                        println!("moved: {b} -> {a}");
                    }
                    for e in &d.modified {
                        let b = e.before.as_ref().map(|i| i.full_path()).unwrap_or_default();
                        let a = e.after.as_ref().map(|i| i.full_path()).unwrap_or_default();
                        println!("modified: {b} -> {a}");
                    }
                    for i in &d.added {
                        println!("added: {} ({})", i.full_path(), i.file_path);
                    }
                    for i in &d.removed {
                        println!("removed: {} ({})", i.full_path(), i.file_path);
                    }
                }
                return Ok(0);
            }
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
            overlay,
            force,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let gid = gemel::query::resolve_state(&repo, state)?;
            let target = dir.clone().unwrap_or_else(|| repo.root().to_path_buf());
            let ignore = gemel::ignore::Ignore::from_root(&target);
            // Protection for unrecorded work: gate on the read-only removal
            // plan BEFORE any mutation. An exact checkout at the repository
            // root refuses to delete unignored extras unless --force.
            if !*overlay {
                let would_remove = gemel::content::exact_removals(&repo, &gid, &target, &ignore)?;
                if !would_remove.is_empty() && dir.is_none() && !*force {
                    return Err(Error::Invalid(format!(
                        "checkout would remove {} unrecorded file(s): {}; pass --force to remove them, or --overlay to leave them",
                        would_remove.len(),
                        would_remove.join(", ")
                    )));
                }
            }
            let removed = if *overlay {
                gemel::content::materialize_overlay(&repo, &gid, &target)?;
                Vec::new()
            } else {
                gemel::content::materialize_exact(&repo, &gid, &target, &ignore)?
            };
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
                    json!({
                        "state": gid.to_string(),
                        "dir": target.display().to_string(),
                        "mode": if *overlay { "overlay" } else { "exact" },
                        "removed": removed,
                    }),
                );
            } else {
                println!("checked out {gid} into {}", target.display());
                if *overlay {
                    println!("  mode: overlay (extra files untouched)");
                } else if !removed.is_empty() {
                    println!("  removed: {}", removed.join(", "));
                }
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
                    "exchange": {
                        "frontiers": report.exchange_frontiers,
                        "imported": report.exchange_imported,
                    },
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
                if report.exchange_frontiers > 0 {
                    println!(
                        "exchange: {} frontier(s), {} imported",
                        report.exchange_frontiers, report.exchange_imported
                    );
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
        Command::Index { state, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let gid = match state {
                Some(s) => gemel::query::resolve_state(&repo, s)?,
                None => gemel::query::head_state(&repo)?.ok_or_else(|| {
                    Error::Invalid("no head state to index; create a change first".into())
                })?,
            };
            let producer = gemel::defaults::automation_producer_object_at(
                gemel::semantic::INDEXER_PRODUCER_NAME,
                0,
            );
            let producer_gid = gemel::content::object_identity(&repo, &producer)?;
            let out = gemel::semantic::index_state(&repo, &gid, &producer_gid)?;
            if *json {
                print_json(
                    "index",
                    json!({
                        "state": gid.to_string(),
                        "index": out.index.to_string(),
                        "entities": out.entities,
                        "files": out.files,
                        "new": out.new_entities,
                        "modified": out.modified_entities,
                        "moved": out.moved_entities,
                        "lineage_links": out.lineage_links,
                    }),
                );
            } else {
                println!(
                    "indexed {}: {} entity(ies) in {} file(s)",
                    gid, out.entities, out.files
                );
                println!("  index: {}", out.index);
                if out.lineage_links > 0 {
                    println!(
                        "  lineage: {} new, {} modified, {} moved ({} links)",
                        out.new_entities,
                        out.modified_entities,
                        out.moved_entities,
                        out.lineage_links
                    );
                }
            }
            Ok(0)
        }
        Command::Semantic {
            subject,
            limit,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            match subject {
                Some(subject) => {
                    let resolved = gemel::semantic::resolve_subject(&repo, subject)?;
                    let info = resolved.entity.ok_or_else(|| {
                        Error::Invalid(format!(
                            "no semantic entity matches {subject:?} (is the head state indexed?)"
                        ))
                    })?;
                    if *json {
                        print_json("semantic", entity_json(&info));
                    } else {
                        render_entity(&info);
                    }
                }
                None => {
                    let entities = gemel::semantic::current_entities(&repo)?.unwrap_or_default();
                    let entities = entities
                        .into_iter()
                        .take(*limit)
                        .filter_map(|(gid, _)| gemel::semantic::entity_info(&repo, &gid).ok())
                        .collect::<Vec<_>>();
                    if *json {
                        print_json(
                            "semantic",
                            json!({"entities": entities.iter().map(entity_json).collect::<Vec<_>>()}),
                        );
                    } else {
                        for e in &entities {
                            println!("{}  {}  {}", e.kind, e.full_path(), e.file_path);
                        }
                        if entities.is_empty() {
                            println!("no indexed entities (run gemel index)");
                        }
                    }
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
        Command::Exchange(cmd) => match cmd {
            ExchangeCmd::Status { json } => {
                let cwd = std::env::current_dir()?;
                let start = cli.repo.as_deref().unwrap_or(&cwd);
                let root = match discover_exchange_root(start)? {
                    Some(r) => r,
                    None => return Err(Error::NotARepository(start.to_path_buf())),
                };
                let repo = if root
                    .join(gemel::store::META_DIR)
                    .join("meta.json")
                    .is_file()
                {
                    Some(Repo::open(&root)?)
                } else {
                    None
                };
                let s = gemel::exchange::ingest::status(repo.as_ref(), &root)?;
                if *json {
                    print_json(
                        "exchange status",
                        json!({
                            "detected": s.detected,
                            "native_store": s.native_store,
                            "current_source_state": s.current_source_state.map(|g| g.to_string()),
                            "frontiers": s.frontiers.iter().map(|f| json!({
                                "id": f.id,
                                "source_state": f.source_state.to_string(),
                                "head_change": f.head_change.to_string(),
                                "profile": f.profile,
                                "imported": f.imported,
                                "binding": match f.binding {
                                    gemel::exchange::ingest::SourceBinding::Matched => "matched",
                                    gemel::exchange::ingest::SourceBinding::Diverged => "diverged",
                                },
                            })).collect::<Vec<_>>(),
                            "active": s.active,
                            "pending_export": s.pending_export,
                        }),
                    );
                } else {
                    if !s.detected {
                        println!("no exchange material present");
                    } else {
                        println!(
                            "exchange detected ({} frontier(s)){}",
                            s.frontiers.len(),
                            if s.native_store {
                                ""
                            } else {
                                " [no native store]"
                            }
                        );
                        for f in &s.frontiers {
                            println!(
                                "  {} source={} profile={} imported={} {}",
                                f.id,
                                f.source_state,
                                f.profile,
                                f.imported,
                                match f.binding {
                                    gemel::exchange::ingest::SourceBinding::Matched => "MATCHED",
                                    gemel::exchange::ingest::SourceBinding::Diverged => "DIVERGED",
                                }
                            );
                        }
                        if s.pending_export {
                            println!(
                                "pending export: exchange does not describe the current source"
                            );
                        }
                    }
                }
                Ok(0)
            }
            ExchangeCmd::Export { profile, json } => {
                let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
                let profile = gemel::exchange::export::Profile::parse(profile)?;
                let out = gemel::exchange::export::export(&repo, profile)?;
                if *json {
                    print_json(
                        "exchange export",
                        json!({
                            "profile": profile.as_str(),
                            "packs_written": out.packs_written,
                            "packs_reused": out.packs_reused,
                            "objects": out.objects,
                            "frontier": out.frontier,
                            "source_state": out.source_state.to_string(),
                        }),
                    );
                } else {
                    println!(
                        "exported {} objects ({} packs written, {} reused)",
                        out.objects, out.packs_written, out.packs_reused
                    );
                    println!("  frontier: {}", out.frontier);
                    println!("  source state: {}", out.source_state);
                }
                Ok(0)
            }
            ExchangeCmd::Ingest { json } => {
                let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
                let out = gemel::exchange::ingest::ingest(&repo)?;
                if *json {
                    print_json(
                        "exchange ingest",
                        json!({
                            "frontiers_found": out.frontiers_found,
                            "frontiers_imported": out.frontiers_imported,
                            "frontiers_already_imported": out.frontiers_already_imported,
                            "packs_processed": out.packs_processed,
                            "objects_promoted": out.objects_promoted,
                            "current_source_state": out.current_source_state.map(|g| g.to_string()),
                            "matching": out.matching,
                            "diverged": out.diverged,
                            "activated": out.activated,
                        }),
                    );
                } else {
                    println!(
                        "ingested {} frontier(s) ({} already imported), {} objects promoted",
                        out.frontiers_imported,
                        out.frontiers_already_imported,
                        out.objects_promoted
                    );
                    for m in &out.matching {
                        println!("  matched: {m}");
                    }
                    for d in &out.diverged {
                        println!("  diverged: {d}");
                    }
                    if let Some(a) = &out.activated {
                        println!("  activated: {a}");
                    }
                }
                Ok(0)
            }
            ExchangeCmd::Verify {
                working_tree,
                git_index,
                json,
            } => {
                let cwd = std::env::current_dir()?;
                let start = cli.repo.as_deref().unwrap_or(&cwd);
                let root = match discover_exchange_root(start)? {
                    Some(r) => r,
                    None => return Err(Error::NotARepository(start.to_path_buf())),
                };
                let mode = if *git_index {
                    gemel::exchange::ingest::VerifyMode::GitIndex
                } else {
                    let _ = working_tree;
                    gemel::exchange::ingest::VerifyMode::WorkingTree
                };
                let out = gemel::exchange::ingest::verify(&root, mode)?;
                if *json {
                    print_json(
                        "exchange verify",
                        json!({
                            "frontiers_validated": out.frontiers_validated,
                            "packs_validated": out.packs_validated,
                            "source_state": out.source_state.to_string(),
                            "staged": out.staged,
                            "matched": out.matched,
                            "diverged": out.diverged,
                        }),
                    );
                } else {
                    println!(
                        "validated {} frontier(s), {} pack(s) against {}",
                        out.frontiers_validated,
                        out.packs_validated,
                        if out.staged {
                            "git index"
                        } else {
                            "working tree"
                        }
                    );
                    for m in &out.matched {
                        println!("  matched: {m}");
                    }
                    for d in &out.diverged {
                        println!("  diverged: {d}");
                    }
                }
                Ok(0)
            }
        },
        Command::ExportGit {
            git_dir,
            branch,
            include_claims,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let target = git_dir.clone().unwrap_or_else(|| repo.root().join(".git"));
            let out = gemel::git_interop::export_git(
                &repo,
                &gemel::git_interop::ExportGitOptions {
                    git_dir: target,
                    branch: branch.clone(),
                    include_claims: *include_claims,
                },
            )?;
            if *json {
                print_json(
                    "export-git",
                    json!({
                        "commits": out.commits,
                        "trees": out.trees,
                        "mappings": out.mappings,
                        "head": out.head_oid,
                        "branch": out.branch,
                    }),
                );
            } else {
                println!(
                    "exported {} commit(s) ({}, {} trees, {} mappings)",
                    out.commits, out.branch, out.trees, out.mappings
                );
                println!("  head: {}", out.head_oid);
            }
            Ok(0)
        }
        Command::ImportGit {
            git_dir,
            head,
            json,
        } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let source = git_dir.clone().unwrap_or_else(|| repo.root().join(".git"));
            let out = gemel::git_interop::import_git(
                &repo,
                &gemel::git_interop::ImportGitOptions {
                    git_dir: source,
                    head: head.clone(),
                },
            )?;
            if *json {
                print_json(
                    "import-git",
                    json!({
                        "commits": out.commits,
                        "changes": out.changes,
                        "trajectories": out.trajectories,
                        "mappings": out.mappings,
                        "relinked": out.relinked,
                        "ignored_trailers": out.ignored_trailers,
                        "unknown_producers": out.unknown_producers,
                    }),
                );
            } else {
                println!(
                    "imported {} commit(s) as {} change(s) in {} trajectory(s), {} mapping(s)",
                    out.commits, out.changes, out.trajectories, out.mappings
                );
                if out.relinked > 0 {
                    println!(
                        "  relinked {} Gemel identity(ies) via trailers",
                        out.relinked
                    );
                }
                if out.ignored_trailers > 0 {
                    println!(
                        "  ignored {} hostile/unvalidated trailer(s)",
                        out.ignored_trailers
                    );
                }
            }
            Ok(0)
        }
        Command::Clone { url, dir, json } => {
            // git clone (argv-safe), then a native Gemel store + import.
            let target = dir.clone().unwrap_or_else(|| {
                let base = url.rsplit('/').next().unwrap_or(url).to_string();
                let base = base.strip_suffix(".git").unwrap_or(&base).to_string();
                std::env::current_dir()
                    .unwrap_or_else(|_| ".".into())
                    .join(base)
            });
            gemel::git_adapter::clone_repo(url, &target)?;
            let repo = Repo::init(&target, &InitOptions::default())?;
            gemel::exchange::export::install_local_gitignore(repo.meta_dir())?;
            let out = gemel::git_interop::import_git(
                &repo,
                &gemel::git_interop::ImportGitOptions {
                    git_dir: target.join(".git"),
                    head: "HEAD".into(),
                },
            )?;
            if *json {
                print_json(
                    "clone",
                    json!({
                        "directory": target.display().to_string(),
                        "commits": out.commits,
                        "changes": out.changes,
                        "trajectories": out.trajectories,
                    }),
                );
            } else {
                println!(
                    "cloned {} as {} change(s) in {} trajectory(s)",
                    target.display(),
                    out.changes,
                    out.trajectories
                );
            }
            Ok(0)
        }
        Command::Remote { cmd } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            match cmd {
                None => {
                    let cfg = gemel::sync::read_remotes(&repo)?;
                    if cfg.remotes.is_empty() {
                        println!("no remotes");
                    } else {
                        for (name, path) in &cfg.remotes {
                            println!("{name}\t{path}");
                        }
                    }
                    Ok(0)
                }
                Some(RemoteCmd::Add {
                    name,
                    path,
                    init,
                    json,
                }) => {
                    // Validate (and optionally initialize) the remote before
                    // recording it: a broken remote is never configured.
                    let _ = gemel::sync::FileTransport::open(Path::new(path), *init)?;
                    gemel::sync::add_remote(&repo, name, path)?;
                    if *json {
                        print_json(
                            "remote add",
                            json!({ "name": name, "path": path, "initialized": init }),
                        );
                    } else {
                        println!("{name} -> {path}");
                        if *init {
                            println!("  initialized remote repository");
                        }
                    }
                    Ok(0)
                }
                Some(RemoteCmd::Remove { name, json }) => {
                    gemel::sync::remove_remote(&repo, name)?;
                    if *json {
                        print_json("remote remove", json!({ "name": name }));
                    } else {
                        println!("removed remote {name}");
                    }
                    Ok(0)
                }
            }
        }
        Command::Fetch { remote, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let (name, path) = resolve_remote(&repo, remote)?;
            match open_sync_remote(&path)? {
                RemoteKind::Native(transport) => {
                    let out = gemel::sync::fetch(&repo, &name, &*transport)?;
                    if *json {
                        print_json(
                            "fetch",
                            json!({
                                "remote": out.remote,
                                "mode": "native",
                                "remote_refs": out.remote_refs,
                                "wanted": out.wanted,
                                "transferred": out.transferred,
                                "inserted": out.inserted,
                                "refs_written": out.refs_written,
                            }),
                        );
                    } else {
                        println!("fetched {} object(s) from {}", out.transferred, out.remote);
                        println!("  tracking refs under refs/remotes/{}", out.remote);
                    }
                    Ok(0)
                }
                RemoteKind::Git(git_dir) => {
                    // Git-only remote: deterministic import projection.
                    let out = gemel::git_interop::import_git(
                        &repo,
                        &gemel::git_interop::ImportGitOptions {
                            git_dir,
                            head: "HEAD".into(),
                        },
                    )?;
                    if *json {
                        print_json(
                            "fetch",
                            json!({
                                "remote": name,
                                "mode": "git",
                                "commits": out.commits,
                                "changes": out.changes,
                            }),
                        );
                    } else {
                        println!("fetched {} change(s) from Git remote {}", out.changes, name);
                    }
                    Ok(0)
                }
            }
        }
        Command::Push { remote, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let (_name, path) = resolve_remote(&repo, remote)?;
            match open_sync_remote(&path)? {
                RemoteKind::Native(transport) => {
                    let out = gemel::sync::push(&repo, &_name, &*transport)?;
                    if *json {
                        print_json(
                            "push",
                            json!({
                                "remote": out.remote,
                                "mode": "native",
                                "refs_pushed": out.refs_pushed,
                                "transferred": out.transferred,
                            }),
                        );
                    } else {
                        println!(
                            "pushed {} ref(s), {} object(s) to {}",
                            out.refs_pushed, out.transferred, out.remote
                        );
                    }
                    Ok(0)
                }
                RemoteKind::Git(git_dir) => {
                    // Git-only remote: deterministic export projection.
                    let out = gemel::git_interop::export_git(
                        &repo,
                        &gemel::git_interop::ExportGitOptions {
                            git_dir,
                            branch: "main".into(),
                            include_claims: false,
                        },
                    )?;
                    if *json {
                        print_json(
                            "push",
                            json!({
                                "remote": _name,
                                "mode": "git",
                                "commits": out.commits,
                            }),
                        );
                    } else {
                        println!("pushed {} commit(s) to Git remote {}", out.commits, _name);
                    }
                    Ok(0)
                }
            }
        }
        Command::Pull { remote, json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let (name, path) = resolve_remote(&repo, remote)?;
            match open_sync_remote(&path)? {
                RemoteKind::Native(transport) => {
                    let out = gemel::sync::pull(&repo, &name, &*transport)?;
                    if *json {
                        print_json(
                            "pull",
                            json!({
                                "remote": name,
                                "mode": "native",
                                "transferred": out.fetch.transferred,
                                "fast_forwarded": out.fast_forwarded,
                                "applied_refs": out.applied_refs,
                            }),
                        );
                    } else {
                        println!("pulled {} object(s) from {}", out.fetch.transferred, name);
                        if out.fast_forwarded {
                            println!("  fast-forwarded {} ref(s)", out.applied_refs);
                        }
                    }
                    Ok(0)
                }
                RemoteKind::Git(git_dir) => {
                    let out = gemel::git_interop::import_git(
                        &repo,
                        &gemel::git_interop::ImportGitOptions {
                            git_dir,
                            head: "HEAD".into(),
                        },
                    )?;
                    if *json {
                        print_json(
                            "pull",
                            json!({
                                "remote": name,
                                "mode": "git",
                                "commits": out.commits,
                                "changes": out.changes,
                            }),
                        );
                    } else {
                        println!("pulled {} change(s) from Git remote {}", out.changes, name);
                    }
                    Ok(0)
                }
            }
        }
        Command::Protocol { .. } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            gemel::protocol::run_session(&repo)?;
            Ok(0)
        }
        Command::Next { json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let plan = gemel::query::next_plan(&repo)?;
            if *json {
                print_json(
                    "next",
                    json!({
                        "intent": plan.intent.map(|g| g.to_string()),
                        "trajectory": plan.trajectory.as_ref().map(|(n, g)| json!({ "name": n, "id": g.to_string() })),
                        "state": plan.state.map(|g| g.to_string()),
                        "recommendations": plan.recommendations.iter().map(|r| json!({
                            "kind": r.kind,
                            "subject": r.subject,
                            "refs": r.refs.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                            "rationale": r.rationale,
                            "certainty": r.certainty,
                        })).collect::<Vec<_>>(),
                        "uncertainty": plan.uncertainty,
                    }),
                );
            } else {
                for r in &plan.recommendations {
                    let subject = r.subject.clone().unwrap_or_default();
                    println!("{} {}  ({})", r.kind, subject, r.certainty);
                    println!("    {}", r.rationale);
                    for g in &r.refs {
                        println!("    ref: {g}");
                    }
                }
            }
            Ok(0)
        }
        Command::Policy { json } => {
            let repo = Repo::find(cli.repo.as_deref().unwrap_or(&std::env::current_dir()?))?;
            let matrix = gemel::query::required_verification(&repo)?;
            let gaps = gemel::query::required_verification_gaps(&repo)?;
            if *json {
                print_json(
                    "policy",
                    json!({
                        "required_verification": matrix.iter().map(|(k, p, a)| json!({ "kind": k, "platform": p, "arch": a })).collect::<Vec<_>>(),
                        "gaps": gaps.iter().map(|(k, r)| json!({ "kind": k, "rationale": r })).collect::<Vec<_>>(),
                    }),
                );
            } else {
                if matrix.is_empty() {
                    println!("no required-verification matrix configured");
                } else {
                    for (k, p, a) in &matrix {
                        println!("required: {k} on {p}/{a}");
                    }
                }
                for (_, r) in &gaps {
                    println!("gap: {r}");
                }
                if gaps.is_empty() && !matrix.is_empty() {
                    println!("all required verification present");
                }
            }
            Ok(0)
        }
    }
}

/// Resolves a remote argument: a configured remote name, or a direct path.
fn resolve_remote(repo: &Repo, arg: &str) -> Result<(String, PathBuf), Error> {
    if let Ok(path) = gemel::sync::remote_path(repo, arg) {
        return Ok((arg.to_string(), path));
    }
    let path = PathBuf::from(arg);
    Ok((arg.to_string(), path))
}

/// The sync target behind a path: a native Gemel repository or a Git-only
/// repository (GIT_INTEROP.md §6: push/pull serve both).
enum RemoteKind {
    Native(Box<dyn gemel::sync::Transport>),
    Git(PathBuf),
}

fn open_sync_remote(path: &Path) -> Result<RemoteKind, Error> {
    if path
        .join(gemel::store::META_DIR)
        .join("meta.json")
        .is_file()
    {
        return Ok(RemoteKind::Native(Box::new(
            gemel::sync::FileTransport::open(path, false)?,
        )));
    }
    let git_dir = if path.join(".git").is_dir() {
        Some(path.join(".git"))
    } else if path.join("HEAD").is_file() && path.join("objects").is_dir() {
        Some(path.to_path_buf()) // bare repository
    } else {
        None
    };
    match git_dir {
        Some(g) => Ok(RemoteKind::Git(g)),
        None => Err(Error::Invalid(format!(
            "{} is neither a gemel repository nor a git repository",
            path.display()
        ))),
    }
}
/// Walks up from `start` looking for `.gemel/` (native store) or
/// `.gemel/exchange/` (exchange material only; EXCHANGE.md §34).
fn discover_exchange_root(start: &Path) -> Result<Option<PathBuf>, Error> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let meta = d.join(gemel::store::META_DIR);
        if meta.is_dir() {
            return Ok(Some(d.to_path_buf()));
        }
        if meta.join(gemel::exchange::EXCHANGE_ROOT).is_dir() {
            return Ok(Some(d.to_path_buf()));
        }
        dir = d.parent();
    }
    Ok(None)
}

/// The exchange diagnostics block for `status --json` (EXCHANGE.md §32).
fn exchange_json(
    repo: &Repo,
    exchange: bool,
    bootstrapped: bool,
) -> Result<serde_json::Value, Error> {
    if !exchange {
        return Ok(json!({
            "detected": false,
        }));
    }
    let frontiers = gemel::exchange::discover_frontiers(repo.meta_dir())?;
    let active = gemel::exchange::export::read_active_frontier(repo.meta_dir())?;
    // Source binding is against the checked-out working tree (EXCHANGE.md
    // §19): the frontier describes the current source only when the live
    // tree's content state equals the frontier's source_state. The head
    // state is never the binding (a git-only edit leaves the head state
    // unchanged while the source diverges).
    let source_match = gemel::exchange::export::working_tree_files(repo)
        .ok()
        .and_then(|files| gemel::exchange::export::content_state_identity(repo, &files).ok())
        .map(|content_id| {
            frontiers
                .iter()
                .any(|(f, _, _)| f.source_state == content_id)
        })
        .unwrap_or(false);
    Ok(json!({
        "detected": true,
        "frontier": active.map(|a| gemel::hex::encode(&a)),
        "source_match": source_match,
        "bootstrapped": bootstrapped,
        "coverage": {
            "canonical_metadata": "complete",
            "deep_evidence": "partial",
        },
    }))
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
        gemel::family::Family::SemanticEntity => {
            println!("semantic entity {}", gid);
            println!(
                "  {} {}::{}\n  {}:{}..{}  {}",
                sfield(0x01),
                sfield(0x03),
                sfield(0x02),
                sfield(0x04),
                gemel::query::u64_field(fs, 0x05).unwrap_or(0),
                gemel::query::u64_field(fs, 0x06).unwrap_or(0),
                sfield(0x07)
            );
            if let Some(from) = gfield(0x0A) {
                println!("  lineage from {from} ({} {})", sfield(0x0B), sfield(0x0C));
            }
        }
        gemel::family::Family::SemanticIndex => {
            println!("semantic index {}", gid);
            if let Some(s) = gfield(0x01) {
                println!("  state: {s}");
            }
            println!("  entities: {}", glist(0x02).len());
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
        "semantic": report.semantic.as_ref().map(entity_json),
        "introduced_by": introduced,
        "last_modified": report.last_modified.map(|g| g.to_string()),
        "previous_approaches": report.previous_approaches.iter().map(attempt_json).collect::<Vec<_>>(),
    })
}

/// A semantic entity as JSON (Phase 5; AGENT_PROTOCOL.md §5.9).
fn entity_json(e: &gemel::semantic::EntityInfo) -> serde_json::Value {
    json!({
        "id": e.id.map(|g| g.to_string()),
        "kind": e.kind,
        "name": e.name,
        "module_path": e.module_path,
        "full_path": e.full_path(),
        "file_path": e.file_path,
        "start_line": e.start_line,
        "end_line": e.end_line,
        "signature": e.signature,
        "visibility": e.visibility,
        "lineage": e.lineage.as_ref().map(|(from, evidence, certainty)| json!({
            "from": from.to_string(),
            "evidence": evidence,
            "certainty": certainty,
        })),
        "state": e.state.to_string(),
    })
}

/// Human rendering of a semantic entity.
fn render_entity(e: &gemel::semantic::EntityInfo) {
    println!("{} {}", e.kind, e.full_path());
    if !e.file_path.is_empty() {
        println!("  file: {}:{}:{}", e.file_path, e.start_line, e.end_line);
    }
    if !e.signature.is_empty() {
        println!("  signature: {}", e.signature);
    }
    println!("  visibility: {}", e.visibility);
    if let Some((from, evidence, certainty)) = &e.lineage {
        println!("  lineage: from {from} ({certainty}; {evidence})");
    }
    if let Some(id) = e.id {
        println!("  id: {id}");
    }
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
    if let Some(e) = &report.semantic {
        println!("entity: {} ({})", e.full_path(), e.kind);
        if let Some((from, evidence, certainty)) = &e.lineage {
            println!("  lineage: from {from} ({certainty}; {evidence})");
        }
    }
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
