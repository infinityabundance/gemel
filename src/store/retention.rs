//! Retention tiers (STORAGE.md §7).
//!
//! Phase 1 implements the tier-attribution foundation: the default tier of
//! every family and the graph-aware adjustment for evidence-referenced blobs.
//! The GC pass itself (pruning under policy, audit) is a Phase 2+ feature;
//! tombstones and the `missing` vs `pruned` distinction are already live.

use crate::family::Family;

/// Retention tiers (brief §27, STORAGE.md §7.1).
pub const TIER_0_CANONICAL: u8 = 0;
pub const TIER_1_REPRODUCIBILITY: u8 = 1;
pub const TIER_2_DEVELOPMENTAL: u8 = 2;
pub const TIER_3_FORENSIC: u8 = 3;

/// The default tier of a family (STORAGE.md §7.1 table).
pub fn default_tier(family: Family) -> u8 {
    match family {
        Family::Blob
        | Family::Tree
        | Family::State
        | Family::Operation
        | Family::Episode
        | Family::Intent
        | Family::Change
        | Family::Case
        | Family::Trajectory
        | Family::Claim
        | Family::Residual
        | Family::Verification
        | Family::Producer
        | Family::AgentRun
        | Family::Environment
        | Family::Reconciliation
        | Family::Release
        | Family::ContextManifest
        | Family::Checkpoint
        | Family::Config
        | Family::Mapping => TIER_0_CANONICAL,
        Family::Evidence => TIER_0_CANONICAL, // evidence *identities* are canonical
    }
}

/// Adjusts a blob's tier based on what references it (STORAGE.md §7.1):
/// blobs referenced by evidence (fixtures, tool outputs, oracle inputs, logs)
/// are Tier 1 reproducibility material.
pub fn adjusted_blob_tier(evidence_referenced: bool) -> u8 {
    if evidence_referenced {
        TIER_1_REPRODUCIBILITY
    } else {
        TIER_0_CANONICAL
    }
}

/// The default retention policy record used by the initial repository config.
pub fn default_retention_record() -> serde_json::Value {
    serde_json::json!({
        "tiers": [
            { "tier": 0, "policy": "retain_forever" },
            { "tier": 1, "policy": "retain_policy" },
            { "tier": 2, "policy": "prune_after_days", "days": 90 },
            { "tier": 3, "policy": "prune_after_days", "days": 14 },
        ],
        "default_unknown": "retain",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_families_are_canonical_tier() {
        for f in Family::ALL {
            assert_eq!(default_tier(f), TIER_0_CANONICAL, "{f}");
        }
    }

    #[test]
    fn evidence_referenced_blobs_are_tier1() {
        assert_eq!(adjusted_blob_tier(true), TIER_1_REPRODUCIBILITY);
        assert_eq!(adjusted_blob_tier(false), TIER_0_CANONICAL);
    }
}
