//! The permanent adversarial corpus: one canonical descriptor of every
//! representation family (the fuzz seeds), plus hand-crafted exhibits for
//! every boundary the format defines (the permanent court exhibits).
//!
//! Corpus discipline (Phase-11A):
//! - every family seed decodes, validates, and re-encodes byte-exactly;
//! - graph seeds with real rANS/sequence streams materialize to known
//!   bytes through the hostile resolver (the valid-path oracle);
//! - exhibits never assert "must reject" for random data — only where the
//!   format's outcome is fully determined; the default oracle is
//!   `Either` (bounded-valid or typed-reject);
//! - all bytes are deterministic (fixed seeds; no wall clock).

#![forbid(unsafe_code)]

use crate::core::candidate::{CandidateContext, Encoder, ObjectRecord};
use crate::core::cost::Policy;
use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::representation::{
    Edit, RangeChange, RansCodec, Representation, Residual, TransformId, UniverseId,
};
use crate::format::descriptor;
use crate::tests::hostile_media::seeded_bytes;
use crate::tests::hostile_media::{Exhibit, ExhibitKind, Expect, GraphSpec, encode_graph_spec};

/// A family seed: a name plus the encoded descriptor bytes.
pub type FamilySeed = (String, Vec<u8>);

/// A graph seed: a name plus the encoded graph-spec bytes.
pub type GraphSeed = (String, Vec<u8>);

/// Stable synthetic ids for corpus entries (content-addressed names; the
/// exact values do not matter as long as they are deterministic and
/// collision-free within a seed).
fn id(name: &[u8]) -> ChunkId {
    ChunkId::of(name)
}

/// The entry descriptor id used by graph seeds.
fn entry_id() -> ChunkId {
    id(b"entry")
}

// ---------------------------------------------------------------------------
// Valid-path building blocks
// ---------------------------------------------------------------------------

/// Text with long-distance repeats (the SequenceRans showcase).
fn text_chunk(n: usize) -> Vec<u8> {
    let sentence =
        b"the quick brown fox jumps over the lazy dog and then walks back to the riverbed ";
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while out.len() < n {
        out.extend_from_slice(sentence);
        out.extend_from_slice(format!("sentence number {i} has a unique tail ").as_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// A 64 KiB pattern dictionary (u16-addressable).
fn dict_chunk() -> Vec<u8> {
    let mut out = Vec::with_capacity(65536);
    let pattern: Vec<u8> = (0..7u32).map(|i| (i * 37 % 251) as u8).collect();
    while out.len() < 65536 {
        let take = (65536 - out.len()).min(pattern.len());
        out.extend_from_slice(&pattern[..take]);
    }
    out
}

/// Structured data with real rANS compressibility.
fn compressible(n: usize) -> Vec<u8> {
    (0..n as u32).map(|i| ((i * 13) % 53) as u8).collect()
}

/// Deterministic noise (no structure; RAW stays the winner).
#[allow(dead_code)] // kept for the evidence corpus builders
fn noise(n: usize) -> Vec<u8> {
    seeded_bytes(n, 0x243F_6A88_85A3_08D3)
}

/// Encode N raw streams into a model object + enc object + per-stream
/// encoded lengths (the sequence families' shared machinery).
fn encode_streams(streams: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>, Vec<u32>) {
    let enc = crate::rans::sequence::encode_streams_n(streams)
        .expect("streams must encode for a valid seed");
    (enc.model_obj, enc.enc_obj, enc.lens)
}

/// The sequence families' shared (scale_bits, codec).
fn sequence_scale_codec() -> (u8, RansCodec) {
    crate::rans::sequence::sequence_scale_codec()
}

/// Run one encoder over `input` and take its single candidate.
fn one_candidate(
    enc: &dyn Encoder,
    input: &[u8],
    limits: &Limits,
) -> (Representation, Vec<ObjectRecord>) {
    let ctx = CandidateContext {
        limits,
        policy: &Policy::default(),
        content_id: ChunkId::of(input),
        bases: &[],
        dedup: None,
    };
    let mut cands = enc.encode(input, &ctx);
    assert!(
        !cands.is_empty(),
        "seed encoder {} must produce a candidate ({} input bytes)",
        enc.name(),
        input.len()
    );
    let c = cands.remove(0);
    (c.representation, c.objects)
}

/// Build a graph spec whose entry is a single representation, with its
/// own objects in the object table (ids are the content-addressed object
/// ids, so the materializer resolves them).
fn spec_with_objects(rep: &Representation, objects: &[ObjectRecord]) -> GraphSpec {
    let mut spec = GraphSpec::new(entry_id());
    spec.add_desc(
        entry_id(),
        descriptor::encode(rep).expect("valid seed encodes"),
    );
    for o in objects {
        spec.add_obj(o.id, o.payload.clone());
    }
    spec
}

/// Add a chunk descriptor + its payload object to a spec (a referenced
/// chunk resolved through the descriptor table).
fn add_chunk(spec: &mut GraphSpec, cid: ChunkId, bytes: &[u8]) {
    let rep = Representation::Raw {
        obj: cid,
        len: bytes.len() as u64,
    };
    spec.add_desc(cid, descriptor::encode(&rep).expect("raw chunk encodes"));
    spec.add_obj(cid, bytes.to_vec());
}

// ---------------------------------------------------------------------------
// 1. Canonical family seeds (one descriptor per family; BASE_RESIDUAL for
//    every residual kind). These are the fuzz corpus seeds: the mutation
//    strategies start from them so the fuzzer penetrates deep
//    variant-specific logic instead of spending all day discovering valid
//    tags and lengths.
// ---------------------------------------------------------------------------

/// One real encoded descriptor of every representation family.
#[allow(clippy::vec_init_then_push)] // a push-per-exhibit builder by design
pub fn descriptor_seeds() -> Vec<FamilySeed> {
    let o = id(b"object");
    let mut v: Vec<FamilySeed> = Vec::with_capacity(20);
    v.push(("zero".into(), enc(&Representation::Zero { len: 65536 })));
    v.push((
        "fill".into(),
        enc(&Representation::Fill {
            value: 7,
            len: 1024,
        }),
    ));
    v.push((
        "inline".into(),
        enc(&Representation::Inline {
            data: b"hello entropy".to_vec(),
        }),
    ));
    v.push((
        "raw".into(),
        enc(&Representation::Raw { obj: o, len: 65536 }),
    ));
    v.push((
        "rans".into(),
        enc(&Representation::Rans {
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            len: 4096,
        }),
    ));
    v.push((
        "exact_ref".into(),
        enc(&Representation::ExactRef {
            target: o,
            off: 100,
            len: 512,
        }),
    ));
    v.push((
        "base_residual_xor".into(),
        enc(&Representation::BaseResidual {
            base: o,
            base_len: 4096,
            residual: Residual::XorSparse {
                len: 4096,
                edits: vec![
                    Edit { pos: 1, val: 2 },
                    Edit {
                        pos: 300,
                        val: 0xAA,
                    },
                ],
            },
            len: 4096,
        }),
    ));
    v.push((
        "base_residual_range".into(),
        enc(&Representation::BaseResidual {
            base: o,
            base_len: 64,
            residual: Residual::RangeReplace {
                len: 64,
                changes: vec![RangeChange { start: 4, end: 10 }],
                literals: vec![9; 6],
            },
            len: 64,
        }),
    ));
    v.push((
        "base_residual_rans_coded".into(),
        enc(&Representation::BaseResidual {
            base: o,
            base_len: 4096,
            residual: Residual::RansCoded {
                len: 4096,
                enc_obj: o,
                model: o,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                decoded_len: 4096,
            },
            len: 4096,
        }),
    ));
    v.push((
        "base_residual_base_sequence".into(),
        enc(&Representation::BaseResidual {
            base: o,
            base_len: 64,
            residual: Residual::BaseSequence {
                len: 64,
                enc_obj: o,
                model: o,
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                seq_len: 10,
                lit_len: 5,
                off_len: 8,
                cmds: 4,
                lit_out: 3,
            },
            len: 64,
        }),
    ));
    v.push((
        "sparse".into(),
        enc(&Representation::Sparse {
            k: 2,
            rank: 17,
            literals: vec![1, 2],
            len: 64,
        }),
    ));
    v.push((
        "palette".into(),
        enc(&Representation::Palette {
            palette: vec![0x10, 0x20, 0x30],
            counts: vec![40, 20, 4],
            rank: 5,
            len: 64,
        }),
    ));
    v.push((
        "periodic".into(),
        enc(&Representation::Periodic {
            period: 4,
            pattern: b"abcd".to_vec(),
            count: 3,
            tail: b"xy".to_vec(),
            len: 14,
        }),
    ));
    v.push((
        "entropy_ref".into(),
        enc(&Representation::EntropyRef {
            universe: UniverseId::UniformXofV1,
            seed: [3u8; 16],
            coordinate: 9,
            transform: TransformId::Identity,
            residual: Residual::XorSparse {
                len: 64,
                edits: Vec::new(),
            },
            len: 64,
        }),
    ));
    v.push((
        "permutation".into(),
        enc(&Representation::Permutation {
            rank: 42,
            alphabet: (200u8..230).collect(),
            len: 30,
        }),
    ));
    v.push((
        "sequence_rans".into(),
        enc(&Representation::SequenceRans {
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 100,
            lit_len: 50,
            off_len: 20,
            cmds: 30,
            lit_out: 40,
            len: 4096,
        }),
    ));
    v.push((
        "sparse_block64".into(),
        enc(&Representation::SparseBlock64 {
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            pc_len: 100,
            rank_len: 60,
            lit_len: 20,
            words: 512,
            nonzero: 7,
            lit_out: 9,
            len: 4096,
        }),
    ));
    v.push((
        "sequence_dict".into(),
        enc(&Representation::SequenceDict {
            dictionary: o,
            dictionary_len: 65536,
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 100,
            lit_len: 50,
            off_len: 20,
            src_len: 10,
            cmds: 30,
            lit_out: 40,
            len: 4096,
        }),
    ));
    v.push((
        "sequence_shared_dict".into(),
        enc(&Representation::SequenceSharedDict {
            dictionary: ChunkId::ZERO,
            dictionary_len: 0,
            shared: o,
            shared_len: 65536,
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 100,
            lit_len: 50,
            off_len: 20,
            src_len: 10,
            cmds: 30,
            lit_out: 40,
            len: 4096,
        }),
    ));
    v.push((
        "sequence_deep".into(),
        enc(&Representation::SequenceDeep {
            model: o,
            enc_obj: o,
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 100,
            lit_len: 50,
            off_len: 20,
            len_len: 10,
            cmds: 30,
            lit_out: 40,
            len: 4096,
        }),
    ));
    v
}

fn enc(rep: &Representation) -> Vec<u8> {
    descriptor::encode(rep).expect("seed encodes")
}

// ---------------------------------------------------------------------------
// 2. Graph seeds: valid graphs (entry descriptor + tables) that
//    materialize to KNOWN bytes through the hostile resolver, plus the
//    structural bombs (self-reference, cycles, depth chains, diamonds).
// ---------------------------------------------------------------------------

/// Build the valid graph seeds with real streams.
pub fn graph_seeds() -> Vec<GraphSeed> {
    let limits = Limits::default();
    let mut out: Vec<GraphSeed> = Vec::new();

    // Zero / fill / inline: no objects.
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(entry_id(), enc(&Representation::Zero { len: 256 }));
        out.push(("zero".into(), encode_graph_spec(&spec)));
    }
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Fill {
                value: 0x42,
                len: 256,
            }),
        );
        out.push(("fill".into(), encode_graph_spec(&spec)));
    }
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Inline {
                data: b"inline payload bytes for the hostile resolver".to_vec(),
            }),
        );
        out.push(("inline".into(), encode_graph_spec(&spec)));
    }

    // Raw: entry references an object that exists in the table.
    {
        let payload = compressible(256);
        let oid = ChunkId::of(&payload);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(entry_id(), enc(&Representation::Raw { obj: oid, len: 256 }));
        spec.add_obj(oid, payload);
        out.push(("raw".into(), encode_graph_spec(&spec)));
    }

    // Sparse: rank computed so materialization is well-defined.
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Sparse {
                k: 3,
                rank: crate::entropy::rank::rank_comb_subset(&[5, 100, 200], 256).expect("rank"),
                literals: vec![1, 2, 3],
                len: 256,
            }),
        );
        out.push(("sparse".into(), encode_graph_spec(&spec)));
    }

    // Palette.
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Palette {
                palette: vec![10, 20, 30],
                counts: vec![3, 3, 2],
                rank: 5,
                len: 8,
            }),
        );
        out.push(("palette".into(), encode_graph_spec(&spec)));
    }

    // Periodic.
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Periodic {
                period: 4,
                pattern: vec![1, 2, 3, 4],
                count: 3,
                tail: vec![5, 6],
                len: 14,
            }),
        );
        out.push(("periodic".into(), encode_graph_spec(&spec)));
    }

    // Permutation.
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Permutation {
                rank: 5,
                alphabet: vec![10, 20, 30, 40],
                len: 4,
            }),
        );
        out.push(("permutation".into(), encode_graph_spec(&spec)));
    }

    // EntropyRef (real universe materialization; no objects).
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::EntropyRef {
                universe: UniverseId::UniformXofV1,
                seed: [3u8; 16],
                coordinate: 9,
                transform: TransformId::Identity,
                residual: Residual::XorSparse {
                    len: 64,
                    edits: Vec::new(),
                },
                len: 64,
            }),
        );
        out.push(("entropy_ref".into(), encode_graph_spec(&spec)));
    }

    // RANS with a real model + encoded stream.
    {
        let input = compressible(4096);
        let (rep, objects) = one_candidate(&crate::rans::residual::RansEncoder, &input, &limits);
        let spec = spec_with_objects(&rep, &objects);
        out.push(("rans".into(), encode_graph_spec(&spec)));
    }

    // SEQUENCE_RANS with real streams.
    {
        let input = text_chunk(4096);
        let (rep, objects) =
            one_candidate(&crate::rans::sequence::SequenceEncoder, &input, &limits);
        let spec = spec_with_objects(&rep, &objects);
        out.push(("sequence_rans".into(), encode_graph_spec(&spec)));
    }

    // SPARSE_BLOCK64 with real streams on sparse data (k in (9, n/2): the
    // encoder declines k <= 9 as SPARSE territory and k >= n/2 as dense).
    {
        let mut input = vec![0u8; 65536];
        for i in 0..200usize {
            // One nonzero per distinct 8-byte word, spread out.
            input[i * 8] = (i % 251) as u8 + 1;
        }
        let (rep, objects) = one_candidate(
            &crate::entropy::sparse64::SparseBlock64Encoder,
            &input,
            &limits,
        );
        let spec = spec_with_objects(&rep, &objects);
        out.push(("sparse_block64".into(), encode_graph_spec(&spec)));
    }

    // SEQUENCE_DICT: real streams + a dictionary chunk in the tables.
    {
        let dict = dict_chunk();
        let mut input = dict.clone();
        input[100] ^= 0x5A;
        input[65535] ^= 0x01;
        let enc = crate::rans::sequence::SequenceDictEncoder {
            dictionary: ChunkId::of(&dict),
            dict_bytes: dict.clone(),
            dict_depth: 0,
        };
        let (rep, objects) = one_candidate(&enc, &input, &limits);
        let mut spec = spec_with_objects(&rep, &objects);
        add_chunk(&mut spec, ChunkId::of(&dict), &dict);
        out.push(("sequence_dict".into(), encode_graph_spec(&spec)));
    }

    // SEQUENCE_SHARED_DICT: real streams + the shared dictionary chunk.
    {
        let shared = dict_chunk();
        let mut input = shared.clone();
        for i in (0..65536).step_by(17) {
            input[i] ^= 0x03;
        }
        let enc = crate::rans::sequence::SequenceSharedDictEncoder {
            dictionary: ChunkId::ZERO,
            dict_bytes: Vec::new(),
            dict_depth: 0,
            shared: ChunkId::of(&shared),
            shared_bytes: shared.clone(),
            shared_depth: 0,
        };
        let (rep, objects) = one_candidate(&enc, &input, &limits);
        let mut spec = spec_with_objects(&rep, &objects);
        add_chunk(&mut spec, ChunkId::of(&shared), &shared);
        out.push(("sequence_shared_dict".into(), encode_graph_spec(&spec)));
    }

    // SEQUENCE_DEEP with real streams on repetitive data.
    {
        let pattern: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 8) as u8)
            .collect();
        let mut input = Vec::new();
        while input.len() < 65536 {
            input.extend_from_slice(&pattern);
        }
        input.truncate(65536);
        let (rep, objects) =
            one_candidate(&crate::rans::sequence::SequenceDeepEncoder, &input, &limits);
        let spec = spec_with_objects(&rep, &objects);
        out.push(("sequence_deep".into(), encode_graph_spec(&spec)));
    }

    // BASE_RESIDUAL with a XorSparse residual over a base chunk.
    {
        let base_id = id(b"base");
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(base_id, enc(&Representation::Zero { len: 64 }));
        spec.add_desc(
            entry_id(),
            enc(&Representation::BaseResidual {
                base: base_id,
                base_len: 64,
                residual: Residual::XorSparse {
                    len: 64,
                    edits: vec![Edit { pos: 0, val: 0xFF }, Edit { pos: 63, val: 0x01 }],
                },
                len: 64,
            }),
        );
        out.push(("base_residual_xor".into(), encode_graph_spec(&spec)));
    }

    // BASE_RESIDUAL with a RangeReplace residual.
    {
        let base_id = id(b"base");
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(base_id, enc(&Representation::Fill { value: 1, len: 32 }));
        spec.add_desc(
            entry_id(),
            enc(&Representation::BaseResidual {
                base: base_id,
                base_len: 32,
                residual: Residual::RangeReplace {
                    len: 32,
                    changes: vec![
                        RangeChange { start: 4, end: 8 },
                        RangeChange { start: 16, end: 18 },
                    ],
                    literals: vec![9, 9, 9, 9, 7, 7],
                },
                len: 32,
            }),
        );
        out.push(("base_residual_range".into(), encode_graph_spec(&spec)));
    }

    // BASE_RESIDUAL with a RansCoded residual (real streams).
    {
        let base = vec![0u8; 8192];
        let mut input = vec![0u8; 8192];
        for (i, slot) in input.iter_mut().enumerate() {
            *slot = (i % 9) as u8;
        }
        let base_id = ChunkId::of(&base);
        let base_chunk = crate::core::candidate::BaseChunk {
            id: base_id,
            bytes: base.clone(),
            depth: 0,
        };
        let ctx = CandidateContext {
            limits: &limits,
            policy: &Policy::default(),
            content_id: ChunkId::of(&input),
            bases: std::slice::from_ref(&base_chunk),
            dedup: None,
        };
        let cands = crate::rans::residual::RansResidualEncoder.encode(&input, &ctx);
        assert_eq!(
            cands.len(),
            1,
            "rans-residual seed must produce a candidate"
        );
        let cand = cands.into_iter().next().expect("candidate");
        let mut spec = spec_with_objects(&cand.representation, &cand.objects);
        spec.add_desc(base_id, enc(&Representation::Zero { len: 8192 }));
        out.push(("base_residual_rans_coded".into(), encode_graph_spec(&spec)));
    }

    // BASE_RESIDUAL with a BaseSequence residual: a literal-run command
    // stream over real rANS streams (the copy/literal recipe).
    {
        let base_id = id(b"base");
        let commands = vec![0x3Fu8]; // literal run of 64
        let literals: Vec<u8> = (0..64u8).collect();
        let offsets: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(base_id, enc(&Representation::Fill { value: 0, len: 64 }));
        spec.add_desc(
            entry_id(),
            enc(&Representation::BaseResidual {
                base: base_id,
                base_len: 64,
                residual: Residual::BaseSequence {
                    len: 64,
                    enc_obj: enc_id,
                    model: model_id,
                    scale_bits,
                    codec,
                    seq_len: lens[0],
                    lit_len: lens[1],
                    off_len: lens[2],
                    cmds: 1,
                    lit_out: 64,
                },
                len: 64,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        out.push((
            "base_residual_base_sequence".into(),
            encode_graph_spec(&spec),
        ));
    }

    // EXACT_REF chains: depth exactly at the cap (accepts) and beyond
    // (rejects).
    {
        // chain of length 4: A -> B -> C -> D -> Fill (all len 64).
        let ids: Vec<ChunkId> = (0..5u8).map(|i| id(&[i; 1])).collect();
        let mut spec = GraphSpec::new(ids[0]);
        for i in 0..4 {
            spec.add_desc(
                ids[i],
                enc(&Representation::ExactRef {
                    target: ids[i + 1],
                    off: 0,
                    len: 64,
                }),
            );
        }
        spec.add_desc(ids[4], enc(&Representation::Fill { value: 9, len: 64 }));
        out.push(("chain_depth_4".into(), encode_graph_spec(&spec)));
    }
    {
        // chain of length 5: must hit DepthExceeded at cap 4.
        let ids: Vec<ChunkId> = (0..6u8).map(|i| id(&[0x10 + i; 1])).collect();
        let mut spec = GraphSpec::new(ids[0]);
        for i in 0..5 {
            spec.add_desc(
                ids[i],
                enc(&Representation::ExactRef {
                    target: ids[i + 1],
                    off: 0,
                    len: 64,
                }),
            );
        }
        spec.add_desc(ids[5], enc(&Representation::Fill { value: 9, len: 64 }));
        out.push(("chain_depth_5".into(), encode_graph_spec(&spec)));
    }

    // SELF-REFERENCE: A -> A (the depth cap turns it into a typed error).
    {
        let a = id(b"self");
        let mut spec = GraphSpec::new(a);
        spec.add_desc(
            a,
            enc(&Representation::ExactRef {
                target: a,
                off: 0,
                len: 64,
            }),
        );
        out.push(("self_reference".into(), encode_graph_spec(&spec)));
    }

    // TWO-NODE CYCLE: A -> B -> A.
    {
        let a = id(b"cycle-a");
        let b = id(b"cycle-b");
        let mut spec = GraphSpec::new(a);
        spec.add_desc(
            a,
            enc(&Representation::ExactRef {
                target: b,
                off: 0,
                len: 64,
            }),
        );
        spec.add_desc(
            b,
            enc(&Representation::ExactRef {
                target: a,
                off: 0,
                len: 64,
            }),
        );
        out.push(("two_node_cycle".into(), encode_graph_spec(&spec)));
    }

    // DEPTH BOMB: a 20-long chain (far past the cap).
    {
        let ids: Vec<ChunkId> = (0..21u8).map(|i| id(&[0x40 + i; 1])).collect();
        let mut spec = GraphSpec::new(ids[0]);
        for i in 0..20 {
            spec.add_desc(
                ids[i],
                enc(&Representation::ExactRef {
                    target: ids[i + 1],
                    off: 0,
                    len: 64,
                }),
            );
        }
        spec.add_desc(ids[20], enc(&Representation::Fill { value: 3, len: 64 }));
        out.push(("depth_bomb_20".into(), encode_graph_spec(&spec)));
    }

    // DIAMOND (materializer view): the same terminal node is reachable
    // shallow (via the shared branch) and deep (via the dictionary
    // branch) of one SEQUENCE_SHARED_DICT. The materializer walks both
    // branches depth-bounded; each individual path stays within the cap.
    {
        // entry = SharedDict{dictionary: A, shared: B}; A -> C -> X,
        // B -> X, X -> Fill. Dict branch reaches X at depth 3; the shared
        // branch reaches X at depth 2.
        let entry = id(b"diamond-entry");
        let a = id(b"diamond-a");
        let b = id(b"diamond-b");
        let c = id(b"diamond-c");
        let x = id(b"diamond-x");
        let mut spec = GraphSpec::new(entry);
        spec.add_desc(
            a,
            enc(&Representation::ExactRef {
                target: c,
                off: 0,
                len: 64,
            }),
        );
        spec.add_desc(
            c,
            enc(&Representation::ExactRef {
                target: x,
                off: 0,
                len: 64,
            }),
        );
        spec.add_desc(
            b,
            enc(&Representation::ExactRef {
                target: x,
                off: 0,
                len: 64,
            }),
        );
        spec.add_desc(x, enc(&Representation::Fill { value: 5, len: 64 }));
        // SEQUENCE_SHARED_DICT needs real streams (its command stream can
        // be a single literal run; both dictionaries are present but the
        // walk never copies from them).
        let commands = vec![0x3Fu8];
        let literals: Vec<u8> = vec![7; 64];
        let offsets: Vec<u8> = Vec::new();
        let sources: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals, offsets, sources]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        spec.add_desc(
            entry,
            enc(&Representation::SequenceSharedDict {
                dictionary: a,
                dictionary_len: 64,
                shared: b,
                shared_len: 64,
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                src_len: lens[3],
                cmds: 1,
                lit_out: 64,
                len: 64,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        out.push(("diamond_shallow_then_deep".into(), encode_graph_spec(&spec)));
    }

    // SHARED-DICT DOUBLE BRANCHES: both branches present, the file
    // dictionary chained through another shared-dict (a nested dictionary
    // graph with its own real streams).
    {
        let entry = id(b"db-entry");
        let d1 = id(b"db-d1");
        let d2 = id(b"db-d2");
        let s1 = id(b"db-s1");
        let s2 = id(b"db-s2");
        let mut spec = GraphSpec::new(entry);
        // file dict chain: d1 -> SharedDict{d2, s2} (real streams: a
        // literal-run command stream; both branches resolve but are not
        // copied from); d2 -> Fill; s2 -> Fill.
        let (m1, e1, l1) = encode_streams(&[vec![0x3Fu8], vec![9u8; 64], Vec::new(), Vec::new()]);
        let (scale_bits, codec) = sequence_scale_codec();
        let m1_id = ChunkId::of(&m1);
        let e1_id = ChunkId::of(&e1);
        spec.add_desc(
            d1,
            enc(&Representation::SequenceSharedDict {
                dictionary: ChunkId::ZERO,
                dictionary_len: 0,
                shared: s2,
                shared_len: 64,
                model: m1_id,
                enc_obj: e1_id,
                scale_bits,
                codec,
                seq_len: l1[0],
                lit_len: l1[1],
                off_len: l1[2],
                src_len: l1[3],
                cmds: 1,
                lit_out: 64,
                len: 64,
            }),
        );
        spec.add_obj(m1_id, m1);
        spec.add_obj(e1_id, e1);
        spec.add_desc(d2, enc(&Representation::Fill { value: 2, len: 64 }));
        spec.add_desc(s2, enc(&Representation::Fill { value: 3, len: 64 }));
        spec.add_desc(s1, enc(&Representation::Fill { value: 4, len: 64 }));
        // entry: SharedDict{dictionary: d1, shared: s1} with a literal-run
        // command stream (real streams; the references resolve but are not
        // copied from).
        let commands = vec![0x3Fu8];
        let literals: Vec<u8> = vec![8; 64];
        let offsets: Vec<u8> = Vec::new();
        let sources: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals, offsets, sources]);
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        spec.add_desc(
            entry,
            enc(&Representation::SequenceSharedDict {
                dictionary: d1,
                dictionary_len: 64,
                shared: s1,
                shared_len: 64,
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                src_len: lens[3],
                cmds: 1,
                lit_out: 64,
                len: 64,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        out.push((
            "shared_dict_double_branches".into(),
            encode_graph_spec(&spec),
        ));
    }

    out
}

// ---------------------------------------------------------------------------
// 3. Hand-crafted adversarial exhibits
// ---------------------------------------------------------------------------

/// Expected outcome + known content for every graph seed (keyed by the
/// seed name). `expected = Some(bytes)` pins the materialized bytes
/// exactly; `None` means the seed is a structural bomb whose only contract
/// is the typed rejection / boundedness in `expect`.
pub fn graph_seed_expectations() -> Vec<(String, Expect, Option<Vec<u8>>)> {
    let mut v: Vec<(String, Expect, Option<Vec<u8>>)> = Vec::new();
    let ok = |v: &mut Vec<(String, Expect, Option<Vec<u8>>)>, name: &str, bytes: Vec<u8>| {
        v.push((name.to_string(), Expect::MustAccept, Some(bytes)));
    };
    let bomb = |v: &mut Vec<(String, Expect, Option<Vec<u8>>)>, name: &str| {
        v.push((name.to_string(), Expect::MustReject, None));
    };

    ok(&mut v, "zero", vec![0u8; 256]);
    ok(&mut v, "fill", vec![0x42u8; 256]);
    ok(
        &mut v,
        "inline",
        b"inline payload bytes for the hostile resolver".to_vec(),
    );
    ok(&mut v, "raw", compressible(256));
    {
        let mut bytes = vec![0u8; 256];
        bytes[5] = 1;
        bytes[100] = 2;
        bytes[200] = 3;
        ok(&mut v, "sparse", bytes);
    }
    {
        // rank 5 of multinomial(8, [3,3,2]) mapped through [10,20,30].
        let seq = crate::entropy::rank::unrank_multinomial(5, 8, &[3, 3, 2]).expect("unrank");
        ok(
            &mut v,
            "palette",
            seq.iter().map(|&s| [10u8, 20, 30][s as usize]).collect(),
        );
    }
    {
        let mut bytes = vec![1, 2, 3, 4];
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        bytes.extend_from_slice(&[5, 6]);
        ok(&mut v, "periodic", bytes);
    }
    {
        let seq = crate::entropy::rank::unrank_permutation(5, 4).expect("unrank");
        ok(
            &mut v,
            "permutation",
            seq.iter()
                .map(|&i| [10u8, 20, 30, 40][i as usize])
                .collect(),
        );
    }
    ok(
        &mut v,
        "entropy_ref",
        crate::entropy::universe::UniformXofV1::materialize_range([3u8; 16], 9, 0..64),
    );
    ok(&mut v, "rans", compressible(4096));
    ok(&mut v, "sequence_rans", text_chunk(4096));
    {
        let mut bytes = vec![0u8; 65536];
        for i in 0..200usize {
            bytes[i * 8] = (i % 251) as u8 + 1;
        }
        ok(&mut v, "sparse_block64", bytes);
    }
    {
        let dict = dict_chunk();
        let mut bytes = dict.clone();
        bytes[100] ^= 0x5A;
        bytes[65535] ^= 0x01;
        ok(&mut v, "sequence_dict", bytes);
    }
    {
        let shared = dict_chunk();
        let mut bytes = shared.clone();
        for i in (0..65536).step_by(17) {
            bytes[i] ^= 0x03;
        }
        ok(&mut v, "sequence_shared_dict", bytes);
    }
    {
        let pattern: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 8) as u8)
            .collect();
        let mut bytes = Vec::new();
        while bytes.len() < 65536 {
            bytes.extend_from_slice(&pattern);
        }
        bytes.truncate(65536);
        ok(&mut v, "sequence_deep", bytes);
    }
    {
        let mut bytes = vec![0u8; 64];
        bytes[0] = 0xFF;
        bytes[63] = 0x01;
        ok(&mut v, "base_residual_xor", bytes);
    }
    {
        let mut bytes = vec![1u8; 32];
        bytes[4..8].copy_from_slice(&[9; 4]);
        bytes[16..18].copy_from_slice(&[7; 2]);
        ok(&mut v, "base_residual_range", bytes);
    }
    ok(
        &mut v,
        "base_residual_rans_coded",
        (0..8192u32).map(|i| (i % 9) as u8).collect(),
    );
    ok(&mut v, "base_residual_base_sequence", (0..64u8).collect());
    ok(&mut v, "chain_depth_4", vec![9u8; 64]);
    ok(&mut v, "diamond_shallow_then_deep", vec![7u8; 64]);
    ok(&mut v, "shared_dict_double_branches", vec![8u8; 64]);

    bomb(&mut v, "chain_depth_5");
    bomb(&mut v, "self_reference");
    bomb(&mut v, "two_node_cycle");
    bomb(&mut v, "depth_bomb_20");
    v
}

/// The permanent adversarial exhibit corpus (descriptor-level + graph-level).
pub fn exhibits() -> Vec<Exhibit> {
    let mut v: Vec<Exhibit> = Vec::new();
    v.extend(descriptor_exhibits());
    v.extend(graph_exhibits());
    v
}

/// Descriptor-level boundary exhibits. `default_expect` is asserted under
/// the DEFAULT limits; under the tight set the runner treats each exhibit
/// as `Either` (a tight-mount rejection of an over-cap descriptor is
/// itself correct behavior) except where the exhibit is explicitly
/// constructed to exercise the tight set.
fn descriptor_exhibits() -> Vec<Exhibit> {
    let mut v: Vec<Exhibit> = Vec::new();

    // ---- descriptor size boundaries ----
    // A SPARSE descriptor whose encoded size is exactly 8192 bytes: a
    // 8192-byte logical chunk with k = 8167 nonzero bytes (rank 0 is valid
    // since C(8192, 8167) ≫ u128).
    {
        let mut bytes = vec![0x07u8, 0x00, 0x20, 0x00, 0x00]; // tag SPARSE, len 8192
        bytes.extend_from_slice(&8167u32.to_le_bytes()); // k
        bytes.extend_from_slice(&0u128.to_le_bytes()); // rank
        bytes.extend_from_slice(&vec![0xABu8; 8167]); // literals
        assert_eq!(bytes.len(), 8192, "exactly 8192 bytes");
        v.push(Exhibit::new(
            "desc_exact_8192",
            bytes,
            ExhibitKind::Descriptor,
            Expect::Either, // accepts under default limits; TooLong under tight
        ));
    }
    {
        let mut bytes = vec![0x07u8, 0x00, 0x20, 0x00, 0x00];
        bytes.extend_from_slice(&8168u32.to_le_bytes());
        bytes.extend_from_slice(&0u128.to_le_bytes());
        bytes.extend_from_slice(&vec![0xABu8; 8168]);
        assert_eq!(bytes.len(), 8193, "exactly 8193 bytes");
        v.push(Exhibit::new(
            "desc_exact_8193",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject, // 8193 > 8192 cap: TooLong under any set
        ));
    }

    // ---- unknown tags ----
    v.push(Exhibit::new(
        "desc_unknown_rep_tag",
        vec![0x12, 0x00, 0x00, 0x00, 0x00],
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    // unknown residual kind inside BASE_RESIDUAL
    {
        let mut bytes = vec![0x06u8, 0x40, 0x00, 0x00, 0x00]; // BASE_RESIDUAL len 64
        bytes.extend_from_slice(&[0u8; 32]); // base id
        bytes.extend_from_slice(&64u32.to_le_bytes()); // base_len
        bytes.push(0x05); // unknown residual kind
        v.push(Exhibit::new(
            "desc_unknown_residual_tag",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject,
        ));
    }
    // unknown codec tag inside RANS
    {
        let mut bytes = vec![0x04u8, 0x00, 0x10, 0x00, 0x00]; // RANS len 4096
        bytes.extend_from_slice(&[0u8; 32]); // model
        bytes.extend_from_slice(&[0u8; 32]); // enc
        bytes.push(14); // scale_bits
        bytes.push(0x03); // unknown codec
        v.push(Exhibit::new(
            "desc_unknown_codec_tag",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject,
        ));
    }

    // ---- valid descriptor plus trailing garbage ----
    for (name, seed) in descriptor_seeds() {
        let mut bytes = seed.clone();
        bytes.push(0xAA);
        v.push(Exhibit::new(
            format!("desc_trailing_garbage_{name}"),
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject, // r.done() fails
        ));
    }

    // ---- logical output length boundaries ----
    v.push(Exhibit::new(
        "desc_len_max_chunk",
        enc(&Representation::Zero {
            len: Limits::default().max_chunk_size,
        }),
        ExhibitKind::Descriptor,
        Expect::Either, // accepts under default; TooLong under tight
    ));
    v.push(Exhibit::new(
        "desc_len_max_chunk_plus1",
        enc(&Representation::Zero {
            len: Limits::default().max_chunk_size + 1,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    {
        // len field = u32::MAX
        let mut bytes = vec![0x01u8];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        v.push(Exhibit::new(
            "desc_len_u32max",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject,
        ));
    }

    // ---- SPARSE boundaries ----
    v.push(Exhibit::new(
        "sparse_k_gt_len",
        sparse_bytes(5, 4, 0, &[1, 2, 3, 4, 5]),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SparseKTooLarge
    ));
    v.push(Exhibit::new(
        "sparse_lit_count_mismatch",
        sparse_bytes(2, 4, 0, &[1, 2, 3]), // k claims 2, 3 literal bytes → trailing byte
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "sparse_rank_exact_comb",
        sparse_bytes(3, 8, 56, &[1, 2, 3]), // C(8,3) = 56 → rank out of range
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "sparse_rank_u128max_small",
        sparse_bytes(3, 8, u128::MAX, &[1, 2, 3]),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "sparse_rank_u128max_large",
        sparse_bytes(4096, 8192, u128::MAX, &vec![1; 4096]),
        ExhibitKind::Descriptor,
        Expect::MustReject, // comb overflow / rank out of range
    ));

    // ---- PALETTE boundaries ----
    v.push(Exhibit::new(
        "palette_zero_symbols",
        palette_bytes(0, &[], &[], 0, 0),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "palette_too_many_symbols",
        palette_bytes(17, &(0..17u8).collect::<Vec<_>>(), &[1u32; 17], 17, 0),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "palette_duplicate_values",
        palette_bytes(2, &[7, 7], &[1, 1], 2, 0),
        ExhibitKind::Descriptor,
        Expect::Either, // legal (non-canonical) encoding; materializes boundedly
    ));
    v.push(Exhibit::new(
        "palette_counts_not_summing",
        palette_bytes(2, &[7, 8], &[2, 2], 3, 0),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "palette_zero_count_symbol",
        palette_bytes(2, &[7, 8], &[3, 0], 3, 0),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "palette_rank_invalid",
        palette_bytes(3, &[7, 8, 9], &[2, 2, 2], 6, 90), // multinomial(6, [2,2,2]) = 90
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));

    // ---- PERIODIC boundaries ----
    v.push(Exhibit::new(
        "periodic_period_zero",
        periodic_bytes(0, &[], 3, &[], 0),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "periodic_period_over_cap",
        periodic_bytes(1025, &[0u8; 1025], 1, &[], 1025),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "periodic_tail_ge_period",
        periodic_bytes(4, &[1, 2, 3, 4], 1, &[5, 6, 7, 8], 8),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "periodic_len_mismatch",
        periodic_bytes(4, &[1, 2, 3, 4], 3, &[5, 6], 15), // 3*4+2 = 14 ≠ 15
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "periodic_count_overflow",
        periodic_bytes(1024, &[0u8; 1024], u32::MAX, &[1], u32::MAX),
        ExhibitKind::Descriptor,
        Expect::MustReject, // PeriodicLenMismatch / len > chunk cap
    ));

    // ---- PERMUTATION boundaries ----
    v.push(Exhibit::new(
        "permutation_gt34",
        permutation_bytes(0, &(0..35u8).collect::<Vec<_>>(), 35),
        ExhibitKind::Descriptor,
        Expect::MustReject, // PermutationSize
    ));
    v.push(Exhibit::new(
        "permutation_duplicate_alphabet",
        permutation_bytes(0, &[1, 1], 2),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BadPermutationAlphabet
    ));
    v.push(Exhibit::new(
        "permutation_rank_invalid",
        permutation_bytes(24, &[1, 2, 3, 4], 4), // 4! = 24 → rank out of range
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));

    // ---- EXACT_REF boundaries ----
    v.push(Exhibit::new(
        "exact_ref_zero_target",
        enc(&Representation::ExactRef {
            target: ChunkId::ZERO,
            off: 0,
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // ZeroObjectId
    ));
    v.push(Exhibit::new(
        "exact_ref_off_u32max_len_5",
        enc(&Representation::ExactRef {
            target: id(b"t"),
            off: u32::MAX as u64,
            len: 5,
        }),
        ExhibitKind::Descriptor,
        Expect::Either, // descriptor-valid; range check happens at materialize
    ));

    // ---- residual boundaries ----
    v.push(Exhibit::new(
        "residual_edits_unsorted",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::XorSparse {
                len: 8,
                edits: vec![Edit { pos: 5, val: 1 }, Edit { pos: 3, val: 2 }],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // EditsNotSorted
    ));
    v.push(Exhibit::new(
        "residual_edits_duplicated",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::XorSparse {
                len: 8,
                edits: vec![Edit { pos: 3, val: 1 }, Edit { pos: 3, val: 2 }],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // EditsNotSorted
    ));
    v.push(Exhibit::new(
        "residual_edits_out_of_bounds",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::XorSparse {
                len: 8,
                edits: vec![Edit { pos: 8, val: 1 }],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // EditOutOfRange
    ));
    v.push(Exhibit::new(
        "residual_ranges_overlap",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::RangeReplace {
                len: 8,
                changes: vec![
                    RangeChange { start: 0, end: 4 },
                    RangeChange { start: 2, end: 6 },
                ],
                literals: vec![1; 8],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // RangesOverlap
    ));
    v.push(Exhibit::new(
        "residual_ranges_out_of_range",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::RangeReplace {
                len: 8,
                changes: vec![RangeChange { start: 4, end: 10 }],
                literals: vec![1; 6],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // RangeOutOfRange
    ));
    v.push(Exhibit::new(
        "residual_lit_count_mismatch",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 8,
            residual: Residual::RangeReplace {
                len: 8,
                changes: vec![RangeChange { start: 0, end: 4 }],
                literals: vec![1; 3],
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // LiteralCountMismatch
    ));
    // edit fanout over the tight cap: valid under default (4096), invalid
    // under tight (64).
    {
        let edits: Vec<Edit> = (0..70u32).map(|i| Edit { pos: i * 2, val: 1 }).collect();
        v.push(Exhibit::new(
            "residual_fanout_70",
            enc(&Representation::BaseResidual {
                base: id(b"b"),
                base_len: 512,
                residual: Residual::XorSparse { len: 512, edits },
                len: 512,
            }),
            ExhibitKind::Descriptor,
            Expect::Either, // accepts under default; FanoutTooLarge under tight
        ));
    }

    // ---- sequence command-count boundaries ----
    v.push(Exhibit::new(
        "seq_cmds_gt_output",
        enc(&Representation::SequenceRans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 5,
            off_len: 4,
            cmds: 5,
            lit_out: 4,
            len: 4,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SequenceCmdsMismatch
    ));
    v.push(Exhibit::new(
        "seq_no_commands",
        enc(&Representation::SequenceRans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 0,
            lit_len: 0,
            off_len: 0,
            cmds: 0,
            lit_out: 0,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SequenceNoCommands
    ));
    v.push(Exhibit::new(
        "seq_lit_out_gt_len",
        enc(&Representation::SequenceRans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 10,
            off_len: 0,
            cmds: 2,
            lit_out: 9,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SequenceLitOutMismatch
    ));
    v.push(Exhibit::new(
        "seq_stream_too_large",
        enc(&Representation::SequenceRans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: Limits::default().max_chunk_size as u32 + 65, // > max_stream under both sets
            lit_len: 0,
            off_len: 0,
            cmds: 1,
            lit_out: 1,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SequenceStreamTooLarge
    ));

    // ---- SparseBlock64 boundaries ----
    v.push(Exhibit::new(
        "sb64_words_not_covering",
        enc(&Representation::SparseBlock64 {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            pc_len: 10,
            rank_len: 0,
            lit_len: 1,
            words: 1, // 1*8 = 8 < 64
            nonzero: 1,
            lit_out: 1,
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SparseBlockWords
    ));
    v.push(Exhibit::new(
        "sb64_nonzero_gt_words",
        enc(&Representation::SparseBlock64 {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            pc_len: 10,
            rank_len: 8,
            lit_len: 1,
            words: 2,
            nonzero: 3,
            lit_out: 1,
            len: 16,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SparseBlockWords
    ));
    v.push(Exhibit::new(
        "sb64_nonzero_gt_lit_out",
        enc(&Representation::SparseBlock64 {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            pc_len: 10,
            rank_len: 8,
            lit_len: 1,
            words: 2,
            nonzero: 2,
            lit_out: 1,
            len: 16,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // SparseBlockLiteralCount
    ));

    // ---- dictionary boundaries ----
    v.push(Exhibit::new(
        "dict_zero_dictionary_len",
        enc(&Representation::SequenceDict {
            dictionary: id(b"d"),
            dictionary_len: 0,
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 5,
            off_len: 4,
            src_len: 2,
            cmds: 2,
            lit_out: 4,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BadDictionary
    ));
    v.push(Exhibit::new(
        "dict_len_over_64k",
        enc(&Representation::SequenceDict {
            dictionary: id(b"d"),
            dictionary_len: 65537,
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 5,
            off_len: 4,
            src_len: 2,
            cmds: 2,
            lit_out: 4,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BadDictionary
    ));
    v.push(Exhibit::new(
        "shared_dict_zero_shared_len",
        enc(&Representation::SequenceSharedDict {
            dictionary: ChunkId::ZERO,
            dictionary_len: 0,
            shared: id(b"s"),
            shared_len: 0,
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 5,
            off_len: 4,
            src_len: 2,
            cmds: 2,
            lit_out: 4,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BadDictionary
    ));
    v.push(Exhibit::new(
        "shared_dict_inconsistent_file_dict",
        enc(&Representation::SequenceSharedDict {
            dictionary: id(b"d"),
            dictionary_len: 0, // non-zero id but zero length: inconsistent
            shared: id(b"s"),
            shared_len: 64,
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 14,
            codec: RansCodec::Interleaved2,
            seq_len: 10,
            lit_len: 5,
            off_len: 4,
            src_len: 2,
            cmds: 2,
            lit_out: 4,
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BadDictionary
    ));

    // ---- zero object ids and bad scale bits ----
    v.push(Exhibit::new(
        "raw_zero_obj",
        enc(&Representation::Raw {
            obj: ChunkId::ZERO,
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "rans_scale_bits_zero",
        enc(&Representation::Rans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 0,
            codec: RansCodec::Interleaved2,
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));
    v.push(Exhibit::new(
        "rans_scale_bits_17",
        enc(&Representation::Rans {
            model: id(b"m"),
            enc_obj: id(b"e"),
            scale_bits: 17,
            codec: RansCodec::Interleaved2,
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject,
    ));

    // ---- base length boundaries ----
    v.push(Exhibit::new(
        "base_residual_base_too_short",
        enc(&Representation::BaseResidual {
            base: id(b"b"),
            base_len: 4,
            residual: Residual::XorSparse {
                len: 8,
                edits: Vec::new(),
            },
            len: 8,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // BaseTooShort (positional residual)
    ));

    // ---- entropy-ref residual mismatch (RansCoded.decoded_len is the
    // stored length field; a mismatch with the representation length is a
    // genuine decode-time reject. XorSparse/RangeReplace carry no stored
    // length — the decoder derives it from the representation length, so
    // a "mismatch" there cannot exist in the encoding).
    v.push(Exhibit::new(
        "entropy_ref_residual_len_mismatch",
        enc(&Representation::EntropyRef {
            universe: UniverseId::UniformXofV1,
            seed: [0u8; 16],
            coordinate: 0,
            transform: TransformId::Identity,
            residual: Residual::RansCoded {
                len: 64,
                enc_obj: id(b"e"),
                model: id(b"m"),
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                decoded_len: 8, // != 64: ResidualLenMismatch
            },
            len: 64,
        }),
        ExhibitKind::Descriptor,
        Expect::MustReject, // ResidualLenMismatch
    ));
    // unknown universe id (hand-assembled: the encoder cannot represent
    // an unknown id, which is exactly the point of the exhibit)
    {
        let mut bytes = vec![0x0Au8, 0x40, 0x00, 0x00, 0x00]; // ENTROPY_REF len 64
        bytes.push(0x42); // unknown universe id
        bytes.extend_from_slice(&[0u8; 16]); // seed
        bytes.extend_from_slice(&0u64.to_le_bytes()); // coordinate
        bytes.push(0x01); // transform Identity
        bytes.push(0x01); // residual kind XorSparse
        bytes.extend_from_slice(&0u32.to_le_bytes()); // edit count
        v.push(Exhibit::new(
            "entropy_ref_unknown_universe",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject, // decode: Malformed (unknown universe id)
        ));
    }

    // ---- unknown transform ----
    {
        let mut bytes = vec![0x0Au8, 0x40, 0x00, 0x00, 0x00]; // ENTROPY_REF len 64
        bytes.push(0x01); // universe UniformXofV1
        bytes.extend_from_slice(&[0u8; 16]); // seed
        bytes.extend_from_slice(&0u64.to_le_bytes()); // coordinate
        bytes.push(0x02); // unknown transform
        bytes.push(0x01); // residual kind XorSparse
        bytes.extend_from_slice(&0u32.to_le_bytes()); // edit count
        v.push(Exhibit::new(
            "entropy_ref_unknown_transform",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject,
        ));
    }

    // ---- inline boundaries ----
    {
        let mut bytes = vec![0x0Bu8];
        bytes.extend_from_slice(&4097u32.to_le_bytes()); // len 4097 > max_inline 4096
        bytes.extend_from_slice(&vec![0u8; 4097]);
        v.push(Exhibit::new(
            "inline_over_cap",
            bytes,
            ExhibitKind::Descriptor,
            Expect::MustReject, // InlineTooLarge
        ));
    }

    v
}

/// Graph-level adversarial exhibits. The bytes are graph specs (parsed by
/// `parse_graph_spec`); the oracle is `run_graph_oracle`.
fn graph_exhibits() -> Vec<Exhibit> {
    let mut v: Vec<Exhibit> = Vec::new();
    let limits = Limits::default();

    // Valid descriptor -> corrupted model object.
    {
        let input = compressible(4096);
        let (rep, objects) = one_candidate(&crate::rans::residual::RansEncoder, &input, &limits);
        let mut spec = spec_with_objects(&rep, &objects);
        // Corrupt the model object in the table (the descriptor still
        // references it by id).
        let model_id = match &rep {
            Representation::Rans { model, .. } => *model,
            _ => unreachable!(),
        };
        spec.objs.retain(|(oid, _)| *oid != model_id);
        spec.add_obj(model_id, seeded_bytes(32, 0xCAFE));
        v.push(Exhibit::new(
            "valid_desc_corrupted_model",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // decode_model fails → RansDecode
        ));
    }

    // Valid sequence descriptor -> hostile command stream (a COPY before
    // any literal: distance > pos must be rejected typed).
    {
        let commands = vec![0x84u8]; // COPY clen 8, but pos == 0
        let literals: Vec<u8> = Vec::new();
        let offsets: Vec<u8> = 1u16.to_le_bytes().to_vec();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SequenceRans {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                cmds: 1,
                lit_out: 0,
                len: 8,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "seq_copy_before_history",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // copy distance out of range
        ));
    }

    // Exhausted offset stream: a COPY that needs a u16 the stream lacks.
    {
        let commands = vec![0x00u8, 0x84u8]; // literal 1, then COPY 8
        let literals: Vec<u8> = vec![0xAA];
        let offsets: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SequenceRans {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                cmds: 2,
                lit_out: 1,
                len: 9,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "seq_exhausted_offsets",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // copy offset exhausted
        ));
    }

    // Exhausted literal stream: a literal run longer than lit_out.
    {
        let commands = vec![0x3Fu8]; // literal run 64
        let literals: Vec<u8> = vec![0xAA; 63];
        let offsets: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SequenceRans {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                cmds: 1,
                lit_out: 63,
                len: 64,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "seq_exhausted_literals",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // literal run overflow
        ));
    }

    // Exhausted COPY-SOURCE stream: a COPY command with no source byte
    // left in the four-stream families.
    {
        let dict: Vec<u8> = (0..64u8).collect();
        let dict_id = ChunkId::of(&dict);
        let commands = vec![0x00u8, 0x84u8]; // literal 1, then COPY 8
        let literals: Vec<u8> = vec![0xAA];
        let offsets: Vec<u8> = 1u16.to_le_bytes().to_vec();
        let sources: Vec<u8> = Vec::new(); // the COPY needs one source byte
        let (model_obj, enc_obj, lens) =
            encode_streams(&[commands, literals.clone(), offsets, sources]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SequenceDict {
                dictionary: dict_id,
                dictionary_len: 64,
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                src_len: lens[3],
                cmds: 2,
                lit_out: 1,
                len: 9,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        add_chunk(&mut spec, dict_id, &dict);
        v.push(Exhibit::new(
            "seq_exhausted_copy_sources",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // copy source exhausted
        ));
    }

    // Dictionary copy at exactly the dictionary end and one byte beyond.
    {
        let dict: Vec<u8> = (0..64u8).collect();
        let dict_id = ChunkId::of(&dict);
        // commands: literal 1 then COPY 8 from DICT at offset 64 (exact
        // end: 64 + 8 > 64 → out of bounds).
        for (name, off, expect) in [
            ("dict_copy_at_exact_end", 64u16, Expect::MustReject),
            ("dict_copy_one_beyond", 65u16, Expect::MustReject),
        ] {
            let commands = vec![0x00u8, 0x84u8];
            let literals: Vec<u8> = vec![0xBB];
            let offsets: Vec<u8> = off.to_le_bytes().to_vec();
            let sources: Vec<u8> = vec![crate::rans::sequence::SRC_DICT];
            let (model_obj, enc_obj, lens) =
                encode_streams(&[commands, literals.clone(), offsets, sources]);
            let (scale_bits, codec) = sequence_scale_codec();
            let model_id = ChunkId::of(&model_obj);
            let enc_id = ChunkId::of(&enc_obj);
            let mut spec = GraphSpec::new(entry_id());
            spec.add_desc(
                entry_id(),
                enc(&Representation::SequenceDict {
                    dictionary: dict_id,
                    dictionary_len: 64,
                    model: model_id,
                    enc_obj: enc_id,
                    scale_bits,
                    codec,
                    seq_len: lens[0],
                    lit_len: lens[1],
                    off_len: lens[2],
                    src_len: lens[3],
                    cmds: 2,
                    lit_out: 1,
                    len: 9,
                }),
            );
            spec.add_obj(model_id, model_obj);
            spec.add_obj(enc_id, enc_obj);
            add_chunk(&mut spec, dict_id, &dict);
            v.push(Exhibit::new(
                name,
                encode_graph_spec(&spec),
                ExhibitKind::Graph,
                expect,
            ));
        }
    }

    // Invalid dictionary -> another invalid dictionary (a decode failure
    // inside a reference chain must be a typed error).
    {
        let entry = id(b"idd-entry");
        let d1 = id(b"idd-d1");
        let d2 = id(b"idd-d2");
        let mut spec = GraphSpec::new(entry);
        spec.add_desc(
            d1,
            enc(&Representation::ExactRef {
                target: d2,
                off: 0,
                len: 64,
            }),
        );
        spec.add_desc(d2, vec![0x42, 0x00, 0x00]); // garbage descriptor bytes
        spec.add_desc(
            entry,
            enc(&Representation::SequenceDict {
                dictionary: d1,
                dictionary_len: 64,
                model: id(b"m"),
                enc_obj: id(b"e"),
                scale_bits: 14,
                codec: RansCodec::Interleaved2,
                seq_len: 1,
                lit_len: 1,
                off_len: 0,
                src_len: 0,
                cmds: 1,
                lit_out: 1,
                len: 1,
            }),
        );
        v.push(Exhibit::new(
            "invalid_dict_to_invalid_dict",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // InvalidDescriptor via the chain
        ));
    }

    // SparseBlock64: popcount > 64 in the decoded stream.
    {
        // words = 1, nonzero = 1, popcount byte = 65, rank = 0, literal = 1.
        let commands = vec![65u8];
        let literals: Vec<u8> = vec![0xCD];
        let offsets: Vec<u8> = vec![0u8; 8];
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SparseBlock64 {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                pc_len: lens[0],
                lit_len: lens[1],
                rank_len: lens[2],
                words: 1,
                nonzero: 1,
                lit_out: 1,
                len: 8,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "sb64_popcount_gt_64",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // popcount > 64
        ));
    }

    // SparseBlock64: combination rank out of range (rank >= C(64, k)).
    {
        let commands = vec![1u8]; // one nonzero word
        let literals: Vec<u8> = vec![0xCD];
        let offsets: Vec<u8> = 64u64.to_le_bytes().to_vec(); // C(64,1) = 64
        let (model_obj, enc_obj, lens) = encode_streams(&[commands, literals.clone(), offsets]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SparseBlock64 {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                pc_len: lens[0],
                lit_len: lens[1],
                rank_len: lens[2],
                words: 1,
                nonzero: 1,
                lit_out: 1,
                len: 8,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "sb64_rank_out_of_range",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // unrank fails
        ));
    }

    // SEQUENCE_DEEP: hostile command stream with a reserved byte.
    {
        let commands = vec![0xF2u8]; // reserved
        let literals: Vec<u8> = Vec::new();
        let offsets: Vec<u8> = Vec::new();
        let lengths: Vec<u8> = Vec::new();
        let (model_obj, enc_obj, lens) =
            encode_streams(&[commands, literals.clone(), offsets, lengths]);
        let (scale_bits, codec) = sequence_scale_codec();
        let model_id = ChunkId::of(&model_obj);
        let enc_id = ChunkId::of(&enc_obj);
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::SequenceDeep {
                model: model_id,
                enc_obj: enc_id,
                scale_bits,
                codec,
                seq_len: lens[0],
                lit_len: lens[1],
                off_len: lens[2],
                len_len: lens[3],
                cmds: 1,
                lit_out: 0,
                len: 8,
            }),
        );
        spec.add_obj(model_id, model_obj);
        spec.add_obj(enc_id, enc_obj);
        v.push(Exhibit::new(
            "deep_reserved_command",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // reserved deep command byte
        ));
    }

    // Model object at exactly the cap / one above (decode_model's size
    // gate must hold for hostile model payloads).
    {
        let input = compressible(4096);
        let (rep, objects) = one_candidate(&crate::rans::residual::RansEncoder, &input, &limits);
        let model_id = match &rep {
            Representation::Rans { model, .. } => *model,
            _ => unreachable!(),
        };
        let mut spec = spec_with_objects(&rep, &objects);
        spec.objs.retain(|(oid, _)| *oid != model_id);
        spec.add_obj(model_id, seeded_bytes(2047, 0x1111));
        v.push(Exhibit::new(
            "model_just_below_cap",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::Either, // parse fails typed, or bounded
        ));
        spec.objs.retain(|(oid, _)| *oid != model_id);
        spec.add_obj(model_id, seeded_bytes(2048, 0x3333));
        v.push(Exhibit::new(
            "model_exactly_at_cap",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::Either, // the size gate passes; the parse fails typed
        ));
        spec.objs.retain(|(oid, _)| *oid != model_id);
        spec.add_obj(model_id, seeded_bytes(2049, 0x2222));
        v.push(Exhibit::new(
            "model_just_above_cap",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // decode_model TooLarge → RansDecode
        ));
    }

    // Invalid rANS bitstream with a VALID model: the decode must either
    // fail typed or produce bounded bytes (never panic). This is the
    // "wrong bytes" surface — the STORE court closes it with the content-
    // id binding; here the oracle is boundedness.
    {
        let input = compressible(4096);
        let (rep, objects) = one_candidate(&crate::rans::residual::RansEncoder, &input, &limits);
        let enc_id = match &rep {
            Representation::Rans { enc_obj, .. } => *enc_obj,
            _ => unreachable!(),
        };
        let mut spec = spec_with_objects(&rep, &objects);
        spec.objs.retain(|(oid, _)| *oid != enc_id);
        spec.add_obj(enc_id, seeded_bytes(128, 0xABCD));
        v.push(Exhibit::new(
            "valid_model_hostile_stream",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::Either,
        ));
    }

    // Missing object (a descriptor referencing an absent id).
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(
            entry_id(),
            enc(&Representation::Raw {
                obj: id(b"absent"),
                len: 64,
            }),
        );
        v.push(Exhibit::new(
            "raw_missing_object",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // MissingObject
        ));
    }

    // Missing chunk (entry id absent from the descriptor table).
    {
        let spec = GraphSpec::new(id(b"no-such-chunk"));
        v.push(Exhibit::new(
            "missing_entry_chunk",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // MissingChunk
        ));
    }

    // EXACT_REF range out of bounds (target is 16 bytes, off+len beyond).
    {
        let target_id = id(b"small");
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(target_id, enc(&Representation::Fill { value: 1, len: 16 }));
        spec.add_desc(
            entry_id(),
            enc(&Representation::ExactRef {
                target: target_id,
                off: u32::MAX as u64,
                len: 5,
            }),
        );
        v.push(Exhibit::new(
            "exact_ref_range_out_of_bounds",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // RangeOutOfBounds
        ));
    }
    {
        let target_id = id(b"small2");
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(target_id, enc(&Representation::Fill { value: 1, len: 16 }));
        spec.add_desc(
            entry_id(),
            enc(&Representation::ExactRef {
                target: target_id,
                off: 12,
                len: 5, // 12 + 5 = 17 > 16
            }),
        );
        v.push(Exhibit::new(
            "exact_ref_off_plus_len_boundary",
            encode_graph_spec(&spec),
            ExhibitKind::Graph,
            Expect::MustReject, // RangeOutOfBounds
        ));
    }

    // An empty spec (no bytes at all): the parser must handle it.
    v.push(Exhibit::new(
        "empty_graph_spec",
        Vec::new(),
        ExhibitKind::Graph,
        Expect::MustReject, // entry falls back to a content-derived absent id
    ));

    // A spec whose entry id is present but whose descriptor table is
    // truncated mid-entry (parser leniency must not panic).
    {
        let mut spec = GraphSpec::new(entry_id());
        spec.add_desc(entry_id(), enc(&Representation::Zero { len: 64 }));
        let bytes = encode_graph_spec(&spec);
        let cut = &bytes[..bytes.len() - 20];
        v.push(Exhibit::new(
            "truncated_graph_spec",
            cut.to_vec(),
            ExhibitKind::Graph,
            Expect::Either,
        ));
    }

    v
}

// ---------------------------------------------------------------------------
// Byte builders for boundary exhibits (hand-assembled, not via `encode`,
// so the exhibit can violate what the encoder refuses).
// ---------------------------------------------------------------------------

fn sparse_bytes(k: u32, len: u32, rank: u128, literals: &[u8]) -> Vec<u8> {
    let mut b = vec![0x07u8];
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&k.to_le_bytes());
    b.extend_from_slice(&rank.to_le_bytes());
    b.extend_from_slice(literals);
    b
}

fn palette_bytes(m: u8, palette: &[u8], counts: &[u32], len: u32, rank: u128) -> Vec<u8> {
    let mut b = vec![0x08u8];
    b.extend_from_slice(&len.to_le_bytes());
    b.push(m);
    b.extend_from_slice(palette);
    for &c in counts {
        b.extend_from_slice(&c.to_le_bytes());
    }
    b.extend_from_slice(&rank.to_le_bytes());
    b
}

fn periodic_bytes(period: u32, pattern: &[u8], count: u32, tail: &[u8], len: u32) -> Vec<u8> {
    let mut b = vec![0x09u8];
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&period.to_le_bytes());
    b.extend_from_slice(pattern);
    b.extend_from_slice(&count.to_le_bytes());
    b.extend_from_slice(&(tail.len() as u32).to_le_bytes());
    b.extend_from_slice(tail);
    b
}

fn permutation_bytes(rank: u128, alphabet: &[u8], len: u32) -> Vec<u8> {
    let mut b = vec![0x0Cu8];
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&rank.to_le_bytes());
    b.extend_from_slice(alphabet);
    b
}
