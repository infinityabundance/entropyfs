//! The materialization-graph court (Phase-11A, second target): fuzz the
//! descriptor GRAPH, not only individual descriptors.
//!
//! A representation bomb is inherently graph-shaped: EXACT_REF,
//! BASE_RESIDUAL, SequenceDict and SequenceSharedDict resolve other
//! chunks, and locally valid descriptors can compose into globally invalid
//! graphs (the Phase-10E diamond-depth bug is exactly this class). The
//! fuzz input defines:
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
//! The oracle: materialization either succeeds within all declared
//! resource bounds, or returns a typed error. We NEVER assert that
//! arbitrary data must be rejected — some random inputs legitimately
//! describe valid content. The valid seeds additionally pin the exact
//! materialized bytes (the §32 byte-exactness contract).
//!
//! Termination is by construction: every materialize step spends the
//! work budget or returns; reference depth is capped; every allocation is
//! checked against `max_alloc_bytes` before it happens. This court proves
//! the budget counters hold against adversarial graphs (self-reference,
//! cycles, depth bombs, diamonds, shared-dict double branches, invalid
//! dictionary chains, corrupted model objects, hostile command streams).

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
const MAX_GRAPH_INPUT: usize = 4096;

// ---------------------------------------------------------------------------
// Deterministic tests
// ---------------------------------------------------------------------------

/// Every valid graph seed must materialize to its pinned bytes under the
/// default limits, and every structural bomb must be rejected typed.
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
fn noise_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=MAX_GRAPH_INPUT)
}

/// Valid graph seeds with a hostile splice (bytes overwritten, not just
/// flipped — ids and lengths get replaced wholesale).
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
