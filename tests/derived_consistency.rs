//! Derived-consistency invariants (INVARIANTS DER-01).
//!
//! The SQLite index is a disposable accelerator, never an oracle: deleting
//! it must not change a single semantic answer. Every derived query has a
//! canonical slow path; this suite proves
//!
//!     query(repo_with_index) == query(repo_after_delete_index)
//!
//! for every derived query, by computing a canonical digest of all derived
//! answers before and after deleting the index database.

// Test closures return the rich store error type; boxed variants would
// obscure construction sites for no measurable gain.
#![allow(clippy::result_large_err)]

use gemel::query::{self, IncludeFlags};
use gemel::store::{InitOptions, Repo};
use gemel::value::{Field, Object, Value};
use gemel::workflow::{
    self, BeginOptions, ClaimSpec, CloseTrajectoryOptions, EvidenceSpec, FinishOptions,
    ResidualSpec, ResolveResidualOptions,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gemel-der-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A repository exercising every append-chain and derived-status feature.
fn seed(root: &Path) -> Repo {
    write_file(root, "parser.rs", "v1\n");
    let repo = Repo::init(root, &InitOptions::default()).unwrap();
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent_summary: Some("Parser compatibility".into()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(root, "parser.rs", "v2\n");
    let f = workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "parser rewrite".into(),
            claims: vec![ClaimSpec {
                subject: Some("parser.rs".into()),
                predicate: "parser now matches upstream".into(),
                kind: "compatibility".into(),
                evidence: vec![0, 1],
            }],
            evidence: vec![
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "pass".into(),
                    kind: "court_receipt".into(),
                },
                EvidenceSpec {
                    subject: Some("parser.rs".into()),
                    outcome: "mismatch".into(),
                    kind: "oracle_comparison".into(),
                },
            ],
            residuals: vec![ResidualSpec {
                summary: "FreeBSD diverges".into(),
                severity: "high".into(),
                classification: "platform_divergence".into(),
                affected_claims: vec![0],
                origin_evidence: Some(1),
                affected_changes: Vec::new(),
            }],
            ..Default::default()
        },
    )
    .unwrap();

    // Chains: resolve the residual twice, close the trajectory.
    let _ = workflow::resolve_residual(
        &repo,
        &ResolveResidualOptions {
            residual: f.residuals[0].to_string(),
            disposition: "acknowledged".into(),
            reason: Some("tracked".into()),
            producer: None,
        },
    )
    .unwrap();
    let _ = workflow::resolve_residual(
        &repo,
        &ResolveResidualOptions {
            residual: f.residuals[0].to_string(),
            disposition: "resolved".into(),
            reason: Some("fixed".into()),
            producer: None,
        },
    )
    .unwrap();
    workflow::close_trajectory(
        &repo,
        &CloseTrajectoryOptions {
            trajectory: "T1".into(),
            outcome: "rejected".into(),
            reason: Some("FreeBSD diverged".into()),
            producer: None,
        },
    )
    .unwrap();

    // A superseding claim (canonical; supersedes edge in the index).
    let claim = f.claims[0];
    let cobj = repo.load(&claim).unwrap();
    let cfs = cobj.field_sequence().unwrap();
    let mut fields: Vec<Field> = cfs.to_vec();
    fields.retain(|fld| fld.tag != 0x0B);
    fields.push(Field::new(0x0B, Value::Gid(claim)));
    fields.sort_by_key(|fld| fld.tag);
    let _ = repo.insert_object(&Object::fields(gemel::family::Family::Claim, fields));

    // A second trajectory (for attempts/why richness).
    workflow::begin_change(
        &repo,
        &BeginOptions {
            intent: Some(repo.resolve("I1").unwrap()),
            ..Default::default()
        },
    )
    .unwrap();
    write_file(root, "parser.rs", "v3\n");
    workflow::finish_change(
        &repo,
        &FinishOptions {
            summary: "followup".into(),
            ..Default::default()
        },
    )
    .unwrap();
    repo
}

/// A canonical digest of every derived answer. Must be byte-identical with
/// and without the derived index.
fn derived_digest(repo: &Repo) -> String {
    let mut parts: Vec<String> = Vec::new();

    // chain_latest for every chained object we can reach.
    for name in ["T1", "T2"] {
        if let Ok(t) = repo.resolve(name) {
            parts.push(format!(
                "chain({name})={}",
                query::chain_latest(repo, &t).unwrap()
            ));
        }
    }
    // claim status + supersession for every claim.
    let (claims, _) = query::claims(repo, &query::ClaimsFilter::default()).unwrap();
    for row in &claims {
        let (status, sup, con) = query::claim_status(repo, &row.gid).unwrap();
        parts.push(format!(
            "claim({})={}:{}:{}:{}",
            row.gid,
            status.as_str(),
            ids(&sup),
            ids(&con),
            row.subject.clone().unwrap_or_default()
        ));
    }
    // residuals: disposition + persistence + links.
    let (residuals, _) = query::residuals(repo, &query::ResidualsFilter::default()).unwrap();
    for r in &residuals {
        parts.push(format!(
            "residual({})={}:{}:{}:{}",
            r.gid,
            r.disposition,
            r.persistence,
            ids(&r.affected_claims),
            ids(&r.affected_changes)
        ));
    }
    // why on the subject.
    let why = query::why(repo, "parser.rs").unwrap();
    let node = why.introduced_by.as_ref().map(|n| {
        format!(
            "why(change={},claim={},evidence=[{}],residuals=[{}])",
            n.change,
            n.claim
                .as_ref()
                .map(|c| c.id.to_string())
                .unwrap_or_default(),
            n.evidence
                .iter()
                .map(|e| format!("{}:{}", e.id, e.outcome.clone().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(","),
            n.residuals
                .iter()
                .map(|r| format!("{}:{}", r.id, r.disposition))
                .collect::<Vec<_>>()
                .join(",")
        )
    });
    parts.push(node.unwrap_or_else(|| "why(none)".into()));
    parts.push(format!(
        "why_attempts={}",
        why.previous_approaches
            .iter()
            .map(|a| format!(
                "{}:{}",
                a.name.clone().unwrap_or_default(),
                a.outcome.clone().unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join(",")
    ));
    // attempts.
    parts.push(format!(
        "attempts={}",
        query::attempts(repo, "parser.rs")
            .unwrap()
            .iter()
            .map(|a| format!("{}:{}", a.trajectory, a.outcome.clone().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(",")
    ));
    // trajectory detail (sequence + handoff + accumulated lists).
    for name in ["T1", "T2"] {
        if let Ok(d) = query::trajectory_detail(repo, name) {
            parts.push(format!(
                "traj({})={}:seq=[{}]:ev=[{}]:res=[{}]",
                name,
                d.outcome.clone().unwrap_or_default(),
                d.sequence
                    .iter()
                    .map(|c| c.change.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                ids(&d.evidence),
                ids(&d.residuals)
            ));
        }
    }
    // evidence freshness.
    let mut ev_seen: std::collections::HashSet<gemel::gid::Gid> = Default::default();
    for (_, latest) in query::all_trajectories(repo).unwrap() {
        for (_, tobj) in query::trajectory_versions(repo, &latest).unwrap() {
            let fs = tobj.field_sequence().unwrap_or(&[]);
            for c in query::gid_list(fs, 0x06) {
                if let Ok(cobj) = repo.load(&c) {
                    let cfs = cobj.field_sequence().unwrap_or(&[]);
                    for ev in query::gid_list(cfs, 0x0D) {
                        if ev_seen.insert(ev) {
                            parts.push(format!(
                                "evidence({})={}",
                                ev,
                                query::evidence_freshness(repo, &ev).unwrap().as_str()
                            ));
                        }
                    }
                }
            }
        }
    }
    // context bundle.
    let bundle = query::context_bundle(
        repo,
        "parser.rs",
        Some("I1"),
        8192,
        IncludeFlags::parse("").unwrap(),
    )
    .unwrap();
    parts.push(format!(
        "bundle={}:consumed={}:expanded={}/{}/{}/{}",
        bundle
            .items
            .iter()
            .map(|i| format!("{}:{}:{}", i.id, i.family.short(), i.level))
            .collect::<Vec<_>>()
            .join(","),
        bundle.consumed,
        bundle.expanded.claims,
        bundle.expanded.residuals,
        bundle.expanded.attempts,
        bundle.expanded.evidence
    ));

    parts.sort();
    parts.join("\n")
}

fn ids(gids: &[gemel::gid::Gid]) -> String {
    gids.iter()
        .map(|g| g.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn derived_queries_are_index_independent() {
    let root = temp_root("consistency");
    let repo = seed(&root);

    // With the derived index (fresh).
    let with_index = derived_digest(&repo);
    assert!(repo.index_is_fresh(), "index should be fresh after seeding");

    // Delete the index database: derived queries must answer identically.
    gemel::store::index::remove(&repo).unwrap();
    assert!(!repo.index_is_fresh(), "deleted index is never fresh");

    let without_index = derived_digest(&repo);
    assert_eq!(
        with_index, without_index,
        "deleting the derived index must not change a single semantic answer"
    );

    // Rebuild the index: answers still identical (and fsck is clean).
    repo.rebuild_index().unwrap();
    let after_rebuild = derived_digest(&repo);
    assert_eq!(with_index, after_rebuild);
    let report = repo
        .fsck(&gemel::store::fsck::FsckOptions::default())
        .unwrap();
    assert!(report.is_clean(), "fsck problems: {:?}", report.problems);
}
