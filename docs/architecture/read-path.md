# Read path

## 1. Request flow

```text
FUSE read(ino, offset, size)
  → fuse::file::read
  → inode.extent_tree.covering(offset..offset+size)
  → for each extent E in range order:
        chunk = materialize_extent(E)          (cacheable)
        splice chunk[local_range] into reply
  → reply (cached by kernel page cache)
```

## 2. Extent lookup

The extent tree is a persistent B-tree keyed by logical offset
(`src/store/extent_tree.rs`). Lookup is O(log n) with COW-shared nodes.
Extents never overlap and are ordered; fsck verifies this.

## 3. materialize_extent — the bounded interpreter

`core::materialize::materialize(desc, resolver, limits, out)`:

1. Validate `desc.output_len` against `Limits::max_chunk_size` **before**
   any allocation; allocate exactly `output_len`.
2. Dispatch on the representation tag:

| Tag | Materialization |
|-----|-----------------|
| ZERO | `memset` (bounded by `output_len`) |
| FILL | `memset(value)` |
| INLINE/RAW | copy bytes (RAW object fetched via resolver) |
| RANS | validate persisted model (`malformed::validate_freq_model`), decode with `ryg-rans-rs`; decoded length must equal `output_len` |
| EXACT_REF | fetch target chunk (depth+1 ≤ 4), copy subrange `[off, off+len)` |
| BASE_RESIDUAL | fetch base (depth+1 ≤ 4), apply residual transform (sparse edit set / range replaces), then apply any second-stage rANS decode of the residual |
| SPARSE | unrank position subset from `rank` (checked u128), fill literals at positions, zeros elsewhere |
| PALETTE | unrank multiset coordinate, emit symbols by counts |
| PERIODIC | repeat pattern `count` times, append tail |
| ENTROPY_REF | universe materializer for `(seed, coordinate, range)`; apply transform; XOR residual |

3. Every step decrements a deterministic operation budget
   (`Limits::max_decode_work`); exceeding it returns
   `MaterializeError::BudgetExceeded`, never a partial result.
4. Reference cycles are impossible by construction (depth cap + content
   IDs point only to committed immutable objects; fsck checks for cycles
   anyway).

## 4. Resolver

```text
trait ObjectResolver {
    fn fetch(&self, id: &ChunkId, range: Range<u64>) -> Result<Bytes, StoreError>;
}
```

`store` implements it via the content index → segment read. Fetches are
bounded by the chunk class.

## 5. Caching

`cache::materialized` is a bounded LRU keyed by **logical content ID** of
the extent; a hit is trusted only when the extent's recorded content ID
matches (ADR-0011/0014). Eviction never affects correctness. `cache::model`
holds decoded rANS symbol tables keyed by model ID.

## 6. SEEK_DATA / SEEK_HOLE

Implemented from the extent tree: holes are gaps between extents and beyond
EOF (files are sparse by construction — an unallocated range materializes
as ZERO). No special representation needed.

## 7. copy_file_range

When both ranges are in the same file (or cross-file with identical
content), the destination extents become `EXACT_REF` aliases to the source
chunk IDs — a server-side clone at metadata cost. Fallback: read+write
through the normal path (identical logical result).

## 8. Amplification accounting

Each read records `physical_bytes_read` and `dependent_reads`
(ADR-0010), reported in `status` and `benchmark` — this is the H5
measurement surface ("regeneration is cheaper than fetching expanded
bytes", §45).
