# Gemel — Git-Carried Exchange Rollups

Status: **Normative.** Version 1.0.0. Exchange protocol: `v1`
(schemas `gemel.exchange.pack.v1`, `gemel.exchange.frontier.v1`).

This document specifies how Gemel's evidence-bearing development model travels
through ordinary Git infrastructure. Git transports bytes; Gemel restores meaning.
A repository may keep using Git hosting, clones, pushes, CI, code review, mirrors,
backups, and credentials while carrying enough deterministic Gemel state inside the
Git tree that any Gemel-capable client can reconstruct the engineering context of the
repository with one invocation:

```text
git clone <url>
cd <repo>
gemel status --json
```

A second implementation must be possible from this specification alone.

---

## 1. Architecture

```text
Gemel native store
        │  deterministic export (append-only)
        ▼
Git-carried exchange artifacts  (.gemel/exchange/v1/)
        │  ordinary git push / pull / clone
        ▼
Fresh machine
        │  validate + quarantine-ingest
        ▼
Gemel native store
```

Invariants:

- **Native objects are authoritative.** Exchange artifacts are deterministic
  projections, never the primary mutation surface. No API mutates repository
  knowledge by editing exchange files.
- **Git is transport, not semantics.** Git never interprets Intent/Trajectory/
  Claim/Evidence/Residual. A Git-only consumer may ignore the exchange entirely.
- **Exchange files never alter Gemel source identity.** Gemel source snapshotting
  excludes `.gemel/`; changing only `.gemel/exchange/**` must never change the
  canonical source State (§10).
- **Integrity ≠ authenticity ≠ authority ≠ verification.** BLAKE3 pack ids prove
  byte integrity against the advertised identity, nothing more. Hosting auth tells
  who pushed. FRF evidence tells what verification was performed. Phase 1.5 invents
  no PKI; the separation is documented, not collapsed.

---

## 2. Path Layout

```text
.gemel/
├── .gitignore            local-native-store privacy (see §3)
└── exchange/
    └── v1/
        ├── frontiers/
        │   ├── ab/
        │   │   └── abcd…                (FrontierId hex, .gxf)
        │   └── …
        └── packs/
            ├── 18/
            │   └── 18bd…                (PackId hex, .gxp)
            └── …
```

- Every file under `frontiers/` and `packs/` is **immutable after publication**; its
  filename is derived from its exact content digest (see §6, §9). A normal export
  only adds files; it never rewrites prior artifacts (§25).
- Paths are conservative ASCII; identities are lowercase hexadecimal. Only regular
  files are accepted (no symlinks, devices, or special files; §39).
- The local native store (`objects/`, `refs/`, `index/`, `journal/`, `worktrees/`,
  `quarantine/`) lives under `.gemel/` and must remain invisible to Git.

## 3. Local Store Privacy (.gitignore)

Gemel writes a narrowly scoped `.gemel/.gitignore`:

```text
*
!.gitignore
!exchange/
!exchange/**
```

`git status` therefore exposes only intentionally tracked exchange artifacts. The
exact ignore representation is protocol tooling, not canonical Gemel content; users
must not maintain it by hand.

## 4. Profiles

| Profile | Purpose | Contents |
|---|---|---|
| `frontier` (default) | carry enough durable engineering knowledge that a fresh agent can orient immediately | canonical metadata: Intents, Trajectories, Changes, Cases, Claims, Residuals, verification summaries, compact Evidence, Reconciliations, AgentRun summaries, environment identities, checkpoints, trees, the head state's semantic index (Phase 5); source payloads are carrier-backed (omitted); forensic traces omitted |
| `portable` | substantially self-contained transported context | frontier + canonical source objects (blobs), Tier 1 reproducibility material, compact fixtures, fuller Evidence closure |
| `forensic` | reserved for deeper retained trace material | reserved; activating it is an explicit repository policy decision, never a silent upgrade |

Coverage is first-class: presence of exchange material never implies presence of all
Gemel knowledge (§12).

Phase 5: the frontier seeds `refs/semantic/head`, so the semantic index of the head
state (and its entities, lineage ancestors, and the published indexer producer)
travels with the frontier. On activation, ingestion re-establishes
`refs/semantic/state/<hex>`, `refs/semantic/current`, and `refs/semantic/head` over
the imported objects when the imported material carries exactly one index for the
activated state — divergent derived indexes for one state are never silently
preferred (the client rebuilds deterministically instead). Rehydrated semantic
identities are identical to the exporting repository's (content-addressed).

## 5. Coverage

A Frontier Descriptor declares structured coverage:

```json
"coverage": {
  "canonical_metadata": "complete",
  "source_content": "carrier-backed",
  "evidence_receipts": "complete",
  "evidence_payloads": "partial",
  "conversations": "omitted",
  "forensic_traces": "omitted"
}
```

Queries must propagate these limits. A query for material not carried must answer
`NOT_EXPORTED` (a structured unknown), never "no such thing exists".

## 6. Pack Format — `gemel.exchange.pack.v1`

Binary, uncompressed (Git already compresses tracked blobs), streaming, simple:

```text
MAGIC            "GXPK"                      4 bytes
FORMAT_VERSION   0x01                        u8
OBJECT_COUNT     u64 LE
OBJECT 1 .. N    see below
TRAILER          "GXPK-END"                  8 bytes
```

Each object:

```text
object_id                33 bytes   (family byte + 32-byte BLAKE3 digest)
canonical_length         u64 LE
canonical_envelope_bytes canonical_length bytes (the GCE envelope, verbatim)
```

Rules:

- Objects appear in ascending `object_id` byte order (family byte first, then digest
  bytes), with no duplicates.
- The advertised `object_id` must equal the family byte plus `BLAKE3(canonical
  envelope bytes)` per Gemel object-identity rules. Mismatch ⇒ reject.
- Every envelope must decode with the supported schema versions; unknown mandatory
  schema versions ⇒ reject. Objects exceeding configured limits ⇒ reject.
- `PackId = BLAKE3(exact pack bytes)`, stored at `packs/<2 hex>/<62 hex>.gxp`.

## 7. Deterministic Pack Partitioning

Given the same (object set, profile, protocol version), two machines produce
byte-identical packs:

1. sort the selected objects by object id;
2. encode each exactly;
3. greedily append objects to the current pack until the next object would exceed
   the target pack size;
4. start the next pack.

Target pack size is the protocol constant **262144 bytes** (256 KiB) for v1. Pack
boundaries never derive from filesystem order, scheduling, hash-map iteration,
memory, CPU count, or wall clock.

## 8. Frontier Descriptor — `gemel.exchange.frontier.v1`

A Frontier Descriptor is the exported knowledge frontier corresponding to one
canonical source State. Canonically encoded as JSON with **sorted keys** (UTF-8),
content-addressed: `FrontierId = BLAKE3(exact descriptor bytes)`, stored at
`frontiers/<2 hex>/<62 hex>.gxf`.

```json
{
  "schema": "gemel.exchange.frontier.v1",
  "source_state": "state.…",
  "head_change": "change.…",
  "trajectory": "trajectory.…",
  "intent": "intent.…",
  "parent_frontiers": ["frontier.…"],
  "packs": ["<64 lowercase hex PackId>", "…"],
  "profile": "frontier",
  "coverage": {
    "canonical_metadata": "complete",
    "source_content": "carrier-backed",
    "evidence_receipts": "complete",
    "evidence_payloads": "partial",
    "conversations": "omitted",
    "forensic_traces": "omitted"
  },
  "required_schemas": [1]
}
```

**The descriptor never contains**: export wall-clock time, hostname, temporary
filesystem paths, process ids, random nonces, absolute repository paths, or the
containing Git commit id. The source binding is the Gemel State identity; Git commit
identity is transport metadata, not canonical exchange identity.

`source_state` is the **pure-content State identity**: the would-be state object
`{root_tree}` over the source tree, excluding capture metadata (so identical source
content yields the identical binding on every machine and capture; OBJECT_MODEL.md
§6.3). `packs` list every pack required to reconstruct the frontier's objects.
`parent_frontiers` links to prior frontiers of the same lineage (append-only).

Multiple frontier descriptors may coexist in one tree (independent branches, §7 of
the brief). Their union is the transport surface; source-State identity selects the
active one.

## 9. Export Publication

Per pack: encode to temp → fsync → verify exact `PackId` → atomic rename to the
content-addressed path. The Frontier Descriptor is published **last**, only after
every referenced pack exists and verifies. A descriptor never references
half-written packs. A crash may leave unreferenced temp/abandoned packs; the next
export cleans abandoned temporaries safely and reuses verified packs. Normal export
is **append-only**: it never deletes previously published packs or frontiers
(compaction is a future, separate operation; new pack sets may coexist with old
ones because identities derive from exact content).

## 10. Source-State Non-Interference

Adding, removing, or modifying files under `.gemel/exchange/**` must not change the
canonical Gemel source State. This is a permanent protocol invariant with a
regression court (§13.8).

## 11. Ingestion (Quarantine)

Every exchange artifact is hostile input. Ingestion:

```text
discover frontiers → validate descriptor identity → validate pack identity →
stream-decode → verify canonical object ids → validate schemas → validate
relationships/limits → verify declared coverage → promote immutable objects →
atomically publish imported-frontier state
```

- Verified objects may enter the object store before the whole frontier succeeds
  **only** as unreachable orphans; no active ref is updated until the entire
  relevant frontier has passed validation. Partial semantic activation is never
  observable.
- Ingestion is idempotent: object already present → verify/skip; pack already
  processed → skip; frontier already activated → no-op.
- Conflicts are fatal: the same object id with different bytes (within or across
  packs, or vs a local native object) is a content-addressing violation. Fail
  loudly; never prefer local or remote (§13.9).
- Limits: max descriptor bytes 1 MiB; max pack bytes 64 MiB; max objects per pack
  1 000 000; max packs per frontier 4096; max total automatic ingestion 512 MiB;
  max reference depth 4096; string/path/array limits per THREAT_MODEL.md §5. An
  exchange exceeding automatic limits returns
  `IMPORT_REQUIRES_EXPLICIT_APPROVAL` and leaves the Git repository usable.
- **No execution during ingestion.** Evidence commands, reproduction steps, and
  tool invocations are inert data. `gemel status` is safe against untrusted
  repositories.

## 12. Source-State Binding and Activation

On observing a checkout, Gemel independently computes the current source content
State. For each valid frontier, compare:

- `S == S'` (exactly one) → **activate**: establish imported refs; queries treat
  imported objects as native.
- multiple matching frontiers with non-contradictory identities → ingest the union.
- no match → **import historical context** but claim nothing about the current
  source: readiness and current-context APIs carry the mismatch
  (`SOURCE_CONTEXT_DIVERGED` / `CONTEXT_STALE`).

Selection never uses recency, filename order, directory order, or Git HEAD.

A Git-only source change (no Gemel export) is detected as `CONTEXT_STALE`; old
context remains queryable as history; old Claims are never presented as current
truth; nothing is silently repaired by inventing an unrecorded Change.

## 13. Courts (acceptance tests)

13.1 **Transport**: Repository A (gemel init, change, export) → `git add/commit`
of source + exchange → push to a bare remote → `git clone` in Repository B →
`gemel status --json` reconstructs identical canonical object ids. No native
`.gemel/objects` bytes cross the boundary.

13.2 **Shallow clone**: `git clone --depth=1` recovers the current exported
frontier.

13.3 **Branch merge**: independent branches export distinct frontiers; a Git merge
unions immutable files; both frontiers remain ingestible; neither old frontier is
claimed to describe the merged source; mismatch is explicit; reconciliation +
new frontier are subsequently possible.

13.4 **Git-only mutation**: valid F1 for S1; source modified with Git only; fresh
clone reports F1 valid, imported, source mismatch, context stale; old Claims not
current truth.

13.5 **Corruption**: modified/truncated pack, filename↔hash mismatch, malformed
frontier, missing pack, duplicate object id, id↔body mismatch, unsupported schema,
oversized object, illegal symlink, malformed hex path, partial publication,
malicious fanout — each fails deterministically and structured, with no panic and
no silent partial activation.

13.6 **Idempotence**: repeated `gemel status` → stable object count, stable refs,
no exchange rewrites, no duplicate index rows, unchanged source State, identical
semantic result.

13.7 **Determinism**: two repositories with byte-identical canonical state, at
different paths, export byte-identical frontiers, packs, pack ids, frontier ids.
Path, pid, creation time, thread count, filesystem order, locale, timezone must
not affect canonical exchange bytes.

13.8 **Non-interference**: source State before and after adding exchange files is
identical.

13.9 **Git cleanliness**: after `git clone` + `gemel status`, `git status` shows
only intentionally tracked exchange differences, not thousands of untracked files.

13.10 **Index independence**: clone → auto-ingest → query X → delete SQLite →
rebuild → query X. Exchange ingestion never makes SQLite authoritative.

13.11 **Incremental export**: a new Change reuses all prior packs and adds only
packs required for newly exported objects; an unchanged export is byte-idempotent.

13.12 **Conflicting identity**: the same object id with different bytes (local
corruption + transport) fails loudly as a fatal integrity violation, never
"pick one".

13.13 **Resource bounds**: a hostile descriptor cannot force unbounded ingestion
(`IMPORT_REQUIRES_EXPLICIT_APPROVAL`); parent-chain fanout deeper than the
protocol limit fails with a structured limit error, never a stack overflow.

13.14 **Verify without a native store**: `gemel exchange verify` validates
artifacts and the source binding on a fresh checkout that has never run
`gemel status` (CI path).

13.15 **Activated-then-stale**: a frontier that matched at clone time flips to
`CONTEXT_STALE` the moment the source diverges through Git only.

13.16 **fsck integration**: `gemel fsck` reports native_store and
exchange_transport separately; blobs absent by exchange policy
(`carrier-backed`/`partial` coverage) are `exchange-omitted` warnings, never
native-store corruption.

13.17 **Atomicity/recovery**: abandoned export temporaries are cleaned and the
next export recovers deterministically without touching published files.

## 14. Structured Unknowns and Error Handling

Every exchange-aware query must distinguish *why* data is unavailable. Never
collapse into `null`; absence of a result must never imply absence of knowledge.

The vocabulary (machine-readable, versioned):

| Code | Meaning |
|---|---|
| `KNOWN` | present and verified |
| `KNOWN_ABSENT` | present and verified as absent (e.g. no residuals in the closure) |
| `NOT_EXPORTED` | intentionally omitted by the exchange profile (§5 coverage) |
| `PRUNED` | removed by native retention policy |
| `CORRUPT` | identity/byte verification failed |
| `SCHEMA_UNSUPPORTED` | mandatory schema version this client cannot interpret |
| `SOURCE_STATE_MISMATCH` | context valid historically; does not describe the
  current source (CONTEXT_STALE / SOURCE_CONTEXT_DIVERGED) |
| `NOT_YET_VERIFIED` | verification pending |
| `IMPORT_REQUIRES_EXPLICIT_APPROVAL` | automatic ingestion limits exceeded;
  explicit ingest with a higher budget is required |

Every failure is deterministic, structured, and panic-free; no silent partial
semantic activation is ever observable.

## 15. CLI Surface

```text
gemel exchange status   [--json]   discover/validate/activate report
gemel exchange export   [--profile frontier|portable] [--json]
gemel exchange ingest   [--json]
gemel exchange verify   [--working-tree|--git-index] [--json]
```

`gemel status` auto-discovers exchange material: existing native store → inspect
and auto-ingest new frontiers; no native store but exchange present → safe
bootstrap (create native metadata without clobbering tracked files, install the
`.gitignore`, ingest, build the index, establish imported refs, return status).
Bootstrap and existing-repository sync are different operations: a repository with
local trajectories/pending changes/refs is never silently replaced; the remote
context is exposed as `REMOTE_CONTEXT_AVAILABLE` with an explicit relationship
(same base / diverged / newer / unrelated / requires reconciliation).

## 16. Versioning and Compatibility

Exchange protocol versions coexist under `.gemel/exchange/vN/`. A client never
interprets `v2` as `v1`. Newer clients read supported older versions, preserve
unknown optional metadata where specified, reject unknown mandatory semantics, and
report unsupported versions structurally. The Gemel package version is not the
wire-format version.

## 17. Human Reviewability

Frontier descriptors are canonical JSON and inspectable. Packs are binary transport
artifacts; recommended `.gitattributes`: `packs/** -diff`. None of this is required
for correctness.

## 18. CI

`gemel exchange verify --json` fails on corrupt artifacts, missing packs,
source-state mismatch, or protocol invariant violations, without requiring private
deep evidence.

## 19. Pure-Git Usability

A repository carrying exchange files remains an ordinary Git repository. Gemel
requires no clean/smudge filter, no LFS, and no custom merge driver for basic
operation. Git merge transports competing knowledge as immutable file unions;
Gemel reconciles semantics.
