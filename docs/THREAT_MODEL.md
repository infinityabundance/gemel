# Gemel — Threat Model

Status: **Normative.** Version 1.0.0.

This document defines the security model: assets, trust boundaries, threat actors, the
threat catalog, the fail-closed catalog, resource limits, the confidentiality/
disclosure model, integrity and forensic properties, workspace threats, retention
threats, remote-protocol threats (Phase 6), the hash-collision policy, and residual
risk.

Governing principle (brief §48): **treat repository inputs as hostile.** Parse
everything with limits. Never execute anything from metadata without explicit policy.
Fail closed.

---

## 1. Assets and Trust Boundaries

### 1.1 Assets

| Asset | Description | Confidentiality | Integrity |
|---|---|---|---|
| Canonical objects | the engineering memory substrate | disclosure policy governs | critical (immutability + hashes) |
| Refs | mutable names | public within repo | critical (atomicity) |
| Derived indexes | acceleration only | — | rebuildable |
| Workspaces | working trees, dirty state | user-controlled | important |
| Journal | ref transaction audit | — | important |
| Remote data (P6) | fetched objects | policy | critical (verified by hash) |
| Keys/credentials | remote auth (P6) | critical | critical |

### 1.2 Trust boundaries

```
        ┌─────────────── untrusted input ───────────────┐
        │  object bytes   git repos   remotes   env     │
        │  manifests      ref names   workspaces        │
        ▼                                               ▼
   ┌─────────┐   ┌─────────┐   ┌─────────┐   ┌─────────┐
   │ decoder │   │ store   │   │ query   │   │ policy  │
   └─────────┘   └─────────┘   └─────────┘   └─────────┘
        │               │            │            │
        └─────── canonical layer (trusted) ───────┘
```

- The **canonical layer** (decoder validation, object store, ref layer) is the security
  boundary. Everything outside it is hostile.
- **No ambient authority**: objects carry no executable authority. Evidence
  reproduction fields are inert data; only the policy layer can authorize execution.

---

## 2. Threat Actors

| Actor | Capabilities | Motivation |
|---|---|---|
| Malicious object author | crafts object bytes/refs | DoS, confusion, phishing agents, corrupt reasoning |
| Compromised repository / supply chain | ships crafted objects or indexes | injection into agent-visible context |
| Malicious remote (P6) | serves crafted objects | hash-confusion, data poisoning |
| Malicious environment manifest author | crafts environment/evidence metadata | command injection via reproduction |
| Local attacker with repo write | writes refs/objects | corruption, confusion |
| Insider (disclosure) | reads repo | extracting private reasoning/prompts |

---

## 3. Threat Catalog

| ID | Threat | Vector | Impact | Mitigation | Status |
|---|---|---|---|---|---|
| T-01 | Malformed object (bad magic/version/flags) | crafted bytes | parse confusion | Fail-closed envelope validation (ENC-01…06) | mitigated (Phase 0) |
| T-02 | Non-canonical encoding (redundant varint, dup/unsorted fields, invalid UTF-8) | crafted bytes | smuggling past hash-identical duplicates, parser divergence | Canonical-form rejection (ENC-07…13, ENC-17, ENC-23) | mitigated (Phase 0) |
| T-03 | Unknown mandatory field | newer-schema bytes | semantic misread | Reject `UnknownMandatoryField` (ENC-14) | mitigated (Phase 0) |
| T-04 | Decompression bomb | compressed payload | memory exhaustion | **No compression in canonical objects** (OBJECT_MODEL §1.1); size limits (§5) | mitigated by design |
| T-05 | Path traversal via tree names / operation paths | crafted names (`..`, absolute, `\`) | escape workspace, overwrite files | Path validation (ENC-23, FAM-04); workspace materialization bounds (SEC-04) | mitigated (Phase 0/1) |
| T-06 | Path traversal via ref names | crafted refs (`../`, NUL) | write outside ref namespace | Ref name syntax + rejection (SEC-05) | mitigated (Phase 1) |
| T-07 | Reference abuse (gid to wrong family) | crafted gid bytes | type confusion in queries | Gid family prefix in text; expected-family validation in schema; family-in-envelope (FAM-03, ENC-11) | mitigated (Phase 0) |
| T-08 | Hash collision / substitution | crafted second preimage | object substitution | BLAKE3-256; insert-refuses-different-bytes (STO-03); collision policy §11 | mitigated (residual risk negligible) |
| T-09 | Graph bombs (deep records, wide arrays, long chains) | crafted nesting/fanout | stack/memory exhaustion | Depth/array/object limits (ENC-19, ENC-20); chain-length limits in query (SEC-06) | mitigated (Phase 0) |
| T-10 | Query expansion bombs | many objects matching subject | context flooding, token cost | Budgets, pagination, server-side limits (QRY-06, SEC-06) | mitigated (Phase 2) |
| T-11 | Corrupted remote data (P6) | network fault / malicious server | corrupt objects | Per-record hash verification before use (SEC-07); abort on mismatch | mitigated (Phase 6) |
| T-12 | Malicious environment manifest | crafted env metadata | confusion; if executed, RCE | Manifests inert; execution requires policy (`never_auto_execute` default) (SEC-03, FAM-20) | mitigated by design |
| T-13 | Command injection via reproduction metadata | crafted `command`/`argv` fields | RCE if executed | Execution only under policy; command strings are inert data (SEC-03) | mitigated by design |
| T-14 | Index tampering | modified `index/gemel.db` | wrong query results | Index is derived; fsck detects drift; rebuild restores (STO-05, STO-06) | mitigated (Phase 1) |
| T-15 | Ref tampering | modified ref files | retargeted names | Ref files are validated; journal audit; fsck detects (REF-05, REF-03) | mitigated (Phase 1) |
| T-16 | Symbolic-link attack in workspace | crafted tree with symlink to sensitive path | write outside workspace via materialized link | Materialization bounds; symlink targets are blob content; checkout never follows links outside root (SEC-04) | mitigated (Phase 1) |
| T-17 | Tombstone abuse (fake tombstone for existing object) | crafted tombstone | hide corruption | Tombstones are signed metadata? (see §9); fsck validates tombstones against policy | partially mitigated; residual risk noted §12 |
| T-18 | Confidentiality breach (prompt/reasoning leakage) | query output, git export | private reasoning exposed | Disclosure policies (FULL/DIGEST_ONLY/REDACTED/EXTERNAL_ATTESTATION/EPHEMERAL) enforced at query/export (SEC-08, FAM-10); instruction digest replaces text by default | mitigated (Phase 2/4) |
| T-19 | Trailer spoofing on Git import | crafted `Gemel-*` trailers | fabricated identity links | Trailers validated against existing objects; invalid ⇒ ignored + warning (GIT_INTEROP §4.3) | mitigated (Phase 4) |
| T-20 | Malicious external attestation | crafted attestation evidence | false provenance | Attestation is evidence (kind `external_attestation`); trusted only per policy; never auto-trusted | mitigated by policy |
| T-21 | DoS via huge objects (blob payload) | giant blob | disk/memory exhaustion | Object size limits before allocation (§5); streaming where applicable | mitigated (Phase 0) |
| T-22 | Local race / TOCTOU on refs | concurrent writers | torn refs | Atomic rename; exclusive writer lock; journal replay (STORAGE §4) | mitigated (Phase 1) |
| T-23 | Log/journal injection (nonce/timestamps) | crafted ref names in journal | log confusion | Journal entries escaped/validated; names validated (SEC-05) | mitigated (Phase 1) |
| T-24 | Remote auth/authorization failures (P6) | credential theft, over-privileged fetch | unauthorized read/write | Phase 6 design: authentication, authorization, repository policy, signed fetches | designed (Phase 6) |

---

## 4. Fail-Closed Catalog

The decoder rejects (each is a distinct error code; see also INVARIANTS ENC-*):

No decoder path: guesses, repairs, skips mandatory structure, executes, or allocates
before bounds checks.

The complete error vocabulary of the Phase 0 reference implementation
(`src/error.rs`) is:

`BadMagic`, `UnknownEncodingVersion`, `UnknownFamily`, `UnknownSchemaVersion`,
`ReservedFlags`, `LengthMismatch`, `TrailingBytes`, `NonCanonicalInteger`,
`IntegerOverflow`, `InvalidBoolean`, `InvalidUtf8`, `InvalidGid`, `UnsortedFields`,
`DuplicateField`, `ReservedTag`, `UnknownMandatoryField`, `ExtensionNotPermitted`,
`ValueLengthMismatch`, `MissingRequiredField`, `LimitExceeded`, `InvalidPath`,
`InvalidEnumValue`, `FamilyMismatch`, `TypeMismatch`, `BodyKindMismatch`,
`UnexpectedRawValue`, `ExtensionMustBeRaw`, `InvalidTreeMode`, `InvalidTreeTargetFamily`,
`InvalidTreeOrder`, `InvalidTreeName`, `UndeclaredOperationTag`, `EmptyPredicate`,
`MappingFabricatedNonEmpty`, `RefCountExceeded`, `UnknownFieldName`, and the
primitive wrappers `Varint`, `Hex`, `GidParse`, `DigestLength`.

Depth-limit and count-limit violations surface as `LimitExceeded` with a `kind`
(`record depth`, `array size`, `string size`, `bytes size`, `object size`, `field size`,
`record size`, `gid size`, `refs`).

---

## 5. Limits and Resource Controls

Defaults (overridable per repository in `config.limits`; STORAGE/OBJECT_MODEL §5):

| Resource | Default | Enforced |
|---|---|---|
| object size | 1 GiB | decode, before allocation |
| record depth | 64 | decode |
| array elements | 1,000,000 | decode |
| string bytes | 16 MiB | decode |
| refs per object | 100,000 | validation |
| query limit | 1,000 results | query |
| context budget max | policy-defined | query |
| fields per record | 239 (structural) | decode |
| graph walk depth (why/context) | policy-defined | query |

All limits are fail-closed: exceeding one is an error, not a truncation of
security-relevant structure.

---

## 6. Confidentiality and Disclosure

- Producers and agent runs carry a disclosure policy (`FULL`, `DIGEST_ONLY`,
  `REDACTED`, `EXTERNAL_ATTESTATION`, `EPHEMERAL`).
- Defaults: instruction text is stored as a digest, not prose; conversation material is
  referenced, never required; reasoning is never required.
- The query and export layers enforce disclosure (SEC-08, FAM-10): a change with
  disclosure `REDACTED` exposes summaries and identities, never the underlying
  instruction or context contents.
- `instruction_digest` enables the proof "this change came from AgentRun A17 with
  ContextManifest C31 and instruction digest H(…)" without publishing private material
  (brief §20).

---

## 7. Integrity and Forensic Properties

- Every object is self-verifying (hash of its bytes; STO-01). Corruption is detected on
  read and by `fsck`; it is reported, never repaired silently.
- Every ref update is journaled and atomic (REF-02…04); the journal is an audit trail.
- `fsck` distinguishes corruption (`missing`, `corrupt`) from policy (`pruned`) —
  RET-05 — so an operator can tell "attacked/crashed" from "retention decided".
- GC writes audit entries (RET-07): what was removed, when, under which rule.
- Derived indexes cannot falsify history: rebuild from canonical objects always
  reproduces the same index (STO-05).

---

## 8. Workspace and Checkout Threats

- Materialization rejects any canonical path escaping the workspace root; symlinks in
  trees are materialized as symlinks whose targets are the blob content — never
  followed during checkout (SEC-04).
- Snapshot reads are bounded; special files (sockets, devices) are rejected.
- Workspace dirty state is recomputed conservatively on `status`, never trusted from
  disk alone (STO-08).

---

## 9. GC and Retention Threats

- Tombstones are created before unlink (RET-03); a tombstoned object yields `Pruned`
  with the tombstone — never a fabricated object (RET-04).
- Fake tombstones for live objects are detectable: fsck re-derives tombstones from
  policy and flags inconsistencies; Phase 4+ may sign tombstones.
- GC never prunes reachable objects without policy authorization (RET-02); open
  journal transactions are respected (RET-06).
- Deletion of Tier 1–3 blobs is the *intended* degradation; canonical identities never
  silently break.

---

## 10. Remote Protocol Threats (Phase 6, implemented)

- Fetch verification: every received record is hashed before use; mismatch aborts the
  transfer (SEC-07). Native sync re-verifies `BLAKE3(envelope) == id` on both
  directions; a single corrupt record fails the whole transfer and publishes no refs
  (DISTRIBUTED.md §3–§4).
- Negotiation: want/have sets are id sets; no executable payloads.
- Authentication/authorization: transport-scoped. The shipped `FileTransport`
  delegates to the filesystem; network transports MUST use mutual auth and
  capability-scoped fetch grants, with TLS mandatory for non-local remotes.
- Poisoning: remote-announced objects that fail hash verification are never inserted;
  the remote is reported. Same-id different-bytes is a fatal conflict (§11).
- Local-only refs never travel: public-ref filtering happens on the advertising side
  (DISTRIBUTED.md §2).
- No TLS downgrade: transport encryption is mandatory for non-local remotes.

---

## 11. Hash Collision Policy

- Identity uses BLAKE3-256. The failure mode "two distinct canonical objects with the
  same identity" is treated as a **security incident**: insert of an existing id with
  different bytes fails closed (STO-03), and fsck reports the pair. No silent
  resolution, ever.
- The residual risk of collision is negligible for the threat model's scale; if a
  future requirement demands stronger guarantees, identity may be extended
  (e.g., BLAKE3 with a second independent hash) via a new `encver` — a documented
  breaking change, never an in-place mutation.

---

## 12. Residual Risk Summary

| Area | Residual risk | Rationale / mitigation path |
|---|---|---|
| T-17 tombstone forgery | low | fsck re-derivation; signing in Phase 4+ |
| T-18 disclosure leakage via summaries | low–medium | summaries are producer-authored; audit + policy tightening in Phase 2+ |
| T-24 remote auth | medium (until P6) | Phase 6 design fixed; not shipped before |
| Hash collision | negligible | policy §11 |
| Zero-day in BLAKE3/Rust toolchain | low | dependency audit; Rust memory safety |
| Agent-context poisoning (crafted objects read by LLM agents) | medium | objects are presented as data with uncertainty markers; agents must not treat summaries as truth; mitigations: `uncertainty[]`, derived statuses, provenance display — Phase 2 hardening |

The security posture is **fail-closed by default**: what is not explicitly permitted
(re-execution, disclosure, remote trust, fabricated provenance) is denied.
