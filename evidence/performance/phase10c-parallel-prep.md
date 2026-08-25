# Phase-10C: parallel chunk preparation (sealed court comparison, revision 5a5f2f3)

Archives: `fs-court-1787694187-5a5f2f3/` (foreground=full),
`fs-court-1787694222-5a5f2f3/` (foreground=cheap). Zero waivers,
privileged docker VM, symmetric rules, 1 FUSE thread, background
optimizer disabled for the foreground section. Same machine and tooling
as the sealed Phase-10B pair (`fs-court-1787691596-d38f73f`,
`fs-court-1787691637-d38f73f`).

## What changed

`prepare_write` now composes every chunk's final bytes serially, encodes
the chunks CONCURRENTLY (scoped threads; the candidate search is
independent per chunk — prev version from the RMW read, in-batch
dictionary from the composed bytes), and applies the batch semantics
(in-batch dedup canonicalization, real chain-depth enforcement, pending
registration) serially in offset order for ONE commit. A single-chunk
write (the common FUSE request) runs inline to avoid thread-spawn
latency. Byte-identical to the serial path: each chunk's search validates
its candidates against a synthetic view of its in-batch dictionary
(assumed depth 0 — the encoder's streams depend only on input + dict
bytes), and the serial phase re-validates against the REAL batch state,
re-encoding any outcome the synthetic view got wrong with exactly the
serial search's input (real pending + real dictionary depth). Regressions:
`tests/write_parallel` (byte-exactness across chunk counts, cross-store
determinism, consecutive-identical-chunk hazard, aliased-first-occurrence
duplicates, depth-cap re-anchor), crash courts, fsck, snapshots green.

## The result (mounted FUSE, same corpus set, same machine as 10B)

| corpus | 10B full | 10C full | 10B cheap | 10C cheap |
| ------ | -------- | -------- | --------- | --------- |
| src tiny-file writes (buffered) | 10.1 | 10.4 | 10.0 | 10.2 MiB/s |
| src tiny-file writes (durable) | 5.5 | 7.9 | 5.3 | 9.1 MiB/s |
| random 64 MiB writes (buffered) | 66.5 | **148.8** (2.2×) | 229.3 | 233.9 MiB/s |
| random 64 MiB writes (durable) | 61.9 | **145.4** (2.3×) | 176.5 | 226.1 MiB/s |
| zeros 64 MiB writes (buffered) | 239.6 | **271.8** | 235.4 | 265.8 MiB/s |
| compressed.tgz writes (buffered) | 42.0 | **63.8** | 66.3 | 68.1 MiB/s |
| compressed.tgz writes (durable) | 4.7 | **26.9** (5.7×) | 4.9 | 26.8 MiB/s |
| warm random reads | 2570 | 2743 | 2493 | 2559 MiB/s |
| settled density | 1.994× | **1.994×** | 1.994× | **1.994×** |
| settle cost | 5.46 s | 5.39 s | 5.36 s | 5.39 s |
| settled fsck / reconciliation | clean | clean (live canonical 100.0%) | clean | clean |

The parallel search scales the search-bound paths (random full 2.2×
buffered, tgz durable 5.7×); the src tiny-file path is still
namespace-transaction-bound (create_entry/setattr = one transaction
each) — the 10D metadata-epoch lever, unchanged in 10C. The settled
density floor holds byte-for-byte at 1.994× in all four runs; the
cheap-policy random advantage over full (233.9 vs 148.8) is preserved
from 10B.

## Direct-store diagnostic (same machine, release profile)

A/B of `tests/perf_diag.rs` at d38f73f (serial) vs 5a5f2f3 (parallel),
64 MiB single-call writes:

| corpus | d38f73f (serial) | 5a5f2f3 (parallel) | Δ |
| ------ | ---------------- | ------------------ | - |
| random 64 MiB (full) | 38.0 | **180.3 MiB/s** | 4.7× |
| src pack (full) | 26.9 | **164.0 MiB/s** | 6.1× |
| ZERO 64 MiB (full) | 907.6 | **1708.1 MiB/s** | 1.9× |
| random (raw-only control) | 589.3 | 605.2 MiB/s | — |

(The 10B commit message's direct-store 852 MiB/s was measured on a
different machine; the same-machine A/B above is the controlled
comparison.)
