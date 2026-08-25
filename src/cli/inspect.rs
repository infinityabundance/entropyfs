//! `entropyfs inspect <store> <path> [--offset N]`: per-extent detail for
//! a file (§49). Works on an unmounted store.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::store::{Store, StoreConfig};

/// Options for inspect.
#[derive(Debug, Clone, clap::Args)]
pub struct InspectArgs {
    /// Store directory.
    #[arg(value_name = "STORE")]
    pub store: PathBuf,
    /// File path inside the store (absolute).
    #[arg(value_name = "PATH")]
    pub path: String,
    /// Logical offset to focus on.
    #[arg(long, default_value_t = 0)]
    pub offset: u64,
}

/// Run inspect.
pub fn run(args: &InspectArgs) -> Result<(), String> {
    let config = StoreConfig::default();
    let store = Store::open(&args.store, &config).map_err(|e| e.to_string())?;
    let ino = store
        .resolve_path(args.path.as_bytes())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no such path: {}", args.path))?;
    let inode = store
        .get_inode(ino)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("inode {ino} missing"))?;
    match inode.data {
        crate::store::inode::InodeData::File { extent_root } => {
            let entries = crate::store::extent_tree::scan_all(
                extent_root,
                crate::store::BTREE_ORDER,
                store.limits().max_fanout,
                &store,
            )
            .map_err(|e| e.to_string())?;
            let mut total_logical = 0u64;
            let mut total_persisted = 0u64;
            println!("file: {}", args.path);
            println!("logical: {} bytes ({} extents)", inode.size, entries.len());
            for (start, bytes) in entries {
                let desc = crate::format::descriptor::decode(
                    &bytes,
                    store.limits().max_descriptor_bytes,
                    store.limits().max_inline_bytes,
                    store.limits().max_palette,
                    store.limits().max_period,
                    store.limits().max_chunk_size,
                )
                .map_err(|e| format!("descriptor decode: {e:?}"))?;
                let focused = start <= args.offset && args.offset < start + desc.len();
                if focused {
                    println!();
                    println!(
                        "logical extent: 0x{start:08x}..0x{:08x}",
                        start + desc.len()
                    );
                    println!("content id: {}", desc.content_id_hint());
                    println!("representation: {:?}", family_name(&desc));
                    println!(
                        "materialized bytes: {}  descriptor bytes: {}",
                        desc.len(),
                        bytes.len()
                    );
                    crate::cli::explain::print_alternatives(&store, &desc);
                }
                total_logical += desc.len();
                total_persisted += bytes.len() as u64;
                let _ = focused;
            }
            println!();
            println!(
                "totals: logical {}  descriptor bytes {}",
                total_logical, total_persisted
            );
            Ok(())
        }
        _ => Err("not a regular file".into()),
    }
}

/// Short family name for a representation.
pub fn family_name(desc: &crate::core::representation::Representation) -> &'static str {
    match desc {
        crate::core::representation::Representation::Zero { .. } => "ZERO",
        crate::core::representation::Representation::Fill { .. } => "FILL",
        crate::core::representation::Representation::Inline { .. } => "INLINE",
        crate::core::representation::Representation::Raw { .. } => "RAW",
        crate::core::representation::Representation::Rans { .. } => "RANS",
        crate::core::representation::Representation::ExactRef { .. } => "EXACT_REF",
        crate::core::representation::Representation::BaseResidual { .. } => "BASE_RESIDUAL",
        crate::core::representation::Representation::Sparse { .. } => "SPARSE",
        crate::core::representation::Representation::Palette { .. } => "PALETTE",
        crate::core::representation::Representation::Periodic { .. } => "PERIODIC",
        crate::core::representation::Representation::EntropyRef { .. } => "ENTROPY_REF",
        crate::core::representation::Representation::Permutation { .. } => "PERMUTATION",
        crate::core::representation::Representation::SequenceRans { .. } => "SEQUENCE_RANS",
    }
}

/// Content id of the materialized bytes (used by the explain path).
pub trait ContentIdHint {
    /// Short hex of the chunk content id when determinable, else a marker.
    fn content_id_hint(&self) -> String;
}

impl ContentIdHint for crate::core::representation::Representation {
    fn content_id_hint(&self) -> String {
        match self {
            crate::core::representation::Representation::Raw { obj, .. } => short_hex(obj),
            crate::core::representation::Representation::Rans { enc_obj, .. } => short_hex(enc_obj),
            _ => "see --explain (materialize to compute)".into(),
        }
    }
}

/// Short hex of a chunk id.
pub fn short_hex(id: &crate::core::extent::ChunkId) -> String {
    let hex = crate::cli::mkfs::hex_encode(id.as_bytes());
    hex.chars().take(16).collect()
}
