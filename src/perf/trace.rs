//! Phase 12E.7: structured tracing spans.
//!
//! # PURPOSE
//!
//! A single crate-visible macro that emits a `tracing` span (with
//! attributes) on the externally important operations — engine
//! put/get/range/sync/compact, store open/create, the durability
//! barrier, epoch checkpoints, GC, the optimizer — and compiles to
//! nothing when the `tracing` feature is off (the base embeddable
//! build).
//!
//! # ATTRIBUTE DISCIPLINE
//!
//! - Never log user payload bytes (the prompt's hard rule).
//! - Content ids are logged TRUNCATED (first 8 hex chars) unless a
//!   caller has a stronger need and documents it.
//! - Attributes are cheap primitives (u64, &'static str, small strings).
//! - No subscriber is bundled: embedders attach their own
//!   `tracing_subscriber`/collector; without one the spans are no-ops
//!   (tracing's macro cost when disabled is a handful of nanoseconds on
//!   per-REQUEST operations — never on per-chunk hot loops).
//!
//! # CONCURRENCY / PERFORMANCE
//!
//! Spans are per-call-site thread-local (tracing's registry); the guard
//! lives until the end of the enclosing scope, so a span covers the
//! operation's duration. The macro is a no-op expansion with the feature
//! off: zero symbols, zero runtime cost.

#![forbid(unsafe_code)]

/// Emit a span for the rest of the enclosing scope, with attributes.
///
/// ```rust,ignore
/// crate::perf::trace::span!("engine.put_blob", op = "put_blob", len = n, id = hex);
/// ```
#[cfg(feature = "tracing")]
macro_rules! span {
    ($name:literal $(, $k:ident = $v:expr)* $(,)?) => {{
        let __efs_span = tracing::info_span!($name $(, $k = $v)*);
        let _efs_span_guard = __efs_span.enter();
    }};
}

/// Feature-off expansion: nothing.
#[cfg(not(feature = "tracing"))]
macro_rules! span {
    ($name:literal $(, $k:ident = $v:expr)* $(,)?) => {};
}

pub(crate) use span;
