//! `nod-llvm` — DFM -> LLVM IR codegen + MCJIT execution.
//!
//! Sprint 07: kernel-subset codegen (i64 / f32 / f64 / bool arithmetic,
//! branches, direct calls, returns) plus a thin JIT wrapper that hands
//! back raw function pointers. No `gc.statepoint`, no opt passes — those
//! land in Sprints 11 and 11/12 respectively.

// Sprint 39a — AOT support: entry-stub injection + object-file emission.
// Imported here so `nod-driver`'s `build` subcommand can call
// `nod_llvm::aot::emit_aot_object` directly.
pub mod aot;
pub mod cache;
pub mod codegen;
pub mod jit;
pub mod jit_mm;
pub mod symbols;

pub use cache::{
    CacheKey, JitCacheStats, JitReplayResult, NOD_RUNTIME_ABI_VERSION, OPT_LEVEL, ReplayFn,
    cache_entry_count, cache_key, cache_key_for_dfm, cache_max_bytes, cache_size_on_disk,
    clear_cache_dir, default_cache_dir, disk_cache_stats, evict_to, in_process_clear,
    in_process_contains, in_process_get, in_process_insert, read_cache_entry,
    read_cache_entry_with_manifest, read_stats, record_disk_hit, record_disk_miss, record_hit,
    record_miss, reset_stats, target_triple, write_cache_entry, write_cache_entry_with_manifest,
};
pub use codegen::{
    CodeInstallSurface, CodegenError, CodegenOutput, FunctionMap, InstalledTextRegion,
    InstalledTextRegionKind, SafepointInstallRecord, SafepointKind, SafepointPlan,
    codegen_module, codegen_module_for_surface, codegen_module_with_key,
    codegen_module_with_key_for_surface, plan_safepoints,
};
pub use jit::{Jit, JitError, bitcode_to_ir_text};

/// Sprint 38 — re-export the LLVM `Context` so downstream tests can
/// drive [`Jit::add_module_from_bitcode`] without depending on
/// `inkwell` directly. The replay-load path needs a context to parse
/// bitcode into; the cold-compile path threads one through
/// `eval_wrapped_source`.
pub use inkwell::context::Context as LlvmContext;
/// Sprint 39a — re-export `OptimizationLevel` so `nod-driver` can pass
/// it through to `aot::emit_aot_object` without an explicit `inkwell`
/// dependency in `nod-driver`'s `Cargo.toml`.
pub use inkwell::OptimizationLevel;
pub use symbols::{
    MANIFEST_VERSION, ModuleManifest, RelocEntry, RelocKind, cache_slot_symbol, class_md_symbol,
    generic_symbol, imm_false_symbol, imm_false_wrapper_symbol, imm_nil_symbol, imm_true_symbol,
    key_prefix, strlit_symbol, stub_symbol, symlit_symbol,
};

// Sprint 39c — registration payload types so `nod-sema` can build
// the merged-stdlib registrations and hand them to the AOT pipeline.
// Sprint 40a — extended with user-class registrations.
pub use aot::{
    AotBlockHandlerRegistration, AotBlockRegistration, AotFunctionRegistration,
    AotMethodRegistration, AotRegistrations, AotShape, AotSlotRegistration,
    AotUserClassRegistration, AotVariableRegistration,
};
