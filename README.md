# Gemel

**Evidence-native version control for agentic software development.**

Gemel is a distributed version-control system built from first principles for agentic
software engineering. Its native unit of version control is an **evidence-bearing
change**: a durable record of what was attempted, against which exact state, by which
producer, through which operations, with which claims, supported by which evidence,
contradicted by which residuals, and resulting in which state.

Gemel is **not** a Git wrapper, not an AI commit-message generator, not a metadata
sidecar, and not a conventional VCS rewritten in Rust. It is a durable, machine-usable
memory substrate for software engineering — a repository that answers not only *what
changed, when, by whom*, but *what was attempted, why, against what exact state, what
the change claims, what evidence supports it, what contradicts it, what remains
unexplained, which alternatives failed, and what the next agent should inspect*.

Git stores surviving snapshots and ancestry. Gemel preserves intent, developmental
trajectory, intermediate operations, provenance, agent execution identity, claims,
evidence, residual disagreement, failed attempts, verification scope, reconciliation
decisions, and exact reproducibility information — in a content-addressed, immutable,
deterministically encoded object model implemented in native Rust.

## Status

**Phase 0 — Specification before features — complete.**

- Seven normative documents (`docs/`): SPECIFICATION, OBJECT_MODEL, STORAGE,
  INVARIANTS, GIT_INTEROP, AGENT_PROTOCOL, THREAT_MODEL.
- Canonical encoding (GCE): deterministic, fail-closed, extension-preserving binary
  grammar (the `gemel` crate, `src/`).
- BLAKE3-256 object identity; twenty-two object families specified field-by-field.
- Executable golden fixtures (`golden/`) pinning canonical bytes and identities, with a
  regeneration policy.

Next: **Phase 1 — Minimal Useful Gemel** (`init`, `status`, `snapshot`, `change`,
`log`, `show`, `diff`, `fsck` on a local store).

## Reading order

1. `docs/SPECIFICATION.md` — purpose, principles, architecture, conformance matrix.
2. `docs/OBJECT_MODEL.md` — the ontology: encoding, identity, all object families.
3. `docs/STORAGE.md` — persistence, refs, indexes, retention, fsck.
4. `docs/INVARIANTS.md` — the complete correctness contract.
5. `docs/GIT_INTEROP.md` — deterministic Git interchange.
6. `docs/AGENT_PROTOCOL.md` — the machine query surface.
7. `docs/THREAT_MODEL.md` — security model and fail-closed catalog.

## Layout

```text
src/                    the `gemel` crate
├── lib.rs              crate root
├── family|gid|…        canonical primitives (varint, family table, Gid, hex, limits)
├── spec|encode|…       canonical encoding, schema tables, hashing, validation
├── golden/             executable golden fixture definitions
└── bin/golden-gen.rs   golden vector generator
golden/                 pinned golden vectors (canonical bytes + identities)
docs/                   the normative specification set
```

Layering is disciplined within the crate: primitives → encoding → schema → fixtures;
nothing depends upward.

## Validation

```sh
cargo test               # encoding determinism, fail-closed catalog, golden vectors
cargo run --bin golden-gen   # regenerate vectors (protocol-change only)
```

Golden vectors must never be regenerated casually: any change to a pinned digest is a
breaking protocol change (`docs/OBJECT_MODEL.md` §10.4).
