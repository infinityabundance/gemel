# Gemel — Specification (Master Document)

Status: **Phase 8 — Hosted Workflows & Network Transports — complete.** Next phase:
Phase 9 (per the roadmap; see the Phase Plan §10).
Document version: 1.3.0 (schema version `encver=1`, all object families `schemever=1`).
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

- No distributed synchronization beyond the native sync of Phase 6 (hosted
  workflows, network authentication servers, and the agent protocol interface are
  Phase 7).
- No Git wire-protocol compatibility (Phase 4/6; Git *interchange* is designed now).
- No semantic indexing beyond the object model (Phase 5; the lexical extractor is
  deterministic declaration indexing, not program analysis).
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

### Phase 2 — Agent-Native Value (complete)

**Entry:** Phase 1 complete. **Exit:** `why`, `claims`, `evidence`, `residuals`,
`attempts`, `trajectory`, `checkpoint`, `context` with machine-readable JSON forms; the
§12 acceptance demo works end-to-end with two agents and no shared conversation.

Supporting mechanics delivered with Phase 2: deterministic cursor pagination,
budget-bounded context bundles with progressive-disclosure levels, derived evidence
freshness (`MAY_REQUIRE_REFRESH`), chained trajectory outcomes
(`trajectory close`), chained residual dispositions (`residual resolve`), checkpoint
creation from structured repository state, and the rule that a closed trajectory is
terminal (new work on the same intent spawns a fresh attempt — §7).

Phase 2 exit verified by the §11.6 conformance matrix and the §18 exit statement.

### Phase 3 — Reconciliation (complete)

**Entry:** Phase 2 complete. **Exit:** `gemel reconcile` using textual changes, path
changes, explicit dependency relationships, Claims, Evidence, Residuals; produces a
Reconciliation object (adopted / rejected / unresolved / verification required /
resulting State); concurrent agents from the same base State demonstrated. No semantic
claims beyond actual evidence; uncertainty is exposed, never invented.

Phase 3 also delivers **multiple concurrent workspaces** (brief §34): named workspaces
keep their own pending-change and materialized-state records, and `change
begin/finish` accept `--workspace` and `--worktree`, so agents never serialize merely
to avoid filesystem collisions.

Adoption policy (documented in every rationale): per-path first-input-trajectory-wins.
Textual conflicts carry `certainty: observed`; claim interactions carry `certainty:
possible`; nothing is declared beyond the evidence (brief §13).

Phase 3 exit verified by the §11.7 conformance matrix and the §19 exit statement.

### Phase 1.5 — Git-Carried Exchange Rollups (complete)

**Entry:** Phase 3 complete (interleaved before Phase 4 per the roadmap). **Exit:** an
ordinary Git repository carries deterministic exchange artifacts (`.gemel/exchange/v1/`)
such that `git clone && gemel status --json` restores the exported engineering frontier
— Intents, Trajectories, Changes, Claims, Residuals, evidence summaries — with
cryptographically verified identities, explicit uncertainty, and exact source-state
binding. See `docs/EXCHANGE.md` (normative) and `tests/exchange_tests.rs` (34 courts
using real Git repositories: transport, shallow clone, branch-merge unions, Git-only
mutation, corruption, idempotence, byte determinism, non-interference, git cleanliness,
index independence, incremental export, golden fixtures).

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

### Phase 4 — Git Interchange (complete)

**Entry:** Phase 1.5 complete. **Exit:** deterministic Git→Gemel and Gemel→Git with
authorship/timestamps/messages/trees/ancestry preserved; Gemel-native metadata
represented explicitly as unknown when unavailable; stable Gemel identifiers embedded
in Git trailers; round-trip behavior proven where mathematically possible; intentional
loss documented.

Delivered: `gemel export-git` (deterministic projection of the head's causal-parent
closure into loose Git objects; `GEMEL-*` trailers; per-commit/tree `mapping` objects
under `refs/mappings/export/`), `gemel import-git` (topological import with synthetic
`git_import`/`human`/`unknown` producers, deterministic operations with conservative
rename detection, first-parent-chain trajectories, `refs/mappings/import/` with trailer
re-linking and hostile-trailer rejection, idempotent re-import), and `gemel clone`.
Proven by `tests/git_interop_tests.rs` (9 courts): byte-deterministic export, identical
re-import, Gemel→Git→Gemel identity re-link, Git→Gemel→Git content/topology/author/
timestamp preservation, foreign-history never-fabrication, merge-commit export/import,
and conservative rename detection.

### Phase 5 — Semantic Depth (complete)

**Entry:** Phase 4 complete. **Exit:** language-aware semantic indexing (starting with
Rust) providing identities for module/type/trait/impl/function/method/constant/static/
test/feature/dependency; richer `why`, `diff --semantic`, `attempts`, `impact`.
Language intelligence is an enhancement, never a core storage dependency; arbitrary
files work without it.

Delivered: two new canonical families — `semantic-entity` (kind, name, module path,
file span, signature, visibility, explicit lineage: `lineage_from`/`lineage_evidence`/
`lineage_certainty` ∈ {observed, possible, unknown}, state, producer, created-at) and
`semantic-index` (state → entity list) — plus a deterministic lexical Rust extractor
(`src/semantic/rust.rs`: strings, raw strings, char literals, line/nested-block
comments, attributes, lifetimes, generics; `pub fn`/`struct`/`enum`/`union`/`trait`/
`impl Trait for Type`/`mod` (inline + file)/`const`/`static`/`use`/`macro_rules`/
`extern "C" fn`/`type` aliases; nested inline modules). `gemel index` builds the index
of a state (head by default); `gemel semantic` resolves entities by name, `path::name`,
`file:line`, or identity; `gemel diff --semantic` reports added/removed/modified/moved
entities (moves only via explicit recorded lineage); `gemel why` and `gemel attempts`
resolve semantic subjects and match across lineage aliases, so moved entities surface
the work that touched their ancestors. The indexer producer is published, so fsck
reachability and exchange export carry the semantic graph; the exchange frontier now
seeds `refs/semantic/head` and ingest re-establishes the semantic refs on activation,
so a fresh Git clone recovers identical semantic identities. Proven by
`tests/phase5_tests.rs` (12 courts): extraction correctness, index determinism and
dedup, observed-vs-possible lineage, file-movement survival via aliases, semantic
diff, trait/test extraction, nested modules, non-Rust files, Cargo.toml features/deps,
index independence, exchange export of the semantic graph, and semantic context
surviving a depth-1 Git clone.

### Phase 6 — Distributed Operation (complete)

**Entry:** Phase 5 complete. **Exit:** remotes, object negotiation, partial fetch,
resumable transfer, integrity verification, multiple producers, authentication,
authorization, repository policy, hosted workflows. Content addressing makes sync
naturally deduplicated. Gemel sync is a separate problem from Git interchange.

Delivered: `gemel remote add|remove|list`, `gemel fetch|push|pull` with native
Gemel↔Gemel sync and Git-only remote projection fallback (GIT_INTEROP.md §6). The
`src/sync/` module implements the transport-agnostic protocol: public-ref policy
(local-only namespaces never travel), `gemlpack` (GMLP v1) verified transfer packs,
content-identity negotiation (`reachable_ids`/`missing_ids`), end-to-end per-record
integrity (corruption and same-id-different-bytes fail closed with local state
untouched), `refs/remotes/<name>/*` tracking, and fetch + fast-forward pull that
refuses divergence. Network transports implement the same six-operation trait
(TLS mandatory for non-local remotes; THREAT_MODEL.md §10). Proven by
`tests/phase6_tests.rs` (10 courts): identical identities across machines, negotiation
dedup and idempotence, tracking refs, multi-producer and semantic-object transport,
corrupt-remote fail-closed, conflicting-identity fatality, divergence preservation,
Git-only projection round-trip, non-repository fail-closed, and fsck-clean sync.

### Phase 7 — Agent Protocol / Hosted Workflows (complete)

**Entry:** Phase 6 complete. **Exit:** the lightweight agent protocol interface
(brief §15.4), hosted workflows, repository policy enforcement, and deeper FRF court
integration. (Roadmap placeholder; exact scope is refined against the Phase 6
postmortem.)

Delivered (the substrate-first slice; hosted servers are Phase 8):

- **`gemel protocol`** (`src/protocol.rs`): a bounded, line-delimited JSON session
  over stdin/stdout — `status`, `next`, `why`, `semantic`, `claims`, `evidence`,
  `residuals`, `attempts`, `context`, `log`, `index` — with stable error codes and
  strict request parsing. It is session framing over the existing query layer, never
  a parallel ontology.
- **`gemel next`** (brief §57): recommendations derived purely from durable state —
  pending change → `continue`; open residuals → `resolve`; blocked claims →
  `verify`; required-but-missing verification → `verify` (possible); unindexed head
  → `index`; failed attempts → `inspect`; stale exchange context → `reconcile`.
  Never fake intelligence: every recommendation carries a derived rationale and an
  explicit certainty.
- **`gemel policy`**: the required-verification matrix (config `required_verification`,
  OBJECT_MODEL.md §6.21) with evidence-based gap detection; missing required
  verification makes readiness `NOT_READY` (OBJECT_MODEL.md §8.4).
- Proven by `tests/phase7_tests.rs` (6 courts): protocol routing and error codes,
  malformed/oversized request rejection, honest next recommendations, blocked-claim
  verification, policy-driven readiness, and protocol/CLI consistency.

### Phase 8 — Hosted Workflows & Network Transports (complete)

**Entry:** Phase 7 complete. **Exit:** network transports (SSH/HTTP) implementing the
Phase 6 transport trait with mutual auth and capability-scoped grants (THREAT_MODEL
§10), hosted sync, repository policy enforcement servers, and deeper FRF court
integration. Normative protocol: `docs/HOSTED.md`.

Delivered:

- **Remote URL grammar** (`src/sync/transports.rs`): `ssh://[user@]host[:port]/path`,
  `http://[token@]host[:port]/path` (port default 80), and local paths; `https://` is
  rejected fail-closed (TLS is a proxy concern; THREAT_MODEL §10). Malformed ports are
  errors, never silently swallowed; omitted ssh ports defer to `ssh(1)`. `gemel
  remote add` validates URLs strictly; `--init` applies to local paths only.
- **`gemel serve`**: the stdio session (the SSH transport backend —
  `ssh host gemel serve <path>`, argv-safe) and `--http` hosted server with
  bearer-token capability grants (`read`/`write`), `--read-only` enforcement,
  `--root` multi-repository hosting with single-segment repo names and traversal
  rejection, and non-loopback-without-tokens startup refusal.
- **The bounded session protocol** (`src/sync/session.rs`): line-delimited JSON
  `list_refs`/`reachable`/`missing`/`update_refs` plus raw-`gemlpack` `fetch`/`push`
  with `pack_len` framing; limits (64 KiB lines, 4 GiB packs, 10M ids); push packs
  decoded, schema-validated, and content-verified before insertion; `update_refs`
  validated (public refs, full closure) and journaled atomically.
- **Transports implement the Phase 6 trait** with `&mut self` methods (sessions own a
  read position); FileTransport, SshTransport, and HttpTransport are interchangeable;
  CLI `fetch`/`push`/`pull` accept names, URLs, or paths, and Git-only paths still
  receive the deterministic projections (GIT_INTEROP.md §6).
- **The FRF court runner** (`src/court.rs`; brief §38): `gemel court <evidence-id>`
  re-executes the recorded reproduction command under `config.execution_policy`
  (default `never_auto_execute`; `policy_gated` needs `--allow`; `allowlist` checks
  `.gemel/court.allowlist`, `*` prefix patterns). The fresh observation is a new
  evidence object — `court-runner` producer, outcome/exit_code, replayable
  reproduction record, `evaluated_state` = head — and ingestion/status/sync/protocol
  never execute anything.
- Proven by `tests/phase8_tests.rs` (16 courts): URL grammar and fail-closed forms,
  HTTP roundtrip with identical identities, idempotent re-push, capability auth
  (missing/wrong token 401, read-only grants, read-only servers), non-loopback
  fail-closed, `--root` multi-repo serving with traversal rejection, SSH-equivalent
  sessions over the local binary, CLI push/pull over an HTTP URL, the court policy
  matrix (default deny, `--allow`, allowlist, timeout → inconclusive, nothing
  executes during status), and policy-gap readiness through `--json`.

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

### 11.6 Phase 2 Conformance Matrix

Every §43 requirement of the brief, bound to its implementation. Verified by
`tests/phase2_tests.rs` (10 integration tests) including the §12 acceptance demo.

| Brief requirement (Phase 2) | Implementation |
|---|---|
| `gemel why <subject>` | `query::why` — subject → Change → Intent → Claim → Evidence → Residual (AGENT_PROTOCOL.md §5.2) |
| `gemel claims` | `query::claims` — subject/status filters, derived statuses, cursor pagination (§5.3) |
| `gemel evidence <id>\|--subject` | `query::evidence_show`/`evidence_for_subject` with derived freshness (§5.4) |
| `gemel residuals` | `query::residuals` — latest chain version, disposition/persistence, filters (§5.5) |
| `gemel attempts <subject>` | `query::attempts` — touching + intent-sharing trajectories, outcomes (§5.6) |
| `gemel trajectory <id>` | `query::trajectory_detail` — materialized change sequence, handoff (§5.7) |
| `gemel trajectory close` | `workflow::close_trajectory` — chained outcome version; closed ⇒ terminal |
| `gemel checkpoint` | `workflow::create_checkpoint` — machine-generated from `query::checkpoint_plan` (§9.2) |
| `gemel context <subject>` | `query::context_bundle` — phased, budget-bounded, deduplicated, deterministic (§6) |
| `gemel residual resolve` | `workflow::resolve_residual` — chained disposition event (§8.3) |
| Machine-readable JSON forms | `gemel.query.v1` envelope on every command; paged envelopes with cursors (§4) |
| Derived statuses over stored truth | claim status, residual disposition, readiness — always computed (§8) |
| Progressive disclosure | context bundle levels L1/L2, expansion pointers, `omitted` (§6.1, §6.4) |
| Negative knowledge surfaced | `attempts`/`why.previous_approaches` preserve rejected/interrupted trajectories (§7) |
| Two-agent acceptance demo | `acceptance_demo_two_agents` (§12): T17 rejected, T18 interrupted, checkpoint, context, T19 |

### 11.7 Phase 3 Conformance Matrix

Every §44 requirement of the brief, bound to its implementation. Verified by
`tests/phase3_tests.rs` (9 integration tests) including concurrent agents from the
same base State.

| Brief requirement (Phase 3) | Implementation |
|---|---|
| `gemel reconcile` | `reconcile::reconcile` (`src/reconcile.rs`) |
| `gemel reconcile --plan` | `reconcile::analyze` — pure, deterministic, read-only (AGENT_PROTOCOL.md §5.10) |
| Textual changes | operation subject paths incl. rename from/to; per-path ownership |
| Path changes | merged file map = base + adopted deltas in application order |
| Claims | retained (adopted) / invalidated (rejected touching adopted subjects, `possible`) |
| Evidence | `evidence_retained` from adopted changes |
| Residuals | open → carried forward; resolved → `resolved_residuals` (latest chain versions) |
| Reconciliation object | adopted / rejected / unresolved / interactions / rationale / resulting State + Change (OBJECT_MODEL.md §6.17) |
| Resulting State | deterministic merge; byte-identical between plan identity and execution |
| Concurrent agents from one base | named workspaces + `--worktree` (brief §34) |
| Uncertainty, never invented | `certainty: observed` textual, `certainty: possible` claim interactions |
| Fail-closed on divergent bases | trajectories without a common base are refused with a clear error |
| `--apply` | advances `refs/head`, `refs/state/head`, and the workspace |

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

---

## 18. Phase 2 Exit Statement

Phase 2 is complete when: `why`, `claims`, `evidence`, `residuals`, `attempts`,
`trajectory`, `checkpoint`, and `context` operate with deterministic machine-readable
JSON forms; the §12 acceptance demo works end-to-end — Agent A discovers a rejected
attempt (T17), records a claim with mixed evidence and an open residual, stops at a
checkpoint; Agent B, receiving only the repository and the intent, retrieves the
smallest sufficient context (T17 rejected, T18 interrupted, C1 partially supported,
E1/E2, R1), resolves the residual, and spawns a fresh attempt; and every row of the
§11.6 matrix is satisfied.

Phase 2 exit verified: `cargo test` green (78 unit, 2 golden, 11 Phase 1, 10 Phase 2
integration), `cargo clippy --all-targets` zero warnings, `cargo fmt --check` clean,
golden vectors unchanged, and the acceptance demo exercised both through the library
API and the CLI JSON surface.

---

## 19. Phase 3 Exit Statement

Phase 3 is complete when: `gemel reconcile` operates over textual changes, path
changes, explicit Claims, Evidence, and Residuals; produces a Reconciliation object
recording adopted / rejected / unresolved / verification-required / resulting State
while never erasing its inputs; concurrent agents work from the same base State in
separate workspaces; divergent bases are refused fail-closed; uncertainty is exposed
with calibrated `certainty` rather than invented; and every row of the §11.7 matrix is
satisfied.

Phase 3 exit verified: `cargo test` green (78 unit, 2 golden, 11 Phase 1, 10 Phase 2,
9 Phase 3 integration), `cargo clippy --all-targets` zero warnings, `cargo fmt --check`
clean, golden vectors unchanged, and `gemel reconcile T1 T2 [--plan|--apply]` exercised
through both the library API and the CLI.
