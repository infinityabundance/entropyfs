//! The materialization-graph court (Phase-11A, second target): fuzz the
//! descriptor GRAPH, not only individual descriptors.
//!
//! # Purpose
//!
//! Prove the bounded-valid-or-typed-rejection oracle over the
//! materialization GRAPH — the second hostile-media layer after the
//! descriptor codec. A representation bomb is inherently graph-shaped:
//! EXACT_REF, BASE_RESIDUAL, SequenceDict and SequenceSharedDict resolve
//! other chunks, and locally valid descriptors can compose into globally
//! invalid graphs (the Phase-10E diamond-depth bug is exactly this class).
//! The fuzz input defines:
//!
//! ```text
//! descriptor table:  id0 -> descriptor bytes, id1 -> descriptor bytes, …
//! object table:      obj0 -> bytes, obj1 -> bytes, …
//! entry descriptor:  idN
//! ```
//!
//! and the court materializes the entry through an in-memory hostile
//! resolver (`HostileResolver`) whose `fetch_descriptor` decodes hostile
//! bytes through the real codec — the same path a hostile store's chunk
//! index takes.
//!
//! # Boundary
//!
//! The court MAY feed any bounded byte string as a graph spec (descriptor
//! bytes, object payloads — rANS models and streams included — and the
//! entry id), and may mutate valid graph seeds byte-wise, noise-wise, or
//! by wholesale splice. It may NEVER assume that arbitrary data must be
//! rejected: some random inputs legitimately describe valid content, so
//! the default outcome class is bounded-valid OR typed-rejection
//! (`Either`), with `MustAccept`/`MustReject` asserted only where the
//! format fully determines the outcome. Note the contrast with the store
//! court: the graph-spec byte format has NO envelope CRC (it is a bare
//! fuzz surface), so byte mutations here reach the parser directly and no
//! CRC recomputation is needed — the CRC-aware distinction is the store
//! court's separate concern.
//!
//! # Model
//!
//! A graph spec is parsed by the lenient `parse_graph_spec` (any byte
//! string is a valid spec), loaded into `HostileResolver`, and the entry
//! descriptor is materialized with `materialize_to_vec` under the real
//! `Limits`. The oracle outcome is `GraphOutcome::Ok { len }` (bounded
//! success; `len` must equal the descriptor's declared length — never
//! silent wrong bytes) or `GraphOutcome::Rejected` (a typed
//! materialization error). The court NEVER asserts that arbitrary data
//! must be rejected — some random inputs legitimately describe valid
//! content. The valid seeds additionally pin the exact materialized
//! bytes (the §32 byte-exactness contract).
//!
//! # Resource bounds
//!
//! Termination is by construction: every materialize step spends the
//! work budget or returns; reference depth is capped; every allocation is
//! checked against `max_alloc_bytes` before it happens. This court proves
//! the budget counters hold against adversarial graphs (self-reference,
//! cycles, depth bombs, diamonds, shared-dict double branches, invalid
//! dictionary chains, corrupted model objects, hostile command streams).
//! The fuzz dimension itself is bounded: inputs cap at
//! `MAX_GRAPH_INPUT` (4096) bytes, the parser caps tables at
//! `GRAPH_MAX_TABLES` (32) entries, and every case runs under BOTH limit
//! sets (the tight set and the real defaults) so the guards hold at
//! constrained deployment sizes too.
//!
//! # Failure modes
//!
//! Expected: any typed rejection from the materializer (including
//! `MissingChunk` for an absent entry id) — the oracle's admissible
//! rejection arm. What must NEVER happen: panic, OOM, infinite loop,
//! unbounded recursion, unbounded CPU, or an `Ok` whose length differs
//! from the descriptor's declared length. A court `Err(description)`
//! names the violated invariant and becomes a test failure.
//!
//! # History / evidence
//!
//! Phase 11A (v0.7.0) introduced this court after the security
//! documentation claimed fuzz assurance the repository did not implement
//! (CHANGELOG.md 11A entry). Sealed evidence:
//! `evidence/hostile-media/court-1787750784-a2983dc/` — 200k graph cases
//! per proptest target in release mode, full suite green. The graph
//! exhibits descend from the Phase-10E diamond-depth bug (a node reached
//! shallow and deep must report the DEEPEST path — the materializer's
//! depth cap and `optimizer::rebase`'s longest-path walk are the fixes
//! this court keeps pinned) and the Phase-10G self-reference/cycle class
//! (EXACT_REF self-aliasing the chunk index; two chunks referencing each
//! other would loop to the depth cap).

#![forbid(unsafe_code)]

use proptest::prelude::*;

use crate::core::limits::Limits;
use crate::core::materialize::DecoderContext;
use crate::tests::hostile_media::corpus::{graph_seed_expectations, graph_seeds};
use crate::tests::hostile_media::{
    ExhibitKind, Expect, LIMIT_SETS, parse_graph_spec, run_graph_oracle, tight_limits,
};

/// The maximum fuzz-input size for the graph court. Each table entry costs
/// ≥33 bytes and the parser caps tables at 32 entries, so this bounds the
/// resolver's memory while leaving room for rich graphs.
///
/// # Units
///
/// Bytes (the flat graph-spec encoding — see `encode_graph_spec`). The
/// cap bounds both the fuzz dimension and every allocation the parser and
/// resolver derive from it: a hostile spec can never grow the court's own
/// memory beyond O(input).
const MAX_GRAPH_INPUT: usize = 4096;

// ---------------------------------------------------------------------------
// Deterministic tests
// ---------------------------------------------------------------------------

/// Every valid graph seed must materialize to its pinned bytes under the
/// default limits, and every structural bomb must be rejected typed.
///
/// # Why
///
/// The valid seeds are the §32 byte-exactness contract made executable:
/// they carry REAL rANS/sequence streams and their materialized bytes are
/// pinned in the corpus — if a future change to the materializer or the
/// codec silently re-interprets a descriptor, this test fails on the exact
/// bytes. The `MustReject` seeds pin the typed-rejection arm (depth bombs
/// at exactly 4/5, cycles, invalid chains, hostile streams).
#[test]
fn graph_seeds_materialize_to_pinned_content() {
    let limits = Limits::default();
    let expectations = graph_seed_expectations();
    let seeds = graph_seeds();
    assert_eq!(
        seeds.len(),
        expectations.len(),
        "every graph seed must have an expectation"
    );
    for (name, bytes) in &seeds {
        let (_, expect, expected) = expectations
            .iter()
            .find(|(n, _, _)| n == name)
            .unwrap_or_else(|| panic!("no expectation for seed {name}"));
        let spec = parse_graph_spec(bytes);
        let outcome = run_graph_oracle(&spec, &limits)
            .unwrap_or_else(|e| panic!("seed {name}: oracle invariant violated: {e}"));
        match expect {
            Expect::MustAccept => {
                let len = match outcome {
                    crate::tests::hostile_media::GraphOutcome::Ok { len } => len,
                    crate::tests::hostile_media::GraphOutcome::Rejected(e) => {
                        panic!("seed {name} must materialize, rejected: {e}")
                    }
                };
                let expected = expected.as_ref().expect("accepted seed pins content");
                assert_eq!(
                    len,
                    expected.len() as u64,
                    "seed {name}: materialized length differs"
                );
                // Re-materialize to compare bytes exactly.
                let resolver =
                    crate::tests::hostile_media::HostileResolver::from_spec(&spec, &limits);
                let entry = resolver
                    .fetch_descriptor(&spec.entry)
                    .expect("entry resolves");
                let out = crate::core::materialize::materialize_to_vec(&entry, &resolver, &limits)
                    .expect("materializes");
                assert_eq!(
                    &out, expected,
                    "seed {name}: materialized bytes differ from the pinned content"
                );
            }
            Expect::MustReject => {
                assert!(
                    matches!(
                        outcome,
                        crate::tests::hostile_media::GraphOutcome::Rejected(_)
                    ),
                    "seed {name} must be rejected typed"
                );
            }
            Expect::Either => unreachable!("graph seeds are accept or reject, not either"),
        }
    }
}

/// The same seeds under the tight limits: bounded-valid or typed-reject,
/// never a panic or an invariant violation (the tight limits must not
/// break the budget/depth/allocation guards).
///
/// # Why
///
/// A hostile or constrained deployment can mount with small limits
/// (`tight_limits` is the adversarial minimum: 16 KiB chunks, depth 2,
/// 1 MiB work budget, 64 KiB allocations). A materializer that only
/// behaves at default sizes is a bomb waiting for that deployment — every
/// guard must hold at both ends of the limit range.
#[test]
fn graph_seeds_bounded_under_tight_limits() {
    let limits = tight_limits();
    for (name, bytes) in graph_seeds() {
        let spec = parse_graph_spec(&bytes);
        run_graph_oracle(&spec, &limits)
            .unwrap_or_else(|e| panic!("seed {name}: tight oracle invariant violated: {e}"));
    }
}

/// Every graph exhibit under both limit sets: MustReject must reject,
/// MustAccept must accept, Either accepts both — and nothing may panic.
///
/// # Why
///
/// The exhibits are the hand-crafted adversarial canon (self-reference,
/// two-node cycles, 4/5-depth chains, diamonds, shared-dict double
/// branches, invalid dict chains, corrupted models, hostile command
/// streams at the exact end and one byte beyond). Running them under BOTH
/// limit sets pins that the guards hold at default AND constrained
/// sizes, and that the deterministic expectations never drift.
#[test]
fn graph_exhibits_pass() {
    for set in LIMIT_SETS {
        let limits = match set {
            "tight" => tight_limits(),
            _ => Limits::default(),
        };
        for ex in crate::tests::hostile_media::corpus::exhibits()
            .into_iter()
            .filter(|e| e.kind == ExhibitKind::Graph)
        {
            let spec = parse_graph_spec(&ex.bytes);
            let outcome = run_graph_oracle(&spec, &limits)
                .unwrap_or_else(|e| panic!("[{set}] exhibit {}: {e}", ex.name));
            match ex.expect {
                Expect::MustReject => {
                    assert!(
                        matches!(
                            outcome,
                            crate::tests::hostile_media::GraphOutcome::Rejected(_)
                        ),
                        "[{set}] exhibit {} must be rejected typed",
                        ex.name
                    );
                }
                Expect::MustAccept => {
                    assert!(
                        matches!(
                            outcome,
                            crate::tests::hostile_media::GraphOutcome::Ok { .. }
                        ),
                        "[{set}] exhibit {} must materialize boundedly",
                        ex.name
                    );
                }
                Expect::Either => {
                    // bounded-valid or typed-reject: either is admissible.
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fuzz targets (proptest)
// ---------------------------------------------------------------------------

/// Byte-level mutation over a graph-spec seed (reuses the descriptor
/// court's op set; a valid graph mutated byte-wise is the highest-value
/// corpus: the graph stays near-valid while fields drift hostile).
///
/// # Ops (op % 6)
///
/// 0 flip a byte (XOR with `b | 1`, so a flip always changes it);
/// 1 set a byte to `b`;
/// 2 insert a byte at a position;
/// 3 delete a byte;
/// 4 truncate from a position;
/// 5 overwrite a 1–8 byte window with an LCG-derived mask (a hostile
///   splice: ids and lengths get replaced wholesale).
///
/// All positions are derived modulo the current length, so no op can
/// panic; mutation count is capped by the strategy (0..=10 ops).
fn mutate_bytes(bytes: &mut Vec<u8>, ops: &[(u8, u8, u8)]) {
    for (op, a, b) in ops {
        let a = *a as usize;
        match op % 6 {
            0 => {
                if !bytes.is_empty() {
                    let i = a % bytes.len();
                    bytes[i] ^= b | 1;
                }
            }
            1 => {
                if !bytes.is_empty() {
                    let i = a % bytes.len();
                    bytes[i] = *b;
                }
            }
            2 => {
                let i = if bytes.is_empty() {
                    0
                } else {
                    a % (bytes.len() + 1)
                };
                bytes.insert(i, *b);
            }
            3 => {
                if !bytes.is_empty() {
                    let i = a % bytes.len();
                    bytes.remove(i);
                }
            }
            4 => {
                if !bytes.is_empty() {
                    let i = a % bytes.len();
                    bytes.truncate(i);
                }
            }
            _ => {
                if !bytes.is_empty() {
                    let start = a % bytes.len();
                    let mut rng = a as u64 ^ (*b as u64) << 8;
                    for k in start..bytes.len().min(start + 8) {
                        rng = rng
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        bytes[k] ^= (rng >> 32) as u8 | 1;
                    }
                }
            }
        }
    }
}

/// Start from a valid graph seed (a materializable graph with real
/// streams) and mutate 0..=10 bytes: the fuzzer stays near the valid
/// surface and drifts fields, ids, lengths, stream bytes hostile.
///
/// # Why near-valid is the highest-value corpus
///
/// Pure noise mostly fails at the descriptor codec's tag/length checks
/// long before deep logic; a mutated VALID graph keeps the structure
/// plausible while individual fields (ids, lengths, command bytes, stream
/// contents) drift hostile — that is what reaches the deep materializer
/// arms and the budget/depth/allocation counters this court exists to
/// prove.
fn mutated_graph_strategy() -> impl Strategy<Value = Vec<u8>> {
    let seeds = graph_seeds();
    let seed_bytes: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    prop::sample::select(seed_bytes).prop_flat_map(|seed| {
        prop::collection::vec(any::<(u8, u8, u8)>(), 0..=10).prop_map(move |ops| {
            let mut bytes = seed.clone();
            mutate_bytes(&mut bytes, &ops);
            bytes
        })
    })
}

/// Uniform noise over the graph-spec byte format: the parser must accept
/// any byte string, and the materializer must stay bounded on any
/// resulting graph.
///
/// # Why
///
/// The lenient parser's contract is "any byte string is a valid spec" —
/// so uniform noise is the parser's own fuzz surface (truncation
/// mid-structure, absurd counts, absent entry ids all resolve to typed
/// rejections or bounded results). This is the court's lower bound on
/// how adversarial the format can get.
fn noise_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=MAX_GRAPH_INPUT)
}

/// Valid graph seeds with a hostile splice (bytes overwritten, not just
/// flipped — ids and lengths get replaced wholesale).
///
/// # Why a separate strategy
///
/// Flip/set mutations change one byte at a time; a splice replaces a
/// whole window (up to 32 bytes) with arbitrary blob bytes. That is how
/// a 32-byte content id, a u32 length field, or a sequence of command
/// bytes gets REPLACED rather than nudged — the hostile-store mutation
/// class where a field's semantic identity changes completely.
fn spliced_graph_strategy() -> impl Strategy<Value = Vec<u8>> {
    let seeds = graph_seeds();
    let seed_bytes: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    prop::sample::select(seed_bytes).prop_flat_map(|seed| {
        (
            0..seed.len(),
            1usize..32,
            prop::collection::vec(any::<u8>(), 0..=32),
        )
            .prop_map(move |(start, len, blob)| {
                let mut bytes = seed.clone();
                let end = (start + len).min(bytes.len());
                if end > start {
                    bytes.splice(start..end, blob);
                }
                bytes
            })
    })
}

proptest! {
    /// Mutated valid graphs: bounded-valid or typed-reject, never a
    /// panic, never an invariant violation.
    ///
    /// Both limit sets (`tight` and `default`) run per case: a guard that
    /// only holds at default sizes is a bomb for a constrained
    /// deployment. The oracle's `Err` arm reports the violated invariant
    /// as the panic message, so a regression names itself.
    #[test]
    fn mutated_graphs_oracle(bytes in mutated_graph_strategy()) {
        for set in LIMIT_SETS {
            let limits = match set { "tight" => tight_limits(), _ => Limits::default() };
            let spec = parse_graph_spec(&bytes);
            run_graph_oracle(&spec, &limits)
                .unwrap_or_else(|e| panic!("[{set}] mutated graph {} bytes: {e}", bytes.len()));
        }
    }

    /// Uniform noise graph specs.
    ///
    /// The parser's lenient contract under adversarial input: every byte
    /// string parses, and materialization of whatever graph results is
    /// bounded or typed-rejected under both limit sets.
    #[test]
    fn noise_graphs_oracle(bytes in noise_strategy()) {
        for set in LIMIT_SETS {
            let limits = match set { "tight" => tight_limits(), _ => Limits::default() };
            let spec = parse_graph_spec(&bytes);
            run_graph_oracle(&spec, &limits)
                .unwrap_or_else(|e| panic!("[{set}] noise graph {} bytes: {e}", bytes.len()));
        }
    }

    /// Spliced graphs (id/length replacement wholesale).
    ///
    /// The mutation class where fields change IDENTITY rather than drift:
    /// a spliced content id re-targets a reference to a different (or
    /// absent) chunk, a spliced length shifts every parse after it, and a
    /// spliced command window rewrites the materialization program
    /// mid-stream. All must stay within the oracle under both limit sets.
    #[test]
    fn spliced_graphs_oracle(bytes in spliced_graph_strategy()) {
        for set in LIMIT_SETS {
            let limits = match set { "tight" => tight_limits(), _ => Limits::default() };
            let spec = parse_graph_spec(&bytes);
            run_graph_oracle(&spec, &limits)
                .unwrap_or_else(|e| panic!("[{set}] spliced graph {} bytes: {e}", bytes.len()));
        }
    }
}
