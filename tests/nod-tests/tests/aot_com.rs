//! Sprint 40c — COM (DXGI / D3D11 / D2D / DirectWrite) factory creation
//! in AOT EXEs.
//!
//! Sprint 35 shipped the COM-shim infrastructure (DXGI / D3D11 / D2D /
//! DirectWrite via the `windows` crate); Sprint 40c verifies that the
//! same shims work from a standalone AOT-built Dylan EXE — i.e. the
//! same path Sprint 40b proved for Win32 callbacks, applied to COM.
//!
//! ## What the AOT path inherits
//!
//! Sprint 39a's dual-output (`crate-type = ["rlib", "staticlib"]`)
//! statically links the entire `nod_runtime` surface — including
//! `com_shim.rs` — into every AOT EXE. The `windows` crate's D2D /
//! DXGI / DWrite types and `#[unsafe(no_mangle)]`-exported shim
//! functions (`nod_d2d_create_factory`, `nod_dxgi_create_factory`,
//! `nod_dwrite_create_factory`, `nod_com_release`, …) land in
//! `nod_runtime.lib`; the codegen extern table in `nod-llvm` declares
//! each one against the merged LLVM module, so the linker resolves
//! them out of the staticlib exactly the same way it does for
//! `nod_format_out` or `nod_register_wndproc`.
//!
//! The `nod-driver` link line already carries `ole32.lib`,
//! `oleaut32.lib`, `uuid.lib`, `dxgi.lib`, `d3d11.lib`, `d2d1.lib`,
//! and `dwrite.lib` unconditionally (Sprint 39a comment: "the
//! `windows` crate uses `#[link]` attrs that the staticlib's metadata
//! propagates"). So no driver-side change is needed either.
//!
//! ## Why no `CoInitialize` call
//!
//! Sprint 35 chose factory APIs whose creation entry points are
//! "free" DLL exports — `D2D1CreateFactory`, `CreateDXGIFactory2`,
//! `DWriteCreateFactory`, and `D3D11CreateDevice` do not require COM
//! apartment initialisation (they don't go through `CoCreateInstance`
//! / the registry). The shim JIT tests in `winffi_d2d.rs` confirm
//! this — they never call `CoInitializeEx` and still successfully
//! build the full D2D + DXGI + D3D11 + DWrite chain. The AOT path
//! inherits the same property, so `nod_runtime_init` doesn't need a
//! `CoInitializeEx` call for these factories.
//!
//! Sprint 41+ (DUIM bootstrap) will integrate full COM apartment
//! initialisation if it pulls in helpers like `CoCreateInstance` for
//! WIC/Direct3D11 device enumeration — at that point the right
//! call to add is `CoInitializeEx(None, COINIT_APARTMENTTHREADED)`
//! in `nod_runtime_init`, treating `S_OK` / `S_FALSE` /
//! `RPC_E_CHANGED_MODE` all as success.
//!
//! ## Run with
//!
//! ```text
//! cargo test --test aot_com -- --ignored --nocapture
//! ```
//!
//! Each test is `#[ignore]` because the AOT pipeline shells out to
//! `cargo run --bin nod-driver` plus MSVC's `link.exe`. `#[serial]`
//! prevents concurrent Cargo build-lock contention.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Mirrors the
/// helpers in `aot_exe.rs` / `aot_dylan.rs` / `aot_win32.rs`.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn make_temp_dir(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nod-aot-com-test-{test_name}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Build a Dylan source string to an EXE, run it, return
/// (stdout, stderr, exit_code). On success the temp dir is cleaned up;
/// on failure it's kept for forensic inspection.
fn build_and_run(test_name: &str, source: &str) -> (String, String, i32) {
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

    let exe = Command::new(&exe_path).output().expect("spawn user EXE");
    let stdout = String::from_utf8_lossy(&exe.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&exe.stderr).into_owned();
    let code = exe.status.code().unwrap_or(-1);

    if code == 0 {
        let _ = remove_dir_all_best_effort(&dir);
    }

    (stdout, stderr, code)
}

fn remove_dir_all_best_effort(p: &Path) -> std::io::Result<()> {
    if let Err(_e) = std::fs::remove_dir_all(p) {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

// ─── Sprint 40c headline ──────────────────────────────────────────────────

/// **The Sprint 40c headline.** Create a D2D factory in an AOT-built
/// EXE, verify the returned handle is non-zero (success), release it,
/// and print "ok". Mirrors the simplest end-to-end shape of Sprint 35's
/// JIT test `d2d_factory_creates_handle` in `winffi_d2d.rs`.
///
/// Exercises:
///   * `nod_d2d_create_factory` linked from `nod_runtime.lib`.
///   * The `windows` crate's `D2D1CreateFactory` reaching `d2d1.dll`
///     via the IAT pulled in by `d2d1.lib`.
///   * `nod_com_release` looking up the registered handle and dropping
///     the underlying `ID2D1Factory1` (which fires `Release`).
///   * Sprint 39c's stdlib-merged `format-out` printing after the COM
///     call returns — proves nothing the COM path does leaves the
///     runtime in a state that breaks the merged dispatch table.
#[test]
#[ignore]
#[serial]
fn aot_d2d_factory_smoke() {
    let source = "Module: d2d-factory\n\n\
        define function main () => ()\n  \
            let factory = %d2d-create-factory();\n  \
            if (factory = 0)\n    \
                format-out(\"d2d factory failed\\n\");\n  \
            else\n    \
                format-out(\"d2d factory ok\\n\");\n    \
                %com-release(factory);\n  \
            end if;\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("d2d-factory", source);
    assert_eq!(code, 0, "exit code; stdout=\n{stdout}\nstderr=\n{stderr}");
    assert_eq!(stdout, "d2d factory ok\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Same shape for the DXGI factory — proves the DXGI side of the COM
/// surface (which the swap-chain path needs for Sprint 41 DUIM rendering)
/// works in AOT too. `CreateDXGIFactory2` lives in `dxgi.dll`; the IAT
/// entry comes from `dxgi.lib` already on the driver's link line.
#[test]
#[ignore]
#[serial]
fn aot_dxgi_factory_smoke() {
    let source = "Module: dxgi-factory\n\n\
        define function main () => ()\n  \
            let factory = %dxgi-create-factory();\n  \
            if (factory = 0)\n    \
                format-out(\"dxgi factory failed\\n\");\n  \
            else\n    \
                format-out(\"dxgi factory ok\\n\");\n    \
                %com-release(factory);\n  \
            end if;\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("dxgi-factory", source);
    assert_eq!(code, 0, "exit code; stdout=\n{stdout}\nstderr=\n{stderr}");
    assert_eq!(stdout, "dxgi factory ok\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Same shape for the DirectWrite factory — proves the third major
/// COM surface (text formatting / layout, needed by DUIM for any UI
/// that renders text) works in AOT. `DWriteCreateFactory` lives in
/// `dwrite.dll`; IAT entry from `dwrite.lib` already on the link line.
#[test]
#[ignore]
#[serial]
fn aot_dwrite_factory_smoke() {
    let source = "Module: dwrite-factory\n\n\
        define function main () => ()\n  \
            let factory = %dwrite-create-factory();\n  \
            if (factory = 0)\n    \
                format-out(\"dwrite factory failed\\n\");\n  \
            else\n    \
                format-out(\"dwrite factory ok\\n\");\n    \
                %com-release(factory);\n  \
            end if;\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("dwrite-factory", source);
    assert_eq!(code, 0, "exit code; stdout=\n{stdout}\nstderr=\n{stderr}");
    assert_eq!(stdout, "dwrite factory ok\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Stretch: the full DXGI → D3D11 → D2D device chain, mirroring the
/// initial steps of Sprint 35's headline `d2d_offscreen_renders_text_glyphs`
/// test. This proves vtable method dispatch (`IDXGIDevice` cast to
/// `IDXGIAdapter::GetParent` to `IDXGIFactory2`, `ID2D1Factory1::CreateDevice`,
/// `ID2D1Device::CreateDeviceContext`) all work through COM vtables in
/// an AOT EXE — not just the "free" factory entry points.
///
/// Does NOT render anything (no `BeginDraw`/`EndDraw`/text layout) —
/// just builds the device chain, asserts each handle is non-zero, and
/// releases everything. Safe for headless CI as long as D3D11's WARP
/// software-rasteriser fallback works (it always does on modern Windows).
#[test]
#[ignore]
#[serial]
fn aot_d2d_device_chain() {
    let source = "Module: d2d-chain\n\n\
        define function main () => ()\n  \
            let d3d-device   = %d3d11-create-device();\n  \
            let d2d-factory  = %d2d-create-factory();\n  \
            let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device);\n  \
            let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device);\n  \
            let dc           = %d2d-create-device-context(d2d-device);\n  \
            if (d3d-device = 0)\n      format-out(\"d3d-device failed\\n\");\n  \
            elseif (d2d-factory = 0)\n  format-out(\"d2d-factory failed\\n\");\n  \
            elseif (dxgi-device = 0)\n  format-out(\"dxgi-device failed\\n\");\n  \
            elseif (d2d-device = 0)\n   format-out(\"d2d-device failed\\n\");\n  \
            elseif (dc = 0)\n           format-out(\"dc failed\\n\");\n  \
            else\n    \
                format-out(\"d2d chain ok\\n\");\n    \
                %com-release(dc);\n    \
                %com-release(d2d-device);\n    \
                %com-release(dxgi-device);\n    \
                %com-release(d2d-factory);\n    \
                %com-release(d3d-device);\n  \
            end if;\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("d2d-chain", source);
    assert_eq!(code, 0, "exit code; stdout=\n{stdout}\nstderr=\n{stderr}");
    assert_eq!(stdout, "d2d chain ok\n", "stdout mismatch; stderr=\n{stderr}");
}
