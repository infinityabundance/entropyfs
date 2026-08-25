//! Runtime rANS backend selection (ADR-0003, `docs/theory/rans-state.md` §7).
//!
//! The scalar paths are the authority; SIMD backends are deferred to Phase 6
//! and must produce byte-identical streams (upstream bitstream parity).

#![forbid(unsafe_code)]

use crate::core::representation::RansCodec;
use crate::rans::model::RansModel;
use crate::rans::residual::{RansStreamError, decode_stream, encode_stream};

/// Available rANS backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// Scalar single-state byte rANS.
    ScalarSingle,
    /// Scalar two-state interleaved byte rANS (Phase-1 authority path).
    ScalarInterleaved2,
}

impl Backend {
    /// The default authority path.
    pub const fn authority() -> Self {
        Self::ScalarInterleaved2
    }

    /// The codec this backend emits/consumes.
    pub const fn codec(self) -> RansCodec {
        match self {
            Backend::ScalarSingle => RansCodec::Single,
            Backend::ScalarInterleaved2 => RansCodec::Interleaved2,
        }
    }

    /// Whether this backend is available on the current host (all scalar
    /// backends always are).
    pub fn available(self) -> bool {
        true
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Encode with an explicitly selected backend (scalar authority path).
pub fn encode_with_backend(
    input: &[u8],
    model: &RansModel,
    backend: Backend,
) -> Result<Vec<u8>, RansStreamError> {
    // The backend's codec must match the model's codec; enforce it.
    if model.codec != backend.codec() {
        return Err(RansStreamError::Model("backend/codec mismatch".into()));
    }
    encode_stream(input, model)
}

/// Decode with the codec recorded in the model.
pub fn decode(encoded: &[u8], model: &RansModel, out_len: u64) -> Result<Vec<u8>, RansStreamError> {
    decode_stream(model, encoded, out_len)
}

/// Report the active backend (for `capabilities` and `status`).
pub fn active_backend() -> Backend {
    Backend::authority()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rans::model::normalize_histogram;

    fn hist_of(data: &[u8]) -> [u32; 256] {
        let mut h = [0u32; 256];
        for &b in data {
            h[b as usize] += 1;
        }
        h
    }

    #[test]
    fn authority_roundtrip() {
        let data: Vec<u8> = (0..10000u32).map(|i| ((i * 7) % 41) as u8).collect();
        let model = normalize_histogram(&hist_of(&data), 14, Backend::authority().codec()).unwrap();
        let encoded = encode_with_backend(&data, &model, Backend::authority()).unwrap();
        let decoded = decode(&encoded, &model, data.len() as u64).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn backend_codec_mismatch_rejected() {
        let data = b"abc".to_vec();
        let model = normalize_histogram(&hist_of(&data), 14, RansCodec::Interleaved2).unwrap();
        assert!(encode_with_backend(&data, &model, Backend::ScalarSingle).is_err());
    }
}
