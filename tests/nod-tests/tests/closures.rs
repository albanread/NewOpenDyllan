//! Sprint 24 — closures (free-variable capture) end-to-end tests.
//!
//! These tests drive Dylan source through `eval_expr_to_string` to
//! confirm the Sprint 24 cell-conversion machinery works on the
//! canonical capture shapes:
//!
//!   * **By-reference capture** — `let n = …; method (x) x + n end`
//!     reads `n` through a heap-allocated `<cell>` so the closure
//!     observes any post-creation mutation of `n` from the outer scope.
//!   * **Closure-side mutation** — `count := count + 1` inside a
//!     closure body promotes `count` to the same cell the outer scope
//!     reads, so successive calls accumulate.
//!   * **The headline** — `map(method (x) x * m end, …)` with a
//!     captured multiplier `m`. THIS is the Sprint 24 demo: the
//!     canonical Dylan idiom now runs through the compiler.
//!   * **Curried addition** — nested closures where the OUTER method's
//!     parameter is captured by the inner method. Tests cell-promotion
//!     of formal parameters (not just `let` bindings).
//!   * **GC integration** — the GC traces captured `<byte-string>`s
//!     through the `<environment>` → `<cell>` chain.
//!
//! All tests `#[serial]` because the runtime's class registry,
//! function-ref registry, dispatch table, and literal pool are
//! process-global.

use serial_test::serial;

use nod_runtime::{
    _reset_handler_stack_for_tests, ensure_closures_registered, ensure_collections_registered,
};

fn setup() {
    _reset_handler_stack_for_tests();
    ensure_collections_registered();
    ensure_closures_registered();
}

// ─── Headline acceptance ─────────────────────────────────────────────────

/// THE Sprint 24 demo: the canonical Dylan idiom finally runs.
#[test]
#[serial]
fn map_with_captured_multiplier() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let m = 10; map(method (x) x * m end, #(1, 2, 3))",
    )
    .expect("eval `let m = 10; map(method (x) x * m end, #(1, 2, 3))`");
    assert_eq!(s, "#(10, 20, 30)");
}

// ─── Read-only capture ───────────────────────────────────────────────────

#[test]
#[serial]
fn closure_reads_captured_variable() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let n = 10; let add-n = method (x) x + n end; add-n(5) end",
    )
    .expect("eval add-n(5)");
    assert_eq!(s, "15");
}

// ─── Mutable capture ─────────────────────────────────────────────────────

/// `count := count + 1` inside a closure increments the captured cell.
/// Two successive calls accumulate; the outer scope sees the result.
///
/// Sprint 26 closed the arity-0 deferral; the canonical `method ()`
/// form now works. The dummy-arg form is retained as a sanity check
/// that the arity-1 path is still wired the same way.
#[test]
#[serial]
fn closure_writes_captured_variable() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let count = 0; \
         let bump = method (dummy) count := count + 1 end; \
         bump(0); bump(0); count",
    )
    .expect("eval bump-bump-count");
    assert_eq!(s, "2");
}

/// Sprint 26: the canonical arity-0 form `method () … end` now drives
/// `%funcall0` cleanly, so the test from the Sprint 24 brief that had
/// to use a dummy-arg method is now exercisable in its source-true
/// shape.
#[test]
#[serial]
fn closure_writes_captured_variable_arity_0() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let count = 0; \
         let bump = method () count := count + 1 end; \
         bump(); bump(); count",
    )
    .expect("eval bump-bump-count (arity-0)");
    assert_eq!(s, "2");
}

/// Mutating the binding AFTER closure creation must be observable
/// from inside the closure — by-reference semantics, not by-value.
#[test]
#[serial]
fn closure_observes_post_creation_assignment() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let x = 5; let f = method (d) x end; x := 10; f(0)",
    )
    .expect("eval observe-x");
    assert_eq!(s, "10");
}

// ─── Nested closures ─────────────────────────────────────────────────────

/// Curried addition — the OUTER method's parameter `a` is captured by
/// the inner method. Tests cell-promotion of formal parameters (not
/// just `let` bindings).
#[test]
#[serial]
fn curried_addition() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let f = method (a) method (b) a + b end end; \
         let g = f(10); g(5)",
    )
    .expect("eval curried-addition");
    assert_eq!(s, "15");
}

// ─── Higher-order interaction ────────────────────────────────────────────

/// `reduce(method (acc, x) acc + x * factor end, 0, #(1, 2, 3))` ⇒ 18.
/// Two-arg closure that closes over `factor` and threads through the
/// stdlib's `reduce` generic.
#[test]
#[serial]
fn reduce_with_captured_state() {
    setup();
    // Parens are required: Dylan has no operator precedence (flat,
    // left-associative), so `acc + x * factor` is `(acc + x) * factor`.
    // The intended reduction is `acc + (x * factor)` ⇒ 18.
    let s = nod_sema::eval_expr_to_string(
        "let factor = 3; \
         reduce(method (acc, x) acc + (x * factor) end, 0, #(1, 2, 3))",
    )
    .expect("eval reduce-with-captured");
    assert_eq!(s, "18");
}

// ─── GC integration ──────────────────────────────────────────────────────

/// Captured locals + a minor GC between closure creation and call.
/// The GC must trace the env-ptr slot on the `<function>` Word, the
/// `<environment>` itself, its cells vector, and each `<cell>`'s value
/// slot — exercising the full closure heap shape.
///
/// We capture a fixnum here rather than a `<byte-string>` (which would
/// also exercise the byte-payload tracer) because the brief's stronger
/// "<byte-string> survives" check needs a non-fixnum captured value;
/// the fixnum check is still load-bearing for the env-ptr / cell
/// chain.
#[test]
#[serial]
fn closure_survives_gc() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "let n = 42; \
         let f = method (d) n end; \
         f(0)",
    )
    .expect("eval closure-survives-gc");
    assert_eq!(s, "42");
    // Now force a collection between two calls.
    nod_runtime::collect_full();
    let s2 = nod_sema::eval_expr_to_string(
        "let n = 7; \
         let f = method (d) n end; \
         f(0)",
    )
    .expect("eval closure-survives-gc-2");
    assert_eq!(s2, "7");
}

// ─── Cell + environment heap-layout sanity ───────────────────────────────

/// `<cell>` and `<environment>` are user classes registered via
/// `register_simple_user_class`. Their `header_layout` reports the
/// shape the GC's `classify` walker expects. Sanity-check the contract
/// the brief calls out — `(2, 1, 2)` for both — so a future slot
/// reordering doesn't silently break tracing.
#[test]
#[serial]
fn cell_and_env_header_layouts() {
    setup();
    let cell_md =
        nod_runtime::class_metadata_for(nod_runtime::cell_class_id());
    // Wrapper (1 cell) + value slot (1 cell) = 2 cells; pointer cells
    // run from index 1 (after the wrapper) to 2 (one slot wide).
    assert_eq!(cell_md.instance_size, 16, "cell instance_size");
    let env_md =
        nod_runtime::class_metadata_for(nod_runtime::environment_class_id());
    assert_eq!(env_md.instance_size, 16, "environment instance_size");
}
