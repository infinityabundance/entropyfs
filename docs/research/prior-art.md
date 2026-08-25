# Prior-art comparison: filesystems and storage designs

Phase 0 deliverable. Lessons from mature designs, explicitly recorded as
adopted / rejected / observed with rationale. This is not a cargo-cult
exercise; every adoption maps to an EntropyFS mechanism.

## 1. Log-structured / append-only

| System | Lesson | Verdict |
|--------|--------|---------|
| LFS (Ousterhout) | Append-only writes turn random writes into sequential; garbage collection is the tax | **Adopted**: append-only segments (ADR-0008); GC with live-ratio compaction (ADR-0009) |
| ZFS (copy-on-write, txg) | COW + checksums + pooled storage; never overwrite live data | **Adopted**: COW object graph (ADR-0007); dual-slot superblock with generation |
| Btrfs (COW, subvolumes, checksums) | Tree of trees; snapshots cheap; but metadata amplification and fsck complexity are real | **Adopted** (conceptually): immutable node trees, cheap snapshots. **Rejected**: btrfs's online-mutation metadata model; EntropyFS nodes are immutable objects |
| bcachefs (BTrees, COW, snapshots, layered transactions) | Very capable general COW design; huge implementation surface | **Observed**: its transaction/lock ordering and snapshot semantics are reference material for our commit coordinator; we do not adopt its scope |
| XFS (extent-based, delayed allocation, journal) | Extent trees + ordered journaling are battle-tested | **Adopted** (conceptually): extent-based file mapping (`offset → extent`); ordered commit protocol inspired by ordered-journal durability |
| ext4 (extents, journal, delayed alloc) | Simplicity of the ext4 recovery model (replay journal, mount-time check) | **Adopted** (conceptually): recovery is deterministic and replayable; **Rejected**: in-place metadata updates (we are append-only) |

## 2. Content addressing / dedup

| System | Lesson | Verdict |
|--------|--------|---------|
| Git (immutable content-addressed objects) | Address by hash; identical content collapses; GC by reachability | **Adopted**: content-addressed immutable objects (ADR-0007); reachability GC |
| VDO / dm-dedup | Hash-index dedup with verification; dedup metadata costs RAM | **Adopted** (conceptually): 256-bit content IDs (BLAKE3), verify-before-alias (ADR-0011). **Rejected**: kernel-block-layer approach (we are filesystem-level) |
| ZFS dedup (ditto tables) | Dedup tables are memory-heavy; verify before reuse | **Observed**: keep the index swappable/derived (ours is derived and disposable) |
| composefs | Read-only image mounts backed by content-addressed file objects (EROFS + CAS); OCI-style layering | **Adopted** (conceptually): whole-file and chunk objects referenced by hash; our `EXACT_REF` is the writable generalization |

## 3. Compression-oriented systems

| System | Lesson | Verdict |
|--------|--------|---------|
| Btrfs zstd/zlib/lzo compression | Per-extent compression with fixed block sizes; decompression on read; write amplification from recompression | **Adopted** (conceptually): per-extent coded representations; bounded read units. **Extended**: EntropyFS is not limited to one compressor — representations include references, rank/config, residuals, universes |
| zram/zswap | Compressed RAM swap; incompressible data handled by not compressing | **Adopted**: the RAW escape hatch is first-class (ADR-0005); RAM mode (§46) mirrors zram ideas |
| EROFS | Read-only, fixed-size compression units with pcluster design; micro-layout control | **Observed**: excellent cold-read design; informs our read-path layout and prefetch, but EntropyFS is writable |
| SquashFS | Whole-file block compression; gzip/xz/zstd; read-only | **Observed**: baseline for read-only image comparisons only |

## 4. Layering / userspace

| System | Lesson | Verdict |
|--------|--------|---------|
| OverlayFS | Stacked layers with copy-up; whiteouts | **Observed**: useful mental model for snapshot layering; not adopted (EntropyFS snapshots are pinned roots, not overlay layers) |
| FUSE filesystems (sshfs, fuse-overlayfs, passthrough) | Userspace crash isolation vs. performance ceiling | **Adopted**: FUSE frontend (ADR-0002); passthrough only as later optional optimization |
| Wine/DAX-style direct maps | mmap performance requires page-level integration | **Observed**: Phase 6 concern (DAX is available in this kernel; not a Phase-1 dependency) |

## 5. Persistent functional data structures

| System | Lesson | Verdict |
|--------|--------|---------|
| HAMT / persistent B-trees (functional maps in Clojure, Haskell, Rust `rpds`) | COW trees give O(log n) structural sharing; node immutability enables lock-free reads | **Adopted**: persistent B-tree indexes in `src/store/index.rs` for inode/directory/extent trees |
| Journaling vs shadow-paging vs write-ahead logs | Each solves atomic multi-update differently | **Adopted**: shadow-paging (COW) + dual-superblock commit — the simplest protocol with the required crash invariant (ADR-0008) |

## 6. Explicit rejections (with reasons)

- **In-place-update metadata (ext4-style)**: incompatible with snapshots
  and with the "immutable objects" premise.
- **Journal + replay as the primary mechanism**: log replay is still used
  implicitly (append-only segments are replayable), but the authoritative
  commit is the superblock flip, not a journal tail.
- **Hash-index-only dedup without byte verification**: rejected; we verify
  (ADR-0011).
- **Optimistic capacity reporting (VDO advertised ratios)**: rejected;
  `statfs` is physical (ADR-0018).
- **Kernel module first**: rejected (ADR-0002).
- **Database-backed metadata**: rejected (ADR-0017).

## 7. What EntropyFS does that prior art does not

Prior art compresses bytes, dedups bytes, or represents sparse bytes.
EntropyFS additionally persists **mathematical configuration as storage**:
combinatorial rank/unrank coordinates, palette/multinomial ranks,
permutation ranks, periodic structures, entropy-universe references, and
base+residual configurations — each chosen by exact cost, each validated
byte-exact before commit, each accounted bit-for-bit
(`docs/theory/information-accounting.md`). The claim is narrow and testable:
for structured workloads, the irreducible persisted state is smaller than
the materialized byte representation; for unstructured workloads, EntropyFS
degrades gracefully toward RAW.
