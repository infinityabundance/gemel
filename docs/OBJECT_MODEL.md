# Gemel — Object Model

Status: **Normative.** Version 1.0.0. Applies to `encver=1`, all families `schemever=1`.

This document defines, in order: the canonical encoding grammar (GCE), hashing and
object identity, textual identities and names, versioning and extension rules, the
complete family catalog (all twenty-two families, every field), graph semantics,
derived-status algorithms, object lifetime, compatibility rules, limits, and the golden
fixture specification.

The implementation (the `gemel` crate, `src/`) is a reference
implementation. Where implementation and this document disagree, this document is
authoritative and the disagreement is a defect.

---

## 1. Canonical Encoding (GCE)

### 1.1 Scope

The Gemel Canonical Encoding (GCE) is a deterministic binary encoding for all canonical
objects. It is designed so that:

- the same logical object encodes to the **same byte string on every machine and every
  implementation** (no endianness, no locale, no serializer defaults, no timestamps
  injected implicitly);
- an implementation can be written independently from this document alone;
- parsing is **fail-closed** (reject, never guess) and **bounded**;
- unknown optional extension fields can be **retained losslessly** by older readers
  where the schema permits extension;
- the byte string itself is the input to identity hashing (no separate canonicalization
  step exists).

There is **no compression inside canonical objects**. Compression is a transport-level
concern only (STORAGE.md §7.5) and never participates in identity.

### 1.2 Primitive grammar

Notation: `X*` zero or more; `X+` one or more; `len(X)` = byte length of X. All
multi-byte integers are little-endian.

**UINT** — canonical unsigned integer, value `v ∈ [0, 2^64−1]`.
Encoded as canonical LEB128: groups of 7 bits, least-significant group first; each byte
has a continuation bit in bit 7 (0x80) set on every byte except the last.

Canonical form requirements (violation ⇒ reject):
1. At most 10 bytes.
2. Value ≤ 2^64 − 1 (the 10th byte may carry at most 1 payload bit).
3. **Minimal form**: if the encoding is longer than one byte, the final byte must be
   non-zero. (Equivalently: an encoding that ends in 0x00 with more than one byte, or
   any redundant leading zero group, is non-minimal and rejected.)

Examples: `0 → 0x00`; `127 → 0x7F`; `128 → 0x80 0x01`; `33188 → 0xA4 0x83 0x02`.
Rejected: `0x80 0x00`, `0x81 0x00`, `0x80 0x80 0x00`.

**SINT** — signed integer, value `n ∈ [−2^63, 2^63−1]`. Encoded as UINT of the zigzag
map `z(n) = (n << 1) ^ (n >> 63)` (64-bit). Decode: `n = (z >> 1) ^ −(z & 1)` (as i64).

**BOOL** — exactly one byte: `0x00` (false) or `0x01` (true). Any other byte ⇒ reject.

**BYTES** — `UINT len` followed by exactly `len` raw bytes.

**STRING** — `UINT len` (byte length) followed by exactly `len` bytes that must form
**valid UTF-8** (strict: overlong forms, surrogates, and code points above U+10FFFF are
rejected). Strings are stored **byte-identical; no Unicode normalization is applied,
ever**, at any layer. `len` is the byte count, not the code-point count.

**GID** (object reference) — a 33-byte value: one family code byte followed by the
32-byte object digest (see §2). Encoded as `BYTES` of length 33. Decoders validate the
length and, where the field type declares an expected family, the family byte.

**RECORD** — `UINT len` followed by exactly `len` bytes that must parse as a **field
sequence** (§1.3).

**ARRAY** — `UINT count` followed by `count` concatenated element encodings. Element
encodings are self-delimiting by their element type (UINT/SINT/BOOL are fixed-form;
BYTES/STRING/GID/RECORD/ARRAY are length- or count-prefixed). `count` is bounded by the
array-element limit (§11).

**VALUE** — any of the above, as declared by the schema for the field in which it
appears.

### 1.3 Field sequence grammar

A field sequence is the body of a record (or of an object envelope, §1.4):

```
field      = tag value_len value
tag        = UINT          ; 0x01..=0xEF (see tag policy)
value_len  = UINT          ; byte length of value
value      = exactly value_len bytes, parseable as the schema-declared type for tag
```

Canonical form requirements (violation ⇒ reject):

1. **Ascending order**: tags strictly increase left to right.
2. **No duplicate tags.**
3. **Tag policy**:
   - `0x00` — forbidden.
   - `0x01..=0x7F` — schema-defined fields. A tag unknown to the reader's schema is a
     **mandatory-structure violation**: the object is rejected (**fail closed**).
   - `0x80..=0xEF` — **extension fields**. Permitted only for families whose schema
     declares extensions allowed (§6). An extension field unknown to the reader is
     retained **verbatim** (tag, value_len, value bytes) and re-emitted byte-identically
     on re-encode (lossless retention). If the family does not permit extensions, any
     extension tag ⇒ reject.
   - `0xF0..=0xFF` — reserved; any use ⇒ reject.
4. `value_len` must equal the byte length of the canonical value encoding for the
   declared type. (For extension fields the bytes are opaque; retained verbatim.)
5. Recursion depth (records within records) is bounded by the depth limit (§11).

Because tags are bounded by `0xEF`, a single record contains **at most 239 fields** —
a structural bound that needs no separate limit.

### 1.4 Object envelope

```
offset 0  MAGIC     = 0x47 0x45 0x4D 0x4C        ; "GEML"
offset 4  ENCVER    = 0x01                        ; encoding version (byte)
offset 5  FAMILY    = family code (§6)            ; byte
offset 6  SCHEMEVER = per-family schema version   ; byte, ≥ 1
offset 7  FLAGS     = 0x00                        ; byte, reserved
offset 8  BODYLEN   = UINT                        ; byte length of BODY
after     BODY      = exactly BODYLEN bytes: a field sequence (§1.3)
```

Envelope validation (violation ⇒ reject):

- `MAGIC` mismatch.
- `ENCVER` ≠ 1 (unknown encoding version).
- `FAMILY` not in the family table (§6).
- `SCHEMEVER` not supported by this reader for that family (§10; Phase 0: must be 1).
- `FLAGS` ≠ 0x00 (any bit set is rejected).
- `BODYLEN` exceeds the object-size limit (§11), or the input is shorter/longer than
  `8 + len(BODYLEN) + BODYLEN` (trailing garbage ⇒ reject).
- The body fails field-sequence validation (§1.3).

The envelope (all bytes, header and body) is the input to identity hashing (§2).
`blob` objects have an empty field sequence by definition; their body is the raw blob
bytes and is governed only by envelope rules.

### 1.5 Absence vs. null

- In canonical form, an optional field is either **present** (its tag appears in order)
  or **absent** (its tag does not appear). There is **no null encoding**.
- The JSON interchange form (AGENT_PROTOCOL.md §3.1) maps JSON `null` for an optional
  field to **absent** on import, and never emits `null` for absent optional fields on
  export. JSON `null` for a required field is an error.
- Schema-optionality is declared per field (§6); absence of a required field ⇒ reject.

### 1.6 Paths

Paths that appear as field values (operation subjects, tree names, claim scopes, …) are
**canonical repository-relative paths**:

- UTF-8, `'/'` as the only separator; `'\'` is invalid.
- No absolute path (no leading `/`), no empty segments (`//`), no trailing `/`.
- No `.` or `..` segments.
- Non-empty; each segment is a non-empty valid UTF-8 string that is not `.` or `..` and
  contains no `/`, `\`, or NUL.
- No Unicode normalization; comparison is byte comparison.

Absolute filesystem paths never participate in canonical identity. Workspace layers
translate OS-specific paths to and from canonical paths at the boundary
(STORAGE.md §6).

### 1.7 Determinism rules (recap)

The brief's §29 requirements are satisfied as follows:

| Requirement | GCE resolution |
|---|---|
| canonical byte encoding | §1.2–§1.4 of this document |
| normalized integer representation | canonical LEB128; zigzag for signed (§1.2) |
| deterministic map ordering | records are tag-sorted field sequences; there are no string-keyed maps in canonical form (§1.3) |
| deterministic string encoding | UTF-8 bytes, no normalization, byte length prefix (§1.2) |
| path normalization | §1.6 |
| schema version rules | §1.4, §10 |
| absence/null distinction | §1.5 |
| deterministic compression boundaries | no compression inside canonical objects; compression only at transport (§1.1, STORAGE.md §7.5) |

### 1.8 Worked example — `blob` containing bytes `0x68 0x69` ("hi")

```
47 45 4D 4C   magic "GEML"
01            encver 1
01            family blob
01            schemever 1
00            flags
02            bodylen = 2 (canonical LEB128)
68 69         body
```

Full canonical bytes: `47 45 4D 4C 01 01 01 00 02 68 69` (11 bytes).
Its identity digest (pinned by `golden/vectors/blob-hello.id`) is
`81430b0735af52231b2addeac4d52dd1ff14abf5232d9e25602ec841a8f3517a`, i.e. the
vector `blob.81430b…517a` is `BLAKE3-256(47454d4c01010100026869)`.
See also the `blob-empty` vector (bodylen 0).

### 1.9 Worked example — `tree` with one entry (illustrative digests)

A `tree` object with a single entry: name `a.txt`, mode `0o100644`, target blob.

Entry record (fields in ascending tag order):

| tag | value |
|---|---|
| 0x01 | STRING `a.txt` → `05 61 2E 74 78 74` (len 5 + bytes) |
| 0x02 | UINT 33188 → `A4 83 02` |
| 0x03 | GID → BYTES len 33: `21 01` + 32 digest bytes |

Fields (tag, value_len, value):

```
01 06 05 61 2E 74 78 74     ; name
02 03 A4 83 02              ; mode
03 22 21 01 <32 bytes>      ; target (family 0x01 = blob)
```

Record body = 49 bytes; record value = `31` + body (50 bytes).

Tree body field: tag 0x01 (entries), ARRAY count 1: value = `01` + record (51 bytes);
field = `01 33` + value (53 bytes). Tree body = 53 bytes = `0x35`.

Envelope: `47 45 4D 4C 01 02 01 00 35 <53 bytes>`.

The golden vectors in `golden/vectors/tree-*` pin real instances of this structure.

---

## 2. Hashing and Object Identity

- **Identity hash**: BLAKE3-256 (32-byte output) over the **full envelope bytes**
  (§1.4). `ObjectId = BLAKE3_256(envelope)`.
- The identity therefore commits to the family code, schema version, flags, length, and
  body. Two objects with identical envelopes have identical identities; objects with
  any differing byte differ in identity (collision resistance of BLAKE3-256).
- Identity is derived from nothing else: **never** timestamps, sequence numbers, random
  UUIDs, filesystem paths, process IDs, or repository-local counters. (Timestamp fields
  exist as *metadata* inside some objects and, being part of the body, contribute to
  identity when present. Callers that require byte-identical reconstruction across
  machines may omit optional timestamp metadata.)
- BLAKE3 is used in its plain, unkeyed, 32-byte mode. No truncation. No domain
  separation beyond the envelope bytes themselves (family is already inside the
  envelope).
- Golden vectors pin example digests (§12); any change to the encoding or hashing that
  alters a pinned digest is a breaking protocol change (regeneration policy §10.4).

---

## 3. Textual Identities and Names

### 3.1 Object identity textual form

```
<family-short>.<64 lowercase hex digits>
```

`family-short` values are fixed (§6). Example: `change.9f3a...e1`.
Grammar: `^[a-z]+(-[a-z]+)?\.[0-9a-f]{64}$`. Parsing is case-sensitive; uppercase
hex is rejected. The family prefix prevents type confusion and makes identities
self-describing; the digest alone (without family) is never a valid identity.

### 3.2 Names (refs)

Human-readable names are **mutable pointers** in the ref namespace (STORAGE.md §4)
and are distinct from identities. `T82`, `I17`, `C91`, `main`, `release/1.2` are names;
`trajectory.9f3a…` is an identity. Names may be retargeted; identities never change.

---

## 4. Versioning and Extension Rules

- **encver** changes only if the primitive grammar of §1 changes. Expected to remain 1
  for a very long time.
- **schemever** is per family and changes on any semantic or structural change to that
  family (field added in the mandatory range, field removed, semantics changed, enum
  values changed).
- **Additive evolution without a schemever bump** must use extension tags (0x80..=0xEF)
  for families that permit extensions. Older readers then retain the data losslessly.
- **Reader acceptance**: a reader accepts an object iff it knows the family, and the
  object's schemever is in that family's supported set (Phase 0: exactly `{1}`).
  Unknown higher schemever ⇒ reject (fail closed). Downgrade rules for future
  schemever-2 objects are defined per family at the time the bump is made, in §10.2.
- **Enum fields** are STRING-typed and validated against the declared value set at
  decode; an unknown value ⇒ reject. Extending an enum's value set therefore requires a
  schemever bump (or a new extension field).
- **Extension retention** is byte-exact: re-encoding an object that contained unknown
  extension fields must reproduce those fields byte-for-byte (tag, value_len, value),
  so a decode→re-encode cycle is the identity function on the envelope.

---

## 5. Limits

Defaults (overridable per repository via `config`; see §6.21 and THREAT_MODEL.md §5):

| Limit | Default | Enforced at |
|---|---|---|
| max object bytes (`BODYLEN` + header) | 1 GiB | envelope parse |
| max record depth | 64 | record parse |
| max array elements per array | 1,000,000 | array parse |
| max string bytes | 16 MiB | string parse |
| max refs (GID values) per object | 100,000 | object validation |
| max fields per record | 239 (structural, tag range) | tag parse |

A decoder MUST enforce these bounds before allocating; a parse that would exceed a bound
fails closed with a `LimitExceeded` error (THREAT_MODEL.md §4).

---

## 6. Family Catalog

Conventions: "Req" = required (absent ⇒ reject). Optional fields are absent-by-default
(§1.5). "Ext" = extensions permitted on this family. Types: UINT, SINT, BOOL, BYTES,
STRING, GID(family), GID* (any family), ARRAY<T>, RECORD{…}. Enum value sets are
normative. All tags within a family are unique and sorted as listed.

### 6.1 `blob` — code 0x01, schemever 1, Ext: n/a

Body = raw bytes (no field sequence). Envelope rules only. Invariants:
- Any byte content, including empty and binary.
- Blob identity = envelope identity (§2); the same raw content inside the envelope
  always yields the same identity.

### 6.2 `tree` — code 0x02, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | entries | ARRAY<RECORD{…}> | ✓ | See entry schema |

Entry record:

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | name | STRING | ✓ | Single path segment: no `/`, `\`, NUL; not `.` or `..`; non-empty; valid UTF-8 |
| 0x02 | mode | UINT | ✓ | One of `0o100644` (file), `0o100755` (executable file), `0o120000` (symlink), `0o040000` (directory); anything else ⇒ reject |
| 0x03 | target | GID | ✓ | `tree` for mode `0o040000`; `blob` otherwise |

Tree invariants:
- Entries sorted ascending by **name bytes** (bytewise UTF-8 order); duplicates ⇒ reject.
- Mode/type consistency (target family matches mode) enforced at decode.
- Symlink targets are the raw bytes of a blob (no interpretation; the blob content is
  the link target, which may be arbitrary bytes).
- Recursion: a tree's descendants are trees/blobs; depth bounded by limits (§5).
- No absolute paths, no `.`/`..` — names are segments (§1.6).

### 6.3 `state` — code 0x03, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | root_tree | GID(tree) | ✓ | Merkle root of the exact repository content |
| 0x80 | capture | RAW | | Capture coherence attestation (extension; see below) |

Invariants:
- A State represents pure content; it carries no machine, time, or producer (those
  belong to Change/Environment/Producer). It must be reproducible independently of the
  original working directory.
- The root tree (transitively) must exist and be valid (enforced by fsck; STORAGE.md
  §8).

Capture coherence attestation (`0x80`, extension, RAW):

- Present on States captured from a live working tree (`gemel snapshot`, `change
  finish`); absent on States synthesized by reconciliation or other pure constructions.
- Value: canonical JSON with sorted keys — `{"coherent": bool, "attempts": uint}`.
  `coherent` records whether the capture was **observationally coherent**: every
  entry's (size, mtime) was re-verified after reading, so the bytes correspond to a
  single observed moment. `attempts` records how many capture attempts were needed
  (cap: 3). An incoherent capture is recorded honestly, never silently claimed
  coherent (brief §34: a State must know whether it was observationally coherent;
  forensic provenance rejects the `A(old)+B(new)` mixture that may never have existed).
- No timestamp is recorded: identical stable captures deduplicate to the same state
  identity.

### 6.4 `operation` — code 0x04, schemever 1, Ext: yes

Common fields (all operation kinds):

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | op_type | STRING enum | ✓ | `create_file` `write_file` `write_range` `delete_file` `rename_path` `apply_patch` `exec_command` `run_test` `invoke_oracle` `inspect_artifact` `ast_transform` `other` |
| 0x02 | subject_path | STRING | | Canonical path (§1.6) this operation applies to |
| 0x03 | subject_ref | GID | | Canonical object this operation applies to (e.g., artifact) |
| 0x04 | input_refs | ARRAY<GID> | | Inputs (blobs, states, artifacts) |
| 0x05 | output_refs | ARRAY<GID> | | Outputs produced |
| 0x06 | result | RECORD{…} | | See result schema |
| 0x07 | producer | GID(producer) | | Who performed it |
| 0x08 | environment | GID(environment) | | Where it ran |
| 0x09 | started_at | SINT | | Unix milliseconds (metadata only) |
| 0x0A | ended_at | SINT | | Unix milliseconds (metadata only) |
| 0x0B | description | STRING | | Free-form, brief |
| 0x0C | outcome_refs | ARRAY<GID> | | Evidence/artifacts produced by this operation |

Result record:

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | status | STRING enum | ✓ | `ok` `failed` `partial` `skipped` `inconclusive` |
| 0x02 | detail | STRING | | Human-readable detail |
| 0x03 | exit_code | SINT | | Process exit code, where applicable |
| 0x04 | refs | ARRAY<GID> | | Result artifacts |

Per-kind parameter tags (0x11..=0x29), normative. Each tag has exactly **one** type
across all kinds (no tag ambiguity); each tag is valid only for the kinds listed:

| Tag | Field | Type | Valid for op_types |
|---|---|---|---|
| 0x11 | content | GID(blob) | `create_file`, `write_file` |
| 0x12 | start | UINT | `write_range` |
| 0x13 | length | UINT | `write_range` |
| 0x14 | new_content | GID(blob) | `write_range` |
| 0x15 | old_content | GID(blob) | `write_range` |
| 0x16 | from | STRING (canonical path) | `rename_path` |
| 0x17 | to | STRING (canonical path) | `rename_path` |
| 0x18 | patch | GID(blob) | `apply_patch` |
| 0x19 | patch_format | STRING | `apply_patch` |
| 0x1A | argv | ARRAY<STRING> | `exec_command` |
| 0x1B | cwd | STRING | `exec_command` |
| 0x1C | stdin_ref | GID | `exec_command` |
| 0x1D | stdout_ref | GID | `exec_command` |
| 0x1E | stderr_ref | GID | `exec_command` |
| 0x1F | test_command | STRING | `run_test` |
| 0x20 | test_ids | ARRAY<STRING> | `run_test` |
| 0x21 | tool | STRING | `run_test` |
| 0x22 | oracle | STRING | `invoke_oracle` |
| 0x23 | oracle_version | STRING | `invoke_oracle` |
| 0x24 | query | GID | `invoke_oracle` |
| 0x25 | response | GID | `invoke_oracle` |
| 0x26 | artifact | GID | `inspect_artifact` |
| 0x27 | transform | STRING | `ast_transform` |
| 0x28 | input_ast | GID | `ast_transform` |
| 0x29 | output_ast | GID | `ast_transform` |

Invariant (kind-tag matrix): a parameter tag present in an operation must be declared
for that operation's `op_type`; otherwise the object is rejected. `other` permits any
0x11..=0x7F parameter tag, with meaning documented in `description`.

### 6.5 `episode` — code 0x05, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(episode) | | Prior episode in the same sequence (append chain) |
| 0x02 | intent | GID(intent) | | Intent pursued (may be inherited) |
| 0x03 | input_state | GID(state) | | State at episode start |
| 0x04 | operations | ARRAY<GID(operation)> | | Ordered operations of this episode |
| 0x05 | output_state | GID(state) | | State at episode end |
| 0x06 | producer | GID(producer) | | Actor |
| 0x07 | agent_run | GID(agentrun) | | Agent execution, when applicable |
| 0x08 | environment | GID(environment) | | Environment |
| 0x09 | summary | STRING | | Brief summary |
| 0x0A | outcome | STRING enum | | `completed` `interrupted` `aborted` `inconclusive` |
| 0x0B | started_at | SINT | | Unix milliseconds (metadata) |
| 0x0C | ended_at | SINT | | Unix milliseconds (metadata) |
| 0x0D | trajectory | GID(trajectory) | | Owning trajectory, when known |

An Episode is not defined solely by prompts or LLM turns; any actor (including
non-LLM automation) is representable via producer.

### 6.6 `intent` — code 0x06, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | summary | STRING | ✓ | One-line statement of intent |
| 0x02 | description | STRING | | Extended description |
| 0x03 | acceptance_criteria | ARRAY<STRING> | | Verifiable completion criteria |
| 0x04 | constraints | ARRAY<STRING> | | Binding constraints |
| 0x05 | requested_scope | ARRAY<STRING> | | In-scope subjects (paths/entities) |
| 0x06 | prohibited_scope | ARRAY<STRING> | | Out-of-scope subjects |
| 0x07 | related_objects | ARRAY<GID> | | Related canonical objects |
| 0x08 | parent_intent | GID(intent) | | Decomposition parent |
| 0x09 | external_refs | ARRAY<RECORD{…}> | | External references (see below) |
| 0x0A | case | GID(case) | | Owning case, when known |
| 0x0B | producer | GID(producer) | | Author |
| 0x0C | created_at | SINT | | Unix milliseconds (metadata) |

External ref record: 0x01 name: STRING; 0x02 uri: STRING; 0x03 digest: BYTES.

Invariants: Intent is independently addressable; multiple trajectories may share one
Intent; an Intent is immutable (decomposition expressed via `parent_intent`).

### 6.7 `change` — code 0x07, schemever 1, Ext: yes — **the central object**

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | summary | STRING | ✓ | One-line summary |
| 0x02 | intent | GID(intent) | | The why |
| 0x03 | input_state | GID(state) | | Exact state before |
| 0x04 | operations | ARRAY<GID(operation)> | | Ordered operations applied |
| 0x05 | resulting_state | GID(state) | | Exact state after |
| 0x06 | producer | GID(producer) | ✓ | Who made the change |
| 0x07 | agent_run | GID(agentrun) | | Agent execution identity |
| 0x08 | context_manifest | GID(context-manifest) | | What the producer saw |
| 0x09 | disclosure | STRING enum | | `FULL` `DIGEST_ONLY` `REDACTED` `EXTERNAL_ATTESTATION` `EPHEMERAL` |
| 0x0A | instruction_digest | BYTES | | BLAKE3-256 of the instruction, if disclosed as digest |
| 0x0B | environment | GID(environment) | | Where it was made |
| 0x0C | claims | ARRAY<GID(claim)> | | Claims declared by this change |
| 0x0D | evidence | ARRAY<GID(evidence)> | | Evidence produced by this change |
| 0x0E | residuals | ARRAY<GID(residual)> | | Residuals introduced/observed |
| 0x0F | verification | ARRAY<GID(verification)> | | Verification runs attached |
| 0x10 | dependencies | ARRAY<GID> | | Explicit dependencies on any objects |
| 0x11 | causal_parents | ARRAY<GID(change)> | | Causal (not merely textual) parents |
| 0x12 | case | GID(case) | | Owning case |
| 0x13 | trajectory | GID(trajectory) | | Owning trajectory |
| 0x14 | episode | GID(episode) | | Owning episode |
| 0x15 | created_at | SINT | | Unix milliseconds (metadata) |

Invariants:
- `causal_parents` edges form a DAG (acyclic; fsck-verified). A reconciliation result
  may have multiple causal parents (§6.17, §8.5).
- At least one of {operations, resulting_state} should be present for a material change;
  metadata-only changes (e.g., a claims-only declaration) are permitted.
- `disclosure` governs how much of the provenance chain may be revealed; it never
  changes identity.

### 6.8 `case` — code 0x08, schemever 1, Ext: yes (append-chained)

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(case) | | Prior version (append chain) |
| 0x02 | summary | STRING | ✓ | Objective statement, e.g. "Achieve BIND-compatible compressed DNS-name behavior" |
| 0x03 | intent | GID(intent) | | Root intent |
| 0x04 | description | STRING | | Extended |
| 0x05 | status | STRING enum | | `open` `active` `closed` `abandoned` |
| 0x06 | added_changes | ARRAY<GID(change)> | | Changes appended by this version |
| 0x07 | added_trajectories | ARRAY<GID(trajectory)> | | Trajectories appended by this version |
| 0x08 | releases | ARRAY<GID(release)> | | Releases derived |
| 0x09 | producer | GID(producer) | | Updater |
| 0x0A | created_at | SINT | | Unix ms (metadata) |
| 0x0B | updated_at | SINT | | Unix ms (metadata) |

### 6.9 `trajectory` — code 0x09, schemever 1, Ext: yes (append-chained)

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(trajectory) | | Prior version (append chain) |
| 0x02 | intent | GID(intent) | | Intent pursued |
| 0x03 | base_state | GID(state) | | State the trajectory derives from |
| 0x04 | producer | GID(producer) | | Principal producer |
| 0x05 | agent_run | GID(agentrun) | | Agent execution, when applicable |
| 0x06 | added_changes | ARRAY<GID(change)> | | Changes appended by this version |
| 0x07 | added_episodes | ARRAY<GID(episode)> | | Episodes appended by this version |
| 0x08 | added_evidence | ARRAY<GID(evidence)> | | Evidence accumulated by this version |
| 0x09 | added_residuals | ARRAY<GID(residual)> | | Residuals encountered by this version |
| 0x0A | outcome | STRING enum | | `completed` `abandoned` `superseded` `rejected` `inconclusive` `interrupted` |
| 0x0B | termination_reason | STRING | | Why it ended (machine-summarizable) |
| 0x0C | handoff | RECORD{…} | | Structured continuation state (§7.4) |
| 0x0D | created_at | SINT | | Unix ms (metadata) |
| 0x0E | updated_at | SINT | | Unix ms (metadata) |

Handoff record:

| Tag | Field | Type | Req | Description |
|---|---|---|---|---|
| 0x01 | summary | STRING | | Generated handoff summary |
| 0x02 | completed | ARRAY<STRING> | | Completed items |
| 0x03 | remaining | ARRAY<STRING> | | Remaining items |
| 0x04 | open_residuals | ARRAY<GID> | | Residuals still open |
| 0x05 | important_evidence | ARRAY<GID> | | Evidence to carry forward |
| 0x06 | recommended_objects | ARRAY<GID> | | Suggested next objects |
| 0x07 | next_steps | ARRAY<STRING> | | Suggested next actions |

Invariants: the full sequence of changes of a trajectory = the concatenation of
`added_changes` across the chain from the earliest version to the latest (head).
Unsuccessful trajectories are preserved; outcome is metadata, not deletion.

### 6.10 `claim` — code 0x0A, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | subject | STRING | | Subject path/entity name the claim is about |
| 0x02 | subject_refs | ARRAY<GID> | | Canonical subject objects |
| 0x03 | predicate | STRING | ✓ | The asserted proposition ("parser accepts all valid RFC inputs") |
| 0x04 | predicate_kind | STRING enum | | `compatibility` `correctness` `performance` `security` `safety` `invariant` `behavior` `other` |
| 0x05 | scope | STRING | | Declared scope (paths, platforms, configs) |
| 0x06 | scope_refs | ARRAY<GID> | | Scoped objects |
| 0x07 | producer | GID(producer) | ✓ | Declarer |
| 0x08 | evidence | ARRAY<GID(evidence)> | | Evidence relevant to this claim |
| 0x09 | residuals | ARRAY<GID(residual)> | | Residuals relevant to this claim |
| 0x0A | dependencies | ARRAY<GID> | | Objects the claim depends on |
| 0x0B | supersedes | GID(claim) | | The claim this one replaces |
| 0x0C | change | GID(change) | | The change that declared it |
| 0x0D | assertion | STRING | | Human-readable assertion text |
| 0x0E | created_at | SINT | | Unix ms (metadata) |

Invariants: **status is never stored.** The derived status (§8.1) is computed from
evidence/residual/supersession relationships. A claim is a declaration, not a fact.

### 6.11 `evidence` — code 0x0B, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | producer | GID(producer) | ✓ | Who produced the evidence |
| 0x02 | kind | STRING enum | ✓ | `test_result` `compiler_result` `fuzz_result` `benchmark` `oracle_comparison` `static_analysis` `formal_proof` `binary_comparison` `runtime_trace` `replay` `environment_manifest` `artifact_hash` `external_attestation` `court_receipt` |
| 0x03 | subject | STRING | | What it is evidence about |
| 0x04 | subject_refs | ARRAY<GID> | | Canonical subjects |
| 0x05 | command | STRING | | Command/operation text (inert data; never auto-executed) |
| 0x06 | command_ref | GID(operation) | | The operation object |
| 0x07 | input_refs | ARRAY<GID> | | Input identities |
| 0x08 | environment | GID(environment) | | Environment identity |
| 0x09 | tools | ARRAY<RECORD{…}> | | Tool identities (name/version/digest) |
| 0x0A | fixtures | ARRAY<GID> | | Fixtures used |
| 0x0B | normalizers | ARRAY<RECORD{…}> | | Normalizer identities (name/digest) |
| 0x0C | comparators | ARRAY<RECORD{…}> | | Comparator identities (name/digest) |
| 0x0D | result | RECORD{…} | | Structured result (§below) |
| 0x0E | output_refs | ARRAY<GID> | | Output identities (logs, artifacts) |
| 0x0F | reproduction | RECORD{…} | | Reproduction information (inert) |
| 0x10 | created_at | SINT | | Unix ms (metadata) |
| 0x11 | evaluated_state | GID(state) | | State against which this evidence was evaluated (freshness anchor, §8.2) |

Tool record: 0x01 name: STRING; 0x02 version: STRING; 0x03 digest: BYTES.
Normalizer/comparator record: 0x01 name: STRING; 0x02 digest: BYTES.

Result record:

| Tag | Field | Type | Req | Description |
|---|---|---|---|---|
| 0x01 | outcome | STRING enum | ✓ | `pass` `fail` `mismatch` `inconclusive` `error` `skipped` |
| 0x02 | detail | STRING | | Human-readable |
| 0x03 | exit_code | SINT | | Exit code |
| 0x04 | counts | RECORD{…} | | Aggregate counts |

Counts record: 0x01 passed: UINT; 0x02 failed: UINT; 0x03 skipped: UINT; 0x04 total: UINT.

Reproduction record:

| Tag | Field | Type | Req | Description |
|---|---|---|---|---|
| 0x01 | replayable | BOOL | | Whether replay is possible from stored inputs |
| 0x02 | inputs_present | BOOL | | Whether all inputs are present locally |
| 0x03 | inputs_remote | BOOL | | Whether inputs are archived remotely |
| 0x04 | policy_required | BOOL | | Whether execution requires policy approval |

Invariants: evidence is never reduced to `passed = true`; scope, inputs, environment,
tools, and reproduction information are preserved. Evidence is immutable. Evidence is
never interpreted for claims implicitly: the claim→evidence edge is explicit (§8.1).

### 6.12 `residual` — code 0x0C, schemever 1, Ext: yes (append-chained)

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(residual) | | Prior version (append chain) |
| 0x02 | summary | STRING | ✓ | Statement of the unresolved disagreement |
| 0x03 | classification | STRING enum | | `semantic_divergence` `expected_mismatch` `platform_divergence` `performance_divergence` `unexplained_divergence` `contract_mismatch` `verification_gap` `other` |
| 0x04 | severity | STRING enum | | `low` `medium` `high` `blocking` |
| 0x05 | scope | RECORD{…} | | Scope (see below) |
| 0x06 | affected_claims | ARRAY<GID(claim)> | | Claims affected |
| 0x07 | affected_changes | ARRAY<GID(change)> | | Changes affected |
| 0x08 | origin_evidence | GID(evidence) | | Where it was first observed |
| 0x09 | first_observed_at | SINT | | Unix ms (metadata) |
| 0x0A | disposition_event | RECORD{…} | | Latest disposition decision (§8.3) |
| 0x0B | recurrence | ARRAY<GID(residual)> | | Recurring/rerelated residuals |
| 0x0C | created_at | SINT | | Unix ms (metadata) |

Scope record: 0x01 intent: GID(intent); 0x02 trajectories: ARRAY<GID>; 0x03 paths:
ARRAY<STRING>; 0x04 entities: ARRAY<GID>.

Disposition event record:

| Tag | Field | Type | Req | Description |
|---|---|---|---|---|
| 0x01 | disposition | STRING enum | ✓ | `open` `acknowledged` `resolved` `superseded` `irrelevant` |
| 0x02 | by | GID(producer) | ✓ | Who decided |
| 0x03 | evidence | GID(evidence) | | Evidence basis |
| 0x04 | reconciliation | GID(reconciliation) | | Reconciliation that resolved it |
| 0x05 | reason | STRING | | Rationale |
| 0x06 | at | SINT | | Unix ms (metadata) |

Invariants: a residual survives unless a disposition_event explicitly changes its state.
The **current disposition** is the disposition_event of the latest version in the chain;
absent any event, disposition is `open`. Persistence (descendant-change count) is
derived, never stored (§8.3).

### 6.13 `verification` — code 0x0D, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | subject | STRING | | What was verified |
| 0x02 | subject_refs | ARRAY<GID> | | Canonical subjects |
| 0x03 | scope | RECORD{…} | | Scope (see below) |
| 0x04 | claims | ARRAY<GID(claim)> | | Claims addressed |
| 0x05 | evidence | ARRAY<GID(evidence)> | | Results collected |
| 0x06 | residuals | ARRAY<GID(residual)> | | Residuals observed |
| 0x07 | result | STRING enum | ✓ | `pass` `partial` `fail` `inconclusive` `not_run` |
| 0x08 | producer | GID(producer) | | Runner |
| 0x09 | environment | GID(environment) | | Environment |
| 0x0A | started_at | SINT | | Unix ms (metadata) |
| 0x0B | ended_at | SINT | | Unix ms (metadata) |
| 0x0C | policy | GID(config) | | Policy under which the run was authorized |

Scope record: 0x01 platforms: ARRAY<RECORD{0x01 os: STRING, 0x02 arch: STRING, 0x03
variant: STRING}>; 0x02 configs: ARRAY<STRING>; 0x03 tool_versions: ARRAY<RECORD{0x01
name: STRING, 0x02 version: STRING}>.

Invariants: every verification result retains its scope. There is no global
`verified = true`; a global statement is only valid when the term is explicitly scoped
and policy-defined (§8.4).

### 6.14 `producer` — code 0x0E, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | kind | STRING enum | ✓ | `human` `agent` `automation` `compiler` `fuzzer` `external_oracle` `git_import` `unknown` |
| 0x02 | name | STRING | ✓ | Display name |
| 0x03 | identity | RECORD{…} | | Kind-specific identity (see below) |
| 0x04 | disclosure | STRING enum | ✓ | `FULL` `DIGEST_ONLY` `REDACTED` `EXTERNAL_ATTESTATION` `EPHEMERAL` |
| 0x05 | attestation | GID(evidence) | | External attestation |
| 0x06 | created_at | SINT | | Unix ms (metadata) |

Identity record (kind-conditional): 0x01 human: RECORD{0x01 name: STRING, 0x02 email:
STRING}; 0x02 agent: RECORD{0x01 model_family: STRING, 0x02 model_id: STRING, 0x03
harness: STRING, 0x04 permissions: ARRAY<STRING>}; 0x03 automation: RECORD{0x01 system:
STRING, 0x02 version: STRING}.

Invariants: `kind` `human` `agent` `automation` `compiler` `fuzzer` `external_oracle`
are sufficient for the corresponding identity sub-record; `git_import`/`unknown` may
carry only name/disclosure. `unknown` is the correct value when provenance is
unavailable — never a fabricated identity. Disclosure policy governs publication, never
identity.

### 6.15 `agentrun` — code 0x0F, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | producer | GID(producer) | ✓ | Agent producer |
| 0x02 | model_family | STRING | | Model family |
| 0x03 | model_id | STRING | | Model identifier |
| 0x04 | harness | STRING | | Agent harness/tooling |
| 0x05 | permissions | ARRAY<STRING> | | Tool permissions granted |
| 0x06 | input_state | GID(state) | | State at run start |
| 0x07 | intent | GID(intent) | | Intent pursued |
| 0x08 | context_manifest | GID(context-manifest) | | What the agent saw |
| 0x09 | instruction_digest | BYTES | | BLAKE3-256 of the instruction (privacy-preserving) |
| 0x0A | tool_identities | ARRAY<RECORD{…}> | | Tool name/version/digest |
| 0x0B | environment | GID(environment) | | Environment |
| 0x0C | parent | GID(agentrun) | | Parent execution |
| 0x0D | output_trajectory | GID(trajectory) | | Resulting trajectory |
| 0x0E | disclosure | STRING enum | ✓ | `FULL` `DIGEST_ONLY` `REDACTED` `EXTERNAL_ATTESTATION` `EPHEMERAL` |
| 0x0F | conversation_ref | GID | | Optional reference to conversation material (never required for correctness) |
| 0x10 | started_at | SINT | | Unix ms (metadata) |
| 0x11 | ended_at | SINT | | Unix ms (metadata) |

Invariants: private prompt text is never required; `instruction_digest` + disclosure
policy prove provenance without publishing reasoning (§20 of the brief). Conversations
are optional evidence, not structural requirements.

### 6.16 `environment` — code 0x10, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | os | RECORD{…} | | OS identity (family/name/version/kernel) |
| 0x02 | arch | STRING | | Architecture |
| 0x03 | compiler | RECORD{…} | | Compiler name/version |
| 0x04 | runtime | RECORD{…} | | Runtime name/version |
| 0x05 | toolchain | ARRAY<RECORD{…}> | | Toolchain (name/version/digest) |
| 0x06 | hardware | RECORD{…} | | cpu/cores/memory_bytes |
| 0x07 | container | RECORD{…} | | image_digest/runtime |
| 0x08 | network | STRING enum | | `none` `restricted` `full` `unknown` |
| 0x09 | env_manifest | GID(blob) | | Canonicalized sorted environment manifest |
| 0x0A | determinism | STRING enum | | `fully_deterministic` `reproducible_with_fixture` `best_effort` `unknown` |
| 0x0B | created_at | SINT | | Unix ms (metadata) |

OS record: 0x01 family, 0x02 name, 0x03 version, 0x04 kernel (all STRING).
Compiler/runtime record: 0x01 name, 0x02 version (STRING).
Toolchain record: 0x01 name, 0x02 version (STRING), 0x03 digest (BYTES).
Hardware record: 0x01 cpu (STRING), 0x02 cores (UINT), 0x03 memory_bytes (UINT).
Container record: 0x01 image_digest (BYTES), 0x02 runtime (STRING).

Invariants: environment manifests are content (blobs) and may be malicious or wrong —
they are inert data, never executed (§48 of the brief; THREAT_MODEL.md §6).

### 6.17 `reconciliation` — code 0x11, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | summary | STRING | ✓ | One-line decision summary |
| 0x02 | intent | GID(intent) | | Intent reconciled |
| 0x03 | input_trajectories | ARRAY<GID(trajectory)> | | Input trajectories |
| 0x04 | input_states | ARRAY<GID(state)> | | Input states |
| 0x05 | adopted_changes | ARRAY<GID(change)> | | Changes adopted |
| 0x06 | rejected_changes | ARRAY<GID(change)> | | Changes rejected (preserved, not erased) |
| 0x07 | unresolved_residuals | ARRAY<GID(residual)> | | Carried forward |
| 0x08 | resolved_residuals | ARRAY<GID(residual)> | | Resolved by this reconciliation |
| 0x09 | semantic_interactions | ARRAY<RECORD{…}> | | Interactions considered (§8.5) |
| 0x0A | claims_retained | ARRAY<GID(claim)> | | Claims retained |
| 0x0B | claims_invalidated | ARRAY<GID(claim)> | | Claims invalidated |
| 0x0C | evidence_retained | ARRAY<GID(evidence)> | | Evidence retained |
| 0x0D | verification_required | ARRAY<GID(verification)> | | New verification required |
| 0x0E | resulting_state | GID(state) | | Resulting state |
| 0x0F | resulting_change | GID(change) | | The change embodying the result |
| 0x10 | rationale | STRING | | Why this direction |
| 0x11 | producer | GID(producer) | | Reconciler |
| 0x12 | created_at | SINT | | Unix ms (metadata) |

Semantic interaction record: 0x01 kind: STRING enum (`textual` `semantic` `claim`
`invariant` `dependency` `behavioral` `verification`); 0x02 certainty: STRING enum
(`observed` `possible` `unknown`); 0x03 subjects: ARRAY<GID>; 0x04 severity: STRING;
0x05 detail: STRING.

Invariants: inputs are never erased; a reconciliation chooses a direction while
recording that alternatives existed. `certainty: unknown/possible` is preferred over
invented certainty (§13 of the brief).

### 6.18 `release` — code 0x12, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | name | STRING | ✓ | Release name |
| 0x02 | version | STRING | | Version string |
| 0x03 | state | GID(state) | ✓ | Distributable state |
| 0x04 | changes | ARRAY<GID(change)> | | Changes included |
| 0x05 | cases | ARRAY<GID(case)> | | Cases completed |
| 0x06 | claims | ARRAY<GID(claim)> | | Claims accepted for this release |
| 0x07 | residuals | ARRAY<GID(residual)> | | Known open residuals |
| 0x08 | verification | ARRAY<GID(verification)> | | Verification attached |
| 0x09 | artifacts | ARRAY<RECORD{…}> | | Artifact name/digest/uri |
| 0x0A | producer | GID(producer) | | Author |
| 0x0B | created_at | SINT | | Unix ms (metadata) |

Artifact record: 0x01 name: STRING; 0x02 digest: BYTES; 0x03 uri: STRING.

### 6.19 `context-manifest` — code 0x13, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | source_objects | ARRAY<GID> | | Source content the agent saw |
| 0x02 | documentation_objects | ARRAY<GID> | | Docs seen |
| 0x03 | claims | ARRAY<GID(claim)> | | Claims seen |
| 0x04 | residuals | ARRAY<GID(residual)> | | Residuals seen |
| 0x05 | previous_trajectories | ARRAY<GID(trajectory)> | | Prior trajectories seen |
| 0x06 | external_artifacts | ARRAY<GID> | | External artifacts |
| 0x07 | tool_outputs | ARRAY<GID> | | Tool outputs |
| 0x08 | policies | ARRAY<GID(config)> | | Policies in force |
| 0x09 | producer | GID(producer) | | Builder |
| 0x0A | instruction | STRING | | Instruction text (optional; digest preferred) |
| 0x0B | instruction_digest | BYTES | | BLAKE3-256 of instruction |
| 0x0C | created_at | SINT | | Unix ms (metadata) |

Invariants: a ContextManifest is content-addressed and enables `context-diff`
(AGENT_PROTOCOL.md §8): "A91 saw Residual R882; A92 did not."

### 6.20 `checkpoint` — code 0x14, schemever 1, Ext: yes (append-chained)

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(checkpoint) | | Prior checkpoint |
| 0x02 | summary | STRING | ✓ | Continuation summary |
| 0x03 | intent | GID(intent) | | Current intent |
| 0x04 | trajectory | GID(trajectory) | | Current trajectory |
| 0x05 | state | GID(state) | | Current state |
| 0x06 | open_claims | ARRAY<GID(claim)> | | Open claims |
| 0x07 | unresolved_residuals | ARRAY<GID(residual)> | | Unresolved residuals |
| 0x08 | important_evidence | ARRAY<GID(evidence)> | | Evidence to carry |
| 0x09 | recent_decisions | ARRAY<GID(change)> | | Recent decisions |
| 0x0A | relevant_attempts | ARRAY<GID(trajectory)> | | Relevant previous attempts |
| 0x0B | continuation_scope | ARRAY<STRING> | | Suggested continuation scope |
| 0x0C | producer | GID(producer) | | Author |
| 0x0D | created_at | SINT | | Unix ms (metadata) |

A checkpoint is a **continuation boundary**, not a commit (§36 of the brief). It is
machine-generatable from structured repository state.

### 6.21 `config` — code 0x15, schemever 1, Ext: yes (append-chained)

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | previous | GID(config) | | Prior version (append chain) |
| 0x02 | retention | RECORD{…} | | Retention policy (§STORAGE 7) |
| 0x03 | gc | RECORD{…} | | GC policy |
| 0x04 | execution_policy | STRING enum | ✓ | `never_auto_execute` `policy_gated` `allowlist` |
| 0x05 | disclosure_default | STRING enum | | Default disclosure for new provenance |
| 0x06 | limits | RECORD{…} | | Parse/query limits (§5, THREAT_MODEL §5) |
| 0x07 | created_at | SINT | | Unix ms (metadata) |
| 0x08 | required_verification | RECORD{0x01 entries: ARRAY<RECORD{0x01 kind: STRING, 0x02 platforms: ARRAY<RECORD{0x01 platform: STRING, 0x02 arch: STRING}>}>} | | The required-verification matrix (Phase 7): which claim kind × platform/arch combinations must have supporting evidence before readiness may count claims as verified (OBJECT_MODEL §8.4). Missing required verification ⇒ readiness `NOT_READY` |

Retention record: 0x01 tiers: ARRAY<RECORD{0x01 tier: UINT, 0x02 policy: STRING enum
(`retain_forever` `retain_policy` `prune_after_days` `size_limit_bytes`
`archive_remote`), 0x03 days: UINT, 0x04 bytes: UINT, 0x05 remote: STRING}>; 0x02
default_unknown: STRING enum (`retain` `prune` `archive`).

GC record: 0x01 enabled: BOOL; 0x02 interval_days: UINT.

Limits record: 0x01 max_object_bytes: UINT; 0x02 max_record_depth: UINT; 0x03
max_array_elements: UINT; 0x04 max_refs_per_object: UINT; 0x05 max_string_bytes: UINT.

Invariants: execution_policy defaults to `never_auto_execute` on repository creation;
changing it is an audited policy change (new config version).

### 6.22 `mapping` — code 0x16, schemever 1, Ext: yes

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | kind | STRING enum | ✓ | `git_commit` `git_tree` `external` |
| 0x02 | from | STRING | ✓ | External identifier (e.g., git commit hex) |
| 0x03 | to | GID | ✓ | Gemel object identity |
| 0x04 | loss | RECORD{…} | | Loss documentation (see below) |
| 0x05 | producer | GID(producer) | | Creator |
| 0x06 | created_at | SINT | | Unix ms (metadata) |

Loss record: 0x01 known_loss: ARRAY<STRING>; 0x02 unknowns: ARRAY<STRING>; 0x03
fabricated: ARRAY<STRING> — **must be empty** (invariant: Gemel never fabricates).

Invariants: mapping objects make Git interchange deterministic and auditable
(GIT_INTEROP.md §2). `from` is a string exactly as the external system writes it.

### 6.23 `semantic-entity` — code 0x17, schemever 1, Ext: yes

A deterministically derived declaration identity (Phase 5; brief §22–§24). Entities
are **derived facts** — deterministic scanner output over observed source bytes — never
model-confidence or natural-language truth. Permanent semantic identity is never
silently inferred from heuristics: a changed or moved entity becomes a new object
linked to its predecessor by explicit lineage.

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | kind | STRING enum | ✓ | `module` `type` `trait` `impl` `function` `method` `constant` `static` `test` `feature` `dependency` `other` |
| 0x02 | name | STRING | ✓ | Declared name (for `impl Trait for Type`, `Trait for Type`) |
| 0x03 | module_path | STRING | ✓ | `crate::parser`-style path (file-derived + inline modules) |
| 0x04 | file_path | STRING | | Repository-relative source path |
| 0x05 | start_line | U64 | | 1-based; 0 = unknown |
| 0x06 | end_line | U64 | | 1-based, inclusive; 0 = unknown |
| 0x07 | signature | STRING | | Item header (keyword…body-open or `;`), whitespace-collapsed, capped |
| 0x08 | visibility | STRING enum | | `public` `crate` `private` `unknown` |
| 0x09 | parent | GID(entity) | | Containing entity (reserved; not yet populated) |
| 0x0A | lineage_from | GID(entity) | | Predecessor entity (edit/move predecessor) |
| 0x0B | lineage_evidence | STRING | | Why the lineage is claimed: `same-name-kind-path` (observed) or `similarity:same-name-kind` (possible) |
| 0x0C | lineage_certainty | STRING enum | | `observed` `possible` `unknown` |
| 0x0D | dependencies | ARRAY<GID(entity)> | | Resolved semantic dependencies (reserved; not yet populated) |
| 0x0E | state | GID(state) | | The state the entity was derived from |
| 0x0F | producer | GID(producer) | | The deterministic indexer (`semantic-indexer`), published on first use |
| 0x10 | created_at | SINT | | 0 for derived entities (determinism: derivation time is not identity) |

Invariants: an entity's identity is the content hash of its full description, so an
unchanged declaration deduplicates across states and re-derivation is byte-identical.
`lineage_from` is written **only** with a documented evidence string and certainty;
module renames and file moves yield `possible`, never a silent merge. The entity object
must never reference an unpublished producer (fsck reachability).

### 6.24 `semantic-index` — code 0x18, schemever 1, Ext: yes

The per-state grouping of derived entities (Phase 5). A disposable accelerator in the
spirit of the SQLite index: the canonical objects + refs are truth; deleting and
rebuilding an index must never change query answers.

| Tag | Field | Type | Req | Description / Invariants |
|---|---|---|---|---|
| 0x01 | state | GID(state) | ✓ | The indexed state |
| 0x02 | entities | ARRAY<GID(entity)> | ✓ | Deterministic entity order (module_path, kind, name, file_path, start_line) |
| 0x03 | producer | GID(producer) | | The deterministic indexer |
| 0x04 | created_at | SINT | | 0 (deterministic) |

Refs: `refs/semantic/state/<state-hex>` per state, `refs/semantic/current` (most recent
build), `refs/semantic/head` (the head state's index). The exchange frontier seeds
`refs/semantic/head` and ingestion re-establishes the refs on activation, so a fresh
Git clone recovers identical semantic identities (EXCHANGE.md §11, §56).

---

## 7. Graph Semantics

### 7.1 Edges

References (GID fields) form a directed graph over objects. Canonical edge kinds:

| Edge | From | To | Meaning |
|---|---|---|---|
| ownership | change/episode | trajectory, case | containment |
| causality | change | change (causal_parents) | causal predecessor |
| derivation | trajectory/case/residual/checkpoint/config | same family (previous) | append chain |
| declaration | change | claim | claim introduced by change |
| support | claim | evidence | evidence relevant to claim |
| contradiction-context | claim | residual | residual relevant to claim |
| observation | residual | evidence (origin_evidence) | where observed |
| aggregation | verification | evidence/claims/residuals | run contents |
| provenance | change/episode | producer, agentrun, environment, context-manifest | who/where/what-seen |
| content | state | tree | content root |
| composition | tree | tree/blob | Merkle content |
| reconciliation | reconciliation | trajectories, changes, residuals | inputs/outcomes |
| mapping | mapping | any | external correspondence |
| lineage | semantic-entity | semantic-entity (lineage_from) | explicit predecessor (evidence + certainty) |
| derivation | semantic-index | semantic-entity | entities derived from a state |

### 7.2 Acyclicity and structure

- `causal_parents` chains are acyclic (a change cannot be its own ancestor).
- Append chains (`previous`) are acyclic.
- Content composition (tree→tree) is acyclic and bounded by depth limits.
- Cycles through any mixture of edges are **repository corruption** (fsck detects;
  STORAGE.md §8, INVARIANTS.md §4).
- The canonical object graph is a **DAG**; identity is by content, so the graph is
  also deduplicated by construction (identical subtrees share identities).

### 7.3 Trajectory semantics

A Trajectory is a sequence of work derived from a base state in pursuit of an Intent:

```text
                 ┌─ T1 → C1 → C2
                 │
State S0 ────────┼─ T2 → C3
                 │
                 └─ T3 → C4 → C5 → C6
```

- `base_state` anchors the derivation; every change in the trajectory derives
  (transitively) from it.
- The change sequence is the concatenation of `added_changes` along the append chain
  (§6.9). The latest version's ref is the trajectory's current identity; older versions
  remain reachable through `previous`.
- Outcome vocabulary: `completed`, `abandoned`, `superseded`, `rejected`,
  `inconclusive`, `interrupted`. Outcomes never delete work.
- Trajectories replace branches as the primary agent abstraction; branches, if present,
  are compatibility/naming constructs only.

### 7.4 Handoff semantics

A trajectory's `handoff` record is the structured continuation state (brief §35):
completed items, remaining items, open residuals, important evidence, recommended next
objects, next steps. It is machine-generated from repository state wherever possible
(AGENT_PROTOCOL.md §9), enabling resumption without rereading the originating
interaction.

---

## 8. Derived Status Algorithms

Derived statuses are computed properties. They are never stored as fields. They are
deterministic functions of the canonical graph plus (where noted) the query context
(e.g., a reference state).

### 8.1 Claim status

Inputs: claim C; the set of evidence E(C) explicitly linked from C; residuals R(C)
linked from C; supersession edges; a query state S (default: the head state of the
trajectory/case being queried).

Rule (evaluated in order; first match wins):

1. If ∃ C′ with `C′.supersedes = C` ⇒ `SUPERSEDED`.
2. If any E ∈ E(C) has `result.outcome ∈ {fail, mismatch}` OR any R ∈ R(C) has current
   disposition ∈ {open, acknowledged} ⇒ if any supporting evidence also exists
   ⇒ `PARTIALLY_SUPPORTED`, else `CONTRADICTED`.
3. If any E ∈ E(C) has `result.outcome ∈ {pass, match}` ⇒ `SUPPORTED` (scoped to the
   union of the scopes of those E).
4. If no evidence at all is linked ⇒ `UNVERIFIED`.
5. Staleness overlay: let stale(E) hold when E has an evaluated-state anchor (its own
   `evaluated_state`, else the resulting_state of the change that declared C) and there
   exists a change between that anchor and S touching C's subject (§8.2). If all of
   E(C) is stale (and no contradiction) ⇒ `STALE`.

Precedence (a stale result never masks a contradiction): `SUPERSEDED` >
`CONTRADICTED` > `STALE` > `PARTIALLY_SUPPORTED` > `SUPPORTED` > `UNVERIFIED`.

Output record: `{ status, supporting: [gid], contradicting: [gid], stale: [gid],
unverified_reason, basis: "<rule id>" }`.

Gemel stores claims and the evidence relevant to evaluating them; it never silently
converts assertions into facts.

### 8.2 Evidence freshness / MAY_REQUIRE_REFRESH

- A change "touches" a subject path p if any of its operations' `subject_path` equals p
  or is a segment-prefix of p (or vice versa), or shares a `subject_ref`/entity with
  the evidence's subjects.
- Evidence E with anchor state A is `MAY_REQUIRE_REFRESH` with respect to query state S
  if there is a change on a path from A to S that touches E's subjects.
- Without an anchor, freshness is `UNKNOWN` (reported as uncertainty, never asserted).
- Derived vocabulary: `FRESH`, `MAY_REQUIRE_REFRESH`, `UNKNOWN`.
- Gemel never declares evidence invalid automatically; it conservatively marks
  `MAY_REQUIRE_REFRESH` (brief §33). Precision improves as impact analysis improves
  (Phase 5).

### 8.3 Residual persistence and "Where does certainty end?"

- Current disposition = latest version's `disposition_event.disposition`; absent any
  event, `open`.
- Persistence = number of descendant changes of `affected_changes` (transitive closure
  over reverse `causal_parents` edges).
- "Where does certainty end?" = open/acknowledged residuals ordered by
  (severity, then first_observed_at, then gid). This is a core query
  (AGENT_PROTOCOL.md §7.6).
- A residual persists across reconciliation unless explicitly resolved, acknowledged,
  superseded, or proven irrelevant (disposition_event). Choosing one implementation
  never deletes a residual.

### 8.4 Verification scope and readiness

- A verification run's `result` is scoped to its `scope` record; there is no global
  `verified = true`.
- A global statement is valid only when explicitly scoped and policy-defined in
  `config` (the "required verification matrix": which predicate_kind × platform
  combinations must be satisfied before a claim can count as supported for readiness).
- Readiness (for `status`): computed per §AGENT_PROTOCOL 9.3 as a deterministic function
  of open blocking residuals, contradicted claims in scope, required-but-missing
  verification, and unverified claims. Output: `READY` | `READY_WITH_RESIDUALS` |
  `NOT_READY`, always with an explicit reason list.

### 8.5 Reconciliation semantics

A Reconciliation chooses an engineering direction over input trajectories without
erasing them:

- Adopted changes are applied over the input trajectories' common base to produce
  `resulting_state` (Phase 3 algorithm: textual apply + explicit dependency checks +
  explicit claims/evidence/residual bookkeeping; GIT_INTEROP and STORAGE document the
  mechanics).
- Semantic interactions are derived **conservatively** from: overlapping touched
  paths/entities, explicit `dependencies` edges, claim/residual relationships, and
  verification gaps. Certainty is `observed` (direct evidence), `possible` (structural
  overlap), or `unknown` (insufficient evidence). Gemel exposes uncertainty rather than
  inventing certainty.
- Rejected changes, unresolved residuals, invalidated claims, and required verification
  are recorded, never dropped.
- The result is a new `change` (with multiple `causal_parents`) and an updated head
  state.

### 8.6 Verification substrate boundary (FRF)

Gemel uses the Forensic Residual Framework as its verification substrate but does not
merge FRF into Gemel (brief §39). Gemel owns version-control semantics, Changes,
Trajectories, repository State, provenance, reconciliation, agent query, Git
projection, and distributed synchronization. FRF owns courts, authority definitions,
fixtures, comparators, normalizers, evidence protocol, residual analysis,
reproducibility semantics, and verification receipts. Gemel references FRF artifacts
by immutable identity (evidence kind `court_receipt`, tool records, fixture refs) and
never executes reproduction metadata without explicit policy (default
`never_auto_execute`).

### 8.7 Embeddings are never canonical

Embeddings may eventually aid discovery (brief §37). They are **derived indexes**:
rebuildable, model-versioned, optional, replaceable, and never required for repository
integrity. Exact structured relationships (typed edges, identities, derived statuses)
always take precedence; embeddings never define canonical identity or truth.

---

## 9. Object Lifetime and Retention Mapping

- All canonical objects are immutable once published. "Updating" a trajectory, case,
  residual, checkpoint, or config publishes a new version chained via `previous`.
- A ref (name) keeps its target (and transitively everything the target references)
  alive for GC.
- Retention tiers (policy in `config.retention`; mechanics in STORAGE.md §7):
  - **Tier 0 — Canonical**: states, changes, intents, claims, evidence identities,
    residuals, trajectories, reconciliations, releases, producers, agent runs,
    environments, context manifests, checkpoints, mappings, configs, operations,
    episodes, and the trees/blobs they reference. Default: retain forever.
  - **Tier 1 — Reproducibility**: commands, environment manifests, fixtures, tool
    outputs, oracle inputs, verification logs (typically blobs referenced by evidence).
  - **Tier 2 — Developmental provenance**: conversation references, agent summaries,
    discarded implementations, plans, decision material.
  - **Tier 3 — Deep forensic trace**: syscalls, huge runtime traces, full tool
    transcripts, token-level artifacts, debug captures.
- Pruning of Tier 1–3 blobs that remain referenced by canonical objects produces a
  **tombstone** (explicit marker + optional remote location) so canonical identity is
  never silently broken (STORAGE.md §7.6).
- Preserve **knowledge**, not noise: canonical metadata, compact failure summaries, and
  reproducibility artifacts are retained; large transient traces are pruned by policy.

---

## 10. Compatibility and Evolution Rules

### 10.1 Golden vectors as anchors

`golden/` pins canonical bytes and identities for representative objects (§12).
Any committed change that alters a pinned digest or byte string is a breaking protocol
change and requires the process of §10.4.

### 10.2 Schema evolution

- **Additive within a schemever**: use extension tags (0x80..=0xEF) on families that
  permit extensions. Old readers retain losslessly; no bump.
- **Semantic change**: bump `schemever`. The bump is documented with: the delta, the
  rationale, the downgrade rule (what a schemever-2 reader does with schemever-1
  objects — Phase 0 default: accept), and the upgrade rule (what a schemever-1 reader
  does with schemever-2 objects — Phase 0 default: reject).
- **Primitive grammar change**: bump `encver`. All families may be affected; the
  process is the same as §10.4.

### 10.3 JSON interchange mapping

The machine query surface uses a deterministic JSON projection of objects
(AGENT_PROTOCOL.md §3.1): `{"family": "...", "schemever": 1, "body": [{"tag": 1,
"name": "summary", "value": "..."}, ...]}`. Names are annotations; tags are
authoritative. Value encoding: UINT/SINT → decimal strings (exact, no precision loss);
BOOL → JSON boolean; BYTES → lowercase hex string; STRING → JSON string; GID →
textual identity; RECORD → array of field objects; ARRAY → JSON array. Optional absent
fields are omitted; `null` is never emitted for them.

### 10.4 Regeneration policy

Golden vectors are regenerated only as part of a deliberate protocol change (encver or
schemever bump), reviewed, and committed with the change that required them. Ad-hoc
regeneration is prohibited; `golden-gen` refuses to run when the tree is dirty unless
`--force` is passed and the operator confirms the protocol intent.

---

## 11. Golden Fixture Specification

### 11.1 Layout

```text
golden/
├── README.md                 ; provenance & regeneration policy
├── manifest.json             ; machine-readable vector registry
└── vectors/
    ├── <name>.gce.hex        ; canonical envelope bytes, lowercase hex
    └── <name>.id             ; textual object identity (§3.1)
```

### 11.2 manifest.json

```json
{
  "schema": "gemel.golden.v1",
  "generator": "gemel golden-gen",
  "generated_at": "<unix seconds; informative metadata only; never affects vector bytes>",
  "encver": 1,
  "vectors": [
    {
      "name": "blob-hello",
      "family": "blob",
      "schemever": 1,
      "bytes": "vectors/blob-hello.gce.hex",
      "id": "vectors/blob-hello.id",
      "description": "blob containing the bytes 0x68 0x69",
      "constructed_from": { ... }
    }
  ]
}
```

`generated_at` is informative metadata only (integer unix seconds; a sentinel is
permitted); it does not affect vector bytes or identities.

### 11.3 Coverage

One vector per family (22), plus: empty blob, empty tree, executable/symlink tree
entries, chained objects (trajectory, case, residual, checkpoint, config with
`previous`), a change with claims/evidence/residuals (the acceptance-demo shape), a
multi-parent change (reconciliation result), an agentrun with context manifest, an
oracle evidence with reproduction record, and at least one vector exercising an
extension field with lossless retention. Cross-references between vectors are real
(referenced gids are the pinned identities of other vectors).

### 11.4 Executable verification

The `gemel` crate (`src/`, binary `golden-gen`) implements:

- encode(decode(bytes)) == bytes for every vector (byte-exact, including extension
  retention);
- decode(encode(object)) == object for every constructed fixture;
- computed identity equals the pinned `.id`;
- family/schemever in the envelope match the manifest;
- cross-references resolve to the pinned identities of the referenced vectors;
- the negative fixture catalog (THREAT_MODEL.md §4) is exercised by unit tests and
  must reject.

### 11.5 Determinism statement

The same logical object must produce identical bytes and identity across machines,
operating systems, and implementations. The only inputs to encoding are the schema and
the field values; no environment state, no wall clock (unless a timestamp field is
explicitly provided as a value), no locale, no parallelism-dependent ordering.

---

## 12. Cross-Reference to Other Documents

- Storage mechanics: STORAGE.md (object store, refs, indexes, GC, fsck).
- Invariants: INVARIANTS.md (every invariant here is enumerated there with enforcement).
- Git interchange: GIT_INTEROP.md (mapping family usage).
- Query surface and JSON: AGENT_PROTOCOL.md.
- Security and limits: THREAT_MODEL.md.
