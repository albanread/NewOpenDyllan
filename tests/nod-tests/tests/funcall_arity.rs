//! Sprint 26 — direct-funcall trampolines at arities 0..=5.
//!
//! Sprint 21 wired the env-bound funcall dispatch at arities 1 and 2;
//! arity 0 and arity 3+ were explicitly deferred (the lowerer surfaced a
//! "not supported" error). Sprint 26 lifts the cap: every arity from 0
//! through 5 now routes through a dedicated `nod_funcall_N` trampoline,
//! removing both the lower bound (arity 0) and the previously-deferred
//! arities 3–5. Arities 6+ continue to require `nod_apply`.
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

/// Arity-0 closure: the headline check that the Sprint 21/24 deferral
/// is closed. `bump()` increments a captured cell; the outer scope
/// observes the accumulated count.
#[test]
#[serial]
fn arity_0_closure_runs() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let count = 0; \
         let bump = method () count := count + 1 end; \
         bump(); bump(); count",
    )
    .expect("eval arity-0 closure");
    assert_eq!(s, "2");
}

/// Arity-0 method invoked once. Pure check of the funcall0 path with
/// no captured state.
#[test]
#[serial]
fn arity_0_method_returns_literal() {
    setup();
    let s = nod_sema::eval_expr_to_string("let bump = method () 42 end; bump() end")
        .expect("eval arity-0 method");
    assert_eq!(s, "42");
}

/// Arity 3.
#[test]
#[serial]
fn arity_3_closure_runs() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let f = method (a, b, c) a + b + c end; f(1, 2, 3) end",
    )
    .expect("eval arity-3 closure");
    assert_eq!(s, "6");
}

/// Arity 4 — distinguishable result so a slot-misordering at the call
/// site would change the output.
#[test]
#[serial]
fn arity_4_closure_runs() {
    setup();
    // Parens required — Dylan has no operator precedence (flat,
    // left-associative), so the products must be grouped explicitly.
    let s = nod_sema::eval_expr_to_string(
        "let f = method (a, b, c, d) (a * 1000) + (b * 100) + (c * 10) + d end; \
         f(1, 2, 3, 4) end",
    )
    .expect("eval arity-4 closure");
    assert_eq!(s, "1234");
}

/// Arity 5 — top of the direct-funcall family.
#[test]
#[serial]
fn arity_5_closure_runs() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let f = method (a, b, c, d, e) \
            (a * 10000) + (b * 1000) + (c * 100) + (d * 10) + e \
         end; \
         f(1, 2, 3, 4, 5) end",
    )
    .expect("eval arity-5 closure");
    assert_eq!(s, "12345");
}

/// Arity 3 with a captured variable — exercises the closure dispatch
/// branch of `nod_funcall3` (the `env_ptr != 0` arm that transmutes to
/// the env-widened `Arity4Fn` signature).
#[test]
#[serial]
fn arity_3_closure_with_capture() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let bias = 100; \
         let f = method (a, b, c) a + b + c + bias end; \
         f(1, 2, 3) end",
    )
    .expect("eval arity-3 with capture");
    assert_eq!(s, "106");
}
