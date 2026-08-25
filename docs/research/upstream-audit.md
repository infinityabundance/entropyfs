# Upstream source audit: `ryg-rans-rs` and `dsfb` (2026-08-25)

Phase 0 deliverable #2. Source-guided audit of the current default branches
as published on crates.io, with the vendored sources read from the local
registry (`cargo fetch`).

## 1. `ryg-rans-rs` 0.5.1 (facade) + `ryg-rans-rs-core` 0.5.1

Crate posture: facade is `#![deny(unsafe_code)]`, `#![no_std]`; core is
`#![forbid(unsafe_code)]`, `#![no_std]`. Core re-exports: `byte` (32-bit),
`r64` surface, word surface, alias surface, `malformed` module. SIMD
(`ryg-rans-rs-simd`) is feature-gated; parallel engine
(`ryg-rans-rs-parallel`) is a separate published crate (not used in Phase 1).

### 1.1 API surface actually used by EntropyFS (Phase 1)

| Item | Notes |
|------|-------|
| `RansByteEncSymbol::new(start, freq, scale_bits)` | Validated (`ModelError`); scale_bits 1..=16; reciprocal fast path (Alverson) with freq=1 special case |
| `RansByteDecSymbol::new(start, freq)` | Validated; start/freq ≤ 2^16 |
| `BackwardByteWriter` / `ByteReader` | Zero-allocation byte I/O; `encoded()` slices |
| `rans_byte_enc_put_symbol`, `rans_byte_enc_flush` | Reciprocal encode path |
| `rans_byte_dec_init`, `rans_byte_dec_get`, `rans_byte_dec_advance_symbol` | Decode path; typed `DecodeError` |
| `ByteInterleavedEncoder` / `ByteInterleavedDecoder` | Two-state interleaving; `encode_reverse`, `flush`, `finalize` |
| `malformed::validate_byte_compressed`, `validate_freq_model`, `RenormGuard` | Malformed-input hardening |
| `EncodeError`, `DecodeError`, `ModelError` | Typed errors; no panics on malformed data |

### 1.2 Model construction

- The core provides **no generic histogram→frequency normalization** for
  byte rANS (only `rans_byte_alias_normalize_freqs`, alias-method-specific,
  with zero-frequency theft). EntropyFS therefore owns deterministic
  normalization in `src/rans/model.rs`: histogram → scale_bits-normalized
  frequencies (documented rounding, zero-frequency theft, validated
  invariants) → `RansByteEncSymbol`/`RansByteDecSymbol` arrays via the
  public constructors. This is model *construction*, not a fork of coder
  logic.
- Word rANS (`RansWordTables`, 4096-slot) and 64-bit rANS (`rans64_*`) are
  present and available for Phase 6 performance work; the scalar byte rANS
  (single + interleaved2) is the Phase-1 authority path.
- `malformed::validate_freq_model` gives a strong validity check for
  persisted models before any table construction.

### 1.3 Determinism and bitstream contract

- Division and reciprocal encode paths produce identical output (upstream
  parity + Kani proofs in the crate). Stream format is the pinned
  ryg_rans bitstream contract (reverse-order encode, backward writer,
  LSB-first renormalization, 4-byte flush). Cross-backend identity holds:
  SIMD decode consumes the same word-rANS stream as scalar decode.

### 1.4 Pinning

`ryg-rans-rs = "=0.5.1"` with `alloc` feature (convenience helpers). SIMD
feature deferred to Phase 6 after profiling. The facade is the single
import surface (`ryg_rans_rs::byte::*`, `ryg_rans_rs::malformed::*`).

## 2. `dsfb` 0.1.2

Crate posture: pure Rust, f64 arithmetic, deterministic, O(M) per step,
no ML, Apache-2.0.

### 2.1 API surface used by EntropyFS

| Item | Notes |
|------|-------|
| `DsfbParams::new(k_phi, k_omega, k_alpha, rho, sigma0)` / `default_params()` | Gains + EMA smoothing + trust softness |
| `DsfbState::new(phi, omega, alpha)` | Position, drift, slew |
| `DsfbObserver::new(params, channels)` | Per-channel EMA + trust stats |
| `observer.init(state)`, `observer.step(&measurements, dt)` | Predict → trust-weight → correct |
| `trust_weight(channel)`, `ema_residual(channel)`, `trust_stats()` | Channel evidence |
| `calculate_trust_weights(residuals, ema, rho, sigma0)` | Raw weight `1/(σ0+s)`, normalized |

### 2.2 Adaptation to storage (documented in `docs/theory/dsfb-selection.md`)

- **Channels** = candidate predictor families P0..P7 (previous version,
  adjacent chunk, exact/shared content, previous chunk in file, file-family
  base, entropy universe, rANS, raw).
- **Measurement** per channel = bounded residual-evidence scalar derived
  from exact candidate evaluation (e.g., log-scaled residual-cost ratio).
- **Drift** (slow residual growth on a stable basis) → keep basis, update
  small residuals, raise trust, narrow search.
- **Slew** (abrupt residual jump) → drop basis, lower trust, broaden search,
  establish new baseline.
- **Selection** is by exact cost (ADR-0010); DSFB only orders the search and
  sizes the budget. Zero decoding authority (ADR-0004).

## 3. Revision pins

| Dependency | Pin | Purpose |
|------------|-----|---------|
| `ryg-rans-rs` | `=0.5.1` | rANS backend (facade, alloc feature) |
| `dsfb` | `=0.1.2` | observer core |
| `blake3` | `=1.8.7` | content IDs / XOF universe |
| `crc32c` | `=0.6.8` | physical record integrity |
| `fuser` | `=0.18.0` | FUSE frontend |
| `clap` | `4.6.6` (lock) | CLI |
| `serde`/`serde_json` | `1.0.229` (lock) | JSON evidence only |
| `rustix` | `1.1.4` (lock) | safe syscalls where std is insufficient |
