//! Test helpers: a fully functional in-memory `DecoderContext` and object
//! store used by engine, optimizer, and store tests. Never used in
//! production paths.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::ops::Range;

use crate::core::extent::ChunkId;
use crate::core::materialize::{DecoderContext, MaterializeError};
use crate::core::representation::{RansCodec, Representation, UniverseId};
use crate::rans::metadata;

/// In-memory resolver with real rANS decode and real universe
/// materialization wired in.
#[derive(Debug, Default, Clone)]
pub struct MemResolver {
    /// Content-addressed objects (raw payloads, rANS streams, models).
    pub objects: HashMap<ChunkId, Vec<u8>>,
    /// Logical chunk index: content id → representation descriptor.
    pub chunks: HashMap<ChunkId, Representation>,
}

impl MemResolver {
    /// Empty resolver.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Resolver from an object map only.
    pub fn from_map(objects: HashMap<ChunkId, Vec<u8>>) -> Self {
        Self {
            objects,
            chunks: HashMap::new(),
        }
    }

    /// Insert a chunk descriptor plus any objects it references.
    pub fn put_chunk(&mut self, id: ChunkId, desc: Representation) {
        self.chunks.insert(id, desc);
    }

    /// Insert an object.
    pub fn put_object(&mut self, id: ChunkId, bytes: Vec<u8>) {
        self.objects.insert(id, bytes);
    }
}

/// Histogram of byte values in `data`.
pub fn histogram_of(data: &[u8]) -> [u32; 256] {
    let mut hist = [0u32; 256];
    for &b in data {
        hist[b as usize] += 1;
    }
    hist
}

impl DecoderContext for MemResolver {
    fn fetch_object(&self, id: &ChunkId) -> Result<Vec<u8>, MaterializeError> {
        self.objects
            .get(id)
            .cloned()
            .ok_or(MaterializeError::MissingObject(*id))
    }

    fn fetch_descriptor(&self, id: &ChunkId) -> Result<Representation, MaterializeError> {
        self.chunks
            .get(id)
            .cloned()
            .ok_or(MaterializeError::MissingChunk(*id))
    }

    fn decode_rans(
        &self,
        model: &[u8],
        encoded: &[u8],
        scale_bits: u8,
        codec: RansCodec,
        out_len: u64,
    ) -> Result<Vec<u8>, MaterializeError> {
        let parsed = metadata::decode_model(model, 2048)
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
