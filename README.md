# EntropyFS

de Beer, R. (2026). *EntropyFS: Entropy-Native Configurational Storage as a Filesystem Substrate - Broad Prior-Art Technical Disclosure and Research Architecture* (Version v1.0). Zenodo. https://doi.org/10.5281/zenodo.22092869

**Persist irreducible state. Materialize structure. Preserve exact bytes.
Measure everything.**

EntropyFS is a native-Rust, mountable Linux filesystem whose central research
premise is:

> Logical bytes are an interface presented to applications. They do not have
> to be the primary persisted representation. EntropyFS persists the minimum
> exact reversible entropy/configuration state necessary to reproduce those
> bytes.

The defining equation:

```text
X = Materialize(D)
```

`X` is the exact logical byte sequence; `D` is the persisted representation
descriptor. A more general family:

```text
X = T(E(U, S, P)) ⊕ R
```

`U` = versioned entropy universe · `S` = seed/state · `P` = rank/coordinate ·
`T` = bounded reversible transform · `R` = exact residual · `E` =
deterministic materialization.

EntropyFS does **not** claim to evade information theory: the SSD still
stores physical bits. Its innovation is to make *entropy state, mathematical
rank/configuration, immutable references, deterministic generators,
transforms, and irreducible residuals* first-class storage representations
instead of assuming logical byte blocks must themselves exist at rest. For
random/encrypted/incompressible data it converges gracefully toward ordinary
physical storage (RAW fallback) — that is a success condition, not a failure.

## Current status

| Phase | Scope | Status |
|-------|-------|--------|
| 0 | Research, ADRs, information-theory boundary, format v1, crash protocol | ✅ sealed (`docs/`) |
| 1 | In-memory representation engine: RAW/ZERO/FILL/INLINE/RANS/EXACT_REF/BASE_RESIDUAL/SPARSE/PALETTE/PERIODIC/PERMUTATION/ENTROPY_REF, cost accounting, round trips | ✅ implemented (tests green) |
| 2 | Persistent immutable store: segments, dual superblocks, records, descriptor codec, feature bits, fsck, crash courts, ENOSPC | ✅ sealed (fsck-verified, crash-court matrix) |
| 3 | Mountable FUSE filesystem: mkfs/mount/unmount, full POSIX battery (cp/diff/rsync/git clone/cargo build/untar/truncate/rename/hardlink/symlink/xattr/fsync), kernel-cache invalidation, fsck-clean | ✅ sealed (live-mount verified) |
| 4 | Entropy-native optimization: DSFB-guided search (P0-P5 channels, trust-ordered budget), exact dedup, base+residual with rebase-on-write, background optimizer (CAS-protected, resumable) + idle daemon worker, ablation benchmarks | ✅ implemented (ablation fixture + campaign `evidence/performance/campaign-1787658658-67d977a/`) |
| 5 | Snapshots, GC, robustness: snapshot create/list/delete/restore (live verified), GC pins snapshot roots, chunk-index reachability fix (deleted data reclaimable), near-full GC recovery from the emergency reserve, shrink-write extent fix, snapshot crash-court matrix | ✅ implemented (live + fsck verified) |
| 6 | Performance: deferred durability (logical commit + fsync barrier), search fast path, oversized-descriptor validation fix (SIGBUS root cause), multi-threaded FUSE. Sealed before/after FUSE court pair (`evidence/performance/INDEX.md`): 1M writes 185→653 MiB/s, 4K buffered 0.6→24.4 MiB/s, bindgen cold build FAILED (SIGSEGV/SIGBUS) → 9.5 s; fsync p50 320→1647 µs (deferred-durability tradeoff, reported honestly) | ✅ implemented (evidence-sealed) |
| 7 | Experimental ublk frontend: `src/ublk/` over the same engine — BlockStore adapter (4K blocks, read/write/flush/discard via the entropy engine, device = hidden store file), libublk target glue + `ublk run` CLI (root + `ublk_drv` required), `ublk bench` (kernel-free), unit tests, ADR-0020 | ✅ implemented (adapter live-verified; kernel binding needs root) |
| 8 (M1) | Concurrency refactor: `Store` interior mutability (root/superblock behind `RwLock`, 64-shard object index, per-inode lock table, short commit coordinator), reads traverse root snapshots without the global writer lock; FUSE writeback-cache negotiation (`FUSE_WRITEBACK_CACHE | ASYNC_READ | PARALLEL_DIROPS | BIG_WRITES`, 1 MiB max_write, background queues) | ✅ implemented (`d90772c`, 264→278 tests) |
| 8 (M2) | Write aggregation: `write_region_batch` group commit (one transaction + generation per batch, in-batch overlay for overlapping partial chunks), deferred durability, live 4K writes 24.4 → 319 MiB/s (13×), 1M writes 653 MiB/s, reads 2212 MiB/s | ✅ implemented (live court) |
| 8 (M3) | **SequenceRans** — the general-purpose compression floor: bounded LZ77 hash-chain matcher + three rANS-coded (or raw) streams over `ryg-rans-rs` (tag 0x0D, feature bit 10). Fixes two real defects found by the H2 campaign: encoder tail-remainder bug (`0x7F` corruption for 1–3-byte copy tails) and the flatten-on-write §32 validation gap; also fixed the store GC reachability walk (it under-counted SequenceRans objects — a withdrawn campaign caught it). src corpus: pure byte rANS 1.633× → SequenceRans **3.556×** (zstd -1 per-64KiB 3.739× — the per-extent floor is within 5%, the gap to whole-file zstd is cross-chunk context); urandom still 0.997× | ✅ implemented (evidence-sealed `campaign-1787671040-923df7b/`; the earlier “at parity with direct rANS” description conflated the pre-split gate and is amended in `INDEX.md`) |
| 8 (M4) | **BaseSequence** — shift-aware copy/literal delta residuals (residual kind 0x04 inside BASE_RESIDUAL): `COPY(base_offset, len)` / `LITERAL(run)` commands, three-stream rANS/raw codec shared with SequenceRans. Inserted/deleted regions cost only their own bytes. H2 flips back to **+35.2%** (sequential 2.752× vs shuffled 1.784×); the shuffled control grows because deltas also capture structural similarity — recorded as the finding | ✅ implemented (evidence-sealed `campaign-1787666036-43bf17e/`) |
| 8 (M5) | **SparseBlock64** — blockwise-64 enumerative sparse coding (tag 0x0E, feature bit 11): per-word popcount + `C(64,k)` rank (fits u64) + literals, three-stream rANS/raw codec. Removes the plain-SPARSE `u128` cliff (`10 ≤ k ≤ n−10` at 64 KiB). The campaign caught a 3× write-throughput regression from missing dense-input pre-gating; a `k ≥ n/2` density gate fixed it (regression-tested) | ✅ implemented (evidence-sealed `campaign-1787666589-e895fcf/`) |
| 8 (8A) | Evidence-protocol correction: the strict cumulative ladder A0–A8 (each step adds one mechanism, A8 = +background pass) now runs beside the leave-one-out table (spec §43, methodology §4); both are kept forever. The first campaign's nine-row table is amended as the leave-one-out table (protocol note, never rewritten) | ✅ implemented + evidence-sealed (`campaign-1787668526-d04227f/`) |
| 8 (8B) | Derived chunk-index rebuild: GC rebuilds the chunk index to exactly the reachable set (live extents + transitive reference closure), so overwritten unsnapshotted content cannot grow it permanently. H2 post-GC permanent footprint: sequential full 1,528,175 → **1,366,816 B** (10.6% pruned); regression-tested invariant `chunk_index_entries ≤ reachable + closure`, repeated GC never regrows the index, remount + fsck clean | ✅ implemented + evidence-sealed (`campaign-1787668526-d04227f/`) |
| 8 (8C) | Attribution correction + transaction-local CAS canonicalization: `allow_exact_ref` gates only the EXACT_REF alias representation (content-addressed object sharing is a store invariant, separately accounted: `cas_shared_bytes_saved` vs `exact_ref_bytes_saved`); `allow_rans` split into byte rANS (A1, pure again) + SequenceRans (E1, post-registration); duplicate records are never re-appended (one record per content id per transaction); duplicate chunks short-circuit to the canonical descriptor or alias, marginally cheapest (existing objects cost zero); post-GC footprint evidence (reachable/total backing/allocated blocks). Structured: E1 50,528 B (1,328×), post-GC allocated 61,440 B = **1,092×** (was 5.1 MB pre-GC backing); zstd-per-64K diagnostic: SequenceRans within 5% of zstd-per-64K ⇒ cross-chunk context is the next lever (SequenceDict) | ✅ implemented + evidence-sealed (`campaign-1787671040-923df7b/`) |
| 8 (8H) | Competitive filesystem court: `tools/fs-court.sh` measures the same corpora across ext4, zstd -1/-3/-19, and mounted EntropyFS; XFS/Btrfs±zstd/EROFS/SquashFS recorded as explicit waivers with the exact root-capable-VM commands. First run `fs-court-1787669946-b165d60`: EntropyFS effective density 1.488× incl. a 64 MiB incompressible control; zeros 453/4374 MiB/s write/read, random 85/3532 MiB/s, fsck clean | ✅ tooling + first run sealed (VM run clears the loop-mount waivers) |
| 9 (9A) | Physical floor: transaction-local COW-intermediate pruning — the incompressible backing floor collapses to ~1.00× (urandom reachable 33,652,515 / total backing 33,658,070 / allocated 33,665,024 B); `unreachable_bytes_by_record_tag` evidence identifies the pruned record class; ENOSPC guard on the pruned footprint | ✅ implemented + evidence-sealed (`campaign-1787674068-4892644/`) |
| 9 (9B) | **SequenceDict** — cross-chunk dictionary match coding (tag 0x0F, feature bit 12): the previous same-file chunk as an external ≤64 KiB dictionary beside local history, with a fourth copy-source stream (LOCAL backward distance vs DICT absolute offset; DICT continuation advances the offset). Reference depth accounted like a base chain (`dictionary chain + 1 ≤ max_reference_depth`), so cross-chunk references can never defeat bounded random access; terminal anchors emerge automatically at the depth cap. src corpus **4.070×** — beats standalone SequenceRans (3.627×) and zstd-per-64KiB -1 (3.848×). Also fixed three latent defects it surfaced: `flatten_if_deep` staged-object resolution (`MissingObject`), `current_persisted_bytes` object accounting (object-backed incumbents looked free), background full-byte candidate ordering | ✅ implemented + evidence-sealed (`campaign-1787676607-8250f6b/`) |
| 9 (9C) | **SequenceSharedDict** — shared amortized dictionary match coding (tag 0x10, feature bit 13): local history + optional previous same-file chunk + a **shared cross-file dictionary** in one stream (third copy-source symbol `SRC_SHARED`). The background `shared_dict_pass` picks a per-directory anchor — an existing terminal chunk that maximizes savings against member incumbents — and rewrites strictly-cheaper extents through the same CAS-gated, byte-validated commit path. GC pins the anchor through the reference closure (survives owner deletion). Sealed by the campaign's **tree court**: 279/282 real-tree files are single-chunk (previous-chunk dictionaries get ~no opportunity on a real tree — the packed-stream density is cross-FILE structure); per-file writes **2.182× → 2.328× post-GC** (102 extents, ~85.2 KiB saved) vs zstd per-file 3.541× / per-64KiB 3.991× (-1). The modest real-text gain and the strong synthetic-family mechanism are both recorded as-is | ✅ implemented + evidence-sealed (`campaign-1787679299-8d6e147/`) |
| 9 (9D) | **Anchor pool**: `shared_dict_pass` selects up to four per-directory anchors greedily by marginal savings against member incumbents; each extent picks its best pool anchor during the rewrite. Heterogeneous directories get per-file dictionary choice (pool saves ~2× the single-anchor on a two-cluster fixture) | ✅ implemented (sealed with 9E) |
| 9 (9E) | **SequenceDeep** — deep-match family (tag 0x11, feature bit 14): repcodes (REP0/REP1 carry no offset symbol) + extended length codes (one XCOPY/XLIT + u16 extra instead of 131-byte continuation runs), fed by a deep background matcher (chain 256, lazy parse with a minimum-gain threshold, rep-distance priority). Background-only; terminal. Standalone deep floor **3.786× vs fast 3.736×** on the src pack (deep wins all chunks); ladder E4 densifies structured 50,528 → 50,238 B | ✅ implemented + evidence-sealed (`campaign-1787681660-9be6bd3/`) |
| 9 (9F) | **Gap decomposition sealed** — the remaining gap to per-file zstd is measured, not asserted: zstd with the same per-directory anchor (`-D`, self-excluded) gains only ~8.5% (the anchor policy is NOT the cap); the residual gap is ~2/3 **per-extent persistence overhead** (multi-stream rANS models + descriptors on small files, ~26.5% of footprint) and ~1/3 coder quality. Also falsified: `scale_bits` does not shrink models (symbol-count-dominated encoding). Sets the 9G direction: amortized model sharing | ✅ sealed (`campaign-1787683904-da26c75/`) |

## Measured results

All performance and storage-density claims are governed by
`docs/performance/methodology.md`; admitted results live in
`evidence/performance/` (see `evidence/performance/INDEX.md`).

**FUSE-frontend before/after pair (Phase 6, same workloads, same machine):**

| Workload | `709a710` (before) | `027c959` (after) |
| --- | --- | --- |
| 1M writes | 185 MiB/s | 653 MiB/s |
| 4K buffered writes | 0.6 MiB/s | 24.4 MiB/s |
| bindgen cold build (target on mount) | FAILED (SIGSEGV/SIGBUS) | 9.5 s |
| fsync p50 | 320 µs | 1647 µs |

The fsync regression is the measured cost of deferred durability and is
reported honestly; the before half reproduces the crashes the
oversized-descriptor fix (Phase 6) eliminated.

**What EntropyFS does and does not claim:**

- The synthetic ablation fixture (`evidence/ablation-2026-08-25.json`) is an
  *ablation fixture*, never a headline: on a corpus containing four unique
  64 KiB chunks its 16.876× is dominated by content-addressed dedup. The
  campaign's structured-corpus ratios (up to 1,328×) are *structural +
  EXACT_REF aliasing + CAS object sharing* — attribution is now
  measured, not labeled: per-run accounting separates `cas_shared_bytes_saved`
  (a store invariant) from `exact_ref_bytes_saved` (the gated alias
  representation), and A1 is pure byte rANS with SequenceRans as the
  post-registration E1 step (`campaign-1787671040-923df7b/`). The earlier
  “dedup = 0” and “dedup-dominated” statements conflated the two layers
  and are amended in `evidence/performance/INDEX.md`, never rewritten.
- The campaign's ablation evidence is two tables, both kept forever: the
  **strict cumulative ladder A0–A8** (each step adds one mechanism) and the
  **leave-one-out table** (one mechanism disabled at a time). The first
  campaign's nine-row table is the leave-one-out table; it predates the
  two-table rule and is amended as such in `evidence/performance/INDEX.md`
  (protocol note, never rewritten).
- The campaign's DSFB investigation is a sealed three-era series, all
  with byte-identical physical representations: RANS-era 765.4 vs 334.7
  MiB/s (2.29×, user CPU halved — `67d977a`), SequenceRans-era 773.9 vs
  717.1 MiB/s (~8%, `b165d60`), and CAS-era 1,120.8 vs 1,106.1 MiB/s
  (~1.3%, `923df7b`). DSFB's marginal benefit collapsed as the
  SequenceRans floor simplified the search landscape — evidence for its
  assigned role as candidate-search budget intelligence, not compression;
  its marginal value now is small on this tiny synthetic corpus and its
  proper counters are deferred. Historical numbers are preserved in
  `evidence/performance/INDEX.md`; the 2.29× is not a current claim.
- The source-corpus progression is now sealed across eras: `923df7b` pure
  byte rANS 1.633× / standalone SequenceRans 3.556× (with
  zstd-per-64KiB -1 at 3.739× — the per-extent floor was within 5%, and
  the gap to whole-file zstd was cross-chunk context), `8250f6b`
  **EntropyFS full 4.070× with SequenceDict** on the packed stream, and
  the Phase-9C/9D/9E tree court answering the open mount-level question:
  on a real tree of separately-written files, per-file zstd -1 is ~3.5×
  and EntropyFS per-file writes are **2.194×, rising to 2.354× post-GC
  after the shared-dict pool + deep background pass** — so most of the
  packed-stream density was indeed cross-FILE structure, and the shared
  dictionary (plus the deep matcher, which lifts the standalone src-pack
  floor from 3.736× to 3.786×) recovers a measured part of it. The
  Phase-9F gap decomposition (sealed in the 9F tree court) shows where
  the remaining gap lives: the anchor policy is NOT the cap (zstd with
  the same per-directory anchor gains only ~8.5%, less than EntropyFS's
  pool already recovers), and the residual gap is **~2/3 per-extent
  persistence overhead** (per-chunk multi-stream rANS models + descriptors
  on small files; ~26.5% of the EntropyFS footprint) and **~1/3 coder
  quality**. Recorded as current state, not claims.
- The campaign's H2 experiment (synthetic drift corpus) is now a sealed
  **controlled series**: `67d977a` +7.2% (RANS-era floor), `a6641d1`
  −24% (SequenceRans floor, positional residuals only), `43bf17e`
  **+35.2%** (SequenceRans floor + BASE_SEQUENCE shift-aware deltas), and
  `923df7b` **+40.6%** (marginal costing). The shuffled control grows in
  the delta campaign because copy/literal deltas also exploit structural
  similarity between unrelated-history chunks — the control no longer
  isolates pure temporal causality, and that confounding is itself
  recorded as the finding.
- Random/encrypted/already-compressed data falls back toward RAW (urandom
  0.997×, zstd -19 pack 0.993×) — the honest negative control.

## Honesty rules

- A 128-bit seed does not "store" a gigabyte. Descriptor bits select at most
  `2^k` states; every persisted bit is accounted
  (`docs/theory/information-accounting.md`).
- No hidden corpus, no network, no RNG in materialization, no CPU-dependent
  floating point. The universe specification is part of the format version.
- No arbitrary generator programs: the descriptor language is bounded and
  not Turing-complete (`docs/adr/0005-representation-set.md`).
- DSFB has zero decoding authority (`docs/adr/0004-dsfb-observer.md`).
- `statfs` reports physical capacity; effective ratio is an observation,
  never a promise (`docs/adr/0018-statfs.md`).
- Every optimization claim requires reproducible evidence
  (`docs/performance/methodology.md`).

## Building

```sh
rustup toolchain install stable --profile minimal --component rustfmt,clippy
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Target platform

CachyOS/Arch Linux, x86-64, FUSE (`/dev/fuse` + `fusermount3`; kernel
`CONFIG_FUSE_FS=y`). No custom kernel, no out-of-tree module, no reboot.

## Reading order

- `docs/architecture/overview.md` — architecture map
- `docs/theory/entropy-medium.md` — the information-theory boundary statement
- `docs/format/ondisk-v1.md` — the on-disk format
- `docs/recovery/crash-consistency.md` — the crash protocol
- `docs/adr/` — all architecture decision records

## License

MIT OR Apache-2.0, at your option.
