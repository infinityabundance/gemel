# Gemel — Agent Protocol

Status: **Normative.** Version 1.0.0. Query surface schema: `gemel.query.v1`.

This document defines the machine-readable query surface: response envelope, JSON
object mapping, pagination, token budgets, every query endpoint with its schema,
progressive disclosure levels, the evidence-ingestion protocol, context manifest
construction, handoff/checkpoint generation, readiness computation, and the FRF
boundary.

Every important Gemel query exists in four forms: human CLI output, machine JSON
(`gemel … --json`), library API, and (Phase 6+) a lightweight agent protocol. Agents
never scrape decorative terminal prose.

---

## 1. Principles

1. **Deterministic.** Identical (repository, query, parameters) ⇒ identical response
   bytes. No wall-clock timestamps in responses unless explicitly requested; no
   random ordering; defined tie-breaks.
2. **Versioned.** Every response declares `"schema": "gemel.query.v1"`.
3. **Bounded.** Every response is size-limited; expansions are explicit.
4. **Paginated.** Stable opaque cursors.
5. **Explicit omission.** Anything not returned that could be relevant is listed in
   `omitted[]` with a reason.
6. **Explicit uncertainty.** Uncertainty is reported, never silently resolved.
7. **ID-first.** Responses return canonical identities; payloads are expandable on
   demand (progressive disclosure, §6).
8. **Budget-aware.** Context endpoints honor token budgets.

---

## 2. Response Envelope

```json
{
  "schema": "gemel.query.v1",
  "request": {"command": "status", "params": {...}},
  "pagination": {"has_more": false, "next_cursor": null, "count": 12},
  "result": {...},
  "omitted": [{"class": "evidence", "count": 812, "reason": "budget"}],
  "uncertainty": [{"class": "evidence_freshness", "ids": ["evidence.ab12…"], "reason": "no evaluated-state anchor"}]
}
```

- `omitted` and `uncertainty` are mandatory arrays (possibly empty).
- Ordering convention: any array of objects is sorted by canonical identity (hex
  string), unless the endpoint documents another deterministic order. Strings sort by
  byte value. `null` is never emitted for absent optional fields (§3.1).

---

## 3. JSON Object Mapping

### 3.1 Canonical form

```json
{
  "family": "change",
  "schemever": 1,
  "body": [
    {"tag": 1, "name": "summary", "value": "Fix parser compatibility problem"},
    {"tag": 6, "name": "producer", "value": "producer.9f3a…"}
  ]
}
```

- `name` is an annotation; `tag` is authoritative.
- Value encodings: UINT/SINT → decimal strings (exact, no precision loss); BOOL →
  JSON boolean; BYTES → lowercase hex; STRING → JSON string; GID → textual identity;
  RECORD → array of field objects (as above); ARRAY → JSON array.
- Optional absent fields are omitted; `null` is never emitted.

### 3.2 Compact form

Endpoints may return a compact form (`"form": "compact"`) with named fields for the
most useful subsets (e.g., `status` summaries). Compact forms are documented per
endpoint; the canonical form is always available via `gemel show <id> --json`.

### 3.3 Identity and reference conventions

- All identities are textual (§OBJECT_MODEL 3.1).
- A reference object: `{"id": "change.ab12…", "family": "change", "summary": "…"}`
  (summary included when cheap; otherwise omitted).

---

## 4. Pagination and Budgets

### 4.1 Pagination

- Request: `{"limit": 100, "cursor": "<opaque>"}`. Default limit 100, max 1000.
- Cursor: base64url of `{"anchor": "<last id>", "offset": <n>, "order": "<asc|desc>"}`.
  Cursors are stable while the repository is unchanged; they are invalidated (never
  misdirected) by GC of referenced objects — the response then restarts with an
  explicit `notice`.

### 4.2 Token budgets

Context endpoints accept `budget` (tokens) or `budget_bytes`. Semantics:

1. Results are computed as ID-level references first (level L0, §6).
2. Expansion proceeds in deterministic order (§6.2) until the budget is exhausted.
3. The response reports what was expanded and what was omitted with reason `budget`.
4. Budgets are enforced server-side; clients cannot request more than the policy
   maximum (`config.limits`; THREAT_MODEL.md §5).

---

## 5. Query Endpoints

All endpoints: `gemel <command> [args] --json [--limit N] [--cursor C]`.

### 5.1 `status`

Human default: §50 of the brief (one screen). JSON:

```json
{
  "schema": "gemel.query.v1",
  "request": {"command": "status", "params": {}},
  "result": {
    "trajectory": {"id": "trajectory.…", "summary": "Implement DNS compression"},
    "intent": {"id": "intent.…", "summary": "…"},
    "state": {"id": "state.…"},
    "changed": {"files": 4, "semantic_entities": 12},
    "claims": {"proposed": 8, "supported": 6, "partially_supported": 0,
               "contradicted": 1, "unverified": 1, "stale": 0, "superseded": 0},
    "evidence": [
      {"platform": "linux", "arch": "x86_64", "result": "pass"},
      {"platform": "linux", "arch": "aarch64", "result": "pass"},
      {"platform": "freebsd", "arch": "x86_64", "result": "not_run"},
      {"oracle": "bind", "match_rate": 0.9998}
    ],
    "residuals": [{"id": "residual.…", "summary": "pointer-loop discrepancy",
                   "severity": "high", "disposition": "open"}],
    "previous_attempts": 2,
    "readiness": {"verdict": "NOT_READY", "reasons": [
      "open residual residual.… (blocking)",
      "contradicted claim claim.…",
      "missing verification: freebsd/x86_64" ]}
  },
  "omitted": [],
  "uncertainty": []
}
```

`--verbose` adds per-claim status detail, evidence scopes, and the readiness algorithm
inputs (§9.3).

### 5.2 `why <subject>`

Subject = canonical path, identity, or entity name. Traverses (brief §14):

```text
current code → Change → Intent → Claim → Evidence → Residual → Decision
```

```json
{
  "schema": "gemel.query.v1",
  "request": {"command": "why", "params": {"subject": "src/name.rs:417"}},
  "result": {
    "subject": "src/name.rs:417",
    "current_identity": {"path": "src/dns/name/parser.rs:82",
                         "lineage": "explicit|similarity|unknown", "confidence": "high"},
    "introduced_by": {
      "change": {"id": "change.…", "summary": "Match upstream pointer-loop behavior"},
      "intent": {"id": "intent.…", "summary": "Implement pointer-loop detection matching upstream behavior"},
      "claim": {"id": "claim.…", "predicate": "BIND 9.20 compatibility",
                "status": "SUPPORTED"},
      "evidence": [{"id": "evidence.…", "kind": "oracle_comparison", "outcome": "match"},
                   {"id": "evidence.…", "kind": "court_receipt", "outcome": "pass"}],
      "residuals": [{"id": "residual.…", "summary": "BIND 9.11 differs",
                     "disposition": "acknowledged"}],
      "decision": {"id": "reconciliation.…", "summary": "Target BIND 9.20 behavior"}
    },
    "previous_approaches": [
      {"trajectory": "trajectory.…", "outcome": "rejected",
       "reason": "strict RFC behavior diverged in 17 oracle cases",
       "evidence": ["evidence.…"]}
    ]
  },
  "omitted": [],
  "uncertainty": []
}
```

The traversal is a bounded typed query (indexes: change-by-subject, claim-by-subject,
claim→evidence edges), never a prose search.

### 5.3 `claims [--subject X] [--status S]`

```json
{"result": {"claims": [
  {"id": "claim.…", "predicate": "parser accepts all valid RFC inputs",
   "predicate_kind": "correctness", "status": "SUPPORTED",
   "supporting": ["evidence.…"], "contradicting": [], "stale": [],
   "scope": "parser::decode_name",
   "change": "change.…", "trajectory": "trajectory.…"}
]}}
```

Status derivation per §OBJECT_MODEL 8.1, always with `basis`.

### 5.4 `evidence <id>`

Canonical object (form `canonical`) or compact with derived freshness:

```json
{"result": {"id": "evidence.…", "kind": "oracle_comparison",
  "subject": "decode_name", "outcome": "mismatch",
  "evaluated_state": "state.…",
  "freshness": {"status": "MAY_REQUIRE_REFRESH",
                "caused_by": ["change.…"]},
  "reproduction": {"replayable": true, "inputs_present": true, "policy_required": false}}}
```

### 5.5 `residuals [--subject X] [--disposition open]`

```json
{"result": {"residuals": [
  {"id": "residual.…", "summary": "semantic divergence", "classification":
   "semantic_divergence", "severity": "high", "disposition": "open",
   "persistence": {"descendant_changes": 7},
   "scope": {"intent": "intent.…", "paths": ["parser::decode_name"]},
   "origin_evidence": "evidence.…",
   "affected_claims": ["claim.…"], "affected_changes": ["change.…"]}
]}}
```

"Where does certainty end?" = `residuals --disposition open --order severity`.

### 5.6 `attempts <subject>`

Trajectories whose changes touch the subject, plus intent-sharing trajectories:

```json
{"result": {"attempts": [
  {"trajectory": "trajectory.…", "intent": "intent.…", "outcome": "rejected",
   "termination_reason": "FreeBSD race reproduced",
   "evidence": ["evidence.…"], "residuals": ["residual.…"]},
  {"trajectory": "trajectory.…", "outcome": "incomplete", "handoff": "…"}
]}}
```

### 5.7 `trajectory <id>`

```json
{"result": {"id": "trajectory.…", "intent": "intent.…", "base_state": "state.…",
  "outcome": "incomplete",
  "sequence": [{"change": "change.…", "summary": "…", "state": "state.…"}, …],
  "evidence": ["evidence.…"], "residuals": ["residual.…"],
  "handoff": {"summary": "…", "remaining": ["FreeBSD verification"],
              "open_residuals": ["residual.…"],
              "recommended_objects": ["change.…", "trajectory.…"]}}}
```

The sequence is the concatenation of `added_changes` along the chain (§OBJECT_MODEL
7.3), materialized by the query layer.

### 5.8 `checkpoint` (create/read)

`gemel checkpoint --json` generates from structured state (§9.2) and returns the
checkpoint object. Read returns the checkpoint with its referenced context pre-resolved
at level L1.

### 5.9 `context <subject> --for-intent I91 --budget 32000 --include residuals,claims,attempts`

See §6 for the full algorithm. Response is a bundle (§6.4):

```json
{"result": {"subject": "parser::decode_name", "intent": "intent.…",
  "budget": {"tokens": 32000, "consumed": 28144, "remaining": 3856},
  "bundle": {
    "objects": [{"id": "…", "family": "…", "level": 1, "summary": "…"}, …],
    "deduplicated": 41,
    "expanded": {"claims": 3, "residuals": 2, "attempts": 2, "evidence": 5}
  },
  "next": {"expand": ["evidence.ab12…", "trajectory.ef56…"], "why": "budget"}}}
```

### 5.10 `reconcile --plan --json`

Dry-run plan for reconciling trajectories (no state mutation):

```json
{"result": {"plan": {
  "inputs": ["trajectory.…", "trajectory.…"],
  "textual_conflicts": [{"path": "src/name.rs", "changes": ["change.…", "change.…"]}],
  "semantic_interactions": [
    {"kind": "dependency", "certainty": "possible",
     "detail": "serialize depends on invariant changed by normalize",
     "subjects": ["change.…", "change.…"]}],
  "claims": {"retained": ["claim.…"], "invalidated": ["claim.…"],
             "verification_required": ["claim.…"]},
  "resulting_state": "state.…",
  "unresolved_residuals": ["residual.…"],
  "rationale": "…"
}}}
```

### 5.11 `diff <A> <B> --semantic --claims --behavior --evidence --residuals --dependencies`

Multidimensional delta (brief §23):

```json
{"result": {
  "textual": {"files": {"changed": ["src/name.rs"], "added": [], "deleted": []}},
  "behavioral": [{"entity": "decode_name", "delta": "rejects pointer cycles > 16"}],
  "claims": {"added": ["claim.…"], "invalidated": ["claim.…"]},
  "evidence": {"gained": [{"id": "evidence.…", "count": 1922, "kind": "oracle_comparison"}],
               "invalidated": ["evidence.…"]},
  "residuals": {"introduced": ["residual.…"], "resolved": ["residual.…", "residual.…"]},
  "semantic_interactions": [{"subject": "serializer::emit", "certainty": "possible"}]
}}
```

### 5.12 `impact <change-id>`

Conservative impact (brief §33): objects potentially affected.

```json
{"result": {
  "change": "change.…",
  "touches": {"paths": ["src/name.rs"], "entities": ["normalize"]},
  "may_require_refresh": [
    {"evidence": "evidence.…", "supports": "claim.…", "rule": "path overlap"}],
  "affected_claims": ["claim.…"],
  "affected_residuals": ["residual.…"],
  "verification_required": ["freebsd/x86_64"]
}}
```

### 5.13 `context-diff <run-a> <run-b>`

Compares two agent runs via their context manifests (brief §19):

```json
{"result": {
  "seen_by_a_only": [{"class": "residual", "id": "residual.…"}],
  "seen_by_b_only": [{"class": "external_artifact", "id": "blob.…"}],
  "upstream_revision": {"a": "U18", "b": "U19"},
  "instruction_digests_equal": false
}}
```

### 5.14 `log`, `show`

`log` lists changes/episodes/operations at the requested resolution (brief §3);
`show <id> --json` returns the canonical form (§3.1).

---

## 6. Progressive Disclosure and Context Bundles

### 6.1 Levels

| Level | Contents |
|---|---|
| L0 | identities only (ids, families, summaries absent) |
| L1 | + summaries (default for list endpoints) |
| L2 | + structured fields (predicates, outcomes, dispositions, scopes) |
| L3 | + full canonical objects and evidence payloads (blobs/logs) |

`context` requests default to L1 for referenced objects, L2 for directly relevant
claims/residuals, and never L3 without explicit `--include-l3`.

### 6.2 Deterministic expansion order

Within a budget: phase 1 = objects directly referencing the subject (changes,
claims); phase 2 = residuals affecting the subject (open first); phase 3 = previous
attempts (trajectories sharing intent or touching subject); phase 4 = evidence for the
included claims; phase 5 = context manifests of the relevant agent runs. Within a
phase: newest first (by `created_at` metadata when present, else 0), then ascending
gid. All ties break on gid ascending. Expansion stops at the budget; the remainder is
reported in `omitted`.

### 6.3 Deduplication

Objects already included by identity are never repeated (bundle reports
`deduplicated`). Identical evidence payloads referenced by multiple claims appear once.

### 6.4 Bundles

A context bundle is the concrete output of `context`: a deduplicated, budget-bounded
set of object references with levels, plus `next` expansion pointers. Bundles are
deterministic for identical inputs and may be cached by content hash.

---

## 7. Evidence Ingestion Protocol

`gemel evidence ingest` accepts a stable JSON document (schema `gemel.evidence.v1`):

```json
{
  "schema": "gemel.evidence.v1",
  "producer": "producer.…",
  "kind": "court_receipt",
  "subject": "parser::decode_name",
  "command": "frf court run parser-decode-name --corpus dns-2024",
  "inputs": [{"id": "state.…", "role": "evaluated_state"}],
  "environment": "environment.…",
  "tools": [{"name": "frf-court", "version": "1.2.0"}],
  "result": {"outcome": "pass", "counts": {"passed": 99421, "failed": 0,
             "skipped": 0, "total": 99421}},
  "reproduction": {"replayable": true, "inputs_present": true, "policy_required": false},
  "outputs": [{"id": "blob.…", "role": "court_log"}]
}
```

Three concepts are kept distinct (brief §38):

1. **tool ran** — the evidence object exists;
2. **result produced** — `result.outcome` and payloads;
3. **interpreted as evidence for Claim C** — the explicit `claim → evidence` edge
   created by `gemel claim link C --evidence E`, a deliberate act by a producer.

Ingestion never auto-links claims; it validates references, computes the identity, and
publishes. Execution of reproduction instructions requires explicit policy
(§10; default `never_auto_execute`).

---

## 8. Context Manifest Construction

`context-manifest` objects are built from: source/documentation objects supplied,
claims/residuals included, previous trajectories considered, external artifacts, tool
outputs, policies in force, instruction + digest. Construction is deterministic from
the input list (order normalized by id). Manifests enable `context-diff` (§5.13) and
make `AgentRun.context_manifest` a content-addressed record of what an agent saw.

---

## 9. Handoff, Checkpoint, and Readiness Generation

### 9.1 Handoff

`trajectory.handoff` is machine-generated from repository state: `remaining` from the
trajectory's open residuals + incomplete verification scope; `open_residuals` from the
current residual set; `important_evidence` from evidence supporting the trajectory's
claims; `recommended_objects` from the impact frontier (objects whose evidence is
`MAY_REQUIRE_REFRESH`). Human text is allowed but the structured fields are the
contract.

### 9.2 Checkpoint

`gemel checkpoint` assembles: current intent, trajectory, state; open claims and
residuals; important evidence; recent decisions (head changes); relevant attempts;
continuation scope (impact frontier). All fields are references resolved to L1 in the
output.

### 9.3 Readiness

Deterministic function of:

1. open residuals with severity `blocking` ⇒ `NOT_READY`;
2. contradicted claims within the queried scope ⇒ `NOT_READY`;
3. required verification matrix (config) entries unsatisfied ⇒ `NOT_READY`
   (each with the missing platform/config);
4. open residuals with severity ≤ `high` but acknowledged ⇒
   `READY_WITH_RESIDUALS` (each listed);
5. unverified claims in scope ⇒ `READY_WITH_RESIDUALS` (listed);
6. otherwise `READY`.

Every verdict includes `reasons[]`; no verdict is produced without them.

---

## 10. FRF Integration Boundary

- Gemel references FRF courts, fixtures, comparators, normalizers, and receipts by
  immutable identity (evidence kind `court_receipt`, tool records, fixture refs).
- Gemel never executes FRF or reproduction commands itself; execution is policy-gated
  (`config.execution_policy`; default `never_auto_execute`).
- FRF logic (courts, comparators, reproducibility semantics, residual analysis) is not
  duplicated in Gemel; Gemel stores FRF artifacts and derives statuses from them.
- The `reconcile --plan` endpoint may flag `verification_required` scopes; actual runs
  happen under policy.

---

## 11. Error Semantics

```json
{
  "schema": "gemel.query.v1",
  "error": {
    "code": "NOT_FOUND",
    "message": "object not present: change.ab12… (pruned; tombstone: …)",
    "ids": ["change.ab12…"]
  }
}
```

Codes: `NOT_FOUND`, `INVALID_ARGUMENT`, `BUDGET_EXCEEDED`, `PAGINATION_INVALID`,
`REPOSITORY_CORRUPT` (with fsck guidance), `POLICY_DENIED`, `LIMIT_EXCEEDED`,
`INTERNAL`. Errors are deterministic; `message` is for humans, `code` is the contract.

---

## 12. Determinism and Stability

- Response bytes are a pure function of (repository content, query, parameters).
- Schema evolution of the query surface follows additive rules: new endpoints and new
  fields are additive within `gemel.query.v1`; breaking changes bump the schema string.
- Clients must tolerate unknown fields (forward compatibility) and must never depend on
  `message` text.
