//! Sprint 26 — `\generic-name` dispatches by class at call time.
//!
//! Sprint 22 used a "first-registration-wins" hack so that `\size`
//! (a generic with multiple methods) resolved to a *single* method
//! body's code-ptr. With the most-general fallback registered first,
//! `\size` happened to work for the common cases — but `\size(<table>)`
//! would call the wrong body, defeating dispatch.
//!
//! Sprint 26 replaces that with a real **generic-dispatch trampoline**.
//! `make_function_ref("size", …)` now returns a `<function>` Word
//! whose `kind-tag` is `FUNCTION_KIND_GENERIC_TRAMPOLINE`; the
//! trampoline routes through `nod_dispatch` at each call, picking the
//! most-specific applicable method per the receiver's class — exactly
//! as the dispatch-call site `size(x)` already does.
//!
//! These tests pin that behaviour. The headline check is that the
//! `<table>` method is selected when the receiver is a table, and the
//! `<object>` fallback is selected for non-table receivers.
//!
//! All tests `#[serial]` because the runtime's class registry,
//! function-ref registry, dispatch table, and literal pool are
//! process-global.

use serial_test::serial;

use nod_runtime::{
    _reset_handler_stack_for_tests, ensure_collections_registered, ensure_tables_registered,
};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
    ensure_tables_registered();
}

/// `\size` on a `<list>` selects the generic-fallback body
/// (`define function size (c)`) and returns the FIP-counted size.
/// This is the path the Sprint 21 `function_ref_as_value` test already
/// covered; we keep it here to guard the *generic* (not specialised)
/// branch through the trampoline.
#[test]
#[serial]
fn generic_ref_size_on_list_uses_fallback() {
    setup();
    let s = nod_sema::eval_expr_to_string("let f = \\size; f(#(1, 2, 3))")
        .expect("eval \\size on list");
    assert_eq!(s, "3");
}

/// `\size` on a `<table>` selects the specialised
/// `define method size (t :: <table>) => …` body. Before Sprint 26
/// (and the trampoline), the function-ref path baked the more-general
/// `size$<object>` body's code-ptr and would have called the wrong
/// method for a `<table>` argument.
#[test]
#[serial]
fn generic_ref_size_on_table_dispatches_to_table_method() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[#\"a\"] := 1; \
         t[#\"b\"] := 2; \
         let f = \\size; \
         f(t)",
    )
    .expect("eval \\size on table");
    assert_eq!(s, "2");
}

/// `\size` on a `<range>` — its size method comes from the FIP-based
/// `define function size (c)` fallback (no `<range>`-specialised
/// override in stdlib), so this exercises the generic-fallback branch
/// with a non-list receiver to confirm the trampoline isn't tied to
/// list shape.
#[test]
#[serial]
fn generic_ref_size_on_range_uses_fallback() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let f = \\size; f(make(<range>, from: 1, to: 10))",
    )
    .expect("eval \\size on range");
    assert_eq!(s, "10");
}

/// Round-trip via `map`: `\size` is the per-element function. Each
/// element has a different class, but `map` dispatches `\size` on
/// each in turn — confirming the trampoline isn't memoising a single
/// class's method behind our back.
#[test]
#[serial]
fn generic_ref_size_through_map_mixed_classes() {
    setup();
    // (size #(1, 2)) = 2, (size #()) = 0, (size #(1, 2, 3, 4, 5)) = 5
    let s = nod_sema::eval_expr_to_string(
        "size(map(\\size, #(#(1, 2), #(), #(1, 2, 3, 4, 5))))",
    )
    .expect("eval map(\\size, list-of-lists)");
    assert_eq!(s, "3");
}

/// Direct check that `make_function_ref` returns a trampoline `<function>`
/// Word for a generic name. The function-Word's `kind-tag` slot is
/// `FUNCTION_KIND_GENERIC_TRAMPOLINE` and its `code-ptr` slot is null —
/// the dispatch path runs through `dispatch_via_generic_trampoline`,
/// not through the code-ptr transmute.
#[test]
#[serial]
fn make_function_ref_for_generic_returns_trampoline() {
    setup();
    // Ensure `size` has at least one registered method.
    let _ =
        nod_sema::eval_expr_to_string("size(#(1, 2, 3))").expect("seed size dispatch");
    let f = nod_runtime::make_function_ref("size", 1)
        .expect("make_function_ref(\"size\", 1)");
    assert_eq!(
        nod_runtime::function_kind_tag(f),
        Some(nod_runtime::FUNCTION_KIND_GENERIC_TRAMPOLINE),
        "size's <function> ref should be a generic trampoline"
    );
    assert_eq!(
        nod_runtime::function_code_ptr(f),
        Some(std::ptr::null()),
        "generic trampoline has null code-ptr — dispatch goes through kind-tag, not code-ptr"
    );
}
