# Gemel — Invariants

Status: **Normative.** Version 1.0.0.

This document enumerates every invariant Gemel guarantees, the layer that enforces it,
and the failure mode when it is violated. Invariants are identified by stable IDs
(`ENC-`, `ID-`, `OBJ-`, `FAM-`, `STO-`, `REF-`, `DER-`, `RET-`, `QRY-`, `SEC-`).

An invariant is either:

- **structural** — enforced at encode/decode time (fail closed; a violation is a
  rejected object), or
- **state** — enforced at store time (a violation is a detected corruption or an
  explicit policy decision), or
- **derived** — a property of computations over the canonical graph (a violation is an
  implementation bug).

Enforcement abbreviations: **ENC** = canonical encoder; **DEC** = canonical decoder;
**VAL** = object validation layer; **STO** = store; **REF** = ref layer; **IDX** =
derived index; **FSK** = `fsck`; **QRY** = query layer; **WS** = workspace layer;
**GC** = garbage collector; **POL** = policy/config layer.

---

## 1. Encoding Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| ENC-01 | The envelope begins with magic `GEML` (0x47 0x45 0x4D 0x4C). | DEC | Reject `BadMagic` |
| ENC-02 | `encver` is exactly 1 (Phase 0). | DEC | Reject `UnknownEncodingVersion` |
| ENC-03 | `family` is in the family table (0x01..=0x16). | DEC | Reject `UnknownFamily` |
| ENC-04 | `schemever` is in the family's supported set (Phase 0: exactly {1}). | DEC | Reject `UnknownSchemaVersion` |
| ENC-05 | `flags` is exactly 0x00. | DEC | Reject `ReservedFlags` |
| ENC-06 | `bodylen` equals the actual body length; no trailing bytes. | DEC | Reject `LengthMismatch` |
| ENC-07 | Every UINT is canonical minimal LEB128 (no redundant leading zero groups; ≤10 bytes; ≤2^64−1). | DEC | Reject `NonCanonicalInteger` / `IntegerOverflow` |
| ENC-08 | Every SINT decodes to a valid i64 via the zigzag map. | DEC | Reject `NonCanonicalInteger` |
| ENC-09 | BOOL is exactly 0x00 or 0x01. | DEC | Reject `InvalidBoolean` |
| ENC-10 | STRING length matches byte count and content is strict valid UTF-8 (no overlongs, no surrogates, ≤ U+10FFFF). | DEC | Reject `InvalidUtf8` |
| ENC-11 | GID values are exactly 33 bytes (family + 32-byte digest). | DEC | Reject `InvalidGid` |
| ENC-12 | Record field tags are strictly ascending and unique. | DEC | Reject `UnsortedFields` / `DuplicateField` |
| ENC-13 | Field tags are in 0x01..=0xEF; tag 0x00 and 0xF0..=0xFF are forbidden. | DEC | Reject `ReservedTag` |
| ENC-14 | A tag in 0x01..=0x7F unknown to the schema ⇒ reject (mandatory-structure fail-closed). | DEC | Reject `UnknownMandatoryField` |
| ENC-15 | A tag in 0x80..=0xEF on a family that does not permit extensions ⇒ reject. | DEC | Reject `ExtensionNotPermitted` |
| ENC-16 | A tag in 0x80..=0xEF on a family that permits extensions is retained verbatim and re-emitted byte-identically. | ENC/DEC | Round-trip mismatch ⇒ bug |
| ENC-17 | Field `value_len` equals the byte length of the canonical value encoding for the declared type. | DEC | Reject `ValueLengthMismatch` |
| ENC-18 | Required fields are present; absent required field ⇒ reject. | VAL | Reject `MissingRequiredField` |
| ENC-19 | Record depth is ≤ the depth limit. | DEC | Reject `LimitExceeded` |
| ENC-20 | Array counts and object sizes are ≤ the configured limits. | DEC | Reject `LimitExceeded` |
| ENC-21 | Encoding is deterministic: identical logical input ⇒ identical bytes (no environment, locale, or wall-clock influence unless a timestamp value is explicitly supplied). | ENC | Golden-vector mismatch ⇒ bug |
| ENC-22 | Canonical strings are never normalized; bytes are preserved exactly. | ENC/DEC | — |
| ENC-23 | Canonical paths obey §OBJECT_MODEL 1.6 (no absolute, no `\`, no empty/`.`/`..` segments). | VAL | Reject `InvalidPath` |

## 2. Identity Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| ID-01 | `ObjectId = BLAKE3-256(envelope bytes)` and nothing else. | ENC/STO | — |
| ID-02 | Equal envelopes ⇒ equal identities; unequal envelopes ⇒ unequal identities (BLAKE3-256 collision resistance). | — | Collision ⇒ security incident |
| ID-03 | Identity never derives from timestamps, sequence numbers, UUIDs, paths, PIDs, or counters. | ENC | — |
| ID-04 | Textual identities parse as `<family-short>.<64 lowercase hex>`; the family prefix matches the envelope family. | DEC/QRY | Reject `MalformedIdentity` |
| ID-05 | Names (refs) and identities are separate concepts; a name may be retargeted, an identity never changes. | REF | — |
| ID-06 | The golden vectors pin identities; any change to encoding/hashing that alters a pinned identity is a breaking protocol change. | tests | Vector mismatch ⇒ blocked release |

## 3. Object and Graph Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| OBJ-01 | All canonical objects are immutable once published. | STO | — |
| OBJ-02 | The canonical graph is a DAG: no cycles through any mixture of edges. | FSK | Report `CycleDetected` |
| OBJ-03 | `causal_parents` chains are acyclic. | VAL/FSK | Reject/Report |
| OBJ-04 | Append chains (`previous` on trajectory/case/residual/checkpoint/config) are acyclic. | VAL/FSK | Reject/Report |
| OBJ-05 | Content composition (tree→tree) is acyclic and within depth limits. | VAL/FSK | Reject/Report |
| OBJ-06 | Every GID reference resolves: to an existing object, a tombstone, or a declared remote location. | FSK | Report `MissingReference` |
| OBJ-07 | A change's `causal_parents` (transitively) derive from its `input_state` (consistency, derived check). | FSK/QRY | Report `StateInconsistency` |
| OBJ-08 | The change sequence of a trajectory (concatenation of `added_changes` across the chain) has monotonically advancing states (each change's input_state = predecessor's resulting_state, where present). | FSK/QRY | Report `SequenceGap` |
| OBJ-09 | Claim status is never stored; it is derived per §OBJECT_MODEL 8.1. | VAL | Stored status ⇒ schema violation |
| OBJ-10 | Residual persistence is derived, never stored. | VAL | Stored persistence ⇒ schema violation |
| OBJ-11 | Verification results retain scope; no un-scoped global `verified`. | VAL | — |
| OBJ-12 | A `mapping.loss.fabricated` array is always empty. | VAL | Reject non-empty |

## 4. Per-Family Invariants

| ID | Family | Invariant | Enforced |
|---|---|---|---|
| FAM-01 | tree | Entries sorted ascending by name bytes; duplicate names rejected. | DEC |
| FAM-02 | tree | Mode ∈ {0o100644, 0o100755, 0o120000, 0o040000}. | DEC |
| FAM-03 | tree | Target family matches mode (tree↔0o040000; blob otherwise). | DEC |
| FAM-04 | tree | Names are single segments (§OBJECT_MODEL 1.6). | DEC |
| FAM-05 | state | `root_tree` resolves to a valid tree. | FSK |
| FAM-06 | operation | Kind-specific tags (0x11+) are declared for the operation's `op_type`; undeclared ⇒ reject. | DEC |
| FAM-07 | operation | `result.status` ∈ enum; exit_code semantics per kind. | DEC |
| FAM-08 | intent | Intent is immutable; decomposition via `parent_intent`. | VAL |
| FAM-09 | change | At least one of {operations, resulting_state} present for material changes (advisory: enforced by policy, not schema). | POL |
| FAM-10 | change | `disclosure` ∈ enum; provenance chain respects it (query layer). | QRY |
| FAM-11 | trajectory | Outcome ∈ enum; `termination_reason` may accompany any outcome; unsuccessful trajectories are never deleted. | VAL/GC |
| FAM-12 | claim | `predicate` non-empty; `supersedes` never points to itself (acyclic). | VAL/FSK |
| FAM-13 | evidence | `result.outcome` ∈ enum; `evaluated_state` (when present) resolves to a state. | VAL/FSK |
| FAM-14 | evidence | Never reduced to a single boolean; scope/inputs/environment/tools/reproduction preserved. | VAL |
| FAM-15 | residual | Current disposition = latest version's `disposition_event`; absent ⇒ `open`. | QRY |
| FAM-16 | residual | `affected_changes` non-empty for a meaningful residual (advisory). | POL |
| FAM-17 | verification | `result` ∈ enum; scope present. | VAL |
| FAM-18 | producer | `kind` `git_import`/`unknown` carry no fabricated identity. | VAL |
| FAM-19 | agentrun | Conversation material is optional; correctness never depends on it. | VAL |
| FAM-20 | environment | Manifests are inert data; never executed. | POL/EXEC |
| FAM-21 | reconciliation | Inputs are preserved; adopted+rejected+unresolved are recorded, not dropped. | VAL |
| FAM-22 | mapping | `from` string exactly as external system writes it. | VAL |

## 5. Storage Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| STO-01 | An object file's bytes re-hash to its filename identity. | STO/FSK | Report `CorruptObject` |
| STO-02 | Objects appear atomically (temp + verify + rename + dir fsync). | STO | — |
| STO-03 | Insert of an existing id with different bytes ⇒ fail closed (never overwrite). | STO | Report `HashCollision` |
| STO-04 | Readers never observe partial objects or partial ref transactions. | STO/REF | — |
| STO-05 | Indexes are derived; no query result depends on an index that cannot be rebuilt. | IDX/FSK | Report `IndexCorrupt` |
| STO-06 | Index rebuild is atomic (fresh DB + rename). | IDX | — |
| STO-07 | A ref never points to an object that is not durable. | REF | — |
| STO-08 | Workspace `state.ref` resolves; dirty records are conservative (recomputed on `status`). | WS/FSK | Report `WorkspaceInconsistent` |

## 6. Ref and Namespace Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| REF-01 | Ref names obey the name syntax (§STORAGE 4.1); traversal-resistant. | REF | Reject `InvalidRefName` |
| REF-02 | Ref updates are journaled and atomic; recovery replays or rolls back. | REF | — |
| REF-03 | The journal is an audit/recovery aid; ref files are the source of truth. | REF/FSK | — |
| REF-04 | Multi-ref transactions (e.g., head + state/head) commit atomically. | REF | — |
| REF-05 | Ref contents are validated on read (must parse as a valid identity). | REF | Report `CorruptRef` |

## 7. Derived-State Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| DER-01 | Claim status computation is deterministic and follows §OBJECT_MODEL 8.1 precedence. | QRY | Test-verified |
| DER-02 | Staleness is conservative: `MAY_REQUIRE_REFRESH` never silently downgrades a contradiction. | QRY | Test-verified |
| DER-03 | Residual persistence counts descendant changes via reverse `causal_parents` closure. | QRY | Test-verified |
| DER-04 | Readiness is a deterministic function with an explicit reason list. | QRY | Test-verified |
| DER-05 | Query results are stable under identical (repo, query, params) inputs. | QRY | Test-verified |
| DER-06 | **The derived index never changes semantic answers** (INVARIANTS core): every semantically meaningful indexed query has a canonical slow path, and the index is consulted only when fresh (not stale, object/ref counts match the canonical store). `query(with index) == query(after deleting the index)` for every derived query. | IDX/QRY | `tests/derived_consistency.rs` — a violation is an implementation bug |
| DER-07 | Claim→evidence, residual→claim, and residual→evidence links are **explicit producer declarations only**; the system never infers a semantic relationship from insertion order, shared subject strings, or any heuristic (AGENT_PROTOCOL.md §7: ingestion never auto-links). Absent links stay unknown. | QRY/workflow | A violation is an implementation bug |
| DER-08 | `log` attributes each change to the trajectory whose `added_changes` contain it (canonical reverse derivation) — never to the currently selected trajectory. | QRY | Test-verified |
| DER-09 | Captured States record their capture coherence (`capture` extension, OBJECT_MODEL.md §6.3); an incoherent capture is recorded as such, never silently claimed coherent. | WS | Test-verified |
| DER-10 | Exact materialization removes unignored extras but never destroys metadata (`.gemel`), enclosing Git metadata (`.git`), configuration (root `.gitignore`), or ignored (declared non-content) paths; at the repository root, removal of unignored unrecorded work requires an explicit `--force`. | WS/QRY | Test-verified |

## 8. Retention Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| RET-01 | Tier 0 objects are pruned only under explicit policy override (default: never). | GC | — |
| RET-02 | Reachable objects are never pruned without policy authorization. | GC | — |
| RET-03 | Tombstones are created before the blob is unlinked. | GC | — |
| RET-04 | A tombstoned object yields `Pruned` + tombstone, never a fabricated object. | STO | — |
| RET-05 | `fsck` distinguishes `missing` (corruption) from `pruned` (policy). | FSK | — |
| RET-06 | GC consults open journal transactions before pruning. | GC | — |
| RET-07 | GC writes an audit entry (objects removed, bytes reclaimed, tombstones). | GC | — |

## 9. Query Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| QRY-01 | Machine output is versioned (`gemel.query.v1`), deterministic, bounded, paginated. | QRY | — |
| QRY-02 | Omitted data is explicit (`omitted[]` with reason). | QRY | — |
| QRY-03 | Uncertainty is explicit (`uncertainty[]`), never silently resolved. | QRY | — |
| QRY-04 | Object references in output are canonical identities, resolvable for expansion. | QRY | — |
| QRY-05 | Pagination cursors are stable and opaque. | QRY | — |
| QRY-06 | Context budgets are honored; expansion is progressive (§AGENT_PROTOCOL 6). | QRY | — |

## 10. Security Invariants

| ID | Invariant | Enforced | Failure mode |
|---|---|---|---|
| SEC-01 | All parsing is bounded by limits before allocation. | DEC | Reject `LimitExceeded` |
| SEC-02 | Repository inputs are treated as hostile; no code path trusts object bytes. | all | — |
| SEC-03 | Reproduction/command metadata is inert; execution requires explicit policy (default `never_auto_execute`). | POL/EXEC | — |
| SEC-04 | Workspace materialization cannot escape the workspace root. | WS | Reject `PathTraversal` |
| SEC-05 | Ref names cannot traverse directories or alias namespaces. | REF | Reject `InvalidRefName` |
| SEC-06 | No unbounded query expansion: depth, fanout, and budget limits apply. | QRY | Reject `BudgetExceeded` |
| SEC-07 | Remote data (Phase 6) is verified by hash before use. | NET | Abort transfer |
| SEC-08 | Disclosure policies are respected by the query layer. | QRY | — |

## 11. fsck Check Mapping

| fsck check (STORAGE.md §8) | Invariants verified |
|---|---|
| 1. Envelope and hash | ENC-01…06, ENC-09…13, ID-01, STO-01 |
| 2. Schema validity | ENC-07…08, ENC-10…20, ENC-23, FAM-* |
| 3. References | OBJ-06, FAM-05, FAM-13 |
| 4. Impossible relationships | OBJ-02…05, FAM-01…04, FAM-06, FAM-12 |
| 5. Reachability | REF-05, STO-07 |
| 6. Index consistency | STO-05, STO-06 |
| 7. Working-state metadata | STO-08 |
| 8. Journal | REF-02, REF-03, REF-04 |

## 12. Enforcement Matrix Summary

| Layer | Enforces | Recovers |
|---|---|---|
| Encoder | ENC-16, ENC-21, ENC-22, ID-01, ID-02 | — |
| Decoder | ENC-01…20, ENC-23, FAM-01…04, FAM-06, FAM-07 | — |
| Validation | ENC-18, ENC-23, OBJ-12, FAM-09…22 (as marked) | — |
| Store | STO-01…08, ID-03 | — |
| Refs | REF-01…05, ID-05 | journal replay |
| Index | STO-05, STO-06 | rebuild |
| fsck | all FSK-marked rows | repair of derived artifacts |
| Query | DER-01…10, QRY-01…06, SEC-08 | — |
| GC | RET-01…07 | tombstone restore (remote) |
| Workspace | SEC-04, STO-08, DER-09, DER-10 | recompute dirty |

Every row of this matrix is a Phase 1+ test target; the negative fixture catalog
(THREAT_MODEL.md §4) and golden vectors (OBJECT_MODEL.md §12) already exercise the
decoder rows in Phase 0.
