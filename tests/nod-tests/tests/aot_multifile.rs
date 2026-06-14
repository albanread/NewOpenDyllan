//! Sprint 44 — multi-file (same-module) AOT compilation tests.
//!
//! Validates `nod-driver build a.dylan b.dylan ...` end-to-end:
//! the per-file lower path, the `compile_files_for_aot` merge, the
//! `Module:`-header consistency check, and the cross-file duplicate-
//! definition check. Mirrors the harness pattern of `aot_dylan.rs`:
//! every test is `#[ignore]` because it shells out to cargo + the
//! linker, and `serial_test::serial` keeps concurrent invocations
//! from stalling on Cargo's build-system lock.
//!
//! Run with:
//!
//! ```text
//! cargo test --test aot_multifile -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn make_temp_dir(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nod-aot-multifile-{test_name}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn remove_dir_all_best_effort(p: &Path) -> std::io::Result<()> {
    if let Err(_e) = std::fs::remove_dir_all(p) {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

/// Build a set of (filename, source) pairs to an EXE in a fresh temp
/// dir using `nod-driver build` with multiple positional file args.
/// Run the EXE and return (stdout, stderr, exit_code). On success the
/// temp dir is removed; on failure it's preserved for inspection.
fn build_multi_and_run(
    test_name: &str,
    files: &[(&str, &str)],
) -> (String, String, i32) {
    assert!(!files.is_empty(), "build_multi_and_run needs at least one file");
    let dir = make_temp_dir(test_name);
    let mut src_paths: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (name, contents) in files {
        let p = dir.join(name);
        std::fs::write(&p, contents).expect("write source file");
        src_paths.push(p);
    }
    let exe_path = dir.join("output.exe");

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

    // Assemble: `nod-driver build <file1> <file2> ... -o out.exe`
    let mut args: Vec<String> = vec![
        "run".into(),
        "--quiet".into(),
        "--bin".into(),
        "nod-driver".into(),
        "--".into(),
        "build".into(),
    ];
    for p in &src_paths {
        args.push(p.to_str().unwrap().to_string());
    }
    args.push("-o".into());
    args.push(exe_path.to_str().unwrap().to_string());

    let driver = Command::new("cargo")
        .current_dir(&workspace)
        .args(&args)
        .output()
        .expect("spawn nod-driver");
    // We DON'T panic on driver failure here — the negative tests below
    // rely on the driver exiting non-zero with a diagnostic on stderr.
    // Tests that need a successful build assert `code == 0` themselves.
    let driver_stdout = String::from_utf8_lossy(&driver.stdout).into_owned();
    let driver_stderr = String::from_utf8_lossy(&driver.stderr).into_owned();
    let driver_code = driver.status.code().unwrap_or(-1);

    if driver_code != 0 {
        // Driver failed — surface the diagnostic as our "stderr" + a
        // sentinel exit code so the test can assert on the message.
        return (driver_stdout, driver_stderr, driver_code);
    }
    assert!(
        exe_path.is_file(),
        "EXE not produced at {} despite driver exit 0",
        exe_path.display()
    );

    let exe = Command::new(&exe_path).output().expect("spawn user EXE");
    let stdout = String::from_utf8_lossy(&exe.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&exe.stderr).into_owned();
    let code = exe.status.code().unwrap_or(-1);

    if code == 0 {
        let _ = remove_dir_all_best_effort(&dir);
    }

    (stdout, stderr, code)
}

// ─── Happy path: two files in the same module link and run ───────────

/// Headline: two files share `Module: greet`. `helpers.dylan` defines
/// `greeting()`, `main.dylan` defines `main()` and calls it. The EXE
/// must build, link, and print "hello from helpers".
///
/// This exercises the full multi-file pipeline:
///   - `compile_files_for_aot` lowers each file in order
///   - `merge_modules` concatenates the two `LoweredModule`s
///   - codegen emits one merged LLVM module with both functions
///   - the AOT resolver registers both top-level bodies
///   - `main` calls `greeting` through the standard funcall path
#[test]
#[ignore]
#[serial]
fn aot_multifile_two_files_same_module() {
    let helpers = "Module: greet\n\n\
        define function greeting () => ()\n  \
            format-out(\"hello from helpers\\n\");\n\
        end function;\n";
    let main = "Module: greet\n\n\
        define function main () => ()\n  \
            greeting();\n\
        end function main;\n";
    let (stdout, stderr, code) = build_multi_and_run(
        "two-files",
        &[("helpers.dylan", helpers), ("main.dylan", main)],
    );
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "hello from helpers\n", "stdout; stderr=\n{stderr}");
}

// ─── Negative path: Module: header mismatch errors out ──────────────

/// File A declares `Module: foo`, file B declares `Module: bar`. The
/// driver must reject the build with a clear diagnostic mentioning
/// both file paths and the conflicting module names.
#[test]
#[ignore]
#[serial]
fn aot_multifile_module_header_mismatch_errors() {
    let a = "Module: foo\n\n\
        define function helper () => ()\n  \
            format-out(\"unused\\n\");\n\
        end function;\n";
    let b = "Module: bar\n\n\
        define function main () => ()\n  \
            helper();\n\
        end function main;\n";
    let (_stdout, stderr, code) = build_multi_and_run(
        "module-mismatch",
        &[("a.dylan", a), ("b.dylan", b)],
    );
    assert_ne!(code, 0, "expected driver to exit non-zero");
    assert!(
        stderr.contains("module-header mismatch"),
        "diagnostic should mention `module-header mismatch`; got:\n{stderr}"
    );
    assert!(
        stderr.contains("foo") && stderr.contains("bar"),
        "diagnostic should mention both module names; got:\n{stderr}"
    );
}

// ─── Negative path: duplicate definition across files errors out ────

/// Both files declare `define function helper`. The driver must
/// reject with a diagnostic naming the duplicated symbol and both
/// source paths.
#[test]
#[ignore]
#[serial]
fn aot_multifile_duplicate_user_definition_errors() {
    let a = "Module: dup\n\n\
        define function helper () => ()\n  \
            format-out(\"from a\\n\");\n\
        end function;\n";
    let b = "Module: dup\n\n\
        define function helper () => ()\n  \
            format-out(\"from b\\n\");\n\
        end function;\n\n\
        define function main () => ()\n  \
            helper();\n\
        end function main;\n";
    let (_stdout, stderr, code) = build_multi_and_run(
        "duplicate-def",
        &[("a.dylan", a), ("b.dylan", b)],
    );
    assert_ne!(code, 0, "expected driver to exit non-zero");
    assert!(
        stderr.contains("duplicate top-level definition"),
        "diagnostic should mention `duplicate top-level definition`; got:\n{stderr}"
    );
    assert!(
        stderr.contains("helper"),
        "diagnostic should mention the duplicated name `helper`; got:\n{stderr}"
    );
}
