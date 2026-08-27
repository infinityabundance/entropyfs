# Feature registry (format v1) — normative

Phase 12E.3: the single normative registry of every persistent feature bit
in format v1. The authoritative source of the bit positions is
`src/format/features.rs`; this document is the human contract. A change
to either without the other is a documentation bug (the registry test in
`compat_seal.rs` pins the gate behavior; the registry itself is reviewed
with every format change).

## 1. Sets and rules (normative)

| Set | Unknown bit ⇒ | Implemented since |
|-----|----------------|-------------------|
| `compat` | still open read-write; feature may simply not activate | v0.1.0 |
| `ro_compat` | refuse writable open; permit read-only open (Phase 12E.3) | v0.1.0 (gate honored since 12E.3) |
| `incompat` | refuse open entirely | v0.1.0 |

The format-major contract: `format_major` bump ⇒ incompatible (new tools
refuse old stores, old tools refuse new stores). `format_minor` bump ⇒
additive and backward-compatible when the feature bits allow. A semantic
break that cannot be expressed through feature bits requires a new format
major — never a silent in-place interpretation change.

## 2. Registry

`first writer` = the first release that can SET the bit on disk;
`first reader` = the first release that understands it; `retireable` =
whether a future encoder may stop emitting it (the decoder commitment
remains); `decoder commitment` = what a decoder must do when the bit is
set. Bits 1–15 were all *defined* in v0.1.0; only the ones below are ever
*written* by a v1 writer.

| # | Name | Set | First writer | First reader | Retireable | Persistent structures affected | Decoder commitment |
|---|------|-----|--------------|--------------|------------|--------------------------------|--------------------|
| 1 | `CHUNK_4K` | incompat | v0.1.0 | v0.1.0 | yes | chunk class (4 KiB) descriptors/extents | must decode 4 KiB chunk-class geometry |
| 2 | `CHUNK_16K` | incompat | v0.1.0 | v0.1.0 | yes | chunk class (16 KiB) descriptors/extents | must decode 16 KiB chunk-class geometry |
| 3 | `CHUNK_256K` | incompat | v0.1.0 | v0.1.0 | yes | chunk class (256 KiB) descriptors/extents | must decode 256 KiB chunk-class geometry |
| 4 | `ENTROPY_REF` | incompat | v0.1.0 | v0.1.0 | yes | ENTROPY_REF descriptors (content-addressed references) | must decode ENTROPY_REF descriptors |
| 5 | `PALETTE` | incompat | v0.1.0 | v0.1.0 | yes | PALETTE descriptors + symbol-model objects | must decode PALETTE descriptors |
| 6 | `PERMUTATION` | incompat | v0.1.0 | v0.1.0 | yes | PERMUTATION descriptors (reserved; not emitted in v1) | must decode PERMUTATION descriptors if present |
| 7 | `ENCRYPTED` | ro_compat | never (unimplemented, ADR-0015) | never | n/a | record payloads (AEAD) | treat as unknown → read-only fallback; never misread |
| 8 | `EXTENT_DELTA_INDEX` | compat | never (reserved; derived-index marker) | v0.1.0 | n/a | derived extent-delta index (disposable) | ignorable (feature simply inactive) |
| 9 | `OPTIMIZER_REWRITE` | compat | never (reserved; history marker) | v0.1.0 | n/a | optimizer rewrite history markers | ignorable (feature simply inactive) |
| 10 | `SEQUENCE_RANS` | incompat | v0.2.0 | v0.2.0 | yes | SEQUENCE_RANS descriptors + LZ77/rANS stream objects | must decode the three-stream codec |
| 11 | `SPARSE_BLOCK64` | incompat | v0.2.0 | v0.2.0 | yes | SPARSE_BLOCK64 descriptors + enumerative stream objects | must decode blockwise-64 rank coding |
| 12 | `SEQUENCE_DICT` | incompat | v0.3.0 | v0.3.0 | yes | SEQUENCE_DICT descriptors + dictionary stream objects | must decode the dictionary codec |
| 13 | `SEQUENCE_SHARED_DICT` | incompat | v0.4.0 | v0.4.0 | yes | SEQUENCE_SHARED_DICT descriptors + shared-dictionary objects | must decode the shared-dictionary codec |
| 14 | `SEQUENCE_DEEP` | incompat | v0.5.0 | v0.5.0 | yes | SEQUENCE_DEEP descriptors + deep-match stream objects | must decode the deep-match codec |
| 15 | `MUTATION_LOG` | incompat | v0.6.0 | v0.6.0 | no | MUTATION_LOG records (tag 0x07) + `root.log_seq` | must replay the writeback log (refuse the store otherwise) |

`CHUNK_64K` is the baseline chunk class and needs no bit (v1 always
supports it).

## 3. Retirement policy (normative)

- A **decoder** must remain able to READ every representation family
  whose incompat bit it ever granted — a "known retired representation"
  stays decodable for the life of format v1.
- A **new encoder release may stop EMITTING** an old representation (its
  bit is then simply never set on new stores). Stores that carry the bit
  remain mountable.
- Bits 1–6 and 10–14 are retireable under that rule. Bit 15
  (`MUTATION_LOG`) is NOT retireable while the epoch writeback design is
  the acknowledged-write path — every new writer keeps emitting it.
- Reserved bits (7, 8, 9) are never written; removing them would be a
  registry change requiring a format-minor consideration, never a silent
  repurpose.

## 4. Range-reservation policy

No ranges are reserved for speculative application domains (video, game,
AI, etc.). Future bits are allocated only by generic format-extension
policy, in bit order, with a registry entry written at the same commit as
the code that sets them.
