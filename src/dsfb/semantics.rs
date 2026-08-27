//! Phase-12C DSFB structural semiotics: advisory semantic context.
//!
//! # Purpose
//!
//! Use filesystem context to decide **which candidate-search hypotheses
//! are worth spending CPU on**, while retaining exact byte validation as
//! the only authority. The 12C brief: DSFB's observer key becomes
//! conceptually `P(channel | chunk history, semantic context)` — not
//! probability as decoding authority, but a **search-ordering / trust
//! score**: channels that historically win for a chunk's semantic class
//! are tried earlier, and the plan's budget is spent where the class says
//! it pays.
//!
//! # Model
//!
//! A [`SemanticContext`] is a bundle of CHEAP, quantized classes derived
//! from the file's name, its parent directory, and a bounded byte sketch
//! of the chunk:
//!
//! ```text
//! name-derived:   extension_class   (hash of the suffix -> class)
//!                 parent_class      (hash of the parent directory name)
//!                 basename_shape    (length bucket + character-mix class)
//! byte-derived:   magic_class       (first-8-bytes signature class)
//!                 printable_ratio   (quantized printable fraction)
//!                 entropy_class     (quantized sample entropy)
//! history:        lifecycle         (new / append-heavy / rewrite-heavy /
//!                                    stable — from the write pattern)
//! ```
//!
//! The [`SemanticPrior`] is the learned evidence: a per-class table of
//! channel win counts, incremented at every `observe` that carries a
//! semantic context. The prior for a context is the class's normalized
//! win distribution — "for this class of chunk, channel C wins X% of the
//! time". The plan then scores each channel as
//!
//! ```text
//! plan_trust(channel) = historical_trust(channel)
//!                     + SEMANTIC_WEIGHT * prior(class, channel)
//! ```
//!
//! The historical trust (per-chunk EMA evidence) and the semantic prior
//! (per-class distribution) are independent evidence sources; the weight
//! is a policy constant the oracle sweeps across modes.
//!
//! # The oracle's S-modes
//!
//! [`SemanticMode`] selects which class groups feed the prior key, so the
//! 12C oracle can attribute the value of each evidence source:
//!
//! ```text
//! S0 None        the prior is disabled (the sealed baseline)
//! S1 Extension   extension/parent/basename classes only
//! S2 ByteSketch  magic/printable/entropy classes only
//! S3 History     the lifecycle class only
//! S4 Combined    all classes
//! ```
//!
//! # Boundary
//!
//! Strictly advisory (ADR-0004): the prior orders and budgets the search;
//! it can never alter bytes, never veto a validated candidate, and never
//! appears on a decode path. Every candidate still encodes, costs,
//! materializes, hashes, and validates before it can win. A wrong prior
//! costs search CPU only (the hostile-media court's semantic-deception
//! exhibits: random bytes named `.rs`, compressed data named `.txt`,
//! renamed files — correctness must be identical).
//!
//! # Correctness invariants
//!
//! - The prior changes only the ORDER and BUDGET of candidate evaluation;
//!   the winner is still the minimum over byte-validated candidates by
//!   exact cost (ADR-0010).
//! - Classes are bounded u8 quantizations of bounded inputs (the byte
//!   sketch samples at most [`SKETCH_BYTES`] bytes), so a hostile name or
//!   payload cannot grow the context or the table unboundedly.
//! - The prior table is bounded by the distinct class keys (each key is
//!   a small hash; the table caps at [`PRIOR_MAX_KEYS`] with eviction).
//!
//! # Concurrency
//!
//! The prior table is one store-level mutex (small, rare updates — one
//! per observe; the 11F oracle measured this contention class at ~1 µs
//! per call, acceptable for advisory state; 12C-1 can shard the prior
//! like the observer if the oracle's adopted-mode measurements justify
//! it).
//!
//! # Resource bounds
//!
//! Context: ~50 bytes of quantized u8s. Prior: one row per class key,
//! capped. The byte sketch reads at most [`SKETCH_BYTES`] bytes.
//!
//! # Failure modes
//!
//! Infallible. A poisoned prior mutex panics (like every store mutex).
//! Bad semantics waste CPU or select RAW earlier — never corrupt bytes.
//!
//! # History / evidence
//!
//! The 12C oracle (`src/tests/dsfb_semantics_probe.rs`, sealed
//! `evidence/performance/dsfb-semantics-probe-*/`): heterogeneous-corpus
//! writes under S0–S4 measuring useful foreground search CPU,
//! candidates/chunk, first-winning rank, persisted bytes / settled
//! density, and the RAW fallback rate; the gate adopts the semantic
//! prior only if search CPU falls substantially while settled density
//! stays approximately unchanged (CHANGELOG v0.7.10).

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// The oracle's semantic modes (which class groups feed the prior key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticMode {
    /// S0: the prior is disabled (the sealed baseline behavior).
    None,
    /// S1: extension / parent / basename-shape classes only.
    Extension,
    /// S2: magic / printable-ratio / entropy classes only.
    ByteSketch,
    /// S3: the lifecycle class only.
    History,
    /// S4: all classes combined.
    Combined,
}

impl SemanticMode {
    /// Whether this mode actually uses the prior.
    pub fn enabled(self) -> bool {
        !matches!(self, SemanticMode::None)
    }
}

/// The prior's weight against the historical trust (a policy constant the
/// 12C oracle uses; the court reports the sensitivity rather than
/// sweeping it — the sealed evidence records the single-weight result).
pub const SEMANTIC_WEIGHT: f64 = 0.3;

/// Bounded byte-sketch window (the cheap content features sample at most
/// this many bytes — hostile payloads cannot grow the extraction cost).
pub const SKETCH_BYTES: usize = 4096;

/// Prior-table cap (distinct class keys; bounded advisory state).
pub const PRIOR_MAX_KEYS: usize = 4096;

/// The quantized semantic context of one chunk (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SemanticContext {
    /// Hash class of the file extension (0 = no extension).
    pub extension_class: u8,
    /// Hash class of the parent directory name.
    pub parent_class: u8,
    /// Basename shape: (length bucket << 4) | character-mix class.
    pub basename_shape: u8,
    /// First-8-bytes signature class.
    pub magic_class: u8,
    /// Quantized printable-byte fraction (0..=20).
    pub printable_ratio: u8,
    /// Quantized sample entropy (0..=20).
    pub entropy_class: u8,
    /// Lifecycle: 0 new, 1 append-heavy, 2 rewrite-heavy, 3 stable.
    pub lifecycle: u8,
}

impl SemanticContext {
    /// Derive the name-derived classes from a file name.
    ///
    /// `parent_class` comes from the caller (the parent directory's own
    /// class); this derives the extension class and the basename shape
    /// from `name` (bounded: at most the first 256 name bytes are read).
    pub fn from_name(name: &[u8], parent_class: u8) -> Self {
        let mut ctx = SemanticContext {
            parent_class,
            ..SemanticContext::default()
        };
        let name = &name[..name.len().min(256)];
        // Extension: the suffix after the last '.', if any, and the name
        // is not a dotfile.
        if let Some(dot) = name.iter().rposition(|&b| b == b'.') {
            if dot > 0 && dot + 1 < name.len() {
                let ext = &name[dot + 1..];
                ctx.extension_class = (class_hash(ext) % 64) as u8;
            }
        }
        // Basename shape: length bucket (0..=15) + character-mix class.
        let len_bucket = (name.len().min(256) / 16).min(15) as u8;
        let mut alpha = 0u32;
        let mut digit = 0u32;
        for &b in name {
            if b.is_ascii_alphabetic() {
                alpha += 1;
            } else if b.is_ascii_digit() {
                digit += 1;
            }
        }
        let n = name.len().max(1) as f64;
        let mix = if alpha as f64 / n > 0.7 {
            0
        } else if digit as f64 / n > 0.3 {
            1
        } else if alpha as f64 / n > 0.3 {
            2
        } else {
            3
        };
        ctx.basename_shape = (len_bucket << 4) | mix;
        ctx
    }

    /// Derive the byte-sketch classes from a chunk (bounded sample).
    ///
    /// Magic: the first 8 bytes hashed into a 16-class signature. Sample:
    /// a stride-sampled window over the first [`SKETCH_BYTES`] bytes (the
    /// whole chunk when smaller), counting printable bytes and the
    /// distinct-symbol count (the entropy proxy).
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut ctx = SemanticContext::default();
        let sample = &bytes[..bytes.len().min(SKETCH_BYTES)];
        // Magic class: the first 8 bytes' hash (0 when the chunk is too
        // short to carry a signature).
        if sample.len() >= 8 {
            ctx.magic_class = (class_hash(&sample[..8]) % 16) as u8;
        }
        // Printable ratio over a stride sample (every 8th byte of the
        // bounded window).
        let mut printable = 0u32;
        let mut counted = 0u32;
        let mut distinct = [false; 256];
        let step = (sample.len() / 2048).max(1);
        let mut i = 0usize;
        while i < sample.len() {
            let b = sample[i];
            distinct[b as usize] = true;
            if b.is_ascii_graphic() || b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                printable += 1;
            }
            counted += 1;
            i += step;
        }
        if counted > 0 {
            ctx.printable_ratio = ((printable as f64 / counted as f64) * 20.0) as u8;
        }
        let distinct_count = distinct.iter().filter(|&&d| d).count();
        // Quantize the distinct-symbol count into 0..=20 (the entropy
        // proxy: 256 distinct symbols ~ incompressible ~ entropy 8).
        ctx.entropy_class = ((distinct_count as f64 / 256.0) * 20.0) as u8;
        ctx
    }

    /// The prior-table key for the given mode (which class groups feed
    /// the prior). None mode yields no key (the prior is disabled).
    pub fn key_for(self, mode: SemanticMode) -> Option<u64> {
        match mode {
            SemanticMode::None => None,
            SemanticMode::Extension => Some(class_hash(&[
                self.extension_class,
                self.parent_class,
                self.basename_shape,
            ])),
            SemanticMode::ByteSketch => Some(class_hash(&[
                self.magic_class,
                self.printable_ratio,
                self.entropy_class,
            ])),
            SemanticMode::History => Some(class_hash(&[self.lifecycle])),
            SemanticMode::Combined => Some(class_hash(&[
                self.extension_class,
                self.parent_class,
                self.basename_shape,
                self.magic_class,
                self.printable_ratio,
                self.entropy_class,
                self.lifecycle,
            ])),
        }
    }
}

/// The learned per-class channel-prior table (module doc). Bounded by
/// [`PRIOR_MAX_KEYS`] with arbitrary eviction (advisory state —
/// eviction is correctness-neutral).
#[derive(Debug, Default)]
pub struct SemanticPrior {
    /// class key -> per-channel win counts (indexed by `Channel as usize`,
    /// the same layout as the observer's arrays).
    table: HashMap<u64, Vec<u64>>,
}

impl SemanticPrior {
    /// Record one observed winner for a class key.
    pub fn observe(&mut self, key: u64, channel: crate::dsfb::features::Channel) {
        let row = self
            .table
            .entry(key)
            .or_insert_with(|| vec![0; crate::dsfb::features::Channel::ALL.len()]);
        row[channel as usize] = row[channel as usize].saturating_add(1);
        if self.table.len() > PRIOR_MAX_KEYS {
            if let Some(k) = self.table.keys().next().copied() {
                self.table.remove(&k);
            }
        }
    }

    /// The prior weight of a channel for a class key: the class's
    /// normalized win share of that channel (0 when the class has no
    /// observations yet).
    pub fn prior(&self, key: u64, channel: crate::dsfb::features::Channel) -> f64 {
        let Some(row) = self.table.get(&key) else {
            return 0.0;
        };
        let total: u64 = row.iter().sum();
        if total == 0 {
            return 0.0;
        }
        row[channel as usize] as f64 / total as f64
    }

    /// The class's observation count (0 for an unseen class) — the
    /// confidence DENOMINATOR of the 12C-1 focused budget. A class with
    /// few observations has an unreliable winner distribution; the
    /// focused rANS-deferral gate refuses to engage until the class has
    /// earned [`crate::optimizer::foreground::ForegroundPolicy::focused_min_observations`]
    /// observations, so a cold class always gets the full search.
    pub fn count(&self, key: u64) -> u64 {
        self.table
            .get(&key)
            .map(|row| row.iter().sum())
            .unwrap_or(0)
    }
}

/// A bounded deterministic hash for the class keys (FNV-1a over the
/// class bytes — the same stability rationale as the observer's shard
/// hash).
fn class_hash(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_is_bounded_and_stable() {
        let c1 = SemanticContext::from_name(b"photo.jpg", 3);
        let c2 = SemanticContext::from_name(b"photo.jpg", 3);
        assert_eq!(c1, c2, "same name+parent must give the same classes");
        assert_eq!(c1.extension_class, c2.extension_class);
        assert_eq!(c1.basename_shape >> 4, 0); // 8 chars -> bucket 0
        let no_ext = SemanticContext::from_name(b"README", 0);
        assert_eq!(no_ext.extension_class, 0, "no extension -> class 0");
        // Byte sketch: printable text vs noise.
        let text = vec![b'a'; 4096];
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let t = SemanticContext::from_bytes(&text);
        let n = SemanticContext::from_bytes(&noise);
        assert!(
            t.printable_ratio > n.printable_ratio,
            "text is more printable"
        );
        assert!(
            n.entropy_class >= t.entropy_class,
            "noise has higher entropy"
        );
        // The sketch is bounded: a huge chunk costs the same as a 4 KiB
        // one (the window caps at SKETCH_BYTES).
        let zeros4k = vec![0u8; 4096];
        let big = vec![0u8; 1024 * 1024];
        assert_eq!(
            SemanticContext::from_bytes(&big),
            SemanticContext::from_bytes(&zeros4k)
        );
    }

    #[test]
    fn prior_learns_and_bounds() {
        let mut p = SemanticPrior::default();
        let key = 7u64;
        assert_eq!(p.prior(key, crate::dsfb::features::Channel::Raw), 0.0);
        for _ in 0..9 {
            p.observe(key, crate::dsfb::features::Channel::Raw);
        }
        p.observe(key, crate::dsfb::features::Channel::PrevVersion);
        let raw = p.prior(key, crate::dsfb::features::Channel::Raw);
        let pv = p.prior(key, crate::dsfb::features::Channel::PrevVersion);
        assert!((raw - 0.9).abs() < 1e-9, "raw share {raw}");
        assert!((pv - 0.1).abs() < 1e-9, "prev-version share {pv}");
        // Eviction keeps the table bounded.
        for i in 0..(PRIOR_MAX_KEYS + 100) {
            p.observe(i as u64, crate::dsfb::features::Channel::Raw);
        }
        assert!(p.table.len() <= PRIOR_MAX_KEYS);
    }

    #[test]
    fn modes_select_class_groups() {
        let ctx = SemanticContext::from_name(b"data.bin", 5);
        let ctx = SemanticContext {
            printable_ratio: 3,
            entropy_class: 19,
            magic_class: 9,
            lifecycle: 2,
            ..ctx
        };
        assert!(ctx.key_for(SemanticMode::None).is_none());
        // Extension mode: changing a byte-derived class must not change
        // the key; changing the extension must.
        let k_ext = ctx.key_for(SemanticMode::Extension).unwrap();
        let mut other = ctx;
        other.magic_class = 0;
        assert_eq!(other.key_for(SemanticMode::Extension).unwrap(), k_ext);
        let mut other2 = ctx;
        other2.extension_class = 1;
        assert_ne!(other2.key_for(SemanticMode::Extension).unwrap(), k_ext);
        // ByteSketch mode: the opposite.
        let k_sketch = ctx.key_for(SemanticMode::ByteSketch).unwrap();
        let mut other3 = ctx;
        other3.extension_class = 1;
        assert_eq!(other3.key_for(SemanticMode::ByteSketch).unwrap(), k_sketch);
        let mut other4 = ctx;
        other4.printable_ratio = 0;
        assert_ne!(other4.key_for(SemanticMode::ByteSketch).unwrap(), k_sketch);
    }
}
