//! Normative per-family schema tables (OBJECT_MODEL.md §6).
//!
//! These tables are the machine-readable form of the specification. The
//! encoder, decoder, and validator are driven exclusively by these tables;
//! the golden fixtures pin their bytes. Any change here is a protocol change.

use gemel_core::family::Family;

/// The schema type of a field value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// Unsigned integer.
    U64,
    /// Signed integer.
    I64,
    /// Boolean.
    Bool,
    /// Raw bytes.
    Bytes,
    /// UTF-8 string (byte-identical, never normalized).
    Str,
    /// A canonical repository-relative path (OBJECT_MODEL.md §1.6).
    Path,
    /// A string validated against a fixed value set (fail closed on unknown).
    Enum(&'static [&'static str]),
    /// A reference to an object of a specific family.
    Gid(Family),
    /// A reference to an object of any family.
    GidAny,
    /// A nested canonical field sequence governed by the given spec.
    Record(&'static [FieldSpec]),
    /// An array of a homogeneous element type.
    Array(&'static Type),
}

/// A single field declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpec {
    pub tag: u8,
    pub name: &'static str,
    pub ty: Type,
    pub required: bool,
}

impl FieldSpec {
    pub const fn new(tag: u8, name: &'static str, ty: Type, required: bool) -> FieldSpec {
        FieldSpec {
            tag,
            name,
            ty,
            required,
        }
    }
}

/// The schema of one family at one schema version.
#[derive(Debug, Clone, Copy)]
pub struct FamilySchema {
    pub family: Family,
    pub schemever: u8,
    pub extensions_allowed: bool,
    pub fields: &'static [FieldSpec],
}

impl FamilySchema {
    /// Looks up a field declaration by tag.
    pub fn field(&self, tag: u8) -> Option<&'static FieldSpec> {
        self.fields.iter().find(|f| f.tag == tag)
    }

    /// Looks up a field declaration by name.
    pub fn field_by_name(&self, name: &str) -> Option<&'static FieldSpec> {
        self.fields.iter().find(|f| f.name == name)
    }
}

// ---------------------------------------------------------------------------
// Enum value sets (normative; extending one requires a schemever bump).
// ---------------------------------------------------------------------------

pub static ENUM_DISCLOSURE: &[&str] = &[
    "FULL",
    "DIGEST_ONLY",
    "REDACTED",
    "EXTERNAL_ATTESTATION",
    "EPHEMERAL",
];
pub static ENUM_OP_TYPE: &[&str] = &[
    "create_file",
    "write_file",
    "write_range",
    "delete_file",
    "rename_path",
    "apply_patch",
    "exec_command",
    "run_test",
    "invoke_oracle",
    "inspect_artifact",
    "ast_transform",
    "other",
];
pub static ENUM_RESULT_STATUS: &[&str] = &["ok", "failed", "partial", "skipped", "inconclusive"];
pub static ENUM_EPISODE_OUTCOME: &[&str] = &["completed", "interrupted", "aborted", "inconclusive"];
pub static ENUM_CASE_STATUS: &[&str] = &["open", "active", "closed", "abandoned"];
pub static ENUM_TRAJECTORY_OUTCOME: &[&str] = &[
    "completed",
    "abandoned",
    "superseded",
    "rejected",
    "inconclusive",
    "interrupted",
];
pub static ENUM_CLAIM_KIND: &[&str] = &[
    "compatibility",
    "correctness",
    "performance",
    "security",
    "safety",
    "invariant",
    "behavior",
    "other",
];
pub static ENUM_EVIDENCE_KIND: &[&str] = &[
    "test_result",
    "compiler_result",
    "fuzz_result",
    "benchmark",
    "oracle_comparison",
    "static_analysis",
    "formal_proof",
    "binary_comparison",
    "runtime_trace",
    "replay",
    "environment_manifest",
    "artifact_hash",
    "external_attestation",
    "court_receipt",
];
pub static ENUM_EVIDENCE_OUTCOME: &[&str] = &[
    "pass",
    "fail",
    "mismatch",
    "inconclusive",
    "error",
    "skipped",
];
pub static ENUM_RESIDUAL_CLASS: &[&str] = &[
    "semantic_divergence",
    "expected_mismatch",
    "platform_divergence",
    "performance_divergence",
    "unexplained_divergence",
    "contract_mismatch",
    "verification_gap",
    "other",
];
pub static ENUM_RESIDUAL_SEVERITY: &[&str] = &["low", "medium", "high", "blocking"];
pub static ENUM_DISPOSITION: &[&str] = &[
    "open",
    "acknowledged",
    "resolved",
    "superseded",
    "irrelevant",
];
pub static ENUM_VERIFY_RESULT: &[&str] = &["pass", "partial", "fail", "inconclusive", "not_run"];
pub static ENUM_PRODUCER_KIND: &[&str] = &[
    "human",
    "agent",
    "automation",
    "compiler",
    "fuzzer",
    "external_oracle",
    "git_import",
    "unknown",
];
pub static ENUM_NETWORK: &[&str] = &["none", "restricted", "full", "unknown"];
pub static ENUM_DETERMINISM: &[&str] = &[
    "fully_deterministic",
    "reproducible_with_fixture",
    "best_effort",
    "unknown",
];
pub static ENUM_INTERACTION_KIND: &[&str] = &[
    "textual",
    "semantic",
    "claim",
    "invariant",
    "dependency",
    "behavioral",
    "verification",
];
pub static ENUM_CERTAINTY: &[&str] = &["observed", "possible", "unknown"];
pub static ENUM_MAPPING_KIND: &[&str] = &["git_commit", "git_tree", "external"];
pub static ENUM_RETENTION_POLICY: &[&str] = &[
    "retain_forever",
    "retain_policy",
    "prune_after_days",
    "size_limit_bytes",
    "archive_remote",
];
pub static ENUM_DEFAULT_UNKNOWN: &[&str] = &["retain", "prune", "archive"];
pub static ENUM_EXEC_POLICY: &[&str] = &["never_auto_execute", "policy_gated", "allowlist"];

// ---------------------------------------------------------------------------
// Reusable single-field types for arrays.
// ---------------------------------------------------------------------------

pub static TY_STR: Type = Type::Str;
pub static TY_U64: Type = Type::U64;
pub static TY_I64: Type = Type::I64;
pub static TY_GID_ANY: Type = Type::GidAny;
pub static TY_BLOB: Type = Type::Gid(Family::Blob);
pub static TY_TREE: Type = Type::Gid(Family::Tree);
pub static TY_STATE: Type = Type::Gid(Family::State);
pub static TY_OPERATION: Type = Type::Gid(Family::Operation);
pub static TY_EPISODE: Type = Type::Gid(Family::Episode);
pub static TY_INTENT: Type = Type::Gid(Family::Intent);
pub static TY_CHANGE: Type = Type::Gid(Family::Change);
pub static TY_CASE: Type = Type::Gid(Family::Case);
pub static TY_TRAJECTORY: Type = Type::Gid(Family::Trajectory);
pub static TY_CLAIM: Type = Type::Gid(Family::Claim);
pub static TY_EVIDENCE: Type = Type::Gid(Family::Evidence);
pub static TY_RESIDUAL: Type = Type::Gid(Family::Residual);
pub static TY_VERIFICATION: Type = Type::Gid(Family::Verification);
pub static TY_PRODUCER: Type = Type::Gid(Family::Producer);
pub static TY_AGENTRUN: Type = Type::Gid(Family::AgentRun);
pub static TY_ENVIRONMENT: Type = Type::Gid(Family::Environment);
pub static TY_RECONCILIATION: Type = Type::Gid(Family::Reconciliation);
pub static TY_RELEASE: Type = Type::Gid(Family::Release);
pub static TY_CONTEXT_MANIFEST: Type = Type::Gid(Family::ContextManifest);
pub static TY_CHECKPOINT: Type = Type::Gid(Family::Checkpoint);
pub static TY_CONFIG: Type = Type::Gid(Family::Config);
pub static TY_MAPPING: Type = Type::Gid(Family::Mapping);

pub static TY_ARR_STR: Type = Type::Array(&TY_STR);
pub static TY_ARR_GID: Type = Type::Array(&TY_GID_ANY);
pub static TY_ARR_BLOB: Type = Type::Array(&TY_BLOB);
pub static TY_ARR_STATE: Type = Type::Array(&TY_STATE);
pub static TY_ARR_OPERATION: Type = Type::Array(&TY_OPERATION);
pub static TY_ARR_EPISODE: Type = Type::Array(&TY_EPISODE);
pub static TY_ARR_CHANGE: Type = Type::Array(&TY_CHANGE);
pub static TY_ARR_TRAJECTORY: Type = Type::Array(&TY_TRAJECTORY);
pub static TY_ARR_CLAIM: Type = Type::Array(&TY_CLAIM);
pub static TY_ARR_EVIDENCE: Type = Type::Array(&TY_EVIDENCE);
pub static TY_ARR_RESIDUAL: Type = Type::Array(&TY_RESIDUAL);
pub static TY_ARR_VERIFICATION: Type = Type::Array(&TY_VERIFICATION);
pub static TY_ARR_CASE: Type = Type::Array(&TY_CASE);
pub static TY_ARR_RELEASE: Type = Type::Array(&TY_RELEASE);
pub static TY_ARR_CONFIG: Type = Type::Array(&TY_CONFIG);

// ---------------------------------------------------------------------------
// Nested record specs.
// ---------------------------------------------------------------------------

/// tree entry (OBJECT_MODEL.md §6.2).
pub static ENTRY_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "mode", Type::U64, true),
    FieldSpec::new(0x03, "target", Type::GidAny, true),
];
pub static TY_REC_ENTRY: Type = Type::Record(&ENTRY_SPEC);

/// operation result (OBJECT_MODEL.md §6.4).
pub static RESULT_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "status", Type::Enum(ENUM_RESULT_STATUS), true),
    FieldSpec::new(0x02, "detail", Type::Str, false),
    FieldSpec::new(0x03, "exit_code", Type::I64, false),
    FieldSpec::new(0x04, "refs", Type::Array(&TY_GID_ANY), false),
];

/// intent external reference (OBJECT_MODEL.md §6.6).
pub static EXT_REF_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "uri", Type::Str, true),
    FieldSpec::new(0x03, "digest", Type::Bytes, false),
];
pub static TY_REC_EXT_REF: Type = Type::Record(&EXT_REF_SPEC);

/// trajectory handoff (OBJECT_MODEL.md §6.9).
pub static HANDOFF_SPEC: [FieldSpec; 7] = [
    FieldSpec::new(0x01, "summary", Type::Str, false),
    FieldSpec::new(0x02, "completed", Type::Array(&TY_STR), false),
    FieldSpec::new(0x03, "remaining", Type::Array(&TY_STR), false),
    FieldSpec::new(0x04, "open_residuals", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x05, "important_evidence", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x06, "recommended_objects", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x07, "next_steps", Type::Array(&TY_STR), false),
];

/// tool identity: name/version/digest.
pub static TOOL_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, true),
    FieldSpec::new(0x03, "digest", Type::Bytes, false),
];
pub static TY_REC_TOOL: Type = Type::Record(&TOOL_SPEC);

/// normalizer/comparator identity: name/digest.
pub static NC_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "digest", Type::Bytes, true),
];
pub static TY_REC_NC: Type = Type::Record(&NC_SPEC);

/// evidence result counts (OBJECT_MODEL.md §6.11).
pub static COUNTS_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "passed", Type::U64, true),
    FieldSpec::new(0x02, "failed", Type::U64, true),
    FieldSpec::new(0x03, "skipped", Type::U64, true),
    FieldSpec::new(0x04, "total", Type::U64, true),
];
pub static TY_REC_COUNTS: Type = Type::Record(&COUNTS_SPEC);

/// evidence result (OBJECT_MODEL.md §6.11).
pub static EVIDENCE_RESULT_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "outcome", Type::Enum(ENUM_EVIDENCE_OUTCOME), true),
    FieldSpec::new(0x02, "detail", Type::Str, false),
    FieldSpec::new(0x03, "exit_code", Type::I64, false),
    FieldSpec::new(0x04, "counts", Type::Record(&COUNTS_SPEC), false),
];
pub static TY_REC_EVIDENCE_RESULT: Type = Type::Record(&EVIDENCE_RESULT_SPEC);

/// evidence reproduction info (OBJECT_MODEL.md §6.11).
pub static REPRO_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "replayable", Type::Bool, false),
    FieldSpec::new(0x02, "inputs_present", Type::Bool, false),
    FieldSpec::new(0x03, "inputs_remote", Type::Bool, false),
    FieldSpec::new(0x04, "policy_required", Type::Bool, false),
];
pub static TY_REC_REPRO: Type = Type::Record(&REPRO_SPEC);

/// residual scope (OBJECT_MODEL.md §6.12).
pub static RESIDUAL_SCOPE_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x02, "trajectories", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x03, "paths", Type::Array(&TY_STR), false),
    FieldSpec::new(0x04, "entities", Type::Array(&TY_GID_ANY), false),
];
pub static TY_REC_RESIDUAL_SCOPE: Type = Type::Record(&RESIDUAL_SCOPE_SPEC);

/// residual disposition event (OBJECT_MODEL.md §6.12).
pub static DISPOSITION_EVENT_SPEC: [FieldSpec; 6] = [
    FieldSpec::new(0x01, "disposition", Type::Enum(ENUM_DISPOSITION), true),
    FieldSpec::new(0x02, "by", Type::Gid(Family::Producer), true),
    FieldSpec::new(0x03, "evidence", Type::Gid(Family::Evidence), false),
    FieldSpec::new(
        0x04,
        "reconciliation",
        Type::Gid(Family::Reconciliation),
        false,
    ),
    FieldSpec::new(0x05, "reason", Type::Str, false),
    FieldSpec::new(0x06, "at", Type::I64, false),
];

/// verification platform: os/arch/variant.
pub static PLATFORM_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "os", Type::Str, true),
    FieldSpec::new(0x02, "arch", Type::Str, true),
    FieldSpec::new(0x03, "variant", Type::Str, false),
];
pub static TY_REC_PLATFORM: Type = Type::Record(&PLATFORM_SPEC);

/// verification tool version: name/version.
pub static TV_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, true),
];
pub static TY_REC_TV: Type = Type::Record(&TV_SPEC);

/// verification scope (OBJECT_MODEL.md §6.13).
pub static VERIFY_SCOPE_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "platforms", Type::Array(&TY_REC_PLATFORM), false),
    FieldSpec::new(0x02, "configs", Type::Array(&TY_STR), false),
    FieldSpec::new(0x03, "tool_versions", Type::Array(&TY_REC_TV), false),
];
pub static TY_REC_VERIFY_SCOPE: Type = Type::Record(&VERIFY_SCOPE_SPEC);

/// producer human identity.
pub static HUMAN_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "email", Type::Str, false),
];

/// producer agent identity.
pub static AGENT_ID_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "model_family", Type::Str, false),
    FieldSpec::new(0x02, "model_id", Type::Str, false),
    FieldSpec::new(0x03, "harness", Type::Str, false),
    FieldSpec::new(0x04, "permissions", Type::Array(&TY_STR), false),
];

/// producer automation identity.
pub static AUTOMATION_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "system", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, false),
];

/// producer identity (kind-conditional; OBJECT_MODEL.md §6.14).
pub static PRODUCER_IDENTITY_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "human", Type::Record(&HUMAN_SPEC), false),
    FieldSpec::new(0x02, "agent", Type::Record(&AGENT_ID_SPEC), false),
    FieldSpec::new(0x03, "automation", Type::Record(&AUTOMATION_SPEC), false),
];
pub static TY_REC_PRODUCER_IDENTITY: Type = Type::Record(&PRODUCER_IDENTITY_SPEC);

/// environment OS record.
pub static OS_SPEC: [FieldSpec; 4] = [
    FieldSpec::new(0x01, "family", Type::Str, true),
    FieldSpec::new(0x02, "name", Type::Str, true),
    FieldSpec::new(0x03, "version", Type::Str, false),
    FieldSpec::new(0x04, "kernel", Type::Str, false),
];
pub static TY_REC_OS: Type = Type::Record(&OS_SPEC);

/// name/version pair (compiler, runtime).
pub static NV_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, false),
];
pub static TY_REC_NV: Type = Type::Record(&NV_SPEC);

/// toolchain record: name/version/digest.
pub static TC_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, false),
    FieldSpec::new(0x03, "digest", Type::Bytes, false),
];
pub static TY_REC_TC: Type = Type::Record(&TC_SPEC);

/// hardware record: cpu/cores/memory_bytes.
pub static HW_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "cpu", Type::Str, true),
    FieldSpec::new(0x02, "cores", Type::U64, true),
    FieldSpec::new(0x03, "memory_bytes", Type::U64, true),
];
pub static TY_REC_HW: Type = Type::Record(&HW_SPEC);

/// container record: image_digest/runtime.
pub static CONTAINER_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "image_digest", Type::Bytes, true),
    FieldSpec::new(0x02, "runtime", Type::Str, true),
];
pub static TY_REC_CONTAINER: Type = Type::Record(&CONTAINER_SPEC);

/// semantic interaction (OBJECT_MODEL.md §6.17).
pub static INTERACTION_SPEC: [FieldSpec; 5] = [
    FieldSpec::new(0x01, "kind", Type::Enum(ENUM_INTERACTION_KIND), true),
    FieldSpec::new(0x02, "certainty", Type::Enum(ENUM_CERTAINTY), true),
    FieldSpec::new(0x03, "subjects", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x04, "severity", Type::Str, false),
    FieldSpec::new(0x05, "detail", Type::Str, false),
];
pub static TY_REC_INTERACTION: Type = Type::Record(&INTERACTION_SPEC);

/// release artifact (OBJECT_MODEL.md §6.18).
pub static ARTIFACT_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "digest", Type::Bytes, true),
    FieldSpec::new(0x03, "uri", Type::Str, false),
];
pub static TY_REC_ARTIFACT: Type = Type::Record(&ARTIFACT_SPEC);

/// retention tier policy (OBJECT_MODEL.md §6.21).
pub static TIER_SPEC: [FieldSpec; 5] = [
    FieldSpec::new(0x01, "tier", Type::U64, true),
    FieldSpec::new(0x02, "policy", Type::Enum(ENUM_RETENTION_POLICY), true),
    FieldSpec::new(0x03, "days", Type::U64, false),
    FieldSpec::new(0x04, "bytes", Type::U64, false),
    FieldSpec::new(0x05, "remote", Type::Str, false),
];
pub static TY_REC_TIER: Type = Type::Record(&TIER_SPEC);

/// retention policy (OBJECT_MODEL.md §6.21).
pub static RETENTION_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "tiers", Type::Array(&TY_REC_TIER), false),
    FieldSpec::new(
        0x02,
        "default_unknown",
        Type::Enum(ENUM_DEFAULT_UNKNOWN),
        false,
    ),
];
pub static TY_REC_RETENTION: Type = Type::Record(&RETENTION_SPEC);

/// GC policy.
pub static GC_SPEC: [FieldSpec; 2] = [
    FieldSpec::new(0x01, "enabled", Type::Bool, true),
    FieldSpec::new(0x02, "interval_days", Type::U64, true),
];
pub static TY_REC_GC: Type = Type::Record(&GC_SPEC);

/// limits record (OBJECT_MODEL.md §6.21).
pub static LIMITS_SPEC: [FieldSpec; 5] = [
    FieldSpec::new(0x01, "max_object_bytes", Type::U64, true),
    FieldSpec::new(0x02, "max_record_depth", Type::U64, true),
    FieldSpec::new(0x03, "max_array_elements", Type::U64, true),
    FieldSpec::new(0x04, "max_refs_per_object", Type::U64, true),
    FieldSpec::new(0x05, "max_string_bytes", Type::U64, true),
];
pub static TY_REC_LIMITS: Type = Type::Record(&LIMITS_SPEC);

/// mapping loss documentation (OBJECT_MODEL.md §6.22).
pub static LOSS_SPEC: [FieldSpec; 3] = [
    FieldSpec::new(0x01, "known_loss", Type::Array(&TY_STR), false),
    FieldSpec::new(0x02, "unknowns", Type::Array(&TY_STR), false),
    FieldSpec::new(0x03, "fabricated", Type::Array(&TY_STR), false),
];
pub static TY_REC_LOSS: Type = Type::Record(&LOSS_SPEC);

// ---------------------------------------------------------------------------
// Per-family schemas.
// ---------------------------------------------------------------------------

/// blob (OBJECT_MODEL.md §6.1): body is raw bytes; no fields.
pub static BLOB_SPEC: [FieldSpec; 0] = [];

/// tree (OBJECT_MODEL.md §6.2).
pub static TREE_SPEC: [FieldSpec; 1] = [FieldSpec::new(
    0x01,
    "entries",
    Type::Array(&TY_REC_ENTRY),
    true,
)];

/// state (OBJECT_MODEL.md §6.3).
pub static STATE_SPEC: [FieldSpec; 1] = [FieldSpec::new(
    0x01,
    "root_tree",
    Type::Gid(Family::Tree),
    true,
)];

/// operation (OBJECT_MODEL.md §6.4).
pub static OPERATION_SPEC: [FieldSpec; 37] = [
    FieldSpec::new(0x01, "op_type", Type::Enum(ENUM_OP_TYPE), true),
    FieldSpec::new(0x02, "subject_path", Type::Path, false),
    FieldSpec::new(0x03, "subject_ref", Type::GidAny, false),
    FieldSpec::new(0x04, "input_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x05, "output_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x06, "result", Type::Record(&RESULT_SPEC), false),
    FieldSpec::new(0x07, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x08, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x09, "started_at", Type::I64, false),
    FieldSpec::new(0x0A, "ended_at", Type::I64, false),
    FieldSpec::new(0x0B, "description", Type::Str, false),
    FieldSpec::new(0x0C, "outcome_refs", Type::Array(&TY_GID_ANY), false),
    // Kind-specific parameter tags (flat, unambiguous; see §6.4 matrix).
    FieldSpec::new(0x11, "content", Type::Gid(Family::Blob), false),
    FieldSpec::new(0x12, "start", Type::U64, false),
    FieldSpec::new(0x13, "length", Type::U64, false),
    FieldSpec::new(0x14, "new_content", Type::Gid(Family::Blob), false),
    FieldSpec::new(0x15, "old_content", Type::Gid(Family::Blob), false),
    FieldSpec::new(0x16, "from", Type::Path, false),
    FieldSpec::new(0x17, "to", Type::Path, false),
    FieldSpec::new(0x18, "patch", Type::Gid(Family::Blob), false),
    FieldSpec::new(0x19, "patch_format", Type::Str, false),
    FieldSpec::new(0x1A, "argv", Type::Array(&TY_STR), false),
    FieldSpec::new(0x1B, "cwd", Type::Str, false),
    FieldSpec::new(0x1C, "stdin_ref", Type::GidAny, false),
    FieldSpec::new(0x1D, "stdout_ref", Type::GidAny, false),
    FieldSpec::new(0x1E, "stderr_ref", Type::GidAny, false),
    FieldSpec::new(0x1F, "test_command", Type::Str, false),
    FieldSpec::new(0x20, "test_ids", Type::Array(&TY_STR), false),
    FieldSpec::new(0x21, "tool", Type::Str, false),
    FieldSpec::new(0x22, "oracle", Type::Str, false),
    FieldSpec::new(0x23, "oracle_version", Type::Str, false),
    FieldSpec::new(0x24, "query", Type::GidAny, false),
    FieldSpec::new(0x25, "response", Type::GidAny, false),
    FieldSpec::new(0x26, "artifact", Type::GidAny, false),
    FieldSpec::new(0x27, "transform", Type::Str, false),
    FieldSpec::new(0x28, "input_ast", Type::GidAny, false),
    FieldSpec::new(0x29, "output_ast", Type::GidAny, false),
];

/// episode (OBJECT_MODEL.md §6.5).
pub static EPISODE_SPEC: [FieldSpec; 13] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Episode), false),
    FieldSpec::new(0x02, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x03, "input_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x04, "operations", Type::Array(&TY_OPERATION), false),
    FieldSpec::new(0x05, "output_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x06, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x07, "agent_run", Type::Gid(Family::AgentRun), false),
    FieldSpec::new(0x08, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x09, "summary", Type::Str, false),
    FieldSpec::new(0x0A, "outcome", Type::Enum(ENUM_EPISODE_OUTCOME), false),
    FieldSpec::new(0x0B, "started_at", Type::I64, false),
    FieldSpec::new(0x0C, "ended_at", Type::I64, false),
    FieldSpec::new(0x0D, "trajectory", Type::Gid(Family::Trajectory), false),
];

/// intent (OBJECT_MODEL.md §6.6).
pub static INTENT_SPEC: [FieldSpec; 12] = [
    FieldSpec::new(0x01, "summary", Type::Str, true),
    FieldSpec::new(0x02, "description", Type::Str, false),
    FieldSpec::new(0x03, "acceptance_criteria", Type::Array(&TY_STR), false),
    FieldSpec::new(0x04, "constraints", Type::Array(&TY_STR), false),
    FieldSpec::new(0x05, "requested_scope", Type::Array(&TY_STR), false),
    FieldSpec::new(0x06, "prohibited_scope", Type::Array(&TY_STR), false),
    FieldSpec::new(0x07, "related_objects", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x08, "parent_intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x09, "external_refs", Type::Array(&TY_REC_EXT_REF), false),
    FieldSpec::new(0x0A, "case", Type::Gid(Family::Case), false),
    FieldSpec::new(0x0B, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x0C, "created_at", Type::I64, false),
];

/// change (OBJECT_MODEL.md §6.7) — the central object.
pub static CHANGE_SPEC: [FieldSpec; 21] = [
    FieldSpec::new(0x01, "summary", Type::Str, true),
    FieldSpec::new(0x02, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x03, "input_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x04, "operations", Type::Array(&TY_OPERATION), false),
    FieldSpec::new(0x05, "resulting_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x06, "producer", Type::Gid(Family::Producer), true),
    FieldSpec::new(0x07, "agent_run", Type::Gid(Family::AgentRun), false),
    FieldSpec::new(
        0x08,
        "context_manifest",
        Type::Gid(Family::ContextManifest),
        false,
    ),
    FieldSpec::new(0x09, "disclosure", Type::Enum(ENUM_DISCLOSURE), false),
    FieldSpec::new(0x0A, "instruction_digest", Type::Bytes, false),
    FieldSpec::new(0x0B, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x0C, "claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x0D, "evidence", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(0x0E, "residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x0F, "verification", Type::Array(&TY_VERIFICATION), false),
    FieldSpec::new(0x10, "dependencies", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x11, "causal_parents", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(0x12, "case", Type::Gid(Family::Case), false),
    FieldSpec::new(0x13, "trajectory", Type::Gid(Family::Trajectory), false),
    FieldSpec::new(0x14, "episode", Type::Gid(Family::Episode), false),
    FieldSpec::new(0x15, "created_at", Type::I64, false),
];

/// case (OBJECT_MODEL.md §6.8).
pub static CASE_SPEC: [FieldSpec; 11] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Case), false),
    FieldSpec::new(0x02, "summary", Type::Str, true),
    FieldSpec::new(0x03, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x04, "description", Type::Str, false),
    FieldSpec::new(0x05, "status", Type::Enum(ENUM_CASE_STATUS), false),
    FieldSpec::new(0x06, "added_changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(
        0x07,
        "added_trajectories",
        Type::Array(&TY_TRAJECTORY),
        false,
    ),
    FieldSpec::new(0x08, "releases", Type::Array(&TY_RELEASE), false),
    FieldSpec::new(0x09, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x0A, "created_at", Type::I64, false),
    FieldSpec::new(0x0B, "updated_at", Type::I64, false),
];

/// trajectory (OBJECT_MODEL.md §6.9).
pub static TRAJECTORY_SPEC: [FieldSpec; 14] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Trajectory), false),
    FieldSpec::new(0x02, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x03, "base_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x04, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x05, "agent_run", Type::Gid(Family::AgentRun), false),
    FieldSpec::new(0x06, "added_changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(0x07, "added_episodes", Type::Array(&TY_EPISODE), false),
    FieldSpec::new(0x08, "added_evidence", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(0x09, "added_residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x0A, "outcome", Type::Enum(ENUM_TRAJECTORY_OUTCOME), false),
    FieldSpec::new(0x0B, "termination_reason", Type::Str, false),
    FieldSpec::new(0x0C, "handoff", Type::Record(&HANDOFF_SPEC), false),
    FieldSpec::new(0x0D, "created_at", Type::I64, false),
    FieldSpec::new(0x0E, "updated_at", Type::I64, false),
];

/// claim (OBJECT_MODEL.md §6.10).
pub static CLAIM_SPEC: [FieldSpec; 14] = [
    FieldSpec::new(0x01, "subject", Type::Str, false),
    FieldSpec::new(0x02, "subject_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x03, "predicate", Type::Str, true),
    FieldSpec::new(0x04, "predicate_kind", Type::Enum(ENUM_CLAIM_KIND), false),
    FieldSpec::new(0x05, "scope", Type::Str, false),
    FieldSpec::new(0x06, "scope_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x07, "producer", Type::Gid(Family::Producer), true),
    FieldSpec::new(0x08, "evidence", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(0x09, "residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x0A, "dependencies", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x0B, "supersedes", Type::Gid(Family::Claim), false),
    FieldSpec::new(0x0C, "change", Type::Gid(Family::Change), false),
    FieldSpec::new(0x0D, "assertion", Type::Str, false),
    FieldSpec::new(0x0E, "created_at", Type::I64, false),
];

/// evidence (OBJECT_MODEL.md §6.11).
pub static EVIDENCE_SPEC: [FieldSpec; 17] = [
    FieldSpec::new(0x01, "producer", Type::Gid(Family::Producer), true),
    FieldSpec::new(0x02, "kind", Type::Enum(ENUM_EVIDENCE_KIND), true),
    FieldSpec::new(0x03, "subject", Type::Str, false),
    FieldSpec::new(0x04, "subject_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x05, "command", Type::Str, false),
    FieldSpec::new(0x06, "command_ref", Type::Gid(Family::Operation), false),
    FieldSpec::new(0x07, "input_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x08, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x09, "tools", Type::Array(&TY_REC_TOOL), false),
    FieldSpec::new(0x0A, "fixtures", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x0B, "normalizers", Type::Array(&TY_REC_NC), false),
    FieldSpec::new(0x0C, "comparators", Type::Array(&TY_REC_NC), false),
    FieldSpec::new(0x0D, "result", Type::Record(&EVIDENCE_RESULT_SPEC), false),
    FieldSpec::new(0x0E, "output_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x0F, "reproduction", Type::Record(&REPRO_SPEC), false),
    FieldSpec::new(0x10, "created_at", Type::I64, false),
    FieldSpec::new(0x11, "evaluated_state", Type::Gid(Family::State), false),
];

/// residual (OBJECT_MODEL.md §6.12).
pub static RESIDUAL_SPEC: [FieldSpec; 12] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Residual), false),
    FieldSpec::new(0x02, "summary", Type::Str, true),
    FieldSpec::new(
        0x03,
        "classification",
        Type::Enum(ENUM_RESIDUAL_CLASS),
        false,
    ),
    FieldSpec::new(0x04, "severity", Type::Enum(ENUM_RESIDUAL_SEVERITY), false),
    FieldSpec::new(0x05, "scope", Type::Record(&RESIDUAL_SCOPE_SPEC), false),
    FieldSpec::new(0x06, "affected_claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x07, "affected_changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(0x08, "origin_evidence", Type::Gid(Family::Evidence), false),
    FieldSpec::new(0x09, "first_observed_at", Type::I64, false),
    FieldSpec::new(
        0x0A,
        "disposition_event",
        Type::Record(&DISPOSITION_EVENT_SPEC),
        false,
    ),
    FieldSpec::new(0x0B, "recurrence", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x0C, "created_at", Type::I64, false),
];

/// verification (OBJECT_MODEL.md §6.13).
pub static VERIFICATION_SPEC: [FieldSpec; 12] = [
    FieldSpec::new(0x01, "subject", Type::Str, false),
    FieldSpec::new(0x02, "subject_refs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x03, "scope", Type::Record(&VERIFY_SCOPE_SPEC), false),
    FieldSpec::new(0x04, "claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x05, "evidence", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(0x06, "residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x07, "result", Type::Enum(ENUM_VERIFY_RESULT), true),
    FieldSpec::new(0x08, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x09, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x0A, "started_at", Type::I64, false),
    FieldSpec::new(0x0B, "ended_at", Type::I64, false),
    FieldSpec::new(0x0C, "policy", Type::Gid(Family::Config), false),
];

/// producer (OBJECT_MODEL.md §6.14).
pub static PRODUCER_SPEC: [FieldSpec; 6] = [
    FieldSpec::new(0x01, "kind", Type::Enum(ENUM_PRODUCER_KIND), true),
    FieldSpec::new(0x02, "name", Type::Str, true),
    FieldSpec::new(
        0x03,
        "identity",
        Type::Record(&PRODUCER_IDENTITY_SPEC),
        false,
    ),
    FieldSpec::new(0x04, "disclosure", Type::Enum(ENUM_DISCLOSURE), true),
    FieldSpec::new(0x05, "attestation", Type::Gid(Family::Evidence), false),
    FieldSpec::new(0x06, "created_at", Type::I64, false),
];

/// agentrun (OBJECT_MODEL.md §6.15).
pub static AGENTRUN_SPEC: [FieldSpec; 17] = [
    FieldSpec::new(0x01, "producer", Type::Gid(Family::Producer), true),
    FieldSpec::new(0x02, "model_family", Type::Str, false),
    FieldSpec::new(0x03, "model_id", Type::Str, false),
    FieldSpec::new(0x04, "harness", Type::Str, false),
    FieldSpec::new(0x05, "permissions", Type::Array(&TY_STR), false),
    FieldSpec::new(0x06, "input_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x07, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(
        0x08,
        "context_manifest",
        Type::Gid(Family::ContextManifest),
        false,
    ),
    FieldSpec::new(0x09, "instruction_digest", Type::Bytes, false),
    FieldSpec::new(0x0A, "tool_identities", Type::Array(&TY_REC_TOOL), false),
    FieldSpec::new(0x0B, "environment", Type::Gid(Family::Environment), false),
    FieldSpec::new(0x0C, "parent", Type::Gid(Family::AgentRun), false),
    FieldSpec::new(
        0x0D,
        "output_trajectory",
        Type::Gid(Family::Trajectory),
        false,
    ),
    FieldSpec::new(0x0E, "disclosure", Type::Enum(ENUM_DISCLOSURE), true),
    FieldSpec::new(0x0F, "conversation_ref", Type::GidAny, false),
    FieldSpec::new(0x10, "started_at", Type::I64, false),
    FieldSpec::new(0x11, "ended_at", Type::I64, false),
];

/// environment (OBJECT_MODEL.md §6.16).
pub static ENVIRONMENT_SPEC: [FieldSpec; 11] = [
    FieldSpec::new(0x01, "os", Type::Record(&OS_SPEC), false),
    FieldSpec::new(0x02, "arch", Type::Str, false),
    FieldSpec::new(0x03, "compiler", Type::Record(&NV_SPEC), false),
    FieldSpec::new(0x04, "runtime", Type::Record(&NV_SPEC), false),
    FieldSpec::new(0x05, "toolchain", Type::Array(&TY_REC_TC), false),
    FieldSpec::new(0x06, "hardware", Type::Record(&HW_SPEC), false),
    FieldSpec::new(0x07, "container", Type::Record(&CONTAINER_SPEC), false),
    FieldSpec::new(0x08, "network", Type::Enum(ENUM_NETWORK), false),
    FieldSpec::new(0x09, "env_manifest", Type::Gid(Family::Blob), false),
    FieldSpec::new(0x0A, "determinism", Type::Enum(ENUM_DETERMINISM), false),
    FieldSpec::new(0x0B, "created_at", Type::I64, false),
];

/// reconciliation (OBJECT_MODEL.md §6.17).
pub static RECONCILIATION_SPEC: [FieldSpec; 18] = [
    FieldSpec::new(0x01, "summary", Type::Str, true),
    FieldSpec::new(0x02, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(
        0x03,
        "input_trajectories",
        Type::Array(&TY_TRAJECTORY),
        false,
    ),
    FieldSpec::new(0x04, "input_states", Type::Array(&TY_STATE), false),
    FieldSpec::new(0x05, "adopted_changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(0x06, "rejected_changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(
        0x07,
        "unresolved_residuals",
        Type::Array(&TY_RESIDUAL),
        false,
    ),
    FieldSpec::new(0x08, "resolved_residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(
        0x09,
        "semantic_interactions",
        Type::Array(&TY_REC_INTERACTION),
        false,
    ),
    FieldSpec::new(0x0A, "claims_retained", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x0B, "claims_invalidated", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x0C, "evidence_retained", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(
        0x0D,
        "verification_required",
        Type::Array(&TY_VERIFICATION),
        false,
    ),
    FieldSpec::new(0x0E, "resulting_state", Type::Gid(Family::State), false),
    FieldSpec::new(0x0F, "resulting_change", Type::Gid(Family::Change), false),
    FieldSpec::new(0x10, "rationale", Type::Str, false),
    FieldSpec::new(0x11, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x12, "created_at", Type::I64, false),
];

/// release (OBJECT_MODEL.md §6.18).
pub static RELEASE_SPEC: [FieldSpec; 11] = [
    FieldSpec::new(0x01, "name", Type::Str, true),
    FieldSpec::new(0x02, "version", Type::Str, false),
    FieldSpec::new(0x03, "state", Type::Gid(Family::State), true),
    FieldSpec::new(0x04, "changes", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(0x05, "cases", Type::Array(&TY_CASE), false),
    FieldSpec::new(0x06, "claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x07, "residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(0x08, "verification", Type::Array(&TY_VERIFICATION), false),
    FieldSpec::new(0x09, "artifacts", Type::Array(&TY_REC_ARTIFACT), false),
    FieldSpec::new(0x0A, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x0B, "created_at", Type::I64, false),
];

/// context-manifest (OBJECT_MODEL.md §6.19).
pub static CONTEXT_MANIFEST_SPEC: [FieldSpec; 12] = [
    FieldSpec::new(0x01, "source_objects", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(
        0x02,
        "documentation_objects",
        Type::Array(&TY_GID_ANY),
        false,
    ),
    FieldSpec::new(0x03, "claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(0x04, "residuals", Type::Array(&TY_RESIDUAL), false),
    FieldSpec::new(
        0x05,
        "previous_trajectories",
        Type::Array(&TY_TRAJECTORY),
        false,
    ),
    FieldSpec::new(0x06, "external_artifacts", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x07, "tool_outputs", Type::Array(&TY_GID_ANY), false),
    FieldSpec::new(0x08, "policies", Type::Array(&TY_CONFIG), false),
    FieldSpec::new(0x09, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x0A, "instruction", Type::Str, false),
    FieldSpec::new(0x0B, "instruction_digest", Type::Bytes, false),
    FieldSpec::new(0x0C, "created_at", Type::I64, false),
];

/// checkpoint (OBJECT_MODEL.md §6.20).
pub static CHECKPOINT_SPEC: [FieldSpec; 13] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Checkpoint), false),
    FieldSpec::new(0x02, "summary", Type::Str, true),
    FieldSpec::new(0x03, "intent", Type::Gid(Family::Intent), false),
    FieldSpec::new(0x04, "trajectory", Type::Gid(Family::Trajectory), false),
    FieldSpec::new(0x05, "state", Type::Gid(Family::State), false),
    FieldSpec::new(0x06, "open_claims", Type::Array(&TY_CLAIM), false),
    FieldSpec::new(
        0x07,
        "unresolved_residuals",
        Type::Array(&TY_RESIDUAL),
        false,
    ),
    FieldSpec::new(0x08, "important_evidence", Type::Array(&TY_EVIDENCE), false),
    FieldSpec::new(0x09, "recent_decisions", Type::Array(&TY_CHANGE), false),
    FieldSpec::new(
        0x0A,
        "relevant_attempts",
        Type::Array(&TY_TRAJECTORY),
        false,
    ),
    FieldSpec::new(0x0B, "continuation_scope", Type::Array(&TY_STR), false),
    FieldSpec::new(0x0C, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x0D, "created_at", Type::I64, false),
];

/// config (OBJECT_MODEL.md §6.21).
pub static CONFIG_SPEC: [FieldSpec; 7] = [
    FieldSpec::new(0x01, "previous", Type::Gid(Family::Config), false),
    FieldSpec::new(0x02, "retention", Type::Record(&RETENTION_SPEC), false),
    FieldSpec::new(0x03, "gc", Type::Record(&GC_SPEC), false),
    FieldSpec::new(0x04, "execution_policy", Type::Enum(ENUM_EXEC_POLICY), true),
    FieldSpec::new(
        0x05,
        "disclosure_default",
        Type::Enum(ENUM_DISCLOSURE),
        false,
    ),
    FieldSpec::new(0x06, "limits", Type::Record(&LIMITS_SPEC), false),
    FieldSpec::new(0x07, "created_at", Type::I64, false),
];

/// mapping (OBJECT_MODEL.md §6.22).
pub static MAPPING_SPEC: [FieldSpec; 6] = [
    FieldSpec::new(0x01, "kind", Type::Enum(ENUM_MAPPING_KIND), true),
    FieldSpec::new(0x02, "from", Type::Str, true),
    FieldSpec::new(0x03, "to", Type::GidAny, true),
    FieldSpec::new(0x04, "loss", Type::Record(&LOSS_SPEC), false),
    FieldSpec::new(0x05, "producer", Type::Gid(Family::Producer), false),
    FieldSpec::new(0x06, "created_at", Type::I64, false),
];

// ---------------------------------------------------------------------------
// Family schema registry.
// ---------------------------------------------------------------------------

static SCHEMA_BLOB: FamilySchema = FamilySchema {
    family: Family::Blob,
    schemever: 1,
    extensions_allowed: false,
    fields: &BLOB_SPEC,
};
static SCHEMA_TREE: FamilySchema = FamilySchema {
    family: Family::Tree,
    schemever: 1,
    extensions_allowed: true,
    fields: &TREE_SPEC,
};
static SCHEMA_STATE: FamilySchema = FamilySchema {
    family: Family::State,
    schemever: 1,
    extensions_allowed: true,
    fields: &STATE_SPEC,
};
static SCHEMA_OPERATION: FamilySchema = FamilySchema {
    family: Family::Operation,
    schemever: 1,
    extensions_allowed: true,
    fields: &OPERATION_SPEC,
};
static SCHEMA_EPISODE: FamilySchema = FamilySchema {
    family: Family::Episode,
    schemever: 1,
    extensions_allowed: true,
    fields: &EPISODE_SPEC,
};
static SCHEMA_INTENT: FamilySchema = FamilySchema {
    family: Family::Intent,
    schemever: 1,
    extensions_allowed: true,
    fields: &INTENT_SPEC,
};
static SCHEMA_CHANGE: FamilySchema = FamilySchema {
    family: Family::Change,
    schemever: 1,
    extensions_allowed: true,
    fields: &CHANGE_SPEC,
};
static SCHEMA_CASE: FamilySchema = FamilySchema {
    family: Family::Case,
    schemever: 1,
    extensions_allowed: true,
    fields: &CASE_SPEC,
};
static SCHEMA_TRAJECTORY: FamilySchema = FamilySchema {
    family: Family::Trajectory,
    schemever: 1,
    extensions_allowed: true,
    fields: &TRAJECTORY_SPEC,
};
static SCHEMA_CLAIM: FamilySchema = FamilySchema {
    family: Family::Claim,
    schemever: 1,
    extensions_allowed: true,
    fields: &CLAIM_SPEC,
};
static SCHEMA_EVIDENCE: FamilySchema = FamilySchema {
    family: Family::Evidence,
    schemever: 1,
    extensions_allowed: true,
    fields: &EVIDENCE_SPEC,
};
static SCHEMA_RESIDUAL: FamilySchema = FamilySchema {
    family: Family::Residual,
    schemever: 1,
    extensions_allowed: true,
    fields: &RESIDUAL_SPEC,
};
static SCHEMA_VERIFICATION: FamilySchema = FamilySchema {
    family: Family::Verification,
    schemever: 1,
    extensions_allowed: true,
    fields: &VERIFICATION_SPEC,
};
static SCHEMA_PRODUCER: FamilySchema = FamilySchema {
    family: Family::Producer,
    schemever: 1,
    extensions_allowed: true,
    fields: &PRODUCER_SPEC,
};
static SCHEMA_AGENTRUN: FamilySchema = FamilySchema {
    family: Family::AgentRun,
    schemever: 1,
    extensions_allowed: true,
    fields: &AGENTRUN_SPEC,
};
static SCHEMA_ENVIRONMENT: FamilySchema = FamilySchema {
    family: Family::Environment,
    schemever: 1,
    extensions_allowed: true,
    fields: &ENVIRONMENT_SPEC,
};
static SCHEMA_RECONCILIATION: FamilySchema = FamilySchema {
    family: Family::Reconciliation,
    schemever: 1,
    extensions_allowed: true,
    fields: &RECONCILIATION_SPEC,
};
static SCHEMA_RELEASE: FamilySchema = FamilySchema {
    family: Family::Release,
    schemever: 1,
    extensions_allowed: true,
    fields: &RELEASE_SPEC,
};
static SCHEMA_CONTEXT_MANIFEST: FamilySchema = FamilySchema {
    family: Family::ContextManifest,
    schemever: 1,
    extensions_allowed: true,
    fields: &CONTEXT_MANIFEST_SPEC,
};
static SCHEMA_CHECKPOINT: FamilySchema = FamilySchema {
    family: Family::Checkpoint,
    schemever: 1,
    extensions_allowed: true,
    fields: &CHECKPOINT_SPEC,
};
static SCHEMA_CONFIG: FamilySchema = FamilySchema {
    family: Family::Config,
    schemever: 1,
    extensions_allowed: true,
    fields: &CONFIG_SPEC,
};
static SCHEMA_MAPPING: FamilySchema = FamilySchema {
    family: Family::Mapping,
    schemever: 1,
    extensions_allowed: true,
    fields: &MAPPING_SPEC,
};

/// Returns the schema for a family, or `None` for an unknown family.
pub fn schema_for(family: Family) -> &'static FamilySchema {
    match family {
        Family::Blob => &SCHEMA_BLOB,
        Family::Tree => &SCHEMA_TREE,
        Family::State => &SCHEMA_STATE,
        Family::Operation => &SCHEMA_OPERATION,
        Family::Episode => &SCHEMA_EPISODE,
        Family::Intent => &SCHEMA_INTENT,
        Family::Change => &SCHEMA_CHANGE,
        Family::Case => &SCHEMA_CASE,
        Family::Trajectory => &SCHEMA_TRAJECTORY,
        Family::Claim => &SCHEMA_CLAIM,
        Family::Evidence => &SCHEMA_EVIDENCE,
        Family::Residual => &SCHEMA_RESIDUAL,
        Family::Verification => &SCHEMA_VERIFICATION,
        Family::Producer => &SCHEMA_PRODUCER,
        Family::AgentRun => &SCHEMA_AGENTRUN,
        Family::Environment => &SCHEMA_ENVIRONMENT,
        Family::Reconciliation => &SCHEMA_RECONCILIATION,
        Family::Release => &SCHEMA_RELEASE,
        Family::ContextManifest => &SCHEMA_CONTEXT_MANIFEST,
        Family::Checkpoint => &SCHEMA_CHECKPOINT,
        Family::Config => &SCHEMA_CONFIG,
        Family::Mapping => &SCHEMA_MAPPING,
    }
}

/// All schemas, in code order (conformance tests).
pub fn all_schemas() -> [&'static FamilySchema; 22] {
    [
        &SCHEMA_BLOB,
        &SCHEMA_TREE,
        &SCHEMA_STATE,
        &SCHEMA_OPERATION,
        &SCHEMA_EPISODE,
        &SCHEMA_INTENT,
        &SCHEMA_CHANGE,
        &SCHEMA_CASE,
        &SCHEMA_TRAJECTORY,
        &SCHEMA_CLAIM,
        &SCHEMA_EVIDENCE,
        &SCHEMA_RESIDUAL,
        &SCHEMA_VERIFICATION,
        &SCHEMA_PRODUCER,
        &SCHEMA_AGENTRUN,
        &SCHEMA_ENVIRONMENT,
        &SCHEMA_RECONCILIATION,
        &SCHEMA_RELEASE,
        &SCHEMA_CONTEXT_MANIFEST,
        &SCHEMA_CHECKPOINT,
        &SCHEMA_CONFIG,
        &SCHEMA_MAPPING,
    ]
}

/// The operation kind-tag matrix (OBJECT_MODEL.md §6.4).
///
/// A parameter tag (0x11..=0x7F) present in an operation must be declared for
/// the operation's `op_type`. `other` permits any parameter tag.
pub fn op_kind_tags(op_type: &str) -> Option<&'static [u8]> {
    match op_type {
        "create_file" | "write_file" => Some(&[0x11]),
        "write_range" => Some(&[0x12, 0x13, 0x14, 0x15]),
        "delete_file" => Some(&[]),
        "rename_path" => Some(&[0x16, 0x17]),
        "apply_patch" => Some(&[0x18, 0x19]),
        "exec_command" => Some(&[0x1A, 0x1B, 0x1C, 0x1D, 0x1E]),
        "run_test" => Some(&[0x1F, 0x20, 0x21]),
        "invoke_oracle" => Some(&[0x22, 0x23, 0x24, 0x25]),
        "inspect_artifact" => Some(&[0x26]),
        "ast_transform" => Some(&[0x27, 0x28, 0x29]),
        "other" => None, // any parameter tag permitted
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_well_formed() {
        for s in all_schemas() {
            // Tags strictly ascending, within range, no duplicates.
            let mut prev = 0u8;
            for f in s.fields {
                assert!(f.tag > prev, "family {} tags out of order", s.family);
                assert!(
                    f.tag >= 0x01 && f.tag <= 0x7F,
                    "family {} tag out of range",
                    s.family
                );
                prev = f.tag;
            }
            assert_eq!(s.schemever, 1);
            assert!(s.family.supports_schemever(s.schemever));
        }
        assert_eq!(all_schemas().len(), Family::ALL.len());
    }

    #[test]
    fn blob_has_no_fields() {
        assert!(schema_for(Family::Blob).fields.is_empty());
        assert!(!schema_for(Family::Blob).extensions_allowed);
        assert!(schema_for(Family::Change).extensions_allowed);
    }

    #[test]
    fn required_fields_are_as_documented() {
        assert!(schema_for(Family::Change).field(0x01).unwrap().required); // summary
        assert!(schema_for(Family::Change).field(0x06).unwrap().required); // producer
        assert!(schema_for(Family::State).field(0x01).unwrap().required); // root_tree
        assert!(schema_for(Family::Claim).field(0x03).unwrap().required); // predicate
        assert!(!schema_for(Family::Claim).field(0x01).unwrap().required); // subject optional
    }
}
