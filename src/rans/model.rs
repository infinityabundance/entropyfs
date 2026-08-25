//! Canonical rANS model construction: deterministic histogram
//! normalization (`docs/theory/rans-state.md` §3).
//!
//! The normalization is a pure function of the histogram: encode and
//! decode rebuild identical symbol tables from the persisted frequencies.
//! No floating point anywhere in the model path.

#![forbid(unsafe_code)]

use crate::core::representation::RansCodec;

/// v1 supported scale bits: 8..=15 (frequencies must fit `u16`; at
/// `scale_bits == 16` a dominant symbol can reach 65536 which does not fit).
pub const MIN_SCALE_BITS: u8 = 8;
pub const MAX_SCALE_BITS: u8 = 15;
/// Default scale bits (16384 total frequency).
pub const DEFAULT_SCALE_BITS: u8 = 14;

/// A canonical 256-symbol rANS model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RansModel {
    /// Frequency scale bits.
    pub scale_bits: u8,
    /// Codec used for streams under this model.
    pub codec: RansCodec,
    /// Normalized frequencies (sum == 1 << scale_bits).
    pub freqs: [u16; 256],
}

impl RansModel {
    /// Scale total (1 << scale_bits).
    pub fn total(&self) -> u32 {
        1u32 << self.scale_bits
    }

    /// Entropy estimate in bits per symbol over the normalized
    /// frequencies: `Σ -p·log2(p)`.
    pub fn entropy_bits_per_symbol(&self) -> f64 {
        let total = self.total() as f64;
        let mut h = 0.0f64;
        for &f in self.freqs.iter() {
            if f > 0 {
                let p = f as f64 / total;
                h -= p * p.log2();
            }
        }
        h
    }

    /// Expected encoded length in bytes for `n` symbols: `ceil(n·H/8)`.
    /// `None` when the model is degenerate (single symbol).
    pub fn expected_encoded_len(&self, n: u64) -> Option<u64> {
        let h = self.entropy_bits_per_symbol();
        if h <= 0.001 {
            return None;
        }
        Some((n as f64 * h / 8.0).ceil() as u64)
    }

    /// Number of distinct symbols with nonzero frequency.
    pub fn distinct_symbols(&self) -> usize {
        self.freqs.iter().filter(|&&f| f > 0).count()
    }

    /// Build validated encoder symbols (one slot per byte value; zero-
    /// frequency symbols are `None` and must never be encoded — the encode
    /// loop only encounters bytes present in the input, which have nonzero
    /// frequency).
    pub fn build_enc_symbols(
        &self,
    ) -> Result<Vec<Option<ryg_rans_rs::byte::RansByteEncSymbol>>, ryg_rans_rs::byte::ModelError>
    {
        let mut start: u32 = 0;
        let mut out = vec![None; 256];
        for (i, &f) in self.freqs.iter().enumerate() {
            if f == 0 {
                continue;
            }
            let sym =
                ryg_rans_rs::byte::RansByteEncSymbol::new(start, f as u32, self.scale_bits as u32)?;
            start += f as u32;
            out[i] = Some(sym);
        }
        Ok(out)
    }

    /// Build validated decoder symbols (same slot convention).
    pub fn build_dec_symbols(
        &self,
    ) -> Result<Vec<Option<ryg_rans_rs::byte::RansByteDecSymbol>>, ryg_rans_rs::byte::ModelError>
    {
        let mut start: u32 = 0;
        let mut out = vec![None; 256];
        for (i, &f) in self.freqs.iter().enumerate() {
            if f == 0 {
                continue;
            }
            let sym = ryg_rans_rs::byte::RansByteDecSymbol::new(start, f as u32)?;
            start += f as u32;
            out[i] = Some(sym);
        }
        Ok(out)
    }

    /// Validate model invariants (used before decode and in fsck).
    pub fn validate(&self) -> Result<(), ModelValidationError> {
        if !(MIN_SCALE_BITS..=MAX_SCALE_BITS).contains(&self.scale_bits) {
            return Err(ModelValidationError::BadScaleBits);
        }
        let total = self.total() as u64;
        let mut sum: u64 = 0;
        let mut any = false;
        for &f in self.freqs.iter() {
            if f == 0 {
                continue;
            }
            any = true;
            if f > 32768 {
                return Err(ModelValidationError::FrequencyTooLarge);
            }
            sum += f as u64;
        }
        if !any {
            return Err(ModelValidationError::EmptyModel);
        }
        if sum != total {
            return Err(ModelValidationError::TotalMismatch { sum, total });
        }
        Ok(())
    }
}

/// Model validation errors (typed; never panics on corrupt models).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelValidationError {
    /// scale_bits outside 8..=15.
    BadScaleBits,
    /// A frequency exceeds 32768.
    FrequencyTooLarge,
    /// Model with no symbols.
    EmptyModel,
    /// Frequencies do not sum to 1 << scale_bits.
    TotalMismatch { sum: u64, total: u64 },
}

impl std::fmt::Display for ModelValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ModelValidationError {}

/// Deterministic normalization of a 256-bin histogram into a canonical
/// model (`docs/theory/rans-state.md` §3).
///
/// Algorithm:
/// 1. floor scale: `f_i = (h_i · 2^s) / T`;
/// 2. every present symbol gets at least 1 (stealing from the largest
///    frequency when the residual does not cover the deficit);
/// 3. the remaining residual (`2^s − Σf`) is distributed to the symbols
///    with the largest rounding error `(h_i · 2^s) % T`, ties broken by
///    ascending symbol index.
///
/// Returns `None` when the histogram is degenerate (≤ 1 distinct symbol —
/// ZERO/FILL/PERIODIC handle those).
pub fn normalize_histogram(
    hist: &[u32; 256],
    scale_bits: u8,
    codec: RansCodec,
) -> Option<RansModel> {
    if !(MIN_SCALE_BITS..=MAX_SCALE_BITS).contains(&scale_bits) {
        return None;
    }
    let total: u64 = hist.iter().map(|&h| h as u64).sum();
    if total == 0 {
        return None;
    }
    let distinct = hist.iter().filter(|&&h| h > 0).count();
    if distinct <= 1 {
        return None;
    }
    let scale: u64 = 1u64 << scale_bits;

    // 1. floor scaling
    let mut f = [0u64; 256];
    let mut error = [0u64; 256];
    for i in 0..256 {
        f[i] = (hist[i] as u64 * scale) / total;
        error[i] = (hist[i] as u64 * scale) % total;
    }

    // 2. every present symbol >= 1
    //    steal order: repeatedly from the current largest frequency
    //    (deterministic tie-break: smaller index).
    let mut rem = scale - f.iter().sum::<u64>();
    for i in 0..256 {
        if hist[i] > 0 && f[i] == 0 {
            if rem > 0 {
                f[i] += 1;
                rem -= 1;
            } else {
                let target = (0..256)
                    .filter(|&j| f[j] > 1)
                    .max_by_key(|&j| (f[j], std::cmp::Reverse(j)));
                let target = match target {
                    Some(t) => t,
                    None => return None, // cannot happen: scale >= distinct
                };
                f[target] -= 1;
                f[i] = 1;
            }
        }
    }

    // 3. distribute remaining residual by largest rounding error
    if rem > 0 {
        let mut order: Vec<usize> = (0..256).filter(|&i| hist[i] > 0).collect();
        order.sort_by(|&a, &b| error[b].cmp(&error[a]).then(a.cmp(&b)));
        for &i in order.iter().take(rem as usize) {
            f[i] += 1;
        }
    }

    let mut freqs = [0u16; 256];
    for i in 0..256 {
        debug_assert!(f[i] <= 32768, "frequency exceeds u16-safe range");
        freqs[i] = f[i] as u16;
    }
    let model = RansModel {
        scale_bits,
        codec,
        freqs,
    };
    debug_assert_eq!(model.validate().map(|_| ()), Ok(()));
    Some(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist_of(data: &[u8]) -> [u32; 256] {
        let mut h = [0u32; 256];
        for &b in data {
            h[b as usize] += 1;
        }
        h
    }

    #[test]
    fn normalization_sums_to_scale() {
        for scale_bits in MIN_SCALE_BITS..=MAX_SCALE_BITS {
            let data: Vec<u8> = (0..20000u32).map(|i| (i % 37) as u8).collect();
            let m =
                normalize_histogram(&hist_of(&data), scale_bits, RansCodec::Interleaved2).unwrap();
            m.validate().unwrap();
            assert_eq!(m.total(), 1u32 << scale_bits);
        }
    }

    #[test]
    fn normalization_deterministic() {
        let data: Vec<u8> = (0..50000u32).map(|i| ((i * 7) % 256) as u8).collect();
        let h = hist_of(&data);
        let a = normalize_histogram(&h, 14, RansCodec::Single).unwrap();
        let b = normalize_histogram(&h, 14, RansCodec::Single).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn every_present_symbol_has_frequency() {
        // heavily skewed: one symbol dominates; scale_bits = 8 forces
        // aggressive zero-frequency theft.
        let mut data = vec![0u8; 65536];
        let mut present = [false; 256];
        for i in 0..65536 {
            data[i] = if i % 1000 == 0 {
                present[(i / 1000) as usize] = true;
                (i / 1000) as u8
            } else {
                present[7] = true;
                7
            };
        }
        let m = normalize_histogram(&hist_of(&data), 8, RansCodec::Single).unwrap();
        for i in 0..256 {
            if present[i] {
                assert!(m.freqs[i] > 0, "symbol {i} present but zero frequency");
            }
        }
        m.validate().unwrap();
    }

    #[test]
    fn degenerate_models_rejected() {
        assert!(normalize_histogram(&hist_of(&[0u8; 100]), 14, RansCodec::Single).is_none());
        assert!(normalize_histogram(&[0u32; 256], 14, RansCodec::Single).is_none());
        assert!(normalize_histogram(&hist_of(&[1u8; 10]), 20, RansCodec::Single).is_none());
    }

    #[test]
    fn entropy_of_uniform_is_8() {
        let data: Vec<u8> = (0..65536u32).map(|i| (i % 256) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let h = m.entropy_bits_per_symbol();
        assert!((h - 8.0).abs() < 0.01, "h = {h}");
    }

    #[test]
    fn expected_len_low_for_skewed() {
        let mut data = vec![0u8; 65536];
        for i in 0..65536 {
            data[i] = if i % 10 == 0 { 1 } else { 0 };
        }
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let expected = m.expected_encoded_len(65536).unwrap();
        assert!(expected < 65536, "expected {expected}");
        // ~0.47 bits/symbol ⇒ ~3.9 KB
        assert!(expected < 10_000);
    }

    #[test]
    fn symbols_rebuild() {
        let data: Vec<u8> = (0..20000u32).map(|i| (i % 53) as u8).collect();
        let m = normalize_histogram(&hist_of(&data), 14, RansCodec::Single).unwrap();
        let esyms = m.build_enc_symbols().unwrap();
        let dsyms = m.build_dec_symbols().unwrap();
        assert_eq!(esyms.len(), 256);
        assert_eq!(dsyms.len(), 256);
    }
}
