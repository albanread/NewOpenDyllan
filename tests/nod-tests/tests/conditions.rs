//! Sprint 19 — `block` / `exception` / `cleanup` end-to-end tests.
//!
//! Tests exercise three layers:
//!
//!   1. **Runtime-only** — register test thunks as a `BlockFns`, call
//!      `nod_run_block` directly, assert the value returned and the
//!      side-effects of cleanup / handler thunks. No JIT involved.
//!   2. **Dispatch + signal interplay** — the Sprint 13 no-applicable-
//!      methods panic is now a signalled `<no-applicable-methods-error>`;
//!      a `block`-style handler installed via the runtime API catches it.
//!   3. **Introspection** — `handlers_report()` lists the active chain.
//!
//! Every test that touches the process-global handler stack / block
//! registry / dispatch tables is `#[serial]`. Each test also calls
//! `_reset_handler_stack_for_tests()` and
//! `_reset_block_registry_for_tests()` at the start so an earlier test
//! that triggered an unhandled-condition panic (which can transit
//! `catch_unwind` before the enclosing block's Drop guard pops the
//! frames) doesn't leave a stale frame.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serial_test::serial;

use nod_reader::{SourceMap, lex, parse_module, scan_preamble};
use nod_runtime::{
    BlockFns, ClassId, HandlerFn, Word, _reset_block_registry_for_tests,
    _reset_dispatch_for_tests, _reset_handler_stack_for_tests, add_method_full,
    class_metadata_for, condition_class_name, condition_message,
    ensure_conditions_registered, error_class_id, find_class_id_by_name,
    get_or_create_generic, handler_stack_snapshot, handlers_report, intern_string_literal,
    make_simple_error, make_simple_warning, nod_dispatch, nod_run_block, register_block_fns,
    rust_make, simple_error_class_id, try_byte_string, warning_class_id,
};

// `nod_runtime::allocate_block_id` was removed when block ids became
// deterministic SipHash-derived values computed in `nod-sema/lower.rs` (the
// block-return fix). These runtime-level tests only need DISTINCT ids to key the
// per-block registry, so a process-local counter stands in for the old runtime
// allocator without re-introducing a production symbol.
fn allocate_block_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ───────────────────────────────────────────────────────────────────────────
// Test scaffolding.
// ───────────────────────────────────────────────────────────────────────────

fn setup() {
    _reset_handler_stack_for_tests();
    _reset_block_registry_for_tests();
    ensure_conditions_registered();
}

fn register_user_classes(src: &str) {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    let m = parse_module(src, &toks, pre.as_ref()).expect("parse");
    let _ = nod_sema::lower_module(&m).expect("lower");
}

type BodyThunk = extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64;
type HandlerThunk =
    extern "C-unwind" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64;

/// Build a `BlockFns` from a body fn (and optional cleanup / handler
/// list). Class names are pinned via `Box::leak` to match the JIT path.
fn make_block_fns(
    body: BodyThunk,
    cleanup: Option<BodyThunk>,
    handlers: Vec<(ClassId, &'static str, HandlerThunk)>,
) -> BlockFns {
    let handler_fns: Vec<HandlerFn> = handlers
        .into_iter()
        .map(|(cid, name, f)| {
            let pinned: &'static str = Box::leak(name.to_string().into_boxed_str());
            HandlerFn {
                class_id: cid,
                class_name_ptr: pinned.as_ptr(),
                class_name_len: pinned.len(),
                body: f as *const u8,
            }
        })
        .collect();
    let handlers_static: &'static [HandlerFn] = Box::leak(handler_fns.into_boxed_slice());
    BlockFns {
        body: body as *const u8,
        cleanup: cleanup.map(|f| f as *const u8),
        afterwards: None,
        handlers: handlers_static,
    }
}

// ─── 1. block + signal + handler → roundtrip the message ───────────────────

static T1_BODY_RAN: AtomicBool = AtomicBool::new(false);
static T1_HANDLER_RAN: AtomicBool = AtomicBool::new(false);

extern "C-unwind" fn t1_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T1_BODY_RAN.store(true, Ordering::SeqCst);
    let cond = make_simple_error("x");
    // SAFETY: `nod_signal` accepts any pointer-tagged condition Word.
    // Diverges via NLX; the value the handler returns becomes the
    // block's result.
    unsafe { nod_runtime::nod_signal(cond.raw()) };
    #[allow(unreachable_code)]
    0
}

extern "C-unwind" fn t1_handler(
    condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T1_HANDLER_RAN.store(true, Ordering::SeqCst);
    let cond = Word::from_raw(condition);
    condition_message(cond).raw()
}

#[test]
#[serial]
fn block_signal_handler_roundtrip() {
    setup();
    T1_BODY_RAN.store(false, Ordering::SeqCst);
    T1_HANDLER_RAN.store(false, Ordering::SeqCst);

    let block_id = allocate_block_id();
    let fns = make_block_fns(
        t1_body,
        None,
        vec![(error_class_id(), "<error>", t1_handler)],
    );
    register_block_fns(block_id, fns);

    // SAFETY: thunks have the canonical `extern "C-unwind" fn(u64*8)
    // -> u64` / `(u64*9) -> u64` signatures.
    let result = unsafe { nod_run_block(block_id, 0, 0, 0, 0, 0, 0, 0, 0) };

    assert!(T1_BODY_RAN.load(Ordering::SeqCst), "body should have run");
    assert!(T1_HANDLER_RAN.load(Ordering::SeqCst), "handler should have run");

    let msg_word = Word::from_raw(result);
    let bs = unsafe { try_byte_string(msg_word, ClassId::BYTE_STRING) }.expect("byte-string");
    assert_eq!(unsafe { bs.as_str() }, Some("x"));
}

// ─── 2. block with cleanup, no signal → cleanup runs on normal exit ────────

static T2_BODY_RAN: AtomicBool = AtomicBool::new(false);
static T2_CLEANUP_RAN: AtomicBool = AtomicBool::new(false);

extern "C-unwind" fn t2_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T2_BODY_RAN.store(true, Ordering::SeqCst);
    Word::from_fixnum(42).unwrap().raw()
}

extern "C-unwind" fn t2_cleanup(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T2_CLEANUP_RAN.store(true, Ordering::SeqCst);
    0
}

#[test]
#[serial]
fn block_normal_exit_runs_cleanup() {
    setup();
    T2_BODY_RAN.store(false, Ordering::SeqCst);
    T2_CLEANUP_RAN.store(false, Ordering::SeqCst);

    let block_id = allocate_block_id();
    let fns = make_block_fns(t2_body, Some(t2_cleanup), vec![]);
    register_block_fns(block_id, fns);

    // SAFETY: as above.
    let result = unsafe { nod_run_block(block_id, 0, 0, 0, 0, 0, 0, 0, 0) };

    assert!(T2_BODY_RAN.load(Ordering::SeqCst));
    assert!(T2_CLEANUP_RAN.load(Ordering::SeqCst), "cleanup should run on normal exit");
    assert_eq!(Word::from_raw(result).as_fixnum(), Some(42));
}

// ─── 3. block with cleanup, signal fires → cleanup also runs ───────────────

static T3_CLEANUP_RAN: AtomicU64 = AtomicU64::new(0);

extern "C-unwind" fn t3_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cond = make_simple_error("boom");
    // SAFETY: see test 1.
    unsafe { nod_runtime::nod_signal(cond.raw()) };
    #[allow(unreachable_code)]
    0
}

extern "C-unwind" fn t3_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    Word::from_fixnum(7).unwrap().raw()
}

extern "C-unwind" fn t3_cleanup(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T3_CLEANUP_RAN.fetch_add(1, Ordering::SeqCst);
    0
}

#[test]
#[serial]
fn block_signal_runs_cleanup() {
    setup();
    T3_CLEANUP_RAN.store(0, Ordering::SeqCst);

    let block_id = allocate_block_id();
    let fns = make_block_fns(
        t3_body,
        Some(t3_cleanup),
        vec![(error_class_id(), "<error>", t3_handler)],
    );
    register_block_fns(block_id, fns);

    // SAFETY: as above.
    let result = unsafe { nod_run_block(block_id, 0, 0, 0, 0, 0, 0, 0, 0) };

    assert_eq!(
        T3_CLEANUP_RAN.load(Ordering::SeqCst),
        1,
        "cleanup should run exactly once on signal-driven exit"
    );
    assert_eq!(Word::from_raw(result).as_fixnum(), Some(7));
}

// ─── 4. handler class specificity — <simple-error> beats <error> ───────────
//
// Two handlers on the same block: `<error>` (first in source) and
// `<simple-error>` (second in source). The signal walker checks
// most-recently-pushed first, which is source-LAST. So with both
// matching, the `<simple-error>` handler — which is more specific AND
// last in source — wins. We assert specifically against the "second"
// firing to lock in the contract.

static T4_GENERIC_HANDLER_RAN: AtomicBool = AtomicBool::new(false);
static T4_SPECIFIC_HANDLER_RAN: AtomicBool = AtomicBool::new(false);

extern "C-unwind" fn t4_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cond = make_simple_error("oops");
    // SAFETY: see test 1.
    unsafe { nod_runtime::nod_signal(cond.raw()) };
    #[allow(unreachable_code)]
    0
}

extern "C-unwind" fn t4_generic_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T4_GENERIC_HANDLER_RAN.store(true, Ordering::SeqCst);
    Word::from_fixnum(1).unwrap().raw()
}

extern "C-unwind" fn t4_specific_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T4_SPECIFIC_HANDLER_RAN.store(true, Ordering::SeqCst);
    Word::from_fixnum(2).unwrap().raw()
}

#[test]
#[serial]
fn handler_class_specificity() {
    setup();
    T4_GENERIC_HANDLER_RAN.store(false, Ordering::SeqCst);
    T4_SPECIFIC_HANDLER_RAN.store(false, Ordering::SeqCst);

    let block_id = allocate_block_id();
    let fns = make_block_fns(
        t4_body,
        None,
        vec![
            (error_class_id(), "<error>", t4_generic_handler),
            (simple_error_class_id(), "<simple-error>", t4_specific_handler),
        ],
    );
    register_block_fns(block_id, fns);

    // SAFETY: as above.
    let result = unsafe { nod_run_block(block_id, 0, 0, 0, 0, 0, 0, 0, 0) };

    assert!(
        T4_SPECIFIC_HANDLER_RAN.load(Ordering::SeqCst),
        "the more specific <simple-error> handler should fire"
    );
    assert!(
        !T4_GENERIC_HANDLER_RAN.load(Ordering::SeqCst),
        "the generic <error> handler should not fire"
    );
    assert_eq!(Word::from_raw(result).as_fixnum(), Some(2));
}

// ─── 5. Unhandled signal → process-level panic ─────────────────────────────

#[test]
#[serial]
fn unhandled_signal_panics() {
    setup();
    let result = std::panic::catch_unwind(|| {
        let cond = make_simple_error("loose");
        // No block context — should panic.
        // SAFETY: see test 1.
        unsafe { nod_runtime::nod_signal(cond.raw()) }
    });
    let err = result.expect_err("nod_signal with no handler should panic");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("unhandled signalled condition"),
        "expected 'unhandled signalled condition' in panic message; got: {msg}"
    );
    assert!(
        msg.contains("<simple-error>"),
        "expected the condition class name in panic message; got: {msg}"
    );
    _reset_handler_stack_for_tests();
}

// ─── 6. Nested blocks: outer handler catches signal from inner block ───────

static T6_INNER_CLEANUP_RAN: AtomicBool = AtomicBool::new(false);
static T6_OUTER_HANDLER_RAN: AtomicBool = AtomicBool::new(false);
static T6_INNER_BLOCK_ID: AtomicU64 = AtomicU64::new(0);

extern "C-unwind" fn t6_outer_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let inner_id = T6_INNER_BLOCK_ID.load(Ordering::SeqCst);
    // SAFETY: as above.
    unsafe { nod_run_block(inner_id, 0, 0, 0, 0, 0, 0, 0, 0) }
}

extern "C-unwind" fn t6_outer_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T6_OUTER_HANDLER_RAN.store(true, Ordering::SeqCst);
    Word::from_fixnum(99).unwrap().raw()
}

extern "C-unwind" fn t6_inner_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let cond = make_simple_error("inner-signal");
    // SAFETY: as above.
    unsafe { nod_runtime::nod_signal(cond.raw()) };
    #[allow(unreachable_code)]
    0
}

extern "C-unwind" fn t6_inner_warning_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    panic!("inner <warning> handler shouldn't fire for a <simple-error>");
}

extern "C-unwind" fn t6_inner_cleanup(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    T6_INNER_CLEANUP_RAN.store(true, Ordering::SeqCst);
    0
}

#[test]
#[serial]
fn nested_blocks_walk_outer() {
    setup();
    T6_INNER_CLEANUP_RAN.store(false, Ordering::SeqCst);
    T6_OUTER_HANDLER_RAN.store(false, Ordering::SeqCst);

    let inner_id = allocate_block_id();
    let outer_id = allocate_block_id();
    T6_INNER_BLOCK_ID.store(inner_id, Ordering::SeqCst);

    let inner_fns = make_block_fns(
        t6_inner_body,
        Some(t6_inner_cleanup),
        vec![(warning_class_id(), "<warning>", t6_inner_warning_handler)],
    );
    register_block_fns(inner_id, inner_fns);

    let outer_fns = make_block_fns(
        t6_outer_body,
        None,
        vec![(error_class_id(), "<error>", t6_outer_handler)],
    );
    register_block_fns(outer_id, outer_fns);

    // SAFETY: as above.
    let result = unsafe { nod_run_block(outer_id, 0, 0, 0, 0, 0, 0, 0, 0) };
    assert!(
        T6_INNER_CLEANUP_RAN.load(Ordering::SeqCst),
        "inner cleanup should run when the panic transits out"
    );
    assert!(
        T6_OUTER_HANDLER_RAN.load(Ordering::SeqCst),
        "outer <error> handler should catch the signal"
    );
    assert_eq!(Word::from_raw(result).as_fixnum(), Some(99));
}

// ─── 7. No applicable methods signals <no-applicable-methods-error> ────────

static T7_HANDLER_CLASS_NAME: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

extern "C-unwind" fn t7_handler(
    condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let c = Word::from_raw(condition);
    let name = condition_class_name(c).unwrap_or_default();
    *T7_HANDLER_CLASS_NAME.lock().unwrap() = name;
    Word::from_fixnum(0).unwrap().raw()
}

extern "C" fn t7_body_returns_1(_self: u64) -> u64 {
    Word::from_fixnum(1).unwrap().raw()
}

extern "C-unwind" fn t7_block_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let b_id = find_class_id_by_name("<t7-b>").expect("<t7-b> registered");
    let b_md = class_metadata_for(b_id);
    // SAFETY: registered metadata, no required init keywords.
    let inst = unsafe { rust_make(b_md, &[]) };
    let g = get_or_create_generic("t7-only-on-a");
    // SAFETY: arity=1, no cache slot.
    unsafe {
        nod_dispatch(
            g as *const _ as u64,
            0,
            1,
            inst.raw(),
            0, 0, 0, 0, 0, 0, 0,
        )
    }
}

#[test]
#[serial]
fn no_applicable_methods_signals() {
    setup();
    // Don't call `_reset_user_classes_for_tests` here: the conditions
    // registry holds OnceLock'd cached metadata pointers, and dropping
    // the registry entries leaves the cache stale (the metadata is
    // still pinned in the static area, but `class_metadata_ptr` returns
    // null for the freed ids). The Sprint 13 dispatch tests use
    // per-test unique class names instead — we do the same with
    // `<t7-a>`/`<t7-b>`.
    _reset_dispatch_for_tests();
    *T7_HANDLER_CLASS_NAME.lock().unwrap() = String::new();

    register_user_classes(
        "\
define class <t7-a> (<object>) end class;
define class <t7-b> (<object>) end class;
",
    );

    let a_id = find_class_id_by_name("<t7-a>").expect("<t7-a>");
    // SAFETY: t7_body_returns_1 has the right signature.
    unsafe {
        add_method_full(
            "t7-only-on-a",
            vec![a_id],
            t7_body_returns_1 as *const u8,
            1,
        );
    }

    let block_id = allocate_block_id();
    let fns = make_block_fns(
        t7_block_body,
        None,
        vec![(error_class_id(), "<error>", t7_handler)],
    );
    register_block_fns(block_id, fns);

    // SAFETY: as above.
    let _result = unsafe { nod_run_block(block_id, 0, 0, 0, 0, 0, 0, 0, 0) };

    let caught = T7_HANDLER_CLASS_NAME.lock().unwrap().clone();
    assert_eq!(
        caught, "<no-applicable-methods-error>",
        "handler should have caught a <no-applicable-methods-error>"
    );
}

// ─── 8. handlers_report() lists nested chain in order ──────────────────────

static T8_INNER_BLOCK_ID: AtomicU64 = AtomicU64::new(0);

extern "C-unwind" fn t8_inner_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let report = handlers_report();
    intern_string_literal(&report).raw()
}

extern "C-unwind" fn t8_outer_body(
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    let id = T8_INNER_BLOCK_ID.load(Ordering::SeqCst);
    // SAFETY: as above.
    unsafe { nod_run_block(id, 0, 0, 0, 0, 0, 0, 0, 0) }
}

extern "C-unwind" fn t8_never_handler(
    _condition: u64,
    _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64, _: u64,
) -> u64 {
    panic!("t8: no signal expected; handlers are just for chain inspection");
}

#[test]
#[serial]
fn handlers_report_lists_chain() {
    setup();

    let inner_id = allocate_block_id();
    let outer_id = allocate_block_id();
    T8_INNER_BLOCK_ID.store(inner_id, Ordering::SeqCst);

    let inner_fns = make_block_fns(
        t8_inner_body,
        None,
        vec![(warning_class_id(), "<warning>", t8_never_handler)],
    );
    register_block_fns(inner_id, inner_fns);

    let outer_fns = make_block_fns(
        t8_outer_body,
        None,
        vec![(error_class_id(), "<error>", t8_never_handler)],
    );
    register_block_fns(outer_id, outer_fns);

    // SAFETY: as above.
    let result = unsafe { nod_run_block(outer_id, 0, 0, 0, 0, 0, 0, 0, 0) };
    let bs = unsafe { try_byte_string(Word::from_raw(result), ClassId::BYTE_STRING) }
        .expect("byte-string");
    let report = unsafe { bs.as_str() }.unwrap_or("").to_string();

    // Both class names appear, innermost first (<warning> before <error>).
    let w_idx = report
        .find("<warning>")
        .unwrap_or_else(|| panic!("expected <warning> in report:\n{report}"));
    let e_idx = report
        .find("<error>")
        .unwrap_or_else(|| panic!("expected <error> in report:\n{report}"));
    assert!(
        w_idx < e_idx,
        "innermost frame (<warning>) should appear before outer (<error>); got:\n{report}"
    );

    // Sanity: after both blocks return, the chain is empty.
    assert_eq!(handler_stack_snapshot().len(), 0);

    // Suppress unused-warning on make_simple_warning if no test pulls
    // it in: keep it referenced.
    let _ = make_simple_warning;
}

// ─── 9. End-to-end: Sprint 19 headline acceptance test ─────────────────────
//
// `block () signal(make(<simple-error>, message: "x")) exception (c :: <error>)
//  condition-message(c) end` → "x".
//
// This drives the full Dylan-source → DFM → LLVM → JIT path through the
// new block/exception lowering. The seed condition classes are
// re-registered automatically at the top of `lower_module_full`.

#[test]
#[serial]
fn dylan_source_block_signal_handler_acceptance() {
    setup();
    let result = nod_sema::eval_expr_to_string(
        r#"block () signal(make(<simple-error>, message: "x")) exception (c :: <error>) condition-message(c) end"#,
    );
    let s = result.unwrap_or_else(|e| panic!("eval failed: {e:?}"));
    // `condition-message` returns a `<byte-string>` Word; the eval
    // formatter debug-quotes it as `"x"`.
    assert_eq!(s, "\"x\"");
}
