# Gemel — Storage

Status: **Normative.** Version 1.0.0.

This document defines how the canonical object model (OBJECT_MODEL.md) is persisted:
repository layout, the object store and its atomicity guarantees, the mutable ref
namespace, derived indexes, workspaces, retention and GC, `fsck`, concurrency, crash
safety, transfer formats (Phase 6 design), and performance targets.

Architectural rule (brief §30–§31):

> Canonical repository state = content-addressed immutable objects + mutable refs.
> Everything else is a **derived index**, disposable and rebuildable from canonical
> objects alone.

---

## 1. Design Goals

1. **Canonical objects are immutable and self-verifying.** Any byte read as an object
   is re-hashed and schema-validated before use.
2. **Atomic publication.** An object is either fully present and correct or not present
   at all. Ref updates are atomic. Interrupted operations never corrupt.
3. **Index corruption is never history loss.** A derived index can always be rebuilt.
4. **Concurrent readers, controlled writers.** Many agents and humans read
   simultaneously; writers serialize on a repository lock for ref transactions, while
   object insertion is lock-free (content-addressed inserts are independent).
5. **Local-first.** Everything works with no network.
6. **Bounded.** All reads are subject to the limits in OBJECT_MODEL.md §5 and
   THREAT_MODEL.md §5.

---

## 2. Repository Layout

A Gemel repository is a directory; by convention the repository metadata lives in
`.gemel/` at the repository root. The working tree (if any) is outside `.gemel/` and is
a *workspace* concern (§6).

```text
.gemel/
├── config.ref                 ; ref file → config object (head config)
├── head.ref                   ; ref file → head change (or absent)
├── lock                       ; writer lock (flock/O_EXCL semantics)
├── journal/
│   └── 0000000001.log         ; append-only ref-transaction journal
├── objects/
│   ├── ab/
│   │   ├── ab12…cd.gce        ; canonical envelope bytes
│   │   └── ab12…cd.tomb       ; tombstone (pruned blob, still referenced)
│   └── …
├── refs/
│   ├── names/                 ; human names → any gid
│   ├── trajectories/          ; labels → trajectory gid
│   ├── cases/                 ; labels → case gid
│   ├── releases/              ; names → release gid
│   ├── remotes/               ; (Phase 6) remote tracking
│   └── …
├── index/
│   ├── gemel.db               ; SQLite derived index (disposable)
│   └── LOCK                   ; sqlite locking (internal)
└── worktrees/
    └── <workspace-id>/        ; workspace metadata (§6)
```

Conventions:

- Object files are named by their 64-hex identity, sharded by the first byte:
  `objects/<2-hex>/<64-hex>.gce`.
- Ref files contain exactly one line: the textual object identity (§OBJECT_MODEL 3.1)
  followed by `\n`. Absence of the file = no ref.
- Nothing under `.gemel/` is ever created by copying from the working tree; content
  enters the store only through the validated insertion path (§3).

---

## 3. Object Store

### 3.1 Insertion protocol

1. Construct the object's canonical bytes (encoder; OBJECT_MODEL §1).
2. Validate against the schema (fail closed) and limits.
3. Compute `ObjectId = BLAKE3-256(bytes)`.
4. Write `objects/<shard>/<id>.gce` to a temporary file in the same directory,
   `fsync` the file.
5. Verify the written bytes by re-reading and re-hashing (or verify the write
   returned exactly the expected bytes and length).
6. `rename` the temp file to the final name (atomic on POSIX).
7. `fsync` the containing directory (durability of the rename).
8. If the target already exists with identical bytes (deduplicated insert), the write
   is a no-op; if it exists with different bytes, that is **corruption** (hash
   collision or bit rot) and the insert fails closed.

Insertion is lock-free: concurrent writers inserting distinct objects cannot conflict;
identical objects deduplicate to the same file.

### 3.2 Read path

1. Open `objects/<shard>/<id>.gce`; if absent, check for `<id>.tomb` (tombstone,
   §7.6) — report `Pruned` vs `Missing`.
2. Read with a size bound (limits §5).
3. Re-hash; mismatch ⇒ `Corrupt` (never trust the filename).
4. Parse and schema-validate; fail closed on any violation.

Readers never take the writer lock. Object files are immutable once published, so
concurrent reads are safe without synchronization.

### 3.3 Crash safety

- Object files appear atomically (temp + rename) and are verified before rename.
- A crash before rename leaves only a temp file, which GC removes.
- Ref updates are journaled (§4.2): the journal entry is durably written before the ref
  file is touched; recovery replays or rolls back.
- Interrupted GC is safe because pruning always happens after object removal ordering
  rules (§7.4) and tombstones are created before the blob is unlinked.

---

## 4. Refs: Mutable Names

Refs are the only mutable state in the canonical layer. They point names at immutable
identities (OBJECT_MODEL §3.2).

### 4.1 Namespaces

| Namespace | Contents |
|---|---|
| `refs/head` | head change (latest material change) |
| `refs/state/head` | derived convenience: head change's resulting state (maintained transactionally with head) |
| `refs/config` | head config object |
| `refs/names/*` | arbitrary human names → any gid |
| `refs/trajectories/*` | trajectory labels (e.g., `T82` → trajectory gid) |
| `refs/cases/*` | case labels |
| `refs/releases/*` | release names |
| `refs/remotes/*` | (Phase 6) remote-tracking refs |
| `refs/mappings/*` | deterministic git-interchange anchors (GIT_INTEROP.md) |

Name syntax: names are UTF-8, non-empty, contain no NUL, no leading/trailing `/`, no
`..` segment, no `.` segment. Path-traversal and ambiguity checks apply
(THREAT_MODEL.md §8).

### 4.2 Atomic ref update and journal

A ref update is a transaction:

1. Acquire the writer lock (`flock` on `.gemel/lock`; exclusive).
2. Append a journal entry: `{op: set|delete, ref: <name>, new: <gid|none>,
   prev: <gid|none>, nonce: <random>, ts: <wallclock ms>}`; `fsync` the journal.
3. Perform the file update (temp + rename + directory fsync).
4. Append a commit marker to the journal; `fsync`.

Recovery (on next open, or by `fsck`): if the last journal entry for a ref is an
uncommitted `set`, the ref file contents are authoritative (the journal is advisory
for multi-ref consistency and audit); if a multi-ref transaction (e.g., head + state)
is interrupted, the journal records the transaction id and recovery replays the
remaining entries or rolls back both. The journal is truncated after a clean checkpoint.

The journal is an audit trail and crash-recovery aid — **never** the source of truth
for ref contents (ref files are; INVARIANTS.md §7).

---

## 5. Derived Indexes

### 5.1 Role

Indexes accelerate queries (brief §32). They are derived; the canonical objects + refs
are the sole source of truth. Any index corruption is repaired by rebuild, never by
history loss.

### 5.2 Backend

Phase 1 uses SQLite (`index/gemel.db`) with WAL mode. The schema:

- `objects(id TEXT PK, family INT, schemever INT, size INT, inserted_at INT)` —
  present only for objects that have been indexed; absence does not imply absence of
  the object (index may be stale).
- `edges(from TEXT, to TEXT, kind TEXT, ordinal INT)` — graph edges (OBJECT_MODEL
  §7.1), used by query.
- `refs(name TEXT PK, gid TEXT)` — cached mirror of ref files.
- `subjects(gid TEXT, subject TEXT, kind TEXT)` — subject strings for `why`/`attempts`
  /`context` queries (paths, entities).
- `claim_index(claim TEXT, subject TEXT)` — claim-by-subject acceleration.
- `meta(key TEXT PRIMARY KEY, value TEXT)` — index schema version etc.

Index schema version is recorded in `meta`; rebuild is triggered when the stored
version is older than the code's.

### 5.3 Rebuild

`gemel fsck --rebuild-index` (and automatic repair on detected inconsistency) walks
all reachable objects from refs and all objects present on disk, and rebuilds every
table. Rebuild is atomic (write to a fresh DB file, rename over the old one).

The rule is absolute: **no query result is ever derived from an index that cannot be
reproduced from canonical objects.**

---

## 6. Workspaces

### 6.1 Separation

The canonical representation (trees/states) is distinct from any working directory.
A workspace binds:

- a **working directory** (OS paths, arbitrary layout),
- a **canonical state** (the state it claims to materialize),
- a **worktree metadata directory** (`.gemel/worktrees/<workspace-id>/`):
  `state.ref` (the materialized state), `mapping` (working path ↔ canonical path
  translation cache), `dirty` status record.

### 6.2 Materialization (checkout)

Given a state: walk its root tree; create files/directories/symlinks with modes from
the tree; reject any canonical path that maps outside the workspace root (path
traversal defense, THREAT_MODEL.md §8). Materialization is deterministic: same state,
same workspace policy ⇒ same bytes and modes. Executable bits and symlinks are
preserved; timestamps are not part of identity (files may carry mtime metadata but
never influence content identity).

### 6.3 Snapshot (working → canonical)

Walk the working directory with the same validation rules in reverse; produce blobs
(structurally shared and deduplicated), trees, and a state. Files are read with
bounded size; special files (sockets, devices) are unsupported and cause a clear
error. Dirty detection compares the current working tree against the workspace's
recorded state (by content hashing, not mtime).

### 6.4 Concurrency of workspaces

Many workspaces may derive from the same state (brief §34). Workspaces are
independent directories; no shared mutable working tree. The logical model never
depends on the physical workspace mechanism (separate directories in Phase 1;
worktrees/overlays/copy-on-write later).

---

## 7. Retention, GC, and Tombstones

### 7.1 Tier attribution

Tiers (OBJECT_MODEL §9). Tier attribution is a policy function over
(family, object metadata, config), computed at GC time:

| Family | Default tier |
|---|---|
| state, change, intent, claim, trajectory, case, residual, verification, reconciliation, release, producer, agentrun, environment, context-manifest, checkpoint, config, mapping, operation, episode, tree | 0 (canonical) |
| evidence objects (identity) | 0 |
| blobs referenced by trees (repository content) | 0 |
| blobs referenced by evidence (fixtures, tool outputs, oracle inputs, logs) | 1 |
| conversation refs, agent summaries, discarded implementation blobs, plans | 2 |
| syscall traces, huge runtime traces, full tool transcripts, debug captures | 3 |

Any object may be explicitly assigned a tier by policy override; unknown assignments
fall back to `config.retention.default_unknown`.

### 7.2 Policy

`config.retention.tiers[]` declares per-tier policy: `retain_forever`,
`retain_policy`, `prune_after_days`, `size_limit_bytes`, `archive_remote`. GC applies
the policy; nothing is pruned before its policy says so, and Tier 0 objects are
pruned only under an explicit `retain_policy` override (default: never).

### 7.3 GC algorithm

1. Compute the **reachable set** by walking refs (all namespaces) transitively over
   the canonical graph. Unreachable objects are collectible.
2. For reachable objects, apply tier policy. A Tier 1–3 blob that is reachable and
   policy-pruned becomes a **tombstone** (§7.6); its *identity* remains canonical
   (the referencing objects are untouched).
3. For unreachable objects: delete after the audit entry is written, unless the object
   is referenced by a tombstone already.
4. Remove stale temp files and truncated journal entries.
5. Write a GC audit entry: objects removed, bytes reclaimed, tombstones created.

GC never deletes an object that is reachable from any ref, and never deletes a
tombstone's referent without a policy override (tombstone deletion requires the
operator to confirm the referent is truly dispensable or archived).

### 7.4 Ordering rules

- Tombstones are created **before** the blob file is unlinked.
- A blob is unlinked only after the tombstone (if required) is durable.
- An object that is still needed by an in-flight transaction (journal) is never
  pruned; GC consults the journal's open transactions.

### 7.5 Compression

There is **no compression inside canonical objects** (identity must not depend on
compression boundaries; OBJECT_MODEL §1.1, §1.7). Compression is permitted only for
transport packs (§10) and for archived remote blobs (compression of the archived copy
is fine; the identity refers to the uncompressed canonical bytes and the archive
records the exact byte length to verify on restore).

### 7.6 Tombstones

A tombstone `objects/<shard>/<id>.tomb` is a JSON document:

```json
{
  "schema": "gemel.tombstone.v1",
  "id": "blob.ab12…cd",
  "family": "blob",
  "size": 123456,
  "pruned_at": "<ISO-8601 UTC>",
  "policy": {"tier": 1, "rule": "prune_after_days"},
  "archive": {"remote": "s3://…", "key": "…", "compressed": false, "bytes": 123456}
}
```

- Reading a tombstoned object yields `Pruned` with the tombstone (never a fabricated
  object).
- `fsck` distinguishes: `missing` (corruption) vs `pruned` (policy) — INVARIANTS §8.
- Canonical identity is never silently broken: referencing objects remain valid; only
  the payload is absent, explicitly.

---

## 8. fsck

`gemel fsck` verifies the repository. Checks (each maps to an invariant;
INVARIANTS.md §12):

1. **Envelope and hash**: every object file parses (§OBJECT_MODEL 1.4) and re-hashes to
   its filename identity.
2. **Schema validity**: full schema validation of every reachable object (fail closed).
3. **References**: every GID in a reachable object resolves to an existing object, a
   valid tombstone, or a declared remote location; unresolved ⇒ `MissingReference`.
4. **Impossible relationships**: cycles in `causal_parents`, append chains, or content
   composition; tree entries with wrong target families; mode/value violations; dup
   tree names; non-minimal encodings; depth/limit violations.
5. **Reachability**: refs resolve; dangling refs ⇒ warning/repair.
6. **Index consistency**: derived index matches objects/refs (drift ⇒ report +
   `--rebuild-index`).
7. **Working-state metadata**: workspace `state.ref` resolves; dirty records are
   consistent.
8. **Journal**: replay/rollback any interrupted transactions; truncate after checkpoint.

Exit codes: 0 clean; 1 repairs made; 2 corruption found. `--repair` fixes rebuildable
artifacts (index, ref cache) and reports what it cannot fix.

---

## 9. Concurrency Model

| Operation | Locking |
|---|---|
| object insert | none (content-addressed, atomic rename) |
| object read | none (immutable) |
| ref read | none (atomic file read) |
| ref write / multi-ref transaction | exclusive writer lock + journal |
| index write | serialized with ref writes (same writer lock) or WAL-consistent batching |
| GC | exclusive writer lock |
| fsck | read-only scan; `--repair` takes the writer lock |

Correctness rule: a reader never observes a partially applied ref transaction (atomic
file replacement) and never observes a partially written object (temp+rename). A
writer never publishes a ref pointing to an object that is not already durable.

---

## 10. Transfer and Pack Format (Phase 6, implemented)

Native synchronization (`gemel remote` / `fetch` / `push` / `pull`; DISTRIBUTED.md) is
separate from Git interchange (brief §47). The Phase 6 design is implemented in
`src/sync/`:

- **Negotiation**: want/have sets are object-id sets compared by content identity
  (deduplicating by construction). `reachable_ids(seeds)` walks canonical gid edges;
  `missing_ids(ids)` filters by local presence. A re-push transfers nothing; a resumed
  fetch re-negotiates from the new have-set.
- **Pack format `gemlpack`** (`src/sync/gemlpack.rs`): magic `GMLP`, version, count,
  total bytes, records `[id 33 bytes][len][envelope]`, ascending id order. Every
  record is verified (`BLAKE3(envelope) == id`, family match) during decode; a single
  failure rejects the whole pack.
- **Refs**: only public refs travel; every published ref has its closure verified
  present (no dangling refs). Fetch tracks under `refs/remotes/<name>/*`; pull is
  fetch + fast-forward and refuses divergence.
- **Integrity**: end-to-end per-record verification on both directions; same-id
  different-bytes is a fatal conflict (THREAT_MODEL.md §11).
- **Transports**: `FileTransport` is shipped; network transports implement the same
  six-operation trait (TLS mandatory for non-local remotes; THREAT_MODEL.md §10).
- **Resumability**: there is no byte-level resume state; the have-set grows as verified
  objects are inserted, so re-negotiation transfers exactly the remainder.

---

## 11. Performance Targets and Amplification

### 11.1 Targets (Phase 1+ benchmarks; brief §49)

| Operation | Target |
|---|---|
| object insert (small, deduped) | ≤ 100 µs amortized |
| state construction (10k files) | ≤ 2 s |
| state materialization (10k files) | ≤ 2 s |
| change lookup by id | ≤ 1 ms |
| why traversal (bounded) | ≤ 50 ms |
| claim/residual lookup by subject | ≤ 10 ms |
| trajectory sequence query | ≤ 10 ms |
| context bundle (32k-token budget) | ≤ 200 ms |
| fsck (100k objects) | ≤ 30 s |
| Git projection (10k commits) | ≤ 5 s |

Targets are measured on commodity hardware; results are reported in a benchmark suite
added in Phase 1.

### 11.2 Repository amplification metric

Amplification = `canonical_metadata_bytes / source_bytes_changed`, tracked per
repository and reported by `gemel status --verbose` and the benchmark suite. The goal
is metadata growth that is defensible: bounded summaries, structural sharing, and
tiered pruning keep amplification proportional to engineering knowledge, not to
telemetry volume.

---

## 12. Crash-Safety Summary

- Objects: temp + verify + rename + directory fsync; immutable thereafter.
- Refs: journaled transactions; atomic file replacement; recovery replays/rolls back.
- Indexes: disposable; WAL; rebuildable.
- GC: tombstone-before-unlink; journal-aware; audited.
- Workspaces: state.ref updated atomically; dirty records conservative (recomputed on
  `status`).

Interrupted operations at any point leave the repository in a state from which `fsck`
can either repair or precisely diagnose. History loss is impossible by design.
