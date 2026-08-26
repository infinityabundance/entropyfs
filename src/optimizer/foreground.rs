//! Phase-10B: the foreground representation policy — how much search CPU
//! a write-path chunk deserves RIGHT NOW.
//!
//! `OptimizeOptions` describes which representations EXIST (the ablation
//! semantics, unchanged). `ForegroundPolicy` decides which families are
//! worth evaluating for an incoming chunk in the write path, where
//! latency is the product. The two are deliberately separate: ablations
//! construct `OptimizeOptions` and run with `ForegroundPolicy::full()`;
//! the mounted filesystem carries a policy chosen by the court.
//!
//! The 10A millisecond map measured the motivation: on incompressible
//! data the full foreground search spends ~440 µs/chunk in the LZ/entropy
//! families before RAW wins (sequence_rans 481 ms + sequence_dict 120 ms
//! over the court workload; direct-store 64 MiB random 37.7 MiB/s full
//! vs 592.7 MiB/s raw-only). A cheap probe classifies each chunk first:
//!
//! - obvious ZERO/FILL: the zero/fill candidates decide immediately (they
//!   are already evaluated first and are ~free);
//! - HIGH entropy (incompressible): dedup (CAS) + ZERO/FILL + RAW — the
//!   rANS/LZ/configurational families are skipped, because they cannot
//!   beat RAW on data the probe already knows is random;
//! - LOW/uncertain: the full foreground search runs as before.
//!
//! False negatives are harmless: RAW is exact, and the background
//! optimizer (full search) can revisit any extent later — the
//! foreground-state/settled-state distinction is exactly what makes this
//! asymmetry safe.
//!
//! PURPOSE
//!     Decide, per incoming write-path chunk, how much search CPU the
//!     representation search deserves right now — the foreground half of
//!     the foreground/settled division of labor.
//!
//! BOUNDARY
//!     Decides CPU budget only. Which families EXIST is `OptimizeOptions`
//!     (`optimizer::policy`, the ablation authority); this module never
//!     defines correctness, never touches the store, and never commits.
//!     The background optimizer (`optimizer::background`) is the other
//!     half: it may revisit any extent later, which is what makes the
//!     aggressive skips here safe.
//!
//! MODEL
//!     Two gates compose per chunk: the policy (this module) decides
//!     whether the full candidate search may run, and the options decide
//!     which families exist. Cheap mode classifies each chunk with a
//!     deterministic entropy probe before spending the expensive
//!     LZ/entropy searches (see the three classes above).
//!
//! PERSISTENT AUTHORITY
//!     None. A skip here changes only which candidate is chosen in
//!     memory; RAW is exact, so no on-disk representation depends on the
//!     policy.
//!
//! CORRECTNESS INVARIANTS
//!     - the probe is deterministic (fixed stride), so the classification
//!       is reproducible across runs;
//!     - false negatives are harmless: a chunk misclassified LOW costs
//!       CPU, never bytes; a chunk misclassified HIGH falls back to RAW,
//!       which is exact, and the background optimizer revisits it later;
//!     - anti-aliasing: the probe takes the MINIMUM entropy over three
//!       consecutive strides, so periodic data never looks random (a
//!       period p > 1 cannot divide three consecutive integers);
//!     - chunks smaller than 256 bytes always run the full search (the
//!       probe is unreliable and the families are cheap).
//!
//! CONCURRENCY
//!     Per-chunk and single-threaded; no locks. The probe reads only the
//!     chunk buffer handed to it.
//!
//! DURABILITY
//!     None: nothing here persists.
//!
//! RESOURCE BOUNDS
//!     The probe reads at most `probe_bytes` (default 4096) bytes of the
//!     chunk over three strides, so classification cost is
//!     O(probe_bytes) regardless of chunk size. The families themselves
//!     are bounded by the policy mode plus `OptimizeOptions`.
//!
//! PERFORMANCE
//!     The 10A millisecond map measured the motivation (above). The
//!     sealed 10B court pair (evidence `8062f2d` / `d38f73f`) measured
//!     the outcome: mounted random 64 MiB writes 66.5 → 229.3 MiB/s
//!     (3.4×), compressed.tgz 42.0 → 66.3 MiB/s, daemon CPU 0.41× →
//!     0.26× (−37%), and — the decisive number — settled density
//!     UNCHANGED at 1.994×, because the background optimizer recovers
//!     everything the cheap foreground defers. Direct-store random
//!     writes 39.8 → 852 MiB/s (21×).
//!
//! FAILURE MODES
//!     No hard failures: the only failure is a misclassification, which
//!     is bounded on both sides (wasted CPU, or a densification deferred
//!     to the background pass). The min-over-strides probe is the
//!     conservative direction — families are skipped only when EVERY
//!     stride looks high-entropy.
//!
//! HISTORY / EVIDENCE
//!     Phase-10B introduced `ForegroundMode::Cheap` (evidence `8062f2d` /
//!     `d38f73f`); the anti-aliasing min-over-strides was found by the
//!     periodic fixture in `entropy_classification_is_deterministic_and_sane`;
//!     `ForegroundMode::RawOnly` is the raw-only control arm.

#![forbid(unsafe_code)]

use crate::optimizer::policy::OptimizeOptions;

/// How much CPU the foreground search may spend on one chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundMode {
    /// Evaluate every family the configuration admits (the pre-10B
    /// behavior; the ablation default).
    Full,
    /// Probe first: high-entropy chunks skip the LZ/entropy families and
    /// go dedup + ZERO/FILL + RAW; structured/uncertain chunks get the
    /// full foreground search.
    Cheap,
    /// Hash → CAS → ZERO/FILL → RAW only (the raw-only control; the
    /// background optimizer still densifies later).
    RawOnly,
}

/// The foreground representation policy.
///
/// Role: the CPU-budget authority for one write-path chunk. It composes
/// with `OptimizeOptions` (the family authority): the policy says whether
/// the full search may run, the options say which families exist.
///
/// Invariants: `mode` is one of three sealed modes (the 10B comparison
/// arms); `high_entropy_bits` is Shannon entropy in bits per byte
/// (8.0 = uniform byte alphabet; compressed data sits near it;
/// source/text is typically 4–6); `probe_bytes` is the probe sample size
/// in bytes. A `Copy` value with no interior state, safe to share across
/// threads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForegroundPolicy {
    /// Search mode.
    pub mode: ForegroundMode,
    /// Entropy (bits per byte) at or above which a chunk is classified
    /// high-entropy and the LZ/entropy families are skipped (Cheap mode).
    /// Shannon entropy of a uniform byte alphabet is 8.0; compressed
    /// data sits near it. Source/text is typically 4–6.
    pub high_entropy_bits: f64,
    /// Probe sample size (bytes, deterministic stride over the chunk).
    pub probe_bytes: usize,
}

impl Default for ForegroundPolicy {
    fn default() -> Self {
        Self {
            mode: ForegroundMode::Full,
            high_entropy_bits: 7.2,
            probe_bytes: 4096,
        }
    }
}

impl ForegroundPolicy {
    /// The pre-10B policy: every family, always (ablation semantics).
    pub const fn full() -> Self {
        Self {
            mode: ForegroundMode::Full,
            high_entropy_bits: 7.2,
            probe_bytes: 4096,
        }
    }

    /// The cheap policy (10B): probe + skip hopeless families.
    ///
    /// Evidence (Phase-10B, sealed `8062f2d` / `d38f73f`): the
    /// high-entropy probe skips the LZ/entropy families for incompressible
    /// chunks, and the background optimizer recovers everything the cheap
    /// foreground defers — direct-store random writes 39.8 → 852 MiB/s
    /// (21×), mounted random 64 MiB writes 66.5 → 229.3 MiB/s (3.4×),
    /// daemon CPU −37%, settled density unchanged at 1.994×.
    pub const fn cheap() -> Self {
        Self {
            mode: ForegroundMode::Cheap,
            high_entropy_bits: 7.2,
            probe_bytes: 4096,
        }
    }

    /// The raw-only control policy.
    pub const fn raw_only() -> Self {
        Self {
            mode: ForegroundMode::RawOnly,
            high_entropy_bits: 7.2,
            probe_bytes: 4096,
        }
    }

    /// Whether the full candidate search may run for this chunk under
    /// this policy. The probe is deterministic (fixed stride), so the
    /// classification is reproducible across runs.
    pub fn allow_full_search(&self, chunk: &[u8]) -> bool {
        match self.mode {
            ForegroundMode::Full => true,
            ForegroundMode::RawOnly => false,
            ForegroundMode::Cheap => !high_entropy(chunk, self),
        }
    }

    /// Whether a family may even be evaluated (mode-level gate, kept
    /// separate so `OptimizeOptions` remains the family-authority).
    pub fn family_evaluations_allowed(&self) -> bool {
        self.mode != ForegroundMode::RawOnly
    }
}

/// Deterministic sampled Shannon entropy (bits per byte) of a chunk.
/// Samples `probe_bytes` bytes on a fixed stride across the whole chunk
/// so both small files and 64 KiB chunks are classified from their full
/// extent, not just their head.
///
/// Anti-aliasing (Phase-10B, found by test): a fixed stride can alias
/// with the data's periodicity and misestimate entropy — a 256-period
/// pattern at stride 16 samples one residue class and looks uniformly
/// random. The probe therefore takes the MINIMUM entropy over three
/// consecutive strides: a period `p > 1` cannot divide three consecutive
/// integers, so at least one stride breaks the alias. The minimum is
/// also the conservative direction (only skip the families when EVERY
/// stride looks high-entropy; a false low-entropy verdict just costs CPU,
/// never correctness). Pinned by the periodic fixture in
/// `entropy_classification_is_deterministic_and_sane`: a 256-period
/// uniform pattern must stay below the high-entropy threshold so the
/// configurational (periodic) family still gets evaluated.
pub fn sampled_entropy(chunk: &[u8], probe_bytes: usize) -> f64 {
    if chunk.is_empty() {
        return 0.0;
    }
    let n = probe_bytes.min(chunk.len());
    let base_step = (chunk.len() / n.max(1)).max(1);
    let mut best = f64::INFINITY;
    for shift in 0..3usize {
        let step = base_step.saturating_add(shift).max(1);
        let mut hist = [0u32; 256];
        let mut counted = 0usize;
        let mut i = 0usize;
        while i < chunk.len() && counted < n {
            hist[chunk[i] as usize] += 1;
            counted += 1;
            i += step;
        }
        if counted == 0 {
            continue;
        }
        let mut entropy = 0.0f64;
        for &c in &hist {
            if c == 0 {
                continue;
            }
            let p = c as f64 / counted as f64;
            entropy -= p * p.log2();
        }
        best = best.min(entropy);
    }
    if best.is_infinite() { 0.0 } else { best }
}

/// The 10B classification: high entropy (incompressible) — the LZ and
/// entropy families cannot beat RAW on such data.
///
/// Threshold: Shannon entropy (bits per byte) at or above
/// `policy.high_entropy_bits` (7.2 default). The 256-byte floor exists
/// because on tiny chunks the probe is unreliable and the families are
/// cheap — always run the full search (pinned by `tiny_chunks_never_skip`).
pub fn high_entropy(chunk: &[u8], policy: &ForegroundPolicy) -> bool {
    if chunk.len() < 256 {
        // Tiny chunks: the probe is unreliable and the families are
        // cheap; always run the full search.
        return false;
    }
    sampled_entropy(chunk, policy.probe_bytes) >= policy.high_entropy_bits
}

/// True when a chunk is obviously degenerate (single symbol): the
/// ZERO/FILL candidates decide it without any further search.
pub fn is_degenerate(chunk: &[u8]) -> bool {
    let Some(&first) = chunk.first() else {
        return true;
    };
    chunk.iter().all(|&b| b == first)
}

/// The families the foreground search may evaluate for a chunk, given
/// the policy and the configuration (the two gates compose: the policy
/// decides CPU budget, the options decide what exists).
pub fn foreground_allows(
    options: &OptimizeOptions,
    policy: &ForegroundPolicy,
    chunk: &[u8],
) -> ForegroundFamilySet {
    if !policy.allow_full_search(chunk) {
        // High-entropy / raw-only: dedup + ZERO/FILL + RAW only. The
        // families are skipped entirely; RAW is exact and the background
        // optimizer revisits later (foreground vs settled state).
        return ForegroundFamilySet {
            dedup: true,
            zero_fill: true,
            configurational: false,
            byte_rans: false,
            sequence_rans: false,
            sequence_deep: false,
            sequence_dict: false,
            shared_dict: false,
            bases: false,
            universe: false,
        };
    }
    ForegroundFamilySet {
        dedup: true,
        zero_fill: true,
        configurational: options.allow_configurational,
        byte_rans: options.allow_byte_rans,
        sequence_rans: options.allow_sequence_rans,
        sequence_deep: options.allow_sequence_rans_deep,
        sequence_dict: options.allow_sequence_dict,
        shared_dict: options.allow_shared_dict,
        bases: options.allow_bases,
        universe: options.allow_universe,
    }
}

/// Which families the foreground may evaluate for one chunk.
///
/// Role: the materialized decision `foreground_allows` computes for one
/// chunk — the policy gate applied on top of the options gate. Produced
/// by `foreground_allows` (or `unrestricted()` where CPU is not the
/// product, e.g. the background/guided search), never hand-assembled in
/// the write path.
///
/// Invariant: `dedup` and `zero_fill` are always true in the write path
/// (exact dedup is a store invariant; ZERO/FILL decide immediately); the
/// rest follow `OptimizeOptions` when the policy admits the full search.
#[derive(Debug, Clone, Copy)]
pub struct ForegroundFamilySet {
    /// Exact dedup (P2) — always allowed in the write path.
    pub dedup: bool,
    /// ZERO/FILL structural candidates.
    pub zero_fill: bool,
    /// Sparse / palette / periodic / sparse64 configurational encoders.
    pub configurational: bool,
    /// Byte-level rANS.
    pub byte_rans: bool,
    /// SequenceRans (local-match + rANS floor).
    pub sequence_rans: bool,
    /// SequenceDeep (background-only family; harmless to leave on).
    pub sequence_deep: bool,
    /// SequenceDict (previous same-file chunk dictionary).
    pub sequence_dict: bool,
    /// SequenceSharedDict (cross-file shared dictionary).
    pub shared_dict: bool,
    /// Base+residual channels (P0/P1/P3/P4).
    pub bases: bool,
    /// Entropy-universe negative control (P5).
    pub universe: bool,
}

impl ForegroundFamilySet {
    /// The unrestricted set (all families the options admit).
    pub fn unrestricted() -> Self {
        Self {
            dedup: true,
            zero_fill: true,
            configurational: true,
            byte_rans: true,
            sequence_rans: true,
            sequence_deep: true,
            sequence_dict: true,
            shared_dict: true,
            bases: true,
            universe: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_classification_is_deterministic_and_sane() {
        let zeros = vec![0u8; 65536];
        // Genuine randomness (splitmix64): every stride samples uniform
        // bytes, so the min-over-strides reads ~8 bits and the chunk is
        // classified high-entropy -> the LZ/entropy families are skipped.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let random: Vec<u8> = (0..65536u32)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect();
        // A 256-period uniform pattern: the full-chunk byte distribution
        // is uniform (8 bits), but the pattern is PERIODIC and therefore
        // compressible — the anti-aliasing min-over-strides must keep it
        // OUT of the high-entropy skip (the periodic family wins instead).
        let periodic: Vec<u8> = (0..65536u32)
            .map(|i| (i.wrapping_mul(2654435761)) as u8)
            .collect();
        let text: Vec<u8> = (0..65536u32).map(|i| b'a' + (i % 26) as u8).collect();
        let p = ForegroundPolicy::cheap();
        let e0 = sampled_entropy(&zeros, 4096);
        let er = sampled_entropy(&random, 4096);
        let ep = sampled_entropy(&periodic, 4096);
        let et = sampled_entropy(&text, 4096);
        assert_eq!(e0, 0.0, "zeros are zero-entropy");
        assert!(er >= 7.9, "true random is near 8 bits/byte (got {er})");
        assert!(ep < 6.0, "periodic data must not look random (got {ep})");
        assert!(et < 5.0, "text is low-entropy (got {et})");
        assert!(high_entropy(&random, &p));
        assert!(!high_entropy(&text, &p));
        assert!(!high_entropy(&zeros, &p));
        assert!(
            !high_entropy(&periodic, &p),
            "periodic data stays in the full search"
        );
        // Determinism.
        assert_eq!(sampled_entropy(&random, 4096), er);
    }

    #[test]
    fn tiny_chunks_never_skip() {
        let p = ForegroundPolicy::cheap();
        assert!(!high_entropy(&[7u8; 100], &p));
        assert!(p.allow_full_search(&[7u8; 100]));
    }
}
