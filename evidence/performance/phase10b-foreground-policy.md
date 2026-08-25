# Phase-10B: ForegroundPolicy (sealed court comparison, revision d38f73f)

Archives: `fs-court-1787691596-d38f73f/` (foreground=full),
`fs-court-1787691637-d38f73f/` (foreground=cheap). Zero waivers,
privileged docker VM, symmetric rules, 1 FUSE thread, background
optimizer disabled for the foreground section.

## The result (mounted FUSE, same corpus set)

| corpus | full | cheap | Δ |
| ------ | ---- | ----- | - |
| src tiny-file writes | 10.1 MiB/s | 10.0 MiB/s | — (namespace-bound, 10D) |
| random 64 MiB writes | 66.5 MiB/s | **229.3 MiB/s** | **3.4×** |
| zeros 64 MiB writes | 239.6 MiB/s | 235.4 MiB/s | — |
| compressed.tgz writes | 42.0 MiB/s | **66.3 MiB/s** | 1.6× |
| daemon CPU utilization | 0.41× | **0.26×** | −37% |
| settled density | 1.994× | **1.994×** | **no regression** |
| settle cost | 5.46 s | 5.36 s | — |
| settled fsck | clean | clean | — |

The cheap policy probes each chunk (deterministic sampled entropy,
anti-aliasing min over three consecutive strides) and sends
high-entropy chunks straight to dedup + ZERO/FILL + RAW. The random
corpus is 3.4× faster because the LZ/entropy families no longer prove
the obvious; the src corpus is unchanged because its cost is the
namespace transaction path, not the search. The settled density is
byte-for-byte identical to full: the background optimizer recovers
everything the foreground defers (regression test: raw-only foreground
converges to within 129 B of full-foreground after the optimize pass).

Direct-store diagnostic (tests/perf_diag.rs): random 64 MiB 39.8 →
852 MiB/s (21×), src pack 27 → 199 MiB/s, ZERO ~950 MiB/s unchanged.
