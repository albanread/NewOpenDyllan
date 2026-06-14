//! Sprint 12 — supplementary dump helpers. Reads the point.dylan
//! fixture and prints the LLVM IR for `<user-point>-getter-x` and the
//! full `dump_classes()` listing. Used by the sprint-acceptance
//! report (`cargo test -p nod-tests --test classes_dumps -- --nocapture`).

use std::path::{Path, PathBuf};

use nod_sema::{dump_classes, dump_llvm_for_file};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn dump_point_llvm_and_classes() {
    let path = fixtures_dir().join("point.dylan");
    let ir = dump_llvm_for_file(&path).expect("dump LLVM IR for point.dylan");
    println!("=== <user-point>-getter-x slice ===");
    for line_block in split_llvm_functions(&ir) {
        if line_block.contains("<user-point>-getter-x") {
            println!("{line_block}");
        }
    }
    println!("=== dump_classes() ===");
    println!("{}", dump_classes());
}

fn split_llvm_functions(ir: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in ir.lines() {
        if line.starts_with("define ") && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
