# Gemel

**Evidence-native version control for agentic software development.**

Gemel is a distributed version-control system built from first principles for agentic
software engineering. Its native unit of version control is an **evidence-bearing
change**: a durable record of what was attempted, against which exact state, by which
producer, through which operations, with which claims, supported by which evidence,
contradicted by which residuals, and resulting in which state.

Gemel is a machine-usable memory substrate for software engineering — a repository that answers not only *what changed, when, by whom*, but *what was attempted, why, against what exact state, what the change claims, what evidence supports it, what contradicts it, what remains unexplained, which alternatives failed, and what the next agent should inspect*.

Git stores surviving snapshots and ancestry. Gemel preserves intent, developmental
trajectory, intermediate operations, provenance, agent execution identity, claims,
evidence, residual disagreement, failed attempts, verification scope, reconciliation
decisions, and exact reproducibility information — in a content-addressed, immutable,
deterministically encoded object model implemented in native Rust.

## Status

**Phase 7 — Agent Protocol & Workflow Intelligence — complete.** Next: Phase 8
(hosted workflows / network transports).

**Phase 6 — Distributed Operation — complete.**

**Phase 5 — Semantic Depth — complete.**

**Phase 4 — Git Interchange — complete.**
**Phase 1.5 — Git-Carried Exchange Rollups — complete.**

**Phase 3 — Reconciliation — complete.**

**Phase 2 — Agent-Native Value — complete.**

**Phase 1 — Minimal Useful Gemel — complete.**

**Phase 0 — Specification before features — complete.**

- Seven normative documents (`docs/`): SPECIFICATION, OBJECT_MODEL, STORAGE,
  INVARIANTS, GIT_INTEROP, AGENT_PROTOCOL, THREAT_MODEL.
- Canonical encoding (GCE): deterministic, fail-closed, extension-preserving binary
  grammar (the `gemel` crate, `src/`).
- BLAKE3-256 object identity; twenty-four object families specified field-by-field.
- Executable golden fixtures (`golden/`) pinning canonical bytes and identities, with a
  regeneration policy.

**Phase 1 delivers the minimal useful system** on a local, offline store:

- `gemel init` — repository creation with canonical default producer + config.
- `gemel status` — trajectory, intent, working-tree delta, claims, residuals, readiness.
- `gemel snapshot` — record the working tree as a content-addressed state.
- `gemel change begin` / `change finish` — the evidence-bearing change workflow:
  intent, operations, resulting state, basic claims/evidence/residuals, trajectory.
- `gemel log`, `show`, `diff` (deterministic Myers), `checkout`, `fsck`.
- Immutable, sharded, hash-verified object store with journaled atomic refs.
- A disposable SQLite derived index (rebuildable; fsck detects drift).
- Retention tiers and tombstones (GC pass is Phase 2+).
- `--json` output on every command (`gemel.query.v1` envelope).
- The demonstration `State S0 → Intent I1 → Trajectory T1 → Change C1 → State S1` with
  byte-exact content-addressed reconstruction, proven by `tests/phase1_tests.rs`.

**Phase 2 delivers the agent-native query surface** Git fundamentally lacks:

- `gemel why <subject>` — causal blame: subject → Change → Intent → Claim → Evidence →
  Residual → previous approaches (rejected/interrupted trajectories preserved).
- `gemel claims` / `evidence` / `residuals` — filtered, paginated, with derived
  statuses (never stored truth), dispositions, and freshness (`MAY_REQUIRE_REFRESH`).
- `gemel attempts <subject>` — what was tried, why it ended, its evidence/residuals.
- `gemel trajectory <id>` — the full materialized change sequence + handoff;
  `trajectory close` publishes a chained outcome (closed trajectories are terminal;
  new work on the same intent spawns a fresh attempt).
- `gemel checkpoint` — a machine-generated continuation boundary (intent, trajectory,
  state, open claims, unresolved residuals, important evidence, continuation scope).
- `gemel context <subject> --budget N` — the smallest sufficient context: phased,
  budget-bounded, deduplicated bundles with progressive-disclosure levels.
- `gemel residual resolve` — chained disposition events (open/acknowledged/resolved/…).
- The §52 acceptance demo — two agents, no shared conversation — proven by
  `tests/phase2_tests.rs`.

**Phase 3 delivers reconciliation and concurrent workspaces:**

- `gemel reconcile T1 T2 [--plan|--apply]` — merges trajectories from a common base
  into a chosen direction: adopted / rejected / unresolved residuals / claims
  retained + invalidated / verification required / resulting State.
- Per-path first-input-trajectory-wins adoption; textual conflicts with
  `certainty: observed`; claim interactions with `certainty: possible` — uncertainty
  is exposed, never invented.
- `gemel reconcile --plan` — pure, deterministic, read-only (the resulting state
  identity is computed in memory, nothing is published).
- Inputs are never erased: rejected changes and their claims/evidence/residuals stay
  canonical (negative knowledge, brief §7).
- **Named workspaces + `--worktree`** (brief §34): `change begin/finish --workspace wa
  --worktree /path/wa` — concurrent agents never serialize merely to avoid filesystem
  collisions; proven by `tests/phase3_tests.rs`.

**Phase 1.5 delivers Git-carried exchange rollups** — Gemel's richer model travels
through ordinary Git infrastructure without reducing Gemel to Git's ontology:

- Immutable, content-addressed **packs** (`GXPK`, `gemel.exchange.pack.v1`) and
  **Frontier Descriptors** (`gemel.exchange.frontier.v1`) under `.gemel/exchange/v1/`
  — append-only publication (packs first, frontier last), deterministic partitioning
  (256 KiB target), no timestamps/hostnames/git-commit-ids in descriptors.
- A narrowly scoped `.gemel/.gitignore` keeps the native store invisible to Git while
  the exchange namespace is tracked (`git status` stays clean).
- **Quarantine ingestion**: every artifact is hostile input — identity verification,
  schema checks, resource limits, bounded parent chains, no execution, idempotent;
  conflicts (same id, different bytes) are fatal (§42).
- **Source-state binding**: the frontier's `source_state` is the pure-content State of
  the checked-out tree; a Git-only source change flips the context to
  `CONTEXT_STALE` / `SOURCE_CONTEXT_DIVERGED` instead of pretending old evidence
  verifies new source.
- **Zero-ceremony bootstrap**: `git clone && gemel status --json` restores the
  engineering frontier (trajectory, intent, claims, residuals, readiness) with no
  `gemel init`, no server, no manual import; imported names (T1/C1/S1) continue the
  local counters.
- `gemel exchange status|export|ingest|verify` (frontier/portable profiles,
  `--working-tree`/`--git-index` verification for CI), auto-export on `change finish`
  inside Git worktrees, and `gemel fsck` reporting `native_store` + `exchange_transport`
  sections with `exchange-omitted` warnings distinct from corruption.
- 33 integration courts with **real Git repositories**: transport, shallow clone,
  branch-merge unions, Git-only mutation, corruption classes, idempotence, byte
  determinism across paths, non-interference, git cleanliness, index independence,
  incremental export, golden fixtures — `tests/exchange_tests.rs`.
- Normative exchange protocol: `docs/EXCHANGE.md` (a second implementation must be
  possible from the specification alone).

**Phase 4 delivers deterministic Git interchange** (GIT_INTEROP.md):

- `gemel export-git` — projects the head's causal-parent closure into ordinary Git
  commits (loose objects via the pure-Rust `git_io`), with `GEMEL-CHANGE`/`INTENT`/
  `TRAJECTORY`/`CLAIM`/`EXPORT-VERSION` trailers, deterministic authors/timestamps
  (no wall clock), and canonical `mapping` objects under `refs/mappings/export/`.
- `gemel import-git` — topological import of real Git histories (packed or loose):
  states from Git trees, deterministic operations with conservative rename
  detection, first-parent-chain trajectories, synthetic `git_import`/`human`/
  `unknown` producers, `refs/mappings/import/` with trailer re-linking, hostile
  trailer rejection, and idempotent re-import. Provenance Git cannot supply is
  `unknown`, never fabricated.
- `gemel clone <url>` — git clone + native store + import in one step.
- The provable round-trip core: trees/topology/messages/authors/timestamps survive
  Git→Gemel→Git exactly; identity linkage survives Gemel→Git→Gemel through trailers
  (re-import into the originating repository re-links the original objects).
- 9 courts in `tests/git_interop_tests.rs` using real Git repositories.

**Phase 5 delivers language-aware semantic depth** (OBJECT_MODEL.md §6.23–§6.24;
brief §22–§24, §13):

- Two new canonical families — `semantic-entity` and `semantic-index` — published by
  `gemel index` (deterministic; unchanged entities deduplicate by content identity).
- A deterministic lexical Rust extractor (`src/semantic/rust.rs`): strings, raw
  strings, char literals, line/nested-block comments, attributes, lifetimes,
  generics; `pub fn`/`struct`/`enum`/`union`/`trait`/`impl Trait for Type`/`mod`
  (inline + file)/`const`/`static`/`use`/`macro_rules`/`extern "C" fn`/`type`
  aliases; nested inline modules.
- **Explicit lineage, never inferred identity**: edited entities link
  `lineage_from` with certainty `observed` (`same-name-kind-path`); moved entities
  link with certainty `possible` (`similarity:same-name-kind`) and a documented
  evidence string. Semantic identity survives file movement without silent merges.
- `gemel semantic <subject>` — resolve entities by name, `path::name`, `file:line`,
  or identity; `gemel diff --semantic` — added/removed/modified/moved entities with
  body-digest detection of edits; `gemel why`/`attempts` — resolve semantic subjects
  and match across lineage aliases so moved entities surface the work that touched
  their ancestors.
- The indexer producer is published, fsck stays clean, and the exchange frontier
  carries the semantic graph: a depth-1 `git clone` + `gemel status --json`
  re-establishes `refs/semantic/*` over the imported objects with **identical
  identities** (12 courts in `tests/phase5_tests.rs`).

**Phase 6 delivers native distributed operation** (DISTRIBUTED.md; STORAGE.md §10):

- `gemel remote add|remove|list`, `gemel fetch|push|pull` — transport-agnostic sync:
  content-identity negotiation (`reachable_ids`/`missing_ids`), verified `gemlpack`
  transfers, and atomic validated ref publication. A re-push transfers nothing;
  resumed fetches re-negotiate from the new have-set.
- **Public-ref policy**: only knowledge refs travel (head, names, trajectories,
  semantic indexes, …); workspace, exchange-marker, and mapping bookkeeping never
  leaves home. Fetched refs track under `refs/remotes/<name>/*`.
- **Fail-closed integrity**: every envelope is re-verified end-to-end; a corrupt
  remote aborts with local state untouched; same-id different-bytes is fatal; a
  diverged `pull` refuses and preserves local work for `gemel reconcile`.
- Git-only remotes receive the deterministic export/import projections.
- 10 courts in `tests/phase6_tests.rs` (identical identities across machines,
  negotiation dedup, multi-producer + semantic transport, corruption, conflict,
  divergence, Git projection, fsck-clean sync).

**Phase 7 delivers the agent protocol & workflow intelligence** (brief §15.4, §57):

- `gemel protocol` — a bounded line-delimited JSON session over stdin/stdout
  (status/next/why/semantic/claims/evidence/residuals/attempts/context/log/index)
  with stable error codes: agents query Gemel without scraping terminal prose. It is
  session framing over the existing query layer — never a parallel ontology.
- `gemel next` — recommendations derived purely from durable state: continue a
  pending change, resolve open residuals, verify blocked claims, index the head
  state, inspect failed attempts, reconcile stale context. Every recommendation
  carries a derived rationale and an explicit certainty; nothing is invented.
- `gemel policy` — the required-verification matrix (config §0x08) with evidence-
  based gap detection; missing required verification makes readiness `NOT_READY`.
- 6 courts in `tests/phase7_tests.rs`.

## Reading order

1. `docs/SPECIFICATION.md` — purpose, principles, architecture, conformance matrices.
2. `docs/OBJECT_MODEL.md` — the ontology: encoding, identity, all object families.
3. `docs/STORAGE.md` — persistence, refs, indexes, retention, fsck.
4. `docs/INVARIANTS.md` — the complete correctness contract.
5. `docs/GIT_INTEROP.md` — deterministic Git interchange.
6. `docs/AGENT_PROTOCOL.md` — the machine query surface + agent session protocol.
7. `docs/DISTRIBUTED.md` — native sync protocol (Phase 6).
8. `docs/EXCHANGE.md` — Git-carried exchange rollups (Phase 1.5).
9. `docs/THREAT_MODEL.md` — security model and fail-closed catalog.

## Layout

```text
src/                    the single `gemel` crate
├── lib.rs              crate root
├── primitive layer     varint, family table, Gid, hex, limits, value model
├── encoding layer      spec tables, encode/decode, hashing, validation, JSON projection
├── store/              object store, refs+journal, lock, index, tombstone, retention, fsck
├── content.rs          working tree ↔ state, tree deltas, operation synthesis, Myers diff
├── workflow.rs         change begin/finish, intents, trajectories, checkpoint,
│                       trajectory close, residual resolve
├── semantic/          Phase 5: deterministic Rust extractor, entity/index objects,
│                      lineage, semantic diff/resolution
├── sync/              Phase 6: gemlpack transfer format, transport trait,
│                      negotiation, fetch/push/pull, remotes config
├── protocol.rs        Phase 7: bounded agent session protocol (stdin/stdout JSON)
├── query.rs           log/show/status, why/claims/evidence/residuals/attempts/
│                      trajectory/context bundles, derived statuses, pagination
├── exchange/           Phase 1.5: pack/frontier encoding, deterministic export,
│                       quarantine ingest, source-state binding (EXCHANGE.md)
├── git_adapter.rs      isolated `git` index/staged-tree adapter (argv-safe)
├── git_io.rs           pure-Rust loose-object/tree/commit read-write (Phase 4 base)
├── ignore.rs           .gitignore matcher (documented subset of git semantics)
├── defaults.rs         default producer/config/retention builders
├── golden/             executable golden fixture definitions
└── bin/
    ├── gemel.rs        the CLI (init status snapshot change log show diff checkout fsck
    │                   why claims evidence residuals attempts trajectory checkpoint
    │                   context reconcile exchange)
    └── golden-gen.rs   golden vector generator
golden/                 pinned golden vectors (canonical bytes + identities)
docs/                   the normative specification set
tests/                  Phase 1 + 2 + 3 + 1.5 + 4 + 5 + 6 + 7 integration suites (the acceptance demos)
```

Layering is disciplined within the crate: primitives → encoding → schema → fixtures →
store → workflow/query → CLI; nothing depends upward.

## Validation

```sh
cargo test                       # unit + golden + Phase 1/2/3/1.5 integration suites
cargo clippy --all-targets       # zero warnings
cargo run --bin golden-gen       # golden vectors up to date (protocol-change only)
```

Golden vectors must never be regenerated casually: any change to a pinned digest is a
breaking protocol change (`docs/OBJECT_MODEL.md` §10.4).

## Quick demo

```sh
gemel init
gemel snapshot                 # S1
gemel change begin --intent-summary "Fix parser compatibility"
# ... edit files ...
gemel change finish --summary "Add pointer-loop rejection"
gemel log --json
gemel status
gemel diff S1 S2 --stat
gemel fsck
```
