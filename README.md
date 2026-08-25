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
| 8 (M3) | **SequenceRans** — the general-purpose compression floor: bounded LZ77 hash-chain matcher + three rANS-coded (or raw) streams over `ryg-rans-rs` (tag 0x0D, feature bit 10). Fixes two real defects found by the H2 campaign: encoder tail-remainder bug (`0x7F` corruption for 1–3-byte copy tails) and the flatten-on-write §32 validation gap; also fixed the store GC reachability walk (it under-counted SequenceRans objects — a withdrawn campaign caught it). src corpus density 1.636× → **3.344×** (at parity with direct rANS; zstd -1 3.83× — the deeper matcher is the measured next step); urandom still 0.997× | ✅ implemented (evidence-sealed `campaign-1787665094-a6641d1/`) |
| 8 (M4) | **BaseSequence** — shift-aware copy/literal delta residuals (residual kind 0x04 inside BASE_RESIDUAL): `COPY(base_offset, len)` / `LITERAL(run)` commands, three-stream rANS/raw codec shared with SequenceRans. Inserted/deleted regions cost only their own bytes. H2 flips back to **+35.2%** (sequential 2.752× vs shuffled 1.784×); the shuffled control grows because deltas also capture structural similarity — recorded as the finding | ✅ implemented (evidence-sealed `campaign-1787666036-43bf17e/`) |

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
  campaign's structured-corpus ratios (up to 211× on that same corpus)
  have the same corpus property and are presented as ablation data only.
- The campaign's DSFB investigation (5+5 repeated runs) found: with DSFB
  ranking enabled vs disabled, the final physical representation is
  byte-identical (79,298 B) while write throughput is 765 vs 335 MiB/s and
  user CPU halves — evidence for DSFB's assigned role as candidate-search
  budget intelligence, not compression. Single synthetic corpus; under
  further study.
- The campaign's H2 experiment (synthetic drift corpus) is now a sealed
  **three-campaign controlled series**: `67d977a` +7.2% (RANS-era
  floor), `a6641d1` −24% (SequenceRans floor, positional residuals
  only), `43bf17e` **+35.2%** (SequenceRans floor + BASE_SEQUENCE
  shift-aware deltas — sequential 2.752× vs shuffled 1.784×). The
  shuffled control grows in the delta campaign because copy/literal
  deltas also exploit structural similarity between unrelated-history
  chunks — the control no longer isolates pure temporal causality, and
  that confounding is itself recorded as the finding.
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
