# Distributed Operation (`gemel` Phase 6)

Native synchronization is **separate from Git interchange** (brief §47): Git remains a
deterministic projection; this document specifies how Gemel repositories exchange the
canonical object graph directly. A second transport implementation must be possible
from this specification alone.

## 1. Principles

- **Content addressing is the negotiation.** An object exists iff its identity exists
  locally. "What do you have / what do you want" are id-set questions, never byte
  comparisons. Re-push transfers nothing; a resumed fetch re-negotiates from the new
  have-set (resumability falls out of negotiation).
- **Refs are the only mutable state exchanged**, and every ref update is validated
  before publication: names must be public refs, and the referenced object closure
  must be fully present (a ref never dangles).
- **Integrity is per-record and end-to-end.** Every envelope is re-verified against its
  advertised identity by the receiver before insertion; the store rejects same-id
  different-bytes as a fatal conflict (THREAT_MODEL.md §11).
- **Transport success is not trust.** A transport may be hostile; integrity
  verification is the receiver's job. Authentication/authorization is the transport's
  concern (§6).
- **Full content travels.** Unlike the Git-carried exchange profile (carrier-backed),
  native sync carries source blobs: the destination reconstructs the complete graph.
- **Pull never overwrites divergence.** `pull` is fetch + fast-forward; a diverged
  local head is preserved and reconciliation is the explicit next step.

## 2. Refs policy

Refs are partitioned into **public** (sync) and **local-only** (never sync):

| Public (travels) | Local-only (stays home) |
|---|---|
| `refs/head` | `refs/mappings/*` (interchange bookkeeping) |
| `refs/state/head` | `refs/exchange/*` (imported-frontier markers) |
| `refs/config` | `refs/remotes/*` (tracking, local) |
| `refs/names/*` | workspace state files (not refs) |
| `refs/trajectories/*` (incl. `current`) | |
| `refs/cases/*`, `refs/releases/*` | |
| `refs/checkpoints/*`, `refs/reconciliations/*` | |
| `refs/semantic/*` (per-state, current, head) | |

The remote advertises its public refs. `fetch` records them under
`refs/remotes/<name>/<flat-name>` (e.g. `refs/names/C1` → `refs/remotes/origin/names/C1`)
without touching local refs. `push` updates the remote's public refs. `pull` fast-forwards
local public refs to the remote values only when the remote head descends from (or
equals) the local head.

## 3. Transfer pack format — `gemlpack` (GMLP v1)

Distinct from the Git-carried `GXPK` exchange pack: GMLP carries full canonical
envelopes (including blobs) and is the native wire format.

```
MAGIC            "GMLP"                      4 bytes
FORMAT_VERSION   0x01                        u8
OBJECT_COUNT                                u64 (LE)
TOTAL_BYTES                                 u64 (LE)  (envelope bytes only)
RECORD 1..N
    object_id                               33 bytes (family code + BLAKE3 digest)
    envelope_length                         u64 (LE)
    canonical_envelope_bytes                [envelope_length]
```

Invariants:

- Records appear in ascending canonical id byte order.
- The advertised id must equal `BLAKE3(envelope)`; the decoded family must equal the
  id's family.
- Duplicate ids are rejected.
- Limits (defaults): 4 GiB per pack, 10,000,000 objects per pack, 1 GiB per object —
  enforced during decode before allocation.
- A single failing record invalidates the whole pack: refs are never published over a
  partially verified transfer.

The pack bytes themselves receive `PackId = BLAKE3(exact_pack_bytes)` for caching and
resume bookkeeping; the pack id is transport metadata, not a Gemel object identity.

## 4. Protocol messages

The transport trait (Rust `sync::Transport`) defines six operations. Message
parameters are id sets and refs; there are no executable payloads (SEC; THREAT_MODEL
§10).

| Operation | Direction | Semantics |
|---|---|---|
| `list_refs` | remote → client | the remote's public refs |
| `reachable_ids(seeds)` | remote → client | the remote's reachable closure of the seeds (ids only) |
| `missing_ids(ids)` | remote → client | which of the ids the remote lacks |
| `fetch_objects(ids)` | remote → client | verified envelopes for the ids |
| `push_objects(records)` | client → remote | envelopes; the remote verifies each identity and rejects conflicts |
| `update_refs(refs)` | client → remote | atomically publish refs (names validated, closures present) |

### 4.1 Fetch

1. `list_refs` → remote public refs.
2. `reachable_ids(ref values)` → remote closure.
3. `want = closure − local present` (by identity).
4. `fetch_objects(want)` → envelopes; verify `BLAKE3(envelope) == id` and insert
   (dedup/conflict-safe).
5. Publish `refs/remotes/<name>/<flat>` for every remote ref — only after the whole
   transfer verified.

### 4.2 Push

1. Local public refs.
2. `reachable_ids(local ref values)` → local closure (computed locally).
3. `missing_ids(closure)` → what the remote lacks.
4. `push_objects(missing)` → remote verifies and stores.
5. `update_refs(local public refs)` → remote validates names + presence, applies
   journaled-atomically.

### 4.3 Pull

`pull = fetch + fast-forward`: after fetch, the remote head must equal or descend from
the local head (causal-parent walk, bounded). On divergence the pull refuses and
preserves local refs; the fetched tracking refs remain available for `gemel
reconcile`.

## 5. Concurrency and locking

- Object insertion is lock-free and content-addressed: concurrent writers cannot
  conflict on objects (identical ids deduplicate; different ids coexist).
- Refs updates use the journaled ref transaction under the repository's write lock on
  each side (STORAGE.md §4). The remote's own lock protects it from concurrent local
  writers.

## 6. Transports and security

- `FileTransport` addresses a local filesystem path to an initialized Gemel
  repository; `gemel remote add --init` creates one. Authentication is the
  filesystem's; the protocol still verifies every byte.
- **Network transports (Phase 8; HOSTED.md):** `SshTransport` runs
  `ssh [-p port] [user@]host gemel serve <path>` — SSH supplies mutual auth and
  encryption; `HttpTransport` POSTs the same session operations to
  `gemel serve --http` with bearer-token capability grants (`read`/`write`) and
  `--read-only` servers. All three implement the same six-operation trait with
  `&mut self` methods (sessions own a read position). Remote arguments accept
  `ssh://[user@]host[:port]/path`, `http://[token@]host:port/path`, or a local
  path; `https://` is rejected fail-closed (TLS is a proxy concern).
  `--root` hosting serves many repositories by single-segment URL path with
  traversal rejection; non-loopback binds without tokens refuse to start.
- Integrity ≠ authenticity: a verified object is byte-exact, but the producer's
  identity is carried by the object's own producer field, not by the transport.
- The sync session is bounded (64 KiB lines, 4 GiB packs, 10M ids); push packs
  are schema- and identity-verified before insertion; nothing is executed during
  sync (the FRF court is the only execution path and is policy-gated; HOSTED.md §7).
- Git-only remotes: `gemel push`/`pull` against a path that is a Git repository (not a
  Gemel one) use the deterministic export/import projections (GIT_INTEROP.md §3–§4);
  the loss documented there applies.

## 7. CLI surface

```
gemel remote                 list configured remotes
gemel remote add <name> <path-or-url> [--init]
gemel remote remove <name>
gemel fetch <remote>         transfer objects + write refs/remotes/<name>/*
gemel push <remote>          update the remote's public refs
gemel pull <remote>          fetch + fast-forward (never overwrites divergence)
gemel serve [path] [--http addr] [--root dir] [--read-only] [--token …]
gemel court <evidence-id> [--allow] [--timeout secs]
```

All commands support `--json` with the `gemel.query.v1` envelope. A remote argument
may be a configured name, an `ssh://`/`http://` URL, or a direct path.
