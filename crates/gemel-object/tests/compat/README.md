# Cross-version compatibility testing

Compatibility anchors for the canonical encoding live in two places:

1. **`golden/`** — the executable golden vectors (OBJECT_MODEL.md §12). They pin the
   canonical bytes and identities of `encver=1` objects and are the primary
   cross-version anchor: any implementation of the protocol must reproduce them
   byte-for-byte.

2. **This directory** — reserved for **historical fixture archives**. When a future
   protocol change (an `encver` or `schemever` bump) is made, a snapshot of the
   previous golden set is archived here (e.g., `encver-1/`), and a compatibility test
   is added that reads objects written under the old version with the new reader
   according to the documented downgrade rules (OBJECT_MODEL.md §10.2).

Rules:

- Every archived snapshot is immutable and carries its own manifest in the
  `gemel.golden.v1` format.
- A reader must pass the compatibility test for every archived version it claims to
  support.
- Regeneration of historical snapshots is forbidden; they are historical records.
