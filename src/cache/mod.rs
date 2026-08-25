//! Bounded performance-only caches (ADR-0014): materialized chunks,
//! metadata, models. Caches are never authoritative; dropping every cache
//! affects only performance.

#![forbid(unsafe_code)]

// (module populated by the cache implementation step)
