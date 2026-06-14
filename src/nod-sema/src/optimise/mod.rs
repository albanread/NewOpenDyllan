//! `nod-sema::optimise` — Sprint 15 sealing analysis + dispatch
//! resolution. Sits inside `nod-sema` per spec 15 §10 OQ1 recommendation;
//! Sprint 18's CSE/inline/DCE will promote this surface into the
//! standalone `nod-opt` crate.
//!
//! Two passes, run in this order after the DFM is built and BEFORE
//! codegen (and BEFORE Sprint 11b's precise-roots post-pass that walks
//! call-shaped computations):
//!
//!   1. `narrow_function` (sub-module `narrowing`) — forward dataflow
//!      that strengthens `TypeEstimate`s. New estimates come from
//!      method-specialiser narrowing, `make` narrowing, slot-type
//!      narrowing, `instance?`-guarded then-branch narrowing, and
//!      direct-call return types.
//!
//!   2. `resolve_dispatches` (sub-module `dispatch`) — for each
//!      `Computation::Dispatch`, consult the sealing facts table and
//!      the narrowed estimates; rewrite to `DirectCall` (single
//!      applicable method, no chain possible) or `SealedDirectCall`
//!      (single most-specific method + a non-empty fallback chain for
//!      `next-method`).
//!
//! See `specs/15-sealing-and-dispatch-resolution.md` for the algorithm
//! and the rationale.

mod dispatch;
mod facts;
mod narrowing;

pub use dispatch::{DispatchResolution, resolve_dispatches};
pub use facts::{SealingFacts, dump_sealed};
pub use narrowing::{NarrowedEstimates, narrow_function};

pub(crate) use facts::{collect_sealing_facts, install_sealing_facts};
