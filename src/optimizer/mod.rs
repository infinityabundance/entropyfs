//! Representation search and migration (Phase 4).
//!
//! Foreground optimization is bounded and latency-conscious
//! (`foreground`, `search` with `SearchMode::Foreground`); background
//! optimization performs deeper, DSFB-guided search (`background`) with
//! reference-chain flattening (`rebase`). The optimizer never defines
//! correctness — it proposes exact candidates that are independently
//! validated before commit (§32). `policy` carries the ablation gates used
//! by the benchmark (spec §43).

#![forbid(unsafe_code)]

pub mod background;
pub mod foreground;
pub mod policy;
pub mod rebase;
pub mod search;
