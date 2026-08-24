# Gemel — Specification (Master Document)

Status: **Phase 1 — Minimal Useful Gemel — complete.** Next phase: Phase 2 (Agent-Native
Value).
Document version: 1.1.0 (schema version `encver=1`, all object families `schemever=1`).
Audience: implementers, protocol engineers, agent authors, maintainers.

This document is the normative entry point for the Gemel project. It states what Gemel
is, the principles that govern every design decision, the architecture at a glance, the
phased delivery plan, and — critically — the **conformance matrices** that bind every
requirement of each completed phase to its defining document and section.

Normative detail lives in the companion documents:

| Document | Normative for |
|---|---|
| `docs/OBJECT_MODEL.md` | Canonical encoding, hashing, identity, all object families, derived status, compatibility |
| `docs/STORAGE.md` | On-disk layout, refs, atomicity, indexes, retention, fsck, concurrency |
| `docs/INVARIANTS.md` | The complete invariant set and its enforcement |
| `docs/GIT_INTEROP.md` | Deterministic Git interchange semantics |
| `docs/AGENT_PROTOCOL.md` | Machine-readable query surface, budgets, ingestion, handoff |
| `docs/THREAT_MODEL.md` | Security model, fail-closed catalog, limits |

Where a companion document and this one disagree, the companion document is authoritative
for its subject; a disagreement is a bug to be filed, not a license to choose.

---

## 1. Purpose and Scope

Gemel is a distributed version-control system designed from first principles for
agentic software development. Its central premise:

> The native unit of version control is an **evidence-bearing change**: a durable record
> of what was attempted, against which exact state, by which producer, through which
> operations, with which claims, supported by which evidence, contradicted by which
> residuals, and resulting in which state.

Gemel preserves intent, developmental trajectory, intermediate operations, resulting
state, provenance, agent execution identity, claims, evidence, residual disagreement,
failed attempts, verification scope, reconciliation decisions, semantic relationships,
and exact reproducibility information. The goal is a **durable machine-usable memory
substrate for software engineering**: a future LLM (or human) must be able to understand
what was attempted, why particular code exists, which alternatives failed, what evidence
justified a decision, which uncertainty remains, and what work can safely be reused.

Gemel must remain useful even if no LLM exists. Its primitives are rigorous
software-engineering concepts, not model-specific abstractions.

Implementation language: native Rust. The implementation must exhibit exceptional
systems taste: simple primitives, deterministic semantics, explicit invariants,
content-addressed identity, fail-closed behavior, bounded complexity, strong forensic
properties, excellent performance, and an architecture capable of surviving decades of
evolution.

### 1.1 Non-goals (Phases 0–1)

- No distributed synchronization (Phase 6).
- No Git wire-protocol compatibility (Phase 4/6; Git *interchange* is designed now).
- No semantic indexing beyond the object model (Phase 5).
- No UI, chat integration, embeddings, or vector database (explicitly deferred;
  embeddings are *never* canonical semantics — see §37 of the brief and §8.7 of
  OBJECT_MODEL.md).
- No hosted service; Gemel is local-first and offline-first (Phase 0 artifacts must all
  function from local storage).

---

## 2. Design Principles

The engineering doctrine, restated as the binding principles for every decision:

1. **Evidence over assertion.** Nothing becomes repository truth merely because it was
   stated. Statements are `Claim` objects; what happened is `Evidence`.
2. **Residuals over false certainty.** Unexplained disagreement persists as a
   first-class `Residual` until explicitly resolved, acknowledged, superseded, or proven
   irrelevant. It never silently disappears because someone chose an implementation.
3. **Immutable objects over mutable historical records.** All canonical objects are
   immutable and content-addressed. History is never rewritten; it is extended.
4. **Structured provenance over prose archaeology.** Provenance is a typed graph
   (`Producer`, `AgentRun`, `ContextManifest`, `Environment`), not a name string.
5. **Exact identity over filename heuristics.** Identity derives from canonical bytes,
   not from paths, line numbers, or repository-local counters. Semantic identity is
   explicit lineage, never silent inference.
6. **Progressive disclosure over context flooding.** Query surfaces return object
   references and bounded summaries first; expansion is explicit and budget-aware.
7. **Negative knowledge over repeated mistakes.** Failed attempts are repository
   knowledge and are never erased. They are simply excluded from default human views.
8. **Reconciliation over destructive flattening.** Multiple trajectories are
   *reconciled* into a chosen direction; the alternatives remain.
9. **Explicit unknowns over fabricated history.** When provenance is unavailable,
   the value is `UNKNOWN`. Gemel never invents development history.
10. **Machine-readable structure over CLI scraping.** Every query has a versioned,
    deterministic, bounded machine form.
11. **Deterministic semantics over convenience.** The same canonical object must hash
    identically on every machine, forever, for a given schema version.
12. **Local ownership over hosted dependency.** Core functionality works with no
    network.
13. **Git compatibility without Git conceptual captivity.** Git interchange is
    mandatory and lossy-by-design; Git's ontology never dictates Gemel's.
14. **Agent usefulness without model-specific coupling.** The substrate is model-agnostic
    and works with no LLM at all.

Supporting discipline: **fail closed** everywhere. Unknown mandatory structure rejects.
Unknown optional extension structure may be retained losslessly only where the schema
explicitly permits extension. Parsing is bounded by explicit limits. Corruption is
treated as unacceptable.

---

## 3. Architectural Overview

```text
                    ┌─────────────────────────────────────────────────────┐
                    │                  Query & Agent Surface             │
                    │   status why claims evidence residuals attempts    │
                    │   trajectory context checkpoint reconcile diff     │
                    │   human CLI  ·  JSON v1  ·  library API            │
                    └──────────────────────────┬──────────────────────────┘
                                               │
                    ┌──────────────────────────▼──────────────────────────┐
                    │      Knowledge Layer (derived, rebuildable)         │
                    │   claim status · residual persistence · readiness   │
                    │   impact analysis · semantic interaction · context  │
                    └──────────────────────────┬──────────────────────────┘
                                               │
                    ┌──────────────────────────▼──────────────────────────┐
                    │      Engineering Layer (immutable object graph)     │
                    │  Release → Case → Trajectory → Change → Episode →   │
                    │  Operation · Intent · Claim · Evidence · Residual   │
                    │  Verification · Reconciliation · Producer ·        │
                    │  AgentRun · Environment · ContextManifest           │
                    └──────────────────────────┬──────────────────────────┘
                                               │
                    ┌──────────────────────────▼──────────────────────────┐
                    │   Content Layer (Merkle content addressing)         │
                    │   Blob → Tree → State                               │
                    └──────────────────────────┬──────────────────────────┘
                                               │
                    ┌──────────────────────────▼──────────────────────────┐
                    │   Canonical Object Store (immutable, verified)      │
                    │   + Derived Indexes (disposable) + Refs (mutable)   │
                    └──────────────────────────┬──────────────────────────┘
                                               │
                    ┌──────────────────────────▼──────────────────────────┐
                    │   Workspaces (working trees, checkout/materialize)  │
                    └─────────────────────────────────────────────────────┘
```

Layering is strict: each layer depends only on layers below. The canonical object store
and refs are the **sole source of truth**; every layer above the store is derived and
must be rebuildable from canonical objects alone.

Crate structure (proposed in the brief, refined for Phase 0; see §7 of this document):

```text
object
  ↓
core
  ↓
store/state
  ↓
trajectory/evidence
  ↓
query/reconcile
  ↓
workspace/git/agent
  ↓
cli
```

No dependency cycles. No god crates. Crates are created only where the boundary earns
them (§40 of the brief). Phase 0 ships a single crate, `gemel`, with disciplined
internal layering: canonical primitives (varint, family table, Gid, hex, limits) →
canonical encoding and schema tables (all 22 object families) → validation and
hashing → golden fixtures. The published crate name on crates.io is `gemel`.

---

## 4. The Resolution Hierarchy

Gemel recognizes that humans and agents need different resolutions of history. One
repository, many projections:

```text
Operation          low-level transformation (byte write, exec, oracle call, test run)
   ↓
Episode            bounded coherent period of activity (tool-use loop, subtask, one fix)
   ↓
Change             coherent software transformation with intent, claims, evidence, result
   ↓
Case               larger engineering objective containing many changes and trajectories
   ↓
Release            user-facing/distributable state derived from completed cases
```

Same underlying history, projectable at each resolution. Humans are not forced to
inspect tens of thousands of operations; agents may drill into them when useful.

---

## 5. Identity Model (Summary)

Normative detail: `docs/OBJECT_MODEL.md` §§2–4.

- Every canonical object is a byte string with a fixed envelope: magic `GEML`,
  encoding version, family code, schema version, flags, length-prefixed body.
- **Object identity** = BLAKE3-256 over the full canonical envelope bytes.
  `ObjectId = H(canonical(Object))`.
- Identity is derived from nothing else: never from timestamps, database sequence
  numbers, random UUIDs, filesystem paths, process IDs, or repository-local counters.
- **Human-readable names** (refs) point to immutable object identities. Naming and
  identity are separate concepts. Names are mutable pointers in a namespace; identities
  are immutable content hashes.
- Textual identity format: `<family-short>.<64-lowercase-hex>`, e.g.
  `change.9f3a…e1`. Machine-consumable, deterministic, family-prefixed to prevent
  type confusion.
- Determinism is non-negotiable: same canonical bytes ⇒ same identity on every machine
  and every implementation, pinned by golden vectors.

---

## 6. Object Catalog (Summary)

Normative field tables: `docs/OBJECT_MODEL.md` §6. Families are fixed and versioned;
Phase 0 defines twenty-two families at `schemever=1`:

| Code | Family | Role |
|---|---|---|
| 0x01 | `blob` | raw bytes |
| 0x02 | `tree` | sorted Merkle directory entries |
| 0x03 | `state` | exact repository content state (Merkle root) |
| 0x04 | `operation` | low-level transformation |
| 0x05 | `episode` | bounded coherent activity period |
| 0x06 | `intent` | first-class why |
| 0x07 | `change` | the central evidence-bearing unit |
| 0x08 | `case` | larger engineering objective |
| 0x09 | `trajectory` | base-state-anchored pursuit of an intent |
| 0x0A | `claim` | asserted engineering proposition |
| 0x0B | `evidence` | durable record of what actually happened |
| 0x0C | `residual` | unresolved disagreement, first-class |
| 0x0D | `verification` | scoped verification run |
| 0x0E | `producer` | identity of an actor (human/agent/automation/…) |
| 0x0F | `agentrun` | agent execution identity |
| 0x10 | `environment` | machine/context manifest |
| 0x11 | `reconciliation` | multi-trajectory engineering decision |
| 0x12 | `release` | distributable state |
| 0x13 | `context-manifest` | content-addressed record of what an agent saw |
| 0x14 | `checkpoint` | continuation boundary |
| 0x15 | `config` | repository policy (retention, limits, execution policy) |
| 0x16 | `mapping` | deterministic external-ID ↔ Gemel-ID correspondence |

Accumulating families (trajectory, case, residual, checkpoint, config) use
append-chaining: each update publishes a new immutable object whose `previous` field
points to the prior version; mutable names track the latest. All other families are
single immutable objects.

---

## 7. Storage & Indexing Model (Summary)

Normative detail: `docs/STORAGE.md`.

- **Canonical state** = content-addressed immutable objects + mutable refs. This is the
  only source of truth.
- **Derived acceleration** = disposable indexes (SQLite) that can always be rebuilt
  from canonical objects. Index corruption is repaired by rebuild, never by history loss.
- Object store: write-temp → verify hash → atomic rename; hash verification on every
  read; single-writer/concurrent-reader; journal for multi-ref transactions.
- Retention tiers 0–3 with per-repository policy; GC understands tiers; pruned blobs
  referenced by canonical objects leave explicit tombstones.
- `gemel fsck` verifies hashes, references, schema validity, missing/corrupt objects,
  index consistency, and working-state metadata.
- Workspaces are separate from canonical representation: canonical paths are
  repository-relative; working-directory representation is a workspace concern.

---

## 8. Query & Agent Surface (Summary)

Normative detail: `docs/AGENT_PROTOCOL.md`.

Every important query exists in four forms: human CLI output, versioned machine-readable
JSON (`schema: gemel.query.v1`), library API, and (later) a lightweight agent protocol.
Machine output is deterministic, versioned, bounded, paginated, explicit about omitted
data, and explicit about uncertainty.

Phase 2 surface (designed now, implemented later): `status`, `why`, `claims`,
`evidence`, `residuals`, `attempts`, `trajectory`, `checkpoint`, `context`,
`reconcile --plan`, multidimensional `diff`, `impact`, `context-diff`.

---

## 9. Verification Substrate: FRF Boundary

Normative detail: `docs/OBJECT_MODEL.md` §8.6, `docs/AGENT_PROTOCOL.md` §10.

Gemel uses the Forensic Residual Framework (FRF) as its verification substrate but does
**not** merge FRF into Gemel. The boundary:

| Gemel owns | FRF owns |
|---|---|
| version-control semantics, Changes, Trajectories, repository State, provenance, reconciliation, agent query, Git projection, distributed synchronization | courts, authority definitions, fixtures, comparators, normalizers, evidence protocol, residual analysis, reproducibility semantics, verification receipts |

Gemel references FRF artifacts by immutable identity. Court receipts arrive as
`evidence` objects of kind `court_receipt`. Gemel never executes commands from
reproduction metadata; execution is always policy-gated (default: never auto-execute).

---

## 10. Phase Plan

Each phase has explicit entry and exit criteria. Phases deliver in order; no phase may
be skipped.

### Phase 0 — Specification Before Features (complete)

**Entry:** none. **Exit:** the seven normative documents; the canonical object grammar,
hashing, identity, and all family semantics defined; compatibility rules defined;
executable golden fixtures for object encoding committed and passing; this document's
conformance matrix (§11) satisfied.

Deliverables:
- `docs/SPECIFICATION.md` (this file)
- `docs/OBJECT_MODEL.md`
- `docs/STORAGE.md`
- `docs/INVARIANTS.md`
- `docs/GIT_INTEROP.md`
- `docs/AGENT_PROTOCOL.md`
- `docs/THREAT_MODEL.md`
- the `gemel` crate: canonical encoding, schema tables,
  fail-closed decoder with lossless extension retention, BLAKE3 identity
- `golden/`: executable golden vectors pinning canonical bytes and identities
- Negative fixtures proving fail-closed behavior

Explicitly *not* in Phase 0: object store, refs, CLI, workspace materialization, git
import/export implementation, query server, distributed sync.

### Phase 1 — Minimal Useful Gemel (complete)

**Entry:** Phase 0 complete. **Exit:** `gemel init`, `status`, `snapshot`, `change
begin`, `change finish`, `log`, `show`, `diff`, `checkout`, `fsck` operate on a local
store; the demonstration `State S0 → Intent I1 → Trajectory T1 → Change C1 → State S1`
works with exact content-addressed reconstruction.

Support: immutable object store, state snapshots, Intent, Change, basic Producer, basic
Claim, basic Evidence, basic Residual, Trajectory, resulting State. No distributed sync.

Phase 1 exit verified by the §11.5 conformance matrix and the §17 exit statement.

### Phase 2 — Agent-Native Value

**Entry:** Phase 1 complete. **Exit:** `why`, `claims`, `evidence`, `residuals`,
`attempts`, `trajectory`, `checkpoint`, `context` with machine-readable JSON forms; the
§12 acceptance demo works end-to-end with two agents and no shared conversation.

### Phase 3 — Reconciliation

**Entry:** Phase 2 complete. **Exit:** `gemel reconcile` using textual changes, path
changes, explicit dependency relationships, Claims, Evidence, Residuals; produces a
Reconciliation object (adopted / rejected / unresolved / verification required /
resulting State); concurrent agents from the same base State demonstrated. No semantic
claims beyond actual evidence; uncertainty is exposed, never invented.

### Phase 4 — Git Interchange

**Entry:** Phase 3 complete. **Exit:** deterministic Git→Gemel and Gemel→Git with
authorship/timestamps/messages/trees/ancestry preserved; Gemel-native metadata
represented explicitly as unknown when unavailable; stable Gemel identifiers embedded
in Git trailers; round-trip behavior proven where mathematically possible; intentional
loss documented.

### Phase 5 — Semantic Depth

**Entry:** Phase 4 complete. **Exit:** language-aware semantic indexing (starting with
Rust) providing identities for module/type/trait/impl/function/method/constant/static/
test/feature/dependency; richer `why`, `diff --semantic`, `attempts`, `impact`.
Language intelligence is an enhancement, never a core storage dependency; arbitrary
files work without it.

### Phase 6 — Distributed Operation

**Entry:** Phase 5 complete. **Exit:** remotes, object negotiation, partial fetch,
resumable transfer, integrity verification, multiple producers, authentication,
authorization, repository policy, hosted workflows. Content addressing makes sync
naturally deduplicated. Gemel sync is a separate problem from Git interchange.

---

## 11. Phase 0 Conformance Matrix

Every §41 requirement of the brief, bound to its normative definition. This matrix is
part of the Phase 0 exit criteria.

| Brief requirement | Normative definition |
|---|---|
| Canonical object grammar | OBJECT_MODEL.md §1 (GCE grammar, canonical rules) |
| Hashing | OBJECT_MODEL.md §2 |
| Object identity | OBJECT_MODEL.md §2–§3 |
| State representation | OBJECT_MODEL.md §6.3 (family `state`), §6.2 (`tree`) |
| Change representation | OBJECT_MODEL.md §6.7 |
| Trajectory semantics | OBJECT_MODEL.md §6.9, §7.3 |
| Claim semantics | OBJECT_MODEL.md §6.10, §8.1 |
| Evidence semantics | OBJECT_MODEL.md §6.11, §8.2 |
| Residual semantics | OBJECT_MODEL.md §6.12, §8.3 |
| Reconciliation semantics | OBJECT_MODEL.md §6.17, §8.5 |
| Object lifetime | OBJECT_MODEL.md §9 |
| Retention | STORAGE.md §7 |
| Compatibility rules | OBJECT_MODEL.md §10; GIT_INTEROP.md |
| Golden fixtures (executable) | OBJECT_MODEL.md §12; `golden/` directory; `gemel` tests |

Additional cross-cutting requirements honored in Phase 0:

| Brief principle | Where enforced |
|---|---|
| Determinism, canonical byte encoding, golden vectors | OBJECT_MODEL.md §1–§2, §11; golden/ |
| Fail-closed parsing, bounded parsing | OBJECT_MODEL.md §1.6, §11; THREAT_MODEL.md §4–§5 |
| Forward evolution (extensions, schema versioning) | OBJECT_MODEL.md §1.7, §10 |
| Absence/null distinction | OBJECT_MODEL.md §1.5 |
| No absolute paths in identity | OBJECT_MODEL.md §1.8, §6.2 |
| No compression in canonical identity | OBJECT_MODEL.md §1.9; STORAGE.md §7.5 |
| Derived statuses over mutable truth | OBJECT_MODEL.md §8 |
| Security model | THREAT_MODEL.md |

### 11.5 Phase 1 Conformance Matrix

Every §42 requirement of the brief, bound to its implementation. This matrix is part of
the Phase 1 exit criteria and is verified by the integration suite
(`tests/phase1_tests.rs`) and the CLI (`src/bin/gemel.rs`).

| Brief requirement (Phase 1) | Implementation |
|---|---|
| `gemel init` | `Repo::init` (`src/store/mod.rs`); `gemel init` (`src/bin/gemel.rs`) |
| `gemel status` | `query::status` (`src/query.rs`) |
| `gemel snapshot` | `content::build_state` (`src/content.rs`) |
| `gemel change begin` | `workflow::begin_change` (`src/workflow.rs`) |
| `gemel change finish` | `workflow::finish_change` (`src/workflow.rs`) |
| `gemel log` | `query::log` (`src/query.rs`) |
| `gemel show` | `query::show` (`src/query.rs`) |
| `gemel diff` | `content::diff_states`, `content::myers_diff` (`src/content.rs`) |
| `gemel checkout` | `content::materialize` (`src/content.rs`) |
| `gemel fsck` | `store::fsck` (`src/store/fsck.rs`) |
| Immutable object store | `store::objects` — temp→verify→rename, sharded, hash-verified reads |
| Ref namespace + journal | `store::refs` — journaled atomic transactions, rollback recovery |
| Crash safety / atomicity | write-temp-verify-rename; journal commit marker; `fsck` recovery |
| Disposable derived index | `store::index` — SQLite, schema versioned, rebuildable, drift-detected |
| State snapshots | `content::build_state`; exact reconstruction via `materialize` |
| Intent | `workflow::begin_change` (intent creation + name `I<n>`) |
| Change | `workflow::finish_change` (operations, resulting state, causal parents) |
| Basic Producer | `defaults::*_producer_object` (`src/defaults.rs`) |
| Basic Claim / Evidence / Residual | `workflow::finish_change` options; derived status in `query` |
| Trajectory | `workflow::finish_change` — continuation by intent, chaining, names `T<n>` |
| Resulting State | content-addressed; reconstructed byte-exact (demo test) |
| Working tree ↔ state | `build_state` / `materialize` / `working_tree_delta` |
| Deterministic textual diff | `myers_diff` — O(ND) trace + linear-space divide-and-conquer |
| .gitignore subset | `ignore` (`src/ignore.rs`); root `.gitignore` is config, never content |
| Retention tiers / tombstones | `store::retention`, `store::tombstone` (GC pass is Phase 2+) |
| JSON output `gemel.query.v1` | `print_json` envelope (`src/bin/gemel.rs`) |

---

## 12. The Core Acceptance Demo

The project is not conceptually proven until this scenario works (Phase 2 exit gate):

1. Agent A receives `Intent: Fix parser compatibility problem.`
2. Agent A queries and discovers `previous attempt T17 rejected because FreeBSD diverged`.
3. Agent A avoids repeating T17; creates T18.
4. During development, Agent A records `Claim C1: parser now matches upstream`.
5. FRF produces `Evidence E1 (Linux match)`, `Evidence E2 (FreeBSD mismatch)`.
6. Gemel records `Residual R1`.
7. Agent A stops, leaving a checkpoint.
8. Agent B receives only `repository` + `Intent`.
9. Agent B asks Gemel for relevant context; Gemel supplies T17 (rejected), T18
   (incomplete), C1 (partially supported), E1, E2, R1, and relevant source objects.
10. Agent B resolves the FreeBSD discrepancy and creates T19.
11. A Reconciliation chooses T19 while preserving T17 and T18 as engineering knowledge.
12. A future Agent C asks *"Why is this strange parser branch here?"* and Gemel walks
    `source → Change → Intent → Claim → Residual → oracle Evidence → previous failed
    Trajectory → reconciliation decision` — without access to the original agents'
    conversations.

That is the product. The object model is designed so every step of that walk is a
reliable typed query, not prose reconstruction.

---

## 13. Success Criteria Mapping

Gemel succeeds when it provides what Git fundamentally does not. The brief's §53
requirements map to concrete, testable artifacts:

| Gemel must tell an agent… | Artifact |
|---|---|
| what was attempted | Operation/Episode/Change graph; Trajectory |
| why | Intent (first-class, §6.6) |
| against what exact state | input_state/resulting_state identities |
| by which producer | Producer/AgentRun (§6.14–§6.15) |
| with what context | ContextManifest (§6.19) |
| through which trajectory | Trajectory (§6.9) |
| what the change claims | Claim (§6.10) |
| what evidence supports those claims | Evidence graph (§6.11) |
| what contradicts them | contradicting Evidence; Residuals (§6.12) |
| what remains unexplained | Residual (persistence, §8.3) |
| which alternatives failed and why | Trajectory outcomes + termination_reason (§7.3) |
| what another agent should inspect next | Handoff (§7.4), Checkpoint (§6.20) |

Reduced rediscovery, reduced repeated failed work, cheaper handoff, durable
verification, visible uncertainty, explainable decisions, safe concurrency, and
engineering memory — these are the measurable success criteria.

---

## 14. Glossary

- **Canonical object** — an immutable byte string conforming to the GCE envelope
  grammar (OBJECT_MODEL.md §1).
- **ObjectId / Gid** — BLAKE3-256 identity of a canonical object (OBJECT_MODEL.md §2).
- **Family** — the fixed object type encoded in the envelope (OBJECT_MODEL.md §6).
- **Schema version (schemever)** — per-family semantic version in the envelope.
- **Encoding version (encver)** — version of the GCE primitive grammar itself.
- **Ref / name** — mutable pointer in a namespace to an immutable object identity
  (STORAGE.md §4).
- **Extension field** — a tag in 0x80..=0xEF retained losslessly by older readers when
  the family permits extensions (OBJECT_MODEL.md §1.7).
- **Derived status** — computed property (never stored as truth) such as claim status,
  residual persistence, readiness (OBJECT_MODEL.md §8).
- **Tier** — retention class 0..3 (STORAGE.md §7).
- **Tombstone** — marker for a blob pruned by retention policy while still referenced
  by a canonical object (STORAGE.md §7.6).
- **FRF** — Forensic Residual Framework; Gemel's verification substrate, referenced by
  immutable identity, never merged (§9).
- **Court / receipt** — FRF verification authority and its result artifact, ingested as
  `evidence` of kind `court_receipt`.
- **MAY_REQUIRE_REFRESH** — conservative derived flag on evidence whose evaluation scope
  may have been affected by a later change (OBJECT_MODEL.md §8.4).

---

## 15. Document Index and Reading Order

1. This document — context, principles, conformance.
2. `docs/OBJECT_MODEL.md` — the ontology; read before anything else technical.
3. `docs/STORAGE.md` — how the ontology is persisted.
4. `docs/INVARIANTS.md` — what must always be true.
5. `docs/GIT_INTEROP.md` — deterministic interchange.
6. `docs/AGENT_PROTOCOL.md` — the machine query surface.
7. `docs/THREAT_MODEL.md` — adversarial analysis and limits.

Golden fixtures in `golden/` are the executable binding between specification and
implementation; the `gemel` crate (`src/`) is the reference encoder/decoder.

---

## 16. Phase 0 Exit Statement

Phase 0 is complete when: all seven documents exist and are internally consistent; the
canonical grammar, hashing, identity, and all twenty-two families are specified; every
row of the §11 conformance matrix is satisfied by a normative definition; golden
fixtures are committed, executable, and passing; the decoder demonstrably fails closed
against the negative fixture catalog; and `cargo test` is green in the reference
implementation.

---

## 17. Phase 1 Exit Statement

Phase 1 is complete when: `init`, `status`, `snapshot`, `change begin`, `change finish`,
`log`, `show`, `diff`, `checkout`, and `fsck` operate on a local store; the
demonstration `State S0 → Intent I1 → Trajectory T1 → Change C1 → State S1` completes
with byte-exact content-addressed reconstruction of both states; every row of the §11.5
matrix is satisfied; `cargo test` (unit + golden + integration) and
`cargo clippy --all-targets` are green with zero warnings; and `cargo run --bin golden-gen`
reports the golden vectors up to date.

Phase 1 exit verified: `cargo test` green (78 unit, 2 golden, 11 integration),
`cargo clippy --all-targets` zero warnings, golden vectors unchanged, and the CLI
demonstration `gemel init → snapshot → change begin → change finish → log → show →
diff → status → fsck` exercised end-to-end by the integration suite.
