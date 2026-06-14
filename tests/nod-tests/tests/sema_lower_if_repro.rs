//! Sprint 42-pre regression tests for the `lower_if` env-merge fix.
//!
//! Background: `lower_if` previously mutated `env` inside arm bodies
//! (via `lower_assign` rebinding a local-variable name to a fresh SSA
//! temp) but didn't merge those env-state changes with a join-block
//! parameter. When the rebound name was also a loop-header phi
//! target, the loop's back-edge would pick up an arm-local temp that
//! the back-edge block didn't dominate, and LLVM's SSA verifier
//! (correctly) rejected the IR with
//!   `Instruction does not dominate all uses!`.
//!
//! The fix walks both arms upfront with `collect_assigned_in_expr` to
//! find every rebound name, allocates a join-block parameter per
//! name, and routes each arm's jump with the right args (using the
//! pre-if env value when an arm didn't touch a particular name). The
//! caller (let-binding, sequence statement, etc.) then sees the
//! join-block parameter as the post-if value of the name.
//!
//! These tests exercise the patterns that triggered the bug in
//! practice — primarily Sprint 42a's `<byte-string>` stdlib methods,
//! but the failure is independent of byte-strings (it's purely an
//! `if` + `:=` + `until` interaction).

use serial_test::serial;

/// THE repro: pure-integer/boolean reproduction of the SSA dominance
/// bug Sprint 42a uncovered. Before the fix this hits
/// `Instruction does not dominate all uses!` during JIT verification;
/// after the fix it returns `"1"` (mismatch becomes true once `i`
/// reaches 3, which then short-circuits the loop).
#[test]
#[serial]
fn until_with_then_only_var_rebind_returns_correct_value() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let mismatch = #f;
          let i = 0;
          until (i = 5 | mismatch)
            if (i = 3)
              mismatch := #t;
            else
              #f
            end;
            i := i + 1;
          end;
          if (mismatch) 1 else 0 end
        end
    "#;

    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed after lower_if env-merge fix");
    assert_eq!(s, "1");
}

/// Symmetric form: else-only rebind. Loop counts to 3, then the
/// else-arm sets `done := #t`, exiting the loop. Result: `i = 3`.
#[test]
#[serial]
fn until_with_else_only_var_rebind_returns_correct_value() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let done = #f;
          let i = 0;
          until (i = 10 | done)
            if (i < 3)
              #f
            else
              done := #t;
            end;
            i := i + 1;
          end;
          i
        end
    "#;

    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed after lower_if env-merge fix");
    // i increments through 0, 1, 2 (all then-arm, no done), then i=3
    // hits else-arm, sets done, increments to 4, loop exits.
    assert_eq!(s, "4");
}

/// Both arms rebind the same name, and the arm bodies use the value
/// AFTER the if (via let-binding the if-value). Confirms the
/// join-block parameter and the named-var join params coexist.
#[test]
#[serial]
fn if_with_both_arms_rebinding_and_value_used_after() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let acc = 0;
          let i = 0;
          until (i = 5)
            let bump = if (i < 3)
                         acc := acc + 1;
                         10
                       else
                         acc := acc + 100;
                         20
                       end;
            i := i + 1;
          end;
          acc
        end
    "#;

    // i = 0,1,2: acc += 1 (×3 = 3). i = 3,4: acc += 100 (×2 = 200).
    // Total: 203.
    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed after lower_if env-merge fix");
    assert_eq!(s, "203");
}

/// Nested if-in-if where the inner then-arm rebinds: confirms the
/// fix composes correctly through nested control flow.
#[test]
#[serial]
fn nested_if_with_then_arm_rebind_composes() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let n = 0;
          let i = 0;
          until (i = 4)
            if (i > 0)
              if (i = 2)
                n := n + 10;
              else
                n := n + 1;
              end;
            else
              #f
            end;
            i := i + 1;
          end;
          n
        end
    "#;

    // i=0: outer-else (no-op). i=1: inner-else → n=1. i=2: inner-then → n=11.
    // i=3: inner-else → n=12. Result: 12.
    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed after lower_if env-merge fix");
    assert_eq!(s, "12");
}

/// Sprint 45e follow-up: a GC-managed binding that is only used AFTER a
/// loop still has to be threaded through the loop-header phi set. Before the
/// fix, the body's inner `if` could leave the loop-exit path reading a
/// body-local join temp for `survivor`, and LLVM rejected the JIT IR with
/// `Instruction does not dominate all uses!`.
#[test]
#[serial]
fn post_loop_live_gc_root_is_carried_through_loop_header() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();

    let src = r#"
        begin
          let survivor = copy-sequence("abc");
          let seen = -1;
          let i = 0;
          until (i = 2)
            if (i = 0)
              seen := i + 10;
            else
              #f
            end;
            copy-sequence("z");
            i := i + 1;
          end;
          if (size(survivor) = 3) 1 else 0 end
        end
    "#;

    let s = nod_sema::eval_expr_to_string(src)
        .expect("eval should succeed after loop-header GC-root carry fix");
    assert_eq!(s, "1");
}
