//! Sprint 51a — Dylan-side C3 linearisation oracle.
//!
//! Builds `dylan-c3-smoke.dylan` (a Dylan port of `src/nod-sema/src/c3.rs`)
//! and asserts its stdout matches the canonical CPL outputs the Rust
//! tests in `c3.rs` assert. Same algorithm, same inputs, identical
//! outputs — first piece of `nod-sema` running through Dylan code.
//!
//! Run with:
//!   cargo test -p nod-tests --test c3_oracle -- --nocapture

use std::path::PathBuf;
use std::process::Command;

use serial_test::serial;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
#[serial]
fn c3_linearisation_matches_rust_reference() {
    // Fresh driver build so the test always reflects on-disk Dylan.
    let build = Command::new("cargo")
        .current_dir(workspace_root())
        .args(["build", "-p", "nod-driver"])
        .output()
        .expect("spawn cargo build");
    assert!(
        build.status.success(),
        "cargo build -p nod-driver failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let workspace = workspace_root();
    let prj = workspace
        .join("tests")
        .join("nod-tests")
        .join("fixtures")
        .join("dylan-c3-smoke.prj");
    let exe = workspace
        .join("tests")
        .join("nod-tests")
        .join("fixtures")
        .join("dylan-c3-smoke.exe");

    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );

    let run = Command::new(&exe).output().expect("spawn smoke exe");
    assert!(
        run.status.success(),
        "dylan-c3-smoke.exe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );

    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    // Expected lines copied verbatim from the assertions in
    // `src/nod-sema/src/c3.rs`'s tests. Six shapes:
    //   T1 — empty class (no parents) — CPL is just [self]
    //   T2 — SI chain two deep
    //   T3 — SI chain four deep
    //   T4 — classic diamond (Python's E.__mro__ = [E, B, C, A])
    //   T5 — MI with shared grandparent (Python's [C, A, B, X])
    //   T6 — cycle / inconsistent merge — error path
    let expected = "\
T1-empty: <x>\n\
T2-si2: <b> <a> <object>\n\
T3-si4: <d> <c> <b> <a> <object>\n\
T4-diamond: <e> <b> <c> <a>\n\
T5-mi-shared: <c> <a> <b> <x>\n\
T6-cycle: ERROR inconsistent-merge for <child>\n";
    assert_eq!(
        stdout, expected,
        "c3 output diverged:\n--- expected ---\n{expected}--- got ---\n{stdout}",
    );
}
