//! Sprint 26 — diagnostic harness for `make(<range>, from:, to:, by:)`.
//!
//! Used during development to confirm that `make(<range>, …)` works as
//! the canonical keyword-init form. The Sprint 21 headline test used
//! the `%make-range` primitive workaround; Sprint 26 closes the gap so
//! the literal spec form drives `reduce(\+, 0, range(from: 1, to: 100))`.

use serial_test::serial;

use nod_runtime::{_reset_handler_stack_for_tests, ensure_collections_registered};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
}

#[test]
#[serial]
fn make_range_with_all_three_kw() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "size(make(<range>, from: 1, to: 100, by: 1))",
    )
    .expect("eval make(<range>, from: 1, to: 100, by: 1)");
    assert_eq!(s, "100");
}

#[test]
#[serial]
fn make_range_with_from_and_to_defaults_by_to_1() {
    // The canonical form. Sprint 26: defaulting `by:` to 1 makes this
    // work end-to-end without the user supplying the step.
    setup();
    let s = nod_sema::eval_expr_to_string(
        "size(make(<range>, from: 1, to: 100))",
    )
    .expect("eval make(<range>, from: 1, to: 100)");
    assert_eq!(s, "100");
}

#[test]
#[serial]
fn make_range_with_negative_step() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "size(make(<range>, from: 100, to: 1, by: -1))",
    )
    .expect("eval make(<range>, from: 100, to: 1, by: -1)");
    assert_eq!(s, "100");
}

/// Closes the Sprint 21 deferral: the headline `reduce(\+, 0, …)` test
/// drives the canonical `make(<range>, …)` form instead of the
/// `%make-range` primitive workaround.
#[test]
#[serial]
fn reduce_plus_zero_range_one_to_hundred_via_make() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "reduce(\\+, 0, make(<range>, from: 1, to: 100))",
    )
    .expect("eval reduce(\\+, 0, make(<range>, from: 1, to: 100))");
    assert_eq!(s, "5050");
}
