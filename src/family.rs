//! The object family table.
//!
//! Families are fixed, versioned object types encoded in the envelope
//! (OBJECT_MODEL.md §6). Phase 0 defines twenty-two families at schemever 1;
//! Phase 5 adds the semantic layer (`semantic-entity`, `semantic-index`).

use std::fmt;

/// The twenty-four canonical object families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Family {
    Blob = 0x01,
    Tree = 0x02,
    State = 0x03,
    Operation = 0x04,
    Episode = 0x05,
    Intent = 0x06,
    Change = 0x07,
    Case = 0x08,
    Trajectory = 0x09,
    Claim = 0x0A,
    Evidence = 0x0B,
    Residual = 0x0C,
    Verification = 0x0D,
    Producer = 0x0E,
    AgentRun = 0x0F,
    Environment = 0x10,
    Reconciliation = 0x11,
    Release = 0x12,
    ContextManifest = 0x13,
    Checkpoint = 0x14,
    Config = 0x15,
    Mapping = 0x16,
    /// A language-level entity observed in a source file (Phase 5).
    SemanticEntity = 0x17,
    /// The derived per-state semantic index (Phase 5).
    SemanticIndex = 0x18,
}

impl Family {
    /// All families in code order (used for iteration and conformance tests).
    pub const ALL: [Family; 24] = [
        Family::Blob,
        Family::Tree,
        Family::State,
        Family::Operation,
        Family::Episode,
        Family::Intent,
        Family::Change,
        Family::Case,
        Family::Trajectory,
        Family::Claim,
        Family::Evidence,
        Family::Residual,
        Family::Verification,
        Family::Producer,
        Family::AgentRun,
        Family::Environment,
        Family::Reconciliation,
        Family::Release,
        Family::ContextManifest,
        Family::Checkpoint,
        Family::Config,
        Family::Mapping,
        Family::SemanticEntity,
        Family::SemanticIndex,
    ];

    /// The family code byte used in the envelope and in binary references.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Resolves a family code byte.
    pub fn from_code(code: u8) -> Option<Family> {
        Family::ALL.iter().copied().find(|f| f.code() == code)
    }

    /// The short name used in textual identities (`change.9f3a…`).
    pub fn short(self) -> &'static str {
        match self {
            Family::Blob => "blob",
            Family::Tree => "tree",
            Family::State => "state",
            Family::Operation => "operation",
            Family::Episode => "episode",
            Family::Intent => "intent",
            Family::Change => "change",
            Family::Case => "case",
            Family::Trajectory => "trajectory",
            Family::Claim => "claim",
            Family::Evidence => "evidence",
            Family::Residual => "residual",
            Family::Verification => "verification",
            Family::Producer => "producer",
            Family::AgentRun => "agentrun",
            Family::Environment => "environment",
            Family::Reconciliation => "reconciliation",
            Family::Release => "release",
            Family::ContextManifest => "context-manifest",
            Family::Checkpoint => "checkpoint",
            Family::Config => "config",
            Family::Mapping => "mapping",
            Family::SemanticEntity => "semantic-entity",
            Family::SemanticIndex => "semantic-index",
        }
    }

    /// Resolves a short name.
    pub fn parse_short(name: &str) -> Option<Family> {
        Family::ALL.iter().copied().find(|f| f.short() == name)
    }

    /// Human display name.
    pub fn human(self) -> &'static str {
        match self {
            Family::Blob => "blob",
            Family::Tree => "tree",
            Family::State => "state",
            Family::Operation => "operation",
            Family::Episode => "episode",
            Family::Intent => "intent",
            Family::Change => "change",
            Family::Case => "case",
            Family::Trajectory => "trajectory",
            Family::Claim => "claim",
            Family::Evidence => "evidence",
            Family::Residual => "residual",
            Family::Verification => "verification",
            Family::Producer => "producer",
            Family::AgentRun => "agent run",
            Family::Environment => "environment",
            Family::Reconciliation => "reconciliation",
            Family::Release => "release",
            Family::ContextManifest => "context manifest",
            Family::Checkpoint => "checkpoint",
            Family::Config => "config",
            Family::Mapping => "mapping",
            Family::SemanticEntity => "semantic entity",
            Family::SemanticIndex => "semantic index",
        }
    }

    /// Whether the family permits extension fields (tags 0x80..=0xEF).
    /// Blob bodies are raw bytes and carry no field structure at all.
    pub fn allows_extensions(self) -> bool {
        self != Family::Blob
    }

    /// The maximum schema version this implementation supports for the family.
    pub fn max_schemever(self) -> u8 {
        1
    }

    /// Whether the family supports the given schema version (Phase 0: {1}).
    pub fn supports_schemever(self, schemever: u8) -> bool {
        schemever == 1 && self.max_schemever() >= schemever
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.short())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_shorts_are_unique_and_consistent() {
        let mut codes = std::collections::HashSet::new();
        let mut shorts = std::collections::HashSet::new();
        for f in Family::ALL {
            assert!(codes.insert(f.code()));
            assert!(shorts.insert(f.short()));
            assert_eq!(Family::from_code(f.code()), Some(f));
            assert_eq!(Family::parse_short(f.short()), Some(f));
        }
        assert_eq!(Family::ALL.len(), 24);
        assert_eq!(Family::from_code(0x00), None);
        assert_eq!(Family::from_code(0x19), None);
        assert_eq!(Family::from_code(0x17), Some(Family::SemanticEntity));
        assert_eq!(Family::parse_short("blob"), Some(Family::Blob));
        assert_eq!(
            Family::parse_short("context-manifest"),
            Some(Family::ContextManifest)
        );
        assert_eq!(Family::parse_short("commit"), None);
    }

    #[test]
    fn schemever_support() {
        for f in Family::ALL {
            assert!(f.supports_schemever(1));
            assert!(!f.supports_schemever(0));
            assert!(!f.supports_schemever(2));
        }
    }
}
