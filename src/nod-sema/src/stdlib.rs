//! Sprint 20b — Dylan stdlib auto-loader.
//!
//! The first call to any public `nod-sema` entry point
//! (`eval_expr_to_string`, `lower_module`, `lower_module_full`,
//! `run_function_to_i64`, the various `dump_*_for_file` helpers)
//! routes through [`ensure_loaded`]. The loader parses
//! `src/nod-dylan/dylan-sources/stdlib.dylan` once, runs the
//! macro engine over it, lowers it to DFM, and JIT-compiles every
//! function. Side effects on `nod_runtime`'s process-global
//! registries (macro table, dispatch table) make the stdlib's
//! definitions visible to subsequent user-code lowering / JITting
//! within the same process.
//!
//! ## How user code reaches stdlib symbols
//!
//! Sprint 20b doesn't yet link separate JIT modules together via
//! shared symbol resolution — the stdlib's JIT engine resides
//! behind a `OnceLock`, but user-code `Jit` instances created in
//! `eval_expr_to_string` etc. are independent. To make
//! `size(c)` in user code resolve, the loader rewrites every
//! `define function` in `stdlib.dylan` to `define method <name>
//! (param :: <object>, …)` BEFORE lowering. This registers each
//! function as a single-method generic against the most-general
//! specialisers. Generic dispatch lives in `nod_runtime` and is
//! process-global, so user code's `Dispatch` IR node (emitted by
//! `nod-sema/src/lower.rs` line ~2138 for known generic names)
//! finds the stdlib method through the same path it uses for
//! user-defined generics.
//!
//! Macros from `stdlib.dylan` populate a process-global macro
//! registry (`stdlib_macros`) which `expand_and_lower_module`
//! merges into the per-call `MacroTable` before expansion.
//!
//! ## Lifetime story
//!
//! The stdlib `Context` is leaked (`Box::leak`) so it lives for
//! the process. The stdlib `Jit` is moved into a static `OnceLock`
//! and never dropped — same pattern Sprint 13/19 used for runtime
//! helpers. Method body pointers registered with
//! `nod_runtime::add_method_named` reference the leaked JIT's
//! memory, so dispatch finds them forever.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use inkwell::context::Context;
use nod_llvm::{Jit, codegen_module};
use nod_macro::MacroTable;
use nod_reader::{Expr, Item, Module, Param, ReturnSig, Span};

use crate::lower::{LoweredModule, MethodRegistration, lower_module_full};
use crate::{register_blocks, register_methods, register_top_level_functions};

/// Static-area for the stdlib LLVM context + JIT. Leaking the
/// engine is deliberate — Sprint 20b doesn't reclaim it, and the
/// addresses registered with `nod_runtime` must outlive every user
/// JIT.
static STDLIB_ARTEFACTS: OnceLock<&'static StdlibArtefacts> = OnceLock::new();

/// Sprint 39c — the stdlib's lowered DFM module, stashed so the AOT
/// pipeline (`compile_file_for_aot`) can merge stdlib functions /
/// methods / blocks / closures into the user's `LoweredModule` before
/// codegen. The JIT path doesn't consult this — it already JITs the
/// stdlib into its own engine and registers the resulting pointers in
/// the process-global dispatch / function tables.
///
/// Held as a `&'static LoweredModule` so per-EXE clones are cheap-ish
/// (the methods / blocks vectors are `Clone`); the AOT pipeline
/// deep-clones into the user's `LoweredModule` to keep the JIT path's
/// view untouched.
static STDLIB_LOWERED: OnceLock<&'static LoweredModule> = OnceLock::new();

/// What the loader hands back. Mostly informational — the
/// dispatch-table / macro-registry side effects are the real
/// payload.
#[derive(Debug)]
pub struct StdlibArtefacts {
    /// Names of every function lowered from `stdlib.dylan`.
    pub function_names: Vec<String>,
    /// Method registrations the loader installed (post-rewrite of
    /// `define function` → `define method ... <object>`).
    pub method_registrations: Vec<MethodRegistration>,
    /// Names of every macro registered (so user-code expansion can
    /// find them via the process-global table merge).
    pub macro_names: Vec<String>,
}

/// Process-global macro table populated from `stdlib.dylan`. Read
/// (without modification) by `expand_and_lower_module` and merged
/// on top of each call's local macro table so user code can use
/// `for-each`, etc.
static STDLIB_MACROS: OnceLock<MacroTable> = OnceLock::new();

/// Sprint 29: process-global table of integer constants curated
/// in `data/win32_constants.txt` and surfaced via
/// `win32-constants.dylan`. User-code lowering consults this map
/// before falling through to the function-ref resolution path, so
/// `$MB-OK` in source code becomes `ConstValue::Integer(0)` at
/// lowering time. The map is populated once by `load_stdlib`; the
/// stdlib JIT engine never sees these constants as functions.
static STDLIB_CONSTANTS: OnceLock<HashMap<String, i128>> = OnceLock::new();

/// Ordered list of stdlib source files the loader processes. Each entry
/// lives under `src/nod-dylan/dylan-sources/stdlib/`. The first file owns
/// the macros (so `for-each` etc. remain reachable via
/// [`stdlib_macro_source`]); subsequent files contribute their items to the
/// merged module, in this order. Add new entries here when introducing a
/// new stdlib facet.
const STDLIB_FILES: &[(&str, &str)] = &[
    (
        "stdlib/macros.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/macros.dylan"),
    ),
    (
        "stdlib/collections.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/collections.dylan"),
    ),
    (
        "stdlib/arrays.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/arrays.dylan"),
    ),
    (
        "stdlib/system-classes.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/system-classes.dylan"),
    ),
    (
        "stdlib/strings.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/strings.dylan"),
    ),
    (
        "stdlib/sequences.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/sequences.dylan"),
    ),
    (
        "stdlib/lists.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/lists.dylan"),
    ),
    (
        "stdlib/functional.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/functional.dylan"),
    ),
    (
        "stdlib/numbers.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/numbers.dylan"),
    ),
    (
        "stdlib/ffi-callbacks.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/ffi-callbacks.dylan"),
    ),
    (
        "stdlib/structs.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/structs.dylan"),
    ),
    (
        "stdlib/streams.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/streams.dylan"),
    ),
    (
        "stdlib/win32-constants.dylan",
        include_str!("../../nod-dylan/dylan-sources/stdlib/win32-constants.dylan"),
    ),
];

/// The stdlib's macro source, embedded at build time. The Dylan-side macro
/// expander needs it to collect the stdlib's `define macro`s (so user code
/// using `unless`/`when`/`cond`/`for-each` expands Dylan-side). This is
/// `STDLIB_FILES[0]` — `stdlib/macros.dylan`, which owns every stdlib macro.
/// See the wire-format notes in `docs/compiler/self-hosting.md`.
pub fn stdlib_macro_source() -> &'static str {
    STDLIB_FILES[0].1
}

/// The set of macro names the stdlib defines, derived by parsing the stdlib
/// macro source and collecting its `define macro` forms WITHOUT a full stdlib
/// load (no JIT, no class registration). The standalone `dump-ast` path uses
/// this to seed the parser's known-macro set so it recognises body-shaped
/// stdlib macros (`when`, `with-cleanup`, `repeat`, …) the same way the real
/// sema pipeline does — and picks up new stdlib macros automatically. Returns
/// an empty vec on any parse/collect error (the caller keeps a static fallback).
pub fn stdlib_macro_names() -> Vec<String> {
    let src = stdlib_macro_source();
    let mut sm = nod_reader::SourceMap::new();
    let id = match sm.add("<stdlib-macros>".to_string(), src.to_string()) {
        Ok(id) => id,
        Err(_) => return Vec::new(),
    };
    let toks = nod_reader::lex(src, id);
    let pre = nod_reader::scan_preamble(src);
    let module = match nod_reader::parse_module_with_macros_rust(
        src,
        &toks,
        pre.as_ref(),
        &std::collections::HashSet::new(),
    ) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut table = MacroTable::default();
    if nod_macro::collect_macros(&module, &sm, &mut table).is_err() {
        return Vec::new();
    }
    table.defs.keys().cloned().collect()
}

/// Sprint 20b: macro entries the loader collected. User-side
/// expansion merges these into the per-call `MacroTable`.
pub(crate) fn stdlib_macros() -> &'static MacroTable {
    STDLIB_MACROS.get().expect(
        "stdlib_macros() called before ensure_loaded(); \
         the lib.rs entry points call ensure_loaded() first.",
    )
}

/// Sprint 29: integer-constant lookup. Returns `Some(value)` when
/// `name` is one of the stdlib's curated Win32 constants (or any
/// future stdlib `define constant`); `None` otherwise. The result
/// is `i128` to match the lowering layer's `Expr::Integer` width
/// (and so values like `#xFFFFFFFF` round-trip cleanly without
/// premature i64 truncation), but every currently-curated value
/// fits in i64.
///
/// User-code lowering (`Expr::Ident` resolution) calls this BEFORE
/// the function-ref fallback so that a bare `$MB-OK` becomes the
/// integer literal `0` and not a `<function>` Word. The stdlib
/// loader must have run for the table to be present; callers
/// outside `nod-sema` go through `eval_expr_to_string` /
/// `lower_module` which call `ensure_loaded` already.
pub fn lookup_constant(name: &str) -> Option<i128> {
    STDLIB_CONSTANTS.get()?.get(name).copied()
}

/// Sprint 29: read-only access to the constants map (for tests
/// and diagnostics). The first call after `ensure_loaded` is
/// guaranteed to find an initialised table.
pub fn constants_table() -> Option<&'static HashMap<String, i128>> {
    STDLIB_CONSTANTS.get()
}

/// Sprint 39c — the stdlib's fully-lowered DFM module. Returns `None`
/// before [`ensure_loaded`] has populated it; the AOT driver calls
/// `ensure_loaded` ahead of `compile_file_for_aot`, so by the time
/// this getter fires the value is guaranteed to be present.
///
/// Used by [`crate::compile_file_for_aot`] to merge stdlib functions
/// / methods / blocks / closures into the user's `LoweredModule`
/// before codegen, so the resulting EXE's `.obj` carries every body
/// the dispatch / function-ref / block tables need at runtime.
pub fn stdlib_lowered() -> Option<&'static LoweredModule> {
    STDLIB_LOWERED.get().copied()
}

/// Top-level entry: parse + expand + lower + JIT `stdlib.dylan`
/// exactly once. Idempotent; subsequent calls return the cached
/// artefacts. Errors during the first call are panicked on — the
/// stdlib is a compile-time-bundled source, so failure indicates
/// an internal-inconsistency bug, not a user error.
pub fn ensure_loaded() -> &'static StdlibArtefacts {
    if let Some(a) = STDLIB_ARTEFACTS.get() {
        return a;
    }
    // `load_stdlib()` has process-global SIDE EFFECTS — it registers the
    // stdlib's classes (`<stream>`, `<string-stream>`, …) into the shared
    // class registry. The artefact `OnceLock` only guards the *result*; it
    // does NOT stop two threads from both taking the slow path and both
    // running the side-effecting load, in which case the loser panics with
    // `ClassRedefinitionNotSupported`. (The test harness runs eval tests in
    // parallel, which is exactly when this raced — codegen/gc/heap_objects/
    // runtime all flaked on it; serialised, they pass.) Serialise the load
    // behind a dedicated gate with double-checked locking so the
    // registration runs at most once per process.
    //
    // We deliberately keep the artefact `OnceLock` free of `get_or_init`:
    // `load_stdlib` lowers the stdlib through nod-sema helpers that read the
    // other stdlib `OnceLock`s, and a `get_or_init` init closure re-entering
    // the same lock would panic. A separate gate Mutex sidesteps that — and
    // since the single-threaded path already loads exactly once (no
    // re-entrant double-registration, or it would fail serially too), the
    // gate cannot deadlock.
    static LOAD_GATE: Mutex<()> = Mutex::new(());
    let _gate = LOAD_GATE.lock().expect("stdlib load gate poisoned");
    // Another thread may have finished the load while we waited on the gate.
    if let Some(a) = STDLIB_ARTEFACTS.get() {
        return a;
    }
    let artefacts = load_stdlib().expect("stdlib.dylan failed to load — internal bug");
    let leaked: &'static StdlibArtefacts = Box::leak(Box::new(artefacts));
    let _ = STDLIB_ARTEFACTS.set(leaked);
    STDLIB_ARTEFACTS
        .get()
        .copied()
        .expect("STDLIB_ARTEFACTS was just set")
}

#[derive(Debug)]
enum LoadError {
    Parse(Vec<nod_reader::Diagnostic>),
    Macro(Vec<nod_macro::MacroError>),
    Lower(Vec<crate::lower::LoweringError>),
    Codegen(nod_llvm::CodegenError),
    Jit(nod_llvm::JitError),
    NoEntry(String),
    SourceMap(nod_reader::SourceMapError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Parse(d) => write!(f, "stdlib parse: {} diagnostic(s)", d.len()),
            LoadError::Macro(es) => {
                write!(f, "stdlib macro: {} error(s)", es.len())?;
                for e in es {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            LoadError::Lower(es) => {
                write!(f, "stdlib lower: {} error(s)", es.len())?;
                for e in es {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            LoadError::Codegen(e) => write!(f, "stdlib codegen: {e}"),
            LoadError::Jit(e) => write!(f, "stdlib jit: {e}"),
            LoadError::NoEntry(n) => write!(f, "stdlib: function `{n}` missing post-JIT"),
            LoadError::SourceMap(e) => write!(f, "stdlib source map: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

fn load_stdlib() -> Result<StdlibArtefacts, LoadError> {
    // Sprint 20b: register the seed collection + condition classes
    // up-front. The stdlib references them by name (`<collection>`,
    // `<error>`, …); if they aren't registered yet `find_class_id_by_name`
    // misses and lowering fails. `ensure_*_registered` is idempotent.
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_collections_registered();
    nod_runtime::ensure_tables_registered();
    // Sprint 34: register `<c-struct>` and the seed struct classes
    // (`<point>`, `<rect>`, …) before the stdlib parses its field
    // accessors. Idempotent.
    nod_runtime::ensure_structs_registered();

    // Parse every file in `STDLIB_FILES` and merge their items into a
    // single module, in file order. Each file carries its own
    // `Module: dylan` preamble; the first file's is the module's, and
    // subsequent preambles are accepted but discarded (only items merge).
    let mut sm = nod_reader::SourceMap::new();
    let mut merged: Option<Module> = None;
    for (label, src) in STDLIB_FILES {
        let file_id = sm
            .add(format!("<stdlib:{label}>"), (*src).to_string())
            .map_err(LoadError::SourceMap)?;
        let toks = nod_reader::lex(src, file_id);
        let pre = nod_reader::scan_preamble(src);
        // Sprint 51e.5 — the stdlib is compiler infrastructure that MUST
        // parse correctly in every process; it is NOT a candidate for the
        // experimental Dylan parser. Call the canonical Rust parser
        // DIRECTLY (`…_rust`), bypassing the `parse_module_with_macros`
        // dispatcher so an installed `--parse-with-dylan` parse-override
        // never routes the stdlib through the partial Dylan parser (which
        // currently signals a Dylan condition — e.g. "expected ) after
        // arguments" — on some stdlib constructs, crashing the build).
        // The Dylan parser feeds the USER pipeline (`parse_user_module`),
        // not the stdlib load. `lex` is left as the dispatcher: the Dylan
        // lexer is byte-identical and robust on the stdlib.
        let mut parsed = nod_reader::parse_module_with_macros_rust(
            src,
            &toks,
            pre.as_ref(),
            &std::collections::HashSet::new(),
        )
        .map_err(LoadError::Parse)?;
        match &mut merged {
            None => merged = Some(parsed),
            Some(m) => m.items.append(&mut parsed.items),
        }
    }
    let mut module =
        merged.expect("STDLIB_FILES must be non-empty so the merged module exists");

    // Collect macros from the merged module INTO the process-global
    // table. Don't expand stdlib's own macro uses here — Sprint 20b's
    // stdlib doesn't call its own macros internally, so the table
    // population is the only side effect we need.
    let mut macro_table = MacroTable::default();
    nod_macro::collect_macros(&module, &sm, &mut macro_table).map_err(LoadError::Macro)?;
    // Drop the macro definitions from the module so lowering doesn't
    // see them again. Mirrors `expand_module`'s cleanup step.
    module.items.retain(|it| !matches!(it, Item::DefineMacro { .. }));
    let macro_names: Vec<String> = macro_table.defs.keys().cloned().collect();
    let _ = STDLIB_MACROS.set(macro_table);

    // Sprint 29: harvest integer constants (`define constant N = <int>`)
    // into the process-global constants table and STRIP them from the
    // module before lowering. Otherwise each would become a 0-arg
    // stdlib function — unreachable from user code's separate JIT
    // engine, and wasteful. User-code lowering will see these names
    // through `lookup_constant`.
    let mut constants_map: HashMap<String, i128> = HashMap::new();
    module.items.retain(|it| match it {
        Item::DefineConstant {
            name,
            value: Expr::Integer(_, n),
            ..
        } => {
            // Last-writer-wins on duplicate names — but
            // `data/win32_constants.txt` rejects mismatched values at
            // build time, so duplicates here should agree.
            constants_map.insert(name.clone(), *n);
            false
        }
        _ => true,
    });
    let _ = STDLIB_CONSTANTS.set(constants_map);

    // Rewrite every `define function` in stdlib into a `define method
    // f (p1 :: <object>, p2 :: <object>, …)` so user code's
    // `Dispatch` IR resolves to it via the process-global dispatch
    // table. This is the cheapest way to make stdlib symbols
    // callable from a separate JIT engine without wiring
    // cross-module symbol resolution (deferred to Sprint 21).
    rewrite_define_function_to_method(&mut module);

    let lm = lower_module_full(&module).map_err(LoadError::Lower)?;

    // Codegen + JIT — leak the Context so engine pointers stay live
    // for the process. The Jit value itself is moved into the leaked
    // artefacts box so engines persist.
    let ctx_box: &'static Context = Box::leak(Box::new(Context::create()));
    let out = codegen_module(ctx_box, &lm.functions, "__nod_stdlib__").map_err(LoadError::Codegen)?;
    let mut jit = Jit::new(ctx_box).map_err(LoadError::Jit)?;
    jit.add_module(out).map_err(LoadError::Jit)?;

    // Wire methods + blocks into the process-global registries.
    register_methods(&jit, &lm.methods).map_err(|e| match e {
        crate::EvalError::NoEntry(n) => LoadError::NoEntry(n),
        crate::EvalError::Jit(e) => LoadError::Jit(e),
        other => LoadError::NoEntry(format!("stdlib method registration: {other}")),
    })?;
    register_blocks(&jit, &lm.blocks).map_err(|e| match e {
        crate::EvalError::NoEntry(n) => LoadError::NoEntry(n),
        crate::EvalError::Jit(e) => LoadError::Jit(e),
        other => LoadError::NoEntry(format!("stdlib block registration: {other}")),
    })?;
    // Sprint 21: register every stdlib `define function` body in the
    // process-global function-ref registry so `\size`, `\reduce`, etc.
    // are reachable as first-class function values from user code.
    register_top_level_functions(&jit, &lm).map_err(|e| match e {
        crate::EvalError::NoEntry(n) => LoadError::NoEntry(n),
        crate::EvalError::Jit(e) => LoadError::Jit(e),
        other => LoadError::NoEntry(format!("stdlib top-level fn registration: {other}")),
    })?;

    // Leak the Jit so engine + emitted code live forever. The
    // Box::leak yields a `&'static mut Jit<'static>`; we drop the
    // reference because we only need the side effects.
    let _: &'static mut Jit<'static> = Box::leak(Box::new(jit));

    let function_names = lm.functions.iter().map(|f| f.name.clone()).collect();
    let method_registrations = lm.methods.clone();

    // Sprint 39c — stash the lowered stdlib so the AOT pipeline can
    // merge its functions / methods / blocks / closures into the
    // user's `LoweredModule` before codegen. Doing this once here
    // (rather than re-parsing + re-lowering on every AOT build)
    // matches the JIT path's amortisation. Box::leak gives a
    // `&'static LoweredModule`; the value is small (~kBs of vectors)
    // so the leak is negligible.
    let leaked_lm: &'static LoweredModule = Box::leak(Box::new(lm));
    let _ = STDLIB_LOWERED.set(leaked_lm);

    Ok(StdlibArtefacts {
        function_names,
        method_registrations,
        macro_names,
    })
}

/// Pre-lowering rewrite: every `Item::DefineFunction { name, params,
/// body, … }` becomes `Item::DefineMethod { name, params (typed as
/// <object>), body, … }`. This makes the stdlib's functions
/// dispatchable as single-method generics on the maximally-general
/// specialisers, so user code's `Dispatch` IR resolves to them.
///
/// Sprint 20b: a small, surgical transform. The brief authorises
/// stdlib-side judgment calls; cross-module symbol linkage is the
/// principled fix and is deferred to Sprint 21 (see DEFERRED.md).
fn rewrite_define_function_to_method(module: &mut nod_reader::Module) {
    let span_dummy = Span {
        file_id: nod_reader::FileId(0),
        lo: 0,
        hi: 0,
    };
    for item in &mut module.items {
        let new = match item {
            Item::DefineFunction {
                span,
                name,
                modifiers,
                params,
                return_,
                body,
            } => {
                if params.is_empty() {
                    // 0-arg functions can't become methods (Dylan
                    // generics require at least one specialiser).
                    // Leave them as direct-call top-level functions —
                    // user code can't reach them via dispatch, which
                    // is fine for stdlib internals.
                    None
                } else {
                    // Synthesise `<object>` type annotations on every
                    // unannotated parameter. Already-annotated params
                    // stay as-is (the stdlib doesn't currently do this,
                    // but keeps the loader robust against future edits).
                    let typed_params: Vec<Param> = params
                        .iter()
                        .map(|p| {
                            let t = p.type_.clone().unwrap_or_else(|| {
                                nod_reader::Expr::Ident(span_dummy, "<object>".to_string())
                            });
                            Param {
                                span: p.span,
                                name: p.name.clone(),
                                type_: Some(t),
                            }
                        })
                        .collect();
                    Some(Item::DefineMethod {
                        span: *span,
                        name: name.clone(),
                        modifiers: modifiers.clone(),
                        params: typed_params,
                        return_: clone_return_sig(return_.as_ref()),
                        body: body.clone(),
                    })
                }
            }
            _ => None,
        };
        if let Some(replacement) = new {
            *item = replacement;
        }
    }
}

fn clone_return_sig(sig: Option<&ReturnSig>) -> Option<ReturnSig> {
    sig.cloned()
}
