# Performance Methodology

Status: pre-results methodology. This document defines what evidence must
exist before any EntropyFS performance, storage-density, optimization, or
capacity claim is treated as a project fact.

This file mirrors the paper's empirical program. It is intentionally written
before performance results are available so the measurement rules are not
adapted after seeing favorable or unfavorable numbers.

## 1. Benchmark Context

Every benchmark run must record:

- input corpus name, source, version, and content hash;
- EntropyFS git revision;
- `Cargo.lock` hash and relevant dependency versions;
- kernel version and filesystem/mount configuration;
- CPU model, CPU feature flags, RAM size, storage device, and governor;
- benchmark command line, environment variables, and policy mode;
- cache state: cold, warm, dropped page cache, or explicitly retained cache;
- representation distribution by descriptor tag and byte count;
- logical bytes, reachable physical bytes, and total backing-store allocation;
- result hashes for materialized output.

If any of these are missing, the run may be exploratory but must not support a
published optimization claim.

## 2. Exact Storage Accounting

For every corpus, report:

```text
P =
    P_payload
  + P_metadata
  + P_models
  + P_indexes
  + P_residuals
  + P_integrity
  + P_allocator
  + P_unreclaimed
```

Then report:

```text
R = L / P
```

where `L` is exact logical materialized bytes and `P` is the total physical
state required to reconstruct them.

Required accounting categories:

- payload objects;
- descriptors and extent maps;
- entropy models;
- indexes required for decoding;
- indexes used only for future optimization;
- residuals;
- content hashes and integrity metadata;
- allocator and segment overhead;
- unreachable bytes awaiting garbage collection;
- reserved garbage-collection headroom.

Indexes or models required for decoding count in `P`. Indexes used only to
accelerate future encoding are reported separately as operational overhead, but
they still consume physical resources.

## 3. Baselines

Use strong baselines, not only raw files.

| Baseline | Purpose |
| --- | --- |
| RAW files on ext4 or XFS | ordinary writable storage baseline |
| Btrfs with compression | mature writable compressed filesystem baseline |
| EROFS or SquashFS | cold read-only compressed image baseline |
| zstd at fast and high-ratio settings | external compression baseline |
| direct rANS using the same backend | entropy-coding-only baseline |
| exact deduplication only | isolates duplicate-content savings |
| base-plus-residual only | isolates temporal/reference savings |
| EntropyFS feature-disabled variants | isolates each added representation family |

Comparisons against only raw storage are insufficient for broad claims.

## 4. Ablation Ladder

Run the system in strict increments:

```text
A0  RAW
A1  RAW + rANS
A2  + exact dedup
A3  + base residuals
A4  + sparse/configuration rank
A5  + temporal candidate bases
A6  + entropy universes
A7  + DSFB candidate guidance
A8  + background re-optimization
```

Rules:

- Report the incremental gain from each stage.
- If most savings occur at `A2`, call them deduplication savings.
- If `A7` changes CPU cost but not storage size, report exactly that.
- Never credit DSFB with savings produced by rANS, deduplication, or base
  residuals.
- If `A6` contributes no benefit after selector and residual cost, report that
  as a negative result.

## 5. Negative Controls

Several experiments are expected to fail. They must remain in the results.

| Control | Expected Result | Failure Signal |
| --- | --- | --- |
| `/dev/urandom` input | RAW or near-RAW accounting | claimed large compression gain |
| encrypted representative corpora | RAW or near-RAW accounting | hidden structure or accounting error |
| already-compressed archives | little or no additional gain | ratio reported without overhead |
| random XOF entropy universe | no net gain after selector cost | seed treated as free information |
| shuffled temporal history | temporal/base gains disappear | history signal is not actually causal |
| random candidate ranking | worse or equal to useful ranking | DSFB benefit not demonstrated |
| metadata accounting off/on | visible ratio degradation when on | headline ratio excludes required bytes |

Negative controls protect the project from mistaking hidden side information,
selection bias, or reporting omissions for storage capacity.

## 6. Required Metrics

Capacity:

- logical bytes;
- reachable physical bytes;
- total physical allocation;
- metadata bytes;
- model bytes;
- index bytes;
- residual bytes;
- integrity bytes;
- unreclaimed garbage;
- effective ratio.

Read path:

- sequential throughput;
- random IOPS;
- p50, p95, and p99 latency;
- physical bytes fetched per logical byte;
- CPU cycles per logical byte;
- cache behavior.

Write path:

- foreground write latency;
- fsync latency;
- physical bytes written;
- device-level write amplification;
- optimizer traffic;
- garbage-collection traffic.

Maintenance:

- mount time;
- recovery time;
- fsck time;
- garbage-collection throughput;
- background optimization throughput;
- peak RAM.

## 7. Statistical Practice

Filesystem correctness is deterministic; performance measurement is noisy.

- Correctness is established by exact materialization hashes and byte
  comparison, not by statistical confidence.
- Performance runs should report medians and tail percentiles.
- Use fixed workload seeds for synthetic corpora.
- Separate cold-cache and warm-cache measurements.
- Record thermal state and CPU governor where they can affect results.
- Report run count and spread; use confidence intervals where appropriate.

## 8. Claim Admission Rules

A claim is admissible only if:

- the benchmark context is complete;
- every required byte is counted;
- all listed baselines relevant to the workload are run or explicitly waived;
- ablations identify which mechanism caused the gain;
- negative controls are included;
- materialized output hashes match the input;
- raw result artifacts are archived.

Statements that do not meet these rules must be labeled exploratory.

## 9. Evidence Location

Future benchmark evidence should be stored under:

```text
evidence/performance/
```

Each run should include:

- raw command output;
- machine-readable JSON or CSV metrics;
- corpus manifest;
- environment manifest;
- result hash manifest;
- short human-readable report.

The absence of files in `evidence/performance/` means no project performance
claim has yet been admitted.
