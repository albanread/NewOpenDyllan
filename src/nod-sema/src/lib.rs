//! `nod-sema` — AST → DFM lowering for the Sprint 06 kernel subset,
//! plus Sprint 07 JIT entry points (`eval_expr_to_string`,
//! `dump_llvm_for_file`, `run_function_to_i64`).
//!
//! Out of scope (emits `LoweringError::Unsupported`):
//!   - Generics, methods, classes, macros.
//!   - Multi-binder `let (a, b) = …`, multi-value return.
//!   - `block` / `for` / `while` / `until`, closures, `local method`.
//!   - Keyword-arg synthetic calls (Sprint 04 carry-over).
//!   - `select` (Sprint 03 carry-over) and multi-cond `case` arms.
//!
//! Kernel subset (lowered):
//!   - `define constant` / `define function` (non-generic).
//!   - `Statement::Expr` and single-binder `Statement::Let`.
//!   - Literal exprs, idents (local + top-level direct-call), `Paren`,
//!     `BinOp` / `UnOp` (integer + float monomorphic), `If`, `Begin`,
//!     `Call` against an ident callee.

mod bench;
pub mod c3;
mod lower;
pub mod optimise;
pub mod sidecar;
pub mod stdlib;

pub use bench::{BenchResult, DispatchProfile, bench_fixture, dispatch_profile};
pub use optimise::{
    DispatchResolution, SealingFacts, dump_sealed, narrow_function, resolve_dispatches,
};

use std::path::Path;

use inkwell::context::Context;
use nod_dfm::TypeEstimate;
use nod_llvm::{Jit, codegen_module};

pub use lower::{
    BindingSource, BlockHandlerRegistration, BlockRegistration, CFunctionBinding, ClosureInfo,
    ClosureRegistry, LoweredModule, LoweringError, LoweringWarning, MethodRegistration,
    SealingViolation, UserClassRegistration, VariableRegistration, dump_classes, lower_function,
    lower_module, lower_module_full,
};

/// Sprint 17: parse + macro-expand + lower in one shot. Existing
/// `lower_module_full(&Module)` remains for AST-direct testing; this
/// is the entry point all driver-facing helpers (`dump_dfm_for_file`,
/// `run_function_to_i64`, `eval_expr_to_string`) now route through so
/// `unless`-style macros expand before lowering.
///
/// Sprint 20b: ensures `stdlib.dylan` is loaded before lowering the
/// caller's module, and merges the stdlib's macros into the
/// per-call macro table so user code can use `for-each` etc.
pub fn expand_and_lower_module(
    module: &nod_reader::Module,
    source: &nod_reader::SourceMap,
) -> Result<LoweredModule, ExpandLowerError> {
    stdlib::ensure_loaded();
    let mut m = module.clone();
    expand_with_stdlib_macros(&mut m, source).map_err(ExpandLowerError::Macro)?;
    lower_module_full(&m).map_err(ExpandLowerError::Lower)
}

/// Sprint 25: parse a user-code module with the stdlib's macro
/// names pre-seeded into the parser's known-macro set. This is what
/// lights up body-shaped macro calls (`unless (c) b end`,
/// `for-each (x in c) b end`) for user code — those names aren't
/// in the user's own `define macro` items, but the stdlib loader
/// registered them at process start, and the parser needs to know
/// about them at parse time to recognise the body-shaped surface.
///
/// Idempotent: assumes the caller has already called
/// `stdlib::ensure_loaded()` (or will route via one of the public
/// helpers in this module that does).
fn parse_user_module(
    src: &str,
    tokens: &[nod_reader::Token],
    preamble: Option<&nod_reader::Preamble>,
) -> Result<nod_reader::Module, Vec<nod_reader::Diagnostic>> {
    let seed = stdlib_macro_name_set();
    nod_reader::parse_module_with_macros(src, tokens, preamble, &seed)
}

fn stdlib_macro_name_set() -> std::collections::HashSet<String> {
    stdlib_macro_names()
}

/// Sprint 51c-1 — public accessor for the stdlib's macro name set,
/// for callers that need to drive `nod_reader::parse_module_with_macros`
/// directly (e.g. the driver's `dump-ast` subcommand running outside
/// the full sema pipeline). Without seeding these names, the parser
/// doesn't recognise stdlib body-shaped macros (`cond`, `case`,
/// `unless`, `when`, `for-each`, …) and errors out on tokens like
/// `KwOtherwise` that only appear inside those forms.
///
/// Triggers `stdlib::ensure_loaded()` on first call so the macro table
/// is materialised. Idempotent on subsequent calls (LazyLock-backed).
pub fn stdlib_macro_names() -> std::collections::HashSet<String> {
    stdlib::ensure_loaded();
    let table = stdlib::stdlib_macros();
    table.defs.keys().cloned().collect()
}

/// Sprint 20b: macro expansion that merges `stdlib_macros()` on top
/// of the user's per-call table. Same semantics as
/// `nod_macro::collect_and_expand` but with the stdlib's macros
/// pre-populated so user code can write `for-each (x in c) … end`.
fn expand_with_stdlib_macros(
    module: &mut nod_reader::Module,
    source: &nod_reader::SourceMap,
) -> Result<nod_macro::MacroTable, Vec<nod_macro::MacroError>> {
    let mut table = stdlib::stdlib_macros().clone();
    nod_macro::collect_macros(module, source, &mut table)?;
    nod_macro::expand_module(module, &table, source)?;
    Ok(table)
}

#[derive(Debug)]
pub enum ExpandLowerError {
    Macro(Vec<nod_macro::MacroError>),
    Lower(Vec<LoweringError>),
}

impl std::fmt::Display for ExpandLowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Macro(es) => {
                write!(f, "macro expansion: {} error(s):", es.len())?;
                for e in es {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            Self::Lower(es) => {
                write!(f, "lower: {} error(s):", es.len())?;
                for e in es {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ExpandLowerError {}

/// Sprint 17 driver helper: read a Dylan file, parse it, macro-expand,
/// return the formatted post-expansion AST. Wired into the future
/// `dump-expanded` CLI subcommand.
pub fn dump_expanded_for_file(path: &Path) -> Result<String, DumpError> {
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(path).map_err(DumpError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add(path.to_path_buf(), src.clone())
        .map_err(DumpError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module = parse_user_module(&src, &toks, pre.as_ref()).map_err(DumpError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(DumpError::Macro)?;
    Ok(nod_reader::format_ast_module(&module))
}

/// Sprint 54c — `--sema-with-dylan` provider. The driver installs a function
/// that, given the source text, returns the Dylan-produced sema model dump
/// (by calling the in-process `dylan-sema-emit` shim entry). When installed,
/// opted-in lowering paths (currently `dump-dfm`) reconstruct the `SemaModel`
/// from the Dylan dump via [`lower::analyse_module_from_dump`] and feed it to
/// [`lower::lower_module_full_with_model`] — making the Dylan sema
/// authoritative for the back-end. Not installed ⇒ the all-Rust
/// [`lower_module_full`] path.
static SEMA_DUMP_PROVIDER: std::sync::OnceLock<fn(&str) -> Result<String, String>> =
    std::sync::OnceLock::new();

/// Install the `--sema-with-dylan` model-dump provider (first install wins,
/// like the parser override). `Err` returns the provider back if already set.
pub fn set_sema_dump_provider(
    f: fn(&str) -> Result<String, String>,
) -> Result<(), fn(&str) -> Result<String, String>> {
    SEMA_DUMP_PROVIDER.set(f)
}

/// Sprint 55 — `--lower-with-dylan` provider. The driver installs a function
/// that, given the source text, returns the Dylan-produced DFM dump (by calling
/// the in-process `dylan-lower-emit` shim entry). When installed, opted-in
/// lowering paths reconstruct `Vec<Function>` from that dump
/// ([`nod_dfm::parse_dfm_module`]) and run the SAME back-end passes on it —
/// making the Dylan lowering authoritative. An empty dump (the Dylan lowering
/// bailed on an unsupported form) transparently falls back to the Rust lowering.
/// Composes with [`SEMA_DUMP_PROVIDER`]: sema source and lowering source are
/// chosen independently.
static DFM_DUMP_PROVIDER: std::sync::OnceLock<fn(&str) -> Result<String, String>> =
    std::sync::OnceLock::new();

/// Install the `--lower-with-dylan` DFM-dump provider (first install wins).
/// `Err` returns the provider back if already set.
pub fn set_dfm_dump_provider(
    f: fn(&str) -> Result<String, String>,
) -> Result<(), fn(&str) -> Result<String, String>> {
    DFM_DUMP_PROVIDER.set(f)
}

/// Lower `module` (already parsed + expanded from `src`), choosing the sema
/// recording source AND the lowering source independently. When the
/// `--sema-with-dylan` provider is installed, build the `SemaModel` from the
/// Dylan walk's dump; else use the all-Rust recording. When the
/// `--lower-with-dylan` provider is installed, replace the Rust Phase-3/4
/// functions with those reconstructed from the Dylan DFM dump (the back-end
/// passes then run on it). The single seam that makes the Dylan front end
/// load-bearing for `dump-dfm` (and, later, the compile paths).
fn lower_with_sema_choice(
    src: &str,
    module: &nod_reader::Module,
) -> Result<LoweredModule, DumpError> {
    // The Dylan lowering dump (if `--lower-with-dylan`): "" ⇒ bailed ⇒ Rust.
    let dfm_dump: Option<String> = match DFM_DUMP_PROVIDER.get() {
        Some(provider) => Some(provider(src).map_err(DumpError::LowerDylan)?),
        None => None,
    };
    let dfm_ref = dfm_dump.as_deref();
    match SEMA_DUMP_PROVIDER.get() {
        Some(provider) => {
            let dump = provider(src).map_err(DumpError::SemaDylan)?;
            let model =
                lower::analyse_module_from_dump(module, &dump).map_err(DumpError::SemaDylan)?;
            lower::lower_module_full_choice(module, Some(model), dfm_ref).map_err(DumpError::Lower)
        }
        None => lower::lower_module_full_choice(module, None, dfm_ref).map_err(DumpError::Lower),
    }
}

/// Driver helper: read a Dylan file, parse it, lower it, return the
/// indented DFM dump. The driver will wire this into `dump-dfm` itself —
/// this is the smallest function-shaped entry point that hides the
/// SourceMap + parser plumbing from the driver. Under `--sema-with-dylan`
/// (provider installed), the recording comes from the Dylan walk (54c).
pub fn dump_dfm_for_file(path: &Path) -> Result<String, DumpError> {
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(path).map_err(DumpError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm.add(path.to_path_buf(), src.clone()).map_err(DumpError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module = parse_user_module(&src, &toks, pre.as_ref()).map_err(DumpError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(DumpError::Macro)?;
    let lm = lower_with_sema_choice(&src, &module)?;
    Ok(nod_dfm::format_dfm_module(&lm.functions))
}

/// Sprint 53 — driver helper: read a Dylan file, parse + expand + run the
/// sema recording walk, and return the deterministic `SemaModel` dump
/// (top-names, generics, classes, sealing). This is the `dump-sema`
/// oracle the Dylan-computed model byte-matches against. Reuses the same
/// parse+expand+lower path as `dump-dfm` (the model is captured on
/// `LoweredModule` during lowering), then formats only the recording
/// half via [`lower::format_sema_model`].
pub fn dump_sema_for_file(path: &Path) -> Result<String, DumpError> {
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(path).map_err(DumpError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm.add(path.to_path_buf(), src.clone()).map_err(DumpError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module = parse_user_module(&src, &toks, pre.as_ref()).map_err(DumpError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(DumpError::Macro)?;
    let lm = lower_module_full(&module).map_err(DumpError::Lower)?;
    Ok(lower::format_sema_model(&lm.sema_model()))
}

/// Driver helper: read a Dylan file, parse + lower + codegen, return the
/// textual LLVM IR. Driver wires this into `dump-llvm`.
pub fn dump_llvm_for_file(path: &Path) -> Result<String, DumpError> {
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(path).map_err(DumpError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm.add(path.to_path_buf(), src.clone()).map_err(DumpError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module = parse_user_module(&src, &toks, pre.as_ref()).map_err(DumpError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(DumpError::Macro)?;
    let lm = lower_module_full(&module).map_err(DumpError::Lower)?;
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dylan-module");
    let ctx = Context::create();
    let out = codegen_module(&ctx, &lm.functions, module_name).map_err(DumpError::Codegen)?;
    Ok(out.module.print_to_string().to_string())
}

/// Parse + lower + codegen + JIT-call a full Dylan module containing
/// the supplied top-level items (e.g. `define c-function` declarations)
/// PLUS a synthetic `<eval-entry>` function whose body is `expr_src`.
/// Returns the formatted return value of `<eval-entry>`.
///
/// Sprint 28 uses this to test `define c-function` declarations followed
/// by call sites: the items are spliced into the module ABOVE the
/// synthetic entry function so c-function bindings are in scope when
/// the entry's body is lowered.
pub fn eval_expr_with_items_to_string(
    items_src: &str,
    expr_src: &str,
) -> Result<String, EvalError> {
    stdlib::ensure_loaded();
    // The blank line after `Module:` is load-bearing — `scan_preamble`
    // continues consuming lines as continuations of the previous
    // header key until it hits a blank line, so omitting it lets an
    // indented `(args)` line on a `define c-function` declaration get
    // swallowed by the preamble parser.
    let wrapped = format!(
        "Module: __eval__\n\
         \n\
         {items_src}\n\
         define function <eval-entry> ()\n  {expr_src}\nend;\n"
    );
    eval_wrapped_source(&wrapped)
}

/// Shared implementation for `eval_expr_to_string` and
/// `eval_expr_with_items_to_string` — handles parse → expand → lower
/// → codegen → JIT → invoke entry → format.
///
/// Sprint 37: routes through the JIT object-code cache. The cache key
/// is the hash of the wrapped Dylan source plus runtime/LLVM/target/
/// opt versioning. On an in-process hit, the entire pipeline (parse,
/// macro-expand, lower, codegen, MCJIT, registrations) is skipped —
/// only `call_and_format` runs against the previously-resolved
/// `<eval-entry>` pointer. See `nod-llvm::cache` for the full
/// mechanism + on-disk bitcode + LRU eviction.
///
/// We hash the **wrapped source string** (not the DFM IR text) because
/// (a) the wrapped string is deterministic by construction — it's
/// produced by `eval_expr_to_string` / `eval_expr_with_items_to_string`
/// from caller inputs with no environment dependency, (b) the full
/// pipeline is monotonic in the source: identical source → identical
/// DFM → identical LLVM IR (modulo audited nondeterminism sources,
/// fixed in Phase A) → identical object code, (c) hashing strings is
/// orders of magnitude faster than running the full pipeline to
/// produce DFM text, which is what makes the hot path actually fast.
fn eval_wrapped_source(wrapped: &str) -> Result<String, EvalError> {
    // Sprint 37 — try the in-process cache first. The hot path doesn't
    // even need to parse: a successful hit goes straight to
    // `call_and_format` on the cached entry pointer.
    let cache_key = nod_llvm::cache_key_for_dfm(wrapped);

    if let Some(replay) = nod_llvm::in_process_get(cache_key) {
        nod_llvm::record_hit();
        let ty = decode_type_tag(replay.return_type_tag, replay.return_type_payload);
        return Ok(call_and_format(replay.eval_entry_ptr as *const (), ty));
    }
    nod_llvm::record_miss();

    // Sprint 38f — try the on-disk replay path BEFORE running the cold
    // compile pipeline. Cross-process scenario: the in-process table is
    // empty in a fresh subprocess, but a previous run may have left
    // bitcode + manifest + registration sidecar on disk. If all three
    // are present and ABI-compatible, `add_module_from_bitcode` is
    // ~10× faster than the full parse→lower→codegen→MCJIT pipeline.
    //
    // Returns `Ok(Some(result))` on hit, `Ok(None)` if any sidecar is
    // missing / corrupt / version-incompatible (cold path then runs),
    // or `Err(_)` on a hard JIT failure (verify error, MCJIT engine
    // creation failure). The cold path doesn't retry on `Err` — a JIT
    // failure here means the bitcode on disk is broken in a way the
    // cold path would also hit.
    if let Some(result) = try_on_disk_replay(cache_key)? {
        return Ok(result);
    }

    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add("<eval>", wrapped.to_string())
        .map_err(EvalError::SourceMap)?;
    let toks = nod_reader::lex(wrapped, file_id);
    let pre = nod_reader::scan_preamble(wrapped);
    let mut module =
        parse_user_module(wrapped, &toks, pre.as_ref()).map_err(EvalError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    let lm = lower_module_full(&module).map_err(EvalError::Lower)?;

    let entry = lm
        .functions
        .iter()
        .find(|f| f.name == "<eval-entry>")
        .ok_or_else(|| EvalError::NoEntry("<eval-entry> missing after lowering".into()))?;
    let return_type = entry.return_type;

    // Sprint 37 — keep the LLVM Context and Jit alive forever so the
    // in-process replay closure stays valid. Each cache miss leaks
    // exactly one Context + Jit pair; on the next call to the same
    // source the replay closure reuses them. Memory cost: ~few KB per
    // distinct evaled source, capped in practice by the user's
    // working set. (The 500MB LRU cap applies to the on-disk bitcode
    // mirror, not the in-process table.)
    let ctx_box: Box<Context> = Box::new(Context::create());
    let ctx_ref: &'static Context = Box::leak(ctx_box);
    // Sprint 38b — use the cache-key-aware codegen entry point so
    // per-module symbol naming and the manifest both pick up the
    // same key the on-disk cache uses for lookup.
    let out = nod_llvm::codegen_module_with_key(ctx_ref, &lm.functions, "__eval__", cache_key)
        .map_err(EvalError::Codegen)?;

    // Capture the bitcode + manifest for the on-disk cache mirror
    // before MCJIT consumes the module pointer. Sprint 38b: cross-
    // process replay needs both — the bitcode is the canonical IR
    // shape and the manifest tells the loader how to recompute every
    // process-volatile address at JIT-link time.
    let bitcode_bytes = out.module.write_bitcode_to_memory();
    let bitcode: Vec<u8> = bitcode_bytes.as_slice().to_vec();
    let manifest = out.manifest.clone();

    let mut jit_box: Box<Jit<'static>> = Box::new(Jit::new(ctx_ref).map_err(EvalError::Jit)?);
    jit_box.add_module(out).map_err(EvalError::Jit)?;
    register_methods(&jit_box, &lm.methods)?;
    register_blocks(&jit_box, &lm.blocks)?;
    register_top_level_functions(&jit_box, &lm)?;
    initialize_module_winffi(&lm)?;
    // GAP-004 — variable init MUST follow `register_top_level_functions`
    // + winffi init: the init expression may call any user / stdlib
    // function or Win32 API, and those need to be resolvable through
    // the dispatch / function-ref registries first.
    register_variables(&jit_box, &lm.variables)?;

    // SAFETY: see `eval_expr_to_string`.
    let ptr = unsafe { jit_box.get_function_ptr("<eval-entry>") }
        .ok_or_else(|| EvalError::NoEntry("<eval-entry>".into()))?;

    // Leak the Jit (it owns the engine pointers; MCJIT engines are
    // never disposed, matching the existing `keep_forever` rationale
    // in `nod-llvm/src/jit.rs`).
    let jit_static: &'static Jit<'static> = Box::leak(jit_box);
    let _ = jit_static; // engines are leak-by-omission; the borrow is for documentation

    // Persist the bitcode + sidecar + manifest to disk. Best-effort:
    // errors are logged but don't fail the JIT (the cache is an
    // optimisation). Sprint 38b: switched from `write_cache_entry` to
    // `write_cache_entry_with_manifest` so cross-process replay has
    // the relocation table it needs to bind external globals.
    let dir = nod_llvm::default_cache_dir();
    nod_llvm::write_cache_entry_with_manifest(&dir, cache_key, &bitcode, &manifest);
    // Sprint 38f — also persist the registration sidecar so a fresh
    // subprocess's `try_on_disk_replay` has the data needed to wire up
    // `register_methods` / `register_blocks` /
    // `register_top_level_functions` without re-running the cold
    // pipeline. (Stub-entry registration is implicit — the manifest
    // already carries `RelocKind::StubEntry` entries, and
    // `stub_entry_slot_addr` eagerly resolves `LoadLibrary` +
    // `GetProcAddress` at JIT-link time.)
    let (ty_tag, ty_payload) = encode_type_tag(return_type);
    let regs = sidecar::RegistrationSidecar::from_lowered_module(&lm, ty_tag, ty_payload);
    regs.write(&dir, cache_key);
    // Run LRU eviction once on each insert if we're over the cap.
    let cap = nod_llvm::cache_max_bytes();
    if nod_llvm::cache_size_on_disk(&dir) > cap {
        let _ = nod_llvm::evict_to(&dir, cap);
    }

    // Install the in-process replay closure. The closure captures the
    // function pointer (`usize` to satisfy `Send + Sync` — pointers
    // aren't `Send` but `usize` is) and the encoded return type.
    let ptr_usize = ptr as usize;
    nod_llvm::in_process_insert(
        cache_key,
        Box::new(move || nod_llvm::JitReplayResult {
            eval_entry_ptr: ptr_usize,
            return_type_tag: ty_tag,
            return_type_payload: ty_payload,
        }),
    );

    Ok(call_and_format(ptr, return_type))
}

/// Sprint 51b — JIT-strap a pre-lowered module into an **isolated**
/// MCJIT engine and return a handle whose raw symbol addresses the
/// caller can transmute and invoke. Unlike [`eval_wrapped_source`],
/// this entry point does **not** synthesise an `<eval-entry>` wrapper;
/// the caller supplies any function name defined in `lm.functions` and
/// is responsible for the calling convention (`extern "C" fn(args...)
/// -> u64` for Word-typed Dylan top-levels — see
/// [`nod_runtime::Word`]).
///
/// **Isolation.** Each call leaks its own `Context` + `Jit` pair. The
/// returned handle owns the JIT engine for the process lifetime, so a
/// caller can hold the handle across many `get_function_ptr` calls
/// without worrying about lifetime. The Dylan runtime's global
/// registries (class table, dispatch cache, stub table) are still
/// shared with any other JIT engine the host runs — that's by design:
/// the side-loaded module's classes need to be visible to subsequent
/// JIT'd user code if/when the host decides to call into both.
///
/// **No `<eval-entry>`-style wrapping.** `module_name` is just the
/// LLVM module label used for diagnostics + cache-key derivation.
/// All registration paths (`register_methods`,
/// `register_top_level_functions`, `register_blocks`,
/// `register_variables`, `initialize_module_winffi`) run identically
/// to the eval cold path.
///
/// **Sprint 37/38 cache integration is deliberately skipped here.**
/// The intended caller (the `--lex-with-dylan` flag in `nod-driver`)
/// JITs the shim exactly once per process; the on-disk replay machinery
/// is the wrong sweet spot for this scale. A later sprint can swap to
/// the cached path by deriving a cache key from the LoweredModule's
/// DFM text and routing through `eval_wrapped_source`'s replay branch.
pub fn jit_lowered_module(
    lm: &LoweredModule,
    module_name: &str,
) -> Result<JittedModule, EvalError> {
    // Derive a deterministic cache key from the module name so the
    // codegen-side per-module symbol prefix is stable across runs
    // (Sprint 38b — every emitted symbol gets a key-prefix derived
    // from this). The key itself doesn't reach disk because we skip
    // the persist path; it's just an input to `codegen_module_with_key`.
    let cache_key = nod_llvm::cache_key_for_dfm(module_name);

    let ctx_box: Box<Context> = Box::new(Context::create());
    let ctx_ref: &'static Context = Box::leak(ctx_box);

    let verbose = std::env::var("NOD_JIT_LOWERED_VERBOSE").map(|v| v == "1").unwrap_or(false);
    if verbose {
        eprintln!("jit_lowered_module: codegen ({} fns) …", lm.functions.len());
    }
    let out = nod_llvm::codegen_module_with_key(ctx_ref, &lm.functions, module_name, cache_key)
        .map_err(EvalError::Codegen)?;

    if verbose {
        eprintln!("jit_lowered_module: Jit::new + add_module …");
    }
    let mut jit_box: Box<Jit<'static>> = Box::new(Jit::new(ctx_ref).map_err(EvalError::Jit)?);
    jit_box.add_module(out).map_err(EvalError::Jit)?;
    if verbose {
        eprintln!("jit_lowered_module: register_methods ({}) …", lm.methods.len());
    }
    register_methods(&jit_box, &lm.methods)?;
    if verbose {
        eprintln!("jit_lowered_module: register_blocks ({}) …", lm.blocks.len());
    }
    register_blocks(&jit_box, &lm.blocks)?;
    if verbose {
        eprintln!(
            "jit_lowered_module: register_top_level_functions ({} fns) …",
            lm.functions.len()
        );
    }
    register_top_level_functions(&jit_box, &lm)?;
    if verbose {
        eprintln!("jit_lowered_module: initialize_module_winffi …");
    }
    initialize_module_winffi(lm)?;
    if verbose {
        eprintln!("jit_lowered_module: register_variables ({}) …", lm.variables.len());
    }
    register_variables(&jit_box, &lm.variables)?;

    if verbose {
        eprintln!("jit_lowered_module: done");
    }
    let jit_static: &'static Jit<'static> = Box::leak(jit_box);
    Ok(JittedModule { jit: jit_static })
}

/// Sprint 51b — handle to a JIT engine populated by
/// [`jit_lowered_module`]. The MCJIT engine and its module are kept
/// alive for the process lifetime (matches the cold-eval lifetime
/// discipline); callers can hold this for arbitrarily many lookups.
pub struct JittedModule {
    jit: &'static Jit<'static>,
}

impl JittedModule {
    /// Resolve a JIT'd symbol to its raw function pointer. The caller
    /// is responsible for transmuting to the correct signature.
    ///
    /// # Safety
    /// The returned pointer is only valid while the host process lives
    /// (the underlying [`Jit`] is leaked-by-design). The caller's
    /// transmuted signature MUST match the Dylan-side function's
    /// calling convention — typically
    /// `extern "C" fn(arg0_word: u64, arg1_word: u64, ...) -> u64`
    /// for a Word-typed top-level Dylan function.
    pub unsafe fn get_function_ptr(&self, name: &str) -> Option<*const ()> {
        unsafe { self.jit.get_function_ptr(name) }
    }
}

/// Sprint 38f — on-disk cross-process replay path. The headline of
/// Sprint 38 was supposed to be ≥10× subprocess speedup; Sprint 38a–38e
/// shipped the relocation infrastructure (manifest, slot allocators,
/// `add_module_from_bitcode`) but `eval_wrapped_source` never read it
/// back from disk. This function closes that loop.
///
/// Steps on a successful hit:
///   1. Read `<key>.bc` + `<key>.json` (Sprint 37 sidecar) +
///      `<key>.manifest.json` (Sprint 38a sidecar) via
///      `read_cache_entry_with_manifest`.
///   2. Read `<key>.registrations.json` (Sprint 38f sidecar) via
///      `RegistrationSidecar::read`.
///   3. Spawn a fresh leaked `Context` + `Jit`.
///   4. Call `Jit::add_module_from_bitcode` — this parses bitcode,
///      walks the manifest, and `LLVMAddGlobalMapping`s each named
///      external global against a fresh current-process address (which
///      lazily allocates / resolves Win32 stub entries through
///      `nod_runtime::stub_entry_slot_addr`).
///   5. Replay the three sema-side registrations (methods, blocks,
///      top-level functions) using the persisted data. Stub-entry
///      registration is **not** needed — Sprint 38d's slot allocator
///      eagerly resolves `LoadLibrary` + `GetProcAddress` during
///      `resolve_reloc_kind` (called from `add_module_from_bitcode`),
///      so the post-codegen `initialize_module_winffi` is redundant on
///      this path.
///   6. Look up `<eval-entry>` by name in the JIT.
///   7. Leak the `Jit` (matches the cold path's lifetime discipline —
///      MCJIT engines are never disposed).
///   8. Install the in-process replay closure so subsequent calls hit
///      the Sprint 37 hot path.
///   9. Increment `record_disk_hit` and return the formatted result.
///
/// Returns `Ok(None)` if any sidecar is missing, corrupt, or
/// ABI-incompatible — the caller falls through to the cold compile
/// pipeline (which will overwrite the sidecars with fresh ones).
///
/// Returns `Err(_)` only on a hard JIT failure (bitcode that fails
/// `verify` after a round-trip, MCJIT engine creation failure, or a
/// reloc whose kind requires data the manifest didn't carry). These
/// are not recoverable by re-running the cold path — they indicate a
/// genuine sidecar corruption, so we surface the error rather than
/// silently masking it.
fn try_on_disk_replay(cache_key: nod_llvm::CacheKey) -> Result<Option<String>, EvalError> {
    let dir = nod_llvm::default_cache_dir();

    // Read the bitcode + manifest sidecars. Missing/malformed → disk
    // miss; cold path runs.
    let (bitcode, _meta, manifest) =
        match nod_llvm::read_cache_entry_with_manifest(&dir, cache_key) {
            Some(t) => t,
            None => {
                nod_llvm::record_disk_miss();
                return Ok(None);
            }
        };

    // Read the registration sidecar; ABI mismatch → disk miss.
    let regs = match sidecar::RegistrationSidecar::read(&dir, cache_key) {
        Some(r) if r.is_abi_compatible() => r,
        _ => {
            nod_llvm::record_disk_miss();
            return Ok(None);
        }
    };

    // Spawn a fresh Context + Jit. Same leak discipline as the cold
    // path: each disk-replay miss-to-hit transition leaks one
    // Context + Jit pair so the in-process replay closure stays valid
    // for the process lifetime.
    let ctx_box: Box<Context> = Box::new(Context::create());
    let ctx_ref: &'static Context = Box::leak(ctx_box);
    let mut jit_box: Box<Jit<'static>> = Box::new(Jit::new(ctx_ref).map_err(EvalError::Jit)?);
    jit_box
        .add_module_from_bitcode(ctx_ref, &bitcode, "__eval__", &manifest)
        .map_err(EvalError::Jit)?;

    // Replay the three sema-side registrations. The fourth (winffi
    // stub init) is redundant on this path — see the doc comment
    // above.
    replay_register_methods(&jit_box, &regs.methods)?;
    replay_register_blocks(&jit_box, &regs.blocks)?;
    replay_register_top_level_functions(&jit_box, &regs.functions)?;
    replay_register_variables(&jit_box, &regs.variables)?;

    // SAFETY: see `eval_wrapped_source` for the lifetime rationale.
    let ptr = unsafe { jit_box.get_function_ptr("<eval-entry>") }
        .ok_or_else(|| EvalError::NoEntry("<eval-entry>".into()))?;

    // Leak the Jit (engines are leak-by-omission; matches cold path).
    let jit_static: &'static Jit<'static> = Box::leak(jit_box);
    let _ = jit_static;

    // Install the in-process replay closure so subsequent calls in
    // this process hit the Sprint 37 hot path. The closure captures
    // the pointer as `usize` (raw pointers aren't `Send + Sync`).
    let ptr_usize = ptr as usize;
    let ty_tag = regs.return_type_tag;
    let ty_payload = regs.return_type_payload;
    nod_llvm::in_process_insert(
        cache_key,
        Box::new(move || nod_llvm::JitReplayResult {
            eval_entry_ptr: ptr_usize,
            return_type_tag: ty_tag,
            return_type_payload: ty_payload,
        }),
    );

    nod_llvm::record_disk_hit();
    let return_type = decode_type_tag(ty_tag, ty_payload);
    Ok(Some(call_and_format(ptr, return_type)))
}

/// Sprint 38f — registration replay for methods, paralleling
/// [`register_methods`] but reading from a [`PersistedMethod`] slice
/// (the data persisted on the cold path) instead of a
/// [`MethodRegistration`] slice (only available with a live
/// `LoweredModule`).
fn replay_register_methods(
    jit: &Jit<'_>,
    methods: &[sidecar::PersistedMethod],
) -> Result<(), EvalError> {
    use nod_runtime::ClassId;
    for m in methods {
        let ptr = unsafe { jit.get_function_ptr(&m.body_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!(
                "method body `{}` not JIT'd (on-disk replay)",
                m.body_fn_name
            ))
        })?;
        let specialisers: Vec<ClassId> =
            m.specialisers.iter().copied().map(ClassId).collect();
        // SAFETY: ptr is the live JIT'd address; specialisers came
        // from the persisted sidecar that the cold-compile path wrote
        // from the same module. Same contract as `register_methods`.
        unsafe {
            nod_runtime::add_method_named(
                &m.generic_name,
                specialisers,
                ptr as *const u8,
                m.param_count as usize,
                &m.body_fn_name,
            );
        }
    }
    Ok(())
}

/// Sprint 38f — registration replay for blocks. Mirrors
/// [`register_blocks`] but consumes a [`PersistedBlock`] slice. The
/// class-name leak pattern matches the cold path (those names live
/// for the process lifetime; the runtime stores raw pointers into
/// them).
fn replay_register_blocks(
    jit: &Jit<'_>,
    blocks: &[sidecar::PersistedBlock],
) -> Result<(), EvalError> {
    use nod_runtime::ClassId;
    for b in blocks {
        let body = unsafe { jit.get_function_ptr(&b.body_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!(
                "block body `{}` not JIT'd (on-disk replay)",
                b.body_fn_name
            ))
        })?;
        let cleanup = match &b.cleanup_fn_name {
            Some(n) => Some(
                unsafe { jit.get_function_ptr(n) }
                    .ok_or_else(|| {
                        EvalError::NoEntry(format!(
                            "block cleanup `{n}` not JIT'd (on-disk replay)"
                        ))
                    })? as *const u8,
            ),
            None => None,
        };
        let afterwards = match &b.afterwards_fn_name {
            Some(n) => Some(
                unsafe { jit.get_function_ptr(n) }
                    .ok_or_else(|| {
                        EvalError::NoEntry(format!(
                            "block afterwards `{n}` not JIT'd (on-disk replay)"
                        ))
                    })? as *const u8,
            ),
            None => None,
        };
        let handlers: Vec<nod_runtime::HandlerFn> = b
            .handlers
            .iter()
            .map(|h| {
                let p = unsafe { jit.get_function_ptr(&h.body_fn_name) }.ok_or_else(|| {
                    EvalError::NoEntry(format!(
                        "block handler `{}` not JIT'd (on-disk replay)",
                        h.body_fn_name
                    ))
                })?;
                let pinned: &'static str = Box::leak(h.class_name.clone().into_boxed_str());
                Ok(nod_runtime::HandlerFn {
                    class_id: ClassId(h.class_id),
                    class_name_ptr: pinned.as_ptr(),
                    class_name_len: pinned.len(),
                    body: p as *const u8,
                })
            })
            .collect::<Result<_, EvalError>>()?;
        let handlers_static: &'static [nod_runtime::HandlerFn] =
            Box::leak(handlers.into_boxed_slice());
        nod_runtime::register_block_fns(
            b.block_id,
            nod_runtime::BlockFns {
                body: body as *const u8,
                cleanup,
                afterwards,
                handlers: handlers_static,
            },
        );
    }
    Ok(())
}

/// Sprint 38f — registration replay for top-level functions. Mirrors
/// [`register_top_level_functions`] but consumes a
/// [`PersistedFunction`] slice. The cold path already filtered out
/// block-form thunks and `<eval-entry>` before persisting, so this
/// just walks every entry and registers it under the persisted
/// (source-) arity.
fn replay_register_top_level_functions(
    jit: &Jit<'_>,
    functions: &[sidecar::PersistedFunction],
) -> Result<(), EvalError> {
    for f in functions {
        let ptr = unsafe { jit.get_function_ptr(&f.name) }.ok_or_else(|| {
            EvalError::NoEntry(format!(
                "top-level function `{}` not JIT'd (on-disk replay)",
                f.name
            ))
        })?;
        // SAFETY: ptr is JIT-emitted; the arity matches the cold
        // path's registration arity (closures: source arity; non-
        // closures: params.len()).
        unsafe {
            nod_runtime::register_jit_function(&f.name, f.source_arity as usize, ptr as *const u8);
        }
    }
    Ok(())
}

/// GAP-004 — replay variable init from the on-disk sidecar. Mirrors
/// `register_variables` but reads from [`sidecar::PersistedVariable`]
/// (the data persisted on the cold path) instead of
/// [`crate::lower::VariableRegistration`] (only available when we
/// have a live `LoweredModule`).
fn replay_register_variables(
    jit: &Jit<'_>,
    variables: &[sidecar::PersistedVariable],
) -> Result<(), EvalError> {
    for v in variables {
        let ptr = unsafe { jit.get_function_ptr(&v.init_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!(
                "variable init `{}` not JIT'd (on-disk replay)",
                v.init_fn_name
            ))
        })?;
        // SAFETY: matches `extern "C-unwind" fn() -> u64`.
        unsafe {
            nod_runtime::nod_aot_register_variable(
                v.name.as_ptr(),
                v.name.len(),
                ptr as *const u8,
            );
        }
    }
    Ok(())
}

/// Sprint 37 — encode a `TypeEstimate` as a `(u32, u64)` pair that
/// fits in the cache's `JitReplayResult` shape. Inverse of
/// [`decode_type_tag`].
fn encode_type_tag(ty: TypeEstimate) -> (u32, u64) {
    match ty {
        TypeEstimate::Integer => (0, 0),
        TypeEstimate::Boolean => (1, 0),
        TypeEstimate::String => (2, 0),
        TypeEstimate::SingleFloat => (3, 0),
        TypeEstimate::DoubleFloat => (4, 0),
        TypeEstimate::Character => (5, 0),
        TypeEstimate::Unit => (6, 0),
        TypeEstimate::Top => (7, 0),
        TypeEstimate::Bottom => (8, 0),
        TypeEstimate::Class(id) => (9, id as u64),
        TypeEstimate::Singleton(bits) => (10, bits),
    }
}

fn decode_type_tag(tag: u32, payload: u64) -> TypeEstimate {
    match tag {
        0 => TypeEstimate::Integer,
        1 => TypeEstimate::Boolean,
        2 => TypeEstimate::String,
        3 => TypeEstimate::SingleFloat,
        4 => TypeEstimate::DoubleFloat,
        5 => TypeEstimate::Character,
        6 => TypeEstimate::Unit,
        7 => TypeEstimate::Top,
        8 => TypeEstimate::Bottom,
        9 => TypeEstimate::Class(payload as u32),
        10 => TypeEstimate::Singleton(payload),
        _ => TypeEstimate::Top,
    }
}

/// Parse + lower + codegen + JIT-call a single Dylan expression. Wraps
/// the expression in a synthetic `<eval-entry>` function whose inferred
/// return type drives the call signature. Single-shot.
pub fn eval_expr_to_string(expr_src: &str) -> Result<String, EvalError> {
    // Wrap the expression in a single-function module so `lower_module`
    // and `codegen_module` can run untouched. The function has no
    // params and no `=> (…)` annotation; lowering infers the return
    // type from the body's final temp.
    //
    // Allow the `let X; expr end` form: callers may write a sequence
    // of statements terminated by `end` (as in the SPRINTS.md acceptance
    // case `let x = 41; x + 1 end`). The Dylan grammar reserves `end`
    // for compound forms, so when the expression *starts* with `let`
    // we strip a trailing `end`; the wrapper supplies its own.
    stdlib::ensure_loaded();
    let trimmed = expr_src.trim();
    let body = if trimmed.starts_with("let ") || trimmed.starts_with("let\t") {
        trimmed.strip_suffix("end").map(str::trim_end).unwrap_or(trimmed)
    } else {
        trimmed
    };
    let wrapped = format!(
        "Module: __eval__\n\
         define function <eval-entry> ()\n  {body}\nend;\n"
    );
    eval_wrapped_source(&wrapped)
}

/// Sprint 28: walk the module's stub-table entries, build a runtime
/// [`nod_runtime::ApiStubTable`] view of them, and call
/// `nod_runtime::initialize_stub_table`. On failure the `<c-ffi-error>`
/// condition Word is surfaced as an `EvalError::WinFfiInit` — Sprint 28
/// tests use this to assert that bad DLL / symbol names raise the
/// expected error class. (The signal-handler path inside Dylan code
/// would catch it through `block/exception` instead; the eval
/// helper's caller doesn't have one of those.)
pub fn initialize_module_winffi(lm: &LoweredModule) -> Result<(), EvalError> {
    if lm.c_function_stub_table.is_empty() {
        return Ok(());
    }
    // The stub-table entries were already pinned in the static area
    // by the lowering pass; we just need to call
    // `initialize_stub_table` on the table descriptor. Recover it by
    // walking the first entry's pointer back to the slice it lives in
    // — except we don't carry a back-pointer. Simpler: rebuild a tiny
    // local slice view from the entry_ptrs and walk it directly via
    // the runtime helper. Each entry_ptr is a `*const ApiStubEntry`;
    // we can resolve them one by one. (`initialize_stub_table` was
    // designed around a static slice for layout-stability, but we
    // never carry that slice back to sema. Resolve each entry
    // directly here.)
    for spec in &lm.c_function_stub_table {
        let entry_ptr = spec.entry_ptr as *const nod_runtime::ApiStubEntry;
        // SAFETY: `entry_ptr` came from `nod_runtime::allocate_stub_table`,
        // which leaks a `Box<[ApiStubEntry]>` for the process lifetime.
        match unsafe { nod_runtime::resolve_into_entry(entry_ptr, &spec.dll, &spec.symbol) } {
            Ok(()) => {}
            Err(err_word) => {
                let class_name = nod_runtime::condition_class_name(err_word)
                    .unwrap_or_else(|| "<c-ffi-error>".to_string());
                return Err(EvalError::WinFfiInit {
                    class_name,
                    dll: spec.dll.clone(),
                    symbol: spec.symbol.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Sprint 39a — full parse + macro-expand + lower of a Dylan source
/// file, returning the [`LoweredModule`] ready for codegen + AOT
/// object emission. The pipeline matches `run_function_to_i64`'s
/// front-end but stops before JIT installation; the driver hands the
/// resulting module to `nod_llvm::aot::emit_aot_object`.
///
/// Includes `stdlib::ensure_loaded()` so the stdlib's macros and seed
/// generics are present in the parser + lowering tables before the
/// user file is processed.
///
/// **Sprint 39c** — the lowered stdlib is merged into the user's
/// [`LoweredModule`] before return so the resulting `.obj` carries
/// every Dylan-side body the dispatch / function-ref / block tables
/// need at AOT runtime. Stdlib items lose to user-defined items on
/// name conflicts (user wins). Embedding the full stdlib per-EXE is
/// wasteful; a future sprint can carve it out into a pre-compiled
/// `stdlib.obj` linked into every EXE. The minimum-viable-correctness
/// approach here keeps the AOT pipeline honest.
pub fn compile_file_for_aot(path: &Path) -> Result<LoweredModule, EvalError> {
    // Sprint 40a — eagerly run the EXE-side `nod_runtime_init` BEFORE
    // anything else touches the class registry. This is the same
    // initialisation the codegen-emitted resolver in the EXE calls;
    // running it here guarantees both processes seed their user-class
    // id counter (`next_user_id` in `nod-runtime/src/classes.rs`)
    // identically. Without this, Sprint 40a's
    // `nod_aot_register_user_class` assert fires on class-id drift:
    // e.g. `nod_runtime_init` calls `ensure_float_types_registered`
    // (2 extra user classes — `<c-float>` / `<c-double>`) that the
    // compiler-side's stdlib loader path doesn't, so EXE-side
    // allocations land 2 IDs higher than compiler-side. Calling
    // `nod_runtime_init` here flattens that delta.
    //
    // `nod_runtime_init` is idempotent (backed by a `LazyLock`); the
    // first call pays the cost, subsequent calls are an atomic load.
    nod_runtime::nod_runtime_init();
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(path).map_err(EvalError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add(path.to_path_buf(), src.clone())
        .map_err(EvalError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module =
        parse_user_module(&src, &toks, pre.as_ref()).map_err(EvalError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    // Sprint 56a-CONSUME — under the combined `--frontend-with-dylan` flag,
    // route the AOT path through the Dylan front-end seam
    // (`lower_with_sema_choice`) so the EXE is built from the Dylan-derived
    // sema/lowering — including the CONSUMED class table
    // (`install_dylan_classes`). Without this reroute the AOT path used
    // `lower_module_full` directly and never reached the consume, so the
    // class derivation was reachable only from `dump-dfm`; this makes it
    // load-bearing for an actual EXE. The eager `nod_runtime_init` above +
    // `merge_stdlib_into_user_module` below are preserved exactly, so the
    // host/EXE seed-registration ordering (and the AOT class-id-drift assert)
    // is unchanged. An empty Dylan lowering dump still falls back to the Rust
    // Phase-3/4 inside the seam, module-granular.
    let mut lm = if std::env::var("NOD_FRONTEND_WITH_DYLAN").as_deref() == Ok("1") {
        lower_with_sema_choice(&src, &module).map_err(|e| match e {
            DumpError::Io(e) => EvalError::Io(e),
            DumpError::SourceMap(e) => EvalError::SourceMap(e),
            DumpError::Parse(d) => EvalError::Parse(d),
            DumpError::Macro(m) => EvalError::Macro(m),
            DumpError::Lower(l) => EvalError::Lower(l),
            DumpError::Codegen(c) => EvalError::Codegen(c),
            DumpError::SemaDylan(s) | DumpError::LowerDylan(s) => {
                EvalError::FrontendWithDylan(s)
            }
        })?
    } else {
        lower_module_full(&module).map_err(EvalError::Lower)?
    };
    merge_stdlib_into_user_module(&mut lm);
    Ok(lm)
}

/// Sprint 44 — multi-file AOT compilation. Lower each `paths[i]`
/// individually (re-using `compile_file_for_aot`'s per-file
/// machinery), merge them pairwise into a single combined
/// [`LoweredModule`], then layer the stdlib on top once at the end.
///
/// **Order matters.** Files are lowered in the order passed. Each
/// file's lowering may register classes in the global class registry
/// (`nod-runtime::class_metadata_for` etc.); subsequent files can
/// reference those classes by name. So `paths[0]` must contain the
/// definitions of any classes referenced by `paths[1..]`, and so on.
/// For the IDE this lets us put low-level helpers in the first file
/// and the entry point in the last. Same discipline as the stdlib:
/// it lowers first, then user code references its classes.
///
/// **Module header consistency.** Every file's `Module:` header must
/// declare the same module name (or all files must be header-less);
/// otherwise we return [`EvalError::ModuleMismatch`]. This is the
/// "same module, multiple files" model — Sprint 44 deliberately does
/// not yet support cross-module imports (deferred to a real Dylan
/// library / module system, see DEFERRED.md).
///
/// **Cross-file collisions.** If two user files declare a top-level
/// definition with the same `body_fn_name` (functions or methods), we
/// return [`EvalError::DuplicateUserDefinition`]. This is stricter
/// than the user-vs-stdlib path where the user silently wins — there,
/// "user wins" is the documented override mechanism; here, the user's
/// intent is ambiguous and the right answer is to surface the
/// collision so they can rename or move one of the definitions.
///
/// Passing a single path is equivalent to (and behaves identically
/// to) calling `compile_file_for_aot` on that path — the merge loop
/// is a no-op when there's only one file.
pub fn compile_files_for_aot(paths: &[&Path]) -> Result<LoweredModule, EvalError> {
    compile_files_for_aot_with_shape(paths, /* library = */ false)
}

/// Sprint 51e — variant of [`compile_files_for_aot`] that knows whether
/// the artifact is a **front-end shim static library** (`--library`).
///
/// A shim library carries its own `define class`es (`<token>`,
/// `<ast-*>`, …) and is designed to be statically linked into a host
/// (`nod-driver`) whose class registry is already populated by the
/// stdlib. To keep the shim's classes from colliding with — and shifting
/// — the host's `FIRST_USER..` user-class ids, the shim's OWN classes
/// are minted from the disjoint shim band (`ClassId::FIRST_SHIM..`): we
/// flip [`nod_runtime::set_shim_class_band_active`] ON across the shim
/// source's `lower_module_full` (the only phase that registers the
/// shim's classes), having already let the stdlib load with its
/// canonical `FIRST_USER..` ids. A plain user build leaves the band OFF,
/// so its classes allocate from `FIRST_USER..` exactly as before — see
/// `ClassId::FIRST_SHIM`'s doc for the full rationale.
pub fn compile_files_for_aot_with_shape(
    paths: &[&Path],
    library: bool,
) -> Result<LoweredModule, EvalError> {
    if paths.is_empty() {
        return Err(EvalError::Lower(vec![]));
    }

    // Same eager init as the single-file path.
    nod_runtime::nod_runtime_init();
    stdlib::ensure_loaded();

    // **AST-level merge, NOT LoweredModule-level merge.** The earlier
    // Sprint 44 attempt lowered each file separately and tried to
    // stitch the `LoweredModule`s together via a `merge_modules`
    // helper. That silently dropped six fields the merge didn't know
    // about (`c_functions`, `c_function_stub_table`, `sealing`,
    // `resolutions`, `warnings`, plus any future addition) and — more
    // catastrophically — invalidated the per-file `WinFfiCall`
    // stub-table indices baked into the lowered IR, because each
    // file's `lower_module_full` numbers its stub-table entries from
    // 0 independently. When the merged stub-table only contained
    // file 1's entries, file 2's call sites still indexed into it as
    // if they were file-2-local, hit the wrong row, and Win32 got
    // junk pointers — observed as a runtime panic
    // `winffi: expected Dylan <byte-string> ... got raw Word 0x18520`
    // on the very first string-typed Win32 call.
    //
    // The fix mirrors how `nod-sema::stdlib::load_stdlib` already
    // composes its own multi-file source: parse each file separately,
    // concatenate their `items` into one combined `Module` AST, then
    // call `lower_module_full` exactly once. All counters, registries,
    // and per-module tables are assigned in a single coherent pass —
    // no merge fragility, no fields-I-forgot-to-merge bugs.
    //
    // The first file's preamble carries the module header that the
    // post-merge `Module` reports; we still verify every file's
    // declared `Module:` value agrees before merging.
    let mut sm = nod_reader::SourceMap::new();
    let mut declared_modules: Vec<(std::path::PathBuf, String)> =
        Vec::with_capacity(paths.len());
    let mut merged: Option<nod_reader::Module> = None;

    for path in paths {
        let src = std::fs::read_to_string(path).map_err(EvalError::Io)?;
        let file_id = sm
            .add(path.to_path_buf(), src.clone())
            .map_err(EvalError::SourceMap)?;
        let toks = nod_reader::lex(&src, file_id);
        let pre = nod_reader::scan_preamble(&src);
        let mut parsed =
            parse_user_module(&src, &toks, pre.as_ref()).map_err(EvalError::Parse)?;

        let mod_name = parsed
            .header
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("module"))
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default();
        declared_modules.push(((*path).to_path_buf(), mod_name));

        match &mut merged {
            None => merged = Some(parsed),
            Some(m) => m.items.append(&mut parsed.items),
        }
    }

    // Module-header consistency check: every file must declare the
    // same `Module:` value (or all be header-less). The first
    // non-empty declared value is the reference; any disagreement is
    // an error before we touch lowering.
    let reference = declared_modules
        .iter()
        .map(|(_, m)| m.as_str())
        .find(|m| !m.is_empty())
        .unwrap_or("");
    if declared_modules
        .iter()
        .any(|(_, m)| !m.is_empty() && m != reference)
    {
        return Err(EvalError::ModuleMismatch {
            files: declared_modules,
        });
    }

    let mut module = merged.expect("at least one file (checked at fn entry)");

    // Cross-file duplicate-definition detection at AST level. The
    // lowering pass DOESN'T diagnose this — it silently uses one body
    // for codegen and registers the other's name in the AOT resolver,
    // which then surfaces as an `unresolved external symbol` linker
    // error several minutes into the build. We catch it upfront here
    // so the user gets a clear "two files defined `helper`" message
    // instead of a cryptic LNK2019.
    //
    // We don't track WHICH source file each item came from after the
    // AST merge (the spans still point at the right SourceMap entry,
    // but reconstructing the file path from the span here would be
    // more wiring than the value justifies), so the diagnostic names
    // the duplicated identifier and the user finds it via grep.
    {
        use nod_reader::Item;
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        let mut first_duplicate: Option<String> = None;
        for item in &module.items {
            let name = match item {
                // `define function` / `define class` / etc. introduce a
                // unique top-level binding; duplicates across files are
                // an error. `define method` is DELIBERATELY excluded —
                // multiple methods on the same generic legitimately share
                // the generic's name (that's how multi-method dispatch
                // works), and dedup at this level would false-positive on
                // every IDE rope method (`rope-size <rope-leaf>`,
                // `rope-size <rope-node>`, …).
                Item::DefineFunction { name, .. }
                | Item::DefineGeneric { name, .. }
                | Item::DefineConstant { name, .. }
                | Item::DefineVariable { name, .. }
                | Item::DefineClass { name, .. } => Some(name.clone()),
                _ => None,
            };
            if let Some(n) = name
                && !seen.insert(n.clone())
            {
                first_duplicate = Some(n);
                break;
            }
        }
        if let Some(name) = first_duplicate {
            // We don't know which two files contributed the dup — use
            // the first and last passed paths as a hint. Better than
            // nothing; if the user has more than two files we point at
            // the bookends and they grep for the name.
            return Err(EvalError::DuplicateUserDefinition {
                name,
                first_path: paths[0].to_path_buf(),
                second_path: paths[paths.len() - 1].to_path_buf(),
            });
        }
    }

    // Single macro-expansion + lowering pass over the combined AST.
    // Everything sees everything: cross-file function references
    // resolve naturally (they're all in the same `module.items`
    // list), the closure lifter sees the union of top-level names,
    // c-function stub-table indices are assigned monotonically across
    // the whole user module, and `register_user_class` runs in
    // source-file order without any per-file segmentation. The
    // resulting `LoweredModule` is structurally identical to what
    // the single-file path produces from a hand-concatenated source.
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    // Sprint 51e — mint the shim library's OWN classes from the shim
    // band so they don't consume `FIRST_USER..` ids. The stdlib already
    // loaded above (its classes keep their canonical low ids); only this
    // source's `register_class` calls run under the band. Restored
    // unconditionally afterwards so nothing else in the process inherits
    // the band. (No-op for a normal user build, where `library` is
    // false.)
    if library {
        nod_runtime::set_shim_class_band_active(true);
    }
    // Sprint 56a-CONSUME — under `--frontend-with-dylan`, route the (single-file)
    // AOT build through the Dylan front-end seam (`lower_with_sema_choice`) so the
    // EXE is built from the CONSUMED Dylan class table (`install_dylan_classes`),
    // making the Dylan class derivation load-bearing for an actual EXE rather than
    // only `dump-dfm`. The seam re-runs the Dylan sema/lower shim on the source
    // text; for the single-file case the merged module IS that one source, so we
    // re-read it. Multi-file builds keep the Rust path (the seam's provider
    // contract is `fn(&str)`; concatenated multi-file source isn't reconstructed
    // here). Empty Dylan dumps still fall back to Rust Phase-3/4 module-granular
    // inside the seam. The eager init + shim-band toggle above are preserved, so
    // the host/EXE seed ordering (and the AOT drift assert) is unchanged.
    let consume_single = std::env::var("NOD_FRONTEND_WITH_DYLAN").as_deref() == Ok("1")
        && paths.len() == 1
        // Never route a front-end-shim static-library build (`--library`)
        // through the consume: the shim's own classes are minted from the
        // disjoint shim band and aren't emitted in the `=== classes ===` dump
        // format the consume parses. The shim build never sets the flag, but
        // guard anyway so the band toggle above stays the only shim path.
        && !library;
    let lowered = if consume_single {
        let src = std::fs::read_to_string(paths[0]).map_err(EvalError::Io)?;
        lower_with_sema_choice(&src, &module).map_err(|e| match e {
            DumpError::Io(e) => EvalError::Io(e),
            DumpError::SourceMap(e) => EvalError::SourceMap(e),
            DumpError::Parse(d) => EvalError::Parse(d),
            DumpError::Macro(m) => EvalError::Macro(m),
            DumpError::Lower(l) => EvalError::Lower(l),
            DumpError::Codegen(c) => EvalError::Codegen(c),
            DumpError::SemaDylan(s) | DumpError::LowerDylan(s) => EvalError::FrontendWithDylan(s),
        })
    } else {
        lower_module_full(&module).map_err(EvalError::Lower)
    };
    if library {
        nod_runtime::set_shim_class_band_active(false);
    }
    let mut lm = lowered?;
    merge_stdlib_into_user_module(&mut lm);
    Ok(lm)
}

/// Sprint 51b — JIT-pipeline sibling of [`compile_files_for_aot`].
/// Runs the same parse → concat-AST → expand → lower pipeline but
/// **does NOT** merge the stdlib into the result. The eval / JIT
/// pipeline depends on stdlib methods + functions being resolved
/// against the **globally-registered** registry that `stdlib::ensure_loaded`
/// populates at process startup, not against an in-module
/// duplication: a merge would re-register every stdlib method with
/// uninitialised JIT-link addresses (the in-module copies don't have
/// JIT-resolvable bodies yet at `register_methods` time), and the
/// global table would crash on first dispatch because Sprint 39c's
/// AOT-shaped resolver gets confused by the duplicate registrations.
///
/// The output `LoweredModule`'s `functions` / `methods` / `blocks`
/// contain ONLY user-defined items; calls to stdlib methods (e.g.
/// `format-out`, `<byte-string>` arithmetic) are codegen-emitted as
/// references to externally-linked symbols which the JIT resolves
/// via the runtime's global tables.
pub fn compile_files_for_jit(paths: &[&Path]) -> Result<LoweredModule, EvalError> {
    if paths.is_empty() {
        return Err(EvalError::Lower(vec![]));
    }

    nod_runtime::nod_runtime_init();
    stdlib::ensure_loaded();

    let mut sm = nod_reader::SourceMap::new();
    let mut declared_modules: Vec<(std::path::PathBuf, String)> =
        Vec::with_capacity(paths.len());
    let mut merged: Option<nod_reader::Module> = None;

    for path in paths {
        let src = std::fs::read_to_string(path).map_err(EvalError::Io)?;
        let file_id = sm
            .add(path.to_path_buf(), src.clone())
            .map_err(EvalError::SourceMap)?;
        let toks = nod_reader::lex(&src, file_id);
        let pre = nod_reader::scan_preamble(&src);
        let mut parsed =
            parse_user_module(&src, &toks, pre.as_ref()).map_err(EvalError::Parse)?;

        let mod_name = parsed
            .header
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("module"))
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default();
        declared_modules.push((path.to_path_buf(), mod_name));

        match &mut merged {
            None => merged = Some(parsed),
            Some(m) => m.items.append(&mut parsed.items),
        }
    }

    let reference = declared_modules
        .iter()
        .map(|(_, m)| m.as_str())
        .find(|m| !m.is_empty())
        .unwrap_or("");
    if declared_modules
        .iter()
        .any(|(_, m)| !m.is_empty() && m != reference)
    {
        return Err(EvalError::ModuleMismatch {
            files: declared_modules,
        });
    }

    let mut module = merged.expect("at least one file (checked at fn entry)");
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    let lm = lower_module_full(&module).map_err(EvalError::Lower)?;
    Ok(lm)
}

/// Sprint 39c — build the AOT registration payload from a (post-
/// merge) [`LoweredModule`]. The driver passes the resulting
/// [`nod_llvm::AotRegistrations`] to
/// `emit_aot_object_with_registrations` so the AOT resolver in the
/// emitted EXE registers every Dylan-side method / block / top-level
/// function body with the runtime dispatch / function-ref / block
/// registries BEFORE `nod_user_main` runs.
///
/// Mirrors `register_methods` / `register_blocks` /
/// `register_top_level_functions` but emits descriptors instead of
/// calling the runtime helpers directly — the AOT path can't take
/// the JIT-style "look up the symbol, then call the helper" route
/// because there's no JIT engine; the LLVM symbol-to-address binding
/// happens at link/load time.
pub fn build_aot_registrations(lm: &LoweredModule) -> nod_llvm::AotRegistrations {
    use nod_llvm::{
        AotBlockHandlerRegistration, AotBlockRegistration, AotFunctionRegistration,
        AotMethodRegistration, AotRegistrations, AotSlotRegistration,
        AotUserClassRegistration, AotVariableRegistration,
    };
    use nod_runtime::{SlotDefault, SlotType};
    let mut out = AotRegistrations::default();
    // Sprint 40a — user classes (replayed FIRST inside the resolver
    // so subsequent method registrations see live class metadata).
    for c in &lm.user_classes {
        let slots: Vec<AotSlotRegistration> = c
            .slots
            .iter()
            .map(|s| {
                let (type_tag, type_class_id) = encode_slot_type(s.type_kind);
                // GAP-009: boolean / nil defaults are process-specific
                // immediate Words (their bits embed a pointer into the
                // literal pool / static area), so baking the compile-time
                // bits into AOT registration produces a stale pointer in
                // the EXE process — reading the slot then faults. Encode
                // them symbolically (tags 2/3/4) and let the AOT registrar
                // re-resolve them from the EXE's own immediates. Fixnums
                // and other process-stable Words stay raw (tag 1). Mirrors
                // Sprint 38b's immediate relocation for codegen bake-sites,
                // which the slot-default path had missed.
                let imm = nod_runtime::literal_pool_immediates();
                let (default_init_tag, default_init_value) = match s.default_init {
                    SlotDefault::Unbound => (0u8, 0u64),
                    SlotDefault::Value(w) if w.raw() == imm.true_.raw() => (2u8, 0u64),
                    SlotDefault::Value(w) if w.raw() == imm.false_.raw() => (3u8, 0u64),
                    SlotDefault::Value(w) if w.raw() == imm.nil.raw() => (4u8, 0u64),
                    SlotDefault::Value(w) => (1u8, w.raw()),
                };
                AotSlotRegistration {
                    name: s.name.clone(),
                    offset: s.offset,
                    type_tag,
                    type_class_id,
                    init_keyword: s.init_keyword.clone(),
                    required_init_keyword: s.required_init_keyword,
                    default_init_tag,
                    default_init_value,
                    has_setter: s.has_setter,
                }
            })
            .collect();
        out.user_classes.push(AotUserClassRegistration {
            name: c.name.clone(),
            class_id: c.class_id.0,
            parent_class_ids: c.parents.iter().map(|p| p.0).collect(),
            cpl: c.cpl.iter().map(|p| p.0).collect(),
            slots,
            slot_origin: c.slot_origin.iter().map(|p| p.0).collect(),
            own_slot_count: c.own_slot_count,
            inherited_slot_count: c.inherited_slot_count,
        });
        let _ = SlotType::Top; // touch the import so it survives clippy
    }
    // Methods.
    for m in &lm.methods {
        out.methods.push(AotMethodRegistration {
            generic_name: m.generic_name.clone(),
            specialisers: m.specialisers.iter().map(|c| c.0).collect(),
            body_fn_name: m.body_fn_name.clone(),
            param_count: m.param_count,
        });
    }
    // Blocks.
    for b in &lm.blocks {
        let handlers = b
            .handlers
            .iter()
            .map(|h| AotBlockHandlerRegistration {
                class_id: h.class_id.0,
                class_name: h.class_name.clone(),
                body_fn_name: h.body_fn_name.clone(),
            })
            .collect();
        out.blocks.push(AotBlockRegistration {
            block_id: b.block_id,
            body_fn_name: b.body_fn_name.clone(),
            cleanup_fn_name: b.cleanup_fn_name.clone(),
            afterwards_fn_name: b.afterwards_fn_name.clone(),
            handlers,
        });
    }
    // Top-level functions. Mirrors `register_top_level_functions`:
    // skip block-form lifted thunks (they have a non-standard ABI)
    // and skip the synthetic `<eval-entry>` (not reachable as a
    // top-level Dylan function).
    let mut block_thunk_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for b in &lm.blocks {
        block_thunk_names.insert(b.body_fn_name.clone());
        if let Some(n) = &b.cleanup_fn_name {
            block_thunk_names.insert(n.clone());
        }
        if let Some(n) = &b.afterwards_fn_name {
            block_thunk_names.insert(n.clone());
        }
        for h in &b.handlers {
            block_thunk_names.insert(h.body_fn_name.clone());
        }
    }
    for f in &lm.functions {
        if block_thunk_names.contains(&f.name) {
            continue;
        }
        if f.name == "<eval-entry>" {
            continue;
        }
        let arity = if let Some(info) = lm.closures.closure_for(&f.name) {
            info.arity
        } else {
            f.params.len()
        };
        out.functions.push(AotFunctionRegistration {
            name: f.name.clone(),
            arity,
            body_fn_name: f.name.clone(),
        });
    }
    // GAP-004 — variable registrations. The AOT resolver runs these
    // last, AFTER classes / methods / blocks / functions, because an
    // init expression can call any user / stdlib function.
    for v in &lm.variables {
        out.variables.push(AotVariableRegistration {
            name: v.name.clone(),
            init_fn_name: v.init_fn_name.clone(),
        });
    }
    out
}

/// Sprint 40a — encode a [`nod_runtime::SlotType`] as the `(tag,
/// class_id)` pair the AOT shim consumes. The tag value matches the
/// `AOT_SLOT_TYPE_*` constants in `nod-runtime::aot`; the payload
/// `class_id` is meaningful only for `Class(_)` (zero otherwise).
///
/// Keep this in lockstep with the decoder in `nod-runtime::aot`. The
/// tag space is dense and stable; new variants append.
fn encode_slot_type(t: nod_runtime::SlotType) -> (u8, u32) {
    use nod_runtime::SlotType::*;
    match t {
        Integer => (0, 0),
        DoubleFloat => (1, 0),
        Boolean => (2, 0),
        Character => (3, 0),
        String => (4, 0),
        Symbol => (5, 0),
        Vector => (6, 0),
        Object => (7, 0),
        Class(c) => (8, c.0),
        Top => (9, 0),
    }
}

/// Sprint 39c — append the stdlib's lowered DFM artefacts onto the
/// user's [`LoweredModule`] so the resulting `.obj` carries every
/// Dylan-side function body the AOT-runtime needs. The merge keeps
/// user definitions (functions / methods / blocks) intact; conflicts
/// on identical `body_fn_name` resolve in the user's favour by
/// skipping the stdlib copy. Block IDs are runtime-allocated and
/// already process-unique, so they never collide.
///
/// The merge runs AFTER `lower_module_full` so the user's IR has
/// finished resolving Dispatch nodes (which may target stdlib
/// generics by name); the user's `methods` / `functions` / `blocks`
/// vectors are then extended with the stdlib's. Codegen sees the
/// concatenated function list and emits one LLVM function per entry;
/// the AOT post-processing pass then walks the merged
/// `methods` / `blocks` / function lists to emit startup registration
/// calls so dispatch, the block registry, and the function-ref
/// registry are populated before `nod_user_main` runs.
fn merge_stdlib_into_user_module(lm: &mut LoweredModule) {
    let Some(stdlib_lm) = stdlib::stdlib_lowered() else {
        // ensure_loaded ran first; this branch should be unreachable.
        // If it ever fires the user gets a useful diagnostic out of
        // codegen ("undefined symbol size$<object>") rather than a
        // crash, so we don't panic here.
        return;
    };
    merge_modules(lm, stdlib_lm);
}

/// Sprint 44 Phase A — generalised module-merge.
///
/// Concatenate `from` into `into`, with `into` winning every
/// collision. Originally extracted from `merge_stdlib_into_user_module`
/// so the same primitive can be reused by Sprint 44's multi-file
/// `compile_files_for_aot` path: it merges N per-file `LoweredModule`s
/// pairwise into one combined user module before the stdlib is layered
/// on top.
///
/// **Collision policy: "into" always wins.** When `from` carries a
/// function or method whose `name` / `body_fn_name` is already present
/// in `into`, the `from` entry is silently skipped. This matches the
/// established stdlib-merge semantics ("user wins over stdlib") and
/// keeps codegen from emitting duplicate LLVM definitions. Callers
/// that need *error*-on-collision (e.g. detecting two user files
/// defining the same function) must walk the inputs themselves before
/// calling this — `compile_files_for_aot` does exactly that.
///
/// **Per-section semantics** (unchanged from the stdlib-only era):
/// * **functions** — dedup on `name`; "into" wins.
/// * **methods** — dedup on `body_fn_name`; "into" wins.
/// * **blocks** — concatenate (block IDs are runtime-allocated and
///   process-unique, so collisions are structurally impossible).
/// * **closures** — extend `by_lifted_name` and
///   `cell_locals_per_function` with `or_insert_with` (first writer
///   wins; since "into" was populated first, that means "into" wins).
/// * **user_classes** — Sprint 40a: prepend `from`'s classes ahead of
///   `into`'s so the EXE-side `nod_aot_register_user_class` call
///   sequence matches the JIT path's ClassId allocation order. For
///   the stdlib case `from` is the stdlib (registered first in the
///   JIT) and `into` is the user; for user-vs-user merges in
///   `compile_files_for_aot`, the function is called in source-file
///   order so the per-file classes interleave consistently with the
///   driver's input order.
pub(crate) fn merge_modules(into: &mut LoweredModule, from: &LoweredModule) {
    use std::collections::HashSet;
    let existing_fn_names: HashSet<String> =
        into.functions.iter().map(|f| f.name.clone()).collect();
    for f in &from.functions {
        if existing_fn_names.contains(&f.name) {
            continue;
        }
        into.functions.push(f.clone());
    }
    let existing_method_bodies: HashSet<String> = into
        .methods
        .iter()
        .map(|m| m.body_fn_name.clone())
        .collect();
    for m in &from.methods {
        if existing_method_bodies.contains(&m.body_fn_name) {
            continue;
        }
        into.methods.push(m.clone());
    }
    for b in &from.blocks {
        into.blocks.push(b.clone());
    }
    for (k, v) in &from.closures.by_lifted_name {
        into.closures
            .by_lifted_name
            .entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    for (k, v) in &from.closures.cell_locals_per_function {
        into.closures
            .cell_locals_per_function
            .entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    if !from.user_classes.is_empty() {
        let mut merged: Vec<UserClassRegistration> =
            Vec::with_capacity(from.user_classes.len() + into.user_classes.len());
        merged.extend(from.user_classes.iter().cloned());
        merged.append(&mut into.user_classes);
        into.user_classes = merged;
    }
    // GAP-004 — variables. Stdlib defines none today, but if it ever
    // does, the same "from first, into second" ordering matters: a
    // stdlib `define variable` should initialise before any user code
    // (including user variables that might transitively read it via a
    // function call from the init expression). Per-name dedup is by
    // VariableRegistration.name; user wins on collision.
    if !from.variables.is_empty() {
        use std::collections::HashSet;
        let existing_var_names: HashSet<String> =
            into.variables.iter().map(|v| v.name.clone()).collect();
        let mut merged: Vec<VariableRegistration> =
            Vec::with_capacity(from.variables.len() + into.variables.len());
        for v in &from.variables {
            if !existing_var_names.contains(&v.name) {
                merged.push(v.clone());
            }
        }
        merged.append(&mut into.variables);
        into.variables = merged;
    }
}

/// Sprint 31: parse the same `items + expr` shape as
/// [`eval_expr_with_items_to_string`] but stop after lowering and
/// return the module's c-function bindings. Tests use this to inspect
/// which bindings the JIT-materialization path synthesized (and to
/// confirm A/W disambiguation + user-override-wins behavior) without
/// actually invoking a Win32 API.
pub fn introspect_bindings(
    items_src: &str,
    expr_src: &str,
) -> Result<Vec<CFunctionBinding>, EvalError> {
    stdlib::ensure_loaded();
    let wrapped = format!(
        "Module: __eval__\n\
         \n\
         {items_src}\n\
         define function <eval-entry> ()\n  {expr_src}\nend;\n"
    );
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add("<eval>", wrapped.clone())
        .map_err(EvalError::SourceMap)?;
    let toks = nod_reader::lex(&wrapped, file_id);
    let pre = nod_reader::scan_preamble(&wrapped);
    let mut module =
        parse_user_module(&wrapped, &toks, pre.as_ref()).map_err(EvalError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    let lm = lower_module_full(&module).map_err(EvalError::Lower)?;
    Ok(lm.c_functions)
}

/// Lower `source_path`, JIT it, look up `entry_name` (a `() => <integer>`
/// function), call once, return its `i64` result.
pub fn run_function_to_i64(
    source_path: &Path,
    entry_name: &str,
) -> Result<i64, EvalError> {
    stdlib::ensure_loaded();
    let src = std::fs::read_to_string(source_path).map_err(EvalError::Io)?;
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add(source_path.to_path_buf(), src.clone())
        .map_err(EvalError::SourceMap)?;
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    let mut module =
        parse_user_module(&src, &toks, pre.as_ref()).map_err(EvalError::Parse)?;
    expand_with_stdlib_macros(&mut module, &sm).map_err(EvalError::Macro)?;
    let lm = lower_module_full(&module).map_err(EvalError::Lower)?;

    let target = lm
        .functions
        .iter()
        .find(|f| f.name == entry_name)
        .ok_or_else(|| EvalError::NoEntry(entry_name.to_string()))?;
    if !matches!(target.return_type, TypeEstimate::Integer) {
        return Err(EvalError::ReturnTypeMismatch {
            entry: entry_name.to_string(),
            expected: "<integer>",
            actual: target.return_type.name(),
        });
    }

    let module_name = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dylan-module");
    let ctx = Context::create();
    let out = codegen_module(&ctx, &lm.functions, module_name).map_err(EvalError::Codegen)?;
    let mut jit = Jit::new(&ctx).map_err(EvalError::Jit)?;
    jit.add_module(out).map_err(EvalError::Jit)?;
    register_methods(&jit, &lm.methods)?;
    register_blocks(&jit, &lm.blocks)?;
    register_top_level_functions(&jit, &lm)?;
    initialize_module_winffi(&lm)?;

    let ptr = unsafe { jit.get_function_ptr(entry_name) }
        .ok_or_else(|| EvalError::NoEntry(entry_name.to_string()))?;
    // SAFETY: target.return_type is Integer (checked above), no params
    // on the DFM function, and `<integer>` lowers to a tagged `Word`
    // (Sprint 09 ABI). Engine outlives the call.
    let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
    let w = nod_runtime::Word::from_raw(f());
    w.as_fixnum().ok_or_else(|| EvalError::ReturnTypeMismatch {
        entry: entry_name.to_string(),
        expected: "<integer> (fixnum)",
        actual: "<pointer-tagged> (Sprint 09 has no boxed integers)",
    })
}

/// Resolve every method's body function in the JIT and register the
/// resulting `(specialisers, fn_ptr)` pair in the runtime's dispatch
/// table. Runs once after `Jit::add_module` returns; the registrations
/// are process-global so subsequent calls just see them.
///
/// Sprint 13 passes the full specialiser list to
/// `nod_runtime::add_method_full` so multi-argument dispatch
/// (`intersect(<rect>, <circle>)` etc.) picks the right method.
/// Sprint 21: register every top-level Dylan function in the lowered
/// module with the runtime's function-ref registry, so that
/// `nod_make_function_ref(name, arity)` resolves to the JIT-emitted
/// address. Skips block-form lifted thunks (body / cleanup /
/// afterwards / handler) — those have a different ABI and aren't
/// callable from a `<function>` Word.
pub fn register_top_level_functions(
    jit: &Jit<'_>,
    lm: &LoweredModule,
) -> Result<(), EvalError> {
    // Build the set of names belonging to block-form lifted thunks
    // (the only Dylan-level functions that DON'T match the regular
    // `(u64, ..., u64) -> u64` calling convention; their leading
    // params are the captured-locals slots, not user args). Sprint 19
    // emits these with predictable names — we read them out of the
    // `blocks` registration list.
    let mut block_thunk_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in &lm.blocks {
        block_thunk_names.insert(b.body_fn_name.clone());
        if let Some(n) = &b.cleanup_fn_name {
            block_thunk_names.insert(n.clone());
        }
        if let Some(n) = &b.afterwards_fn_name {
            block_thunk_names.insert(n.clone());
        }
        for h in &b.handlers {
            block_thunk_names.insert(h.body_fn_name.clone());
        }
    }
    // Auto-generated slot accessors and method bodies belong to
    // generics; we register THEIR names ALSO so `\size` on a method-
    // name resolves to the generic dispatcher. But since those are
    // already in `lm.methods` and registered into the dispatch table
    // with `add_method_named`, we can rely on the generic registry
    // instead — `nod_make_function_ref` won't need a separate entry
    // for `size`.
    //
    // Sprint 21 simplification: register EVERY top-level function
    // whose name isn't a block thunk. The function-ref registry is
    // keyed on `(name, arity)`, so collisions are impossible across
    // arities.
    //
    // Sprint 26: dropped the body-name → source-name fallback shadow
    // registration. Generic source names are now resolved through the
    // dispatch trampoline path (see `nod_runtime::make_function_ref`),
    // so the function-ref registry never needs a generic alias entry.
    for f in &lm.functions {
        if block_thunk_names.contains(&f.name) {
            continue;
        }
        // Skip the synthetic `<eval-entry>` so it isn't reachable via
        // `\<eval-entry>` from inside the evaluated body.
        if f.name == "<eval-entry>" {
            continue;
        }
        // Sprint 24: closure body's JIT signature carries a hidden env
        // parameter on top of the user arity. Register under the
        // *source* arity so `\name` and `%make-closure(name, arity, env)`
        // both resolve. The trampoline (`nod_funcall_N`) reads the
        // env-ptr slot on the `<function>` Word to decide whether to
        // pass env as a hidden first arg or not, so it doesn't need a
        // separate registration arity.
        let arity = if let Some(info) = lm.closures.closure_for(&f.name) {
            info.arity
        } else {
            f.params.len()
        };
        // SAFETY: get_function_ptr returns a valid JIT'd address;
        // the JIT engine outlives the registration (callers leak it).
        let ptr = unsafe { jit.get_function_ptr(&f.name) }.ok_or_else(|| {
            EvalError::NoEntry(format!("top-level function `{}` not JIT'd", f.name))
        })?;
        // SAFETY: ptr is JIT-emitted, signature `(u64*arity) -> u64`.
        unsafe {
            nod_runtime::register_jit_function(&f.name, arity, ptr as *const u8);
        }
    }
    Ok(())
}

pub fn register_methods(
    jit: &Jit<'_>,
    methods: &[MethodRegistration],
) -> Result<(), EvalError> {
    for m in methods {
        // SAFETY: the JIT engine outlives the registration; the body
        // function's `(u64, ..., u64) -> u64` signature is what the
        // dispatcher expects.
        let ptr = unsafe { jit.get_function_ptr(&m.body_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!(
                "method body `{}` not JIT'd",
                m.body_fn_name
            ))
        })?;
        // SAFETY: ptr is the live JIT'd function, matches `(u64, ..., u64) -> u64`.
        // Sprint 16: pass the JIT symbol name so the Sprint 15 dispatch
        // resolver can emit a `DirectCall` against the exact emitted
        // symbol — slot accessors (`<C>-getter-x`) don't follow the
        // `{generic}${specialisers}` convention `add_method_full`
        // assumes.
        unsafe {
            nod_runtime::add_method_named(
                &m.generic_name,
                m.specialisers.clone(),
                ptr as *const u8,
                m.param_count,
                &m.body_fn_name,
            );
        }
    }
    Ok(())
}

/// Sprint 19: resolve every `block` form's lifted thunks to JIT
/// addresses and register them with the runtime. Runs once after the
/// JIT finalises a module.
pub fn register_blocks(
    jit: &Jit<'_>,
    blocks: &[crate::lower::BlockRegistration],
) -> Result<(), EvalError> {
    for b in blocks {
        // SAFETY: JIT engine outlives the registration. The thunk
        // signatures match `extern "C-unwind" fn(u64, ..., u64) -> u64`.
        let body = unsafe { jit.get_function_ptr(&b.body_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!("block body `{}` not JIT'd", b.body_fn_name))
        })?;
        let cleanup = match &b.cleanup_fn_name {
            Some(n) => Some(
                unsafe { jit.get_function_ptr(n) }
                    .ok_or_else(|| EvalError::NoEntry(format!("block cleanup `{n}` not JIT'd")))?
                    as *const u8,
            ),
            None => None,
        };
        let afterwards = match &b.afterwards_fn_name {
            Some(n) => Some(
                unsafe { jit.get_function_ptr(n) }
                    .ok_or_else(|| EvalError::NoEntry(format!("block afterwards `{n}` not JIT'd")))?
                    as *const u8,
            ),
            None => None,
        };
        let handlers: Vec<nod_runtime::HandlerFn> = b
            .handlers
            .iter()
            .map(|h| {
                let p = unsafe { jit.get_function_ptr(&h.body_fn_name) }.ok_or_else(|| {
                    EvalError::NoEntry(format!("block handler `{}` not JIT'd", h.body_fn_name))
                })?;
                // Pin the class name as a static byte slice. Leaking is
                // intentional — these names live for the process.
                let pinned: &'static str = Box::leak(h.class_name.clone().into_boxed_str());
                Ok(nod_runtime::HandlerFn {
                    class_id: h.class_id,
                    class_name_ptr: pinned.as_ptr(),
                    class_name_len: pinned.len(),
                    body: p as *const u8,
                })
            })
            .collect::<Result<_, EvalError>>()?;
        let handlers_static: &'static [nod_runtime::HandlerFn] = Box::leak(handlers.into_boxed_slice());
        nod_runtime::register_block_fns(
            b.block_id,
            nod_runtime::BlockFns {
                body: body as *const u8,
                cleanup,
                afterwards,
                handlers: handlers_static,
            },
        );
    }
    Ok(())
}

/// GAP-004 — for each `define variable` in `lm`, look up its
/// `__init-<name>` thunk in the JIT, call it to get the initial Word,
/// allocate a fresh `<cell>` holding the Word, and store the cell
/// pointer in the variable's process-global slot.
///
/// Runs AFTER `register_methods` / `register_blocks` /
/// `register_top_level_functions` because init expressions may call
/// any of those. Mirrors the AOT resolver's late-stage variable pass.
pub fn register_variables(
    jit: &Jit<'_>,
    variables: &[crate::lower::VariableRegistration],
) -> Result<(), EvalError> {
    for v in variables {
        // SAFETY: JIT engine outlives the registration. The init thunk
        // is a zero-arg Dylan function returning a Word — codegen
        // emitted it with that exact signature (see the
        // `Item::DefineVariable` arm of `lower_module_full`).
        let ptr = unsafe { jit.get_function_ptr(&v.init_fn_name) }.ok_or_else(|| {
            EvalError::NoEntry(format!("variable init `{}` not JIT'd", v.init_fn_name))
        })?;
        // SAFETY: ptr matches `extern "C-unwind" fn() -> u64`. The
        // runtime shim transmutes via the same signature.
        unsafe {
            nod_runtime::nod_aot_register_variable(
                v.name.as_ptr(),
                v.name.len(),
                ptr as *const u8,
            );
        }
    }
    Ok(())
}

fn call_and_format(ptr: *const (), ty: TypeEstimate) -> String {
    // SAFETY: each branch transmutes to the function signature implied
    // by the temp/return type the lowering pass produced. The JIT
    // memory backing `ptr` is kept alive by the caller's `Jit`.
    //
    // Sprint 10 ABI: `<integer>`, `<boolean>`, `<string>`, and Top/Bottom
    // returns are all a tagged `Word` packed into an `i64`.
    match ty {
        // Sprint 15: a `Class(_)` / `Singleton(_)` return value is still
        // a tagged `Word` packed into an `i64` (same ABI as `Top`); the
        // formatter walks the wrapper to surface the class name.
        TypeEstimate::Class(_) | TypeEstimate::Singleton(_) => {
            // SAFETY: ptr has signature `() -> u64`.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
            let w = nod_runtime::Word::from_raw(f());
            format_pointer_word(w)
        }
        TypeEstimate::Integer | TypeEstimate::Top | TypeEstimate::Bottom => {
            // SAFETY: ptr has signature `() -> u64`.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
            let w = nod_runtime::Word::from_raw(f());
            match w.as_fixnum() {
                Some(n) => n.to_string(),
                // Pointer-tagged return — surface the class.
                None => format_pointer_word(w),
            }
        }
        TypeEstimate::Boolean => {
            // SAFETY: ptr has signature `() -> u64`.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
            let raw = f();
            let imm = nod_runtime::literal_pool_immediates();
            if raw == imm.false_.raw() {
                "#f".to_string()
            } else {
                "#t".to_string()
            }
        }
        TypeEstimate::SingleFloat => {
            // SAFETY: ptr has signature `() -> f32`.
            let f: extern "C" fn() -> f32 = unsafe { std::mem::transmute(ptr) };
            format!("{}", f() as f64)
        }
        TypeEstimate::DoubleFloat => {
            // SAFETY: ptr has signature `() -> f64`.
            let f: extern "C" fn() -> f64 = unsafe { std::mem::transmute(ptr) };
            format!("{}", f())
        }
        TypeEstimate::Character => {
            // SAFETY: ptr has signature `() -> u32`.
            let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(ptr) };
            match char::from_u32(f()) {
                Some(c) => format!("'{c}'"),
                None => "<bad-char>".to_string(),
            }
        }
        TypeEstimate::Unit => {
            // SAFETY: ptr has signature `()`.
            let f: extern "C" fn() = unsafe { std::mem::transmute(ptr) };
            f();
            "#unit".to_string()
        }
        TypeEstimate::String => {
            // SAFETY: ptr has signature `() -> u64`.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(ptr) };
            let w = nod_runtime::Word::from_raw(f());
            format_pointer_word(w)
        }
    }
}

/// Render a pointer-tagged Word by reading its wrapper class. Sprint
/// 10 special-cases `<byte-string>` (print the contents) and the
/// pinned immediates; everything else prints as the class name.
fn format_pointer_word(w: nod_runtime::Word) -> String {
    if !w.is_pointer() {
        return format!("<non-pointer-word:{:#x}>", w.raw());
    }
    let imm = nod_runtime::literal_pool_immediates();
    if w == imm.true_ {
        return "#t".to_string();
    }
    if w == imm.false_ {
        return "#f".to_string();
    }
    if w == imm.nil {
        return "#()".to_string();
    }
    // SAFETY: every pointer-tagged Dylan Word in Sprint 10 either
    // points into the heap or into the pinned immediates region. The
    // wrapper-first invariant lets us read the wrapper directly.
    let Some(wrap) = (unsafe { nod_runtime::wrapper_of_unchecked(w) }) else {
        return format!("<bad-word:{:#x}>", w.raw());
    };
    if wrap.class() == nod_runtime::ClassId::BYTE_STRING {
        // SAFETY: class match implies <byte-string> layout.
        if let Some(bs) =
            unsafe { nod_runtime::try_byte_string(w, nod_runtime::ClassId::BYTE_STRING) }
        {
            // SAFETY: bs points at live allocation.
            return match unsafe { bs.as_str() } {
                Some(s) => format!("{s:?}"),
                None => format!("<non-utf8 byte-string len={}>", bs.len),
            };
        }
    }
    // Sprint 21: `<simple-object-vector>` prints as `#(elt0, elt1, …)`
    // matching Dylan's source-literal form. Used by the
    // `dylan_map_squares_three_element_list` headline test, which
    // produces an SOV via `map(...)`.
    if wrap.class() == nod_runtime::ClassId::SIMPLE_OBJECT_VECTOR {
        // SAFETY: class match implies SOV layout.
        if let Some(sov) = unsafe {
            nod_runtime::try_simple_object_vector(w, nod_runtime::ClassId::SIMPLE_OBJECT_VECTOR)
        } {
            // SAFETY: sov points at live allocation.
            let slots = unsafe { sov.slots() };
            let parts: Vec<String> = slots.iter().map(|s| format_element(*s)).collect();
            return format!("#({})", parts.join(", "));
        }
    }
    // Sprint 21: `<pair>` / `<empty-list>` cons-cell list pretty-print.
    if wrap.class() == nod_runtime::ClassId::PAIR
        || wrap.class() == nod_runtime::ClassId::EMPTY_LIST
    {
        return format_list(w);
    }
    format!("<{:?} @ {:#x}>", wrap.class(), w.raw() & !1)
}

/// Helper: render a single Word as it appears INSIDE a collection
/// literal. Fixnums print as their decimal value; pointer-tagged
/// values recurse through `format_pointer_word`.
fn format_element(w: nod_runtime::Word) -> String {
    if let Some(n) = w.as_fixnum() {
        return n.to_string();
    }
    format_pointer_word(w)
}

/// Render a `<pair>` / `<empty-list>` chain as `#(elt0, elt1, …)`.
fn format_list(w: nod_runtime::Word) -> String {
    let imm = nod_runtime::literal_pool_immediates();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = w;
    while cur != imm.nil {
        // SAFETY: walking a Sprint 16 cons-cell chain; `try_pair` checks
        // the wrapper class and returns `None` if `cur` isn't a pair.
        let Some(p) = (unsafe { nod_runtime::try_pair(cur, nod_runtime::ClassId::PAIR) })
        else {
            break;
        };
        parts.push(format_element(p.head));
        cur = p.tail;
    }
    format!("#({})", parts.join(", "))
}

#[derive(Debug)]
pub enum DumpError {
    Io(std::io::Error),
    SourceMap(nod_reader::SourceMapError),
    Parse(Vec<nod_reader::Diagnostic>),
    Macro(Vec<nod_macro::MacroError>),
    Lower(Vec<LoweringError>),
    Codegen(nod_llvm::CodegenError),
    /// Sprint 54c — `--sema-with-dylan`: the Dylan sema provider failed, or
    /// its model dump couldn't be reconstructed into a `SemaModel`.
    SemaDylan(String),
    /// Sprint 55 — `--lower-with-dylan`: the Dylan lowering provider failed
    /// (the DFM-dump reconstruction itself surfaces as `Lower`).
    LowerDylan(String),
}

impl std::fmt::Display for DumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DumpError::Io(e) => write!(f, "io: {e}"),
            DumpError::SourceMap(e) => write!(f, "source map: {e}"),
            DumpError::Parse(d) => write!(f, "parse: {} diagnostic(s)", d.len()),
            DumpError::Macro(errs) => {
                write!(f, "macro: {} error(s):", errs.len())?;
                for e in errs {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            DumpError::Lower(errs) => {
                write!(f, "lower: {} error(s):", errs.len())?;
                for e in errs {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            DumpError::Codegen(e) => write!(f, "codegen: {e}"),
            DumpError::SemaDylan(msg) => write!(f, "sema-with-dylan: {msg}"),
            DumpError::LowerDylan(msg) => write!(f, "lower-with-dylan: {msg}"),
        }
    }
}

impl std::error::Error for DumpError {}

#[derive(Debug)]
pub enum EvalError {
    Io(std::io::Error),
    SourceMap(nod_reader::SourceMapError),
    Parse(Vec<nod_reader::Diagnostic>),
    Macro(Vec<nod_macro::MacroError>),
    Lower(Vec<LoweringError>),
    Codegen(nod_llvm::CodegenError),
    Jit(nod_llvm::JitError),
    NoEntry(String),
    ReturnTypeMismatch {
        entry: String,
        expected: &'static str,
        actual: &'static str,
    },
    /// Sprint 28: the per-module API stub table failed to populate
    /// (LoadLibrary / GetProcAddress returned null). Carries the class
    /// name of the `<c-ffi-error>` (`"<c-ffi-error>"`) and the offending
    /// (dll, symbol) pair so tests can pattern-match without parsing the
    /// rendered message.
    WinFfiInit {
        class_name: String,
        dll: String,
        symbol: String,
    },
    /// Sprint 44: the multi-file `compile_files_for_aot` was given files
    /// that declared incompatible `Module:` headers. Carries the list of
    /// (file path, declared module name) pairs so the driver can surface
    /// a helpful "all source files for one build must share the same
    /// Module: header" diagnostic. An empty string in the second slot
    /// means the file had no `Module:` header at all.
    ModuleMismatch {
        files: Vec<(std::path::PathBuf, String)>,
    },
    /// Sprint 44: two user source files (NOT user-vs-stdlib) declared a
    /// top-level definition with the same `body_fn_name`. Stdlib-side
    /// collisions still silently let the user win (see `merge_modules`),
    /// but cross-user-file collisions are an error because the user's
    /// intent is ambiguous. Carries the duplicated symbol and the two
    /// source paths that produced it.
    DuplicateUserDefinition {
        name: String,
        first_path: std::path::PathBuf,
        second_path: std::path::PathBuf,
    },
    /// Sprint 56a-CONSUME — under `--frontend-with-dylan`, the AOT path routes
    /// through the Dylan front-end seam (`lower_with_sema_choice`); the Dylan
    /// sema/lower provider or the dump reconstruction failed. Carries the
    /// rendered cause.
    FrontendWithDylan(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::Io(e) => write!(f, "io: {e}"),
            EvalError::SourceMap(e) => write!(f, "source map: {e}"),
            EvalError::Parse(d) => write!(f, "parse: {} diagnostic(s)", d.len()),
            EvalError::Macro(errs) => {
                write!(f, "macro: {} error(s):", errs.len())?;
                for e in errs {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            EvalError::Lower(errs) => {
                write!(f, "lower: {} error(s):", errs.len())?;
                for e in errs {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            EvalError::Codegen(e) => write!(f, "codegen: {e}"),
            EvalError::Jit(e) => write!(f, "jit: {e}"),
            EvalError::NoEntry(n) => write!(f, "entry function not found: `{n}`"),
            EvalError::ReturnTypeMismatch { entry, expected, actual } => write!(
                f,
                "entry `{entry}` returns {actual}, expected {expected}"
            ),
            EvalError::WinFfiInit { class_name, dll, symbol } => write!(
                f,
                "winffi init failed: {class_name} raised for `{symbol}@{dll}`"
            ),
            EvalError::ModuleMismatch { files } => {
                write!(
                    f,
                    "module-header mismatch: all source files for one build must share the same `Module:` header\n"
                )?;
                for (p, m) in files {
                    let m_disp = if m.is_empty() { "(no Module: header)" } else { m.as_str() };
                    write!(f, "  {} → {m_disp}\n", p.display())?;
                }
                Ok(())
            }
            EvalError::DuplicateUserDefinition { name, first_path, second_path } => write!(
                f,
                "duplicate top-level definition `{name}` in both\n  {}\n  {}\n\
                 (user-vs-user collisions are not allowed; stdlib overrides are still permitted)",
                first_path.display(),
                second_path.display(),
            ),
            EvalError::FrontendWithDylan(msg) => write!(f, "frontend-with-dylan: {msg}"),
        }
    }
}

impl std::error::Error for EvalError {}
