//! Representation search and migration. Foreground optimization is bounded
//! and latency-conscious; background optimization performs deeper,
//! DSFB-guided search. The optimizer never defines correctness — it
//! proposes exact candidates that are independently validated before commit.

#![forbid(unsafe_code)]

// (module populated by the optimizer implementation step)
