//! The descriptor-decode court (Phase-11A, first target): every bounded
//! byte string through `format::descriptor::decode` under deliberately
//! tight limits AND the real defaults.
//!
//! The oracle (`run_descriptor_oracle`) is the user-specified contract:
//!
//! - decode-OK ⇒ `rep.validate(&limits)` must succeed (enforced inside
//!   `decode` itself since Phase-11A; asserted here so a regression of the
//!   codec's own gate fails the court);
//! - encoded size must remain within the descriptor cap;
//! - every derived size stays within the declared bounds;
//! - the encoding is canonical: re-encoding the decoded representation
//!   reproduces the exact input bytes (so a decodable input is byte-exact
//!   round-trippable — no silent normalization, no trailing ambiguity);
//! - never panic, never OOM (allocations are input-bounded by the
//!   descriptor cap before `Vec` grows).
//!
//! Strategy mix (proptest): uniform noise (every possible bounded byte
//! string), seeded mutation of one real descriptor of every family (the
//! fuzzer penetrates deep variant-specific logic instead of spending all
//! day discovering valid tags and lengths), and seeds plus trailing
//! garbage. Deterministic companions: truncation at every byte boundary of
//! every canonical descriptor, and the 8192/8193 descriptor-cap boundary.

#![forbid(unsafe_code)]

use proptest::prelude::*;

use crate::core::limits::Limits;
use crate::format::descriptor;
use crate::tests::hostile_media::corpus::{descriptor_seeds, exhibits};
use crate::tests::hostile_media::{ExhibitKind, Expect, LIMIT_SETS, tight_limits};

/// The maximum fuzz-input size for the descriptor court: `decode` rejects
/// inputs longer than `max_descriptor_bytes` instantly, so inputs beyond
/// the descriptor cap add nothing but noise.
const MAX_FUZZ_INPUT: usize = 1024;

/// The descriptor court oracle. Returns `Err(description)` on any
/// invariant violation; the courts turn that into a test failure.
pub fn run_descriptor_oracle(bytes: &[u8], limits: &Limits) -> Result<(), String> {
    let decoded = descriptor::decode(bytes, limits);
    match decoded {
        Err(_e) => Ok(()), // typed rejection is an admissible outcome
        Ok(rep) => {
            // Decode-OK implies structural validation OK (the codec's own
            // gate since Phase-11A; asserted so a regression fails here).
            rep.validate(limits).map_err(|e| {
                format!(
                    "decode-ok but validate failed: {e:?} (input {} bytes, {})",
                    bytes.len(),
                    hex_tail(bytes)
                )
            })?;
            // Encoded size within the descriptor cap.
            if rep.encoded_size() > limits.max_descriptor_bytes {
                return Err(format!(
                    "decode-ok but encoded_size {} exceeds the {} descriptor cap",
                    rep.encoded_size(),
                    limits.max_descriptor_bytes
                ));
            }
            // Logical length within the chunk cap.
            if rep.len() > limits.max_chunk_size {
                return Err(format!(
                    "decode-ok but len {} exceeds the {} chunk cap",
                    rep.len(),
                    limits.max_chunk_size
                ));
            }
            // Canonical form: re-encoding reproduces the exact input.
            let re = descriptor::encode(&rep).map_err(|e| format!("re-encode failed: {e:?}"))?;
            if re != bytes {
                return Err(format!(
                    "decode is not canonical: re-encode ({} bytes) differs from the input ({} bytes)",
                    re.len(),
                    bytes.len()
                ));
            }
            Ok(())
        }
    }
}

/// Short hex tail of an input, for failure messages.
fn hex_tail(bytes: &[u8]) -> String {
    let n = bytes.len().min(16);
    let mut s = String::with_capacity(n * 2);
    for b in &bytes[..n] {
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > n {
        s.push('…');
    }
    s
}

/// The limits for a named limit set.
pub fn limits_for(set: &str) -> Limits {
    match set {
        "tight" => tight_limits(),
        _ => Limits::default(),
    }
}

// ---------------------------------------------------------------------------
// Deterministic tests
// ---------------------------------------------------------------------------

/// Every family seed decodes, validates, and re-encodes byte-exactly under
/// the default limits (the corpus contract).
#[test]
fn seeds_are_canonical_and_valid() {
    let limits = Limits::default();
    let seeds = descriptor_seeds();
    assert!(
        seeds.len() >= 20,
        "one descriptor of every family + every residual kind (got {})",
        seeds.len()
    );
    for (name, bytes) in &seeds {
        run_descriptor_oracle(bytes, &limits).unwrap_or_else(|e| panic!("seed {name}: {e}"));
    }
}

/// Under the tight limits the seeds must decode-or-reject typed (a
/// tight-mount rejection of an over-cap descriptor is correct behavior),
/// and never panic.
#[test]
fn seeds_bounded_under_tight_limits() {
    let limits = tight_limits();
    for (name, bytes) in descriptor_seeds() {
        let r = descriptor::decode(&bytes, &limits);
        if let Ok(rep) = r {
            rep.validate(&limits)
                .unwrap_or_else(|e| panic!("seed {name}: tight validate failed: {e:?}"));
            let re = descriptor::encode(&rep).expect("re-encode");
            assert_eq!(re, bytes, "seed {name}: not canonical under tight limits");
        }
    }
}

/// Truncation at every byte boundary of every canonical descriptor must
/// fail with a typed error — never panic, never silently succeed on a
/// prefix.
#[test]
fn truncation_at_every_boundary_of_every_seed() {
    let limits = Limits::default();
    let seeds = descriptor_seeds();
    let mut checked = 0usize;
    for (name, bytes) in &seeds {
        for cut in 0..bytes.len() {
            assert!(
                descriptor::decode(&bytes[..cut], &limits).is_err(),
                "seed {name}: cut at {cut} of {} decoded successfully",
                bytes.len()
            );
            checked += 1;
        }
    }
    // The corpus must be non-trivial: the truncation sweep has to exercise
    // every seed.
    assert!(checked >= 20 * 5, "truncation sweep too small: {checked}");
}

/// The descriptor-cap boundary: a descriptor of exactly 8192 bytes decodes
/// under the default cap; 8193 bytes is rejected typed.
#[test]
fn descriptor_cap_boundary() {
    let limits = Limits::default();
    // Exactly 8192 bytes: SPARSE with k = 8167 literals (encoded size
    // 5 + 4 + 16 + 8167 = 8192), len 8192, rank 0.
    let mut ok = vec![0x07u8, 0x00, 0x20, 0x00, 0x00];
    ok.extend_from_slice(&8167u32.to_le_bytes());
    ok.extend_from_slice(&0u128.to_le_bytes());
    ok.extend_from_slice(&vec![0xABu8; 8167]);
    assert_eq!(ok.len(), 8192);
    run_descriptor_oracle(&ok, &limits)
        .unwrap_or_else(|e| panic!("8192-byte descriptor must decode: {e}"));
    // 8193 bytes must be rejected at the entry length gate.
    let mut over = ok.clone();
    over.push(0x00);
    assert_eq!(over.len(), 8193);
    assert!(
        descriptor::decode(&over, &limits).is_err(),
        "8193-byte descriptor must be rejected"
    );
}

/// Every descriptor exhibit under both limit sets: MustReject exhibits
/// must fail, MustAccept must succeed, Either accepts both — and nothing
/// may panic.
#[test]
fn descriptor_exhibits_pass() {
    for set in LIMIT_SETS {
        let limits = limits_for(set);
        for ex in exhibits()
            .into_iter()
            .filter(|e| e.kind == ExhibitKind::Descriptor)
        {
            let outcome = run_descriptor_oracle(&ex.bytes, &limits);
            match ex.expect {
                Expect::MustReject => {
                    assert!(
                        descriptor::decode(&ex.bytes, &limits).is_err(),
                        "[{set}] exhibit {} must be rejected",
                        ex.name
                    );
                }
                Expect::MustAccept => {
                    outcome.unwrap_or_else(|e| panic!("[{set}] exhibit {}: {e}", ex.name));
                }
                Expect::Either => {
                    let _ = outcome; // bounded-valid or typed-reject: either is admissible
                }
            }
        }
    }
}

/// Hand-assembled boundary bytes that must never panic, exercised directly
/// (the exhibit runner covers them; this is the explicit no-panic sweep).
#[test]
fn exhibits_never_panic() {
    for ex in exhibits() {
        if ex.kind != ExhibitKind::Descriptor {
            continue;
        }
        let _ = descriptor::decode(&ex.bytes, &Limits::default());
        let _ = descriptor::decode(&ex.bytes, &tight_limits());
    }
}

// ---------------------------------------------------------------------------
// Fuzz targets (proptest; the in-package coverage-guided harness — ADR-0001
// keeps one package, so the driver is proptest rather than a `fuzz/`
// Cargo package; `PROPTEST_CASES` scales the run for the release court).
// ---------------------------------------------------------------------------

/// One byte-level mutation op over a seed.
fn apply_op(bytes: &mut Vec<u8>, op: u8, a: usize, b: u8) {
    match op % 6 {
        0 => {
            // flip one byte
            if !bytes.is_empty() {
                let i = a % bytes.len();
                bytes[i] ^= b | 1;
            }
        }
        1 => {
            // set one byte
            if !bytes.is_empty() {
                let i = a % bytes.len();
                bytes[i] = b;
            }
        }
        2 => {
            // insert a byte
            let i = if bytes.is_empty() {
                0
            } else {
                a % (bytes.len() + 1)
            };
            bytes.insert(i, b);
        }
        3 => {
            // delete a byte
            if !bytes.is_empty() {
                let i = a % bytes.len();
                bytes.remove(i);
            }
        }
        4 => {
            // truncate
            if !bytes.is_empty() {
                let i = a % bytes.len();
                bytes.truncate(i);
            }
        }
        _ => {
            // overwrite a small range with pseudo-random bytes
            if !bytes.is_empty() {
                let start = a % bytes.len();
                let mut rng = a as u64 ^ (b as u64) << 8;
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

/// A strategy that starts from a real family seed and applies 0..=8 random
/// mutation ops (flip/set/insert/delete/truncate/overwrite).
fn mutated_seed_strategy() -> impl Strategy<Value = Vec<u8>> {
    let seeds = descriptor_seeds();
    let seed_bytes: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    prop::sample::select(seed_bytes).prop_flat_map(|seed| {
        (
            prop::collection::vec(any::<(u8, u8, u8)>(), 0..=8),
            proptest::bool::ANY,
        )
            .prop_map(move |(ops, append_garbage)| {
                let mut bytes = seed.clone();
                for (op, a, b) in ops {
                    apply_op(&mut bytes, op, a as usize, b);
                }
                if append_garbage && !bytes.is_empty() {
                    // splice a random blob into the middle (not just the
                    // tail — mid-stream garbage tests the length fields).
                    let i = (bytes.len() / 2).min(bytes.len() - 1);
                    bytes.splice(i..i, vec![0xAA, 0x55, 0xFF, 0x00, 0x42]);
                }
                bytes
            })
    })
}

/// Uniform noise: every possible bounded byte string.
fn noise_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=MAX_FUZZ_INPUT)
}

/// A valid seed with a trailing garbage blob (the `r.done()` gate).
fn trailing_garbage_strategy() -> impl Strategy<Value = Vec<u8>> {
    let seeds = descriptor_seeds();
    let seed_bytes: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    (
        prop::sample::select(seed_bytes),
        prop::collection::vec(any::<u8>(), 1..=16),
    )
        .prop_map(|(mut seed, garbage)| {
            seed.extend_from_slice(&garbage);
            seed
        })
}

/// A random sub-slice of a seed (cut into the middle of the fields).
fn slice_strategy() -> impl Strategy<Value = Vec<u8>> {
    let seeds = descriptor_seeds();
    let seed_bytes: Vec<Vec<u8>> = seeds.iter().map(|(_, b)| b.clone()).collect();
    prop::sample::select(seed_bytes).prop_flat_map(|seed| {
        (0usize..seed.len(), 0usize..seed.len())
            .prop_map(move |(a, b)| seed[a.min(b)..a.max(b)].to_vec())
    })
}

proptest! {
    /// Every bounded byte string must satisfy the descriptor oracle under
    /// BOTH the tight and the default limit sets — never panic, and any
    /// decodable input is canonical and validate-OK.
    #[test]
    fn uniform_noise_oracle(bytes in noise_strategy()) {
        for set in LIMIT_SETS {
            let limits = limits_for(set);
            run_descriptor_oracle(&bytes, &limits)
                .unwrap_or_else(|e| panic!("[{set}] noise {} bytes: {e}", bytes.len()));
        }
    }

    /// Mutated family seeds: 0..=8 byte-level ops over one real descriptor
    /// of every family (the corpus-penetration target).
    #[test]
    fn mutated_seeds_oracle(bytes in mutated_seed_strategy()) {
        for set in LIMIT_SETS {
            let limits = limits_for(set);
            run_descriptor_oracle(&bytes, &limits)
                .unwrap_or_else(|e| panic!("[{set}] mutated {} bytes: {e}", bytes.len()));
        }
    }

    /// Valid seeds plus trailing garbage: the `r.done()` gate must reject
    /// (typed), never panic.
    #[test]
    fn trailing_garbage_oracle(bytes in trailing_garbage_strategy()) {
        for set in LIMIT_SETS {
            let limits = limits_for(set);
            assert!(
                descriptor::decode(&bytes, &limits).is_err(),
                "[{set}] trailing garbage decoded: {} bytes",
                bytes.len()
            );
        }
    }

    /// Random sub-slices of real seeds (truncation inside field data).
    #[test]
    fn slice_oracle(bytes in slice_strategy()) {
        for set in LIMIT_SETS {
            let limits = limits_for(set);
            run_descriptor_oracle(&bytes, &limits)
                .unwrap_or_else(|e| panic!("[{set}] slice {} bytes: {e}", bytes.len()));
        }
    }
}
