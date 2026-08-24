# Gemel — Git Interoperability

Status: **Normative.** Version 1.0.0. Implementation phase: Phase 4 (design fixed now).

Git interchange is mandatory initially (brief §25) but must never dictate Gemel's
internal ontology. Export to Git is **lossy by design**; import never fabricates.
Determinism is absolute: identical inputs produce identical Git repositories (export)
and identical Gemel objects (import).

---

## 1. Principles

1. **Deterministic projection.** `gemel export-git` with the same inputs and parameters
   produces byte-identical Git commits, trees, and ancestry. `gemel import-git` of the
   same Git repository produces identical Gemel objects.
2. **Lossy by design, never ambiguously lossy.** Gemel contains information Git cannot
   represent. Export documents exactly what is lost (in `mapping.loss`). Import
   represents unavailable provenance as `UNKNOWN` — never fabricated.
3. **Stable references.** Git commits produced from Gemel carry Gemel identities in
   trailers, so a round trip preserves identity linkage. Import of Gemel-exported Git
   history re-links those identities; import of foreign Git history leaves them
   absent (reported as unknown).
4. **No ontology capture.** Git's tree/commit model is an interchange format; Gemel's
   canonical model (§OBJECT_MODEL 6) is untouched by Git's shape.

---

## 2. Mapping Objects

Every Gemel↔Git correspondence is recorded in a `mapping` object (§OBJECT_MODEL 6.22):

- `kind: git_commit`, `from: <40-hex git commit>`, `to: <change/episode gid>`
- `kind: git_tree`, `from: <40-hex git tree>`, `to: <tree/state gid>`
- `loss` documents: `known_loss` (intentionally dropped fields), `unknowns`
  (provenance absent), `fabricated` (**always empty**).

Mappings are anchored under `refs/mappings/*` and are canonical Tier 0 objects.
Deterministic import/export is verified by re-running and comparing
(`gemel import-git` twice ⇒ identical objects; `gemel export-git` twice ⇒ identical
Git bytes).

---

## 3. Gemel → Git Export

### 3.1 Selection

Export input: a Gemel ref set (e.g., a trajectory's change sequence or a case's
changes) plus parameters (author policy, trailer policy). The projection is the
trajectory's mainline: each change in sequence order becomes one Git commit.

### 3.2 Commit synthesis

For each change C (in sequence order):

- **Tree**: the tree of `C.resulting_state` (canonical tree → Git tree; blobs are
  byte-identical content).
- **Parent**: the commit synthesized from C's primary causal parent's resulting state
  (multi-parent changes export as a merge commit with the recorded parents; parent
  ordering is deterministic: sorted by parent gid).
- **Author/Committer**:
  - `producer.kind == human` with disclosure `FULL`/`DIGEST_ONLY`: author name/email
    from the producer identity; committer = the exporting automation identity.
  - Otherwise (agent, automation, `git_import`, `unknown`, or disclosure
    `REDACTED`/`EPHEMERAL`): author `Gemel Producer <gemel@local>` and a `Gemel-*`
    trailer carries the producer gid. No fabricated human identity is ever created.
- **Committer timestamp**: export time is **not** used; determinism requires fixed or
  carried timestamps. Policy: carry the change's `created_at` metadata when present and
  disclosure allows; otherwise use the epoch-sentinel `1970-01-01T00:00:00Z` and record
  the substitution in `mapping.loss.known_loss`.
- **Message**: the change summary; extended with a stable, ordered footer (trailers):

```
<change summary>

Gemel-Change: change.<64hex>
Gemel-Intent: intent.<64hex>            ; if present
Gemel-Trajectory: trajectory.<64hex>    ; if present
Gemel-Claim: claim.<64hex>              ; repeated, sorted, if disclosure allows
Gemel-Export-Version: 1
```

Trailers are sorted and formatted deterministically. Claims/residuals/evidence gids
are included only when disclosure permits; otherwise a single
`Gemel-Export-Policy: <disclosure>` trailer is emitted and the omission is recorded in
`mapping.loss`.

### 3.3 Loss documentation

Each exported commit's mapping records the loss:

- Known loss: timestamps substituted; agent reasoning, context manifests, evidence
  payloads, residual dispositions, verification scope, reconciliation rationale (all
  unrepresentable in Git) — summarized as counts with gid lists where disclosure allows.
- Unknowns: nothing (export side has full knowledge).

### 3.4 Determinism guarantees

Given the same Gemel repository state and same parameters, `export-git` yields the
same commit hashes (same trees, messages, authors, parents, timestamps). No wall clock
is consulted.

---

## 4. Git → Gemel Import

### 4.1 Object synthesis

For each Git commit (in topological order):

- **Change**: summary = commit subject; `producer` = synthetic `git_import` producer
  (deterministic: name `git-import`, kind `git_import`, disclosure `DIGEST_ONLY`).
- **States**: `input_state` from parent commit's tree; `resulting_state` from this
  commit's tree (both as Gemel states over Git trees).
- **Operations**: derived deterministically from the tree delta:
  - added path ⇒ `create_file` (+ content blob);
  - removed path ⇒ `delete_file`;
  - modified path ⇒ `write_file` (+ new content blob, with the old blob in
    `input_refs`);
  - exact path move with identical content ⇒ `rename_path` (detection is deterministic
    and conservative: only exact content-identity moves are classified as renames;
    everything else is create/delete or write; the detection heuristic is documented
    and pinned by tests — never silently upgraded to "semantic" understanding);
  - mode-only changes ⇒ `write_file` with the mode delta noted in `result.detail`.
- **Provenance**: author/committer name+email become a `human` producer **only when
  the import is of foreign Git**; for re-import of Gemel-exported Git, trailers
  re-link the original Gemel identities. When no trailer and no reliable identity
  exists, producer kind = `unknown` — never invented.
- **Timestamps**: carried as metadata.
- **Trajectory**: commits sharing a first-parent chain are grouped into a synthetic
  trajectory with intent = absent (`UNKNOWN`); `termination_reason` absent.
- **Mappings**: one `mapping` per commit and per tree.

### 4.2 What import never does

- Never fabricates intents, claims, evidence, residuals, or reconciliation decisions.
- Never guesses "why": intent is absent and reported as `unknown`.
- Never invents intermediate states that Git cannot represent.
- Never claims that Gemel-native metadata "was lost" when it never existed — it is
  recorded as `unknowns`, which is correct (brief §26).

### 4.3 Re-import of Gemel-exported Git

Trailers (`Gemel-Change`, `Gemel-Intent`, …) are parsed; when they validate against
the repository (object exists, family matches, hashes agree), the import re-links the
original objects and the mapping records `known_loss: []` for those fields. Trailer
values that do not validate are treated as hostile input (THREAT_MODEL.md §9): they are
ignored with a warning, and the commit is imported as foreign; they are never used to
fabricate links.

---

## 5. Round-Trip Analysis

| Round trip | Provable | Documented loss |
|---|---|---|
| Git → Gemel → Git | Trees byte-identical; commit topology identical; messages identical; author fields identical (subject to §3.2 policy); timestamps identical. | None beyond import-time unknowns (recorded in mappings) |
| Gemel → Git → Gemel | Gemel objects referenced by trailers re-link; content (trees/states) byte-identical; change sequence topology identical. | Agent reasoning, context, evidence payloads, residual dispositions, verification scope, reconciliation rationale — **unless disclosure embeds gid references**, in which case identities are preserved and only payloads are absent |
| Foreign Git only | Trees and ancestry exact; authorship carried as human producers. | All Gemel-native semantics: reported as `unknown`, never fabricated |

The mathematically provable core: **content round-trips exactly** (Gemel trees and Git
trees are isomorphic representations of the same bytes), and **identity linkage
round-trips through trailers**. Intentional loss is documented per mapping; nothing is
silently lost or invented.

---

## 6. clone / push / pull

Semantics outline (implementation Phase 4/6; not Phase 0):

- `gemel clone <url>` initializes a repository and imports the remote's Git history
  via the §4 algorithm (or Gemel-native sync when both sides are Gemel — brief §47:
  Git interchange and Gemel synchronization are separate problems).
- `gemel push` / `gemel pull`:
  - Gemel↔Gemel: native object negotiation (STORAGE.md §10) — exact, deduplicating.
  - Gemel→Git remote: deterministic export projection (§3) of the pushed ref set.
  - Git remote→Gemel: deterministic import projection (§4), never merging Gemel-native
    metadata from the Git side.
- Pushed Git commits are considered derived artifacts: `fsck` and GC treat them as
  disposable (regenerable from canonical objects); mappings are canonical.

---

## 7. Compatibility Limits

- Git cannot represent: intent, claims, evidence, residuals, verification scope,
  reconciliation rationale, context, agent identity (beyond trailers), retention tiers,
  and derived statuses. All are either embedded as trailers (identities) or documented
  as loss.
- Gemel never requires Git; Git support is a projection. A repository with no Git
  remote is fully functional.
- Deterministic export requires deterministic timestamps: policies that use wall-clock
  times are forbidden in export (epoch-sentinel + `known_loss` instead).
