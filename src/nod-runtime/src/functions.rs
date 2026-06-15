//! Sprint 21 — first-class function values.
//!
//! This module owns:
//!
//!   1. **The `<function>` class** — a heap-tagged Word whose slots
//!      carry the function-pointer + arity descriptor + bookkeeping.
//!      Registered idempotently at process boot via
//!      `register_simple_user_class("<function>", None, …)`.
//!
//!   2. **`<wrong-number-of-arguments-error>`** — signalled from the
//!      `nod_funcall_N` extern shims when the descriptor's arity
//!      doesn't match the call shape. Parent `<error>`.
//!
//!   3. **`nod_funcall0` / `nod_funcall1` / `nod_funcall2` /
//!      `nod_funcall3` / `nod_funcall4` / `nod_funcall5`** — JIT-callable
//!      trampolines that pull the function-pointer out of the
//!      `<function>` Word and tail-call to it. The `<function>` ABI for
//!      callees is the same `extern "C-unwind" fn(u64, …) -> u64` shape
//!      that the rest of the JIT uses; the trampoline just transmutes
//!      and calls. Sprint 26 lifted the lower bound to 0 and the upper
//!      bound on direct funcall (without going through `nod_apply`) to
//!      5; higher arities continue to route through `nod_apply`.
//!
//!   4. **`nod_apply`** — variadic dispatch: an args-vector
//!      (`<simple-object-vector>` containing tagged Words) unpacks up to
//!      `MAX_APPLY_ARITY` positional arguments. Sprint 21 caps at 8 —
//!      see DEFERRED.md for higher-arity follow-up.
//!
//! ## Slot layout
//!
//! ```text
//! <function>
//!   name        : <byte-string>   (diagnostics; `\+` -> "+", etc.)
//!   arity       : <integer>       (Sprint 21: fixed arity only)
//!   code-ptr    : <integer>       (RAW host pointer — NOT a Dylan Word)
//!   kind-tag    : <integer>       (0 = top-level, 1 = lifted anon)
//!   env-ptr     : <integer>       (Sprint 21: always 0; closures land Sprint 24)
//!   return-type : <integer>       (encoded TypeEstimate; 0 for now)
//! ```
//!
//! ## Slot typing
//!
//! Sprint 21 typed every slot as `SlotType::Integer` because every
//! `<function>` instance lived in the static area and the `name` slot
//! held a static-area-pinned `<byte-string>` — the GC could safely
//! skip the entire object.
//!
//! Sprint 24 introduces **closure `<function>` instances** that live in
//! the moveable heap with an `env-ptr` slot pointing at an
//! `<environment>` (also moveable). The GC must follow the env-ptr to
//! relocate the environment when it evacuates.
//!
//! The slot retypings:
//!
//!   * `env-ptr` → `SlotType::Object` (pointer-shaped). For top-level
//!     `<function>`s the slot value is `Word::from_raw(0)` — fixnum
//!     zero — which the GC's classify shunt correctly skips. For
//!     closures the slot holds the env's pointer-tagged Word.
//!   * `name` → `SlotType::String`. Always pointer-tagged
//!     `<byte-string>`, sometimes static-area-pinned (top-level),
//!     sometimes pointing at a heap-allocated literal. The GC trivially
//!     handles both via the page-of-reservation gate.
//!
//! The remaining slots (`arity`, `code-ptr`, `kind-tag`, `return-type`)
//! stay `Integer`-typed — they encode 64-bit-bit-pattern host values,
//! not tagged Words.

use std::sync::{Mutex, OnceLock};

use crate::classes::{
    ClassId, ClassMetadata, SlotDefault, SlotInfo, SlotType, class_metadata_for, is_subclass,
};
use crate::make::rust_make;
use crate::word::Word;

/// Sprint 21 cap on the number of positional arguments `nod_apply` will
/// unpack from its arg-vector. Larger applies error with a clear
/// diagnostic; see DEFERRED.md for the lift-the-cap follow-up.
pub const MAX_APPLY_ARITY: usize = 8;

/// `<function>` kind-tag values. The slot is sourced from
/// `make_function`'s `kind_tag` parameter and read back by
/// `function_kind_tag` for dispatch decisions inside `nod_funcall_N`.
///
/// The dispatch is staged: kind-tag is checked FIRST so generic
/// trampolines (which carry a raw `&'static GenericFunction` pointer in
/// `env-ptr`) don't get misinterpreted as closures with a tagged
/// `<environment>` env-ptr.
pub const FUNCTION_KIND_TOP_LEVEL: u32 = 0;
pub const FUNCTION_KIND_LIFTED_ANON: u32 = 1;
pub const FUNCTION_KIND_CLOSURE: u32 = 2;
/// Sprint 26: generic-dispatch trampoline. `env-ptr` carries a raw
/// `*const GenericFunction` (not a Dylan Word); the trampoline reads it
/// when dispatched, walks the applicable-method chain, and tail-calls
/// the winner. Lets `\generic-name` route to the right method body at
/// call time instead of baking one specific method's address in.
pub const FUNCTION_KIND_GENERIC_TRAMPOLINE: u32 = 3;

struct FunctionClassIds {
    function: ClassId,
    wrong_args: ClassId,
    function_md: &'static ClassMetadata,
    wrong_args_md: &'static ClassMetadata,
}

static FUNCTION_CLASSES: OnceLock<FunctionClassIds> = OnceLock::new();

/// Register `<function>` and `<wrong-number-of-arguments-error>`
/// idempotently. Safe to call repeatedly. The condition seed classes
/// are registered first because `<wrong-number-of-arguments-error>`
/// inherits from `<error>`.
///
/// Also installs the built-in operator shims (`+`, `-`, `*`, `=`,
/// `<`, `>`) into the function registry so user code can pass `\+`
/// etc. as first-class values.
pub fn ensure_registered() {
    ensure_operator_shims_registered();
    let _ = FUNCTION_CLASSES.get_or_init(|| {
        crate::conditions::ensure_registered();
        // Sprint 21: parent = `<object>` so `is_subclass(<function>,
        // <object>)` holds. Stdlib methods registered as
        // `(c :: <object>)` need this for the dispatcher's
        // applicability check.
        let (function, _) = crate::register_simple_user_class(
            "<function>",
            Some(ClassId::OBJECT),
            vec![
                // `name` and `env-ptr` are pointer-shaped (Sprint 24)
                // — the GC must trace them. The other slots are opaque
                // 64-bit-bit-pattern values (host pointers / fixnums).
                slot_pointer("name", "function-name", SlotType::String),
                slot_int("arity", "arity"),
                slot_int("code-ptr", "code-ptr"),
                slot_int("kind-tag", "kind-tag"),
                slot_pointer("env-ptr", "env-ptr", SlotType::Object),
                slot_int("return-type", "return-type"),
            ],
        );
        let function_md = class_metadata_for(function);

        let error = crate::conditions::error_class_id();
        let (wrong_args, _) = crate::register_simple_user_class(
            "<wrong-number-of-arguments-error>",
            Some(error),
            vec![
                slot_int("function", "function"),
                slot_int("expected", "expected"),
                slot_int("got", "got"),
            ],
        );
        let wrong_args_md = class_metadata_for(wrong_args);

        FunctionClassIds {
            function,
            wrong_args,
            function_md,
            wrong_args_md,
        }
    });
}

fn slot_int(name: &str, init_kw: &str) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        // Integer-typed — the GC scanner skips these slots. Used for
        // host-pointer / raw-bit-pattern slots that aren't tagged Dylan
        // Words (`arity`, `code-ptr`, `kind-tag`, `return-type`).
        type_kind: SlotType::Integer,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

/// Sprint 24: pointer-shaped slot — the GC scans the slot on every
/// collection. Used for `name` (a `<byte-string>` pointer) and
/// `env-ptr` (an `<environment>` pointer or zero for non-closures).
fn slot_pointer(name: &str, init_kw: &str, kind: SlotType) -> SlotInfo {
    SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: kind,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: SlotDefault::Unbound,
        has_setter: false,
    }
}

fn classes() -> &'static FunctionClassIds {
    ensure_registered();
    FUNCTION_CLASSES
        .get()
        .expect("function classes registered")
}

pub fn function_class_id() -> ClassId {
    classes().function
}

pub fn wrong_number_of_arguments_error_class_id() -> ClassId {
    classes().wrong_args
}

// ─── Builders ──────────────────────────────────────────────────────────────

/// Allocate a `<function>` instance carrying the supplied descriptor.
/// `code_ptr` is the raw host address of an
/// `extern "C-unwind" fn(u64, ..., u64) -> u64` (or compatible) — the
/// trampoline transmutes to the right signature based on `arity`.
///
/// Sprint 21: `env_ptr` is unused (always pass 0). Closures with
/// captured environments land in Sprint 24.
pub fn make_function(
    name: &str,
    arity: usize,
    code_ptr: *const u8,
    kind_tag: u32,
    env_ptr: u64,
) -> Word {
    let md = classes().function_md;
    let name_word = crate::intern_string_literal(name);
    // The code-ptr / env-ptr / arity / kind-tag slots all expect a
    // tagged-Word value. We pack as fixnums where they fit (arity,
    // kind-tag, return-type) and as raw `WordBits`-tagged opaque
    // integers for the code-ptr / env-ptr (host pointers are arbitrary
    // 64-bit values that may not fit in a 63-bit fixnum).
    //
    // Sprint 21 simplification: we treat ALL six slots as opaque
    // 64-bit-bit-pattern values. `nod_make`'s slot-store path writes
    // the supplied Word verbatim into the slot — it doesn't
    // re-tag — so we hand it the raw bit pattern wrapped as a
    // `Word::from_raw`. The readers below pull the bits back out via
    // the same `Word::raw()` accessor.
    let arity_w = Word::from_raw(arity as u64);
    let code_w = Word::from_raw(code_ptr as u64);
    let kind_w = Word::from_raw(kind_tag as u64);
    let env_w = Word::from_raw(env_ptr);
    let ret_w = Word::from_raw(0);
    // SAFETY: registered metadata; init keyword names match the slot
    // names registered in `ensure_registered`.
    unsafe {
        rust_make(
            md,
            &[
                ("function-name", name_word),
                ("arity", arity_w),
                ("code-ptr", code_w),
                ("kind-tag", kind_w),
                ("env-ptr", env_w),
                ("return-type", ret_w),
            ],
        )
    }
}

/// Read the `code-ptr` slot from a `<function>` Word. Returns `None` if
/// the Word isn't pointer-tagged (the caller is expected to have
/// type-checked already; this is the defensive read for the
/// trampoline path).
pub fn function_code_ptr(f: Word) -> Option<*const u8> {
    let md = classes().function_md;
    let p = f.as_ptr::<u8>()?;
    let offset = md.slot_offset("code-ptr")?;
    // SAFETY: caller asserts `f` points at a `<function>` instance.
    // Slot offset is bounded by the class's instance size.
    let raw = unsafe { *((p as usize + offset) as *const u64) };
    Some(raw as *const u8)
}

/// Read the `arity` slot from a `<function>` Word.
pub fn function_arity(f: Word) -> Option<usize> {
    let md = classes().function_md;
    let p = f.as_ptr::<u8>()?;
    let offset = md.slot_offset("arity")?;
    // SAFETY: same as `function_code_ptr`.
    let raw = unsafe { *((p as usize + offset) as *const u64) };
    Some(raw as usize)
}

/// Sprint 26: read the `kind-tag` slot from a `<function>` Word.
///
///   * `0` — top-level non-closure function.
///   * `1` — lifted anonymous (legacy; created by the Sprint 21 lifter
///     before closures landed).
///   * `2` — closure built by `nod_make_closure` (env-ptr points at a
///     real `<environment>`).
///   * `3` — generic-dispatch trampoline (Sprint 26). The `env-ptr`
///     slot then carries a raw `&'static GenericFunction` pointer (NOT
///     a Dylan Word), and `code-ptr` points at one of the per-arity
///     `nod_generic_dispatch_trampoline_N` shims.
pub fn function_kind_tag(f: Word) -> Option<u32> {
    let md = classes().function_md;
    let p = f.as_ptr::<u8>()?;
    let offset = md.slot_offset("kind-tag")?;
    // SAFETY: same as `function_code_ptr`.
    let raw = unsafe { *((p as usize + offset) as *const u64) };
    Some(raw as u32)
}

/// Sprint 24: read the `env-ptr` slot from a `<function>` Word. Returns
/// `0` for top-level (non-closure) functions; a non-zero `u64` is a
/// raw `Word::raw()` whose pointer payload identifies the closure's
/// `<environment>` instance.
pub fn function_env_ptr(f: Word) -> Option<u64> {
    let md = classes().function_md;
    let p = f.as_ptr::<u8>()?;
    let offset = md.slot_offset("env-ptr")?;
    // SAFETY: same as `function_code_ptr`. Slot stores a tagged Word
    // (zero for non-closures, pointer-tagged <environment> for closures).
    let raw = unsafe { *((p as usize + offset) as *const u64) };
    Some(raw)
}

/// Read the `name` slot of a `<function>` instance as a Rust `String`.
/// Used by the wrong-number-of-arguments diagnostic and tests.
pub fn function_name(f: Word) -> Option<String> {
    let md = classes().function_md;
    let p = f.as_ptr::<u8>()?;
    let offset = md.slot_offset("name")?;
    // SAFETY: `name` slot stores a pointer-tagged <byte-string> Word.
    let name_word = unsafe { *((p as usize + offset) as *const Word) };
    let bs = unsafe { crate::try_byte_string(name_word, ClassId::BYTE_STRING) }?;
    // SAFETY: bs points at a live <byte-string>.
    unsafe { bs.as_str() }.map(|s| s.to_string())
}

/// True iff `w` is pointer-tagged and its wrapper class is
/// `<function>` (or a subclass — Sprint 21 has none, but the check
/// generalises).
pub fn is_function(w: Word) -> bool {
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    is_subclass(wrapper.class(), classes().function)
}

/// Build a `<wrong-number-of-arguments-error>` instance carrying the
/// supplied function Word, the expected arity, and the actual arity.
pub fn make_wrong_number_of_arguments_error(
    function: Word,
    expected: usize,
    got: usize,
) -> Word {
    let md = classes().wrong_args_md;
    // SAFETY: registered metadata; init keyword names match.
    unsafe {
        rust_make(
            md,
            &[
                ("function", function),
                (
                    "expected",
                    Word::from_fixnum(expected as i64).unwrap_or(Word::from_raw(0)),
                ),
                (
                    "got",
                    Word::from_fixnum(got as i64).unwrap_or(Word::from_raw(0)),
                ),
            ],
        )
    }
}

// ─── Trampoline externs ───────────────────────────────────────────────────

type Arity0Fn = extern "C-unwind" fn() -> u64;
type Arity1Fn = extern "C-unwind" fn(u64) -> u64;
type Arity2Fn = extern "C-unwind" fn(u64, u64) -> u64;
type Arity3Fn = extern "C-unwind" fn(u64, u64, u64) -> u64;
type Arity4Fn = extern "C-unwind" fn(u64, u64, u64, u64) -> u64;
type Arity5Fn = extern "C-unwind" fn(u64, u64, u64, u64, u64) -> u64;
type Arity6Fn = extern "C-unwind" fn(u64, u64, u64, u64, u64, u64) -> u64;
type Arity7Fn = extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64) -> u64;
type Arity8Fn = extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

/// Common arity check + dispatch error. Diverges if the function's
/// arity doesn't match `expected`.
fn check_arity_or_signal(f: Word, expected: usize) -> usize {
    let arity = function_arity(f).unwrap_or_else(|| {
        panic!(
            "nod_funcall*: argument is not a <function> Word (raw = {:#x})",
            f.raw()
        );
    });
    if arity != expected {
        let cond = make_wrong_number_of_arguments_error(f, expected, arity);
        // Diverges via `nod_signal`'s NLX path; if no handler matches,
        // panics with the unhandled-condition message. The return value
        // is never observed.
        // SAFETY: cond is a freshly-allocated condition Word.
        let _ = unsafe { crate::conditions::nod_signal(cond.raw()) };
    }
    arity
}

fn code_ptr_or_panic(f: Word) -> *const u8 {
    function_code_ptr(f).unwrap_or_else(|| {
        panic!(
            "nod_funcall*: argument is not a <function> Word (raw = {:#x})",
            f.raw()
        );
    })
}

/// Internal: dispatch `f` as a generic-trampoline `<function>` Word.
/// Reads the `env-ptr` slot (which carries a raw
/// `*const crate::dispatch::GenericFunction` — see
/// `FUNCTION_KIND_GENERIC_TRAMPOLINE`), looks up the most-specific
/// applicable method for the argument classes, and tail-calls it via
/// the dispatch crate's `call_method`. Returns the method's result.
///
/// Diverges via `<no-applicable-methods-error>` if no method matches.
///
/// # Safety
///
/// `f` must be a pointer-tagged `<function>` Word with
/// `kind-tag = FUNCTION_KIND_GENERIC_TRAMPOLINE`. Each `args[i]` is a
/// valid Dylan Word. `args.len()` must be `<= 8`.
unsafe fn dispatch_via_generic_trampoline(f: Word, args: &[u64]) -> u64 {
    let env_raw = function_env_ptr(f).unwrap_or(0);
    // The `env-ptr` slot stores `&'static GenericFunction as u64`. A
    // null pointer here would be a registration bug; surface it as a
    // panic rather than silently misbehaving.
    if env_raw == 0 {
        panic!(
            "generic-trampoline <function> Word at {:#x} has null env-ptr — registration is broken",
            f.raw()
        );
    }
    // Pad args out to 8 (the `nod_dispatch` slot count). Unused slots
    // are ignored when `arity < 8`.
    let mut padded = [0u64; 8];
    for (i, &a) in args.iter().enumerate().take(8) {
        padded[i] = a;
    }
    // SAFETY: generic-ptr is a `&'static GenericFunction`; cache-slot
    // is `0` (no inline cache for the trampoline path — Sprint 26 keeps
    // it simple); arity matches the args length the lowerer emitted;
    // each padded[i] is a valid Word.
    unsafe {
        crate::dispatch::nod_dispatch(
            env_raw,
            0,
            args.len() as u64,
            padded[0],
            padded[1],
            padded[2],
            padded[3],
            padded[4],
            padded[5],
            padded[6],
            padded[7],
        )
    }
}

/// `nod_funcall0(f) -> r` — invoke `f` with zero args.
///
/// Sprint 26: completes the symmetry that Sprint 21 set up for arities
/// 1 and 2. Like `nod_funcall1`, dispatches on the `<function>`'s
/// `env-ptr` slot to pick between an env-less callee body
/// (`fn() -> u64`) and a closure body (`fn(env) -> u64`, where the env
/// is the synthetic first argument).
///
/// `<exit-procedure>` is intentionally NOT accepted here — escape
/// procedures always take exactly one value to throw, so an arity-0
/// invoke of one would be ill-typed at the source level. The arity
/// check below diverges via `<wrong-number-of-arguments-error>` if a
/// non-`<function>` Word ends up here.
///
/// # Safety
///
/// `f_raw` must be a pointer-tagged Dylan `<function>` Word. Its
/// `code-ptr` must point at an `extern "C-unwind" fn() -> u64` (no env)
/// or `extern "C-unwind" fn(u64) -> u64` (with env) per the env-ptr
/// slot.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall0(f_raw: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    let _ = check_arity_or_signal(f, 0);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-0 ABI.
        let f0: Arity0Fn = unsafe { std::mem::transmute(code) };
        f0()
    } else {
        // SAFETY: closure body, callee matches (env,) ABI.
        let f1: Arity1Fn = unsafe { std::mem::transmute(code) };
        f1(env_ptr)
    }
}

/// `nod_funcall1(f, a) -> r` — invoke `f` with one arg.
///
/// Sprint 21 also accepts `<exit-procedure>` Words and routes them
/// to `nod_invoke_exit` so that lifted-thunk env-bound names work
/// uniformly: the same lowering path drives `\foo(x)` AND
/// `block (k) ... k(v) ... end`.
///
/// Sprint 24: dispatches on the `<function>`'s `env-ptr` slot. A zero
/// env-ptr means a plain top-level function whose body's actual
/// signature is `fn(u64) -> u64`. A non-zero env-ptr means a closure
/// whose body's actual signature is `fn(env_word, u64) -> u64` — the
/// environment is passed as the synthetic first argument that the
/// lowerer threads through.
///
/// # Safety
///
/// `f_raw` must be a pointer-tagged Dylan Word; `a` is any Dylan Word.
/// If `f` is a `<function>`, its `code-ptr` must point at an
/// `extern "C-unwind" fn(u64) -> u64` (no env) or
/// `extern "C-unwind" fn(u64, u64) -> u64` (with env) per the env-ptr
/// slot. If `f` is an `<exit-procedure>`, the call diverges via NLX.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall1(f_raw: u64, a: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    // Dispatch on the class. `<exit-procedure>` routes through
    // `nod_invoke_exit`; everything else expects `<function>`.
    if crate::conditions::exit_procedure_block_id(f).is_some() {
        // SAFETY: f is an <exit-procedure> Word; nod_invoke_exit
        // diverges.
        return unsafe { crate::conditions::nod_invoke_exit(f_raw, a) };
    }
    let _ = check_arity_or_signal(f, 1);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[a]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-1 ABI.
        let f1: Arity1Fn = unsafe { std::mem::transmute(code) };
        f1(a)
    } else {
        // SAFETY: closure body, callee matches (env, arity-1) ABI.
        let f2: Arity2Fn = unsafe { std::mem::transmute(code) };
        f2(env_ptr, a)
    }
}

/// `nod_funcall2(f, a, b) -> r` — invoke `f` with two args.
///
/// Sprint 24: as `nod_funcall1`, dispatches on the `env-ptr` slot to
/// pick between `(u64, u64) -> u64` (no env) and
/// `(env, u64, u64) -> u64` (closure) body signatures.
///
/// # Safety
///
/// See `nod_funcall1`; callee must match arity-2 ABI (no env) or
/// arity-3 ABI (with env).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall2(f_raw: u64, a: u64, b: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    let _ = check_arity_or_signal(f, 2);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[a, b]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-2 ABI.
        let f2: Arity2Fn = unsafe { std::mem::transmute(code) };
        f2(a, b)
    } else {
        // SAFETY: closure body, callee matches (env, arity-2) ABI.
        let f3: Arity3Fn = unsafe { std::mem::transmute(code) };
        f3(env_ptr, a, b)
    }
}

/// `nod_funcall3(f, a, b, c) -> r` — invoke `f` with three args.
///
/// Sprint 26: extends the direct-funcall family up to arity 5 so the
/// JIT can emit a clean single call instead of having to pack args into
/// a `<simple-object-vector>` and route through `nod_apply`. Higher
/// arities (6+) continue to go through `nod_apply`.
///
/// # Safety
///
/// See `nod_funcall1`; callee must match arity-3 ABI (no env) or
/// arity-4 ABI (with env).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall3(f_raw: u64, a: u64, b: u64, c: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    let _ = check_arity_or_signal(f, 3);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[a, b, c]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-3 ABI.
        let f3: Arity3Fn = unsafe { std::mem::transmute(code) };
        f3(a, b, c)
    } else {
        // SAFETY: closure body, callee matches (env, arity-3) ABI.
        let f4: Arity4Fn = unsafe { std::mem::transmute(code) };
        f4(env_ptr, a, b, c)
    }
}

/// `nod_funcall4(f, a, b, c, d) -> r` — invoke `f` with four args.
///
/// # Safety
///
/// See `nod_funcall1`; callee must match arity-4 ABI (no env) or
/// arity-5 ABI (with env).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall4(f_raw: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    let _ = check_arity_or_signal(f, 4);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[a, b, c, d]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-4 ABI.
        let f4: Arity4Fn = unsafe { std::mem::transmute(code) };
        f4(a, b, c, d)
    } else {
        // SAFETY: closure body, callee matches (env, arity-4) ABI.
        let f5: Arity5Fn = unsafe { std::mem::transmute(code) };
        f5(env_ptr, a, b, c, d)
    }
}

/// `nod_funcall5(f, a, b, c, d, e) -> r` — invoke `f` with five args.
///
/// # Safety
///
/// See `nod_funcall1`; callee must match arity-5 ABI (no env) or
/// arity-6 ABI (with env).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_funcall5(
    f_raw: u64,
    a: u64,
    b: u64,
    c: u64,
    d: u64,
    e: u64,
) -> u64 {
    let f = Word::from_raw(f_raw);
    let _ = check_arity_or_signal(f, 5);
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &[a, b, c, d, e]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    if env_ptr == 0 {
        // SAFETY: env-less function, callee matches arity-5 ABI.
        let f5: Arity5Fn = unsafe { std::mem::transmute(code) };
        f5(a, b, c, d, e)
    } else {
        // SAFETY: closure body, callee matches (env, arity-5) ABI.
        let f6: Arity6Fn = unsafe { std::mem::transmute(code) };
        f6(env_ptr, a, b, c, d, e)
    }
}

/// `nod_apply(f, args_vector) -> r` — variadic dispatch via a
/// `<simple-object-vector>` of tagged Words. Sprint 21 caps the args
/// at `MAX_APPLY_ARITY` (8); higher-arity applies signal a
/// wrong-number-of-arguments condition.
///
/// # Safety
///
/// `f_raw` must be a pointer-tagged `<function>` Word; `args_raw` must
/// be a pointer-tagged `<simple-object-vector>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_apply(f_raw: u64, args_raw: u64) -> u64 {
    let f = Word::from_raw(f_raw);
    let args = Word::from_raw(args_raw);
    let sov = unsafe { crate::try_simple_object_vector(args, ClassId::SIMPLE_OBJECT_VECTOR) }
        .unwrap_or_else(|| {
            panic!(
                "nod_apply: args is not a <simple-object-vector> Word (raw = {:#x})",
                args_raw
            );
        });
    let n = sov.len as usize;
    let arity = function_arity(f).unwrap_or_else(|| {
        panic!(
            "nod_apply: function is not a <function> Word (raw = {:#x})",
            f_raw
        );
    });
    if n != arity {
        let cond = make_wrong_number_of_arguments_error(f, arity, n);
        // SAFETY: cond is a freshly-allocated condition Word; nod_signal
        // diverges, so the return is never observed.
        let _ = unsafe { crate::conditions::nod_signal(cond.raw()) };
    }
    if n > MAX_APPLY_ARITY {
        panic!(
            "nod_apply: Sprint 21 supports up to {MAX_APPLY_ARITY} args, got {n}; \
             higher-arity apply is a Sprint 22+ follow-up"
        );
    }
    // SAFETY: sov has at least `n` element slots; reading each as a u64
    // matches the `<simple-object-vector>` element layout (tagged Word
    // per slot).
    let mut a = [0u64; MAX_APPLY_ARITY];
    let slots = unsafe { sov.slots() };
    for i in 0..n {
        a[i] = slots[i].raw();
    }
    // Sprint 26: generic-trampoline `<function>` Words dispatch by
    // class at call time. Reaching this before reading the raw
    // code-ptr matters because the trampoline's `code-ptr` slot is
    // *unused* (set to null at registration), so a transmute below
    // would crash.
    if function_kind_tag(f) == Some(FUNCTION_KIND_GENERIC_TRAMPOLINE) {
        // SAFETY: kind tag was verified.
        return unsafe { dispatch_via_generic_trampoline(f, &a[..n]) };
    }
    let code = code_ptr_or_panic(f);
    let env_ptr = function_env_ptr(f).unwrap_or(0);
    // Sprint 24: closures get an env-pointer threaded as the synthetic
    // first arg. Plain top-level functions get the args verbatim.
    //
    // SAFETY: arity-N callee ABI; we already verified `n == arity` and
    // `n <= MAX_APPLY_ARITY`. The env-ptr branch transmutes to a
    // signature one wider than the closure-less branch.
    unsafe {
        if env_ptr == 0 {
            match n {
                0 => (std::mem::transmute::<*const u8, Arity0Fn>(code))(),
                1 => (std::mem::transmute::<*const u8, Arity1Fn>(code))(a[0]),
                2 => (std::mem::transmute::<*const u8, Arity2Fn>(code))(a[0], a[1]),
                3 => (std::mem::transmute::<*const u8, Arity3Fn>(code))(a[0], a[1], a[2]),
                4 => (std::mem::transmute::<*const u8, Arity4Fn>(code))(a[0], a[1], a[2], a[3]),
                5 => (std::mem::transmute::<*const u8, Arity5Fn>(code))(
                    a[0], a[1], a[2], a[3], a[4],
                ),
                6 => (std::mem::transmute::<*const u8, Arity6Fn>(code))(
                    a[0], a[1], a[2], a[3], a[4], a[5],
                ),
                7 => (std::mem::transmute::<*const u8, Arity7Fn>(code))(
                    a[0], a[1], a[2], a[3], a[4], a[5], a[6],
                ),
                8 => (std::mem::transmute::<*const u8, Arity8Fn>(code))(
                    a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7],
                ),
                _ => unreachable!("clamped by the n > MAX_APPLY_ARITY check above"),
            }
        } else {
            // Closure body: env-ptr is the synthetic first arg.
            match n {
                0 => (std::mem::transmute::<*const u8, Arity1Fn>(code))(env_ptr),
                1 => (std::mem::transmute::<*const u8, Arity2Fn>(code))(env_ptr, a[0]),
                2 => (std::mem::transmute::<*const u8, Arity3Fn>(code))(env_ptr, a[0], a[1]),
                3 => (std::mem::transmute::<*const u8, Arity4Fn>(code))(
                    env_ptr, a[0], a[1], a[2],
                ),
                4 => (std::mem::transmute::<*const u8, Arity5Fn>(code))(
                    env_ptr, a[0], a[1], a[2], a[3],
                ),
                5 => (std::mem::transmute::<*const u8, Arity6Fn>(code))(
                    env_ptr, a[0], a[1], a[2], a[3], a[4],
                ),
                6 => (std::mem::transmute::<*const u8, Arity7Fn>(code))(
                    env_ptr, a[0], a[1], a[2], a[3], a[4], a[5],
                ),
                7 => (std::mem::transmute::<*const u8, Arity8Fn>(code))(
                    env_ptr, a[0], a[1], a[2], a[3], a[4], a[5], a[6],
                ),
                _ => panic!(
                    "nod_apply: closure with arity {n} exceeds Sprint 24 cap of {} (env adds 1 to the runtime ABI arity)",
                    MAX_APPLY_ARITY - 1
                ),
            }
        }
    }
}

// ─── Top-level function registry ──────────────────────────────────────────
//
// Sprint 21 mints a `<function>` Word per registered name + arity at
// **runtime first-use** (via `nod_make_function_ref(name, arity)`); the
// instance lives in the static area so the Word survives GC and so
// codegen can bake the address as an `i64` constant.
//
// Two registries cooperate:
//
//   * `RUST_FUNCTION_REGISTRY` — names handed in from Rust code (e.g.
//     the operator shims `nod_plus_fn` etc. plus `format-out`,
//     `condition-message`, …). Registered at module-init time.
//
//   * `JIT_FUNCTION_REGISTRY` — names handed in by the sema layer post
//     JIT-compile, mapping a Dylan-source name (`reduce`, `bump`, the
//     synthesised `__anon-method-NNNN`) to the JIT'd entry-point
//     address.
//
// Lookup walks both registries (Rust first) and returns the first match.
// `nod_make_function_ref` then allocates the `<function>` Word in the
// static area; the returned Word's slot values (including the
// `<byte-string>` name slot) are all pinned for the process lifetime.
//
// **Memoisation:** repeated calls for the same `(name, arity)` return
// the same Word — both because we cache the allocation and because
// codegen sites that emit the same reference (e.g. two `\+` uses)
// must observe the same Word identity (so `f == g` comparisons work).

struct FunctionRegistryEntry {
    name: String,
    arity: usize,
    code_ptr: *const u8,
}

// SAFETY: the pointers stored in `FunctionRegistryEntry::code_ptr` are
// JIT-emitted or Rust-defined function addresses pinned for the process
// lifetime. Sharing them across threads is sound.
unsafe impl Send for FunctionRegistryEntry {}
unsafe impl Sync for FunctionRegistryEntry {}

static RUST_FUNCTION_REGISTRY: Mutex<Vec<FunctionRegistryEntry>> = Mutex::new(Vec::new());
static JIT_FUNCTION_REGISTRY: Mutex<Vec<FunctionRegistryEntry>> = Mutex::new(Vec::new());
// Cache of `(name, arity) -> <function> Word`. Each entry's Word is a
// pointer into the static area, stable for the process lifetime.
static FUNCTION_REF_CACHE: Mutex<Vec<((String, usize), Word)>> = Mutex::new(Vec::new());

/// Register a Rust-side function as a callable Dylan name. Used at
/// process boot for operator shims (`+`, `*`, …) and built-in helpers
/// (`format-out`, `condition-message`, …).
///
/// Subsequent `make_function_ref(name, arity)` calls resolve through
/// this table.
///
/// # Safety
///
/// `code_ptr` must point at an `extern "C-unwind" fn(u64, ..., u64) -> u64`
/// pinned for the process lifetime. The arity stated here must match
/// the callee's actual arity.
pub unsafe fn register_rust_function(name: &str, arity: usize, code_ptr: *const u8) {
    let mut g = RUST_FUNCTION_REGISTRY
        .lock()
        .expect("rust function registry poisoned");
    if let Some(slot) = g
        .iter_mut()
        .find(|e| e.name == name && e.arity == arity)
    {
        slot.code_ptr = code_ptr;
    } else {
        g.push(FunctionRegistryEntry {
            name: name.to_string(),
            arity,
            code_ptr,
        });
    }
}

/// Register a JIT-compiled top-level function under its Dylan name. The
/// `nod-sema` layer calls this once per `define function` body after
/// the JIT module is finalised; subsequent `\name` references resolve
/// through here.
///
/// # Safety
///
/// `code_ptr` must point at a JIT-emitted `extern "C-unwind"`-shaped
/// function whose runtime ABI is `(u64, ..., u64) -> u64`. The JIT
/// engine that owns the address must outlive every call site that
/// dispatches through it (Sprint 21's loaders leak the engines).
pub unsafe fn register_jit_function(name: &str, arity: usize, code_ptr: *const u8) {
    let mut g = JIT_FUNCTION_REGISTRY
        .lock()
        .expect("jit function registry poisoned");
    if let Some(slot) = g
        .iter_mut()
        .find(|e| e.name == name && e.arity == arity)
    {
        slot.code_ptr = code_ptr;
    } else {
        g.push(FunctionRegistryEntry {
            name: name.to_string(),
            arity,
            code_ptr,
        });
    }
}

pub fn lookup_function_code(name: &str, arity: usize) -> Option<*const u8> {
    let rust = RUST_FUNCTION_REGISTRY
        .lock()
        .expect("rust function registry poisoned");
    if let Some(e) = rust.iter().find(|e| e.name == name && e.arity == arity) {
        return Some(e.code_ptr);
    }
    drop(rust);
    let jit = JIT_FUNCTION_REGISTRY
        .lock()
        .expect("jit function registry poisoned");
    jit.iter()
        .find(|e| e.name == name && e.arity == arity)
        .map(|e| e.code_ptr)
}

/// Allocate (or reuse the cached) `<function>` Word for the given
/// `(name, arity)`. The Word's storage is in the static area, so it
/// survives GC and so codegen can bake the address as an `i64`
/// constant.
///
/// Returns `None` if the name+arity isn't registered.
///
/// Sprint 26: if `name` is a registered generic with at least one
/// method (i.e. `is_generic_defined(name)` is true), returns a
/// generic-dispatch trampoline Word instead of a direct-pointer Word.
/// This is what makes `\size` (a generic with multiple methods)
/// dispatch by class at call time, replacing the Sprint 22
/// "first-registration-wins" hack that baked a single method's
/// address into the function-Ref.
///
/// A direct (non-generic) function takes the cached static-area path
/// described below; both variants share the same cache so `\name`
/// canonically resolves to one Word per `(name, arity)` either way.
///
/// ## Direct registration shadows the generic trampoline
///
/// A name can be BOTH a registered generic (with methods) AND a directly
/// registered function for the same `(name, arity)`. This happens because
/// the stdlib loader rewrites every `define function f` into a single
/// `define method f (… :: <object>)` so user `Dispatch` IR can reach it
/// (`rewrite_define_function_to_method`). When user code then defines its
/// OWN top-level `define function f` of the same name+arity, that user
/// body is registered directly in the JIT registry — but it is NOT added
/// as a method on the stdlib's `f` generic.
///
/// If we short-circuited to the generic trampoline whenever
/// `is_generic_defined(name)` were true, `\f` (and any `let g = f; g(…)`)
/// would dispatch into the *stdlib* `f$<object>_<object>` method and
/// silently ignore the user's shadowing definition — returning garbage
/// (e.g. user `define function add (a, b) a + b end` reached via a value
/// would run the stdlib list-`add` and yield a pair, not a sum).
///
/// So a DIRECT registration for `(name, arity)` (a rust operator shim or
/// a user/stdlib `define function` body) takes precedence over the
/// generic trampoline. Genuine multi-method generics (`size`, `speak`,
/// user `define generic` + `define method`) carry no direct registration
/// for their `(name, arity)`, so they still resolve to the trampoline and
/// dispatch by class at call time.
pub fn make_function_ref(name: &str, arity: usize) -> Option<Word> {
    // A directly registered function (rust shim, user/stdlib `define
    // function` body) shadows the generic trampoline — see the rationale
    // above. Only fall back to the generic-dispatch trampoline when no
    // direct callee exists for this exact `(name, arity)`.
    let direct = lookup_function_code(name, arity);
    if direct.is_none() && crate::dispatch::is_generic_defined(name) {
        return Some(make_generic_trampoline_ref(name, arity));
    }
    let mut cache = FUNCTION_REF_CACHE
        .lock()
        .expect("function ref cache poisoned");
    if let Some((_, w)) = cache
        .iter()
        .find(|((n, a), _)| n == name && *a == arity)
    {
        return Some(*w);
    }
    let code = direct?;
    // Allocate the <function> in the static area. `rust_make` writes
    // into the moveable heap by default; for a Sprint 21 function-ref
    // we want pinned storage so the codegen-baked address stays valid
    // across GC cycles.
    //
    // Approach: build the instance via `rust_make` (which currently
    // allocates from the moveable heap), then immediately `pin` it by
    // promoting through the static area. Sprint 21 simplification: we
    // skip the promotion and rely on the fact that the function-Word
    // is reachable from the cache (held in this Mutex), so the GC's
    // root-walker preserves it across collections. The cache is a
    // process-global root.
    //
    // Future Sprint 24: when closures land, env-ptr will point to a
    // moveable heap object; the make-function instance moves with it.
    let w = make_function(name, arity, code, 0, 0);
    crate::heap_register_root(Box::leak(Box::new(w)) as *const Word as *mut Word);
    cache.push(((name.to_string(), arity), w));
    Some(w)
}

/// Sprint 26: build a generic-dispatch trampoline `<function>` Word
/// for `(generic_name, arity)`. The Word's `kind-tag` slot is
/// `FUNCTION_KIND_GENERIC_TRAMPOLINE`; its `env-ptr` slot carries the
/// `&'static GenericFunction` pointer (raw u64, bit 0 == 0, so the
/// `SlotType::Object` GC classifier sees it as `Immediate` and
/// correctly skips it). Its `code-ptr` slot is null — the trampoline
/// is invoked via the `nod_funcall_N` / `nod_apply` kind-tag check,
/// which routes to `dispatch_via_generic_trampoline` and never reads
/// code-ptr.
///
/// Registers the Word as a heap root so it survives GC. Multiple
/// calls with the same `(generic_name, arity)` cache one trampoline
/// instance per pair (mirroring `make_function_ref`).
pub fn make_generic_trampoline_ref(generic_name: &str, arity: usize) -> Word {
    // The function-ref cache key shape matches the `make_function_ref`
    // cache exactly — `(name, arity)` is unique across the trampoline
    // and direct-call variants because a generic name with multiple
    // methods can't ALSO be a direct function (the dispatch registry
    // would collide). So we reuse the same cache.
    let mut cache = FUNCTION_REF_CACHE
        .lock()
        .expect("function ref cache poisoned");
    if let Some((_, w)) = cache
        .iter()
        .find(|((n, a), _)| n == generic_name && *a == arity)
    {
        return *w;
    }
    let g = crate::dispatch::get_or_create_generic(generic_name);
    let g_ptr = g as *const _ as u64;
    // `code-ptr` is null (0). The dispatch path is selected by the
    // kind-tag, which routes through `dispatch_via_generic_trampoline`
    // before reading code-ptr — so a null is safe here, and surfaces
    // any future regression that bypasses the kind-tag gate as an
    // immediate crash rather than silent misbehaviour.
    let w = make_function(
        generic_name,
        arity,
        std::ptr::null(),
        FUNCTION_KIND_GENERIC_TRAMPOLINE,
        g_ptr,
    );
    crate::heap_register_root(Box::leak(Box::new(w)) as *const Word as *mut Word);
    cache.push(((generic_name.to_string(), arity), w));
    w
}

/// JIT-callable shim that returns the function-ref Word for the
/// supplied name (a `<byte-string>` Word) and arity (a fixnum Word).
/// Panics if the name isn't registered — codegen only emits this
/// against names it knows are registered.
///
/// # Safety
///
/// `name_raw` must be a pointer-tagged `<byte-string>` Word; `arity_raw`
/// must be a fixnum-tagged Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_function_ref(name_raw: u64, arity_raw: u64) -> u64 {
    let name_word = Word::from_raw(name_raw);
    let bs = unsafe { crate::try_byte_string(name_word, ClassId::BYTE_STRING) }
        .expect("nod_make_function_ref: name is not a <byte-string> Word");
    // SAFETY: bs points at a live <byte-string>.
    let name = unsafe { bs.as_str() }
        .expect("nod_make_function_ref: <byte-string> name not UTF-8")
        .to_string();
    let arity = Word::from_raw(arity_raw)
        .as_fixnum()
        .expect("nod_make_function_ref: arity is not a fixnum") as usize;
    let f = make_function_ref(&name, arity).unwrap_or_else(|| {
        panic!(
            "nod_make_function_ref: no registered function `{name}` with arity {arity}"
        )
    });
    f.raw()
}

/// Sprint 24: build a closure `<function>` Word bound to a specific
/// environment. The body's symbol must already be registered (the
/// lifter / `register_top_level_functions` machinery does this); we
/// look up the JIT'd address through the same registry the
/// `nod_make_function_ref` path uses.
///
/// Returns the freshly-allocated closure Word (lives in the moveable
/// heap, so the GC can scan its `env-ptr` slot and relocate the
/// environment).
///
/// The body's runtime ABI is `extern "C-unwind" fn(env_word, args...) -> u64`
/// — one wider than the closure-less Sprint 21 ABI. The lowering pass
/// must emit the body with a synthetic first parameter.
///
/// # Safety
///
/// `name_raw` must be a pointer-tagged `<byte-string>` Word identifying
/// the body's symbol; `arity_raw` is a fixnum (the surface arity of
/// the closure, NOT counting the env); `env_raw` is a pointer-tagged
/// `<environment>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_closure(
    name_raw: u64,
    arity_raw: u64,
    env_raw: u64,
) -> u64 {
    let name_word = Word::from_raw(name_raw);
    let bs = unsafe { crate::try_byte_string(name_word, ClassId::BYTE_STRING) }
        .expect("nod_make_closure: name is not a <byte-string> Word");
    // SAFETY: bs points at a live <byte-string>.
    let name = unsafe { bs.as_str() }
        .expect("nod_make_closure: <byte-string> name not UTF-8")
        .to_string();
    let arity = Word::from_raw(arity_raw)
        .as_fixnum()
        .expect("nod_make_closure: arity is not a fixnum") as usize;
    // Root the env Word across the allocation in case make_function
    // triggers a minor GC that evacuates it.
    let env_word = Word::from_raw(env_raw);
    let _env_guard = crate::make::RootGuard::new(&env_word);
    let code = lookup_function_code(&name, arity).unwrap_or_else(|| {
        panic!(
            "nod_make_closure: no registered closure body `{name}` with arity {arity}"
        )
    });
    // Allocate a fresh <function> in the moveable heap, parameterised
    // with this site's environment. Different from `make_function_ref`
    // which caches one Word per (name, arity) in the static area; a
    // closure must be a unique Word per creation site so the env-ptr
    // is per-instance.
    let f = make_function(&name, arity, code, /*kind_tag=*/ 2, env_word.raw());
    f.raw()
}

/// Test helper: clear both function registries and the ref cache.
#[doc(hidden)]
pub fn _reset_function_registry_for_tests() {
    RUST_FUNCTION_REGISTRY
        .lock()
        .expect("rust function registry poisoned")
        .clear();
    JIT_FUNCTION_REGISTRY
        .lock()
        .expect("jit function registry poisoned")
        .clear();
    FUNCTION_REF_CACHE
        .lock()
        .expect("function ref cache poisoned")
        .clear();
}

// ─── Built-in operator shims ───────────────────────────────────────────────
//
// `\+`, `\-`, `\*`, `\=`, … get pre-registered Rust shims so user code
// can pass them as first-class function values (`reduce(\+, …)`). Each
// shim consumes two tagged-Word args, narrows to a fixnum, and returns
// the fixnum-tagged result.
//
// The `\=` shim returns a Dylan boolean. The relational comparisons
// (`<`, `>`, `<=`, `>=`) likewise. Float-typed args fall back to fixnum
// 0 — the brief specifies integer semantics; float handling lands when
// the stdlib defines `+` / etc. as real generics in Sprint 25.

/// Sprint 21 operator shims. Each shim has signature
/// `extern "C-unwind" fn(u64, u64) -> u64`. The inputs and output are
/// Dylan tagged Words; non-fixnum inputs decode to 0 (fallback path
/// — Sprint 21 doesn't yet ship a runtime no-applicable-method dispatch
/// for these). The `unsafe` qualifier is required by the
/// `extern "C-unwind"` ABI but the shims have no caller-visible safety
/// preconditions: any 64-bit input is well-defined.
macro_rules! arith_shim {
    ($name:ident, $op:tt) => {
        /// # Safety
        ///
        /// No preconditions. Inputs and output are Dylan tagged Words;
        /// non-fixnum decodes to 0.
        #[unsafe(no_mangle)]
        pub unsafe extern "C-unwind" fn $name(a: u64, b: u64) -> u64 {
            let av = Word::from_raw(a).as_fixnum().unwrap_or(0);
            let bv = Word::from_raw(b).as_fixnum().unwrap_or(0);
            Word::from_fixnum(av $op bv)
                .unwrap_or(Word::from_raw(0))
                .raw()
        }
    };
}

arith_shim!(nod_op_plus, +);
arith_shim!(nod_op_minus, -);
arith_shim!(nod_op_times, *);

/// `\=` — integer equality returning the Dylan boolean singleton.
///
/// # Safety
///
/// No preconditions. Inputs are any Dylan tagged Words; non-fixnum
/// inputs compare by pointer identity.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_eq(a: u64, b: u64) -> u64 {
    let av = Word::from_raw(a).as_fixnum();
    let bv = Word::from_raw(b).as_fixnum();
    let imm = crate::literal_pool_immediates();
    if av == bv && av.is_some() {
        imm.true_.raw()
    } else if av.is_some() && bv.is_some() {
        imm.false_.raw()
    } else {
        // Pointer-identity fallback for non-fixnum values.
        if a == b { imm.true_.raw() } else { imm.false_.raw() }
    }
}

/// `\<` — integer less-than.
///
/// # Safety
///
/// No preconditions. Inputs decode as fixnums; non-fixnums treat as 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_lt(a: u64, b: u64) -> u64 {
    let av = Word::from_raw(a).as_fixnum().unwrap_or(0);
    let bv = Word::from_raw(b).as_fixnum().unwrap_or(0);
    let imm = crate::literal_pool_immediates();
    if av < bv { imm.true_.raw() } else { imm.false_.raw() }
}

/// `\>` — integer greater-than.
///
/// # Safety
///
/// No preconditions. Inputs decode as fixnums; non-fixnums treat as 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_gt(a: u64, b: u64) -> u64 {
    let av = Word::from_raw(a).as_fixnum().unwrap_or(0);
    let bv = Word::from_raw(b).as_fixnum().unwrap_or(0);
    let imm = crate::literal_pool_immediates();
    if av > bv { imm.true_.raw() } else { imm.false_.raw() }
}

/// `\==` — IDENTITY equality. Raw-bit compare of the two Words (fixnums
/// carry their value in the bits, so this matches the inline
/// `PrimOp::EqInt` semantics; pointer-tagged objects compare by
/// identity). Returns the pinned `#t` / `#f` Word.
///
/// # Safety
///
/// No preconditions. Inputs are any Dylan tagged Words.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_eq_eq(a: u64, b: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    if a == b { imm.true_.raw() } else { imm.false_.raw() }
}

/// `\~=` — VALUE inequality. Mirrors the inline `~=` lowering which
/// routes pointer-shaped operands through `%object-equal?`: we call the
/// public `nod_object_equal_p`, then return the OPPOSITE boolean
/// immediate. Boxed numbers / strings get value semantics (two distinct
/// content-equal strings compare `~=` as `#f`), NOT identity semantics.
///
/// # Safety
///
/// No preconditions. Inputs are any Dylan tagged Words.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_ne(a: u64, b: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    // SAFETY: nod_object_equal_p accepts any two Dylan Words.
    let eq = unsafe { crate::nod_object_equal_p(a, b) };
    // Compare-to-immediate-and-invert: equal -> `#f`, not-equal -> `#t`.
    if eq == imm.true_.raw() {
        imm.false_.raw()
    } else {
        imm.true_.raw()
    }
}

/// `\~==` — IDENTITY inequality (the inverse of `\==`).
///
/// # Safety
///
/// No preconditions. Inputs are any Dylan tagged Words.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_op_ne_eq(a: u64, b: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    if a == b { imm.false_.raw() } else { imm.true_.raw() }
}

/// `instance?` — first-class form. The 2nd arg arrives as a runtime
/// class Word (a ClassMetadata pointer that `emit_class_ref` tagged with
/// `| 1`). We untag via `Word::as_ptr` (which masks `& !1`); if the Word
/// isn't pointer-shaped (no class metadata) we defensively return `#f`.
/// Otherwise we read `ClassMetadata.id` and route through
/// `nod_is_instance_of_word`, mirroring the inline `instance?` CPL-walk
/// semantics.
///
/// # Safety
///
/// `value` is any Dylan Word; `class_word` must be a tagged class
/// metadata Word (as produced by `emit_class_ref`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_instance_p(value: u64, class_word: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    // `Word::as_ptr` masks `& !1` and validates alignment; `None` means
    // the Word isn't a pointer-tagged class metadata Word.
    let Some(md_ptr) = Word::from_raw(class_word).as_ptr::<ClassMetadata>() else {
        return imm.false_.raw();
    };
    // SAFETY: a class Word's payload points at a pinned `ClassMetadata`.
    let class_id = unsafe { (*md_ptr).id };
    let v = Word::from_raw(value);
    if crate::nod_is_instance_of_word(v, class_id) {
        imm.true_.raw()
    } else {
        imm.false_.raw()
    }
}

/// Install the operator shims into `RUST_FUNCTION_REGISTRY`. Idempotent
/// — safe to call repeatedly. Called from the `LiteralPool`
/// initialiser path (via `ensure_registered`).
pub fn ensure_operator_shims_registered() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: each shim has the canonical `(u64, u64) -> u64` ABI.
        //
        // This runs in BOTH the JIT path AND AOT-exe startup (via
        // `nod_runtime_init` -> `ensure_registered`), so the shims are
        // present in a built EXE — a prior attempt registered the
        // `==` / `~=` / `instance?` shims on a JIT-only path and the
        // built exe panicked `nod_make_function_ref: no registered
        // function ==`. Keep all built-in operator shims here.
        //
        // CAVEAT (`is_generic_defined` shadowing): `make_function_ref`
        // (~line 932) checks `is_generic_defined(name)` BEFORE the
        // rust-shim path, so a future stdlib `==` / `~=` / `~==` /
        // `instance?` GENERIC would shadow these refs and route through
        // a generic-dispatch trampoline instead. No such generic exists
        // today, so `\==` etc. resolve to these shims.
        unsafe {
            register_rust_function("+", 2, nod_op_plus as *const u8);
            register_rust_function("-", 2, nod_op_minus as *const u8);
            register_rust_function("*", 2, nod_op_times as *const u8);
            register_rust_function("=", 2, nod_op_eq as *const u8);
            register_rust_function("<", 2, nod_op_lt as *const u8);
            register_rust_function(">", 2, nod_op_gt as *const u8);
            register_rust_function("==", 2, nod_op_eq_eq as *const u8);
            register_rust_function("~=", 2, nod_op_ne as *const u8);
            register_rust_function("~==", 2, nod_op_ne_eq as *const u8);
            register_rust_function("instance?", 2, nod_instance_p as *const u8);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C-unwind" fn echo1(a: u64) -> u64 {
        a
    }
    extern "C-unwind" fn add2(a: u64, b: u64) -> u64 {
        // Treat both as fixnums.
        let aw = Word::from_raw(a);
        let bw = Word::from_raw(b);
        let av = aw.as_fixnum().unwrap_or(0);
        let bv = bw.as_fixnum().unwrap_or(0);
        Word::from_fixnum(av + bv).unwrap().raw()
    }

    #[test]
    fn function_class_registers_and_introspects() {
        ensure_registered();
        let id = function_class_id();
        let md = class_metadata_for(id);
        assert_eq!(md.name, "<function>");
        // Six slots: name, arity, code-ptr, kind-tag, env-ptr, return-type.
        assert_eq!(md.slots.len(), 6);
        // Sprint 24: `name` is `String`-typed and `env-ptr` is `Object`-
        // typed (pointer-shaped — the GC must scan them); the other four
        // slots stay `Integer`-typed (opaque host bits).
        for s in &md.slots {
            let expected = match s.name.as_str() {
                "name" => SlotType::String,
                "env-ptr" => SlotType::Object,
                _ => SlotType::Integer,
            };
            assert_eq!(s.type_kind, expected, "slot `{}` type", s.name);
        }
    }

    #[test]
    fn wrong_args_error_inherits_from_error() {
        ensure_registered();
        let wae = wrong_number_of_arguments_error_class_id();
        assert!(is_subclass(wae, crate::conditions::error_class_id()));
    }

    #[test]
    fn make_function_roundtrips_arity_and_code_ptr() {
        ensure_registered();
        let f = make_function("echo1", 1, echo1 as *const u8, 0, 0);
        assert_eq!(function_arity(f), Some(1));
        assert_eq!(
            function_code_ptr(f).unwrap() as usize,
            echo1 as *const () as usize
        );
        assert_eq!(function_name(f).as_deref(), Some("echo1"));
        assert!(is_function(f));
    }

    #[test]
    fn funcall1_dispatches_to_echo() {
        ensure_registered();
        let f = make_function("echo1", 1, echo1 as *const u8, 0, 0);
        let arg = Word::from_fixnum(42).unwrap();
        // SAFETY: f is a real <function>, arg is a real tagged Word, and
        // the callee at code-ptr is arity-1.
        let result = unsafe { nod_funcall1(f.raw(), arg.raw()) };
        assert_eq!(Word::from_raw(result).as_fixnum(), Some(42));
    }

    #[test]
    fn funcall2_dispatches_to_add() {
        ensure_registered();
        let f = make_function("add2", 2, add2 as *const u8, 0, 0);
        let a = Word::from_fixnum(40).unwrap();
        let b = Word::from_fixnum(2).unwrap();
        // SAFETY: arity-2 callee + two tagged Word args.
        let result = unsafe { nod_funcall2(f.raw(), a.raw(), b.raw()) };
        assert_eq!(Word::from_raw(result).as_fixnum(), Some(42));
    }

    #[test]
    fn funcall_arity_mismatch_signals_wae() {
        // No installed handler => process-level panic with the
        // unhandled-condition message. We catch the panic and assert
        // the class name appears in it.
        ensure_registered();
        crate::_reset_handler_stack_for_tests();
        let f = make_function("echo1", 1, echo1 as *const u8, 0, 0);
        let a = Word::from_fixnum(1).unwrap();
        let b = Word::from_fixnum(2).unwrap();
        let outcome = std::panic::catch_unwind(|| {
            // SAFETY: passing 2 args to an arity-1 function — arity
            // mismatch triggers nod_signal -> panic.
            unsafe {
                nod_funcall2(f.raw(), a.raw(), b.raw());
            }
        });
        crate::_reset_handler_stack_for_tests();
        let err = outcome.expect_err("arity mismatch must panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains("<wrong-number-of-arguments-error>"),
            "expected WAE in panic message, got: {msg}"
        );
    }
}
