//! Feature bit sets (compat / ro_compat / incompat) and mount
//! compatibility checks (`docs/format/compatibility.md`).

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
            | Feature::SequenceDict => FeatureSetKind::Incompat,
            Feature::Encrypted => FeatureSetKind::RoCompat,
            Feature::ExtentDeltaIndex | Feature::OptimizerRewrite => FeatureSetKind::Compat,
        }
    }

    /// Bit mask for this feature.
    pub const fn mask(self) -> u64 {
        1u64 << (self as u8 - 1)
    }
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

/// Result of a compatibility check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// Fully compatible.
    Ok,
    /// Compatible read-only only (unknown ro_compat bits).
    ReadOnlyOnly,
    /// Refused (unknown incompat bits, or an unsupported feature).
    Refused(String),
}

/// Check whether `on_disk` features are supported by this tool.
///
/// v1 supports the defined representation feature bits; encryption
/// (ro_compat `Encrypted`) is not yet implemented, so any ro_compat bit is
/// refused rather than silently misread.
pub fn check(on_disk: FeatureBits, _want_write: bool) -> Compatibility {
    let supported_incompat = Feature::Chunk4K.mask()
        | Feature::Chunk16K.mask()
        | Feature::Chunk256K.mask()
        | Feature::EntropyRef.mask()
        | Feature::Palette.mask()
        | Feature::Permutation.mask()
        | Feature::SequenceRans.mask()
        | Feature::SparseBlock64.mask()
        | Feature::SequenceDict.mask();
    if on_disk.incompat & !supported_incompat != 0 {
        return Compatibility::Refused(format!(
            "unsupported incompat feature bits: 0x{:016x}",
            on_disk.incompat & !supported_incompat
        ));
    }
    if on_disk.ro_compat != 0 {
        return Compatibility::Refused(
            "store uses features not yet supported by this tool (encryption)".into(),
        );
    }
    Compatibility::Ok
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
    fn compat_checks() {
        // unknown incompat bit => refuse
        let mut f = FeatureBits::empty();
        f.incompat = 1u64 << 63;
        assert!(matches!(check(f, true), Compatibility::Refused(_)));
        // defined v1 incompat bits are fine
        let mut f = FeatureBits::empty();
        f.enable(Feature::Palette);
        f.enable(Feature::EntropyRef);
        assert_eq!(check(f, true), Compatibility::Ok);
        // encryption (ro_compat) is refused until implemented
        let mut f = FeatureBits::empty();
        f.enable(Feature::Encrypted);
        assert!(matches!(check(f, false), Compatibility::Refused(_)));
    }
}
