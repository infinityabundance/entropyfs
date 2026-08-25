# Baselines and expected regimes

Status: initial expectations, to be replaced by measured evidence.

## 1. Expected behavior by data class

| Data | Expected winning representation | Expected ratio |
|------|--------------------------------|----------------|
| zeros / fills | ZERO / FILL / PERIODIC | ~0 |
| sparse structured (configs, patches) | SPARSE / BASE_RESIDUAL | low |
| low-cardinality text-ish | PALETTE / RANS | low–moderate |
| natural text / source | RANS (interleaved2) | moderate |
| binaries with repetition | RANS + dedup | moderate |
| versioned files (write-in-place) | BASE_RESIDUAL vs prior | low–moderate |
| duplicated trees (build dirs) | EXACT_REF (dedup) | high dedup |
| already-compressed / encrypted / random | RAW | ~1.0 |

The H6 requirement: random/encrypted data shows **no artificial gain** and
converges to RAW within a small constant (descriptor + integrity
overhead, ~0.1–1%).

## 2. Overhead budget per extent (RAW path)

For a 64 KiB RAW extent, persisted bytes ≈ 65536 + descriptor (~40 B) +
record envelope (58 B) + content id amortization + GC overhead. The
"RAW overhead" is measured and reported; it must stay < 1.5% of logical.

## 3. Read/write amplification expectations

- Write amplification: append-only ⇒ ≥ 1 physical write per logical chunk
  at commit, plus GC copying later (GC amplification reported
  separately).
- Read amplification: 1 physical read per raw/rANS object; references may
  add dependent reads (bounded by depth); the H5 question is whether
  materialization beats fetching bytes on slow storage.

## 4. Comparative targets (initial, to be confirmed by measurement)

- vs Btrfs zstd on compressible corpora: EntropyFS should be within a
  modest factor on density while adding dedup/configurational wins where
  structure exists; exact numbers are evidence, not promises.
- vs zstd: rANS is generally comparable on bytes for byte-alphabet
  streams; configurational representations apply where zstd's LZ
  dictionary cannot (e.g., pure combinatorial structure).
- vs EROFS/SquashFS: cold sequential read throughput is the comparison
  surface; EntropyFS is writable, so the comparison is about read-path
  efficiency only.

## 5. Latency budgets (initial targets, unverified)

| Op | Target |
|----|--------|
| cached read p50 (64 KiB extent materialization) | < 50 µs on modern x86 |
| write commit (single chunk, no GC) | < 1 ms including fdatasync on SSD |
| fsync | < 2 ms on SSD |
| mount (cold, 10 GiB store, rebuild index) | < 5 s |

These are targets to measure against, not claims.
