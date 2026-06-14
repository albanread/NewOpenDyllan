//! Sprint 16 benchmarking helpers — used by the Richards-shape headline
//! demo to measure sealed-direct vs open-dispatch performance.
//!
//! The crate-public surface:
//!
//!   - `bench_fixture(path, warmup_iters)` — load a `.dylan` file, JIT
//!     it, warm the inline caches, and run the entry `main()` once
//!     under a `std::time::Instant`. Returns a `BenchResult` carrying
//!     the elapsed time, the returned i64, and a snapshot of the
//!     dispatch profile (cache + resolved-direct counts).
//!
//!   - `dispatch_profile()` — roll up Sprint 13's per-call-site
//!     `CacheSlot::hits` / `misses` plus Sprint 15's resolved-dispatch
//!     index into a single aggregate snapshot. Useful both inside
//!     `bench_fixture` and as a hook for a future `nod-driver --profile`
//!     flag (Sprint 16 deliverable §1).
//!
//! `bench_fixture` is intentionally low-frill — single warmup pass, one
//! timed run, `std::time::Instant`. Sprint 18+ can promote to
//! `criterion`-grade measurement with statistical rigour; the Sprint
//! 16 deliverable is the speedup ratio, not the methodology.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use inkwell::context::Context;
use nod_dfm::TypeEstimate;
use nod_llvm::{Jit, codegen_module};
use nod_runtime::{Word, for_each_generic, resolved_dispatch_snapshot};

use crate::{EvalError, lower_module_full, register_methods};

/// Rolled-up dispatch profile, taken at one point in time. The fields
/// are summed across every registered generic.
///
/// `sealed_direct_sites` is the count of `Computation::Dispatch` nodes
/// the Sprint 15 resolver has rewritten to a direct call (read from
/// `resolved_dispatch_snapshot`). These sites do NOT allocate a
/// `CacheSlot`; they're a literal `call @method$<class>` in the IR.
///
/// `cached_sites` is the count of `CacheSlot`s the JIT-emitted inline
/// caches have registered with their parent generic (one per active
/// call site). `cache_hits` / `cache_misses` sum the per-site counters.
#[derive(Clone, Debug, Default)]
pub struct DispatchProfile {
    /// Number of Sprint 15 resolved-direct sites recorded in the
    /// back-reference index. Each entry is one Dispatch → Direct rewrite.
    pub sealed_direct_sites: usize,
    /// Number of inline-cache slots across every registered generic.
    /// One slot per JIT-emitted `Dispatch` call site (Sprint 13).
    pub cached_sites: usize,
    /// Sum of `CacheSlot::hits` across every registered cache slot.
    pub cache_hits: u64,
    /// Sum of `CacheSlot::misses` across every registered cache slot.
    pub cache_misses: u64,
}

/// Snapshot the current dispatch profile. Reads Sprint 15's resolved-
/// dispatch index and Sprint 13's per-generic `cache_slots` lists.
/// Both data structures are guarded by `RwLock`s; the read locks are
/// released before this function returns so the caller doesn't hold
/// them across user code.
pub fn dispatch_profile() -> DispatchProfile {
    let sealed_direct_sites = resolved_dispatch_snapshot().len();
    let mut cached_sites = 0usize;
    let mut cache_hits = 0u64;
    let mut cache_misses = 0u64;
    for_each_generic(|g| {
        let slots = g
            .cache_slots
            .read()
            .expect("cache_slots rwlock poisoned");
        for slot_ptr in slots.iter() {
            // SAFETY: cache slots are pinned in JIT statics for the
            // process lifetime; the pointers in `cache_slots` are the
            // addresses baked into the IR.
            let slot = unsafe { &**slot_ptr };
            cached_sites += 1;
            cache_hits += slot.hits.load(Ordering::Relaxed);
            cache_misses += slot.misses.load(Ordering::Relaxed);
        }
    });
    DispatchProfile {
        sealed_direct_sites,
        cached_sites,
        cache_hits,
        cache_misses,
    }
}

/// One bench measurement — a single `main()` call timed end-to-end
/// after `warmup_iters` warmup runs. The dispatch profile is the
/// snapshot AFTER the timed run (it includes both warmup and timed
/// hit/miss counts; subtract a pre-run snapshot if you need the
/// delta).
#[derive(Clone, Debug)]
pub struct BenchResult {
    pub fixture: PathBuf,
    pub iterations: u64,
    pub elapsed_ns: u64,
    pub returned_value: i64,
    pub dispatch_profile: DispatchProfile,
}

/// Lower + codegen + JIT-link the fixture at `path`, run `main()` once
/// for warmup (if `warmup_iters > 0`), then once more under a
/// `std::time::Instant` for measurement. Returns a `BenchResult` with
/// the elapsed time and the entry's return value (which must be a
/// fixnum-tagged `<integer>`).
///
/// The runtime's process-global dispatch table is **not** reset between
/// runs — callers that need isolation must `_reset_for_tests` or use
/// per-fixture class/generic name prefixes the way the Sprint 15 tests
/// do.
pub fn bench_fixture(path: &Path, warmup_iters: u32) -> Result<BenchResult, EvalError> {
    let src = std::fs::read_to_string(path).map_err(EvalError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add(path.to_path_buf(), src.clone())
        .map_err(EvalError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let module = nod_reader::parse_module(&src, &toks, pre.as_ref())
        .map_err(EvalError::Parse)?;
    let lm = lower_module_full(&module).map_err(EvalError::Lower)?;

    let target = lm
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| EvalError::NoEntry("main".to_string()))?;
    if !matches!(target.return_type, TypeEstimate::Integer) {
        return Err(EvalError::ReturnTypeMismatch {
            entry: "main".to_string(),
            expected: "<integer>",
            actual: target.return_type.name(),
        });
    }

    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bench-module");
    let ctx = Context::create();
    let out = codegen_module(&ctx, &lm.functions, module_name).map_err(EvalError::Codegen)?;
    let mut jit = Jit::new(&ctx).map_err(EvalError::Jit)?;
    jit.add_module(out).map_err(EvalError::Jit)?;
    register_methods(&jit, &lm.methods)?;

    // SAFETY: `main` has signature `() -> i64` (Sprint 09 ABI for
    // `<integer>` returns). The JIT engine outlives the call (held in
    // `jit` for the duration of this function).
    let ptr = unsafe { jit.get_function_ptr("main") }
        .ok_or_else(|| EvalError::NoEntry("main".to_string()))?;
    let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };

    // Warmup. The MCJIT engine compiles lazily on first call; the
    // warmup passes flush that latency out of the timed run and let
    // Sprint 13's inline caches transition from cold → warm.
    for _ in 0..warmup_iters {
        let _ = f();
    }

    // Timed run.
    let start = Instant::now();
    let raw = f();
    let elapsed = start.elapsed();

    let w = Word::from_raw(raw);
    let returned_value = w.as_fixnum().ok_or_else(|| EvalError::ReturnTypeMismatch {
        entry: "main".to_string(),
        expected: "<integer> (fixnum)",
        actual: "<pointer-tagged>",
    })?;

    Ok(BenchResult {
        fixture: path.to_path_buf(),
        iterations: 1,
        elapsed_ns: elapsed.as_nanos().min(u64::MAX as u128) as u64,
        returned_value,
        dispatch_profile: dispatch_profile(),
    })
}
