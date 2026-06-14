//! Sprint 52.6 — locus-(B) macro expander, end-to-end pipeline gate.
//!
//! Builds `expand-pipeline-smoke.dylan` (a `main` that uses the stdlib
//! `unless` macro) two ways and runs the result:
//!
//!   * WITH `NOD_EXPAND_WITH_DYLAN=1` — the Dylan front-end expands the
//!     macro before the AST wire (the shim's `dylan-expand-source`); the
//!     build's stderr must show `expand-with-dylan: expanded`, and the
//!     EXE must print `42`.
//!   * WITHOUT the flag — the Rust expander runs in nod-sema instead; the
//!     EXE must also print `42`.
//!
//! Same program, same result, expander self-hosted in the flagged build.
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_pipeline -- --nocapture

use std::path::PathBuf;
use std::process::Command;

use serial_test::serial;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

/// Build the fixture to `out`, optionally with `NOD_EXPAND_WITH_DYLAN=1`.
/// Returns the build's combined stderr.
fn build(out: &PathBuf, with_expand: bool) -> String {
    let workspace = workspace_root();
    let src = fixtures_dir().join("expand-pipeline-smoke.dylan");
    let mut cmd = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"));
    cmd.args(["build"]).arg(&src).arg("-o").arg(out);
    if with_expand {
        cmd.env("NOD_EXPAND_WITH_DYLAN", "1");
    }
    let o = cmd.output().expect("spawn nod-driver build");
    let stderr = String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n");
    assert!(
        o.status.success() && out.is_file(),
        "build (expand={with_expand}) failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        stderr,
    );
    stderr
}

fn run(exe: &PathBuf) -> String {
    let o = Command::new(exe).output().expect("spawn smoke exe");
    assert!(
        o.status.success(),
        "exe {} failed:\nstdout:\n{}\nstderr:\n{}",
        exe.display(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n").trim().to_string()
}

#[test]
#[serial]
fn dylan_expander_pipeline_end_to_end() {
    // Ensure the driver (with the engine-bundled shim) is current.
    let workspace = workspace_root();
    let b = Command::new("cargo")
        .current_dir(&workspace)
        .args(["build", "-p", "nod-driver"])
        .output()
        .expect("spawn cargo build");
    assert!(b.status.success(), "cargo build -p nod-driver failed");

    // WITH the flag: the Dylan expander must fire, and the EXE prints 42.
    let exe_dylan = fixtures_dir().join("expand-pipeline-smoke.dylan-expand.exe");
    let stderr = build(&exe_dylan, true);
    assert!(
        stderr.contains("expand-with-dylan: expanded"),
        "expected the Dylan expander to fire; stderr:\n{stderr}"
    );
    assert_eq!(run(&exe_dylan), "42", "Dylan-expanded build should print 42");

    // WITHOUT the flag (Rust expander in nod-sema): also prints 42.
    let exe_rust = fixtures_dir().join("expand-pipeline-smoke.rust-expand.exe");
    let stderr = build(&exe_rust, false);
    assert!(
        !stderr.contains("expand-with-dylan: expanded"),
        "the unflagged build must NOT run the Dylan expander; stderr:\n{stderr}"
    );
    assert_eq!(run(&exe_rust), "42", "Rust-expanded build should print 42");
}
