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
| 0x7F | PAD — zero padding; never referenced |

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

Residual (for BASE_RESIDUAL / ENTROPY_REF), kinds:

| Kind | Payload |
|------|---------|
| 0x01 XOR_SPARSE | edit_count u32, edits: (pos u32, val u8) × count — byte X at `pos` differs from base by `val` (X = base[pos] XOR val) |
| 0x02 RANGE_REPLACE | change_count u32, changes: (start u32, end u32) × count, then literal bytes concatenated in order |
| 0x03 RANS_CODED | enc_obj `[u8;32]`, model `[u8;32]`, scale_bits u8, codec u8, decoded_len u32 (decoded residual applied as XOR_SPARSE after decode? No — decoded residual is a byte stream of length len, applied by XOR with base) |

For 0x03 the decoded byte stream is XORed against the base (all positions),
which is equivalent to XOR_SPARSE with dense edits; the rANS-coded residual
is the *compressed XOR difference*.

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

## 9. Snapshot entry (value in snapshot tree)

| Field | Type |
|-------|------|
| root_id | `[u8;32]` |
| created_unix_ns | u64 |
| name | (the tree key) |

## 10. Versioning policy

Every structure carries its own version field; the superblock carries
format_major/minor and feature bits. Rules in `docs/format/compatibility.md`.
