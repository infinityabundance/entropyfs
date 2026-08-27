# Phase-12B: durability generations + group commit

Sealed: `evidence/performance/fsync-group-probe-baseline-1787792160-91cc1ba/`
(the pre-12B convoy baseline) and
`evidence/performance/fsync-group-probe-group-*/` (the group-commit
after). Crash court: `src/tests/durability_group_crash.rs`. Coordinator:
`src/store/durability.rs`. Probe: `src/tests/fsync_group_probe.rs`.

## The model

The 12B brief: amortize concurrent `fsync` barriers without weakening the
durability contract:

```text
logical_seq   monotonically identifies acknowledged mutation state
durable_seq   highest logical sequence known to survive power loss

fsync(required_seq = N) may return success iff durable_seq >= N
```

EntropyFS's acknowledged-mutation state has TWO monotonic coordinates:

- `seq` — the epoch's mutation-log sequence (`Epoch::seq`; envelopes
  `> root.log_seq` are replayed at recovery). Covers staged epoch ops.
- `gen` — the published root's `generation` (bumped by EVERY commit:
  epoch checkpoints AND direct transaction commits). Covers direct
  non-epoch writes, which never advance `seq`.

A completed physical barrier certifies everything ≤ its cut
`(seq, gen)` componentwise; the group's `durable_seq`/`durable_gen`
atomics advance to the LAST COMPLETED CUT — never beyond it, even when
the owner's checkpoint flushed more (a mutation acknowledged after the
cut was chosen must not inherit the barrier; the brief's example: cut
chosen 100, write seq 101 occurs, barrier completes, `durable_seq = 100`,
the seq-101 fsync stays pending).

## The coordinator

`DurabilityGroup` (`src/store/durability.rs`): waiters (required seq/gen +
the generation that will cover them), the in-flight owner's cut, and a
generation-tagged failure record. The first waiter when idle becomes the
OWNER: it fixes the cut at the componentwise max of the CURRENT waiters,
runs the unchanged physical barrier (epoch checkpoint → commit-lock-held
fdatasync → dir sync → superblock write → superblock fsync, same crash
hooks), advances the durable atomics to the cut on success, stores the
generation-tagged error on failure, and wakes everyone. Each waiter
returns Ok only when the durable state covers its requirement, surfaces
its OWN generation's error on failure (never inherits another
generation's), or becomes the next owner. The physical barrier is
byte-for-byte the pre-12B sequence — only WHO runs it and WHO waits
changed.

## The baseline (sealed at the 12B-0 commit)

`fsync_group_probe`: concurrent write+fsync loops at 1/2/4/8/16/32
writers. Amplification = physical barriers / fsync requests.

| writers | amp | p50 µs | p95 µs | p99 µs | commit_lock_wait |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1.00 | 28.8 | 45.4 | 45.4 | 0.0 ms |
| 2 | 1.00 | 37.6 | 69.5 | 69.8 | 0.1 |
| 4 | 1.00 | 51.5 | 148.4 | 165.0 | 1.0 |
| 8 | 1.00 | 65.1 | 175.3 | 198.1 | 1.6 |
| 16 | 1.00 | 106.2 | 788.7 | 1175.0 | 24.0 |
| 32 | 1.00 | 242.9 | 5481.3 | 7858.3 | 366.1 |

Every fsync ran its own physical barrier (amplification 1.00); the convoy
is the growing tail: p99 45 µs → 7.9 ms at 32 callers, with the
commit-lock wait (366 ms cumulative) as the serialization witness.

## The group-commit result (sealed at 0.7.9)

| writers | amp | p50 µs | p95 µs | p99 µs | commit_lock_wait |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1.00 | 28.9 | 45.6 | 45.6 | 0.0 ms |
| 2 | 1.00 | 38.0 | 64.9 | 72.6 | 0.1 |
| 4 | 0.83 | 61.1 | 122.2 | 172.2 | 0.5 |
| 8 | 0.84 | 72.6 | 126.4 | 165.7 | 0.6 |
| 16 | 0.57 | 213.5 | 563.8 | 967.9 | 4.6 |
| 32 | **0.23** | 1323.2 | 3120.6 | 4063.9 | 13.7 |

- **Amplification collapses under concurrency**: 545 logical fsyncs at 32
  writers become 127 physical barriers (each barrier covers ~4.3
  fsyncs). Single-writer amplification stays 1.00 (nothing to coalesce).
- **The tail convoy is gone**: p99 7.86 → 4.06 ms at 32 writers (−48%);
  commit_lock_wait 366 → 13.7 ms (−96%).
- **The median shifts up** at 16/32 writers (106 → 214 µs, 243 → 1323 µs):
  a waiter parks for the generation's cycle instead of running its own
  barrier immediately. The brief's trade is explicit: group commit trades
  a little median for the elimination of the convoy tail — and the wall
  time still drops (32-writer wall 94 → 80 ms) because the physical
  work is done once per generation instead of once per fsync.
- **Correctness unchanged**: byte-exact read-back at every row; the crash
  court (`durability_group_crash`) injects a crash at every one of the
  five physical-barrier stages under 8 concurrent writers and verifies
  every RETURNED fsync's bytes survive recovery with a clean fsck; the
  full 431-test suite green (including the unmodified `durability`,
  `crash_recovery`, and `io_backend_parity` power-loss courts).

## The gate

The brief's target — `N fsyncs -> << N physical barriers` at high
concurrency without weakening recovery semantics — is met: 0.23 at 32
callers, tail latency −48%, commit-lock wait −96%, and every returned
fsync survives a crash at every barrier stage. The design deliberately
kept the physical barrier (steps, hooks, commit-lock hold) identical; the
group only decides who runs it and who waits.

## Crash oracle (the brief's wording)

```text
if fsync returned:        its required sequence is recoverable after
                          simulated power loss   -> asserted (byte-exact)
if fsync had not returned: either state is admissible depending on the
                          barrier cut             -> never asserted
```

The crash court verifies exactly this at `AfterRecordAppend`,
`AfterSegmentFdatasync`, `AfterSegmentDirFsync`, `AfterSuperblockWrite`,
and `AfterSuperblockFsync`.
