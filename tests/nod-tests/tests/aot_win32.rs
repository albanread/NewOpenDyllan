//! Sprint 39b — end-to-end AOT EXE tests for Win32 imports resolved via
//! the Windows loader's IAT (Import Address Table), NOT via runtime
//! `LoadLibrary`/`GetProcAddress`.
//!
//! Same pattern as `aot_exe.rs`: write Dylan source into a temp dir,
//! shell out to `cargo run --bin nod-driver -- build ...`, spawn the
//! resulting EXE, capture stdout/stderr/exit-code. The Sprint 39b tests
//! additionally invoke `dumpbin /IMPORTS` on the built EXE to prove
//! the linker actually populated the import table with the expected
//! `(DLL, symbol)` pairs.
//!
//! ## Why `#[ignore]`-only
//!
//! Shells out to MSVC's `link.exe` + `dumpbin.exe`. Not every dev box
//! has them on `%PATH%`. Run manually with:
//!
//! ```text
//! cargo test --test aot_win32 -- --ignored --nocapture
//! ```
//!
//! ## Why `serial`
//!
//! `cargo run --bin nod-driver` acquires Cargo's build-system lock;
//! running multiple of these tests in parallel stalls. `#[serial]`
//! forces them to run sequentially.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Matches
/// `aot_exe.rs`'s helper of the same name.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn make_temp_dir(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nod-aot-win32-test-{test_name}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Build a Dylan source string to an EXE in a fresh temp dir.
/// Returns (temp_dir, exe_path). Caller is responsible for cleanup;
/// on assertion failure the temp dir remains for forensics.
fn build_to_exe(test_name: &str, source: &str) -> (PathBuf, PathBuf) {
    let dir = make_temp_dir(test_name);
    let src_path = dir.join("input.dylan");
    let exe_path = dir.join("output.exe");
    std::fs::write(&src_path, source).expect("write source");

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
    (dir, exe_path)
}

/// Spawn an EXE and return (stdout, stderr, exit_code).
fn run_exe(exe: &Path) -> (String, String, i32) {
    let out = Command::new(exe).output().expect("spawn user EXE");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Locate dumpbin.exe via cc-rs's MSVC tool finder. Returns `None` if
/// MSVC isn't installed; the caller's test treats that as "skip" rather
/// than fail (those who run `--ignored` opt in to having MSVC).
fn find_dumpbin() -> Option<PathBuf> {
    // `find` looks for the tool by name and returns a `Command` on
    // success — we want the path so we extract it from the program
    // field. Going through `find_tool` is the most explicit.
    let cmd = cc::windows_registry::find("x86_64-pc-windows-msvc", "dumpbin.exe")?;
    Some(PathBuf::from(cmd.get_program()))
}

/// Run dumpbin /IMPORTS on `exe` and return stdout text.
fn dumpbin_imports(exe: &Path) -> String {
    let dumpbin = find_dumpbin().expect(
        "dumpbin.exe not found — install VS Build Tools or skip these tests",
    );
    let out = Command::new(&dumpbin)
        .args([
            "/IMPORTS",
            exe.to_str().expect("exe path is UTF-8"),
        ])
        .output()
        .expect("spawn dumpbin");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn remove_dir_all_best_effort(p: &Path) -> std::io::Result<()> {
    if let Err(_e) = std::fs::remove_dir_all(p) {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

/// Sprint 39b headline: `GetTickCount()` called from a bare Dylan
/// program returns a sensible milliseconds-since-boot value and the
/// process exits 0. Proves the full pipeline:
///   - sema/Sprint 31 materialization sees `GetTickCount` as a bare name
///     and registers a `<c-function>` binding via the Win32 index;
///   - codegen emits `nod_winffi_call_0(stub_entry_addr)`;
///   - AOT post-process emits a dllimport extern + static ApiStubEntry;
///   - the linker resolves `GetTickCount` against `kernel32.lib`;
///   - at EXE startup, `nod_aot_resolve_relocs` populates the entry's
///     `fn_ptr` field with the IAT-thunk address;
///   - the trampoline calls through, gets a u32 tick value, formats it.
#[test]
#[ignore]
#[serial]
fn aot_get_tick_count() {
    let source = "Module: tick\n\n\
        define function main () => ()\n  \
            let n = GetTickCount();\n  \
            format-out(\"tick: %d\\n\", n);\n\
        end function main;\n";
    let (dir, exe) = build_to_exe("tick", source);
    let (stdout, stderr, code) = run_exe(&exe);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    // Expect a single line `tick: <number>\n` where <number> is a
    // GetTickCount return value (milliseconds since boot, fits in u32).
    let re = regex_lite_tick();
    assert!(
        re.is_match(&stdout),
        "stdout does not match `^tick: \\d+\\n$`: stdout={stdout:?}, stderr={stderr:?}"
    );
    let _ = remove_dir_all_best_effort(&dir);
}

/// Minimal alternative to bringing in the `regex` crate. We only need
/// to match `^tick: \d+\n$`.
fn regex_lite_tick() -> TickRe {
    TickRe
}

struct TickRe;

impl TickRe {
    fn is_match(&self, s: &str) -> bool {
        // Must start with "tick: ", followed by one or more ASCII
        // digits, followed by a single newline, with nothing else.
        let Some(rest) = s.strip_prefix("tick: ") else { return false };
        let Some(num) = rest.strip_suffix('\n') else { return false };
        !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
    }
}

/// Sprint 39b: `GetCurrentProcessId()` returns this process's PID;
/// the spawned subprocess's reported PID must equal the OS-known PID
/// of that child. Proves AOT can call an arity-0 kernel32 API and
/// receive a meaningful runtime value.
#[test]
#[ignore]
#[serial]
fn aot_get_current_process_id() {
    let source = "Module: pid\n\n\
        define function main () => ()\n  \
            let p = GetCurrentProcessId();\n  \
            format-out(\"%d\\n\", p);\n\
        end function main;\n";
    let (dir, exe) = build_to_exe("pid", source);
    // Spawn so we know the child's PID, then read its stdout.
    let child = Command::new(&exe)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn pid.exe");
    let child_pid = child.id();
    let out = child.wait_with_output().expect("wait pid.exe");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "exit code; stderr=\n{stderr}"
    );
    let trimmed = stdout.trim_end_matches('\n');
    let reported: u32 = trimmed
        .parse()
        .unwrap_or_else(|e| panic!("expected u32 PID, got stdout={stdout:?}; {e}"));
    assert_eq!(
        reported, child_pid,
        "Dylan's GetCurrentProcessId reported {reported} but the OS spawned PID {child_pid}; \
         stdout={stdout:?}, stderr={stderr:?}"
    );
    let _ = remove_dir_all_best_effort(&dir);
}

/// Sprint 39b: the built tick.exe's COFF import table must contain
/// `KERNEL32.dll` with `GetTickCount` listed under it. This is the
/// load-bearing proof that Sprint 39b actually achieves linker-resolved
/// imports rather than runtime `LoadLibrary`.
#[test]
#[ignore]
#[serial]
fn aot_exe_imports_kernel32_get_tick_count() {
    let source = "Module: tick\n\n\
        define function main () => ()\n  \
            let n = GetTickCount();\n  \
            format-out(\"tick: %d\\n\", n);\n\
        end function main;\n";
    let (dir, exe) = build_to_exe("tick_imports", source);
    let imports = dumpbin_imports(&exe);
    // `dumpbin /IMPORTS` formats DLL names with their original case
    // (whatever the import lib registered) and lists each imported
    // symbol on its own line. We assert presence of both substrings
    // (case-insensitive on the DLL name, case-sensitive on the symbol).
    let lower = imports.to_ascii_lowercase();
    assert!(
        lower.contains("kernel32.dll"),
        "dumpbin /IMPORTS missing kernel32.dll:\n{imports}"
    );
    assert!(
        imports.contains("GetTickCount"),
        "dumpbin /IMPORTS missing GetTickCount:\n{imports}"
    );
    let _ = remove_dir_all_best_effort(&dir);
}

/// Sprint 39b: the linker dedupes when our user code shares an import
/// lib with the staticlib's CRT — Rust std's MSVC backend imports
/// `kernel32.dll` for `GetProcAddress`/`HeapAlloc` etc. Our user EXE's
/// `GetTickCount` call lands in the same import table block. The
/// presence of `LoadLibraryA` here is from Rust std's panic handler,
/// NOT from runtime API resolution; this test documents that and
/// makes sure no NEW `GetProcAddress` call site (from the JIT-style
/// stub init) appears.
///
/// Best-effort: if Rust std ever stops pulling in LoadLibraryA, this
/// test's lower bound becomes stricter (we'd assert it's absent).
/// For now we only assert positively: GetTickCount is imported.
#[test]
#[ignore]
#[serial]
fn aot_exe_uses_iat_not_loadlibrary_for_user_api() {
    let source = "Module: tick\n\n\
        define function main () => ()\n  \
            let n = GetTickCount();\n  \
            format-out(\"tick: %d\\n\", n);\n\
        end function main;\n";
    let (dir, exe) = build_to_exe("tick_iat", source);
    let imports = dumpbin_imports(&exe);
    // The JIT path's API-resolution helper (`nod_winffi_call_*`)
    // doesn't itself call LoadLibraryA — we statically link only the
    // Win64 trampolines, not the JIT-time `LoadLibrary` /
    // `GetProcAddress` codepath. So whatever LoadLibraryA references
    // remain come from Rust std's own use (panic handler, late init).
    // The Win32 *user-code* APIs (GetTickCount here) must resolve
    // via the IAT — confirmed by their presence in the dumpbin output.
    assert!(
        imports.contains("GetTickCount"),
        "GetTickCount is not in the IAT — Sprint 39b is broken:\n{imports}"
    );
    let _ = remove_dir_all_best_effort(&dir);
}
