# ADR-0004: DSFB is a zero-authority observer

**Status:** accepted · **Date:** 2026-08-25

## Context

The DSFB framework (drift–slew fusion bootstrap) is a deterministic,
trust-adaptive observer: position φ, drift ω, slew α, EMA-of-residual trust
weights, O(M) per step. EntropyFS wants it to decide *which expensive
candidate representations are worth evaluating* and *whether background
re-optimization is promising*.

Source-guided audit findings (crate `dsfb` 0.1.2): public API is
`DsfbParams { k_phi, k_omega, k_alpha, rho, sigma0 }`,
`DsfbState { phi, omega, alpha }`,
`DsfbObserver::new(params, channels)`, `init(state)`,
`step(&measurements, dt) -> DsfbState`, `state()`, `trust_stats()`,
`trust_weight(channel)`, `ema_residual(channel)`, and
`calculate_trust_weights(residuals, ema_residuals, rho, sigma0)`.
Pure f64 arithmetic; deterministic; no ML; no decoding authority.

## Decision

EntropyFS adapts the published `dsfb` crate into a storage observer in
`src/dsfb/`, with an absolute separation of authority:

- **DSFB may**: rank candidate predictor families (P0 previous version,
  P1 adjacent chunk, P2 exact/shared content, P3 previous chunk in file,
  P4 file-family base, P5 entropy universe, P6 rANS, P7 raw), recognize
  persistent representation regimes, classify drift vs slew, set candidate
  search breadth, and decide whether background re-optimization is promising.
- **DSFB may never**: alter bytes, choose a representation that failed exact
  validation, or appear on any materialization path.

The winning representation is always selected by **exact deterministic cost**
(ADR-0010). If DSFB predicts poorly, the filesystem wastes CPU — never
corrupts data. A filesystem image remains perfectly decodable if all DSFB
runtime state is deleted; DSFB state is therefore not persisted in the
authoritative object graph (only in performance-only side files).

Drift semantics: residual structure changes slowly and a representation
family remains effective → keep the basis, update small residuals, increase
trust. Slew semantics: residual structure jumps → stop forcing the old basis,
reduce trust, broaden candidate search, establish a new baseline.

## Consequences

- DSFB credit is only claimed for search-cost reduction, not for savings
  produced by deduplication or rANS (see `docs/performance/methodology.md`,
  ablation science).
- `src/dsfb/` depends on `core` types (evidence features) but `core` never
  depends on `dsfb`.
- Phase 4 wiring (implemented): the store holds one `StorageObserver`
  (bounded, evicted at 100k chunks); the guided search
  (`src/optimizer/search.rs`) feeds per-channel measurements and consumes
  the trust-ordered, budget-bounded plan. The observer is in-memory only —
  unmounting and remounting resets it, which never affects decodability.
  See `docs/theory/dsfb-selection.md` §7.
