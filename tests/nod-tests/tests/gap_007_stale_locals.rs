//! GAP-007 regression — function-local heap references must not go
//! stale across heavy allocation loops.
//!
//! Diagnosis lives in `docs/COMPILER_GAPS.md` GAP-007. The fix snapshots
//! resolved SSA values at jump-emit time (rather than re-resolving
//! TempIds at end-of-function phi-wiring) so that subsequent
//! `end_safepoint` calls — which rebind `state.temps[t]` to fresh
//! `gc.reload.tN` SSA values defined inside the body block — can no
//! longer poison loop-header phi incomings.
//!
//! Two coverage axes:
//!
//!   1. JIT — runs the fixture through `run_function_to_i64`. Before the
//!      fix this panicked (or returned a garbage discriminant from
//!      `stretchy_vector_push`); after the fix it returns 42.
//!   2. IR shape — dumps the fixture's LLVM IR and asserts the
//!      `phi.t*` at the loop header has TWO distinct incoming values
//!      (the entry-edge value and a body-edge `gc.reload`). If the
//!      bug regressed, both incomings would reference the same
//!      `gc.reload.t*` and LLVM's verifier would have already refused
//!      the IR.
//!
//! The AOT EXE variant is gated `#[ignore]` (matching `aot_dylan.rs`)
//! because it shells out to `cargo run` and `link.exe`; run it
//! explicitly with `cargo test --test gap_007_stale_locals -- --ignored`.

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_sema::{dump_llvm_for_file, run_function_to_i64};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Axis 1 — JIT. Before the GAP-007 fix this either panicked inside
/// `stretchy_vector_push` (`not a <stretchy-vector>`) or surfaced a
/// `<no-applicable-methods-error>` from method dispatch once the
/// vector's storage grew enough times to trip the loop-carried phi.
/// After the fix it returns 42 deterministically.
#[test]
#[serial]
fn jit_loop_carried_locals_survive_allocation_pressure() {
    let path = fixtures_dir().join("gap-007-repro.dylan");
    let result = run_function_to_i64(&path, "main").expect("run gap-007 main");
    assert_eq!(
        result, 42,
        "loop-carried <stretchy-vector> locals went stale across the \
         allocation loop — GAP-007 regressed at runtime"
    );
}

/// Axis 2 — IR shape. Asserts the dumped LLVM IR for the fixture has
/// no `phi.t*` node whose incomings all collapse to the same
/// `gc.reload.t*` SSA. Uses a distinct fixture (`gap-007-repro-ir`)
/// with a distinct class name so the process-global class registry
/// doesn't trip `ClassRedefinitionNotSupported` against the JIT test.
///
/// We deliberately don't pin which phi or which reload — the
/// fixture's loop structure may shift across sema/dfm refactors. What
/// we care about is the structural invariant: at least one phi
/// exists, at least one `gc.reload` exists, and no phi has the "same
/// SSA on every edge" pathology.
#[test]
#[serial]
fn ir_dump_has_no_same_value_phi_incomings() {
    let path = fixtures_dir().join("gap-007-repro-ir.dylan");
    let ir = dump_llvm_for_file(&path).expect("dump LLVM IR");

    // Sanity: the fixture exercises the loop-header phi shape we care
    // about. If sema stopped emitting phis here, the test would silently
    // pass on a fixture that no longer covers the bug.
    assert!(
        ir.contains("phi i64"),
        "expected at least one i64 phi in the dumped IR — fixture no \
         longer exercises the loop-carried-temp shape:\n{ir}"
    );
    // The fixture allocates inside the loop body, so a safepoint reload
    // (`gc.s<N>.reload.tN`) is expected as one phi-incoming. The bug
    // shape was BOTH incomings being the same reload.
    //
    // Naming note: codegen emits per-site reload names of the form
    // `gc.s<site_id>.reload.t<temp_id>` (see
    // `src/nod-llvm/src/codegen.rs:4395`). An earlier draft of this
    // assertion looked for the bare `gc.reload.` literal that does
    // not appear in the actual IR — this is the codegen's current
    // shape.
    assert!(
        ir.contains(".reload.t"),
        "expected at least one safepoint reload (`.reload.tN`) in the \
         dumped IR — fixture no longer exercises safepoint reloads:\n{ir}"
    );

    // Scan each phi line and check no `[ %X, %B1 ], [ %X, %B2 ]` shape
    // where the two values are textually identical. LLVM's textual phi
    // form is `phi <ty> [ <val0>, <bb0> ], [ <val1>, <bb1> ], ...`.
    for (lineno, line) in ir.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("%") || !trimmed.contains(" = phi ") {
            continue;
        }
        let after_phi = match trimmed.find(" = phi ") {
            Some(i) => &trimmed[i + " = phi ".len()..],
            None => continue,
        };
        // Each incoming is `[ %val, %bb ]`. Pull each `%val` out.
        let mut vals: Vec<&str> = Vec::new();
        for inc in after_phi.split("],") {
            // inc is like "[ %tmp, %src" (possibly with trailing space
            // and ']' on the last one).
            let inc = inc.trim().trim_start_matches('[').trim_end_matches(']');
            // Take the part before the first comma.
            if let Some(comma) = inc.find(',') {
                vals.push(inc[..comma].trim());
            }
        }
        if vals.len() < 2 {
            continue;
        }
        // All incomings must not be identical *across all edges*; the
        // GAP-007 pathology was every edge holding the same value. A
        // legitimate phi where two of N edges happen to share a value
        // is fine (e.g. constants); the bug shape was ALL edges
        // collapsed onto one body-block-local SSA. Be strict: if every
        // incoming is the same `%name`, flag it.
        let first = vals[0];
        if vals.iter().all(|v| *v == first) {
            panic!(
                "phi at line {} has all incomings identical ({}): \
                 GAP-007 regressed.\nLine: {}\n",
                lineno + 1,
                first,
                line
            );
        }
    }
}

// ─── Axis 3 — AOT EXE smoke (opt-in via --ignored) ───────────────────────

#[cfg(windows)]
mod aot {
    use super::fixtures_dir;
    use serial_test::serial;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn workspace_root() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
    }

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("nod-gap007-{test_name}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// AOT EXE smoke for the GAP-007 fixture. Build → link → run; the
    /// EXE's `main` returns 42 on success, so `exit_code` is the gate.
    #[test]
    #[ignore]
    #[serial]
    fn aot_exe_loop_carried_locals_survive() {
        let dir = make_temp_dir("aot");
        let src_path = fixtures_dir().join("gap-007-repro.dylan");
        let exe_path = dir.join("gap-007-repro.exe");

        let workspace = workspace_root();
        let build = Command::new("cargo")
            .current_dir(&workspace)
            .args(["build", "-p", "nod-driver", "-p", "nod-runtime"])
            .output()
            .expect("spawn cargo build");
        assert!(
            build.status.success(),
            "cargo build failed:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );

        let driver = Command::new("cargo")
            .current_dir(&workspace)
            .args([
                "run",
                "--quiet",
                "--bin",
                "nod-driver",
                "--",
                "build",
                src_path.to_str().unwrap(),
                "-o",
                exe_path.to_str().unwrap(),
            ])
            .output()
            .expect("spawn nod-driver");
        assert!(
            driver.status.success(),
            "nod-driver build failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&driver.stdout),
            String::from_utf8_lossy(&driver.stderr)
        );
        assert!(
            exe_path.is_file(),
            "EXE not produced at {}",
            exe_path.display()
        );

        let exe = Command::new(&exe_path).output().expect("spawn EXE");
        let code = exe.status.code().unwrap_or(-1);
        assert_eq!(
            code,
            42,
            "EXE exit code {} (expected 42 — GAP-007 regressed in AOT)\n\
             stdout:\n{}\nstderr:\n{}",
            code,
            String::from_utf8_lossy(&exe.stdout),
            String::from_utf8_lossy(&exe.stderr)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
