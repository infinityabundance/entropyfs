# Explicit non-claims

Phase 0 deliverable. Anything not listed here that looks like a miracle is
also a non-claim. These statements are binding on EntropyFS documentation,
benchmarks, and UI:

1. **No claim that a seed "stores" a gigabyte.** A descriptor with `k`
   independent bits selects at most `2^k` states. Generated output size and
   independently stored information are different quantities.
2. **No violation of information theory.** The SSD still stores physical
   bits. EntropyFS changes *what* is persisted (irreducible state), not the
   physics.
3. **No hidden corpus.** No network, no machine-local dictionary, no
   timestamps, no RNG, no CPU-dependent floating point in materialization.
   The universe specification is part of the format version.
4. **No free compression of random/encrypted data.** Such data converges to
   RAW at ~100% physical cost. That is a success condition, not a failure.
5. **No guaranteed capacity ratio.** `statfs` reports physical capacity
   (ADR-0018); "effective ratio" is an observed, workload-dependent
   statistic.
6. **No universal speed claim.** Regeneration is never assumed faster than
   reading bytes without measurement (§45).
7. **No DSFB decoding authority.** DSFB never decides bytes (ADR-0004).
8. **No ML/LLM in the correctness path.** The optimizer may be guided; the
   decoder is deterministic arithmetic.
9. **No Turing-complete descriptors.** No arbitrary generator bytecode
   (ADR-0005).
10. **No brute-force 2^128 seed search.** `UniformXofV1` is a negative
    control (ADR-0005).
11. **No GPU requirement.** GPU search is optional, experimental, and never
    needed to decode.
12. **No kernel module, no out-of-tree module, no experimental ABI
    dependency.** FUSE stable ABI; ublk/io_uring-FUSE are not foundations.
13. **No unbounded anything.** Limits are enforced before allocation
    (`docs/security/resource-bounds.md`).
14. **No claim of a "compression ratio."** We report per-component byte
    accounting, not an opaque ratio (§42).
15. **No claim that every workload benefits.** The interesting hypothesis is
    about *many useful* workloads, not all.
16. **No credit misattribution.** Ablation science attributes every byte of
    savings to its actual mechanism (§43).
17. **No encryption before the storage engine is correct** (ADR-0015).
18. **No "infinite storage."** This project is not that, and will not
    become that.
