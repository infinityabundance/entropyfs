//! Storage-specific DSFB observer (ADR-0004).
//!
//! DSFB has **zero decoding authority**. It may rank candidate predictors,
//! recognize persistent representation regimes, detect drift, detect slew,
//! decide how much candidate search to perform, and decide whether a
//! background re-optimization is promising. It may never alter bytes: a
//! filesystem image remains perfectly decodable if all DSFB runtime state
//! is deleted.
//!
//! The winning representation is always selected by exact deterministic
//! cost (`core::cost`); DSFB only orders the candidate search and sizes the
//! budget. If DSFB predicts poorly, the filesystem wastes CPU — never data.
//!
//! The observer core (φ/ω/α + trust weighting) is the published `dsfb`
//! crate (`docs/research/upstream-audit.md` §2); this module adapts it to
//! storage evidence.
//!
//! # Purpose
//!
//! The search must produce **identical bytes regardless of DSFB state**.
//! `encode_guided` (`src/optimizer/search.rs`) evaluates every candidate
//! representation exactly, rejects any that fails byte-exact validation
//! (§32), and selects the winner by deterministic cost (ADR-0010); RAW is
//! always among the candidates and always validates. DSFB contributes only
//! the *order* in which the budgeted base/universe channels are tried and
//! the *budget* that bounds them. Because the winner is a min over
//! validated candidates, deleting every byte of DSFB state — or never
//! creating it — leaves the persisted image bit-identical. The
//! oracle/observation data is advisory: measurements, trust, and regime
//! shape only future search order and budget, never the committed
//! representation.
//!
//! # Boundary
//!
//! DSFB may know per-chunk measurement history, trust weights, regime, and
//! the search plan derived from them. It must never know the materialization
//! path: `core` never imports `dsfb` (`docs/architecture/overview.md` §3),
//! no DSFB code appears on a read/decode path, and DSFB cannot veto a
//! validated candidate — it can only decide whether that candidate is
//! *searched for* at all.
//!
//! # Model
//!
//! Per (file, chunk index, content version) the observer keeps a small
//! state machine (`observer.rs`): a per-channel EMA of evidence error
//! (`|1 − y|`), normalized trust weights, the published crate's φ/ω/α
//! drift–slew state, and a robust regime tracker over the
//! winner-measurement series (`drift.rs`). The regime
//! (Unknown → Stable/Drift/Slew) drives the search strategy and budget
//! (`selection.rs`): slew broadens and re-baselines, drift balances,
//! stable narrows.
//!
//! # Persistent authority
//!
//! **None.** DSFB state is in-memory only (ADR-0004): unmounting and
//! remounting resets it, and it never enters the authoritative object
//! graph. Bounded eviction (`store::DSFB_MAX_CHUNKS` → `evict_one`) drops
//! only performance hints.
//!
//! # Correctness invariants
//!
//! - The winning representation is always the minimum over byte-validated
//!   candidates by exact cost (ADR-0010); DSFB changes which candidates
//!   are searched, never the decision rule.
//! - Every measurement entering the observer is bounded in `[0, 1]`
//!   (clamped at the boundary); one bad measurement cannot corrupt the
//!   EMAs.
//! - Regime comes from the robust `MeasurementTracker`, not from the raw
//!   φ/ω/α integration (which accumulates permanent velocity).
//! - A filesystem image decodes identically with all DSFB state deleted.
//!
//! # Concurrency (Phase-11F)
//!
//! The observer is sharded (`observer.rs`, 11F): per-key state lives in
//! 16 independently locked shards chosen by a stable FNV-1a hash of the
//! key, and the aggregate statistics are lock-free atomics. Every
//! accessor locks exactly one shard; unrelated keys never block each
//! other, and the store holds the observer directly with no outer mutex.
//! The 11D brief predicted the old single store-level mutex would become
//! visible under the 11E fair pool; the 11F oracle (CHANGELOG v0.7.7)
//! falsified that at the sealed scale (~1 µs per call, 0.1–0.5% of
//! `prepare`), so the shard was adopted as the permanently correct shape
//! rather than as a response to a measured emergency.
//!
//! # Resource bounds
//!
//! One `ChunkObserver` per distinct (ino, index, content-id) key, capped
//! by `DSFB_MAX_CHUNKS` (100 000) via the exact atomic count + targeted
//! per-shard eviction in `Store::dsfb_observe`. Per-entry state is a
//! handful of fixed-size 9-channel arrays. A writer can grow the map only
//! by touching many distinct chunks; the cap bounds it, and eviction
//! never affects correctness.
//!
//! # Performance
//!
//! Per write: one map lookup/insert, O(channels) EMA/trust updates, and a
//! sort of 9 elements when a plan is built. This runs on the write path,
//! so the design favors fixed arrays over allocation. The alternative —
//! no guidance, exhaustive search — is the `no-dsfb` ablation mode used to
//! measure DSFB's actual value (H3, `docs/theory/dsfb-selection.md` §5).
//!
//! # Failure modes
//!
//! The observer itself is infallible (no `Result`). External failure
//! classes: a measurement outside `[0, 1]` (or NaN) degrades trust and
//! regime quality but cannot change persisted bytes; a DSFB regression
//! that misorders search costs CPU only. What must never happen: DSFB
//! state influencing winner selection, or DSFB appearing on a
//! materialization/decode path.
//!
//! # History / evidence
//!
//! ADR-0004 (zero-authority observer), ADR-0010 (exact cost wins), Phase 4
//! wiring (ablation campaign `campaign-1787658658-67d977a/`), H3 ablation
//! methodology (§5 of `docs/theory/dsfb-selection.md`), the upstream
//! source audit (`docs/research/upstream-audit.md` §2), the 11D/11E
//! worker-pool gates that surfaced the observer mutex's predicted cost
//! (CHANGELOG v0.7.4), and the 11F shard oracle that falsified the
//! prediction at the sealed scale while verifying zero regression
//! (CHANGELOG v0.7.7).

#![forbid(unsafe_code)]

pub mod drift;
pub mod features;
pub mod observer;
pub mod selection;
pub mod semantics;
pub mod slew;
pub mod trust;
