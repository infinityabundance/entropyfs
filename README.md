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
| 4 | Entropy-native optimization: DSFB-guided search (P0-P5 channels, trust-ordered budget), exact dedup, base+residual with rebase-on-write, background optimizer (CAS-protected, resumable) + idle daemon worker, ablation benchmarks | ✅ implemented (ablation table + live H2 drift verified) |
| 5 | Snapshots, GC, robustness: snapshot create/list/delete/restore (live verified), GC pins snapshot roots, chunk-index reachability fix (deleted data reclaimable), near-full GC recovery from the emergency reserve, shrink-write extent fix, snapshot crash-court matrix | ✅ implemented (live + fsck verified) |
| 6 | Performance: deferred durability (logical commit + fsync barrier; process-crash safe, power-loss falls back to the newest root record), search fast path (P0 from RMW bytes, decisive-win early exit, rANS-coded residuals), oversized-descriptor validation fix (SIGBUS root cause), fsck corrupt-descriptor resilience, multi-threaded FUSE verified. Measured: 4K writes 35→47 MB/s, 1M writes 601→721 MB/s, bindgen build 4m14s→1m13s | ✅ implemented (live verified) |
| 7 | Experimental ublk frontend: `src/ublk/` over the same engine — BlockStore adapter (4K blocks, read/write/flush/discard via the entropy engine, device = hidden store file), libublk target glue + `ublk run` CLI (root + `ublk_drv` required), `ublk bench` (kernel-free), unit tests, ADR-0020 | ✅ implemented (adapter live-verified; kernel binding needs root) |

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
