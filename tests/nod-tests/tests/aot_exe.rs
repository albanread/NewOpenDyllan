//! Sprint 39a — end-to-end AOT EXE tests.
//!
//! Each test:
//!   1. Writes a Dylan source file into a temp directory.
//!   2. Shells out to `cargo run --bin nod-driver -- build <src> -o <exe>`.
//!   3. Spawns the resulting `.exe` and captures stdout + exit code.
//!   4. Asserts both match expectations.
//!
//! ## Why `#[ignore]`-only
//!
//! These tests shell out to MSVC's `link.exe`, which not every
//! development machine has on `%PATH%`. The Sprint 39a brief mandates
//! `#[ignore]` so routine `cargo test --workspace` runs stay green on
//! barebones CI / non-VS-installed dev boxes.
//!
//! Run manually with:
//!
//! ```text
//! cargo test --test aot_exe -- --ignored --nocapture
//! ```
//!
//! ## Why subprocess + temp dir
//!
//! `cargo run --bin nod-driver` re-uses the workspace's `target/debug`
//! directory so the in-process `nod_runtime.lib` is the same artifact
//! the parent test session is linked against — no extra `cargo build`
//! step needed. The temp dir keeps `.dylan`, `.obj`, and `.exe`
//! artifacts isolated per test so concurrent invocations can't
//! clobber each other's outputs.
//!
//! Cleanup: best-effort. On success we remove the temp dir; on failure
//! the artifacts are kept so a developer can re-run `link.exe` by hand
//! and inspect the IR / object files. The temp-dir prefix
//! (`nod-aot-exe-test-`) makes them easy to clean up manually.
//!
//! ## Why `serial`
//!
//! Cargo's test runner spawns tests in parallel by default. Each test
//! here invokes a fresh `cargo run --bin nod-driver` which acquires
//! Cargo's build-system lock; running them concurrently leads to
//! "blocking waiting for file lock" stalls in CI. `serial_test::serial`
//! forces them to run one at a time.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Subprocess
/// invocations of `cargo` use this so `cargo run --bin nod-driver`
/// resolves to the workspace's nod-driver crate.
fn workspace_root() -> PathBuf {
    // The test runner sets `CARGO_MANIFEST_DIR` to
    // `<workspace>/tests/nod-tests`; the workspace root is two levels
    // up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Per-test temp directory. Hand-rolled (no `tempfile` dep). Returns
/// the directory path and the test name suffix used for uniqueness.
fn make_temp_dir(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nod-aot-exe-test-{test_name}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Drive the full Sprint 39a pipeline. Writes `src` to `dir/<stem>.dylan`,
/// invokes `cargo run --bin nod-driver -- build ...`, spawns the resulting
/// EXE, returns (stdout, stderr, exit_code).
fn build_and_run(test_name: &str, source: &str) -> (String, String, i32) {
    build_and_run_with_env(test_name, source, &[])
}

fn build_and_run_with_env(
    test_name: &str,
    source: &str,
    envs: &[(&str, &str)],
) -> (String, String, i32) {
    let dir = make_temp_dir(test_name);
    let src_path = dir.join("input.dylan");
    let exe_path = dir.join("output.exe");
    std::fs::write(&src_path, source).expect("write source");

    // First ensure nod-runtime + nod-driver are fresh. Re-running `cargo
    // build -p nod-driver` is a no-op if already built and avoids race
    // windows where the staticlib is out-of-date.
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
            src_path.to_str().unwrap(),
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
    assert!(exe_path.is_file(), "EXE not produced at {}", exe_path.display());

    // Run the EXE in a fresh process to avoid env-var contamination
    // from the cargo runtime. We do NOT set `current_dir` — the EXE
    // doesn't read any files, only writes stdout — so the working
    // directory is whatever cargo passed us; that's fine.
    let mut exe_cmd = Command::new(&exe_path);
    for (key, value) in envs {
        exe_cmd.env(key, value);
    }
    let exe = exe_cmd.output().expect("spawn user EXE");
    let stdout = String::from_utf8_lossy(&exe.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&exe.stderr).into_owned();
    let code = exe.status.code().unwrap_or(-1);

    // Best-effort cleanup on success. On failure (caller's assertion
    // fires after this returns), the temp dir is left in place so a
    // developer can inspect.
    if code == 0 {
        let _ = remove_dir_all_best_effort(&dir);
    }

    (stdout, stderr, code)
}

fn remove_dir_all_best_effort(p: &Path) -> std::io::Result<()> {
    // Retry once after a brief pause — Windows can hold file handles
    // briefly after process exit.
    if let Err(_e) = std::fs::remove_dir_all(p) {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

/// Sprint 39a's headline test: `define function main () => () format-out("Hello, world\n") end`
/// produces an EXE that prints exactly `"Hello, world\n"` and returns 0.
#[test]
#[ignore]
#[serial]
fn aot_hello_world() {
    let source = "Module: hello\n\n\
        define function main () => ()\n  \
            format-out(\"Hello, world\\n\");\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("hello", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "Hello, world\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Sprint 39a: arithmetic + `%d` formatting. Demonstrates fixnum
/// arithmetic + literal interpolation in the AOT path.
#[test]
#[ignore]
#[serial]
fn aot_arithmetic() {
    let source = "Module: arith\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", 6 * 7);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("arith", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "42\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Sprint 39c: end-to-end exercise of stdlib pre-compilation. The
/// program `size(make(<range>, from: 0, to: 5))` exercises:
///   * Sprint 38c class-metadata relocations (`<range>`).
///   * Sprint 38e cache-slot + generic-function relocations
///     (`size`'s inline cache, `\size`).
///   * Sprint 39c stdlib merging: the user's `.obj` contains the
///     stdlib's `size$<range>` method body and the resolver function
///     registers it with the dispatch table at startup.
///
/// Sprint 39a / 39b documented this as `aot_dispatch_deferred_to_39c`
/// with an expected-failure pattern; Sprint 39c flips it to a real
/// positive assertion (renamed back to `aot_dispatch`).
#[test]
#[ignore]
#[serial]
fn aot_dispatch() {
    let source = "Module: dispatch\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", size(make(<range>, from: 0, to: 5)));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("dispatch", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "6\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Sprint 45c: prove the image safepoint table is consumed on the real
/// AOT execution path. The EXE is built through `nod-driver build`, then
/// run with `NOD_AOT_TRACE_SAFEPOINTS=1`; the runtime registration shim
/// emits a trace line on stderr when the codegen-baked safepoint table is
/// registered during startup, before Dylan `main` runs.
#[test]
#[ignore]
#[serial]
fn aot_startup_registers_image_safepoints() {
    let source = "Module: traced\n\n\
        define function alloc-a (x) => (y)\n  \
            x;\n\
        end function alloc-a;\n\
        define function main () => ()\n  \
            alloc-a(41);\n\
            format-out(\"startup-ok\\n\");\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run_with_env(
        "trace-safepoints",
        source,
        &[
            ("NOD_AOT_TRACE_SAFEPOINTS", "1"),
            ("NOD_AOT_TRACE_EXEC_SAFEPOINTS", "1"),
        ],
    );
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "startup-ok\n", "stdout mismatch; stderr=\n{stderr}");
    let trace_line = stderr
        .lines()
        .find(|line| line.starts_with("nod-aot: registered "))
        .unwrap_or_else(|| panic!("missing safepoint registration trace; stderr=\n{stderr}"));
    let count_text = trace_line
        .strip_prefix("nod-aot: registered ")
        .and_then(|rest| rest.strip_suffix(" image safepoints"))
        .unwrap_or_else(|| panic!("malformed safepoint registration trace: {trace_line}"));
    let count: usize = count_text
        .parse()
        .unwrap_or_else(|_| panic!("non-numeric safepoint registration trace count: {trace_line}"));
    assert!(count > 0, "expected positive safepoint count; trace={trace_line}");
    assert!(
        stderr.lines().any(|line| line.starts_with("nod-aot: begin safepoint site ")),
        "missing executed safepoint begin trace; stderr=\n{stderr}"
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("nod-aot: verified safepoint site ")),
        "missing executed safepoint verify trace; stderr=\n{stderr}"
    );
    assert!(
        stderr.lines().any(|line| line.starts_with("nod-aot: end safepoint site ")),
        "missing executed safepoint end trace; stderr=\n{stderr}"
    );
}
