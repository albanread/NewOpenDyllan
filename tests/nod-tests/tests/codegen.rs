//! Sprint 07 — DFM → LLVM IR codegen + JIT execution.

use std::path::{Path, PathBuf};

use nod_sema::{dump_llvm_for_file, eval_expr_to_string, run_function_to_i64};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn eval_literal_integer() {
    let s = eval_expr_to_string("42").expect("eval `42`");
    assert_eq!(s, "42");
}

#[test]
fn eval_arithmetic_precedence() {
    // Dylan has NO operator precedence: all binary operators share one
    // level and are left-associative (DRM). So `1 + 2 * 3` is
    // `(1 + 2) * 3 = 9`, NOT the C-style `1 + (2 * 3) = 7`. (Sprint 51e
    // fixed the Rust parser, which had wrongly been climbing C-style
    // precedence — see docs/journal/2026-05-31-rust-flat-precedence.md.)
    let s = eval_expr_to_string("1 + 2 * 3").expect("eval `1 + 2 * 3`");
    assert_eq!(s, "9");
}

#[test]
fn eval_let_binding() {
    let s = eval_expr_to_string("let x = 41; x + 1 end").expect("eval `let x = 41; x + 1 end`");
    assert_eq!(s, "42");
}

#[test]
fn eval_float_arithmetic() {
    let s = eval_expr_to_string("3.0 * 2.0").expect("eval `3.0 * 2.0`");
    assert_eq!(s, "6");
}

#[test]
fn eval_if_comparison() {
    let s = eval_expr_to_string("if (1 < 2) 10 else 20 end")
        .expect("eval `if (1 < 2) 10 else 20 end`");
    assert_eq!(s, "10");
}

#[test]
fn factorial_ten_returns_3628800() {
    let path = fixtures_dir().join("factorial.dylan");
    let result = run_function_to_i64(&path, "main").expect("run factorial main");
    assert_eq!(result, 3_628_800);
}

#[test]
fn mutual_call_runs() {
    let path = fixtures_dir().join("mutual.dylan");
    let result = run_function_to_i64(&path, "main").expect("run mutual main");
    assert_eq!(result, 11);
}

#[test]
fn dump_llvm_kernel_arith_shape() {
    let path = fixtures_dir().join("kernel-arith.dylan");
    let ir = dump_llvm_for_file(&path).expect("dump LLVM IR for kernel-arith");
    assert!(
        ir.starts_with("; ModuleID = "),
        "expected `; ModuleID = ` header, got start:\n{}",
        &ir[..ir.len().min(160)]
    );
    for fn_name in ["sq", "abs", "hypot-sq"] {
        assert!(
            ir.contains(fn_name),
            "expected function `{fn_name}` in IR:\n{ir}"
        );
    }
}
