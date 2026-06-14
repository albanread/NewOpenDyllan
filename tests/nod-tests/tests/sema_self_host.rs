//! GAP-011 #2 regression — the self-hosted Dylan sema walk must handle an
//! input whose constant/variable tables are empty.
//!
//! `collect-top-names` in `dylan-sema.dylan` insertion-sorts the `fns`,
//! `consts`, and `vars` tables before printing. The sort loops started
//! `let i = 1; until (i = n)`, which for an *empty* table (`n = 0`) does not
//! terminate at the top: `1 = 0` is false, so the body runs and indexes
//! `v[1]` out of bounds. That stray read returns a non-`<byte-string>` Word,
//! which flows into the comparator and aborts in `nod_byte_string_size`
//! (`%byte-string-size: expected <byte-string>`). The fix guards the loops
//! with `until (i >= n)`.
//!
//! `factorial.dylan` is the minimal trigger: two functions, **no** `define
//! constant` / `define variable`, so `consts` and `vars` are empty. Before
//! the fix this crashed deterministically (0 GC); after it, the EXE prints
//! the `=== top-names ===` section and exits 0.
//!
//! `#[ignore]` like the other AOT tests: it shells out to cargo + the linker.
//! Run with:
//!
//! ```text
//! cargo test --test sema_self_host -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

use serial_test::serial;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
#[ignore]
#[serial]
fn dylan_sema_handles_input_with_empty_const_and_var_tables() {
    let ws = workspace_root();
    let prj = fixtures_dir().join("dylan-sema.prj");
    let input = fixtures_dir().join("factorial.dylan");
    let exe = std::env::temp_dir().join("nod-sema-self-host-regr.exe");

    let build = Command::new("cargo")
        .current_dir(&ws)
        .args([
            "run",
            "--quiet",
            "--bin",
            "nod-driver",
            "--",
            "build",
            "--project",
            prj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("spawn dylan-sema build");
    assert!(
        build.status.success(),
        "building dylan-sema failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(exe.is_file(), "dylan-sema EXE not produced at {}", exe.display());

    let run = Command::new(&exe)
        .arg(&input)
        .output()
        .expect("spawn dylan-sema EXE");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    // The empty-table off-by-one aborted in the runtime; a clean exit is the
    // primary signal the regression is gone.
    assert_eq!(
        run.status.code(),
        Some(0),
        "sema EXE did not exit 0 (the empty-table sort crash?):\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("byte-string-size") && !stderr.contains("panicked"),
        "sema EXE panicked:\nstderr:\n{stderr}"
    );

    // And it must produce the two function entries (with empty consts/vars).
    assert!(
        stdout.contains("fn factorial arity=1"),
        "missing factorial entry:\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("fn main arity=0"),
        "missing main entry:\nstdout:\n{stdout}"
    );

    let _ = std::fs::remove_file(&exe);
}
