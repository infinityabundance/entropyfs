# On-disk format v1

Phase 0 deliverable #5. This document is normative for
`src/format/*`. All integers are little-endian. All lengths and offsets are
checked before use; all structures carry magic/tag, version, length, and
integrity. Never cast disk bytes to Rust structs.

## 1. Backing layout

```text
store/
├── superblock          # two 512-byte slots: A at offset 0, B at offset 4096
├── segments/
│   ├── 0000000000000000.seg
│   ├── 0000000000000001.seg
│   └── ...
└── lock                # flock(2) exclusive; prevents concurrent mounts
```

`segments/` entries are 16-digit zero-padded decimal segment sequence
numbers. `lock` is created at mkfs and held for the mount lifetime.

## 2. Superblock slot (512 bytes; slots A and B)

| Field | Type | Notes |
|-------|------|-------|
| magic | `[u8;8]` | `ENTR0FS\0` |
| struct_version | u8 | 1 |
| format_major | u16 | 1 |
| format_minor | u16 | 0 |
| compat_features | u64 | must be a subset of supported |
| ro_compat_features | u64 | unknown ⇒ refuse rw, allow ro |
| incompat_features | u64 | unknown ⇒ refuse mount |
| uuid | `[u8;16]` | filesystem UUID |
| generation | u64 | commit generation N |
| root_object_id | `[u8;32]` | BLAKE3 of root object payload |
| segment_seq | u64 | current segment sequence |
| created_unix_ns | u64 | informational |
| flags | u32 | reserved |
| extension_len | u16 | length of extension bytes |
| extension | `[u8;248]` | reserved extension area |
| checksum | u32 | CRC32C over bytes [0..508) |

Slot size 512; the checksum covers the first 508 bytes. Slots are written
whole; a torn write fails CRC validation.

Commit alternates slots: slot = `generation & 1` (A even, B odd) — the
"inactive" slot of ADR-0008 is the one whose parity differs from the current
generation.

## 3. Segment record envelope (fixed header 58 bytes + payload)

| Field | Type | Notes |
|-------|------|-------|
| record_tag | u8 | object kind (below) |
| format_version | u8 | 1 |
| flags | u16 | bit0: materialized_len valid |
| header_len | u16 | 58 (v1) — forward-compat |
| stored_len | u32 | payload bytes present |
| materialized_len | u64 | logical length (valid if bit0) |
| content_id | `[u8;32]` | BLAKE3 of payload (logical content hash for data) |
| header_crc | u32 | CRC32C over the 50 bytes before this field |
| payload_crc | u32 | CRC32C over payload |
| payload | `[u8; stored_len]` | |

Record tags (v1):

| Tag | Kind |
|-----|------|
| 0x01 | DATA — arbitrary payload referenced by descriptors (raw bytes, rANS stream, residual stream) |
| 0x02 | MODEL — encoded rANS model |
| 0x03 | INODE — encoded inode |
| 0x04 | DIR_LEAF / BTREE node — encoded persistent B-tree node |
| 0x05 | ROOT — encoded filesystem root |
| 0x06 | XATTR — xattr value payload |
| 0x07 | MUTATION_LOG — one acknowledged namespace/writeback mutation (Phase-10D metadata writeback epoch; the recoverable dirty state between checkpoints) |
| 0x7F | PAD — zero padding; never referenced |

**Object identity and record tags (Phase-8C):** a record's `content_id` is
BLAKE3 of the PAYLOAD ALONE. The record tag (DATA vs MODEL vs BTREE) is
envelope metadata, NOT part of identity: two records with equal payloads
share one content id regardless of tag, and the store stages at most one
physical record per content id per transaction (an id already pending or
committed costs zero new records). The materialized-length flag is likewise
envelope metadata; identical payloads always have identical materialized
content, so a skipped re-stage loses nothing. A descriptor referencing an
object by id never depends on the record's tag — the descriptor knows the
object's role.

Segment files: optional 4 KiB header (magic `ESEG` + segment_seq + record
count at seal time), then records back-to-back, then a sealed trailer
(record_count u64, trailer CRC) when the segment is sealed by rollover or
GC. Recovery tolerates a torn trailer (crash mid-segment): records with
valid envelopes before the torn point are retained; the tail is ignored.

## 4. Persistent B-tree node

Used for: inode index (`u64 ino → inode`), directory (`name bytes →
(ino u64, d_type u8)`), extent tree (`u64 offset → extent descriptor`),
model index (`[u8;32] id → model`), snapshot tree (`name → root id`),
xattr tree (`name → value`).

Node payload v1:

| Field | Type | Notes |
|-------|------|-------|
| node_kind | u8 | 0x01 leaf, 0x02 internal |
| order | u16 | fanout (default 64) |
| entry_count | u16 | |
| entries | entry[] | leaf: (key, value); internal: (key, child_id `[u8;32]`) |

Entry keys are length-prefixed byte strings: `key_len u16 + key bytes`.
Leaf values are length-prefixed: `val_len u32 + val bytes`. Internal
child_ids are fixed 32 bytes. A node with `entry_count == 0` is invalid
except the empty root. Keys in a node are strictly increasing; fsck
verifies.

Node content ID = BLAKE3(payload). COW: mutation rewrites the path from
root to leaf, producing new nodes; unchanged nodes are shared.

## 5. Filesystem root object (payload of tag 0x05)

| Field | Type |
|-------|------|
| format_major | u16 |
| format_minor | u16 |
| inode_index_root | `[u8;32]` |
| root_dir_ino | u64 |
| snapshot_tree_root | `[u8;32]` |
| model_index_root | `[u8;32]` |
| segment_seq | u64 |
| index_epoch | u64 |
| uuid | `[u8;16]` |
| generation | u64 |
| log_seq | u64 | highest epoch log sequence consumed by this root (Phase-10D); 0 for pre-epoch roots; the trailing field is absent in pre-epoch payloads and decodes as 0 |

## 6. Inode object (payload of tag 0x03)

| Field | Type |
|-------|------|
| mode | u32 (st_mode) |
| uid | u32 |
| gid | u32 |
| size | u64 |
| atime/ctime/mtime/crtime | (sec u64, nsec u32) × 4 |
| nlink | u32 |
| rdev | u32 |
| flags | u32 |
| xattr_root | `[u8;32]` (all-zero = none) |
| data_kind | u8: 0x01 dir, 0x02 file, 0x03 symlink, 0x04 device |
| data | per kind: dir → dir_root `[u8;32]`; file → extent_root `[u8;32]`; symlink → target bytes (len-prefixed); device → (none) |

## 7. Extent descriptor (inline value in extent-tree leaves)

| Field | Type |
|-------|------|
| tag | u8 (representation tag below) |
| len | u32 (logical length of this extent) |
| payload | per tag |

Representation tags and payloads:

| Tag | Name | Payload |
|-----|------|---------|
| 0x01 | ZERO | — |
| 0x02 | FILL | value u8 |
| 0x03 | RAW | obj `[u8;32]` (payload is the literal bytes) |
| 0x04 | RANS | model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8 (0=single,1=interleaved2) |
| 0x05 | EXACT_REF | target `[u8;32]`, off u32 |
| 0x06 | BASE_RESIDUAL | base `[u8;32]`, base_len u32, residual (below) |
| 0x07 | SPARSE | k u32, rank u128, literals (k bytes) |
| 0x08 | PALETTE | m u8 (≤16), palette (m bytes), counts (m×u32), rank u128 |
| 0x09 | PERIODIC | period u32, pattern (period bytes), count u32, tail_len u32, tail (tail_len bytes) |
| 0x0A | ENTROPY_REF | universe_id u8, seed `[u8;16]`, coordinate u64, transform u8, residual (below) |
| 0x0B | INLINE | data (len bytes, len ≤ 4096) |
| 0x0C | PERMUTATION | rank u128, alphabet (len bytes; distinct, strictly increasing, len ≤ 34) |
| 0x0D | SEQUENCE_RANS | model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8, seq_len u32, lit_len u32, off_len u32, cmds u32, lit_out u32 |
| 0x0E | SPARSE_BLOCK64 | model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8, pc_len u32, rank_len u32, lit_len u32, words u32, nonzero u32, lit_out u32 |
| 0x0F | SEQUENCE_DICT | dictionary `[u8;32]`, dictionary_len u32, model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8, seq_len u32, lit_len u32, off_len u32, src_len u32, cmds u32, lit_out u32 |
| 0x10 | SEQUENCE_SHARED_DICT | dictionary `[u8;32]` (ZERO = absent), dictionary_len u32, shared `[u8;32]`, shared_len u32, model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8, seq_len u32, lit_len u32, off_len u32, src_len u32, cmds u32, lit_out u32 |
| 0x11 | SEQUENCE_DEEP | model `[u8;32]`, enc_obj `[u8;32]`, scale_bits u8, codec u8, seq_len u32, lit_len u32, off_len u32, len_len u32, cmds u32, lit_out u32 |

SEQUENCE_RANS (0x0D) is the local-match + entropy floor: an LZ77-style
hash-chain matcher turns the extent into three byte streams — *commands*,
*literals*, *offsets* — each of which is either rANS-coded (with its own
model inside the model object) or stored raw when rANS cannot beat raw.

Command encoding (one byte per command):

| Command byte | Meaning |
|--------------|---------|
| 0x00..=0x7F | literal run of `b + 1` (1..=128) bytes, taken from the literal stream |
| 0x80..=0xFF | copy of `b - 0x80 + 4` (4..=131) bytes at distance `d` (u16 LE, next 2 bytes of the offset stream), relative to the current output position |

Copy semantics are byte-progressive (overlap allowed): `out[p+i] =
out[p+i-d]` for `i in 0..len`, the standard LZ77 contract. The only
constraint is `d <= p`. A long match is emitted as repeated copies at one
distance. `seq_len`/`lit_len`/`off_len` are the *encoded* stream lengths
and must sum to the enc object length; `cmds` is the decoded command count
(≤ len) and `lit_out` the decoded literal byte count (≤ len). The enc
object is `[commands][literals][offsets]` concatenated. The model object
is three slots (below).

SPARSE_BLOCK64 (0x0E) is blockwise-64 enumerative sparse coding: the
chunk's nonzero-byte positions are coded as 64-bit subblocks. For each
64-bit word: popcount `k` (one byte in the popcount stream) and the
subset rank among `C(64, k)` (u64 LE in the rank stream — `C(64, 32)`
fits a u64), plus the literal values (one byte per marked position in the
literal stream). `words = ceil(len / 8)`; `nonzero` = number of words with
`k > 0`; the rank stream decodes to `nonzero × 8` bytes; `lit_out` = total
marked bytes. This removes the plain-SPARSE `u128` combination-rank cliff
(`10 ≤ k ≤ n−10` at 64 KiB) while staying bounded and popcount-friendly.
The three streams share the SEQUENCE_RANS codec.

SEQUENCE_DICT (0x0F, Phase-9B) is cross-chunk dictionary match coding: the
same command semantics as SEQUENCE_RANS plus a fourth *copy-source*
stream — one byte per copy command saying whether the command's u16 value
is a LOCAL backward distance into the already-materialized output (`0x00`)
or a DICT absolute offset into the ≤64 KiB dictionary chunk (`0x01`). The
model object holds FOUR slots; the enc object is
`[commands][literals][offsets][sources]` (`seq_len + lit_len + off_len +
src_len` == enc object length; `src_len` decodes to one byte per copy
command). The dictionary is a content-addressed chunk reference (the
previous same-file chunk, v1). A LOCAL copy is byte-progressive
(`out[p+i] = out[p+i-d]`); a DICT copy reads a contiguous range
(`out[p..p+len] = dict[off..off+len]`), so a DICT match longer than 131
bytes is split into continuation commands whose u16 values ADVANCE the
dictionary offset (`off, off+131, off+262, …`). `dictionary_len` bounds
DICT offsets (u16 → ≤ 65536) and the reference depth is accounted like a
base chain: the dictionary's own chain depth plus 1 must not exceed
`max_reference_depth`, so cross-chunk dictionary chains can never defeat
bounded random access.

SEQUENCE_SHARED_DICT (0x10, Phase-9C) is shared amortized dictionary match
coding: the SEQUENCE_DICT command semantics with a third copy-source,
`0x02` = SHARED (absolute offset into a *shared cross-file dictionary*
chunk). The optional `dictionary` field is the previous same-file chunk
(ZERO id + zero length = absent); `shared` is required (≤ 64 KiB). The
shared dictionary is a content-addressed chunk chosen by the background
optimizer (`shared_dict_pass`) to amortize structure common to a file
family/directory: the anchor is an existing terminal chunk, so its own
persisted state is accounted where it is materialized, and the group pays
only the per-extent reference + read cost (enforced by the strict-cheaper
commit gate). Copy semantics match SEQUENCE_DICT exactly (LOCAL
byte-progressive; DICT/SHARED contiguous with advancing continuation
offsets). Reference depth = max(file-dict depth, shared depth) + 1, capped
by `max_reference_depth`; v1 anchors are terminal (depth 0), so rewritten
extents carry depth ≤ 1.

SEQUENCE_DEEP (0x11, Phase-9E) is the deep-match family: the background
matcher (hash chains to depth 256, lazy parsing with a minimum-gain
threshold, recent-distance priority) feeding a richer command language
with repcodes and extended length codes. Command byte:

| Command byte | Meaning |
|--------------|---------|
| 0x00..=0x7F | literal run of `b + 1` (1..=128) bytes from the literal stream |
| 0x80..=0xBF | copy of `4 + (b - 0x80)` (4..=67) at a NEW u16 distance (byte-progressive; rep1 = rep0, rep0 = d) |
| 0xC0..=0xDF | copy of `4 + (b - 0xC0)` (4..=35) at the REP0 distance (no offset symbol) |
| 0xE0..=0xEF | copy of `4 + (b - 0xE0)` (4..=19) at the REP1 distance (no offset symbol) |
| 0xF0 | extended copy: u16 extra in the lengths stream, length `68 + extra` (clamped to the chunk), then a NEW u16 distance (byte-progressive; reps update) |
| 0xF1 | extended literal run: u16 extra in the lengths stream, run `129 + extra` |
| 0xF2..=0xFF | reserved (malformed) |

The rep register is two slots (REP0/REP1), initialized to 0 (a REP0/REP1
copy with rep 0 is malformed), updated only by NEW-distance commands. The
enc object is `[commands][literals][offsets][lengths]`; the offsets stream
carries one u16 per COPY/XCOPY and the lengths stream one u16 per
XCOPY/XLIT (both derived by a command walk at decode, so variable
consumption is deterministic). The model object holds four slots. The
family is evaluated only by the background optimizer (the foreground
keeps the fast greedy `SequenceRans` matcher and its small CPU budget);
it is terminal (reference depth 0) and shares the `SEQUENCE_RANS` rANS/raw
codec.

Residual (for BASE_RESIDUAL / ENTROPY_REF), kinds:

| Kind | Payload |
|------|---------|
| 0x01 XOR_SPARSE | edit_count u32, edits: (pos u32, val u8) × count — byte X at `pos` differs from base by `val` (X = base[pos] XOR val) |
| 0x02 RANGE_REPLACE | change_count u32, changes: (start u32, end u32) × count, then literal bytes concatenated in order |
| 0x03 RANS_CODED | enc_obj `[u8;32]`, model `[u8;32]`, scale_bits u8, codec u8, decoded_len u32 (decoded residual applied as XOR_SPARSE after decode? No — decoded residual is a byte stream of length len, applied by XOR with base) |
| 0x04 BASE_SEQUENCE | enc_obj `[u8;32]`, model `[u8;32]`, scale_bits u8, codec u8, seq_len u32, lit_len u32, off_len u32, cmds u32, lit_out u32 — shift-aware copy/literal delta (below) |

For 0x03 the decoded byte stream is XORed against the base (all positions),
which is equivalent to XOR_SPARSE with dense edits; the rANS-coded residual
is the *compressed XOR difference*.

BASE_SEQUENCE (0x04) is the shift-aware delta: the output `X` is built by
walking a command stream, so inserted/deleted regions (which shift
positions and break positional XOR residuals) cost only their own bytes.
Command stream (one byte per command):

| Command byte | Meaning |
|--------------|---------|
| 0x00..=0x7F | literal run of `b + 1` (1..=128) bytes from the literal stream |
| 0x80..=0xFF | copy of `b - 0x80 + 4` (4..=131) bytes from the base at a u32 LE base offset (next 4 bytes of the offset stream) |

The three streams use the same codec as SEQUENCE_RANS (per-stream rANS
with a raw fallback; three-slot model object; `seq_len + lit_len +
off_len` == enc object length; `cmds` decoded command count ≤ len;
`lit_out` decoded literal bytes ≤ len). The base may be shorter or longer
than the target (the `base_len >= len` constraint of the positional
residuals does not apply).

## 8. rANS model object (payload of tag 0x02)

| Field | Type |
|-------|------|
| scale_bits | u8 |
| codec | u8 |
| sym_count | u16 (256 for byte rANS v1) |
| freqs | delta+RLE-encoded u16 frequencies (decoded length must equal sym_count) |
| model_crc | u32 (CRC32C of preceding bytes) |

Decode rebuilds `RansByteEncSymbol`/`RansByteDecSymbol` arrays through the
validated constructors and validates via `malformed::validate_freq_model`.

## 8a. SEQUENCE_RANS model object (payload of tag 0x02)

Three or four slots (SEQUENCE_RANS / BASE_SEQUENCE / SPARSE_BLOCK64 use
three: commands, literals, offsets; SEQUENCE_DICT uses four: commands,
literals, offsets, copy sources):

| Field | Type |
|-------|------|
| kind | u8 per slot: 0x00 = rANS model, 0x01 = raw stream, 0x02 = empty |
| len | u16 LE per slot: encoded model length for kind 0x00; must be 0 otherwise |
| bytes | the encoded model for kind 0x00 |

For kind 0x01 the raw stream bytes live in the enc object (decoded length
implied by the descriptor: `cmds`, `lit_out`, and `2 × copy-count`
respectively). For kind 0x02 the decoded length must be 0. A stream whose
histogram has ≤ 1 distinct symbol is stored raw (kind 0x01); an empty
stream is kind 0x02; otherwise rANS is used only when strictly smaller
than the raw stream.

## 9. Snapshot entry (value in snapshot tree)

| Field | Type |
|-------|------|
| root_id | `[u8;32]` |
| created_unix_ns | u64 |
| name | (the tree key) |

## 10. Versioning policy

Every structure carries its own version field; the superblock carries
format_major/minor and feature bits. Rules in `docs/format/compatibility.md`.
