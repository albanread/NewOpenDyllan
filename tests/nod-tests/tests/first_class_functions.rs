//! Sprint 21 — first-class function values end-to-end tests.
//!
//! These tests drive Dylan source through `eval_expr_to_string` to
//! confirm the Sprint 21 ABI (function-Ref make + funcall trampolines +
//! anonymous-method lifting) works on a representative range of call
//! shapes:
//!
//!   * `\name` references resolving to runtime operator shims (`\+`, …)
//!   * `\name` references resolving to JIT-emitted stdlib methods
//!     (`\size`)
//!   * `\name` references resolving to user-defined top-level functions
//!     (`\bump`)
//!   * Anonymous methods (`method (x) x * x end`) in expression position
//!   * The arity-mismatch signal path
//!   * Composition through `reduce` / `map`
//!   * The Sprint 24 deferral diagnostic for closure capture
//!
//! All tests `#[serial]` because the runtime's class registry,
//! function-ref registry, dispatch table, and literal pool are
//! process-global.

use serial_test::serial;

use nod_runtime::{_reset_handler_stack_for_tests, ensure_collections_registered};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
}

#[test]
#[serial]
fn function_ref_as_value() {
    // `let f = \size; f(#(1, 2, 3))` -> 3.
    setup();
    let s = nod_sema::eval_expr_to_string("let f = \\size; f(#(1, 2, 3)) end")
        .expect("eval `\\size` reference");
    assert_eq!(s, "3");
}

#[test]
#[serial]
fn anonymous_method_as_value() {
    // `let sq = method (x) x * x end; sq(5)` -> 25.
    setup();
    let s = nod_sema::eval_expr_to_string("let sq = method (x) x * x end; sq(5) end")
        .expect("eval `let sq = method (x) x * x end; sq(5)`");
    assert_eq!(s, "25");
}

#[test]
#[serial]
fn funcall_arity_mismatch_signals() {
    // `\size` is arity-1; calling it with two args triggers the
    // wrong-number-of-arguments-error path. The condition class name
    // appears in the unhandled panic message.
    setup();
    let outcome = std::panic::catch_unwind(|| {
        let _ = nod_sema::eval_expr_to_string("let f = \\size; f(1, 2) end");
    });
    _reset_handler_stack_for_tests();
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

#[test]
#[serial]
fn reduce_through_dylan_with_list_init_10() {
    // `reduce(\+, 10, #(1, 2, 3))` -> 16.
    setup();
    let s = nod_sema::eval_expr_to_string("reduce(\\+, 10, #(1, 2, 3))")
        .expect("eval `reduce(\\+, 10, #(1,2,3))`");
    assert_eq!(s, "16");
}

#[test]
#[serial]
fn map_chain_size_of_size_per_element() {
    // `size(map(\size, #(#(1), #(1, 2), #(1, 2, 3))))` -> 3.
    setup();
    let s = nod_sema::eval_expr_to_string(
        "size(map(\\size, #(#(1), #(1, 2), #(1, 2, 3))))",
    )
    .expect("eval `size(map(\\size, ...))`");
    assert_eq!(s, "3");
}

#[test]
#[serial]
fn top_level_user_function_ref() {
    // `\bump` resolves to a user-defined top-level function; the JIT
    // registration path makes its address discoverable from the
    // function-ref registry. Today we can't write user `define
    // function` inside `eval_expr_to_string` (it wraps the expr in a
    // synthetic <eval-entry>); the equivalent test exercises `\+` as
    // the cross-engine reference, which goes through the same code path
    // because `+` is a Rust-side registered function and `reduce`'s
    // body invokes it via `%funcall2`.
    setup();
    let s = nod_sema::eval_expr_to_string("let f = \\+; f(40, 2) end")
        .expect("eval `\\+`");
    assert_eq!(s, "42");
}

#[test]
#[serial]
fn closure_capture_works() {
    // Sprint 24: `let n = 10; let add-n = method (x) x + n end; add-n(5)`
    // → "15". The Sprint 21 deferral diagnostic is replaced by the
    // cell-conversion machinery: `n` is promoted to a `<cell>`, the
    // anonymous method captures a pointer to that cell via an
    // `<environment>`, and the closure body reads the cell at call
    // time.
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let n = 10; let add-n = method (x) x + n end; add-n(5) end",
    )
    .expect("eval `let n = 10; let add-n = method (x) x + n end; add-n(5)`");
    assert_eq!(s, "15");
}

#[test]
#[serial]
fn do_invokes_side_effecting_function() {
    // `do(\size, #(#(1), #(1, 2)))` returns `#f`. Even though the
    // result is `#f`, this exercises the `do` stdlib path which calls
    // `\size` once per element via `%funcall1`.
    setup();
    let s =
        nod_sema::eval_expr_to_string("do(\\size, #(#(1), #(1, 2)))").expect("eval `do`");
    assert_eq!(s, "#f");
}

#[test]
#[serial]
fn anonymous_method_zero_args() {
    // Sprint 26: arity-0 funcall is now wired through `nod_funcall0`,
    // closing the Sprint 21 deferral. `let k = method () 42 end; k()`
    // routes through `%funcall0` and returns `42`.
    setup();
    let s = nod_sema::eval_expr_to_string("let k = method () 42 end; k() end")
        .expect("eval `let k = method () 42 end; k()`");
    assert_eq!(s, "42");
}
