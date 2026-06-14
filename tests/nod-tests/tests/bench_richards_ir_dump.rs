//! Sprint 16 — diagnostic helper to dump LLVM IR for the richards-shape
//! fixtures. Run with:
//!
//! ```text
//! cargo test -p nod-tests --test bench_richards_ir_dump -- \
//!     --ignored --nocapture dump_step_ir
//! ```

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_runtime::{_reset_dispatch_for_tests, _reset_user_classes_for_tests};
use nod_sema::dump_llvm_for_file;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn reset_state() {
    _reset_dispatch_for_tests();
    _reset_user_classes_for_tests();
}

#[test]
#[ignore = "diagnostic helper"]
#[serial]
fn dump_sealed_step_ir() {
    reset_state();
    let ir = dump_llvm_for_file(&fixtures_dir().join("richards-shape.dylan"))
        .expect("dump_llvm sealed");
    let lines: Vec<&str> = ir.lines().collect();
    let mut in_step = false;
    for line in &lines {
        if line.starts_with("define ") && line.contains("@step(") {
            in_step = true;
        }
        if in_step {
            println!("{line}");
            if line.starts_with("}") {
                break;
            }
        }
    }
}

#[test]
#[ignore = "diagnostic helper"]
#[serial]
fn dump_open_step_ir() {
    reset_state();
    let ir = dump_llvm_for_file(&fixtures_dir().join("richards-shape-open.dylan"))
        .expect("dump_llvm open");
    let lines: Vec<&str> = ir.lines().collect();
    let mut in_step = false;
    for line in &lines {
        if line.starts_with("define ") && line.contains("@step(") {
            in_step = true;
        }
        if in_step {
            println!("{line}");
            if line.starts_with("}") {
                break;
            }
        }
    }
}

#[test]
#[ignore = "diagnostic helper"]
#[serial]
fn dump_visit_list_ir() {
    reset_state();
    let ir = dump_llvm_for_file(&fixtures_dir().join("richards-shape.dylan"))
        .expect("dump_llvm sealed");
    let lines: Vec<&str> = ir.lines().collect();
    let mut in_fn = false;
    for line in &lines {
        if line.starts_with("define ") && line.contains("@\"visit-list\"") {
            in_fn = true;
        }
        if in_fn {
            println!("{line}");
            if line.starts_with("}") {
                break;
            }
        }
    }
}
