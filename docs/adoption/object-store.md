# The object-store adoption story (Phase 12E.13)

The adoption wedge discovered by the 12E.13 court
(`evidence/performance/adoption-oracle-*`,
`docs/performance/adoption-oracle.md`): a **storage-density wedge for
versioned and structured immutable object populations**. Four of the six
brief-mandated workloads clear the 10× footprint bar against a raw-file
baseline on the same device:

| workload | footprint vs raw | mechanism |
| --- | ---: | --- |
| versioned build artifacts | **0.049× (20×)** | 73% dedup + structure-aware compression |
| versioned scientific outputs | **0.055× (18×)** | compression (template structure) |
| container-like layers | **0.084× (12×)** | 52% dedup + compression |
| near-duplicate generated assets | **0.096× (10.4×)** | compression |

Recorded tradeoff, as measured: put throughput is ~14× slower than raw
page-cache file writes on this corpus (the write path is CPU-bound on
the foreground search), get is fast (114–691 MiB/s across workloads,
per-blob p50 20–90 µs), and every byte round-trips exactly through the
facade's hash gate.

## The no-impossible-media-claims policy (Phase 12E.16)

Adoption demonstrations MUST NOT take the form "25 MB physical vs 50 GB
arbitrary H.264" unless the complete experiment proves that exact result
with ALL reconstructive state accounted for (grammar/model/state bytes
included). Specifically:

- already-compressed / encrypted / random inputs remain valid RAW-
  fallback controls — never headline material;
- a demonstration is chosen because a NATURAL workload genuinely
  contains exploitable duplication, version similarity, shared
  structural context, generative structure, or configuration
  relationships — never because a compression ratio was picked first;
- the information-theoretic boundary is absolute: every byte needed to
  reconstruct the logical output is persisted and accounted;
- failed or unflattering measurements are preserved (the evidence
  discipline), never deleted or reframed.

The sealed adoption oracle obeys this policy by construction: its
baselines are plain files on the same device, its corpora are the
brief's natural workloads, and its accounting (logical / unique / dedup
saved / settled physical) is exact.

## Embedding without FUSE

The `Engine` facade (`docs/api/engine.md`) is the adoption surface: put /
get / range / sync / compact / metrics, typed errors, exact bytes. The C
ABI (`docs/api/c-abi.md`) and the Go binding (`docs/api/go.md`, with the
content-store example in `go/examples/content-store/`) are the
interoperability layers. An infrastructure engineer embeds the engine
without mounting FUSE, writing Rust, or parsing native error strings.
