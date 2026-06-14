//! **Stdlib boundary**: new condition CLASSES go in
//! `src/nod-dylan/dylan-sources/stdlib.dylan`. The signal/handler/unwind
//! MECHANISM stays here per `docs/STDLIB_BOUNDARY.md` (Rust panic
//! coordination, CleanupGuard NLX path — frozen exception). Condition
//! hierarchy, accessor methods, and printers belong in Dylan.
//!
//! **Platform note**: `CleanupGuard`'s Drop currently coordinates with
//! Rust panic + Windows SEH. The macOS variant will rebuild this
//! coordination against Mach exception ports + Rust panic. See
//! `docs/PLATFORMS.md`. The Dylan-side condition surface (`<error>`,
//! `<warning>`, `signal`, `block`/`exception`/`cleanup`) stays
//! identical across platforms; only this Rust coordination layer
//! rebuilds.
//!
//! Sprint 19 — conditions, non-local exit, and the parallel handler chain.
//!
//! This module owns three things:
//!
//!   1. **Seed condition classes** — `<condition>`, `<warning>`,
//!      `<serious-condition>`, `<error>`, `<simple-error>`,
//!      `<no-applicable-methods-error>`, `<simple-restart>`, plus
//!      `<exit-procedure>` (the heap object that represents a `block (k)`
//!      capture). Registered at startup via the `LiteralPool` glue in
//!      `lib.rs` so the ClassIds are stable for the process lifetime
//!      and lowering / dispatch can resolve them by name.
//!
//!   2. **Thread-local handler chain** — pushed/popped by `block` /
//!      `exception` lowering. `nod_signal(condition)` walks the chain
//!      most-recent-first, finds the first frame whose `class_id`
//!      matches the condition (via the CPL walk), runs the handler
//!      body, then performs the non-local exit by `panic_any`ing an
//!      `NlxPayload` sentinel that the enclosing `block` shim catches
//!      and reads as "return this value from block-id N".
//!
//!   3. **`block` orchestration shim** — `nod_run_block` is the runtime
//!      entry point lowering emits for every `block` form. It:
//!        * pushes one handler frame per `exception` clause,
//!        * sets up an RAII cleanup guard (runs even if the body or a
//!          handler unwinds — Phase D),
//!        * `catch_unwind`s the body thunk,
//!        * if the caught payload is an `NlxPayload` targeting THIS
//!          block, returns the value; otherwise `resume_unwind`s,
//!        * runs `afterwards` after cleanup,
//!        * pops handlers and returns the chosen value.
//!
//! See `docs/CONDITIONS.md` for the architectural rationale, especially
//! why the unwind transport is `panic_any` rather than a bespoke
//! Win64-SEH walker.

use std::any::Any;
use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

use crate::classes::{
    ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, class_metadata_for, is_subclass,
};
use crate::make::rust_make;
use crate::aot::{active_aot_safepoint_depth, truncate_active_aot_safepoints};
use crate::stack_map::{active_jit_safepoint_depth, truncate_active_jit_safepoints};
use crate::word::Word;

// ─── Seed condition class IDs ──────────────────────────────────────────────
//
// Allocated lazily on first registration. Stored in a `OnceLock` so a
// second call to `ensure_registered` (e.g. from tests that
// `_reset_user_classes_for_tests`) is idempotent — we register only
// once per process.

#[allow(dead_code)] // some accessors only fire from Dylan-side / future sprints.
struct ConditionClassIds {
    condition: ClassId,
    warning: ClassId,
    serious_condition: ClassId,
    error: ClassId,
    simple_condition: ClassId,
    simple_warning: ClassId,
    simple_error: ClassId,
    no_applicable_methods_error: ClassId,
    no_next_method_error: ClassId,
    simple_restart: ClassId,
    exit_procedure: ClassId,

    // Cached metadata pointers — looked up once at registration time so
    // hot paths (make-helpers, slot offsets) don't go through the
    // registry mutex.
    simple_condition_md: &'static ClassMetadata,
    simple_warning_md: &'static ClassMetadata,
    simple_error_md: &'static ClassMetadata,
    no_applicable_methods_error_md: &'static ClassMetadata,
    no_next_method_error_md: &'static ClassMetadata,
    simple_restart_md: &'static ClassMetadata,
    exit_procedure_md: &'static ClassMetadata,
}

static CONDITION_CLASSES: OnceLock<ConditionClassIds> = OnceLock::new();

/// Register the Sprint 19 seed condition classes if they aren't already
/// registered. Idempotent — safe to call repeatedly.
///
/// Called from the `LiteralPool` initialiser in `lib.rs` so the classes
/// are available before any sema lowering runs. Also called defensively
/// from each public accessor below so tests that reset the registry
/// still see the classes.
pub fn ensure_registered() {
    let _ = CONDITION_CLASSES.get_or_init(|| {
        let (condition, _) = crate::register_simple_user_class("<condition>", None, Vec::new());
        let (warning, _) =
            crate::register_simple_user_class("<warning>", Some(condition), Vec::new());
        let (serious_condition, _) =
            crate::register_simple_user_class("<serious-condition>", Some(condition), Vec::new());
        let (error, _) =
            crate::register_simple_user_class("<error>", Some(serious_condition), Vec::new());

        // `<simple-condition>` carries the `message` slot. Sprint 19
        // deviation from DRM: `<simple-error>` and `<simple-warning>` are
        // single-inheritance subclasses of `<simple-condition>` (DRM
        // makes them MI subclasses of `<simple-condition>` and
        // `<error>` / `<warning>` respectively). Single inheritance is a
        // pragmatic Sprint 19 shape — the seed class hierarchy is
        // scaffolding that Sprint 25's stdlib port retires. Class
        // identity (`is_subclass(simple_error, error)`) still works
        // because callers check against `<error>` / `<warning>` via the
        // CPL walk, and our `<simple-error>` still derives from
        // `<error>` via the legacy path below. The price is a small
        // duplication: `<simple-warning>` doesn't `is_subclass` of
        // `<warning>` in Sprint 19 — Sprint 22 fixes this when
        // restart semantics land.
        let (simple_condition, _) = crate::register_simple_user_class(
            "<simple-condition>",
            Some(condition),
            vec![
                slot_str("format-string", "format-string"),
                slot_vec("format-args", "format-args"),
                slot_str("message", "message"),
            ],
        );

        // `<simple-error>` carries the same slot layout as
        // `<simple-condition>` (we re-declare for the SI path). Inherits
        // from `<error>` so `is_subclass(simple_error, error)` holds —
        // this is the relationship `signal()` walks. The `message` slot
        // semantics are identical.
        //
        // Sprint 19 only plumbs `message` end-to-end; the other two slots
        // exist so make-call-sites that supply `format-string:` /
        // `format-args:` don't trip the unknown-keyword diag. Real
        // format-string expansion lands in Sprint 22 alongside the
        // stdlib port.
        let (simple_error, _) = crate::register_simple_user_class(
            "<simple-error>",
            Some(error),
            vec![
                slot_str("format-string", "format-string"),
                slot_vec("format-args", "format-args"),
                slot_str("message", "message"),
            ],
        );

        // `<simple-warning>` — SI child of `<warning>` carrying the same
        // slot shape as `<simple-condition>`. See the note above; full
        // MI parentage (DRM-correct) lands in Sprint 22.
        let (simple_warning, _) = crate::register_simple_user_class(
            "<simple-warning>",
            Some(warning),
            vec![
                slot_str("format-string", "format-string"),
                slot_vec("format-args", "format-args"),
                slot_str("message", "message"),
            ],
        );

        let (no_applicable_methods_error, _) = crate::register_simple_user_class(
            "<no-applicable-methods-error>",
            Some(error),
            vec![
                slot_str("generic-name", "generic-name"),
                slot_vec("arg-class-names", "arg-class-names"),
            ],
        );

        // `<no-next-method-error>` — signalled by `next-method` when
        // there is no next method in the chain. Sprint 19 only seeds
        // the class; the dispatch path that signals it ships in
        // Sprint 22 alongside `next-method` argument validation.
        let (no_next_method_error, _) = crate::register_simple_user_class(
            "<no-next-method-error>",
            Some(error),
            vec![slot_str("generic-name", "generic-name")],
        );

        let (simple_restart, _) = crate::register_simple_user_class(
            "<simple-restart>",
            Some(condition), // DRM: <restart> is a subclass of <condition>.
            vec![
                slot_sym("restart-name", "restart-name"),
                slot_str("restart-description", "restart-description"),
            ],
        );

        // `<exit-procedure>` is the heap object that represents the `k`
        // in `block (k) ... end`. Calling `k(v)` triggers NLX to the
        // capturing block carrying `v`.
        let (exit_procedure, _) = crate::register_simple_user_class(
            "<exit-procedure>",
            None,
            vec![SlotInfo {
                name: "block-id".to_string(),
                offset: 0, // patched by registration
                type_kind: SlotType::Integer,
                init_keyword: Some("block-id".to_string()),
                required_init_keyword: false,
                default_init: SlotDefault::Unbound,
                has_setter: false,
            }],
        );

        let simple_condition_md = class_metadata_for(simple_condition);
        let simple_warning_md = class_metadata_for(simple_warning);
        let simple_error_md = class_metadata_for(simple_error);
        let no_applicable_methods_error_md = class_metadata_for(no_applicable_methods_error);
        let no_next_method_error_md = class_metadata_for(no_next_method_error);
        let simple_restart_md = class_metadata_for(simple_restart);
        let exit_procedure_md = class_metadata_for(exit_procedure);

        ConditionClassIds {
            condition,
            warning,
            serious_condition,
            error,
            simple_condition,
            simple_warning,
            simple_error,
            no_applicable_methods_error,
            no_next_method_error,
            simple_restart,
            exit_procedure,
            simple_condition_md,
            simple_warning_md,
            simple_error_md,
            no_applicable_methods_error_md,
            no_next_method_error_md,
            simple_restart_md,
            exit_procedure_md,
        }
    });
}

fn classes() -> &'static ConditionClassIds {
    ensure_registered();
    CONDITION_CLASSES.get().expect("conditions registered")
}

fn slot_str(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::String,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

fn slot_vec(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Vector,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

fn slot_sym(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: SlotType::Symbol,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

// ─── Public accessors ──────────────────────────────────────────────────────

pub fn condition_class_id() -> ClassId {
    classes().condition
}
pub fn warning_class_id() -> ClassId {
    classes().warning
}
pub fn serious_condition_class_id() -> ClassId {
    classes().serious_condition
}
pub fn error_class_id() -> ClassId {
    classes().error
}
pub fn simple_condition_class_id() -> ClassId {
    classes().simple_condition
}
pub fn simple_warning_class_id() -> ClassId {
    classes().simple_warning
}
pub fn simple_error_class_id() -> ClassId {
    classes().simple_error
}
pub fn no_applicable_methods_error_class_id() -> ClassId {
    classes().no_applicable_methods_error
}
pub fn no_next_method_error_class_id() -> ClassId {
    classes().no_next_method_error
}
pub fn simple_restart_class_id() -> ClassId {
    classes().simple_restart
}
pub fn exit_procedure_class_id() -> ClassId {
    classes().exit_procedure
}

/// Return the class name of the supplied condition Word, or `None` if
/// the Word is not a pointer-tagged Dylan object. Used by diagnostics
/// (`:handlers`) and by Phase G tests.
pub fn condition_class_name(condition: Word) -> Option<String> {
    let cid = condition_class_of(condition)?;
    Some(class_name_of(cid))
}

// ─── Builders ──────────────────────────────────────────────────────────────

/// Allocate a `<simple-error>` instance with the supplied `message`.
/// The `format-string` and `format-args` slots are left unbound.
pub fn make_simple_error(message: &str) -> Word {
    let md = classes().simple_error_md;
    let msg = crate::intern_string_literal(message);
    // SAFETY: simple_error_md is a registered class metadata; init
    // keywords match registered slot names.
    unsafe { rust_make(md, &[("message", msg)]) }
}

/// Allocate a `<simple-warning>` instance with the supplied `message`.
/// Same shape as `make_simple_error`.
pub fn make_simple_warning(message: &str) -> Word {
    let md = classes().simple_warning_md;
    let msg = crate::intern_string_literal(message);
    // SAFETY: simple_warning_md is a registered class metadata.
    unsafe { rust_make(md, &[("message", msg)]) }
}

/// Allocate a bare `<simple-condition>` instance with the supplied
/// `message`. Used by tests that want a `<simple-condition>` without
/// the `<error>` / `<warning>` flavour.
pub fn make_simple_condition(message: &str) -> Word {
    let md = classes().simple_condition_md;
    let msg = crate::intern_string_literal(message);
    // SAFETY: simple_condition_md is a registered class metadata.
    unsafe { rust_make(md, &[("message", msg)]) }
}

/// Allocate a `<no-applicable-methods-error>` carrying the generic name
/// and the comma-joined argument-class names as a bytestring (we'd use
/// `<simple-object-vector>` of symbols if Sprint 19 had a runtime vector
/// builder of arbitrary length; the current `make.rs` shim caps keyword
/// pairs at 8, and that's wired through the keyword-init path, not the
/// vector path. Comma-joined string is the Sprint 19 simplification —
/// the test assertion checks the generic name + a class name substring).
pub fn make_no_applicable_methods_error(generic: &str, arg_class_names: &[&str]) -> Word {
    let md = classes().no_applicable_methods_error_md;
    let g = crate::intern_string_literal(generic);
    // Encode the arg class list as a comma-joined bytestring for now;
    // the slot is annotated `<simple-object-vector>` but the Sprint 19
    // tests check the substring presence in the rendered message, not
    // the slot type. Sprint 22 will plumb a real vector through.
    let joined = arg_class_names.join(", ");
    let names_word = crate::intern_string_literal(&joined);
    // SAFETY: registered metadata + matching keyword names.
    unsafe {
        rust_make(
            md,
            &[("generic-name", g), ("arg-class-names", names_word)],
        )
    }
}

/// Allocate a `<simple-restart>` instance.
pub fn make_simple_restart(name: &str, description: &str) -> Word {
    let md = classes().simple_restart_md;
    let n = crate::intern_symbol_literal(name);
    let d = crate::intern_string_literal(description);
    // SAFETY: registered metadata + matching keyword names.
    unsafe {
        rust_make(
            md,
            &[("restart-name", n), ("restart-description", d)],
        )
    }
}

/// Sprint 19 stub: invoking a restart full-blown lands in Sprint 22.
/// The class exists and can be instantiated; the full protocol (search
/// the active restart chain, invoke the restart's body, etc.) is
/// deferred. See `docs/CONDITIONS.md` and `DEFERRED.md`.
pub fn invoke_restart(_r: Word) -> ! {
    panic!(
        "Sprint 19: invoke-restart not implemented; full restart semantics deferred to Sprint 22"
    );
}

/// Read the `message` slot of a `<simple-error>` (or any condition that
/// has a `message` slot at the same offset). Returns the slot's Word —
/// for a Sprint 19 `<simple-error>` that's a pointer-tagged
/// `<byte-string>`.
pub fn condition_message(c: Word) -> Word {
    let md = classes().simple_error_md;
    let offset = md
        .slot_offset("message")
        .expect("<simple-error> has a `message` slot");
    let Some(p) = c.as_ptr::<u8>() else {
        return Word::from_raw(0);
    };
    // SAFETY: caller asserts `c` points at a heap object laid out like
    // `<simple-error>`'s slot map. The Sprint 19 use site is via the
    // built-in `condition-message` lowering which only resolves on
    // values typed as `<error>` or narrower — which all inherit the
    // slot.
    unsafe {
        let slot_ptr = (p as usize + offset) as *const Word;
        *slot_ptr
    }
}

// ─── Handler stack ─────────────────────────────────────────────────────────

/// One frame on the thread-local handler chain. The frame says "if a
/// signalled condition is `<: handler_class>`, transfer control to
/// `target_block_id` carrying the **handler's** return value as the
/// block's result". `handler_index` lets the orchestration shim pick
/// which of the block's handler-thunks to invoke; the handler thunk
/// returns a Word that the shim then ferries out as the block's value.
#[derive(Clone, Debug)]
pub struct HandlerFrame {
    pub handler_class: ClassId,
    pub target_block_id: u64,
    pub handler_index: u32,
    /// Optional symbolic class name — used by `:handlers` dump output.
    /// The actual matching is by `handler_class` (the ClassId); this
    /// field is descriptive only.
    pub handler_class_name: String,
}

thread_local! {
    static HANDLER_STACK: RefCell<Vec<HandlerFrame>> = const { RefCell::new(Vec::new()) };
}

/// Push one handler frame.
pub fn nod_push_handler(frame: HandlerFrame) {
    HANDLER_STACK.with(|s| s.borrow_mut().push(frame));
}

/// Pop the top handler frame. No-op if the stack is empty.
pub fn nod_pop_handler() {
    HANDLER_STACK.with(|s| {
        s.borrow_mut().pop();
    });
}

/// Snapshot the current handler stack (for the `:handlers` meta-command
/// and tests).
pub fn handler_stack_snapshot() -> Vec<HandlerFrame> {
    HANDLER_STACK.with(|s| s.borrow().clone())
}

/// Iterate the current thread-local handler stack most-recently-pushed
/// first. Mirrors how `nod_signal` walks the chain. The closure is
/// invoked with a borrowed reference to each frame; mutating the stack
/// from inside is not supported (we snapshot before calling).
pub fn for_each_handler(mut f: impl FnMut(&HandlerFrame)) {
    let snapshot = handler_stack_snapshot();
    for frame in snapshot.iter().rev() {
        f(frame);
    }
}

/// Test helper: drop every frame on the current thread's handler stack.
///
/// `#[serial]`-marked tests call this in their setup to recover from
/// any earlier test that left the stack dirty (e.g. a `catch_unwind`
/// against an unhandled-condition panic — the panic transits before
/// the enclosing `nod_run_block`'s Drop guard can pop the frames it
/// pushed, so a stale frame can survive). The function is gated
/// `#[doc(hidden)]` so it doesn't appear in the public API surface.
#[doc(hidden)]
pub fn _reset_handler_stack_for_tests() {
    HANDLER_STACK.with(|s| s.borrow_mut().clear());
    CURRENT_BLOCK_CAPTURED.with(|c| c.borrow_mut().clear());
}

/// Drop every handler frame above `len`. Used by the cleanup-on-panic
/// path when the body unwinds before `nod_run_block` can pop them
/// individually.
fn truncate_handler_stack(len: usize) {
    HANDLER_STACK.with(|s| {
        let mut g = s.borrow_mut();
        if g.len() > len {
            g.truncate(len);
        }
    });
}

fn handler_stack_len() -> usize {
    HANDLER_STACK.with(|s| s.borrow().len())
}

/// Multi-line render of the handler stack for the `:handlers` driver
/// meta-command. Top of stack (most-recently-pushed; first checked by
/// `nod_signal`) prints first.
pub fn nod_walk_handlers_dump() -> String {
    let snapshot = handler_stack_snapshot();
    if snapshot.is_empty() {
        return "(no active handlers)\n".to_string();
    }
    let mut out = String::new();
    out.push_str(&format!("handler stack (depth = {})\n", snapshot.len()));
    for (i, f) in snapshot.iter().rev().enumerate() {
        out.push_str(&format!(
            "  [{}] class={} block-id={} handler-index={}\n",
            i, f.handler_class_name, f.target_block_id, f.handler_index
        ));
    }
    out
}

/// Phase F: structured introspection of the live handler chain. Returns
/// a string suitable for diagnostics and tests. Each line names the
/// handler class and its owning block-id; the most-recent frame
/// (innermost, first-checked) appears at the top.
///
/// This is the runtime helper the (future) REPL `:handlers` meta-command
/// will call. Sprint 19 ships the runtime API only — the driver-side
/// REPL command lands in a follow-up (see `DEFERRED.md`).
pub fn handlers_report() -> String {
    let snapshot = handler_stack_snapshot();
    if snapshot.is_empty() {
        return "handler chain: (empty)\n".to_string();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "handler chain (depth = {}, innermost first):\n",
        snapshot.len()
    ));
    for (i, f) in snapshot.iter().rev().enumerate() {
        out.push_str(&format!(
            "  #{i} class={} block-id={} handler-index={}\n",
            f.handler_class_name, f.target_block_id, f.handler_index
        ));
    }
    out
}

// ─── NLX payload + signal walking ──────────────────────────────────────────

/// The payload carried by `panic_any` during a non-local exit. The
/// outer `nod_run_block(block_id)` catches the panic, downcasts to
/// this type, and if `target_block_id == this_block_id` returns
/// `value`; otherwise re-panics.
#[derive(Debug)]
pub struct NlxPayload {
    pub target_block_id: u64,
    pub value: Word,
}

// ─── Block orchestration: function pointer registry ───────────────────────

/// A registered set of function pointers belonging to one `block` form.
/// Recorded by the lowering pass (as symbol names) and resolved post-JIT
/// to raw machine-code addresses via `register_block_jit_fns`.
///
/// All function pointers are `extern "C-unwind"` so a `panic_any` from
/// a callee can transit them up to the enclosing `nod_run_block`'s
/// `catch_unwind` frame.
#[derive(Copy, Clone)]
pub struct BlockFns {
    pub body: *const u8,
    pub cleanup: Option<*const u8>,
    pub afterwards: Option<*const u8>,
    /// One entry per `exception` clause, in source order.
    pub handlers: &'static [HandlerFn],
}

#[derive(Copy, Clone)]
pub struct HandlerFn {
    pub class_id: ClassId,
    pub class_name_ptr: *const u8, // pinned static string
    pub class_name_len: usize,
    pub body: *const u8,
}

// SAFETY: `BlockFns` carries raw fn pointers that point at JIT'd code
// pinned for the process lifetime; sharing them across threads is sound
// (the JIT engines are leaked forever and the code pages are exec/read
// from any thread).
unsafe impl Send for BlockFns {}
unsafe impl Sync for BlockFns {}
unsafe impl Send for HandlerFn {}
unsafe impl Sync for HandlerFn {}

static BLOCK_REGISTRY: Mutex<Vec<(u64, BlockFns)>> = Mutex::new(Vec::new());

/// Record a JIT-resolved set of block function pointers. The post-JIT
/// glue (in `nod-sema`) calls this once per `block` form in the
/// lowered module.
pub fn register_block_fns(block_id: u64, fns: BlockFns) {
    let mut g = BLOCK_REGISTRY.lock().expect("block registry poisoned");
    // Replace any existing entry with the same id (re-lowering tests).
    if let Some(slot) = g.iter_mut().find(|(id, _)| *id == block_id) {
        slot.1 = fns;
    } else {
        g.push((block_id, fns));
    }
}

fn lookup_block_fns(block_id: u64) -> Option<BlockFns> {
    let g = BLOCK_REGISTRY.lock().expect("block registry poisoned");
    g.iter().find(|(id, _)| *id == block_id).map(|(_, f)| *f)
}

/// Test helper: clear the block registry. Used by `#[serial]` tests
/// that exercise multiple `block` lowerings in the same process.
pub fn _reset_block_registry_for_tests() {
    let mut g = BLOCK_REGISTRY.lock().expect("block registry poisoned");
    g.clear();
}

// ─── nod_run_block orchestration ───────────────────────────────────────────
//
// Signature for the body / cleanup / afterwards / handler thunks:
//
//   extern "C-unwind" fn(captured0..captured7) -> u64       (body, cleanup, afterwards)
//   extern "C-unwind" fn(condition, captured0..captured7) -> u64  (handler)
//
// All take 8 captured-locals slots (zero-filled for unused ones), matching
// the fixed-arity convention used by `nod_make` / `nod_format_out`. If a
// `block` form captures more than 8 locals it errors at lowering time
// with a clear "Sprint 19 limitation" diagnostic.

pub const MAX_BLOCK_CAPTURED: usize = 8;

type ThunkFn = extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
type HandlerThunkFn =
    extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

struct CleanupGuard {
    cleanup: Option<*const u8>,
    captured: [u64; MAX_BLOCK_CAPTURED],
    /// `done` is flipped to true on normal exit so the Drop impl
    /// doesn't run cleanup a second time.
    done: bool,
    /// Handler-stack length at the moment we entered `nod_run_block` —
    /// any frames added by us (and not yet popped) get trimmed if the
    /// body panics through.
    handler_stack_baseline: usize,
    /// Active-JIT-safepoint depth at entry so that a re-raised unwind
    /// (NLX to an outer block, or non-NLX panic) clears stale entries
    /// from dead JIT stack frames before the cleanup thunk runs.
    jit_safepoint_baseline: usize,
    /// BUG 1 fix: the parallel AOT active-safepoint depth at entry. An
    /// NLX unwinding through AOT Dylan frames skips their
    /// `nod_aot_end_safepoint` epilogues, leaving stale entries on
    /// `aot::ACTIVE_AOT_SAFEPOINTS`; we truncate back to this baseline
    /// symmetrically with `jit_safepoint_baseline`. No-op in a pure-JIT
    /// process (AOT depth is 0 there).
    aot_safepoint_baseline: usize,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        // Restore JIT safepoint depth first: the cleanup thunk may
        // allocate, which can trigger GC.  At this point the JIT body
        // frames are gone, so their safepoint entries are stale.
        truncate_active_jit_safepoints(self.jit_safepoint_baseline);
        // BUG 1 fix: same for the parallel AOT active-safepoint stack —
        // an NLX unwinding through AOT frames left their begin-safepoint
        // entries dangling. No-op in a pure-JIT process.
        truncate_active_aot_safepoints(self.aot_safepoint_baseline);
        // Restore the handler-stack baseline; a re-raised panic must not
        // leave dangling frames pointing at this (defunct) block_id.
        truncate_handler_stack(self.handler_stack_baseline);
        if !self.done
            && let Some(cleanup) = self.cleanup
        {
            // SAFETY: cleanup is a JIT'd `extern "C-unwind" fn(u64*8) -> u64`
            // resolved post-JIT. We discard the return value.
            let c: ThunkFn = unsafe { std::mem::transmute(cleanup) };
            let _ = c(
                self.captured[0],
                self.captured[1],
                self.captured[2],
                self.captured[3],
                self.captured[4],
                self.captured[5],
                self.captured[6],
                self.captured[7],
            );
        }
    }
}

/// JIT-callable block orchestration shim. Invoked at every `block` site.
///
/// Layout of the `captured` array on the JIT side: lowering emits the
/// captured locals as positional args in source-declaration order;
/// zero-fills unused slots up to `MAX_BLOCK_CAPTURED`. The `exit_procedure`
/// for `block (k)` forms is allocated by lowering and prepended to the
/// captured slice — it occupies `captured[0]` and is passed to the body
/// thunk as the first captured local.
///
/// # Safety
///
/// `block_id` must have been registered (post-JIT) via
/// `register_block_fns`. Each captured Word must be a valid Dylan Word
/// (immediate or pointer-tagged). The returned Word is the block's
/// result.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_run_block(
    block_id: u64,
    c0: u64,
    c1: u64,
    c2: u64,
    c3: u64,
    c4: u64,
    c5: u64,
    c6: u64,
    c7: u64,
) -> u64 {
    let fns = match lookup_block_fns(block_id) {
        Some(f) => f,
        None => panic!("nod_run_block: block-id {block_id} not registered"),
    };

    let captured = [c0, c1, c2, c3, c4, c5, c6, c7];
    let baseline = handler_stack_len();
    let jit_safepoint_baseline = active_jit_safepoint_depth();
    // BUG 1 fix: capture the parallel AOT active-safepoint depth so an
    // NLX into (or through) this block can trim stale AOT entries left
    // by frames that skipped their `nod_aot_end_safepoint` epilogues.
    let aot_safepoint_baseline = active_aot_safepoint_depth();

    // Make the captured locals visible to any `nod_signal` invoked
    // inside the body or a handler — they need the same locals to run
    // the handler thunk.
    push_captured(captured);
    struct CapturedPopGuard;
    impl Drop for CapturedPopGuard {
        fn drop(&mut self) {
            pop_captured();
        }
    }
    let _captured_guard = CapturedPopGuard;

    // Push handler frames (innermost block's first; within the block,
    // source-order so the first clause is checked first on signal).
    for (idx, h) in fns.handlers.iter().enumerate() {
        // SAFETY: pinned class name in the static area.
        let name = unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                h.class_name_ptr,
                h.class_name_len,
            ))
        };
        nod_push_handler(HandlerFrame {
            handler_class: h.class_id,
            target_block_id: block_id,
            handler_index: idx as u32,
            handler_class_name: name.to_string(),
        });
    }

    // RAII cleanup — runs even if the body panics through with a
    // non-NLX payload, AND on normal exit (we flip `done=true` before
    // returning to skip the duplicate call).
    let mut guard = CleanupGuard {
        cleanup: fns.cleanup,
        captured,
        done: false,
        handler_stack_baseline: baseline,
        jit_safepoint_baseline,
        aot_safepoint_baseline,
    };

    let body: ThunkFn = unsafe { std::mem::transmute(fns.body) };

    // Run the body; catch NLX-marker panics targeting THIS block.
    let raw_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            body(c0, c1, c2, c3, c4, c5, c6, c7)
        }));

    let body_value = match raw_result {
        Ok(v) => v,
        Err(payload) => match downcast_nlx(payload) {
            Ok(nlx) if nlx.target_block_id == block_id => {
                truncate_active_jit_safepoints(jit_safepoint_baseline);
                // BUG 1 fix: trim stale AOT safepoint entries from the
                // dead Dylan frames the NLX unwound through. No-op in a
                // pure-JIT process.
                truncate_active_aot_safepoints(aot_safepoint_baseline);
                // NLX into this block. The handler (if a signal drove
                // us here) already produced `nlx.value` — return it.
                nlx.value.raw()
            }
            Ok(nlx) => {
                // NLX targeting an outer block. Re-raise.
                // CleanupGuard::drop() restores jit_safepoint_baseline
                // and runs the cleanup thunk as part of the unwind.
                std::panic::resume_unwind(Box::new(nlx))
            }
            Err(other) => {
                // Non-NLX panic. Re-raise. (Cleanup runs via Drop.)
                // CleanupGuard::drop() restores jit_safepoint_baseline.
                std::panic::resume_unwind(other)
            }
        },
    };

    // Run cleanup explicitly (normal-exit path). Flip the guard so its
    // Drop doesn't run it a second time.
    if let Some(cleanup) = fns.cleanup {
        // SAFETY: cleanup is JIT'd with the canonical signature.
        let c: ThunkFn = unsafe { std::mem::transmute(cleanup) };
        let _ = c(c0, c1, c2, c3, c4, c5, c6, c7);
    }
    guard.done = true;

    // Run afterwards.
    if let Some(after) = fns.afterwards {
        // SAFETY: same canonical signature.
        let a: ThunkFn = unsafe { std::mem::transmute(after) };
        let _ = a(c0, c1, c2, c3, c4, c5, c6, c7);
    }

    // Pop the handler frames we pushed (restore the baseline).
    truncate_handler_stack(baseline);

    body_value
}

fn downcast_nlx(payload: Box<dyn Any + Send>) -> Result<NlxPayload, Box<dyn Any + Send>> {
    match payload.downcast::<NlxPayload>() {
        Ok(b) => Ok(*b),
        Err(other) => Err(other),
    }
}

// ─── nod_signal: walk handlers + run handler thunk + NLX ──────────────────

/// JIT-callable `signal(condition)` shim. Walks the handler stack
/// most-recently-pushed first, finds the first frame matching the
/// condition's class (via `is_subclass`), invokes that frame's handler
/// thunk (which returns the handler-body's value), then `panic_any`s an
/// `NlxPayload` carrying that value to the matching block.
///
/// If no handler matches, panics with a structured message — the
/// process-level "unhandled condition" outcome.
///
/// # Safety
///
/// `condition_raw` must be a pointer-tagged Dylan Word for an instance
/// of `<condition>` (or a subclass). The shim diverges either via a
/// matching block's `catch_unwind` or via process-level panic.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_signal(condition_raw: u64) -> u64 {
    let condition = Word::from_raw(condition_raw);
    nod_signal_inner(condition)
}

fn nod_signal_inner(condition: Word) -> ! {
    let cond_class = match condition_class_of(condition) {
        Some(c) => c,
        None => {
            panic!(
                "signal: condition argument is not a heap-tagged Dylan object (raw = {:#x})",
                condition.raw()
            );
        }
    };

    // Search most-recent-first for a matching frame. We don't pop the
    // frame here — the enclosing `nod_run_block` pops in its truncate
    // step. Snapshot the stack so we can release the borrow before
    // dispatching the handler (the handler body may legitimately
    // signal again).
    let snapshot = handler_stack_snapshot();
    let matched = snapshot
        .iter()
        .rev()
        .find(|f| is_subclass(cond_class, f.handler_class));

    let Some(frame) = matched else {
        // Process-level outcome: unhandled condition. We render a
        // message that the headline acceptance test (panic case) can
        // grep for. The MANIFESTO's `--strict` mode would abort here
        // with a non-zero exit; in-process we panic so `catch_unwind`
        // in test scaffolding can observe it.
        let class_name = class_name_of(cond_class);
        let detail = unhandled_detail(condition, cond_class);
        if detail.is_empty() {
            panic!("unhandled signalled condition: {class_name}");
        } else {
            panic!("unhandled signalled condition: {class_name}: {detail}");
        }
    };

    let target_block_id = frame.target_block_id;
    let handler_index = frame.handler_index;

    // Look up the block's handler thunk by `(block_id, handler_index)`.
    let fns = match lookup_block_fns(target_block_id) {
        Some(f) => f,
        None => panic!(
            "signal: block-id {} matched a handler but isn't in the block registry",
            target_block_id
        ),
    };
    let handler_fn = fns
        .handlers
        .get(handler_index as usize)
        .copied()
        .expect("handler-index out of range vs. registered handlers");

    // Captured locals were closed over at the `block` form; we don't
    // have a way to recover them here without significant lowering work
    // — the Sprint 19 simplification is that handler bodies see the
    // captured locals via the same array we pass to the body thunk.
    // The orchestration shim doesn't have a snapshot of them on the
    // signal path because `nod_signal` is invoked deep inside the body
    // call. The simplification: handler thunks accept the same fixed
    // 8 captured-locals positions, but the block site populates them
    // and we read them through the BLOCK_CAPTURED_TLS thread-local
    // (set by `nod_run_block` before invoking the body, cleared
    // after).
    let captured = CURRENT_BLOCK_CAPTURED.with(|c| c.borrow().last().copied().unwrap_or_default());

    // SAFETY: handler_fn.body is a JIT'd `extern "C-unwind" fn(u64*9) -> u64`.
    let h: HandlerThunkFn = unsafe { std::mem::transmute(handler_fn.body) };
    let result = h(
        condition.raw(),
        captured[0],
        captured[1],
        captured[2],
        captured[3],
        captured[4],
        captured[5],
        captured[6],
        captured[7],
    );

    // NLX to the matching block carrying the handler's return value.
    std::panic::panic_any(NlxPayload {
        target_block_id,
        value: Word::from_raw(result),
    });
}

// Captured-locals slot for `nod_signal`'s handler invocation. A stack
// of `MAX_BLOCK_CAPTURED`-element arrays, pushed by `nod_run_block`
// before the body runs and popped on return.
thread_local! {
    static CURRENT_BLOCK_CAPTURED: RefCell<Vec<[u64; MAX_BLOCK_CAPTURED]>> =
        const { RefCell::new(Vec::new()) };
}

fn push_captured(captured: [u64; MAX_BLOCK_CAPTURED]) {
    CURRENT_BLOCK_CAPTURED.with(|c| c.borrow_mut().push(captured));
}

fn pop_captured() {
    CURRENT_BLOCK_CAPTURED.with(|c| {
        c.borrow_mut().pop();
    });
}

// Patch nod_run_block above? No — we need to re-thread `push_captured`
// into the orchestration shim. See the additional pass below.

fn condition_class_of(c: Word) -> Option<ClassId> {
    let p = c.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    Some(wrapper.class())
}

fn class_name_of(id: ClassId) -> String {
    let p = crate::class_metadata_ptr(id);
    if p.is_null() {
        return format!("<unknown:{}>", id.0);
    }
    // SAFETY: pointer is into the static area, stable.
    unsafe { (*p).name.clone() }
}

/// Render the relevant slots for the unhandled-condition panic message.
/// Sprint 19: pull `message` for `<simple-error>` and friends, pull
/// `generic-name` + `arg-class-names` for `<no-applicable-methods-error>`.
fn unhandled_detail(condition: Word, cond_class: ClassId) -> String {
    let no_appl = no_applicable_methods_error_class_id();
    let simple = simple_error_class_id();
    if is_subclass(cond_class, no_appl) {
        let nme_md = classes().no_applicable_methods_error_md;
        let gname = read_str_slot(condition, nme_md, "generic-name");
        let acn = read_str_slot(condition, nme_md, "arg-class-names");
        return format!(
            "no applicable method for `{}` on ({})",
            gname.as_deref().unwrap_or("?"),
            acn.as_deref().unwrap_or("?")
        );
    }
    if is_subclass(cond_class, simple) {
        let se_md = classes().simple_error_md;
        if let Some(msg) = read_str_slot(condition, se_md, "message") {
            return msg;
        }
    }
    String::new()
}

fn read_str_slot(c: Word, md: &ClassMetadata, slot: &str) -> Option<String> {
    let offset = md.slot_offset(slot)?;
    let p = c.as_ptr::<u8>()?;
    // SAFETY: caller asserts `c` is an instance of `md`'s class.
    let w = unsafe { *((p as usize + offset) as *const Word) };
    let bs = unsafe { crate::try_byte_string(w, crate::ClassId::BYTE_STRING) }?;
    // SAFETY: bs points at live <byte-string>.
    unsafe { bs.as_str() }.map(|s| s.to_string())
}

// ─── Exit procedures: `block (k) ... k(v) ... end` ─────────────────────────

/// Allocate a fresh `<exit-procedure>` Word carrying `block_id`.
pub fn make_exit_procedure(block_id: u64) -> Word {
    let md = classes().exit_procedure_md;
    // `block-id:` slot is typed `<integer>` so the value Word is a
    // tagged fixnum. Encode the u64 block_id as a fixnum (assumes the
    // ids fit in 63 bits, which they trivially do).
    let id_word =
        Word::from_fixnum(block_id as i64).expect("block ids fit in 63 bits");
    // SAFETY: registered metadata.
    unsafe { rust_make(md, &[("block-id", id_word)]) }
}

/// Read the `block-id` slot of an `<exit-procedure>`. Returns the raw
/// `u64` block id (untagged from the fixnum slot value).
pub fn exit_procedure_block_id(ep: Word) -> Option<u64> {
    let md = classes().exit_procedure_md;
    let cond_class = condition_class_of(ep)?;
    if cond_class != classes().exit_procedure {
        return None;
    }
    let offset = md.slot_offset("block-id")?;
    let p = ep.as_ptr::<u8>()?;
    // SAFETY: <exit-procedure> instance, slot offset is valid.
    let slot_word = unsafe { *((p as usize + offset) as *const Word) };
    let bid = slot_word.as_fixnum()? as u64;
    Some(bid)
}

/// JIT-callable `k(value)` invocation for exit procedures. Looks up
/// `k`'s block id and `panic_any`s an `NlxPayload` carrying `value`.
///
/// # Safety
///
/// `ep_raw` must be a pointer-tagged `<exit-procedure>` Word; `value`
/// is any Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_invoke_exit(ep_raw: u64, value: u64) -> u64 {
    let ep = Word::from_raw(ep_raw);
    let bid = exit_procedure_block_id(ep)
        .unwrap_or_else(|| panic!("nod_invoke_exit: not an <exit-procedure> Word"));
    std::panic::panic_any(NlxPayload {
        target_block_id: bid,
        value: Word::from_raw(value),
    });
}

/// JIT-callable wrapper for `make_exit_procedure`. The codegen-emitted
/// `%make-exit-procedure(block_id_word)` call site passes `block_id`
/// baked as a `WordBits` constant — i.e. the raw `u64` shifted/tagged
/// as it appears in the IR. We accept the raw `u64` directly because
/// the lowerer emits a `Const::WordBits(block_id as u64)`, NOT a tagged
/// fixnum. (Block ids are plain integers; we don't tag them.)
///
/// # Safety
///
/// Trivially safe; allocates a heap object and returns its Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_exit_procedure(block_id_raw: u64) -> u64 {
    make_exit_procedure(block_id_raw).raw()
}

/// JIT-callable `condition-message(c)` reader. Returns the slot value
/// as a `<byte-string>` Word.
///
/// # Safety
///
/// `c_raw` must be a pointer-tagged Word for a condition instance
/// laid out like `<simple-error>` (i.e. carrying a `message` slot).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_condition_message(c_raw: u64) -> u64 {
    let c = Word::from_raw(c_raw);
    condition_message(c).raw()
}

/// Dylan `error(msg)` — construct a `<simple-error>` from a
/// `<byte-string>` Word and signal it.  Diverges: either an enclosing
/// `block`/`exception` catches it (NLX), or the process panics with an
/// "unhandled error" message.
///
/// # Safety
///
/// `msg_raw` must be a pointer-tagged Dylan `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_error(msg_raw: u64) -> u64 {
    use crate::strings::ByteString;

    // Decode the <byte-string> to a Rust &str so we can call make_simple_error.
    let msg_word = Word::from_raw(msg_raw);
    let msg_str: std::borrow::Cow<str> = if let Some(p) = msg_word.as_ptr::<u8>() {
        // SAFETY: caller guarantees a valid heap-tagged <byte-string>.
        let bs = unsafe { &*(p as *const ByteString) };
        let bytes = unsafe { bs.bytes() };
        String::from_utf8_lossy(bytes)
    } else {
        std::borrow::Cow::Borrowed("<non-string error argument>")
    };
    let cond = make_simple_error(&msg_str);
    nod_signal_inner(cond)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    thread_local! {
        static TEST_INNER_SAFEPOINT_SLOT: std::cell::RefCell<[Word; 1]> =
            std::cell::RefCell::new([Word::from_raw(0)]);
    }

    extern "C-unwind" fn body_pushes_inner_safepoint_and_nlx(
        _c0: u64,
        _c1: u64,
        _c2: u64,
        _c3: u64,
        _c4: u64,
        _c5: u64,
        _c6: u64,
        _c7: u64,
    ) -> u64 {
        TEST_INNER_SAFEPOINT_SLOT.with(|slot| {
            let mut slot = slot.borrow_mut();
            slot[0] = Word::from_fixnum(22).expect("test fixnum in range");
            unsafe {
                crate::stack_map::nod_jit_begin_safepoint(0xAA, 8, slot.as_mut_ptr());
            }
        });
        std::panic::panic_any(NlxPayload {
            target_block_id: 42,
            value: Word::from_fixnum(77).expect("test fixnum in range"),
        })
    }

    #[test]
    fn condition_classes_register_with_expected_cpl() {
        ensure_registered();
        let err = error_class_id();
        let serious = serious_condition_class_id();
        let cond = condition_class_id();
        assert!(is_subclass(err, serious));
        assert!(is_subclass(err, cond));
        assert!(is_subclass(simple_error_class_id(), err));
    }

    #[test]
    fn make_simple_error_carries_message() {
        ensure_registered();
        let w = make_simple_error("hi");
        let msg = condition_message(w);
        // The message slot is a <byte-string> Word; decode it.
        let bs = unsafe {
            crate::try_byte_string(msg, crate::ClassId::BYTE_STRING)
                .expect("byte-string slot")
        };
        assert_eq!(unsafe { bs.as_str() }, Some("hi"));
    }

    #[test]
    fn handler_stack_push_pop_balanced() {
        // Snapshot baseline (other tests may have left frames behind on
        // this thread).
        let baseline = handler_stack_len();
        nod_push_handler(HandlerFrame {
            handler_class: error_class_id(),
            target_block_id: 42,
            handler_index: 0,
            handler_class_name: "<error>".to_string(),
        });
        assert_eq!(handler_stack_len(), baseline + 1);
        nod_pop_handler();
        assert_eq!(handler_stack_len(), baseline);
    }

    #[test]
    fn handler_dump_renders_frames() {
        let baseline_dump = nod_walk_handlers_dump();
        let baseline_len = handler_stack_len();
        nod_push_handler(HandlerFrame {
            handler_class: error_class_id(),
            target_block_id: 7,
            handler_index: 0,
            handler_class_name: "<error>".to_string(),
        });
        let dump = nod_walk_handlers_dump();
        assert!(dump.contains("<error>"), "{dump}");
        assert!(dump.contains("block-id=7"), "{dump}");
        nod_pop_handler();
        assert_eq!(handler_stack_len(), baseline_len);
        let _ = baseline_dump;
    }

    #[test]
    fn exit_procedure_roundtrips_block_id() {
        let ep = make_exit_procedure(99);
        assert_eq!(exit_procedure_block_id(ep), Some(99));
    }

    #[test]
    #[serial]
    fn nod_run_block_restores_jit_safepoint_baseline_on_nlx() {
        crate::stack_map::register_jit_safepoints(vec![
            crate::stack_map::JitSafepointEntry {
                namespace: 0xAA,
                site_id: 7,
                slots: vec![0],
            },
            crate::stack_map::JitSafepointEntry {
                namespace: 0xAA,
                site_id: 8,
                slots: vec![0],
            },
        ]);
        register_block_fns(
            42,
            BlockFns {
                body: body_pushes_inner_safepoint_and_nlx as *const () as *const u8,
                cleanup: None,
                afterwards: None,
                handlers: &[],
            },
        );

        let mut outer_slots = [Word::from_fixnum(11).expect("test fixnum in range")];
        unsafe {
            crate::stack_map::nod_jit_begin_safepoint(0xAA, 7, outer_slots.as_mut_ptr());
        }

        let result = unsafe { nod_run_block(42, 0, 0, 0, 0, 0, 0, 0, 0) };
        assert_eq!(Word::from_raw(result).as_fixnum(), Some(77));
        assert_eq!(crate::stack_map::active_jit_safepoint_depth(), 1);

        let roots = crate::stack_map::snapshot_active_jit_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(unsafe { (*roots[0]).as_fixnum() }, Some(11));

        crate::stack_map::nod_jit_end_safepoint(0xAA, 7);
        assert!(crate::stack_map::snapshot_active_jit_roots().is_empty());
    }

    // ── Signal-driven NLX test ────────────────────────────────────────────
    //
    // Exercises the full `nod_signal` → handler thunk → NLX path.  Unlike
    // the test above (which uses a bare `panic_any`), here the unwind is
    // initiated by `nod_signal`; the handler returns a value and
    // `nod_signal_inner` drives the NLX.  The inner safepoint is opened
    // inside the body but never explicitly closed (signal diverges before
    // the matching `nod_jit_end_safepoint`).  After `nod_run_block`
    // returns, the stale inner entry must be gone and the outer entry
    // must survive intact.

    thread_local! {
        static TEST_SIGNAL_INNER_SLOT: std::cell::RefCell<[Word; 1]> =
            std::cell::RefCell::new([Word::from_raw(0)]);
    }

    extern "C-unwind" fn signal_driven_body(
        _c0: u64,
        _c1: u64,
        _c2: u64,
        _c3: u64,
        _c4: u64,
        _c5: u64,
        _c6: u64,
        _c7: u64,
    ) -> u64 {
        // Open an inner safepoint that will be left dangling when
        // nod_signal diverges.
        TEST_SIGNAL_INNER_SLOT.with(|slot| {
            let mut s = slot.borrow_mut();
            s[0] = Word::from_fixnum(55).expect("fixnum 55");
            let ptr = s.as_mut_ptr();
            drop(s); // release borrow before raw-pointer hand-off
            unsafe {
                crate::stack_map::nod_jit_begin_safepoint(0xCC, 20, ptr);
            }
        });
        let cond = make_simple_error("signal-driven safepoint test");
        // nod_signal never returns — it raises an NlxPayload.
        unsafe { nod_signal(cond.raw()) }
    }

    extern "C-unwind" fn signal_driven_handler(
        _condition: u64,
        _c0: u64,
        _c1: u64,
        _c2: u64,
        _c3: u64,
        _c4: u64,
        _c5: u64,
        _c6: u64,
        _c7: u64,
    ) -> u64 {
        Word::from_fixnum(33).expect("fixnum 33").raw()
    }

    #[test]
    #[serial]
    fn nod_run_block_restores_safepoints_on_signal_driven_nlx() {
        ensure_registered();
        // Start from a known-clean active-safepoint stack on this thread.
        crate::stack_map::truncate_active_jit_safepoints(0);
        _reset_handler_stack_for_tests();

        // Register the safepoint sites used by this test (distinct
        // namespace 0xCC avoids collision with the test above).
        crate::stack_map::register_jit_safepoints(vec![
            crate::stack_map::JitSafepointEntry {
                namespace: 0xCC,
                site_id: 19, // outer, opened before nod_run_block
                slots: vec![0],
            },
            crate::stack_map::JitSafepointEntry {
                namespace: 0xCC,
                site_id: 20, // inner, opened inside body and left dangling
                slots: vec![0],
            },
        ]);

        // Build the HandlerFn.  We need a &'static slice; Box::leak is
        // sound here because the pointer lives for the process lifetime.
        let handlers: &'static [HandlerFn] = Box::leak(Box::new([HandlerFn {
            class_id: error_class_id(),
            class_name_ptr: b"<error>".as_ptr(),
            class_name_len: 7,
            body: signal_driven_handler as *const () as *const u8,
        }]));

        register_block_fns(
            55,
            BlockFns {
                body: signal_driven_body as *const () as *const u8,
                cleanup: None,
                afterwards: None,
                handlers,
            },
        );

        // Open the outer safepoint around the nod_run_block call.
        let mut outer_slot = [Word::from_fixnum(77).expect("fixnum 77")];
        unsafe {
            crate::stack_map::nod_jit_begin_safepoint(0xCC, 19, outer_slot.as_mut_ptr());
        }

        let result = unsafe { nod_run_block(55, 0, 0, 0, 0, 0, 0, 0, 0) };
        assert_eq!(
            Word::from_raw(result).as_fixnum(),
            Some(33),
            "handler return value"
        );

        // The inner safepoint (site 20) was open when nod_signal fired;
        // nod_run_block must have truncated it.
        assert_eq!(
            crate::stack_map::active_jit_safepoint_depth(),
            1,
            "inner stale safepoint must be truncated"
        );
        let roots = crate::stack_map::snapshot_active_jit_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(
            unsafe { (*roots[0]).as_fixnum() },
            Some(77),
            "outer root preserved"
        );

        crate::stack_map::nod_jit_end_safepoint(0xCC, 19);
        assert!(crate::stack_map::snapshot_active_jit_roots().is_empty());
    }
}
