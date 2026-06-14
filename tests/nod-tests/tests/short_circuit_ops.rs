//! Task #251 — Dylan `|` (or) and `&` (and) must short-circuit.
//!
//! Before the fix `lower_expr`'s BinOp arm evaluated **both** operands
//! eagerly and then called `PrimOp::BoolOr` / `PrimOp::BoolAnd`,
//! which codegen lowered to bitwise `or` / `and` on the raw Word
//! bits. That had two bugs:
//!
//!   1. **No short-circuit**: the right operand always ran, even
//!      when the left determined the result. The bug surfaced in
//!      `nod-ide.dylan` as `until (i = n | element(bs, i) = 10)`
//!      reading out-of-range when `i = n`.
//!   2. **Wrong value for non-boolean operands**: Dylan's `|` and
//!      `&` return the operand value itself (any Word, truthy = "not
//!      `#f`"). Bitwise OR of two heap-pointer Words is nonsense.
//!
//! Fix: lower `Or`/`And` to a 3-block CFG (cur → If; sc_edge with
//! lhs args; rhs_b with rhs args; join with phi). Same env-merge
//! discipline as `lower_if`. See `lower_short_circuit` in
//! `src/nod-sema/src/lower.rs`.

use serial_test::serial;

/// Headline: `|` short-circuits past the RHS when the LHS is truthy.
/// Before the fix, the RHS `element(bs, i)` ran with `i` past the
/// end and signaled `<out-of-range-error>`. With short-circuit, the
/// `i = n` check fires first and the array index never runs.
#[test]
#[serial]
fn or_short_circuits_past_out_of_range_array_index() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    // Use a byte-string instead of a vector — `vector(...)` isn't
    // wired into the eval-entry scope, but `"..."` literals + `size`
    // + `element` are. The semantics are identical for testing
    // short-circuit: indexing past the end signals out-of-range.
    let src = r#"
        begin
          let s = "abc";
          let n = size(s);
          let i = 0;
          // Scan until we hit the end OR find byte 'b' (= 98). The
          // end-check must short-circuit; without it, element(s, 3)
          // signals out-of-range and the test panics.
          // NOTE: the comparands MUST be parenthesised. Dylan has flat
          // (DRM) operator precedence, so `i = n | element(s,i) = 98`
          // would group as `((i = n | element(s,i)) = 98)` — wrong, and
          // it never terminates (the `|` result is then `= 98`-tested).
          until ((i = n) | (element(s, i) = 98))
            i := i + 1;
          end;
          i
        end
    "#;

    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed once `|` short-circuits");
    assert_eq!(s, "1"); // index of 'b' in "abc"
}

/// Symmetric: `&` short-circuits past the RHS when the LHS is false.
#[test]
#[serial]
fn and_short_circuits_past_out_of_range_array_index() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let s = "abc";
          let n = size(s);
          let i = 0;
          // Walk while the index is in-range AND the byte doesn't
          // match 'z' (= 122). The in-range check must short-
          // circuit; without it, `element(s, n)` signals out-of-
          // range. 'z' is not in "abc", so i walks to n = 3.
          // NOTE: parenthesise the comparands — flat (DRM) precedence
          // groups `i < n & element(s,i) ~= 122` as
          // `((i < n & element(s,i)) ~= 122)`, which never terminates.
          while ((i < n) & (element(s, i) ~= 122))
            i := i + 1;
          end;
          i
        end
    "#;

    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed once `&` short-circuits");
    assert_eq!(s, "3");
}

/// Value semantics: `|` returns the LHS Word itself when truthy
/// (NOT a bitwise OR of operand bits).
#[test]
#[serial]
fn or_returns_lhs_value_when_truthy() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let s = nod_sema::eval_expr_to_string("5 | 10").expect("eval");
    assert_eq!(s, "5");
}

/// Value semantics: `|` returns the RHS Word when LHS is `#f`.
#[test]
#[serial]
fn or_returns_rhs_value_when_lhs_false() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let s = nod_sema::eval_expr_to_string("#f | 42").expect("eval");
    assert_eq!(s, "42");
}

/// Value semantics: `&` returns the RHS Word when LHS is truthy.
#[test]
#[serial]
fn and_returns_rhs_value_when_lhs_truthy() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let s = nod_sema::eval_expr_to_string("5 & 10").expect("eval");
    assert_eq!(s, "10");
}

/// Value semantics: `&` returns the LHS (which is `#f`) when falsy.
#[test]
#[serial]
fn and_returns_lhs_value_when_lhs_false() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let s = nod_sema::eval_expr_to_string("#f & 99").expect("eval");
    assert_eq!(s, "#f");
}

/// Env-merge through the short-circuit path: if RHS assigns to a
/// local, the join block must phi the pre-RHS and post-RHS values.
/// Without the phi, code after the `|` either reads the wrong value
/// or fails SSA verification (back-edge issue mirrors Sprint 42-pre's
/// `lower_if` repro).
#[test]
#[serial]
fn or_with_rhs_assignment_phis_correctly() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let src = r#"
        begin
          let touched = #f;
          let r = #f | begin touched := #t; 7 end;
          if (touched) r else -1 end
        end
    "#;
    let s = nod_sema::eval_expr_to_string(src).expect("eval");
    assert_eq!(s, "7"); // RHS ran (lhs falsy), touched=#t, r=7
}

/// Env-merge when the short-circuit path is taken: any name RHS
/// would rebind keeps its pre-RHS value on the sc edge.
#[test]
#[serial]
fn or_short_circuit_preserves_pre_rhs_env() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let src = r#"
        begin
          let touched = #f;
          let r = 1 | begin touched := #t; 7 end;
          // r is 1 (lhs truthy). touched MUST still be #f because
          // the RHS block didn't run. Earlier bug-mode would've
          // either run RHS eagerly (touched = #t) or scrambled the
          // env merge.
          if (touched) -1 else r end
        end
    "#;
    let s = nod_sema::eval_expr_to_string(src).expect("eval");
    assert_eq!(s, "1");
}

/// Chained `|`: a | b | c. Both `|`s should short-circuit
/// independently. With LHS = #f and middle = 5, result is 5 and
/// the third operand never runs (here: would error if it did).
#[test]
#[serial]
fn chained_or_short_circuits_at_first_truthy() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let src = r#"
        begin
          let s = "x";
          #f | 5 | element(s, 99)   // element(s, 99) would signal
        end
    "#;
    let s = nod_sema::eval_expr_to_string(src).expect("eval");
    assert_eq!(s, "5");
}

/// `&` chained: a & b & c. With middle = `#f`, result is `#f`,
/// third operand skipped.
#[test]
#[serial]
fn chained_and_short_circuits_at_first_falsy() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    let src = r#"
        begin
          let s = "x";
          1 & #f & element(s, 99)   // element(s, 99) would signal
        end
    "#;
    let s = nod_sema::eval_expr_to_string(src).expect("eval");
    assert_eq!(s, "#f");
}
