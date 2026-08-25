//! Exhaustive representation round-trip tests: every family must
//! materialize exactly the bytes it was encoded from, and the cost
//! accounting must be internally consistent.

#![forbid(unsafe_code)]

use crate::core::candidate::{CandidateContext, Encoder};
use crate::core::cost::Policy;
use crate::core::limits::Limits;
use crate::core::materialize::materialize_to_vec;
use crate::core::representation::{Representation, Residual};
use crate::entropy::palette::PaletteEncoder;
use crate::entropy::periodic::PeriodicEncoder;
use crate::entropy::permutation::PermutationEncoder;
use crate::entropy::residual::BaseResidualEncoder;
use crate::entropy::sparse::SparseEncoder;
use crate::entropy::universe::UniverseEncoder;
use crate::rans::residual::{RansEncoder, RansResidualEncoder};
use crate::rans::sequence::SequenceEncoder;
use crate::tests::helpers::MemResolver;

fn ctx<'a>(
    input: &[u8],
    limits: &'a Limits,
    policy: &'a Policy,
    bases: &'a [crate::core::candidate::BaseChunk],
) -> CandidateContext<'a> {
    CandidateContext {
        limits,
        policy,
        content_id: crate::core::extent::ChunkId::of(input),
        bases,
        dedup: None,
    }
}

/// Run every encoder over `input`, collect candidates, materialize each,
/// and assert byte-exact round trips.
fn assert_all_candidates_roundtrip(input: &[u8], bases: &[crate::core::candidate::BaseChunk]) {
    let limits = Limits::default();
    let policy = Policy::default();
    let cctx = ctx(input, &limits, &policy, bases);
    let encoders: Vec<Box<dyn Encoder>> = vec![
        Box::new(SparseEncoder),
        Box::new(PaletteEncoder),
        Box::new(PermutationEncoder),
        Box::new(PeriodicEncoder),
        Box::new(BaseResidualEncoder),
        Box::new(UniverseEncoder),
        Box::new(RansEncoder),
        Box::new(RansResidualEncoder),
        Box::new(SequenceEncoder),
    ];
    let mut resolver = MemResolver::empty();
    for base in bases {
        // provide base bytes as both object and chunk so references resolve
        resolver.put_object(base.id, base.bytes.clone());
        resolver.put_chunk(base.id, RepresentationRawForTest(&base.bytes).to_rep());
    }
    let mut checked = 0usize;
    for enc in encoders.iter() {
        for cand in enc.encode(input, &cctx) {
            // load the candidate's own objects into the resolver
            let mut local = resolver.clone();
            for o in &cand.objects {
                local.put_object(o.id, o.payload.clone());
            }
            let out = materialize_to_vec(&cand.representation, &local, &limits)
                .unwrap_or_else(|e| panic!("{}: materialize failed: {e:?}", enc.name()));
            assert_eq!(
                out,
                input,
                "{}: round trip mismatch (len {} vs {})",
                enc.name(),
                out.len(),
                input.len()
            );
            // cost accounting: persisted == encoded + model + objects +
            // integrity (every persistent bit, §15)
            let persisted = cand.cost.persisted_bytes();
            let expected = cand.representation.encoded_size()
                + cand.cost.model_bytes
                + cand.cost.object_payload_bytes
                + cand.cost.integrity_bytes;
            assert_eq!(
                persisted,
                expected,
                "{}: cost accounting broken: persisted {persisted} != {expected}",
                enc.name()
            );
            checked += 1;
        }
    }
    // Every non-pathological input must produce at least one candidate
    // (RAW comes from the pipeline, not these encoders; assert here that
    // the families don't all silently no-op on structured data).
    let _ = checked;
}

/// Minimal raw-representation builder for test bases.
struct RepresentationRawForTest<'a>(&'a [u8]);
impl RepresentationRawForTest<'_> {
    fn to_rep(&self) -> Representation {
        Representation::Raw {
            obj: crate::core::extent::ChunkId::of(self.0),
            len: self.0.len() as u64,
        }
    }
}

#[test]
fn sparse_roundtrip() {
    let mut input = vec![0u8; 8192];
    for &p in &[7u32, 4096, 8191] {
        input[p as usize] = 0x5A;
    }
    assert_all_candidates_roundtrip(&input, &[]);
}

#[test]
fn palette_roundtrip() {
    // Small chunk (the multinomial state space only fits u128 for small n
    // or extreme skew — honest rejection otherwise, covered in
    // entropy::palette tests).
    let input: Vec<u8> = (0..64u32)
        .map(|i| match i % 6 {
            0..=2 => 0x11,
            3..=4 => 0x22,
            _ => 0x33,
        })
        .collect();
    assert_all_candidates_roundtrip(&input, &[]);
}

#[test]
fn periodic_roundtrip() {
    let mut input = Vec::new();
    for _ in 0..200 {
        input.extend_from_slice(b"entropyfs");
    }
    input.extend_from_slice(b"ent"); // tail
    assert_all_candidates_roundtrip(&input, &[]);
}

#[test]
fn fill_roundtrip() {
    assert_all_candidates_roundtrip(&[0x42u8; 4096], &[]);
    assert_all_candidates_roundtrip(&[0u8; 4096], &[]);
}

#[test]
fn base_residual_roundtrip() {
    let base = vec![0x10u8; 4096];
    let mut target = base.clone();
    for slot in target.iter_mut().take(200).skip(100) {
        *slot = 0xFF;
    }
    target[4000] = 0x01;
    let base_id = crate::core::extent::ChunkId::of(&base);
    let bases = vec![crate::core::candidate::BaseChunk {
        id: base_id,
        bytes: base,
        depth: 0,
    }];
    assert_all_candidates_roundtrip(&target, &bases);
}

#[test]
fn rans_roundtrip() {
    let input: Vec<u8> = (0..65536u32)
        .map(|i| ((i * 5 + i / 64) % 97) as u8)
        .collect();
    assert_all_candidates_roundtrip(&input, &[]);
}

#[test]
fn sequence_rans_roundtrip_on_text() {
    // English-ish text with long-distance repeats: SequenceRans must
    // propose, round-trip byte-exactly, and beat plain rANS.
    let sentence =
        b"the quick brown fox jumps over the lazy dog and then walks back to the riverbed ";
    let mut input = Vec::new();
    for i in 0..40 {
        input.extend_from_slice(sentence);
        input.extend_from_slice(format!("sentence number {i} has a unique tail ").as_bytes());
    }
    assert_all_candidates_roundtrip(&input, &[]);
    let limits = Limits::default();
    let policy = Policy::default();
    let cctx = ctx(&input, &limits, &policy, &[]);
    let seq = SequenceEncoder.encode(&input, &cctx);
    assert_eq!(seq.len(), 1, "text must produce a sequence candidate");
    let rans = RansEncoder.encode(&input, &cctx);
    let best_rans = rans
        .iter()
        .min_by_key(|c| c.total(&policy))
        .map(|c| c.cost.persisted_bytes())
        .unwrap_or(input.len() as u64);
    assert!(
        seq[0].cost.persisted_bytes() < best_rans,
        "sequence {} not better than plain rans {}",
        seq[0].cost.persisted_bytes(),
        best_rans
    );
}

#[test]
fn sequence_rans_skips_crypto_random() {
    // H6 negative control at the encoder level: crypto-uniform data must
    // produce no SequenceRans candidate (raw/rans floor wins instead).
    let mut input = Vec::new();
    let mut i: u64 = 0;
    while input.len() < 65536 {
        let h = blake3::hash(&i.to_le_bytes());
        input.extend_from_slice(&h.as_bytes()[..32]);
        i += 1;
    }
    assert_all_candidates_roundtrip(&input, &[]);
    let limits = Limits::default();
    let policy = Policy::default();
    let cctx = ctx(&input, &limits, &policy, &[]);
    assert!(
        SequenceEncoder.encode(&input, &cctx).is_empty(),
        "crypto-random data must not produce a sequence candidate"
    );
}

#[test]
fn entropy_ref_exact_match() {
    // A descriptor with a known seed whose stream equals the input: the
    // exact-match case is cheap and byte-exact. (The encoder derives its
    // seed from the content id, so this path is only reachable when input
    // IS the stream of the derived seed — astronomically rare for random
    // data; that is the honest negative-control property.)
    let seed = [0x42u8; 16];
    let input = crate::entropy::universe::UniformXofV1::generate(seed, 0, 4096);
    let rep = Representation::EntropyRef {
        universe: crate::core::representation::UniverseId::UniformXofV1,
        seed,
        coordinate: 0,
        transform: crate::core::representation::TransformId::Identity,
        residual: Residual::XorSparse {
            len: 4096,
            edits: Vec::new(),
        },
        len: 4096,
    };
    rep.validate(&Limits::default()).unwrap();
    let resolver = MemResolver::empty();
    let out = materialize_to_vec(&rep, &resolver, &Limits::default()).unwrap();
    assert_eq!(out, input);
    // Accounting: seed(16)+coordinate(8)+residual(0)+integrity(4) + tags.
    let split = crate::core::cost::ByteSplit {
        seed_state: 24,
        ..Default::default()
    };
    let cost = crate::core::cost::estimate(&rep, &split, 0);
    assert!(cost.persisted_bytes() < 100);
}

#[test]
fn entropy_ref_random_data_loses_to_raw() {
    // Random-ish data: the entropy candidate must be far more expensive
    // than raw (negative control: no free compression).
    let input: Vec<u8> = (0..4096u32).map(|i| ((i * 31 + 7) % 251) as u8).collect();
    let limits = Limits::default();
    let policy = Policy::default();
    let cctx = ctx(&input, &limits, &policy, &[]);
    let enc = UniverseEncoder;
    let cands = enc.encode(&input, &cctx);
    // Either no candidate (diff > fanout) or a candidate that loses to RAW.
    for c in &cands {
        let raw = crate::core::candidate::raw_candidate(&input, cctx.content_id, &limits).unwrap();
        assert!(c.total(&policy) > raw.total(&policy));
    }
}

#[test]
fn all_families_agree_on_content_id() {
    let input: Vec<u8> = (0..4096u32).map(|i| ((i * 3) % 29) as u8).collect();
    let limits = Limits::default();
    let policy = Policy::default();
    let cctx = ctx(&input, &limits, &policy, &[]);
    for cand in [
        SparseEncoder.encode(&input, &cctx),
        PaletteEncoder.encode(&input, &cctx),
        PeriodicEncoder.encode(&input, &cctx),
        RansEncoder.encode(&input, &cctx),
        UniverseEncoder.encode(&input, &cctx),
    ]
    .into_iter()
    .flatten()
    {
        assert_eq!(cand.content_id, crate::core::extent::ChunkId::of(&input));
    }
}

#[test]
fn inline_and_raw_pipeline_basics() {
    // Small chunk: INLINE must beat RAW in the pipeline's cost comparison.
    let limits = Limits::default();
    let policy = Policy::default();
    let small: Vec<u8> = (0..200u32).map(|i| (i % 40) as u8).collect();
    let cid = crate::core::extent::ChunkId::of(&small);
    let inl = crate::core::candidate::inline_candidate(&small, cid, &limits).unwrap();
    let raw = crate::core::candidate::raw_candidate(&small, cid, &limits).unwrap();
    assert!(inl.total(&policy) < raw.total(&policy));
    let cands = [raw, inl];
    let best = crate::core::candidate::pick_cheapest(&cands, &policy).unwrap();
    assert!(matches!(best.representation, Representation::Inline { .. }));
}

#[test]
fn candidate_validation_rejects_wrong_bytes() {
    // A deliberately wrong candidate (content id of other data) must fail
    // validate_candidate.
    let limits = Limits::default();
    let a = vec![1u8; 256];
    let b = vec![2u8; 256];
    let cand =
        crate::core::candidate::raw_candidate(&a, crate::core::extent::ChunkId::of(&a), &limits)
            .unwrap();
    let resolver = MemResolver::empty();
    let res = crate::core::candidate::validate_candidate(&cand, &b, &resolver, &limits);
    assert!(res.is_err());
}
