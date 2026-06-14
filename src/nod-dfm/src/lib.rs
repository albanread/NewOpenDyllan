//! `nod-dfm` — Dylan Flow Machine SSA IR.
//!
//! Sprint 06: kernel-subset IR shape — `Function` owns `Block`s which own
//! `Computation`s. SSA: each `TempId` has exactly one defining computation
//! (or is a function parameter). Every block ends in exactly one
//! `Terminator` — there is no fall-through.
//!
//! The shape is deliberately small. Sprint 13 adds `Dispatch` for generic
//! call resolution; Sprints 10-13 will grow `Computation` with slot
//! access, allocation, and exit machinery. This module is the layer the
//! IDE inspector (MANIFESTO "Live inspection") will introspect — keep
//! it inspectable by keeping fields public and printable.

mod format;
mod ir;
mod liveness;
mod parse;
mod verify;

pub use nod_reader::{FileId, Span};
pub use format::{format_dfm, format_dfm_module, format_for_cache_key};
pub use parse::parse_dfm_module;
pub use ir::{
    Block, BlockId, ClassCheck, Computation, ConstValue, Function, FunctionId, PrimOp, SlotTypeKind,
    SafepointLocation, SafepointRootLocation, Temporary, TempId, Terminator, TypeEstimate,
};
pub use liveness::{
    ArgRootCoverageGap, SafepointError, diagnose_arg_root_coverage, populate_safepoint_roots,
    verify_safepoint_roots,
};
pub use verify::{VerifyError, verify};
