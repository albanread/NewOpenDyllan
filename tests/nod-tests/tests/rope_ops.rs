//! Sprint 43a — `<rope>` read-only ops end-to-end test.
//!
//! Compiles `tests/nod-tests/fixtures/rope.dylan` to an AOT EXE and
//! runs it. The Dylan source contains a `main` that exercises every
//! Sprint 43a op against a deterministic test buffer and emits one
//! `PASS: <label>` line per assertion. This Rust test asserts every
//! expected PASS line appears in stdout (and no `FAIL:` line does).
//!
//! What the test proves end-to-end:
//!   * user-defined classes with inheritance (`<rope>` → `<rope-leaf>` /
//!     `<rope-node>`) registered + dispatchable in AOT (Sprint 40a)
//!   * multimethod dispatch on user classes for `rope-size`,
//!     `rope-element`, `rope-copy-into`, `for-each-leaf`
//!   * auto-generated slot accessors (`rope-leaf-bytes`,
//!     `rope-node-weight`, …) work as method calls
//!   * `make(<rope-leaf>, bytes: ..., len: ...)` keyword-init
//!   * recursive tree descent (no tail-call yet but tree depth ~3-4
//!     for the 4000-byte test buffer)
//!   * closures over captured cells (`visited := visited + size(leaf)`)
//!     work inside `for-each-leaf` callbacks
//!   * Sprint 42a `<byte-string>` methods (`size`, `element`,
//!     `copy-sequence`, `=`) called from user code in real use
//!   * `%byte-string-allocate` + `%byte-string-element-setter` +
//!     `%byte-string-copy!` primitives called from user code
//!
//! Not interactive — `#[ignore]` only because it's a build-and-spawn
//! test that's slow under parallel pressure. Use `--ignored` to run.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;

fn workspace_root() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    while p.file_name().and_then(|n| n.to_str()) != Some("NewOpenDylan") {
        if !p.pop() {
            panic!("could not find workspace root from test exe");
        }
    }
    p
}

fn make_temp_dir(test_name: &str) -> PathBuf {
    let base = std::env::temp_dir().join("nod-rope-tests");
    let _ = std::fs::create_dir_all(&base);
    let dir = base.join(format!(
        "{}-{}",
        test_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn remove_dir_all_best_effort(p: &Path) -> std::io::Result<()> {
    std::fs::remove_dir_all(p)
}

fn build_exe_from_fixture(test_name: &str, fixture_path: &Path) -> (PathBuf, PathBuf) {
    let dir = make_temp_dir(test_name);
    let exe_path = dir.join("rope.exe");

    let workspace = workspace_root();
    let build = Command::new("cargo")
        .current_dir(&workspace)
        .args(["build", "-p", "nod-driver", "-p", "nod-runtime"])
        .output()
        .expect("spawn cargo build");
    if !build.status.success() {
        panic!(
            "cargo build failed: {}\nstderr:\n{}",
            build.status,
            String::from_utf8_lossy(&build.stderr)
        );
    }

    let driver = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "run",
            "--quiet",
            "--bin",
            "nod-driver",
            "--",
            "build",
            fixture_path.to_str().unwrap(),
            "-o",
            exe_path.to_str().unwrap(),
        ])
        .output()
        .expect("spawn nod-driver");
    if !driver.status.success() {
        panic!(
            "nod-driver build failed: {}\nstdout:\n{}\nstderr:\n{}",
            driver.status,
            String::from_utf8_lossy(&driver.stdout),
            String::from_utf8_lossy(&driver.stderr)
        );
    }
    assert!(
        exe_path.is_file(),
        "EXE not produced at {}",
        exe_path.display()
    );
    (dir, exe_path)
}

/// Compile `rope.dylan` to an AOT EXE, run it, verify every PASS line
/// the self-test promises is present (and no FAIL lines are).
#[test]
#[ignore = "build-and-spawn: rebuilds nod-driver/nod-runtime + a Dylan EXE. \
            Run with `cargo test --test rope_ops -- --ignored --nocapture`."]
#[serial]
fn rope_self_test_passes_under_aot() {
    let workspace = workspace_root();
    let fixture = workspace
        .join("compiler")
        .join("rope.dylan");
    assert!(fixture.is_file(), "fixture missing: {}", fixture.display());

    let (dir, exe) = build_exe_from_fixture("rope-self-test", &fixture);
    eprintln!("[Sprint 43a] rope EXE at {}", exe.display());

    let run = Command::new(&exe)
        .output()
        .expect("spawn rope.exe");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    eprintln!("[Sprint 43a] stdout:\n{stdout}\n[Sprint 43a] stderr:\n{stderr}");

    assert!(
        run.status.success(),
        "rope.exe exited with non-zero status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        run.status
    );

    // Every PASS line the Dylan main promises. Keep these in lockstep
    // with `tests/nod-tests/fixtures/rope.dylan::main`.
    let expected_passes = [
        // Sprint 43a — read-only core
        "PASS: small rope size",
        "PASS: small rope elements",
        "PASS: big rope size",
        "PASS: big rope element pattern",
        "PASS: rope-substring across leaf boundary",
        "PASS: rope-concatenate",
        "PASS: for-each-leaf covers all bytes",
        "PASS: rope-substring full range == original",
        // Sprint 43b — split / insert / delete
        "PASS: rope-split-at boundary + interior sizes",
        "PASS: split-at + concatenate round-trips",
        "PASS: rope-insert at interior position",
        "PASS: rope-insert at start",
        "PASS: rope-insert at end",
        "PASS: rope-delete interior range",
        "PASS: rope-delete prefix",
        "PASS: rope-delete suffix",
        "PASS: rope-insert across leaf boundary grows size correctly",
        "PASS: insert-then-delete round-trips the original",
        "PASS: 200-op GC-stress walk byte-matches reference",
        // Sprint 43c — line indexing
        "PASS: rope-line-count on simple buffers",
        "PASS: rope-line-to-offset on single-leaf buffer",
        "PASS: rope-line-to-offset across leaf boundaries",
        "PASS: rope-line-to-offset / rope-offset-to-line round-trip",
        "PASS: line count tracks through insert + delete",
    ];
    for label in &expected_passes {
        assert!(
            stdout.contains(label),
            "missing expected line `{label}`; full stdout:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("DONE"),
        "main didn't reach end-of-tests marker; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("FAIL:"),
        "at least one rope op failed; stdout:\n{stdout}"
    );

    // Best-effort cleanup. Leave the dir behind if removal fails —
    // useful for forensics on a flaky build.
    let _ = remove_dir_all_best_effort(&dir);
}
