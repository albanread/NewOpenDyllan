//! Sprint 39a — ahead-of-time (AOT) build helpers.
//!
//! Two responsibilities, both invoked by `nod-driver`'s `build`
//! subcommand:
//!
//! 1. [`emit_aot_entry_stubs`] — post-process a fresh codegen'd
//!    [`inkwell::module::Module`] in place: rename the user's
//!    Dylan-source `main` function to `nod_user_main` and inject a
//!    fresh `i32 @main()` C entry point that calls
//!    `@nod_aot_main_wrapper`. The JIT path never calls this; only the
//!    AOT driver does.
//!
//! 2. [`emit_object_file`] — write the post-processed module to disk
//!    as a Windows COFF (or ELF on `*nix`) `.obj` file via LLVM's
//!    `TargetMachine::write_to_file`. The output is what `link.exe`
//!    consumes alongside `nod_runtime.lib` to produce the user EXE.
//!
//! ## Why post-process instead of teaching codegen
//!
//! Sprint 39a wants the JIT path untouched. Routing through a thin
//! post-codegen step (vs threading an `aot: bool` flag through
//! `codegen_module_with_key`) keeps the JIT's hot path noise-free and
//! confines AOT-specific symbol manipulation to a single 50-line
//! function. The trade-off — re-walking the module to find `main` —
//! is negligible because module sizes are small at this sprint stage.

use std::collections::HashMap;
use std::path::Path;

use inkwell::OptimizationLevel;
use inkwell::AddressSpace;
use inkwell::DLLStorageClass;
use inkwell::module::{Linkage, Module};
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, GlobalValue};

use crate::codegen::{InstalledTextRegionKind, SafepointInstallRecord, SafepointKind};
use crate::jit::JitError;
use crate::symbols::{ModuleManifest, RelocKind};

/// Sprint 39c — registration payload the AOT path emits into the
/// codegen-injected `nod_aot_resolve_relocs` function so the EXE's
/// startup populates the dispatch / function-ref / block registries
/// with the merged-stdlib + user-defined bodies BEFORE `nod_user_main`
/// runs.
///
/// The shape mirrors the JIT-time `register_methods` /
/// `register_top_level_functions` / `register_blocks` helpers in
/// `nod-sema` but carries only the data that's still meaningful at
/// EXE-link time: names + class IDs + arities + the LLVM symbol the
/// linker will resolve to a function pointer. The driver (nod-sema)
/// fills this out from the merged `LoweredModule` and hands it to
/// [`emit_aot_object_with_registrations`].
#[derive(Default, Clone, Debug)]
pub struct AotRegistrations {
    pub methods: Vec<AotMethodRegistration>,
    pub blocks: Vec<AotBlockRegistration>,
    pub functions: Vec<AotFunctionRegistration>,
    /// Sprint 40a — user-defined classes captured from
    /// `LoweredModule::user_classes`. The EXE's resolver replays these
    /// FIRST (before methods, blocks, functions), so dispatch / method
    /// specialiser lookups against user-class IDs find live metadata
    /// by the time those later registrations run.
    pub user_classes: Vec<AotUserClassRegistration>,
    /// GAP-004 — `define variable` registrations captured from
    /// `LoweredModule::variables`. The EXE's resolver replays these
    /// LAST (after classes / methods / blocks / functions), because a
    /// variable's init expression can call any user / stdlib
    /// function or dispatch on any user class. Each entry triggers
    /// one `nod_aot_register_variable(name, init_fn_ptr)` call.
    pub variables: Vec<AotVariableRegistration>,
}

/// GAP-004 — one `define variable` to initialise at startup. The
/// codegen-emitted `__init-<name>` thunk lives in the same module as
/// the variable's getter; the resolver takes its address and hands it
/// + the variable's source name to the runtime shim, which evaluates
/// the thunk and stores the resulting cell pointer in the variable's
/// process-global slot.
#[derive(Clone, Debug)]
pub struct AotVariableRegistration {
    pub name: String,
    pub init_fn_name: String,
}

/// One method registration. Codegen emits one `nod_aot_register_method`
/// call per entry inside `nod_aot_resolve_relocs`.
#[derive(Clone, Debug)]
pub struct AotMethodRegistration {
    pub generic_name: String,
    pub specialisers: Vec<u32>,
    pub body_fn_name: String,
    pub param_count: usize,
}

/// One block registration. `cleanup_fn_name`, `afterwards_fn_name`,
/// and each handler's `body_fn_name` are LLVM symbol names already
/// present as functions in the merged module.
#[derive(Clone, Debug)]
pub struct AotBlockRegistration {
    pub block_id: u64,
    pub body_fn_name: String,
    pub cleanup_fn_name: Option<String>,
    pub afterwards_fn_name: Option<String>,
    pub handlers: Vec<AotBlockHandlerRegistration>,
}

#[derive(Clone, Debug)]
pub struct AotBlockHandlerRegistration {
    pub class_id: u32,
    pub class_name: String,
    pub body_fn_name: String,
}

/// One top-level function. The dispatcher's function-ref registry is
/// keyed on `(name, arity)`, so a single entry suffices regardless of
/// whether the function is a plain `define function`, a closure body
/// (arity is the *source* arity here), or a method-table entry's
/// callable side.
#[derive(Clone, Debug)]
pub struct AotFunctionRegistration {
    pub name: String,
    pub arity: usize,
    pub body_fn_name: String,
}

/// Sprint 40a — one `define class` registration emitted into the
/// EXE's startup resolver. Mirrors `nod_runtime::UserClassSpec` but
/// flattens every field into primitives the C-ABI shim can consume:
/// strings as `(ptr, len)` pairs, `ClassId(u32)` arrays as raw `u32`
/// slices, slots as a flat `[AotSlotRegistration]`.
///
/// The codegen pass bakes each user class's name + parents + slots
/// into private LLVM globals, then emits a single
/// `nod_aot_register_user_class(...)` call in `nod_aot_resolve_relocs`
/// per entry. The runtime shim asserts the registered id matches
/// `class_id`; a mismatch would mean the AOT path's registration order
/// diverged from the JIT path's, which the shim's panic surfaces as a
/// hard codegen bug rather than a silent dispatch failure.
#[derive(Clone, Debug)]
pub struct AotUserClassRegistration {
    pub name: String,
    pub class_id: u32,
    pub parent_class_ids: Vec<u32>,
    /// Full CPL, self at index 0. The compile-time entry stores the
    /// real `class_id` here (no sentinel) so the runtime shim can pass
    /// it through verbatim — `register_user_class_metadata` rebinds
    /// `cpl[0]` to the freshly-allocated id, but since they match, the
    /// rebind is a no-op.
    pub cpl: Vec<u32>,
    pub slots: Vec<AotSlotRegistration>,
    /// For each slot in `slots`, the class id that introduced it.
    pub slot_origin: Vec<u32>,
    pub own_slot_count: usize,
    pub inherited_slot_count: usize,
}

/// Sprint 40a — one slot's worth of metadata, serialised into the
/// EXE. Mirrors `nod_runtime::SlotInfo` plus a `type_tag` describing
/// the `SlotType` variant, plus the optional `init_keyword` flattened
/// into a `(ptr, len)` byte slice.
#[derive(Clone, Debug)]
pub struct AotSlotRegistration {
    pub name: String,
    pub offset: usize,
    /// Encodes the `SlotType` variant. See
    /// `nod_runtime::aot::AOT_SLOT_TYPE_*` constants for the mapping.
    pub type_tag: u8,
    /// Payload for `SlotType::Class(_)` — the class id. Zero for all
    /// other variants (the shim ignores it).
    pub type_class_id: u32,
    pub init_keyword: Option<String>,
    pub required_init_keyword: bool,
    /// Encodes `SlotDefault`: 0 = `Unbound`, 1 = `Value(default_value)`.
    pub default_init_tag: u8,
    /// Raw `Word` bits for `SlotDefault::Value`. Zero for `Unbound`.
    pub default_init_value: u64,
    pub has_setter: bool,
}

/// The renamed user `main` symbol the staticlib's
/// `nod_aot_main_wrapper` (in `nod-runtime/src/aot.rs`) calls. Must
/// agree with the `extern "C-unwind"` declaration there.
pub const NOD_USER_MAIN_SYMBOL: &str = "nod_user_main";

/// The Rust-side wrapper exposed by `nod_runtime.lib`. The codegen-
/// injected `i32 @main()` stub forwards to this symbol; the linker
/// resolves it to the static-library object at AOT link time.
pub const NOD_AOT_MAIN_WRAPPER_SYMBOL: &str = "nod_aot_main_wrapper";

/// Sprint 39a — the synthesised function that walks the manifest at
/// startup and fills every `nod_*` global with its runtime-resolved
/// bits. Emitted by [`emit_aot_entry_stubs`] from the manifest.
const NOD_AOT_RESOLVE_RELOCS_SYMBOL: &str = "nod_aot_resolve_relocs";

/// Errors emitted during the AOT post-processing + object emission
/// pipeline. Wraps [`JitError`] for the LLVM-side failures and adds
/// AOT-specific variants for missing entry points.
#[derive(Debug)]
pub enum AotError {
    /// The codegen'd module didn't contain a function named `main` —
    /// the user's source is missing `define function main () … end`.
    MissingMain,
    /// `inkwell` complained while creating the target machine or
    /// emitting the object file.
    Llvm(String),
    /// Stand-in for a structural problem the post-processing pass
    /// can't recover from (e.g. an existing `nod_user_main` symbol
    /// collision).
    Conflict(String),
    /// Underlying JIT engine plumbing failure (target init, etc.).
    Jit(JitError),
}

impl std::fmt::Display for AotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMain => write!(
                f,
                "AOT: source file must define `main () => () end` for EXE entry"
            ),
            Self::Llvm(s) => write!(f, "AOT/LLVM: {s}"),
            Self::Conflict(s) => write!(f, "AOT/conflict: {s}"),
            Self::Jit(e) => write!(f, "AOT/JIT: {e}"),
        }
    }
}

impl std::error::Error for AotError {}

impl From<JitError> for AotError {
    fn from(e: JitError) -> Self {
        Self::Jit(e)
    }
}

/// Sprint 39a — post-process a codegen'd module in place to add the C
/// `main` entry point an EXE needs.
///
/// Steps:
///   1. Look up the function named `main`. If absent, error out
///      ([`AotError::MissingMain`]).
///   2. Rename it to `nod_user_main` (the symbol the runtime wrapper
///      declares as `extern`).
///   3. Add a fresh `i32 @main()` whose body is:
///         ```llvm
///         %rc = call i32 @nod_aot_main_wrapper()
///         ret i32 %rc
///         ```
///      so the CRT's `mainCRTStartup` calls our `main`, which calls
///      the Rust wrapper, which runs `nod_runtime_init()` and then
///      `nod_user_main()`.
///
/// The injected `main` is declared `External` (not `LinkOnceODR` /
/// `WeakODR`) so the linker treats it as the strong definition of
/// `main` for the EXE.
///
/// # Why the renamed user `main` keeps its original signature
///
/// The brief specifies `nod_user_main() -> i64` on the Rust side. The
/// Dylan-level `main` body is lowered with whatever return type sema
/// inferred (typically `Unit` → no return value at the LLVM level, or
/// `Boolean`/`Integer` → an i64 Word). The Rust wrapper discards the
/// return value, so any signature works at the LLVM level — but the
/// `extern "C-unwind" fn nod_user_main() -> i64` declaration in
/// `nod-runtime` requires the symbol to either have an `i64` return
/// or no return-value site at all. We satisfy this by NOT changing the
/// function's existing signature here; the Dylan-emitted body returns
/// whatever its inferred type lowers to (most commonly `void` for a
/// Unit-returning `main`), and the Rust extern's `i64` return is
/// "what's in `rax` after the call" — a void function leaves `rax`
/// untouched, which the wrapper happens to discard anyway.
///
/// A future sprint could tighten this by inserting an i64-cast
/// trampoline. Sprint 39a accepts the loose contract — the wrapper
/// throws the value away and the hello-world test asserts only on
/// stdout + exit code.
pub fn emit_aot_entry_stubs<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
) -> Result<(), AotError> {
    emit_aot_entry_stubs_with_registrations_and_safepoints(
        module,
        manifest,
        &AotRegistrations::default(),
        &[],
    )
}

/// Sprint 39c — variant of [`emit_aot_entry_stubs`] that also emits
/// startup registration calls for the merged Dylan-side methods /
/// blocks / functions. The driver hands a non-empty
/// [`AotRegistrations`] here when building any AOT EXE that uses
/// stdlib generics (which is essentially every non-trivial program).
pub fn emit_aot_entry_stubs_with_registrations<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
) -> Result<(), AotError> {
    emit_aot_entry_stubs_with_registrations_and_safepoints(
        module,
        manifest,
        registrations,
        &[],
    )
}

/// Variant of [`emit_aot_entry_stubs_with_registrations`] that also
/// bakes the image-installed safepoint descriptors into private module
/// globals for later AOT metadata consumption.
pub fn emit_aot_entry_stubs_with_registrations_and_safepoints<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
) -> Result<(), AotError> {
    emit_aot_entry_stubs_full(
        module,
        manifest,
        registrations,
        safepoint_installs,
        "main",
    )
}

/// Sprint 50d — superset variant that accepts the Dylan-source entry
/// function name. The traditional pipeline uses `"main"` (and every
/// public wrapper above defaults to it); `.prj` files can override via
/// `start_function = "..."` so a bundle whose source files all happen
/// to define `main` can pick a non-colliding entry. The behaviour is
/// otherwise identical: the named function gets renamed to
/// `nod_user_main` and the runtime C wrapper extern-decls against
/// that symbol regardless of the source-language name.
pub fn emit_aot_entry_stubs_full<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
    entry_function: &str,
) -> Result<(), AotError> {
    emit_aot_entry_stubs_full_with_mode(
        module,
        manifest,
        registrations,
        safepoint_installs,
        entry_function,
        AotShape::Executable,
    )
}

/// Sprint 51b — the shape of the AOT object the entry-stub pass should
/// emit. `Executable` is the original "build a Windows EXE" path:
/// rename the user's entry to `nod_user_main`, emit a synthetic
/// `i32 @main` that the CRT calls, the works. `StaticLibrary` skips
/// both — every emitted symbol keeps its source-language name, no
/// `main` is added — so the resulting `.obj` can be statically linked
/// into a host EXE (currently `nod-driver` for the `--lex-with-dylan`
/// path) without colliding with the host's own `main`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AotShape {
    /// Sprint 39a — the original EXE shape. Renames the start function
    /// to `nod_user_main` and injects `i32 @main()` that calls the
    /// resolver + wrapper.
    Executable,
    /// Sprint 51b — library shape. Keep all source-language names
    /// intact; skip the synthetic `main` so the linker doesn't see a
    /// duplicate when the `.obj` is bundled into a Rust binary. The
    /// resolver (`nod_aot_resolve_relocs`) is still emitted with
    /// external linkage; the host must call it once before invoking
    /// any of the Dylan-side functions.
    StaticLibrary,
}

pub fn emit_aot_entry_stubs_full_with_mode<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
    entry_function: &str,
    shape: AotShape,
) -> Result<(), AotError> {
    // Resist the temptation to rename `<eval-entry>` here — that name
    // is reserved for the JIT path. AOT users write `define function
    // main` (or another name per the project's `start_function`).
    let user_main = module
        .get_function(entry_function)
        .ok_or(AotError::MissingMain)?;

    if matches!(shape, AotShape::Executable) {
        // Guard against a pre-existing `nod_user_main` (would be unusual —
        // user shouldn't pick that name — but a clear error beats silent
        // overwriting).
        if module.get_function(NOD_USER_MAIN_SYMBOL).is_some() {
            return Err(AotError::Conflict(format!(
                "module already declares a function named `{NOD_USER_MAIN_SYMBOL}` — \
                 user source must not collide with the AOT entry-stub renaming"
            )));
        }

        // Step 1+2: rename the user's `main` to `nod_user_main`. inkwell
        // exposes `set_name` on `FunctionValue` via `LLVMSetValueName2`.
        user_main.as_global_value().set_name(NOD_USER_MAIN_SYMBOL);
        // External linkage so the staticlib's extern declaration finds it.
        user_main.set_linkage(Linkage::External);

        // Sprint 50c-4 fix — when the user chose a non-`main` entry
        // function, another source file in the bundle may STILL define a
        // function named `main` (the canonical example is bundling
        // `dylan-parser.dylan`, whose CLI entry happens to be named
        // `main`, with a smoke harness whose entry is `smoke-main`).
        //
        // Two problems if we leave that orphan `main` alone:
        //   1. It steals the C entry point. Windows' `mainCRTStartup` /
        //      Unix' `_start` find `main` and call it before
        //      `nod_aot_resolve_relocs` has populated literal-string
        //      globals — every `format-out` inside that `main` crashes
        //      with "format string is not a <byte-string> (raw 0x0)".
        //   2. The synthetic C `main` the AOT pipeline adds below (step
        //      3a) collides on the symbol name.
        //
        // Fix: rename the orphan to a private symbol AND demote linkage
        // to Internal so the synthetic C `main` can claim the name
        // unambiguously. Other Dylan code that referred to it via the
        // dispatch tables already points at the LLVM function value, not
        // the name, so the rename is transparent to callers.
        if entry_function != "main"
            && let Some(orphan) = module.get_function("main")
            && orphan != user_main
        {
            orphan.as_global_value().set_name("nod_orphan_main");
            orphan.set_linkage(Linkage::Internal);
        }
    } else {
        // Library mode — leave the start function untouched (it stays
        // callable by its source-language name from the host). BUT we
        // still need to deal with any `main` symbol that lives in one
        // of the bundled source files: e.g. bundling
        // `dylan-parser.dylan` (whose CLI entry is named `main`)
        // alongside `dylan-lex-shim.dylan` (whose entry is
        // `shim-main`). If we left that `main` with External linkage,
        // linking the .obj into the host (`nod-driver`) would collide
        // with the host's own `main` (Rust's CRT entry).
        //
        // Demote any `main` that ISN'T the user's chosen start
        // function — same Sprint 50c-4 fix as the EXE path, just for
        // a different reason (host conflict instead of CRT-finds-the-
        // wrong-entry).
        if entry_function != "main"
            && let Some(orphan) = module.get_function("main")
            && orphan != user_main
        {
            orphan.as_global_value().set_name("nod_orphan_main");
            orphan.set_linkage(Linkage::Internal);
        }
        // `user_main` itself stays at whatever linkage codegen gave
        // it (External by default for top-level Dylan functions);
        // the host needs to be able to reach it by name.
        let _ = user_main;
    }

    let ctx = module.get_context();

    // Step 3a: emit dllimport externs + static `ApiStubEntry` globals
    // for every `RelocKind::StubEntry`. The Windows loader fills the
    // dllimport's IAT slot before any code in this EXE runs; our
    // resolver later copies `&<symbol>` (the linker-emitted thunk) into
    // each entry's `fn_ptr` field. Returns a `(symbol → stub-entry-global,
    // dllimport-fn)` map so step 3b and step 4 can reach them.
    let stub_entries = emit_stub_entry_globals(module, manifest)?;

    // Step 3b: convert every manifest-mentioned external global into a
    // defining `i64` global with internal linkage. For non-StubEntry
    // kinds the initialiser is zero (resolver populates at startup).
    // For StubEntry the initialiser is `ptrtoint(@__nod_stub_entry_X to i64)`
    // — a constant expression LLVM can fold, so the slot's contents
    // start out pointing at the static stub-entry global from the
    // first instruction of `main()` onward.
    convert_externals_to_defining_storage(module, manifest, &stub_entries)?;

    // Step 4: bake the image safepoint descriptors as a private AOT
    // table so the object file carries canonical installed-site
    // metadata for later image readers. Reject non-image records here:
    // the AOT pipeline should never be handed compile-to-memory
    // descriptors.
    emit_aot_image_safepoint_table(module, safepoint_installs)?;

    // Step 4b: emit the resolver function. It calls a per-RelocKind
    // C-ABI helper for each manifest entry, passing the global's
    // address and any per-kind parameters. For StubEntry the resolver
    // stores `&<dllimport_symbol>` into each entry's `fn_ptr` field.
    //
    // Sprint 39c — the resolver also emits per-method / per-block /
    // per-top-level-function registration calls so the dispatch /
    // function-ref / block registries are populated with the merged
    // stdlib (and user-defined) bodies BEFORE `nod_user_main` runs.
    let resolver_fn =
        emit_resolve_relocs_function(module, manifest, &stub_entries, registrations, shape)?;

    if matches!(shape, AotShape::Executable) {
        // Step 5: emit `i32 @main()` that calls the resolver, then the
        // wrapper, then returns the wrapper's rc.
        let i32_ty = ctx.i32_type();
        let main_ty = i32_ty.fn_type(&[], false);

        // Wrapper extern decl. `nod_runtime.lib` provides the definition.
        let wrapper_fn = match module.get_function(NOD_AOT_MAIN_WRAPPER_SYMBOL) {
            Some(f) => f,
            None => {
                module.add_function(NOD_AOT_MAIN_WRAPPER_SYMBOL, main_ty, Some(Linkage::External))
            }
        };

        let main_fn = module.add_function("main", main_ty, Some(Linkage::External));
        let entry = ctx.append_basic_block(main_fn, "entry");
        let builder = ctx.create_builder();
        builder.position_at_end(entry);
        builder
            .build_call(resolver_fn, &[], "")
            .map_err(|e| AotError::Llvm(format!("build_call resolver: {e}")))?;
        let call = builder
            .build_call(wrapper_fn, &[], "rc")
            .map_err(|e| AotError::Llvm(format!("build_call wrapper: {e}")))?;
        let rc = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| AotError::Llvm("wrapper call returned void".into()))?;
        builder
            .build_return(Some(&rc))
            .map_err(|e| AotError::Llvm(format!("build_return: {e}")))?;
    } else {
        // Library mode — no synthetic `main`. The resolver was emitted
        // above and is callable from the host via its symbol name; the
        // host is responsible for calling it once before any of the
        // Dylan-side functions.
        let _ = resolver_fn;
    }

    // Re-verify so a botched IR change here surfaces early (before the
    // driver hands the module to TargetMachine).
    module
        .verify()
        .map_err(|e| AotError::Llvm(format!("post-AOT-stub verify: {e}")))?;
    Ok(())
}

/// Sprint 39a/b — walk the manifest and convert each external global into
/// a defining `i64` global with internal linkage. For most kinds the
/// runtime-side resolver (emitted by [`emit_resolve_relocs_function`])
/// populates each at startup. For `StubEntry` kinds the initialiser is
/// already `ptrtoint(@__nod_stub_entry_X to i64)` — the linker resolves
/// that constant expression at link time, so the slot's contents start
/// out pointing at the static `ApiStubEntry` from the first instruction
/// of `main()`. The Windows loader has already populated the IAT slot
/// the entry's `fn_ptr` will reference by the time `nod_aot_resolve_relocs`
/// runs.
///
/// Skips symbols that aren't actually present in the module (this
/// happens when optimisation eliminates a load through the global).
fn convert_externals_to_defining_storage<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    stub_entries: &StubEntryInfoMap<'ctx>,
) -> Result<(), AotError> {
    let ctx = module.get_context();
    let i64_ty = ctx.i64_type();
    for entry in &manifest.entries {
        let Some(g) = module.get_global(&entry.symbol) else {
            continue;
        };
        // The global was declared `external` + `externally_initialized`
        // by codegen. Switch to internal storage with the appropriate
        // initialiser. `set_initializer` removes the external flag at
        // the IR level for the global to be a definition.
        let init = match &entry.kind {
            RelocKind::StubEntry { symbol, .. } => {
                // Use `ptrtoint @__nod_stub_entry_<symbol> to i64` so the
                // slot's contents are the address of the static entry
                // from EXE-load onward. No runtime work required for
                // *this* slot; `fn_ptr` inside the entry is still
                // populated at startup by the resolver.
                let info = stub_entries.get(symbol).ok_or_else(|| {
                    AotError::Conflict(format!(
                        "internal: stub-entry global missing for `{symbol}`"
                    ))
                })?;
                info.entry_global
                    .as_pointer_value()
                    .const_to_int(i64_ty)
            }
            _ => i64_ty.const_zero(),
        };
        g.set_initializer(&init);
        g.set_linkage(Linkage::Internal);
        g.set_externally_initialized(false);
    }
    Ok(())
}

/// Sprint 39b — per-unique `(dll, symbol)` info threaded between the
/// stub-entry emission step, the global-conversion step (which points
/// the `@nod_stub__<key>__<idx>` slot at the static entry), and the
/// resolver-function step (which stores `&<symbol>` into the entry's
/// `fn_ptr` field at startup).
struct StubEntryInfo<'ctx> {
    /// The static `%ApiStubEntry`-typed global named
    /// `__nod_stub_entry_<symbol>`, defined in the EXE's data section.
    /// Multiple manifest rows for the same symbol share one.
    entry_global: GlobalValue<'ctx>,
    /// The dllimport extern declared as `declare dllimport i64 @<symbol>(...)`.
    /// Its address is what gets stored into `entry_global`'s `fn_ptr`
    /// field at startup. The Windows loader resolves the symbol via the
    /// import library named in the manifest's `dll` field.
    dllimport_fn: inkwell::values::FunctionValue<'ctx>,
}

type StubEntryInfoMap<'ctx> = HashMap<String, StubEntryInfo<'ctx>>;

/// Sprint 39b — for each `RelocKind::StubEntry { dll, symbol, signature_bytes }`
/// in the manifest, emit:
///
/// 1. A dllimport extern function declaration. The exact signature we
///    declare doesn't have to match the Win32 API's true signature
///    bytewise — the trampoline at `nod_winffi_call_N` performs the
///    real Win64 marshaling using the recorded [`ApiCallSignature`].
///    All we need is *some* signature that lets LLVM emit the dllimport
///    reference; we use `i64 (...)` varargs-style which the linker
///    accepts as a symbol reference.
///
/// 2. A static `%ApiStubEntry`-typed global named `__nod_stub_entry_<symbol>`.
///    The struct's field layout reproduces the `#[repr(C)]` shape of
///    [`nod_runtime::ApiStubEntry`] exactly (see comments in
///    `nod-runtime/src/winffi.rs`).  The `signature` field is baked
///    from `signature_bytes` (which is the `#[repr(C)]` byte dump from
///    sema); `fn_ptr` starts null and is populated at startup by the
///    resolver. The `dll_name_*` / `symbol_name_*` fields are left
///    null — the AOT path doesn't need them for marshaling (only for
///    the JIT-path error message on null `fn_ptr`).
///
/// Multiple manifest rows referencing the same `(dll, symbol)` reuse
/// the same dllimport extern and static stub-entry global — the
/// per-module `nod_stub__<key>__<idx>` slot indices are distinct but
/// each points at the same `__nod_stub_entry_<symbol>` global.
fn emit_stub_entry_globals<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
) -> Result<StubEntryInfoMap<'ctx>, AotError> {
    let ctx = module.get_context();
    let mut out: StubEntryInfoMap<'ctx> = HashMap::new();

    // ApiCallSignature is 14 bytes: { i8, [12 x i8], i8 } with align 1.
    let i8_ty = ctx.i8_type();
    let i16_ty = ctx.i16_type();
    let i32_ty = ctx.i32_type();
    let i64_ty = ctx.i64_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    let arr12 = i8_ty.array_type(12);

    // %ApiStubEntry layout (matches `nod_runtime::ApiStubEntry`
    // 56-byte / 8-aligned struct exactly):
    //   off 0  ptr    dll_name_ptr
    //   off 8  i32    dll_name_len
    //   off 12 i32    [padding]
    //   off 16 ptr    symbol_name_ptr
    //   off 24 i32    symbol_name_len
    //   off 28 i32    [padding]
    //   off 32 ptr    fn_ptr (AtomicPtr<u8>, ABI-identical to ptr on x64)
    //   off 40 i8     arg_count
    //   off 41 [12xi8] arg_kinds
    //   off 53 i8     return_kind
    //   off 54 i16    [tail padding to align size to 8]
    let entry_struct_ty = ctx.struct_type(
        &[
            ptr_ty.into(),
            i32_ty.into(),
            i32_ty.into(),
            ptr_ty.into(),
            i32_ty.into(),
            i32_ty.into(),
            ptr_ty.into(),
            i8_ty.into(),
            arr12.into(),
            i8_ty.into(),
            i16_ty.into(),
        ],
        false, // not packed; rely on natural alignment matching repr(C)
    );

    for entry in &manifest.entries {
        let RelocKind::StubEntry {
            dll: _,
            symbol,
            signature_bytes,
        } = &entry.kind
        else {
            continue;
        };
        if out.contains_key(symbol) {
            continue;
        }
        if signature_bytes.len() != 14 {
            return Err(AotError::Conflict(format!(
                "StubEntry signature for `{symbol}` is {} bytes; expected 14",
                signature_bytes.len()
            )));
        }
        // Build the dllimport extern. We declare it as `i64 @sym(...)` —
        // the actual ABI is honoured by the trampoline at call time.
        // The dllimport storage class tells LLVM to emit a reference
        // through the IAT (`__imp_<symbol>`). We never call this fn
        // directly from IR; the only IR use is taking its address.
        let dllimport_fn = match module.get_function(symbol) {
            Some(f) => f,
            None => {
                let fn_ty = i64_ty.fn_type(&[], /*varargs=*/ true);
                let f = module.add_function(symbol, fn_ty, Some(Linkage::External));
                f.as_global_value()
                    .set_dll_storage_class(DLLStorageClass::Import);
                f
            }
        };
        // Build the static ApiStubEntry global.
        let entry_global_name = format!("__nod_stub_entry_{}", crate::symbols::sanitize(symbol));
        let entry_global = match module.get_global(&entry_global_name) {
            Some(g) => g,
            None => {
                let g = module.add_global(
                    entry_struct_ty,
                    Some(AddressSpace::default()),
                    &entry_global_name,
                );
                g.set_linkage(Linkage::Internal);

                // Initialiser: zero strings, null fn_ptr, baked signature.
                let arg_count = i8_ty.const_int(signature_bytes[0] as u64, false);
                let arg_kinds_vals: Vec<_> = signature_bytes[1..13]
                    .iter()
                    .map(|b| i8_ty.const_int(*b as u64, false))
                    .collect();
                let arg_kinds = i8_ty.const_array(&arg_kinds_vals);
                let return_kind = i8_ty.const_int(signature_bytes[13] as u64, false);

                let zero_i32 = i32_ty.const_zero();
                let zero_i16 = i16_ty.const_zero();
                let null_ptr = ptr_ty.const_null();

                let init = entry_struct_ty.const_named_struct(&[
                    null_ptr.into(),     // dll_name_ptr
                    zero_i32.into(),     // dll_name_len
                    zero_i32.into(),     // padding
                    null_ptr.into(),     // symbol_name_ptr
                    zero_i32.into(),     // symbol_name_len
                    zero_i32.into(),     // padding
                    null_ptr.into(),     // fn_ptr — written at startup
                    arg_count.into(),    // signature.arg_count
                    arg_kinds.into(),    // signature.arg_kinds
                    return_kind.into(),  // signature.return_kind
                    zero_i16.into(),     // tail padding
                ]);
                g.set_initializer(&init);
                g
            }
        };
        out.insert(
            symbol.clone(),
            StubEntryInfo {
                entry_global,
                dllimport_fn,
            },
        );
    }
    Ok(out)
}

/// Sprint 39a/b — emit the `void @nod_aot_resolve_relocs()` function that
/// the `main` stub calls before the user's `main`. The function iterates
/// over every manifest entry and:
///
/// - For most kinds: calls the corresponding `nod_aot_set_*` runtime
///   helper to populate the slot with its in-process bits.
/// - For `RelocKind::StubEntry`: emits an inline `store ptr @<symbol>,
///   ptr <fn_ptr_field>` so the static `ApiStubEntry`'s `fn_ptr` field
///   carries the dllimport function's address. The Windows loader has
///   already populated the IAT slot by the time this code runs, so
///   `@<symbol>` is a stable, valid function pointer for the rest of
///   the process's lifetime.
fn emit_resolve_relocs_function<'ctx>(
    module: &Module<'ctx>,
    manifest: &ModuleManifest,
    stub_entries: &StubEntryInfoMap<'ctx>,
    registrations: &AotRegistrations,
    shape: AotShape,
) -> Result<inkwell::values::FunctionValue<'ctx>, AotError> {
    let ctx = module.get_context();
    let void_ty = ctx.void_type();
    let i64_ty = ctx.i64_type();
    let i32_ty = ctx.i32_type();
    let isize_ty = ctx.ptr_sized_int_type(
        &inkwell::targets::TargetData::create(""),
        Some(AddressSpace::default()),
    );
    let ptr_ty = ctx.ptr_type(AddressSpace::default());

    // Type signatures for each helper. Use C ABI: `void (...)`.
    let helper_set_imm = void_ty.fn_type(&[ptr_ty.into()], false);
    let helper_set_class_md = void_ty.fn_type(&[ptr_ty.into(), i32_ty.into()], false);
    // `(slot, text_ptr, text_len)` — `len` is `size_t`. We use the
    // target's pointer-sized int to stay portable; on x86_64 Windows
    // that's u64.
    let helper_set_lit = void_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), isize_ty.into()], false);
    // `(slot, key_prefix_ptr, key_prefix_len, site_id)`.
    let helper_set_cache_slot = void_ty.fn_type(
        &[ptr_ty.into(), ptr_ty.into(), isize_ty.into(), i64_ty.into()],
        false,
    );
    // `(slot, name_ptr, name_len)`.
    let helper_set_generic = helper_set_lit;
    let helper_register_safepoints = void_ty.fn_type(&[ptr_ty.into(), isize_ty.into()], false);

    // Declare or recover each helper as an external.
    let get_or_add =
        |name: &str, ty: inkwell::types::FunctionType<'ctx>| -> inkwell::values::FunctionValue<'ctx> {
            module
                .get_function(name)
                .unwrap_or_else(|| module.add_function(name, ty, Some(Linkage::External)))
        };

    let set_imm_true = get_or_add("nod_aot_set_imm_true", helper_set_imm);
    let set_imm_false = get_or_add("nod_aot_set_imm_false", helper_set_imm);
    let set_imm_nil = get_or_add("nod_aot_set_imm_nil", helper_set_imm);
    let set_imm_false_wrapper = get_or_add("nod_aot_set_imm_false_wrapper", helper_set_imm);
    let set_class_md = get_or_add("nod_aot_set_class_md", helper_set_class_md);
    let set_strlit = get_or_add("nod_aot_set_strlit", helper_set_lit);
    let set_symlit = get_or_add("nod_aot_set_symlit", helper_set_lit);
    let set_cache_slot = get_or_add("nod_aot_set_cache_slot", helper_set_cache_slot);
    let set_generic = get_or_add("nod_aot_set_generic", helper_set_generic);
    let register_safepoints = get_or_add(
        "nod_aot_register_safepoints",
        helper_register_safepoints,
    );

    // Declare the `nod_runtime_init` extern. The resolver calls it
    // first so class metadata, condition classes, generics, etc., are
    // registered BEFORE we try to populate the slots that reference
    // them. Pre-Sprint-39a's first hello-world attempt called the
    // resolver before the init wrapper, which left `class_metadata_ptr
    // (<range>)` returning null because `ensure_collections_registered`
    // hadn't run yet.
    let runtime_init = module
        .get_function("nod_runtime_init")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_runtime_init",
                void_ty.fn_type(&[], false),
                Some(Linkage::External),
            )
        });

    let resolver_ty = void_ty.fn_type(&[], false);
    // Sprint 51b — library-mode .objs need an external resolver so the
    // host (`nod-driver` for `--lex-with-dylan`) can `extern "C"` it
    // and run it once at startup. EXE shape keeps internal linkage —
    // the only caller in that mode is the synthetic `i32 @main()` we
    // emit a few steps later, and a private resolver doesn't pollute
    // the EXE's symbol table.
    let resolver_linkage = match shape {
        AotShape::Executable => Linkage::Internal,
        AotShape::StaticLibrary => Linkage::External,
    };
    let resolver_fn =
        module.add_function(NOD_AOT_RESOLVE_RELOCS_SYMBOL, resolver_ty, Some(resolver_linkage));
    let entry = ctx.append_basic_block(resolver_fn, "entry");
    let builder = ctx.create_builder();
    builder.position_at_end(entry);

    // Run init first. Idempotent, so safe even if `nod_aot_main_wrapper`
    // also calls it after the resolver returns (which it does — the
    // wrapper's signature is "init then user_main"). The second call
    // is an atomic load of the `LazyLock` guard, negligible cost.
    builder
        .build_call(runtime_init, &[], "")
        .map_err(|e| AotError::Llvm(format!("call runtime_init: {e}")))?;

    if let (Some(table), Some(count)) = (
        module.get_global("__nod_aot_safepoints"),
        module.get_global("__nod_aot_safepoint_count"),
    ) {
        let table_ptr = unsafe {
            builder
                .build_gep(
                    table.get_value_type().into_array_type(),
                    table.as_pointer_value(),
                    &[i32_ty.const_zero(), i32_ty.const_zero()],
                    "aot.safepoints.ptr",
                )
                .map_err(|e| AotError::Llvm(format!("gep aot safepoints: {e}")))?
        };
        let count_value = builder
            .build_load(i64_ty, count.as_pointer_value(), "aot.safepoints.count")
            .map_err(|e| AotError::Llvm(format!("load aot safepoint count: {e}")))?
            .into_int_value();
        let count_isize = builder
            .build_int_cast(count_value, isize_ty, "aot.safepoints.count.isize")
            .map_err(|e| AotError::Llvm(format!("cast aot safepoint count: {e}")))?;
        let args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![table_ptr.into(), count_isize.into()];
        builder
            .build_call(register_safepoints, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_safepoints: {e}")))?;
    }

    // Sprint 40a — register user classes BEFORE the manifest-entry
    // loop. `RelocKind::ClassMetadata { class_id }` slots in the
    // manifest can reference user-class IDs (any `make(<C>, …)` site
    // bakes the class's metadata pointer through such a slot); if the
    // class isn't in the runtime table when `nod_aot_set_class_md`
    // calls `class_metadata_ptr(id)`, the lookup returns null and the
    // slot stays zero — observable later as `make: class metadata
    // pointer is null`. Registering user classes first guarantees
    // every later metadata lookup hits a live entry.
    //
    // User-class allocations bump `next_user_id`; the EXE-side
    // `nod_aot_register_user_class` shim asserts the freshly-allocated
    // id matches the compiler's expected id. Pairing this with the
    // `nod_runtime::nod_runtime_init()` call inside
    // `compile_file_for_aot` keeps `next_user_id` aligned between the
    // two processes.
    emit_user_class_registrations(module, &builder, &ctx, registrations, isize_ty, i32_ty)?;

    // Per-manifest-entry call emission. Each entry resolves its slot
    // and invokes the appropriate helper. Entries that reference symbols
    // not present in the module (eliminated by codegen / opt) are
    // skipped silently.
    for entry in &manifest.entries {
        let Some(g) = module.get_global(&entry.symbol) else {
            continue;
        };
        let slot_ptr = g.as_pointer_value();
        match &entry.kind {
            RelocKind::ImmTrue => {
                builder
                    .build_call(set_imm_true, &[slot_ptr.into()], "")
                    .map_err(|e| AotError::Llvm(format!("call set_imm_true: {e}")))?;
            }
            RelocKind::ImmFalse => {
                builder
                    .build_call(set_imm_false, &[slot_ptr.into()], "")
                    .map_err(|e| AotError::Llvm(format!("call set_imm_false: {e}")))?;
            }
            RelocKind::ImmNil => {
                builder
                    .build_call(set_imm_nil, &[slot_ptr.into()], "")
                    .map_err(|e| AotError::Llvm(format!("call set_imm_nil: {e}")))?;
            }
            RelocKind::ImmFalseWrapper => {
                builder
                    .build_call(set_imm_false_wrapper, &[slot_ptr.into()], "")
                    .map_err(|e| AotError::Llvm(format!("call set_imm_false_wrapper: {e}")))?;
            }
            RelocKind::ClassMetadata { class_id } => {
                let id = i32_ty.const_int(*class_id as u64, false);
                let args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![slot_ptr.into(), id.into()];
                builder
                    .build_call(set_class_md, &args, "")
                    .map_err(|e| AotError::Llvm(format!("call set_class_md: {e}")))?;
            }
            RelocKind::StringLiteral { text } => {
                let (str_ptr, len) = emit_byte_constant(module, &builder, &ctx, text.as_bytes())?;
                let len_v = isize_ty.const_int(len as u64, false);
                let args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![slot_ptr.into(), str_ptr.into(), len_v.into()];
                builder
                    .build_call(set_strlit, &args, "")
                    .map_err(|e| AotError::Llvm(format!("call set_strlit: {e}")))?;
            }
            RelocKind::SymbolLiteral { name } => {
                let (str_ptr, len) = emit_byte_constant(module, &builder, &ctx, name.as_bytes())?;
                let len_v = isize_ty.const_int(len as u64, false);
                let args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![slot_ptr.into(), str_ptr.into(), len_v.into()];
                builder
                    .build_call(set_symlit, &args, "")
                    .map_err(|e| AotError::Llvm(format!("call set_symlit: {e}")))?;
            }
            RelocKind::CacheSlot { site_id } => {
                let (kp_ptr, kp_len) =
                    emit_byte_constant(module, &builder, &ctx, manifest.key_prefix.as_bytes())?;
                let kp_len_v = isize_ty.const_int(kp_len as u64, false);
                let site_id_v = i64_ty.const_int(*site_id, false);
                let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
                    slot_ptr.into(),
                    kp_ptr.into(),
                    kp_len_v.into(),
                    site_id_v.into(),
                ];
                builder
                    .build_call(set_cache_slot, &args, "")
                    .map_err(|e| AotError::Llvm(format!("call set_cache_slot: {e}")))?;
            }
            RelocKind::Generic { name } => {
                let (str_ptr, len) = emit_byte_constant(module, &builder, &ctx, name.as_bytes())?;
                let len_v = isize_ty.const_int(len as u64, false);
                let args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![slot_ptr.into(), str_ptr.into(), len_v.into()];
                builder
                    .build_call(set_generic, &args, "")
                    .map_err(|e| AotError::Llvm(format!("call set_generic: {e}")))?;
            }
            RelocKind::StubEntry { symbol, .. } => {
                // Sprint 39b — populate the static ApiStubEntry's
                // `fn_ptr` field with the dllimport function's address.
                // The Windows loader has already filled the IAT slot
                // for `__imp_<symbol>` by the time this code runs;
                // taking the address of `@<symbol>` yields a stable
                // function pointer (either the symbol itself or a
                // linker-emitted thunk that jumps through the IAT).
                //
                // Skip if we have no record of this symbol — should be
                // impossible given `emit_stub_entry_globals` walks the
                // same manifest, but a clear no-op beats panicking.
                let Some(info) = stub_entries.get(symbol) else { continue };
                // `fn_ptr` lives at struct field index 6 (offset 32).
                let entry_ptr = info.entry_global.as_pointer_value();
                let entry_struct_ty =
                    info.entry_global.get_value_type().into_struct_type();
                let fn_ptr_field = unsafe {
                    builder
                        .build_in_bounds_gep(
                            entry_struct_ty,
                            entry_ptr,
                            &[i32_ty.const_zero(), i32_ty.const_int(6, false)],
                            "stub.fn_ptr",
                        )
                        .map_err(|e| AotError::Llvm(format!("gep stub.fn_ptr: {e}")))?
                };
                let fn_ptr_value: BasicValueEnum<'ctx> =
                    info.dllimport_fn.as_global_value().as_pointer_value().into();
                builder
                    .build_store(fn_ptr_field, fn_ptr_value)
                    .map_err(|e| AotError::Llvm(format!("store stub.fn_ptr: {e}")))?;
            }
        }
    }

    // Sprint 39c — the remaining three categories (functions, methods,
    // blocks) emit after the manifest-entry loop. Sprint 40a already
    // emitted user-class registrations BEFORE the manifest loop (see
    // the call above `for entry in &manifest.entries` — class metadata
    // must exist before `RelocKind::ClassMetadata` slots populate).
    //
    // Order matters in two ways:
    //
    // 1. Top-level functions register before methods so a method body
    //    that references `\name` (via a function-ref construction
    //    site) can resolve at runtime. The function-ref registry is
    //    consulted lazily by the call-site, so technically registering
    //    after methods would still work — but the visit-order here
    //    matches the JIT path's `register_top_level_functions →
    //    register_methods → register_blocks` sequence to keep the
    //    eager-load discipline predictable.
    //
    // 2. Methods register before blocks because a block's `exception`
    //    handler body MAY want to call generics on the captured-
    //    condition value. The handler thunks themselves don't go
    //    through the dispatch table (their addresses are stored
    //    inside `BlockFns.handlers`), but methods they invoke do.
    emit_top_level_function_registrations(module, &builder, &ctx, registrations, isize_ty)?;
    emit_method_registrations(module, &builder, &ctx, registrations, isize_ty, i32_ty)?;
    emit_block_registrations(module, &builder, &ctx, registrations, isize_ty)?;
    // GAP-004 — variable registrations come LAST in the resolver:
    // a variable's `__init-<name>` thunk can call any user function /
    // stdlib method / Win32 API, all of which must be live in the
    // dispatch / function-ref / block registries before init runs.
    emit_variable_registrations(module, &builder, &ctx, registrations, isize_ty)?;

    builder
        .build_return(None)
        .map_err(|e| AotError::Llvm(format!("resolver build_return: {e}")))?;
    Ok(resolver_fn)
}

/// Sprint 40a — for each [`AotUserClassRegistration`] emit a call to
/// `nod_aot_register_user_class(...)` inside the resolver. Bake the
/// per-class arrays (parents, cpl, slots, slot_origin) as private
/// LLVM globals so the resolver hands raw pointer + length pairs to
/// the runtime shim.
///
/// `%AotSlotEntry` layout MUST match `nod_runtime::aot::AotSlotEntry`'s
/// `#[repr(C)]` shape exactly. We construct one struct value per slot
/// and bake them into a `[AotSlotEntry; N]` per class.
#[allow(clippy::too_many_arguments)]
fn emit_user_class_registrations<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    registrations: &AotRegistrations,
    isize_ty: inkwell::types::IntType<'ctx>,
    i32_ty: inkwell::types::IntType<'ctx>,
) -> Result<(), AotError> {
    if registrations.user_classes.is_empty() {
        return Ok(());
    }
    let void_ty = ctx.void_type();
    let i8_ty = ctx.i8_type();
    let i64_ty = ctx.i64_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());

    // `%AotSlotEntry` layout (matches `nod_runtime::aot::AotSlotEntry`
    // exactly — 56 bytes total on x86_64, 8-aligned):
    //   off 0   ptr     name_ptr
    //   off 8   isize   name_len
    //   off 16  isize   offset
    //   off 24  i8      type_tag
    //   off 25  i8      required_init_keyword
    //   off 26  i8      default_init_tag
    //   off 27  i8      has_setter
    //   off 28  i32     _pad (so type_class_id is 4-aligned)
    //   off 32  i32     type_class_id
    //   off 36  i32     _pad2 (so default_init_value is 8-aligned)
    //   off 40  i64     default_init_value
    //   off 48  ptr     init_keyword_ptr
    //   off 56  isize   init_keyword_len
    //
    // Inkwell's `struct_type(&[...], packed=false)` lays out fields
    // naturally; the two explicit `i32` padding fields handle the
    // alignment gaps the Rust side spells out so the codegen layout
    // matches the runtime `#[repr(C)]` byte-for-byte. Total: 64 bytes.
    let slot_struct_ty = ctx.struct_type(
        &[
            ptr_ty.into(),    // name_ptr
            isize_ty.into(),  // name_len
            isize_ty.into(),  // offset
            i8_ty.into(),     // type_tag
            i8_ty.into(),     // required_init_keyword
            i8_ty.into(),     // default_init_tag
            i8_ty.into(),     // has_setter
            i32_ty.into(),    // _pad
            i32_ty.into(),    // type_class_id
            i32_ty.into(),    // _pad2
            i64_ty.into(),    // default_init_value
            ptr_ty.into(),    // init_keyword_ptr
            isize_ty.into(),  // init_keyword_len
        ],
        false,
    );

    // `void nod_aot_register_user_class(
    //     ptr name, isize name_len, i32 class_id,
    //     ptr parents, isize n_parents,
    //     ptr cpl, isize n_cpl,
    //     ptr slots, isize n_slots,
    //     ptr slot_origin, isize n_slot_origin,
    //     isize own_slot_count, isize inherited_slot_count)`.
    let helper_ty = void_ty.fn_type(
        &[
            ptr_ty.into(),
            isize_ty.into(),
            i32_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            isize_ty.into(),
            isize_ty.into(),
        ],
        false,
    );
    let helper = module
        .get_function("nod_aot_register_user_class")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_aot_register_user_class",
                helper_ty,
                Some(Linkage::External),
            )
        });

    for (idx, reg) in registrations.user_classes.iter().enumerate() {
        // Bake the class name as a private `[N x i8]`.
        let (name_ptr, name_len) =
            emit_byte_constant(module, builder, ctx, reg.name.as_bytes())?;

        // Bake parents / cpl / slot_origin as `[N x i32]` private
        // globals. `emit_u32_array_constant` returns a base pointer
        // suitable for passing into the helper as `ptr`.
        let parents_ptr = emit_u32_array_constant(
            module,
            builder,
            i32_ty,
            &reg.parent_class_ids,
            &format!("__nod_aot_class_parents_{idx}"),
        )?;
        let cpl_ptr = emit_u32_array_constant(
            module,
            builder,
            i32_ty,
            &reg.cpl,
            &format!("__nod_aot_class_cpl_{idx}"),
        )?;
        let slot_origin_ptr = emit_u32_array_constant(
            module,
            builder,
            i32_ty,
            &reg.slot_origin,
            &format!("__nod_aot_class_slot_origin_{idx}"),
        )?;

        // Bake the slot array. Each entry's strings (`name`,
        // `init_keyword`) become private LLVM globals; the struct
        // literal points at them.
        let slots_ptr = if reg.slots.is_empty() {
            ptr_ty.const_null()
        } else {
            let mut slot_inits: Vec<inkwell::values::StructValue<'ctx>> =
                Vec::with_capacity(reg.slots.len());
            for (sidx, slot) in reg.slots.iter().enumerate() {
                let slot_name_arr_ty = i8_ty.array_type(slot.name.len() as u32);
                let slot_name_global = module.add_global(
                    slot_name_arr_ty,
                    Some(AddressSpace::default()),
                    &format!("__nod_aot_class_{idx}_slot_{sidx}_name"),
                );
                slot_name_global.set_linkage(Linkage::Private);
                slot_name_global.set_constant(true);
                let bytes: Vec<_> = slot
                    .name
                    .as_bytes()
                    .iter()
                    .map(|b| i8_ty.const_int(*b as u64, false))
                    .collect();
                slot_name_global.set_initializer(&i8_ty.const_array(&bytes));

                let (init_kw_ptr_const, init_kw_len_const) = match &slot.init_keyword {
                    Some(kw) => {
                        let kw_arr_ty = i8_ty.array_type(kw.len() as u32);
                        let kw_global = module.add_global(
                            kw_arr_ty,
                            Some(AddressSpace::default()),
                            &format!("__nod_aot_class_{idx}_slot_{sidx}_initkw"),
                        );
                        kw_global.set_linkage(Linkage::Private);
                        kw_global.set_constant(true);
                        let kw_bytes: Vec<_> = kw
                            .as_bytes()
                            .iter()
                            .map(|b| i8_ty.const_int(*b as u64, false))
                            .collect();
                        kw_global.set_initializer(&i8_ty.const_array(&kw_bytes));
                        (
                            kw_global.as_pointer_value(),
                            isize_ty.const_int(kw.len() as u64, false),
                        )
                    }
                    None => (ptr_ty.const_null(), isize_ty.const_zero()),
                };

                let init = slot_struct_ty.const_named_struct(&[
                    slot_name_global.as_pointer_value().into(),
                    isize_ty.const_int(slot.name.len() as u64, false).into(),
                    isize_ty.const_int(slot.offset as u64, false).into(),
                    i8_ty.const_int(slot.type_tag as u64, false).into(),
                    i8_ty.const_int(if slot.required_init_keyword { 1 } else { 0 }, false).into(),
                    i8_ty.const_int(slot.default_init_tag as u64, false).into(),
                    i8_ty.const_int(if slot.has_setter { 1 } else { 0 }, false).into(),
                    i32_ty.const_zero().into(), // _pad
                    i32_ty.const_int(slot.type_class_id as u64, false).into(),
                    i32_ty.const_zero().into(), // _pad2
                    i64_ty.const_int(slot.default_init_value, false).into(),
                    init_kw_ptr_const.into(),
                    init_kw_len_const.into(),
                ]);
                slot_inits.push(init);
            }
            let arr_ty = slot_struct_ty.array_type(reg.slots.len() as u32);
            let arr_init = slot_struct_ty.const_array(&slot_inits);
            let arr_global = module.add_global(
                arr_ty,
                Some(AddressSpace::default()),
                &format!("__nod_aot_class_slots_{idx}"),
            );
            arr_global.set_linkage(Linkage::Private);
            // Not `const` — the struct fields are immutable but the
            // pointer values themselves are link-time constants;
            // marking the array constant is fine but we leave it
            // mutable for symmetry with the handlers array, which the
            // GC may eventually need to keep mutable.
            arr_global.set_constant(false);
            arr_global.set_initializer(&arr_init);
            arr_global.as_pointer_value()
        };

        let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            name_ptr.into(),
            isize_ty.const_int(name_len as u64, false).into(),
            i32_ty.const_int(reg.class_id as u64, false).into(),
            parents_ptr.into(),
            isize_ty.const_int(reg.parent_class_ids.len() as u64, false).into(),
            cpl_ptr.into(),
            isize_ty.const_int(reg.cpl.len() as u64, false).into(),
            slots_ptr.into(),
            isize_ty.const_int(reg.slots.len() as u64, false).into(),
            slot_origin_ptr.into(),
            isize_ty.const_int(reg.slot_origin.len() as u64, false).into(),
            isize_ty.const_int(reg.own_slot_count as u64, false).into(),
            isize_ty.const_int(reg.inherited_slot_count as u64, false).into(),
        ];
        builder
            .build_call(helper, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_user_class: {e}")))?;
    }
    Ok(())
}

/// Sprint 40a — bake a `[N x i32]` array as a private constant
/// global, return a pointer to its first element. Used for the
/// per-class parents / CPL / slot_origin arrays passed to
/// `nod_aot_register_user_class`. Empty arrays return a null pointer
/// (the shim treats `n == 0` as "no entries").
fn emit_u32_array_constant<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    i32_ty: inkwell::types::IntType<'ctx>,
    values: &[u32],
    name: &str,
) -> Result<inkwell::values::PointerValue<'ctx>, AotError> {
    if values.is_empty() {
        let ptr_ty = module.get_context().ptr_type(AddressSpace::default());
        return Ok(ptr_ty.const_null());
    }
    let arr_ty = i32_ty.array_type(values.len() as u32);
    let g = module.add_global(arr_ty, Some(AddressSpace::default()), name);
    g.set_linkage(Linkage::Private);
    g.set_constant(true);
    let elts: Vec<_> = values
        .iter()
        .map(|v| i32_ty.const_int(*v as u64, false))
        .collect();
    g.set_initializer(&i32_ty.const_array(&elts));
    let zero = i32_ty.const_zero();
    let ptr = unsafe {
        builder
            .build_gep(arr_ty, g.as_pointer_value(), &[zero, zero], "")
            .map_err(|e| AotError::Llvm(format!("build_gep u32 array: {e}")))?
    };
    Ok(ptr)
}

/// Sprint 39c — for each [`AotFunctionRegistration`] emit a call to
/// `nod_aot_register_jit_function(name_ptr, name_len, arity, code_ptr)`
/// inside the resolver. The runtime helper appends `(name, arity,
/// code_ptr)` to the process-global JIT function registry the
/// dispatcher consults for `\name` references.
fn emit_top_level_function_registrations<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    registrations: &AotRegistrations,
    isize_ty: inkwell::types::IntType<'ctx>,
) -> Result<(), AotError> {
    if registrations.functions.is_empty() {
        return Ok(());
    }
    let void_ty = ctx.void_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    // `void nod_aot_register_jit_function(ptr name, isize name_len,
    //                                     isize arity, ptr code)`.
    let helper_ty =
        void_ty.fn_type(&[ptr_ty.into(), isize_ty.into(), isize_ty.into(), ptr_ty.into()], false);
    let helper = module
        .get_function("nod_aot_register_jit_function")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_aot_register_jit_function",
                helper_ty,
                Some(Linkage::External),
            )
        });
    for reg in &registrations.functions {
        // Skip if the body function isn't in the module (codegen-DCE
        // may have stripped an unused stdlib helper).
        let Some(fv) = module.get_function(&reg.body_fn_name) else {
            continue;
        };
        let (name_ptr, name_len) =
            emit_byte_constant(module, builder, ctx, reg.name.as_bytes())?;
        let arity_v = isize_ty.const_int(reg.arity as u64, false);
        let code_ptr: BasicValueEnum<'ctx> = fv.as_global_value().as_pointer_value().into();
        let len_v = isize_ty.const_int(name_len as u64, false);
        let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            name_ptr.into(),
            len_v.into(),
            arity_v.into(),
            code_ptr.into(),
        ];
        builder
            .build_call(helper, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_jit_function: {e}")))?;
    }
    Ok(())
}

/// Sprint 39c — for each [`AotMethodRegistration`] emit a call to
/// `nod_aot_register_method(...)` inside the resolver. The runtime
/// helper looks up / creates the generic, then appends the method to
/// the dispatch table.
///
/// Specialisers (`Vec<u32>` class IDs) are baked as a private LLVM
/// `[u32; N]` constant per method.
fn emit_method_registrations<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    registrations: &AotRegistrations,
    isize_ty: inkwell::types::IntType<'ctx>,
    i32_ty: inkwell::types::IntType<'ctx>,
) -> Result<(), AotError> {
    if registrations.methods.is_empty() {
        return Ok(());
    }
    let void_ty = ctx.void_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    // `void nod_aot_register_method(ptr gname, isize glen, ptr specs,
    //                               isize n_specs, ptr body, isize pcount,
    //                               ptr bname, isize blen)`.
    let helper_ty = void_ty.fn_type(
        &[
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
        ],
        false,
    );
    let helper = module
        .get_function("nod_aot_register_method")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_aot_register_method",
                helper_ty,
                Some(Linkage::External),
            )
        });
    for (idx, reg) in registrations.methods.iter().enumerate() {
        let Some(fv) = module.get_function(&reg.body_fn_name) else {
            // Codegen didn't emit this body — `emit_aot_entry_stubs`
            // verify would have caught a hard miss, so this branch only
            // fires when DCE strips a dead method. Skip silently.
            continue;
        };
        // Bake specialisers as a private `[N x i32]` global.
        let spec_arr_ty = i32_ty.array_type(reg.specialisers.len() as u32);
        let spec_vals: Vec<_> = reg
            .specialisers
            .iter()
            .map(|id| i32_ty.const_int(*id as u64, false))
            .collect();
        let spec_init = i32_ty.const_array(&spec_vals);
        let spec_global = module.add_global(
            spec_arr_ty,
            Some(AddressSpace::default()),
            &format!("__nod_aot_specs_{idx}"),
        );
        spec_global.set_linkage(Linkage::Private);
        spec_global.set_constant(true);
        spec_global.set_initializer(&spec_init);
        let zero = i32_ty.const_zero();
        let spec_ptr = unsafe {
            builder
                .build_gep(
                    spec_arr_ty,
                    spec_global.as_pointer_value(),
                    &[zero, zero],
                    "",
                )
                .map_err(|e| AotError::Llvm(format!("gep specialisers: {e}")))?
        };

        let (gname_ptr, gname_len) =
            emit_byte_constant(module, builder, ctx, reg.generic_name.as_bytes())?;
        let (bname_ptr, bname_len) =
            emit_byte_constant(module, builder, ctx, reg.body_fn_name.as_bytes())?;

        let body_ptr: BasicValueEnum<'ctx> = fv.as_global_value().as_pointer_value().into();
        let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            gname_ptr.into(),
            isize_ty.const_int(gname_len as u64, false).into(),
            spec_ptr.into(),
            isize_ty.const_int(reg.specialisers.len() as u64, false).into(),
            body_ptr.into(),
            isize_ty.const_int(reg.param_count as u64, false).into(),
            bname_ptr.into(),
            isize_ty.const_int(bname_len as u64, false).into(),
        ];
        builder
            .build_call(helper, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_method: {e}")))?;
    }
    Ok(())
}

/// Sprint 39c — for each [`AotBlockRegistration`] emit a call to
/// `nod_aot_register_block(...)` inside the resolver. Cleanup and
/// afterwards thunks are passed as nullable function pointers;
/// handlers are baked into a static `[AotHandlerEntry; N]` per block.
fn emit_block_registrations<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    registrations: &AotRegistrations,
    isize_ty: inkwell::types::IntType<'ctx>,
) -> Result<(), AotError> {
    if registrations.blocks.is_empty() {
        return Ok(());
    }
    let void_ty = ctx.void_type();
    let i32_ty = ctx.i32_type();
    let i64_ty = ctx.i64_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());

    // `void nod_aot_register_block(i64 id, ptr body, ptr cleanup,
    //                              ptr afterwards, ptr handlers,
    //                              isize n_handlers)`.
    let helper_ty = void_ty.fn_type(
        &[
            i64_ty.into(),
            ptr_ty.into(),
            ptr_ty.into(),
            ptr_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
        ],
        false,
    );
    let helper = module
        .get_function("nod_aot_register_block")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_aot_register_block",
                helper_ty,
                Some(Linkage::External),
            )
        });

    // `%AotHandlerEntry` layout (matches `nod_runtime::aot::AotHandlerEntry`
    // exactly):
    //   off 0  i32  class_id
    //   off 4  i32  padding
    //   off 8  ptr  class_name_ptr
    //   off 16 isize class_name_len
    //   off 24 ptr  body
    let handler_struct_ty = ctx.struct_type(
        &[
            i32_ty.into(),
            i32_ty.into(),
            ptr_ty.into(),
            isize_ty.into(),
            ptr_ty.into(),
        ],
        false,
    );

    for (idx, reg) in registrations.blocks.iter().enumerate() {
        let Some(body_fv) = module.get_function(&reg.body_fn_name) else {
            continue;
        };
        let cleanup_ptr: BasicValueEnum<'ctx> = match &reg.cleanup_fn_name {
            Some(n) => match module.get_function(n) {
                Some(fv) => fv.as_global_value().as_pointer_value().into(),
                None => ptr_ty.const_null().into(),
            },
            None => ptr_ty.const_null().into(),
        };
        let afterwards_ptr: BasicValueEnum<'ctx> = match &reg.afterwards_fn_name {
            Some(n) => match module.get_function(n) {
                Some(fv) => fv.as_global_value().as_pointer_value().into(),
                None => ptr_ty.const_null().into(),
            },
            None => ptr_ty.const_null().into(),
        };

        // Bake handlers as a static array.
        let (handlers_ptr_val, n_handlers) = if reg.handlers.is_empty() {
            (ptr_ty.const_null(), 0usize)
        } else {
            let mut handler_initializers: Vec<inkwell::values::StructValue<'ctx>> =
                Vec::with_capacity(reg.handlers.len());
            for h in &reg.handlers {
                let body_fv = match module.get_function(&h.body_fn_name) {
                    Some(fv) => fv,
                    None => {
                        // Unreachable in normal flows; skip via null
                        // body so the block still registers other
                        // handlers cleanly.
                        let name_ptr = ptr_ty.const_null();
                        let init = handler_struct_ty.const_named_struct(&[
                            i32_ty.const_int(h.class_id as u64, false).into(),
                            i32_ty.const_zero().into(),
                            name_ptr.into(),
                            isize_ty.const_zero().into(),
                            ptr_ty.const_null().into(),
                        ]);
                        handler_initializers.push(init);
                        continue;
                    }
                };
                // Bake the class-name string as a private global.
                let name_arr_ty = ctx.i8_type().array_type(h.class_name.len() as u32);
                let name_global = module.add_global(
                    name_arr_ty,
                    Some(AddressSpace::default()),
                    &format!("__nod_aot_handler_cname_{idx}_{}", h.class_id),
                );
                name_global.set_linkage(Linkage::Private);
                name_global.set_constant(true);
                let bytes: Vec<_> = h
                    .class_name
                    .as_bytes()
                    .iter()
                    .map(|b| ctx.i8_type().const_int(*b as u64, false))
                    .collect();
                name_global.set_initializer(&ctx.i8_type().const_array(&bytes));

                let init = handler_struct_ty.const_named_struct(&[
                    i32_ty.const_int(h.class_id as u64, false).into(),
                    i32_ty.const_zero().into(),
                    name_global.as_pointer_value().into(),
                    isize_ty
                        .const_int(h.class_name.len() as u64, false)
                        .into(),
                    body_fv.as_global_value().as_pointer_value().into(),
                ]);
                handler_initializers.push(init);
            }
            let arr_ty = handler_struct_ty.array_type(reg.handlers.len() as u32);
            let arr_init = handler_struct_ty.const_array(&handler_initializers);
            let handlers_global = module.add_global(
                arr_ty,
                Some(AddressSpace::default()),
                &format!("__nod_aot_handlers_{idx}"),
            );
            handlers_global.set_linkage(Linkage::Private);
            handlers_global.set_constant(false); // contains live pointers
            handlers_global.set_initializer(&arr_init);
            (handlers_global.as_pointer_value(), reg.handlers.len())
        };

        let block_id_v = i64_ty.const_int(reg.block_id, false);
        let body_ptr: BasicValueEnum<'ctx> = body_fv.as_global_value().as_pointer_value().into();
        let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            block_id_v.into(),
            body_ptr.into(),
            cleanup_ptr.into(),
            afterwards_ptr.into(),
            handlers_ptr_val.into(),
            isize_ty.const_int(n_handlers as u64, false).into(),
        ];
        builder
            .build_call(helper, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_block: {e}")))?;
    }
    Ok(())
}

/// GAP-004 — for each [`AotVariableRegistration`] emit a call to
/// `nod_aot_register_variable(name_ptr, name_len, init_fn_ptr)`
/// inside the resolver. The shim calls the init thunk to get the
/// variable's initial Word, allocates a fresh `<cell>`, and stores
/// the cell pointer in the variable's process-global slot.
///
/// The init thunk is a regular Dylan function the user-side codegen
/// already emitted as `__init-<name>` (see the `Item::DefineVariable`
/// arm of `lower_module_full`). We take its address by name; skip the
/// variable if the function isn't in the module (codegen-DCE may have
/// stripped it, though that shouldn't happen because the variable's
/// getter also depends on it transitively).
fn emit_variable_registrations<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    registrations: &AotRegistrations,
    isize_ty: inkwell::types::IntType<'ctx>,
) -> Result<(), AotError> {
    if registrations.variables.is_empty() {
        return Ok(());
    }
    let void_ty = ctx.void_type();
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    // `void nod_aot_register_variable(ptr name, isize name_len,
    //                                 ptr init_fn)`.
    let helper_ty =
        void_ty.fn_type(&[ptr_ty.into(), isize_ty.into(), ptr_ty.into()], false);
    let helper = module
        .get_function("nod_aot_register_variable")
        .unwrap_or_else(|| {
            module.add_function(
                "nod_aot_register_variable",
                helper_ty,
                Some(Linkage::External),
            )
        });
    for reg in &registrations.variables {
        let Some(fv) = module.get_function(&reg.init_fn_name) else {
            continue;
        };
        let (name_ptr, name_len) =
            emit_byte_constant(module, builder, ctx, reg.name.as_bytes())?;
        let init_ptr: BasicValueEnum<'ctx> =
            fv.as_global_value().as_pointer_value().into();
        let args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            name_ptr.into(),
            isize_ty.const_int(name_len as u64, false).into(),
            init_ptr.into(),
        ];
        builder
            .build_call(helper, &args, "")
            .map_err(|e| AotError::Llvm(format!("call register_variable: {e}")))?;
    }
    Ok(())
}

/// Emit a private `[N x i8]` constant containing `bytes` and return a
/// pointer to its first element. Used by `emit_resolve_relocs_function`
/// to pass string literals + symbol names + the key prefix as
/// `(ptr, len)` pairs to the runtime helpers.
///
/// Each call creates a fresh global. Doing dedup here would shave a
/// few bytes from the EXE but complicates the code; the linker's COMDAT
/// dedup handles identical constants on its own.
fn emit_byte_constant<'ctx>(
    module: &Module<'ctx>,
    builder: &inkwell::builder::Builder<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    bytes: &[u8],
) -> Result<(inkwell::values::PointerValue<'ctx>, usize), AotError> {
    let i8_ty = ctx.i8_type();
    let arr_ty = i8_ty.array_type(bytes.len() as u32);
    let g = module.add_global(arr_ty, Some(AddressSpace::default()), "__nod_aot_str");
    g.set_linkage(Linkage::Private);
    g.set_constant(true);
    let const_arr = i8_ty.const_array(
        &bytes
            .iter()
            .map(|b| i8_ty.const_int(*b as u64, false))
            .collect::<Vec<_>>(),
    );
    g.set_initializer(&const_arr);
    let zero = ctx.i32_type().const_zero();
    let ptr = unsafe {
        builder
            .build_gep(arr_ty, g.as_pointer_value(), &[zero, zero], "")
            .map_err(|e| AotError::Llvm(format!("build_gep byte constant: {e}")))?
    };
    Ok((ptr, bytes.len()))
}

fn emit_aot_image_safepoint_table<'ctx>(
    module: &Module<'ctx>,
    safepoint_installs: &[SafepointInstallRecord],
) -> Result<(), AotError> {
    if safepoint_installs.is_empty() {
        return Ok(());
    }
    if module.get_global("__nod_aot_safepoints").is_some() {
        return Err(AotError::Conflict(
            "module already defines `__nod_aot_safepoints`".to_string(),
        ));
    }
    if module.get_global("__nod_aot_safepoint_count").is_some() {
        return Err(AotError::Conflict(
            "module already defines `__nod_aot_safepoint_count`".to_string(),
        ));
    }

    let ctx = module.get_context();
    let i8_ty = ctx.i8_type();
    let i64_ty = ctx.i64_type();
    let isize_ty = ctx.ptr_sized_int_type(
        &inkwell::targets::TargetData::create(""),
        Some(AddressSpace::default()),
    );
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    let record_ty = ctx.struct_type(
        &[
            i64_ty.into(),   // site_id
            i8_ty.into(),    // kind_tag
            i64_ty.into(),   // computation_index
            i64_ty.into(),   // root_count
            ptr_ty.into(),   // section_label_ptr
            isize_ty.into(), // section_label_len
            ptr_ty.into(),   // patchpoint_label_ptr
            isize_ty.into(), // patchpoint_label_len
            ptr_ty.into(),   // function_ptr
            isize_ty.into(), // function_len
            ptr_ty.into(),   // block_label_ptr
            isize_ty.into(), // block_label_len
        ],
        false,
    );

    let mut row_inits = Vec::with_capacity(safepoint_installs.len());
    for (idx, site) in safepoint_installs.iter().enumerate() {
        if site.installed_text_region.kind != InstalledTextRegionKind::Image {
            return Err(AotError::Conflict(format!(
                "AOT received non-image safepoint install record for site {}",
                site.site_id
            )));
        }

        let section_label = emit_private_byte_global(
            module,
            &ctx,
            &format!("__nod_aot_safepoint_{idx}_section"),
            site.installed_text_region.section_label.as_bytes(),
        );
        let patchpoint_label = emit_private_byte_global(
            module,
            &ctx,
            &format!("__nod_aot_safepoint_{idx}_patchpoint"),
            site.patchpoint_label.as_bytes(),
        );
        let function_name = emit_private_byte_global(
            module,
            &ctx,
            &format!("__nod_aot_safepoint_{idx}_function"),
            site.function.as_bytes(),
        );
        let block_label = emit_private_byte_global(
            module,
            &ctx,
            &format!("__nod_aot_safepoint_{idx}_block"),
            site.block_label.as_bytes(),
        );

        let kind_tag = match site.kind {
            SafepointKind::DirectCall => 0,
            SafepointKind::Dispatch => 1,
            SafepointKind::SealedDirectCall => 2,
        };

        row_inits.push(record_ty.const_named_struct(&[
            i64_ty.const_int(site.site_id, false).into(),
            i8_ty.const_int(kind_tag, false).into(),
            i64_ty.const_int(site.computation_index as u64, false).into(),
            i64_ty.const_int(site.roots.len() as u64, false).into(),
            section_label.as_pointer_value().into(),
            isize_ty
                .const_int(site.installed_text_region.section_label.len() as u64, false)
                .into(),
            patchpoint_label.as_pointer_value().into(),
            isize_ty.const_int(site.patchpoint_label.len() as u64, false).into(),
            function_name.as_pointer_value().into(),
            isize_ty.const_int(site.function.len() as u64, false).into(),
            block_label.as_pointer_value().into(),
            isize_ty.const_int(site.block_label.len() as u64, false).into(),
        ]));
    }

    let arr_ty = record_ty.array_type(safepoint_installs.len() as u32);
    let table = module.add_global(arr_ty, Some(AddressSpace::default()), "__nod_aot_safepoints");
    table.set_linkage(Linkage::Private);
    table.set_constant(true);
    table.set_initializer(&record_ty.const_array(&row_inits));

    let count = module.add_global(i64_ty, Some(AddressSpace::default()), "__nod_aot_safepoint_count");
    count.set_linkage(Linkage::Private);
    count.set_constant(true);
    count.set_initializer(&i64_ty.const_int(safepoint_installs.len() as u64, false));
    Ok(())
}

fn emit_private_byte_global<'ctx>(
    module: &Module<'ctx>,
    ctx: &inkwell::context::ContextRef<'ctx>,
    name: &str,
    bytes: &[u8],
) -> GlobalValue<'ctx> {
    let i8_ty = ctx.i8_type();
    let arr_ty = i8_ty.array_type(bytes.len() as u32);
    let g = module.add_global(arr_ty, Some(AddressSpace::default()), name);
    g.set_linkage(Linkage::Private);
    g.set_constant(true);
    g.set_initializer(&i8_ty.const_array(
        &bytes
            .iter()
            .map(|b| i8_ty.const_int(*b as u64, false))
            .collect::<Vec<_>>(),
    ));
    g
}

/// Sprint 39a — write `module` to disk as a Windows COFF / ELF `.obj`
/// file at `path`. Caller is `nod-driver`; the produced `.obj` is fed
/// to `link.exe` alongside `nod_runtime.lib` to produce a Dylan EXE.
///
/// # Choices
///
/// - **Triple**: [`TargetMachine::get_default_triple`] — matches the
///   host. Sprint 39a doesn't cross-compile; a future sprint can
///   parameterise this.
/// - **CPU + features**: host CPU via [`TargetMachine::get_host_cpu_name`]
///   and `get_host_cpu_features`. The user's EXE runs on the same
///   machine that built it; using host features lets LLVM emit
///   AVX2/etc when present.
/// - **Optimisation level**: caller chooses. `nod-driver build` passes
///   `OptimizationLevel::Default` (LLVM's `-O2` equivalent) so the
///   shipped EXE is reasonably small + fast; `OptimizationLevel::None`
///   is exposed for debugging.
/// - **RelocMode**: `PIC`. Windows EXEs work fine with PIC (the loader
///   doesn't require it but accepts it), and PIC objects link cleanly
///   against `nod_runtime.lib` regardless of where its code lands in
///   the address space.
/// - **CodeModel**: `Default`. The Default model picks Small on
///   Windows x86_64, which is what we want for a non-huge EXE.
pub fn emit_object_file(
    module: &Module<'_>,
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), AotError> {
    // Initialise the X86 backend so `Target::from_triple` succeeds
    // even if no JIT has been spun up yet in this process. Cheap and
    // idempotent inside LLVM.
    Target::initialize_x86(&InitializationConfig::default());

    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| AotError::Llvm(e.to_string()))?;
    let cpu = TargetMachine::get_host_cpu_name();
    let features = TargetMachine::get_host_cpu_features();

    let machine = target
        .create_target_machine(
            &triple,
            cpu.to_str().unwrap_or("generic"),
            features.to_str().unwrap_or(""),
            opt_level,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| AotError::Llvm(format!("create_target_machine failed for {triple:?}")))?;

    // Ensure the module's data layout + triple match the target machine
    // — `inkwell` doesn't auto-populate these, and `link.exe` will
    // refuse mismatched object files. Setting them on the module is a
    // no-op if they're already set (codegen leaves them blank for JIT
    // use, but a fresh post-codegen module is OK to retag here).
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());

    machine
        .write_to_file(module, FileType::Object, path)
        .map_err(|e| AotError::Llvm(e.to_string()))?;
    Ok(())
}

/// Sprint 39a — convenience: the canonical AOT pipeline step that
/// follows `codegen_module_with_key`. Performs the entry-stub injection
/// plus writes the object file in one call. Most callers
/// (`nod-driver`'s `build` subcommand) want exactly this.
pub fn emit_aot_object(
    module: &Module<'_>,
    manifest: &ModuleManifest,
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), AotError> {
    emit_aot_entry_stubs(module, manifest)?;
    emit_object_file(module, path, opt_level)?;
    Ok(())
}

/// Sprint 39c — variant of [`emit_aot_object`] that threads the
/// merged-stdlib registrations into the entry-stub-injection pass.
/// The driver (`nod-driver build`) uses this; the older
/// [`emit_aot_object`] entry stays for callers that don't have a
/// registrations payload (currently just tests).
pub fn emit_aot_object_with_registrations(
    module: &Module<'_>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), AotError> {
    emit_aot_object_with_registrations_and_safepoints(
        module,
        manifest,
        registrations,
        &[],
        path,
        opt_level,
    )?;
    Ok(())
}

pub fn emit_aot_object_with_registrations_and_safepoints(
    module: &Module<'_>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
    path: &Path,
    opt_level: OptimizationLevel,
) -> Result<(), AotError> {
    emit_aot_object_full(
        module,
        manifest,
        registrations,
        safepoint_installs,
        path,
        opt_level,
        "main",
    )
}

/// Sprint 50d — superset of
/// [`emit_aot_object_with_registrations_and_safepoints`] that accepts
/// the Dylan-source entry-function name. The Rust runtime wrapper's
/// extern symbol is always `nod_user_main`; this name is what we look
/// up in the LLVM module before renaming. Defaulting to `"main"` keeps
/// every existing caller working unchanged.
#[allow(clippy::too_many_arguments)]
pub fn emit_aot_object_full(
    module: &Module<'_>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
    path: &Path,
    opt_level: OptimizationLevel,
    entry_function: &str,
) -> Result<(), AotError> {
    emit_aot_object_full_with_mode(
        module,
        manifest,
        registrations,
        safepoint_installs,
        path,
        opt_level,
        entry_function,
        AotShape::Executable,
    )
}

/// Sprint 51b — superset variant that picks the AOT shape. See [`AotShape`]
/// for the difference between executable and static-library outputs.
pub fn emit_aot_object_full_with_mode(
    module: &Module<'_>,
    manifest: &ModuleManifest,
    registrations: &AotRegistrations,
    safepoint_installs: &[SafepointInstallRecord],
    path: &Path,
    opt_level: OptimizationLevel,
    entry_function: &str,
    shape: AotShape,
) -> Result<(), AotError> {
    emit_aot_entry_stubs_full_with_mode(
        module,
        manifest,
        registrations,
        safepoint_installs,
        entry_function,
        shape,
    )?;
    emit_object_file(module, path, opt_level)?;
    Ok(())
}

/// Synthetic helper used by tests + drivers to construct
/// `TargetTriple` from a string without dragging `inkwell::targets` into
/// callers. Only public so `nod-driver` can print the chosen triple
/// in `--verbose` output.
pub fn default_triple_string() -> String {
    TargetMachine::get_default_triple().as_str().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkwell::context::Context;
    use inkwell::values::AsValueRef;

    fn image_safepoint(site_id: u64) -> SafepointInstallRecord {
        SafepointInstallRecord {
            namespace: 0,
            site_id,
            patchpoint_label: format!("gc.s{site_id}"),
            installed_text_region: crate::codegen::InstalledTextRegion {
                kind: InstalledTextRegionKind::Image,
                section_label: "image.code.text",
            },
            kind: SafepointKind::DirectCall,
            function: "main".to_string(),
            block_label: "entry".to_string(),
            computation_index: 0,
            roots: Vec::new(),
        }
    }

    fn make_user_main_module<'ctx>(ctx: &'ctx Context) -> Module<'ctx> {
        let module = ctx.create_module("hello");
        let i64_ty = ctx.i64_type();
        let main_ty = i64_ty.fn_type(&[], false);
        let user_main = module.add_function("main", main_ty, Some(Linkage::External));
        let bb = ctx.append_basic_block(user_main, "entry");
        let builder = ctx.create_builder();
        builder.position_at_end(bb);
        let zero = i64_ty.const_zero();
        builder.build_return(Some(&zero)).unwrap();
        module
    }

    #[test]
    fn entry_stub_renames_main() {
        let ctx = Context::create();
        let module = make_user_main_module(&ctx);
        let manifest = ModuleManifest::default();
        emit_aot_entry_stubs(&module, &manifest).expect("entry stub emission");
        // Original `main` should be renamed and a fresh `i32 @main`
        // should now exist as a separate function.
        let user = module.get_function(NOD_USER_MAIN_SYMBOL).unwrap();
        assert_eq!(user.get_name().to_str().unwrap(), NOD_USER_MAIN_SYMBOL);
        let new_main = module.get_function("main").unwrap();
        // Distinct function values.
        assert_ne!(user.as_global_value().as_value_ref(),
                   new_main.as_global_value().as_value_ref());
        // New main returns i32.
        let ret = new_main.get_type().get_return_type();
        assert!(matches!(ret.map(|t| t.is_int_type()), Some(true)));
    }

    #[test]
    fn entry_stub_errors_without_main() {
        let ctx = Context::create();
        let module = ctx.create_module("no_main");
        let manifest = ModuleManifest::default();
        let err = emit_aot_entry_stubs(&module, &manifest);
        assert!(matches!(err, Err(AotError::MissingMain)));
    }

    #[test]
    fn object_file_is_written_and_non_empty() {
        // Run inside the test's tempdir (per-process). Smoke check
        // only — checking COFF magic happens in the higher-level
        // `aot_object_emission` integration test.
        let dir = std::env::temp_dir().join(format!(
            "nod-aot-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.obj");
        let ctx = Context::create();
        let module = make_user_main_module(&ctx);
        let manifest = ModuleManifest::default();
        emit_aot_entry_stubs(&module, &manifest).unwrap();
        emit_object_file(&module, &path, OptimizationLevel::None).unwrap();
        let bytes = std::fs::read(&path).expect("read .obj");
        assert!(bytes.len() > 16, "expected non-trivial .obj, got {} bytes", bytes.len());
        // Best-effort cleanup; if removal fails we leave a stray temp
        // file but the test passes.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_stub_bakes_image_safepoint_table() {
        let ctx = Context::create();
        let module = make_user_main_module(&ctx);
        let manifest = ModuleManifest::default();

        emit_aot_entry_stubs_with_registrations_and_safepoints(
            &module,
            &manifest,
            &AotRegistrations::default(),
            &[image_safepoint(7)],
        )
        .expect("entry stub emission with safepoints");

        let ir = module.print_to_string().to_string();
        assert!(ir.contains("@__nod_aot_safepoints = private constant"), "missing safepoint table: {ir}");
        assert!(ir.contains("@__nod_aot_safepoint_count = private constant i64 1"), "missing safepoint count: {ir}");
        assert!(ir.contains("image.code.text"), "missing image section label: {ir}");
        assert!(ir.contains("gc.s7"), "missing patchpoint label: {ir}");
    }

    #[test]
    fn entry_stub_rejects_non_image_safepoint_table() {
        let ctx = Context::create();
        let module = make_user_main_module(&ctx);
        let manifest = ModuleManifest::default();
        let mut site = image_safepoint(1);
        site.installed_text_region.kind = InstalledTextRegionKind::InMemory;
        site.installed_text_region.section_label = "mem.code.text";

        let err = emit_aot_entry_stubs_with_registrations_and_safepoints(
            &module,
            &manifest,
            &AotRegistrations::default(),
            &[site],
        );

        assert!(matches!(err, Err(AotError::Conflict(msg)) if msg.contains("non-image safepoint install record")));
    }

    #[test]
    fn entry_stub_calls_runtime_safepoint_registration() {
        let ctx = Context::create();
        let module = make_user_main_module(&ctx);
        let manifest = ModuleManifest::default();

        emit_aot_entry_stubs_with_registrations_and_safepoints(
            &module,
            &manifest,
            &AotRegistrations::default(),
            &[image_safepoint(3)],
        )
        .expect("entry stub emission with safepoints");

        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("call void @nod_aot_register_safepoints"),
            "missing runtime safepoint registration call: {ir}"
        );
    }
}
