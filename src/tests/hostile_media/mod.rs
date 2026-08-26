//! Phase-11A hostile-media court (see `docs/security/hostile-media-court.md`):
//! the persistent-data adversarial suite.
//!
//! The backing store is treated as **untrusted/corrupt input** (the threat
//! model's first line). This court attacks the one dimension the valid-path
//! suite barely exercises: input EntropyFS did not produce itself. The
//! oracle is simple and uniform:
//!
//! ```text
//! arbitrary hostile bytes
//!         ↓
//! persistent decoder / graph traversal
//!         ↓
//! must terminate boundedly
//!         ↓
//! Ok(valid bounded result)
//!     OR
//! typed rejection
//!
//! NEVER:
//! panic, OOM, infinite loop, unbounded recursion, unbounded CPU,
//! silent wrong bytes
//! ```
//!
//! Three courts run here:
//! - `descriptor_court`: every bounded byte string through the descriptor
//!   codec; decode-OK implies structural validation OK and a byte-exact
//!   canonical re-encode (ADR-0016 "typed error, never panic").
//! - `graph_court`: a fuzz-defined descriptor table + object table + entry
//!   descriptor materialized through an in-memory hostile resolver;
//!   materialization either succeeds within all declared resource bounds
//!   or returns a typed error.
//! - `store_court`: the CRC-aware distinction (physical corruption vs
//!   semantic adversarial mutation) over real tiny stores, plus the
//!   whole-store mutator driving open/fsck/materialize.
//!
//! The corpus (`corpus`) is the permanent hand-crafted exhibit set: one
//! canonical descriptor of every representation family, plus adversarial
//! exhibits for every boundary the format defines.

#![forbid(unsafe_code)]

pub mod corpus;
pub mod descriptor_court;
pub mod graph_court;
pub mod store_court;

use std::collections::HashMap;
use std::ops::Range;

use crate::core::extent::ChunkId;
use crate::core::limits::Limits;
use crate::core::materialize::{DecoderContext, MaterializeError};
use crate::core::representation::{RansCodec, Representation, UniverseId};

/// Deliberately tight limits for the descriptor court: every parse path
/// must honor these, not merely the defaults (a hostile mount could
/// configure small limits, and a parser that only behaves at default sizes
/// is a bomb waiting for a constrained deployment).
pub fn tight_limits() -> Limits {
    Limits {
        max_chunk_size: 16 * 1024,
        chunk_class: 4096,
        max_descriptor_bytes: 512,
        max_reference_depth: 2,
        max_decode_work: 1 << 20,
        max_alloc_bytes: 64 * 1024,
        max_fanout: 64,
        max_model_bytes: 512,
        max_inline_bytes: 256,
        max_period: 64,
        max_palette: 4,
    }
}

/// The two limit sets every court runs under (tight + the real defaults).
pub const LIMIT_SETS: [&str; 2] = ["tight", "default"];

/// Expected outcome class of an exhibit. The court never asserts that
/// arbitrary data must be rejected — some random inputs legitimately
/// describe valid content — so `Either` (bounded-valid or typed-reject) is
/// the default oracle; `MustAccept`/`MustReject` are asserted only where
/// the outcome is fully determined by the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// decode+validate must succeed (and materialize boundedly for graph
    /// exhibits).
    MustAccept,
    /// must be rejected with a typed error.
    MustReject,
    /// bounded-valid or typed-reject: either is admissible.
    Either,
}

/// Which court consumes an exhibit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhibitKind {
    /// Bytes fed to `format::descriptor::decode`.
    Descriptor,
    /// Bytes parsed as a graph spec (descriptor table + object table +
    /// entry id) and materialized.
    Graph,
}

/// One named adversarial exhibit: bytes plus the expected outcome class.
#[derive(Debug, Clone)]
pub struct Exhibit {
    /// Stable exhibit name (also the evidence receipt key).
    pub name: String,
    /// The exhibit bytes.
    pub bytes: Vec<u8>,
    /// Expected outcome class.
    pub expect: Expect,
    /// Which court consumes the bytes.
    pub kind: ExhibitKind,
}

impl Exhibit {
    /// Build an exhibit.
    pub fn new(name: impl Into<String>, bytes: Vec<u8>, kind: ExhibitKind, expect: Expect) -> Self {
        Self {
            name: name.into(),
            bytes,
            expect,
            kind,
        }
    }
}

// ---------------------------------------------------------------------------
// Graph spec: descriptor table + object table + entry descriptor id.
// ---------------------------------------------------------------------------

/// Maximum tables the lenient parser accepts (input-bounded; a u8 count
/// field can claim up to 255, but each entry costs ≥33 bytes and the court
/// bounds its inputs, so 32 is generous and keeps the resolver tiny).
pub const GRAPH_MAX_TABLES: usize = 32;

/// A fuzz-defined hostile graph: a descriptor table (content id →
/// descriptor bytes), an object table (content id → payload bytes), and
/// the entry descriptor id to materialize.
#[derive(Debug, Clone)]
pub struct GraphSpec {
    /// Descriptor table: id → descriptor bytes.
    pub descs: Vec<(ChunkId, Vec<u8>)>,
    /// Object table: id → payload bytes.
    pub objs: Vec<(ChunkId, Vec<u8>)>,
    /// Entry descriptor id.
    pub entry: ChunkId,
}

impl GraphSpec {
    /// Empty spec with the given entry id.
    pub fn new(entry: ChunkId) -> Self {
        Self {
            descs: Vec::new(),
            objs: Vec::new(),
            entry,
        }
    }

    /// Add a descriptor-table entry.
    pub fn add_desc(&mut self, id: ChunkId, bytes: Vec<u8>) -> &mut Self {
        self.descs.push((id, bytes));
        self
    }

    /// Add an object-table entry.
    pub fn add_obj(&mut self, id: ChunkId, bytes: Vec<u8>) -> &mut Self {
        self.objs.push((id, bytes));
        self
    }
}

/// Encode a graph spec into its flat byte form:
///
/// ```text
/// u8 n_descs
///   per desc: [u8;32] id, u32 LE dlen, [dlen] descriptor bytes
/// u8 n_objs
///   per obj:  [u8;32] id, u32 LE olen, [olen] object bytes
/// [u8;32] entry id
/// ```
pub fn encode_graph_spec(spec: &GraphSpec) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(spec.descs.len().min(GRAPH_MAX_TABLES) as u8);
    for (id, bytes) in spec.descs.iter().take(GRAPH_MAX_TABLES) {
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out.push(spec.objs.len().min(GRAPH_MAX_TABLES) as u8);
    for (id, bytes) in spec.objs.iter().take(GRAPH_MAX_TABLES) {
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(spec.entry.as_bytes());
    out
}

/// Lenient graph-spec parser: any byte string is a valid spec. Truncation
/// mid-structure stops parsing (what was read is used); a missing entry id
/// falls back to a content-derived id (almost surely absent → a typed
/// `MissingChunk`). Never panics; allocations are bounded by the input
/// size and the table caps.
pub fn parse_graph_spec(input: &[u8]) -> GraphSpec {
    let mut pos = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        if input.len().saturating_sub(pos) < n {
            return None;
        }
        let s = &input[pos..pos + n];
        pos += n;
        Some(s)
    };
    let mut spec = GraphSpec::new(ChunkId::of(input));
    let n_desc = take(1).map(|b| b[0]).unwrap_or(0);
    for _ in 0..(n_desc as usize).min(GRAPH_MAX_TABLES) {
        let id = match take(32) {
            Some(b) => ChunkId::new(b.try_into().expect("32-byte id")),
            None => break,
        };
        let dlen = match take(4) {
            Some(b) => u32::from_le_bytes(b.try_into().expect("4-byte len")) as usize,
            None => break,
        };
        let Some(payload) = take(dlen) else { break };
        spec.descs.push((id, payload.to_vec()));
    }
    let n_obj = take(1).map(|b| b[0]).unwrap_or(0);
    for _ in 0..(n_obj as usize).min(GRAPH_MAX_TABLES) {
        let id = match take(32) {
            Some(b) => ChunkId::new(b.try_into().expect("32-byte id")),
            None => break,
        };
        let olen = match take(4) {
            Some(b) => u32::from_le_bytes(b.try_into().expect("4-byte len")) as usize,
            None => break,
        };
        let Some(payload) = take(olen) else { break };
        spec.objs.push((id, payload.to_vec()));
    }
    if let Some(b) = take(32) {
        spec.entry = ChunkId::new(b.try_into().expect("32-byte entry id"));
    }
    spec
}

// ---------------------------------------------------------------------------
// Hostile resolver: an in-memory `DecoderContext` whose descriptor table
// holds HOSTILE BYTES (decoded on demand through the real descriptor
// codec, mirroring the store's `fetch_descriptor`), and whose object table
// holds hostile payloads (rANS models/streams included).
// ---------------------------------------------------------------------------

/// In-memory hostile resolver over a graph spec. Mirrors the store's
/// `DecoderContext` semantics exactly (fetch_descriptor = decode-with-
/// limits, decode_rans = decode_model + tag check + decode_stream), so the
/// materializer exercises the same code paths a hostile store would.
#[derive(Debug, Clone)]
pub struct HostileResolver {
    /// Content-addressed objects (payloads, rANS streams, models).
    objects: HashMap<ChunkId, Vec<u8>>,
    /// Chunk index: content id → hostile descriptor bytes.
    chunks: HashMap<ChunkId, Vec<u8>>,
    /// Limits the decode paths enforce.
    limits: Limits,
}

impl HostileResolver {
    /// Build a resolver from a graph spec.
    pub fn from_spec(spec: &GraphSpec, limits: &Limits) -> Self {
        let mut objects = HashMap::with_capacity(spec.objs.len());
        for (id, b) in &spec.objs {
            objects.insert(*id, b.clone());
        }
        let mut chunks = HashMap::with_capacity(spec.descs.len());
        for (id, b) in &spec.descs {
            chunks.insert(*id, b.clone());
        }
        Self {
            objects,
            chunks,
            limits: *limits,
        }
    }
}

impl DecoderContext for HostileResolver {
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
        self.objects
            .get(id)
            .cloned()
            .ok_or(MaterializeError::MissingObject(*id))
    }

    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
        match self.chunks.get(id) {
            Some(bytes) => crate::format::descriptor::decode(bytes, &self.limits)
                .map_err(|e| MaterializeError::InvalidDescriptor(e.to_string())),
            None => Err(MaterializeError::MissingChunk(*id)),
        }
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError> {
        let parsed = crate::rans::metadata::decode_model(model, self.limits.max_model_bytes)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))?;
        if parsed.scale_bits != scale_bits || parsed.codec != codec {
            return Err(MaterializeError::RansDecode("model tag mismatch".into()));
        }
        crate::rans::residual::decode_stream(&parsed, encoded, out_len)
            .map_err(|e| MaterializeError::RansDecode(e.to_string()))
    }

    fn universe_bytes(
        &self,
        universe: UniverseId,
        seed: [u8; 16],
        coordinate: u64,
        range: Range<u64>,
    ) -> Result<Vec<u8>, MaterializeError> {
        match universe {
            UniverseId::UniformXofV1 => Ok(
                crate::entropy::universe::UniformXofV1::materialize_range(seed, coordinate, range),
            ),
        }
    }
}

/// The graph court oracle: materialize the entry descriptor through the
/// hostile resolver. The outcome is either a bounded success (the output
/// length is exactly the descriptor's declared length and within the
/// declared limits) or a typed rejection — never a panic, never an
/// unbounded allocation (every allocation in the materializer is checked
/// against `max_alloc_bytes`/`max_chunk_size` before it happens).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphOutcome {
    /// Materialized successfully within the declared bounds.
    Ok { len: u64 },
    /// Rejected with a typed materialization error.
    Rejected(String),
}

/// Run the graph oracle over a spec; returns an `Err` describing any
/// invariant violation (the court turns that into a failure).
pub fn run_graph_oracle(spec: &GraphSpec, limits: &Limits) -> Result<GraphOutcome, String> {
    let resolver = HostileResolver::from_spec(spec, limits);
    let entry = match resolver.fetch_descriptor(&spec.entry) {
        Ok(r) => r,
        Err(e) => return Ok(GraphOutcome::Rejected(format!("{e:?}"))),
    };
    // Structural preconditions of materialization itself (the materializer
    // checks these, but the court asserts the declared bounds explicitly).
    if entry.len() > limits.max_chunk_size {
        return Err(format!(
            "entry descriptor declares {} bytes, over the {} chunk cap",
            entry.len(),
            limits.max_chunk_size
        ));
    }
    if entry.len() > limits.max_alloc_bytes {
        return Err(format!(
            "entry descriptor declares {} bytes, over the {} allocation cap",
            entry.len(),
            limits.max_alloc_bytes
        ));
    }
    match crate::core::materialize::materialize_to_vec(&entry, &resolver, limits) {
        Ok(bytes) => {
            if bytes.len() as u64 != entry.len() {
                return Err(format!(
                    "materialized {} bytes but the descriptor declares {}",
                    bytes.len(),
                    entry.len()
                ));
            }
            Ok(GraphOutcome::Ok {
                len: bytes.len() as u64,
            })
        }
        Err(e) => Ok(GraphOutcome::Rejected(format!("{e:?}"))),
    }
}

/// Deterministic pseudo-random bytes (SplitMix64). Shared by the courts
/// for reproducible seeded mutation.
pub fn seeded_bytes(n: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let b = z.to_le_bytes();
        let take = (n - out.len()).min(8);
        out.extend_from_slice(&b[..take]);
    }
    out
}
