//! Feature bit sets (compat / ro_compat / incompat) and mount
//! compatibility checks (Phase 12E.3; `docs/format/compatibility.md`,
//! `docs/format/feature-registry.md`).
//!
//! # PURPOSE
//!
//! The normative compatibility contract of the on-disk format v1: three
//! independent bit sets mirroring ext4/btrfs semantics, plus the typed
//! decision machinery that maps a superblock's bits to an open decision.
//!
//! # BOUNDARY
//!
//! KNOWS: the bit registry, the three sets, and the compatibility rules.
//! NEVER KNOWS: the store, the CLI, or any read/write path — callers
//! (store open, fsck, mount) apply the decision.
//!
//! # RULES (Phase 12E.3 — normative, implementation-tested)
//!
//! ```text
//! unknown COMPAT    -> may be ignored (feature simply inactive)
//! unknown RO_COMPAT -> refuse WRITABLE open; permit READ-ONLY open
//! unknown INCOMPAT  -> refuse open entirely
//! known retired representation -> decoder remains able to read it
//! new encoder release -> may stop EMITTING an old representation
//! semantic break inexpressible through flags -> new format major
//! ```
//!
//! The `ro_compat` rule is the Phase-12E.3 fix: before this phase the
//! implementation refused ALL nonzero `ro_compat` (including the defined
//! `ENCRYPTED` bit) even though the documented contract and fsck's
//! `ReadOnlyOnly` warning path already assumed the read-only fallback.
//! The documented contract is the desired contract; the implementation
//! now honors it, and `Store::open(read_only = true)` exercises the
//! fallback.

#![forbid(unsafe_code)]

/// Feature bit registry. Each bit position maps to a documented feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Feature {
    /// Chunk class 4 KiB present in the store (incompat).
    Chunk4K = 1,
    /// Chunk class 16 KiB present (incompat).
    Chunk16K = 2,
    /// Chunk class 256 KiB present (incompat).
    Chunk256K = 3,
    /// ENTROPY_REF descriptors present (incompat).
    EntropyRef = 4,
    /// PALETTE descriptors present (incompat).
    Palette = 5,
    /// PERMUTATION descriptors present (incompat).
    Permutation = 6,
    /// Encrypted record payloads (ro_compat) — mount ro without key.
    Encrypted = 7,
    /// Derived extent-delta index present (compat, disposable).
    ExtentDeltaIndex = 8,
    /// Optimizer rewrite history markers present (compat).
    OptimizerRewrite = 9,
    /// SEQUENCE_RANS descriptors present (incompat).
    SequenceRans = 10,
    /// SPARSE_BLOCK64 descriptors present (incompat).
    SparseBlock64 = 11,
    /// SEQUENCE_DICT descriptors present (incompat).
    SequenceDict = 12,
    /// SEQUENCE_SHARED_DICT descriptors present (incompat).
    SequenceSharedDict = 13,
    /// SEQUENCE_DEEP descriptors present (incompat).
    SequenceDeep = 14,
    /// MUTATION_LOG records present (Phase-10D metadata writeback epoch;
    /// incompat — an implementation that cannot replay the log must refuse
    /// the store).
    MutationLog = 15,
}

impl Feature {
    /// Which feature set this bit belongs to.
    pub const fn set(self) -> FeatureSetKind {
        match self {
            Feature::Chunk4K
            | Feature::Chunk16K
            | Feature::Chunk256K
            | Feature::EntropyRef
            | Feature::Palette
            | Feature::Permutation
            | Feature::SequenceRans
            | Feature::SparseBlock64
            | Feature::SequenceDict
            | Feature::SequenceSharedDict
            | Feature::SequenceDeep
            | Feature::MutationLog => FeatureSetKind::Incompat,
            Feature::Encrypted => FeatureSetKind::RoCompat,
            Feature::ExtentDeltaIndex | Feature::OptimizerRewrite => FeatureSetKind::Compat,
        }
    }

    /// Bit mask for this feature.
    pub const fn mask(self) -> u64 {
        1u64 << (self as u8 - 1)
    }

    /// All defined features, ordered by bit position (drives the
    /// normative registry doc and any exhaustive tooling).
    pub const ALL: [Feature; 15] = [
        Feature::Chunk4K,
        Feature::Chunk16K,
        Feature::Chunk256K,
        Feature::EntropyRef,
        Feature::Palette,
        Feature::Permutation,
        Feature::Encrypted,
        Feature::ExtentDeltaIndex,
        Feature::OptimizerRewrite,
        Feature::SequenceRans,
        Feature::SparseBlock64,
        Feature::SequenceDict,
        Feature::SequenceSharedDict,
        Feature::SequenceDeep,
        Feature::MutationLog,
    ];
}

/// The three feature sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureSetKind {
    /// Unknown bits tolerated (feature simply inactive).
    Compat,
    /// Unknown bits force read-only.
    RoCompat,
    /// Unknown bits refuse mount.
    Incompat,
}

/// Three independent feature masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureBits {
    /// compat
    pub compat: u64,
    /// ro_compat
    pub ro_compat: u64,
    /// incompat
    pub incompat: u64,
}

impl FeatureBits {
    /// Empty feature set.
    pub const fn empty() -> Self {
        Self {
            compat: 0,
            ro_compat: 0,
            incompat: 0,
        }
    }

    /// Enable a feature in its set.
    pub fn enable(&mut self, f: Feature) {
        match f.set() {
            FeatureSetKind::Compat => self.compat |= f.mask(),
            FeatureSetKind::RoCompat => self.ro_compat |= f.mask(),
            FeatureSetKind::Incompat => self.incompat |= f.mask(),
        }
    }

    /// Test whether a feature is enabled.
    pub fn has(&self, f: Feature) -> bool {
        match f.set() {
            FeatureSetKind::Compat => self.compat & f.mask() != 0,
            FeatureSetKind::RoCompat => self.ro_compat & f.mask() != 0,
            FeatureSetKind::Incompat => self.incompat & f.mask() != 0,
        }
    }

    /// The feature bits a store must set given the representation families
    /// it uses (computed at commit/fsck time).
    pub fn for_representations(
        &self,
        has_entropy_ref: bool,
        has_palette: bool,
        has_permutation: bool,
    ) -> Self {
        let mut out = *self;
        if has_entropy_ref {
            out.enable(Feature::EntropyRef);
        }
        if has_palette {
            out.enable(Feature::Palette);
        }
        if has_permutation {
            out.enable(Feature::Permutation);
        }
        out
    }
}

/// The access mode a caller is requesting (or that was refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Read-only open (no writes, no replay, no checkpoint).
    ReadOnly,
    /// Read-write open (the normal mount mode).
    ReadWrite,
}

impl AccessMode {
    /// Stable name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
        }
    }
}

/// Typed compatibility error (Phase 12E.3). Carries everything a caller
/// needs to classify and remediate a refused/permitted open WITHOUT
/// parsing prose: the format version, the unknown bits, the access mode
/// involved, and a suggested remediation string. The library error is
/// structured; the CLI may render the remediation as an upgrade/fsck
/// hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityError {
    /// The store's on-disk format major.
    pub format_major: u16,
    /// The store's on-disk format minor.
    pub format_minor: u16,
    /// Unknown `incompat` bits (non-zero ⇒ refuse every open).
    pub unknown_incompat: u64,
    /// Unknown `ro_compat` bits (non-zero ⇒ refuse writable opens).
    pub unknown_ro_compat: u64,
    /// Unknown `compat` bits (informational only; always ignorable).
    pub unknown_compat: u64,
    /// The access mode involved in the decision.
    pub access: AccessMode,
    /// Suggested remediation (e.g. "upgrade entropyfs, or mount
    /// read-only").
    pub remediation: String,
}

impl CompatibilityError {
    /// The full unknown-bit mask across all three sets.
    pub fn unknown_any(&self) -> u64 {
        self.unknown_incompat | self.unknown_ro_compat | self.unknown_compat
    }
}

impl std::fmt::Display for CompatibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "store format {}.{} carries unknown feature bits \
             (incompat 0x{:016x}, ro_compat 0x{:016x}, compat 0x{:016x}); \
             {} access refused: {}",
            self.format_major,
            self.format_minor,
            self.unknown_incompat,
            self.unknown_ro_compat,
            self.unknown_compat,
            self.access.name(),
            self.remediation
        )
    }
}

impl std::error::Error for CompatibilityError {}

/// Result of a compatibility check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// Fully compatible (every unknown bit is a defined-and-supported
    /// feature, or an ignorable compat bit).
    Ok,
    /// Unknown `ro_compat` bits with a read-only request: permitted.
    /// (A writable request for the same store would be refused.)
    ReadOnlyOnly(CompatibilityError),
    /// Refused: unknown `incompat` bits, or unknown `ro_compat` bits with
    /// a writable request.
    Refused(CompatibilityError),
}

/// The known-supported incompat mask of this build.
fn supported_incompat_mask() -> u64 {
    Feature::Chunk4K.mask()
        | Feature::Chunk16K.mask()
        | Feature::Chunk256K.mask()
        | Feature::EntropyRef.mask()
        | Feature::Palette.mask()
        | Feature::Permutation.mask()
        | Feature::SequenceRans.mask()
        | Feature::SparseBlock64.mask()
        | Feature::SequenceDict.mask()
        | Feature::SequenceSharedDict.mask()
        | Feature::SequenceDeep.mask()
        | Feature::MutationLog.mask()
}

/// The known-supported ro_compat mask of this build. The defined
/// `ENCRYPTED` bit is currently NOT implemented; it is therefore treated
/// as unknown (read-only fallback applies), never silently misread.
fn supported_ro_compat_mask() -> u64 {
    0
}

/// Check whether `on_disk` features are supported by this build for the
/// requested access mode.
///
/// Phase 12E.3 semantics (normative):
///
/// - unknown `incompat` ⇒ [`Compatibility::Refused`] (no mode works);
/// - unknown `ro_compat` ⇒ [`Compatibility::Refused`] when `want_write`,
///   [`Compatibility::ReadOnlyOnly`] when read-only is requested;
/// - unknown `compat` ⇒ ignored (reported in the error payload when a
///   decision is refused, purely for diagnostics).
///
/// The format version reported in errors defaults to this build's
/// constants; [`Self::check_with_version`] lets callers (store open)
/// report the superblock's actual version.
pub fn check(on_disk: FeatureBits, want_write: bool) -> Compatibility {
    check_with_version(
        on_disk,
        want_write,
        crate::format::version::FORMAT_MAJOR,
        crate::format::version::FORMAT_MINOR,
    )
}

/// [`Self::check`] with the store's own format version (the superblock's,
/// not the build's).
pub fn check_with_version(
    on_disk: FeatureBits,
    want_write: bool,
    format_major: u16,
    format_minor: u16,
) -> Compatibility {
    let unknown_incompat = on_disk.incompat & !supported_incompat_mask();
    if unknown_incompat != 0 {
        return Compatibility::Refused(CompatibilityError {
            format_major,
            format_minor,
            unknown_incompat,
            unknown_ro_compat: 0,
            unknown_compat: 0,
            access: AccessMode::ReadWrite,
            remediation: "this store was written by a newer EntropyFS with \
                          incompatible on-disk features; upgrade the tool (never \
                          force-open a store with unknown incompat bits)"
                .to_string(),
        });
    }
    let unknown_ro_compat = on_disk.ro_compat & !supported_ro_compat_mask();
    if unknown_ro_compat != 0 {
        let err = CompatibilityError {
            format_major,
            format_minor,
            unknown_incompat: 0,
            unknown_ro_compat,
            unknown_compat: on_disk.compat & !supported_compat_mask(),
            access: AccessMode::ReadOnly,
            remediation: "mount (or open) the store READ-ONLY: read-only access \
                          is safe because the unknown ro_compat features only \
                          affect writes; upgrade the tool to open it read-write"
                .to_string(),
        };
        return if want_write {
            let mut e = err;
            e.access = AccessMode::ReadWrite;
            Compatibility::Refused(e)
        } else {
            Compatibility::ReadOnlyOnly(err)
        };
    }
    Compatibility::Ok
}

/// Unknown compat bits are always ignorable; this mask is informational
/// (a future build may define more and still open old stores).
fn supported_compat_mask() -> u64 {
    Feature::ExtentDeltaIndex.mask() | Feature::OptimizerRewrite.mask()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_sets() {
        let mut f = FeatureBits::empty();
        assert!(!f.has(Feature::Palette));
        f.enable(Feature::Palette);
        assert!(f.has(Feature::Palette));
        assert_eq!(
            f.incompat & Feature::Palette.mask(),
            Feature::Palette.mask()
        );
        f.enable(Feature::ExtentDeltaIndex);
        assert!(f.has(Feature::ExtentDeltaIndex));
        assert_eq!(f.compat, Feature::ExtentDeltaIndex.mask());
    }

    #[test]
    fn all_features_are_in_one_set() {
        for f in Feature::ALL {
            assert!(matches!(
                f.set(),
                FeatureSetKind::Compat | FeatureSetKind::RoCompat | FeatureSetKind::Incompat
            ));
            assert!(f.mask() != 0);
        }
    }

    #[test]
    fn compat_checks() {
        // unknown incompat bit => refuse both modes
        let mut f = FeatureBits::empty();
        f.incompat = 1u64 << 63;
        assert!(matches!(check(f, true), Compatibility::Refused(_)));
        assert!(matches!(check(f, false), Compatibility::Refused(_)));
        // defined v1 incompat bits are fine
        let mut f = FeatureBits::empty();
        f.enable(Feature::Palette);
        f.enable(Feature::EntropyRef);
        assert_eq!(check(f, true), Compatibility::Ok);
        // unknown ro_compat bit => rw refused, ro permitted (12E.3)
        let mut f = FeatureBits::empty();
        f.enable(Feature::Encrypted);
        assert!(matches!(check(f, true), Compatibility::Refused(_)));
        let ro = check(f, false);
        assert!(matches!(ro, Compatibility::ReadOnlyOnly(_)));
        if let Compatibility::ReadOnlyOnly(e) = ro {
            assert_eq!(e.unknown_ro_compat, Feature::Encrypted.mask());
            assert_eq!(e.access, AccessMode::ReadOnly);
            assert_eq!(e.format_major, crate::format::version::FORMAT_MAJOR);
        }
        // unknown compat bit => always ok
        let mut f = FeatureBits::empty();
        f.compat = 1u64 << 62;
        assert_eq!(check(f, true), Compatibility::Ok);
    }

    #[test]
    fn check_with_version_reports_superblock_version() {
        let mut f = FeatureBits::empty();
        f.incompat = 1u64 << 63;
        if let Compatibility::Refused(e) = check_with_version(f, true, 1, 7) {
            assert_eq!(e.format_major, 1);
            assert_eq!(e.format_minor, 7);
        } else {
            panic!("expected refusal");
        }
    }

    #[test]
    fn compat_error_display_is_structured_not_parsed() {
        let mut f = FeatureBits::empty();
        f.incompat = 1u64 << 63;
        if let Compatibility::Refused(e) = check(f, true) {
            let s = e.to_string();
            assert!(s.contains("0x8000000000000000"));
            assert!(s.contains("refused"));
        } else {
            panic!("expected refusal");
        }
    }
}
