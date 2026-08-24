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

**Phase 2 — Agent-Native Value — complete.** Next: Phase 3 (Reconciliation).

**Phase 1 — Minimal Useful Gemel — complete.**

**Phase 0 — Specification before features — complete.**

- Seven normative documents (`docs/`): SPECIFICATION, OBJECT_MODEL, STORAGE,
  INVARIANTS, GIT_INTEROP, AGENT_PROTOCOL, THREAT_MODEL.
- Canonical encoding (GCE): deterministic, fail-closed, extension-preserving binary
  grammar (the `gemel` crate, `src/`).
- BLAKE3-256 object identity; twenty-two object families specified field-by-field.
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

## Reading order

1. `docs/SPECIFICATION.md` — purpose, principles, architecture, conformance matrices.
2. `docs/OBJECT_MODEL.md` — the ontology: encoding, identity, all object families.
3. `docs/STORAGE.md` — persistence, refs, indexes, retention, fsck.
4. `docs/INVARIANTS.md` — the complete correctness contract.
5. `docs/GIT_INTEROP.md` — deterministic Git interchange.
6. `docs/AGENT_PROTOCOL.md` — the machine query surface.
7. `docs/THREAT_MODEL.md` — security model and fail-closed catalog.

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
├── query.rs            log/show/status, why/claims/evidence/residuals/attempts/
│                       trajectory/context bundles, derived statuses, pagination
├── ignore.rs           .gitignore matcher (documented subset of git semantics)
├── defaults.rs         default producer/config/retention builders
├── golden/             executable golden fixture definitions
└── bin/
    ├── gemel.rs        the CLI (init status snapshot change log show diff checkout fsck
    │                   why claims evidence residuals attempts trajectory checkpoint context)
    └── golden-gen.rs   golden vector generator
golden/                 pinned golden vectors (canonical bytes + identities)
docs/                   the normative specification set
tests/                  Phase 1 + Phase 2 integration suites (the acceptance demos)
```

Layering is disciplined within the crate: primitives → encoding → schema → fixtures →
store → workflow/query → CLI; nothing depends upward.

## Validation

```sh
cargo test                       # unit + golden + Phase 1 integration suites
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
