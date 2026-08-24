//! Resource limits enforced during parsing and validation
//! (OBJECT_MODEL.md §5, THREAT_MODEL.md §5).

/// Parse and validation limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum total envelope size in bytes.
    pub max_object_bytes: u64,
    /// Maximum record nesting depth.
    pub max_record_depth: usize,
    /// Maximum number of elements in any array.
    pub max_array_elements: usize,
    /// Maximum byte length of any STRING or BYTES field value.
    pub max_string_bytes: usize,
    /// Maximum number of GID references in a single object.
    pub max_refs_per_object: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_object_bytes: 1 << 30, // 1 GiB
            max_record_depth: 64,
            max_array_elements: 1_000_000,
            max_string_bytes: 16 << 20, // 16 MiB
            max_refs_per_object: 100_000,
        }
    }
}
