//! Foreground write optimization: the bounded, latency-conscious path the
//! store uses on every write (§16).
//!
//! `encode_foreground` is a thin wrapper over the guided search in
//! `search` with `SearchMode::Foreground`, which means:
//! - exact dedup, cheap structural families, rANS and RAW are always
//!   evaluated;
//! - P0 (previous version, already in hand) is always tried;
//! - adjacent/prev-in-file/family bases are only materialized when a base
//!   channel has earned high DSFB trust or the chunk is in a slew regime;
//! - the entropy universe (background-only negative control) is never run.
//!
//! The winning representation is validated byte-exact and chosen by exact
//! cost inside the search. This module exists so the write path reads as
//! the documented §16 pipeline.

#![forbid(unsafe_code)]

use crate::optimizer::policy::OptimizeOptions;
use crate::optimizer::search::{GuidedContext, SearchMode, SearchOutcome, encode_guided};
use crate::store::{Store, StoreError};

/// Encode one chunk for the write path (bounded foreground search).
pub fn encode_foreground(
    store: &mut Store,
    ctx: &GuidedContext<'_>,
) -> Result<SearchOutcome, StoreError> {
    debug_assert_eq!(ctx.mode, SearchMode::Foreground);
    encode_guided(store, ctx, OptimizeOptions::default())
}
