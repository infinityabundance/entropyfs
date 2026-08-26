# Code commentary standard

EntropyFS treats implementation rationale as part of the custodial
artifact. The implementation itself is research evidence: someone reading
`epoch.rs`, `transaction.rs`, the representation algebra, GC, the io_uring
transport, the hostile-media parser, the worker pool, or the optimizer
should not have to reverse-engineer months of reasoning to understand why
an apparently strange invariant exists.

**No implementation is complete merely because it compiles and passes
tests. The code must be left in a state where a competent systems engineer
unfamiliar with EntropyFS can understand the purpose, algorithm,
invariants, failure modes, concurrency semantics, persistence semantics,
resource bounds, and measured rationale directly from the source.**

This is a permanent repository rule, not an incidental cleanup.

## 1. What "ultra-verbose" means

Comment the semantics, causality, constraints, alternatives, evidence, and
failure modes exhaustively. Do NOT narrate trivial Rust syntax.

Bad verbosity:

```rust
// Increment i by one.
i += 1;
```

Useful EntropyFS verbosity:

```rust
// Advance to the next descriptor only after the current descriptor has
// passed both structural validation and the materialization budget check.
//
// WHY THIS ORDER MATTERS:
//
// The descriptor bytes are persistent, therefore untrusted. In particular,
// a syntactically readable length field cannot be treated as an allocation
// authority. `Representation::validate()` establishes the representation's
// structural invariants first; the materializer then applies the
// independent runtime resource limits.
//
// Reversing those operations would allow a malformed-but-parseable
// descriptor to influence allocation before its semantic bounds had been
// established. The hostile-media court deliberately exercises this
// boundary.
//
// INVARIANT:
//     persistent bytes
//         -> bounded parse
//         -> structural validation
//         -> resource preflight
//         -> materialization
//
// Never move allocation ahead of validation without extending the
// hostile-media court to prove the replacement ordering safe.
```

## 2. Module-level documentation

Every substantial module begins with a large module-level explanation
covering:

```text
PURPOSE
    What subsystem responsibility lives here?

BOUNDARY
    What is this module allowed to know?
    What must it never know?

MODEL
    What conceptual model should a reader hold?

PERSISTENT AUTHORITY
    Does anything here affect on-disk semantics?

CORRECTNESS INVARIANTS
    What must always remain true?

CONCURRENCY
    What can run concurrently?
    Which locks exist?
    What is their ordering?

DURABILITY
    What does acknowledgement mean?
    What survives process crash?
    What survives power loss?

RESOURCE BOUNDS
    Which attacker-controlled sizes can reach this code?
    What bounds them?

PERFORMANCE
    Why is this implementation shaped this way?
    What evidence justified it?

FAILURE MODES
    What errors are expected?
    What must never happen?

HISTORY / EVIDENCE
    Which phase/court found the bugs that motivated unusual decisions?
```

Modules that are part of the persistent-data surface (the store, the
epoch, the transaction layer, the descriptor codec, the representation
algebra, GC, the read/materialization paths) MUST carry the full template.
Diagnostic-only and leaf modules may be lighter, but every module still
states its purpose and its invariants.

## 3. Type documentation

Types explain their ROLE and INVARIANTS, not merely their fields. A type
whose invariants are load-bearing carries the causal history of the bug
that established them — a future maintainer must understand why changing
one innocent field could corrupt a filesystem.

```rust
/// The process-visible mutation overlay between canonical checkpoints.
///
/// # Why this exists
///
/// Persisting every namespace or extent mutation directly into the
/// immutable B-tree DAG made small-file workloads pay the full COW/root-
/// publication cost per syscall. Phase 10D introduced the epoch as a
/// recoverable writeback layer: mutations become visible immediately
/// through this overlay, while canonical immutable trees are constructed
/// in batches.
///
/// # Two different kinds of state
///
/// An epoch contains:
///
/// 1. semantic overlay state — what concurrent readers must observe;
/// 2. mutation-log state — what recovery needs if the process dies before
///    the next checkpoint.
///
/// These are related but are not interchangeable. In particular, clearing
/// the live overlay merely because a checkpoint has taken a snapshot
/// creates a visibility hole. Phase 10G exposed exactly that bug under
/// parallel workloads.
///
/// # Sequence invariant
///
/// Mutation sequence numbers are globally monotonic across checkpoints.
/// They MUST NOT restart when an epoch is checkpointed. Recovery uses the
/// committed root's `log_seq` as a high-water mark, so reusing an earlier
/// sequence can make an acknowledged mutation appear older than the root
/// and be silently discarded.
///
/// ...
pub struct Epoch { ... }
```

## 4. Function documentation

Every nontrivial function answers "what / why / how / guarantees":

```rust
/// <one-line semantic summary>
///
/// # What
///
/// Exactly what transformation/operation this performs.
///
/// # Why
///
/// Why this operation exists in EntropyFS rather than only what Rust
/// code it executes.
///
/// # Inputs and authority
///
/// Which input is trusted, untrusted, persistent, derived, or advisory.
///
/// # Algorithm
///
/// Step-by-step conceptual flow.
///
/// # Invariants
///
/// Preconditions and postconditions.
///
/// # Concurrency
///
/// Locks required/forbidden, state snapshots, linearization point.
///
/// # Durability
///
/// Whether successful return means visible, process-crash-safe, or
/// power-durable.
///
/// # Resource bounds
///
/// Maximum allocations/work/reference traversal.
///
/// # Failure behavior
///
/// Typed errors and what must never occur.
///
/// # Evidence / rationale
///
/// Relevant phase, court, or ADR where a non-obvious decision originated.
```

Not literally every function needs all ten headings — a six-line helper
does not need a dissertation. But every architecturally significant
function must be understandable in isolation.

## 5. Inline comments explain state transitions

Complex systems functions number their algorithmic stages:

```rust
// ---------------------------------------------------------------------
// Stage 1: Snapshot the visible mutation overlay.
//
// We take the snapshot while holding the commit coordinator so that...
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// Stage 2: Merge the immutable committed tree with the snapshot.
//
// This happens without mutating the live epoch...
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// Stage 3: Publish the canonical root.
//
// This is the transaction's persistence linearization point...
// ---------------------------------------------------------------------
```

This makes 500-line systems functions dramatically easier to navigate.

## 6. Critical comments carry causal history

Bugs that look like arbitrary special cases today get the full story:

```rust
// SELF-ALIAS PROHIBITION
//
// The chunk index maps a logical content id to the descriptor capable of
// materializing that content. During an in-flight epoch, exact dedup may
// discover that the target bytes already exist and propose:
//
//     ExactRef { target: cid }
//
// That descriptor is a valid *reference from another logical extent*, but
// it cannot become the canonical chunk-index descriptor for `cid` itself.
//
// Doing so creates:
//
//     cid -> ExactRef(cid) -> ExactRef(cid) -> ...
//
// which is a semantic cycle. The depth limiter eventually rejects it, but
// the canonical terminal descriptor has already been displaced.
//
// Phase 10G found this under concurrent identical-content writes.
// Therefore pending chunk-index registration must retain a
// terminal/non-self descriptor for the content id even when individual
// extents use EXACT_REF aliases.
```

A short "Avoid self-reference." is not enough. The bug must never have to
be rediscovered.

## 7. Evidence-sensitive optimizations

Every optimization that would look bizarre without history preserves the
evidence that chose it:

```text
attempted:    <the rejected alternative>
measured:     <the numbers>
reason:       <why the alternative failed>
decision:     <what the code does>
limitation:   <what the code does not solve>
evidence:     <which oracle/court/A-B run>
```

Examples the standard covers (each already or to be commented in place):
why writeback-cache was removed after initially being negotiated; why
mutation-log sequence numbers never reset; why checkpoint snapshots are
compare-and-removed rather than `mem::take`d; why pending descriptors
resolve staged payloads; why read windows use an exclusive upper bound;
why predecessor inclusion is conditional; why GC uses physical scan
occupancy rather than object-index occupancy; why chunk-index rebuild uses
`bulk_load`; why DSFB has zero decoding authority; why `SequenceSharedDict`
anchors survive deletion; why reference depth is longest-path, not
visited-node depth; why rANS model cost must be included in stream
selection; why random data must converge to RAW; why background
optimization must CAS against the incumbent; why io_uring's unsafe surface
is isolated; why the durability barrier holds the commit lock; why
representation decoding validates before allocation; why semantic
hostile-media fuzz mutations recompute CRCs; why the worker pool uses
per-worker round-robin cursors and admits an oversized request at idle.

## 8. Units and accounting

Any performance/storage variable states its unit and whether it is:

```text
logical / reachable / physical / allocated
inclusive / exclusive
wall time / CPU sum / per-request / cumulative
monotonic / snapshot
```

The 11D oracle made this concrete: thread-CPU sum and wall time answer
very different questions, and the worker instrumentation documents that
distinction in place. Avoid `let cost = ...`; prefer a stronger name or
commentary — e.g. "marginal persisted bytes if this candidate wins,
excluding already-existing CAS objects, including newly introduced model
objects exactly once."

## 9. Do not erase historical comments when code changes

When a later implementation makes an old explanation obsolete, UPDATE the
explanation to describe the code that exists now — but preserve the
historically important rationale in the CHANGELOG / ADR / evidence. Code
comments describe the current code; the changelog and evidence describe
what existed, what failed, what replaced it, and why. That prevents
comments from becoming archaeological clutter while still preserving the
research record.

## 10. The repository-wide documentation invariant

> **README describes the present system. CHANGELOG preserves its temporal
> history (including the development phase ledger). Evidence proves
> measured claims. ADRs explain architectural decisions.**

- `README.md` — what EntropyFS is, why it exists, the architecture, the
  current high-level condition, how to use it, links outward. No
  historical phase table.
- `CHANGELOG.md` — what changed, when, why, the measured outcome,
  rejected approaches, and the complete phase history.
- `evidence/*` — receipts for measured claims.
- `docs/architecture/*` — enduring architecture/specification, including
  this standard.
