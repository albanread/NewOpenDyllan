//! Sprint 41a — the IDE-shell demo as a standalone AOT EXE.
//!
//! THIS TEST POPS A REAL WIN32 WINDOW. It is `#[ignore]`-gated so
//! routine `cargo test` doesn't disturb the user's screen. Run
//! manually with:
//!
//! ```text
//! cargo test --test aot_ide_shell -- --ignored --nocapture
//! ```
//!
//! When you run this, a window titled "NewOpenDylan IDE" appears
//! showing "hello, dylan" rendered via DirectWrite through Direct2D
//! into an HWND-bound DXGI swap chain. Click the close box (X) to
//! close it; the test then asserts the EXE exited with code 0.
//!
//! ## What this proves
//!
//! Sprint 36 shipped the JIT-side IDE shell as `ide_shell.rs`. Sprint
//! 41a both completes that JIT test (replacing the `Sleep(5000)`
//! placeholder with a real blocking message loop) AND lifts the same
//! Dylan source body into an AOT-built EXE — the user's "real Windows
//! app" criterion. Every Sprint-39+40 deliverable has to compose for
//! this to work:
//!
//!   * Sprint 39a's `nod_runtime_init` → eager class / condition /
//!     C-FFI-error registration before user main runs.
//!   * Sprint 39b's IAT-resolved Win32 imports → `CreateWindowExW`,
//!     `ShowWindow`, `UpdateWindow`, `DefWindowProcW`, `PostQuitMessage`
//!     all wired via `dllimport` declarations the linker satisfies
//!     out of `user32.lib`.
//!   * Sprint 39c's merged stdlib → the user code's `format-out`-free
//!     body still drags in dispatch metadata for `<integer>` arithmetic
//!     and the `as-wndproc-callback` stdlib helper.
//!   * Sprint 40b's Win32 callbacks → `as-wndproc-callback`'s
//!     `nod_register_wndproc` call lands in the staticlib-linked
//!     `callbacks.rs` trampoline pool, hands back a real C-ABI
//!     function pointer the OS can invoke.
//!   * Sprint 40c's COM in AOT → DXGI / D3D11 / D2D / DirectWrite
//!     factories + device chain + bitmap creation all reachable from
//!     `nod_runtime.lib`.
//!   * Sprint 40d's bare-name Win32 calls → `PostQuitMessage`,
//!     `DefWindowProcW`, `CreateWindowExW`, `ShowWindow`,
//!     `UpdateWindow` are the bare-name path (no explicit
//!     `define c-function`).
//!   * Sprint 41a's `%run-message-loop()` → the standardlib's
//!     newly-added blocking `GetMessage`/`Translate`/`Dispatch` loop
//!     primitive, statically linked into the EXE via the
//!     `nod_run_message_loop` shim in `com_shim.rs`.
//!
//! ## Why explicit `define c-function` declarations
//!
//! The Dylan source below carries the same `define c-function`
//! declarations as the JIT IDE-shell test (`ide_shell.rs`'s
//! `IDE_SHELL_DECL`). Sprint 31's bare-name Win32 materialization
//! works for both the JIT and AOT pipelines, but `CreateWindowExW`'s
//! second arg (`lpClassName`, `LPCWSTR` in `windows_api.db`) gets
//! classified as a string-typed arg by sema; when the test passes the
//! `atom` Word from `%register-window-class` (a fixnum, not a
//! `<byte-string>`), the winffi marshaler panics on string-shape
//! coercion. The JIT test sidestepped this from Sprint 36 by
//! declaring `lpClassName` as `<c-pointer>` (an integer-shaped arg)
//! via an explicit `define c-function`. The AOT test mirrors that
//! exact shape — both pipelines route through the same lowering
//! path for declared c-functions, so what works for JIT works
//! identically for AOT. Tightening sema's bare-name LPCWSTR
//! classification (allowing it to accept integer-shaped args where
//! the parameter is documented as accepting an atom) is a follow-up
//! cleanup, not part of Sprint 41a.

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
    let dir = std::env::temp_dir().join(format!("nod-aot-ide-test-{test_name}-{nanos}"));
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

/// Build the Dylan source to an EXE under a temp dir, return the EXE
/// path. The temp dir is kept for forensic inspection — it's cleaned
/// up by the caller on success only. Panics on build failure.
/// Sprint 44 — build an EXE from a set of existing fixture files,
/// passing every path to `nod-driver build` as a positional argument.
/// Mirrors `build_exe` but expects on-disk fixtures (no `include_str!`
/// trick) so the multi-file IDE split can be driven through the real
/// `nod-driver build a.dylan b.dylan c.dylan ...` code path.
fn build_exe_from_fixtures(
    test_name: &str,
    fixture_paths: &[PathBuf],
) -> (PathBuf, PathBuf) {
    assert!(
        !fixture_paths.is_empty(),
        "build_exe_from_fixtures needs at least one path"
    );
    for p in fixture_paths {
        assert!(
            p.is_file(),
            "fixture path {} is not a regular file",
            p.display()
        );
    }
    let dir = make_temp_dir(test_name);
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

    let mut args: Vec<String> = vec![
        "run".into(),
        "--quiet".into(),
        "--bin".into(),
        "nod-driver".into(),
        "--".into(),
        "build".into(),
    ];
    for p in fixture_paths {
        args.push(p.to_str().unwrap().to_string());
    }
    args.push("-o".into());
    args.push(exe_path.to_str().unwrap().to_string());

    let driver = Command::new("cargo")
        .current_dir(&workspace)
        .args(&args)
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

fn build_exe(test_name: &str, source: &str) -> (PathBuf, PathBuf) {
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
    assert!(
        exe_path.is_file(),
        "EXE not produced at {}",
        exe_path.display()
    );
    (dir, exe_path)
}

/// **The Sprint 41a headline.** Build an AOT-linked EXE from the same
/// Dylan source the JIT IDE-shell test (`ide_shell.rs`) uses, launch
/// it, and wait for the window to close. The test blocks here until
/// the user clicks the X box on the window; PostQuitMessage(0) inside
/// the WNDPROC's WM_DESTROY handler signals the message loop to exit,
/// the message loop returns 0, the main function returns, and the
/// process exits with code 0.
///
/// Acceptance: the EXE exits with status 0 within a reasonable bound
/// of user patience. The test framework's `.wait()` blocks until the
/// child process exits — so the test only completes after the human
/// has interacted with the window.
///
/// `#[ignore]`-gated because it's interactive (window pops on the
/// user's desktop). `#[serial]` prevents concurrent Cargo build-lock
/// contention with other AOT tests.
#[test]
#[ignore = "interactive: pops a real Win32 window. Run with `cargo test --test aot_ide_shell -- --ignored --nocapture`."]
#[serial]
fn aot_ide_shell_window_renders_hello_dylan() {
    // Identical Dylan body to the JIT test in `ide_shell.rs`, wrapped
    // as a `define function main`. The `%`-prefixed primitives all
    // resolve to staticlib symbols in `nod_runtime.lib`. The explicit
    // `define c-function` declarations match `ide_shell.rs`'s
    // `IDE_SHELL_DECL` — `CreateWindowExW`'s `lpClassName` needs to be
    // typed as `<c-pointer>` (an integer-shaped arg) so the
    // `%register-window-class` atom Word reaches Win32 as a raw int
    // rather than going through the string-marshaling path.
    let source = "Module: ide-shell\n\n\
        define c-function CreateWindowExW\n  \
            (dwExStyle :: <c-int>, lpClassName :: <c-pointer>, lpWindowName :: <c-wide-string>,\n   \
             dwStyle :: <c-int>, x :: <c-int>, y :: <c-int>, nWidth :: <c-int>, nHeight :: <c-int>,\n   \
             hWndParent :: <c-pointer>, hMenu :: <c-pointer>, hInstance :: <c-pointer>,\n   \
             lpParam :: <c-pointer>)\n   \
         => (hwnd :: <c-pointer>);\n    \
            library: \"user32.dll\";\n\
        end;\n\n\
        define c-function ShowWindow\n  \
            (hwnd :: <c-pointer>, nCmdShow :: <c-int>)\n   \
         => (was-visible :: <c-bool>);\n    \
            library: \"user32.dll\";\n\
        end;\n\n\
        define c-function UpdateWindow\n  \
            (hwnd :: <c-pointer>)\n   \
         => (success :: <c-bool>);\n    \
            library: \"user32.dll\";\n\
        end;\n\n\
        define c-function DefWindowProcW\n  \
            (hwnd :: <c-pointer>, msg :: <c-int>,\n   \
             wparam :: <c-pointer>, lparam :: <c-pointer>)\n   \
         => (lresult :: <c-pointer>);\n    \
            library: \"user32.dll\";\n\
        end;\n\n\
        define c-function PostQuitMessage\n  \
            (exit-code :: <c-int>)\n   \
         => ();\n    \
            library: \"user32.dll\";\n\
        end;\n\n\
        define function main () => ()\n  \
            let d3d-device   = %d3d11-create-device();\n  \
            let dxgi-factory = %dxgi-factory-from-d3d-device(d3d-device);\n  \
            let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device);\n  \
            let d2d-factory  = %d2d-create-factory();\n  \
            let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device);\n  \
            let dc           = %d2d-create-device-context(d2d-device);\n  \
            let dwrite       = %dwrite-create-factory();\n  \
            let format       = %dwrite-create-text-format(dwrite, \"Segoe UI\", 2400, \"en-us\");\n  \
            let swap = 0;\n  \
            let bitmap = 0;\n  \
            let wp = method (hwnd, msg, wparam, lparam)\n            \
                       if (msg = 15)\n              \
                         if (swap ~= 0)\n                \
                           if (bitmap = 0)\n                  \
                             bitmap := %d2d-create-bitmap-from-swap-chain(dc, swap);\n                \
                           else 0 end;\n                \
                           %d2d-set-target(dc, bitmap);\n                \
                           %d2d-begin-draw(dc);\n                \
                           %d2d-clear(dc, 255, 255, 255, 255);\n                \
                           let brush  = %d2d-create-solid-color-brush(dc, 0, 0, 0, 255);\n                \
                           let layout = %dwrite-create-text-layout(dwrite, \"hello, dylan\", format, 800, 600);\n                \
                           %d2d-draw-text-layout(dc, 50, 50, layout, brush);\n                \
                           %d2d-end-draw(dc);\n                \
                           %com-release(brush);\n                \
                           %com-release(layout);\n                \
                           %dxgi-swap-chain-present(swap);\n              \
                         else 0 end;\n              \
                         0\n            \
                       elseif (msg = 2)\n              \
                         PostQuitMessage(0);\n              \
                         0\n            \
                       else\n              \
                         DefWindowProcW(hwnd, msg, wparam, lparam)\n            \
                       end\n          \
                     end;\n  \
            let cb = as-wndproc-callback(wp);\n  \
            let atom = %register-window-class(cb, \"NodAotIdeShell\");\n  \
            let hwnd = CreateWindowExW(0, atom, \"NewOpenDylan IDE\",\n                                       \
                13565952, -2147483648, -2147483648, 800, 600,\n                                       \
                0, 0, 0, 0);\n  \
            swap := %dxgi-create-swap-chain-for-hwnd(dxgi-factory, d3d-device, hwnd, 800, 600);\n  \
            ShowWindow(hwnd, 5);\n  \
            UpdateWindow(hwnd);\n  \
            %run-message-loop();\n\
        end function main;\n";
    let (dir, exe_path) = build_exe("ide-shell", source);

    eprintln!(
        "[sprint-41a headline] AOT EXE built at {}; spawning — \
         A WINDOW WILL APPEAR. Click the X to close it. The test will \
         then validate exit code 0.",
        exe_path.display()
    );

    // Spawn the EXE and block until it exits. The user has to close
    // the window manually for `.wait()` to return.
    let mut child = Command::new(&exe_path)
        .spawn()
        .expect("spawn AOT IDE shell EXE");
    let status = child.wait().expect("wait for AOT IDE shell EXE");
    let code = status.code().unwrap_or(-1);
    eprintln!("[sprint-41a headline] AOT IDE shell EXE exited with code {code}");

    assert_eq!(
        code, 0,
        "AOT IDE shell must exit cleanly with code 0 (WM_QUIT received \
         via PostQuitMessage(0) in WM_DESTROY handler); exe={}",
        exe_path.display()
    );

    // Success — clean up the temp dir.
    let _ = remove_dir_all_best_effort(&dir);
}


/// **The Sprint 41g headline.** Build an AOT-linked EXE — the
/// File menu now offers Save / Save As / Recent files — and exercise
/// the round-trip + recent-list persistence.
///
/// Differences vs. Sprint 41e (`aot_nod_ide_menu_open`):
///
///   * File menu now has Save (cmd-id 101), Save As (cmd-id 102), and
///     a Recent submenu (cmd-ids 301..305).
///   * Recent-files list persists across runs in
///     `F:\scratch\nod-ide-recent.txt` — one absolute path per line,
///     most-recent first, capped at 5. Dedup on add.
///   * Window title shows the current file's basename (no longer the
///     bare "NewOpenDylan IDE" — we now know which file is open).
///   * Save / Save As don't change the file content (the editor is
///     still read-only); they rewrite it with its own current bytes.
///     That's intentional — the plumbing is ready for when editing
///     arrives. Round-tripping read → write to a new path produces a
///     byte-identical copy.
///
/// What this test exercises beyond Sprint 41e:
///   * The new `nod_write_file_from_string` shim (byte-string-to-file
///     binary write).
///   * The new `nod_show_save_file_dialog` shim (wrap of
///     `GetSaveFileNameW` with OFN_OVERWRITEPROMPT).
///   * The new `nod_load_recent` / `nod_add_recent` / `nod_basename`
///     shims — recent-list persistence with dedup + 5-entry cap, plus
///     basename extraction for the title bar.
///   * Dynamic menu rebuild: `RemoveMenu(MF_BYPOSITION)` + re-
///     `AppendMenuW` + `DrawMenuBar` keep the Recent submenu in sync
///     with the on-disk list across opens / saves.
///
/// `#[ignore]`-gated because it's interactive. After the user closes
/// the window, the test asserts exit code 0.
#[test]
#[ignore = "interactive: pops a real Win32 window with Save/SaveAs/Recent. Run with `cargo test --test aot_ide_shell -- --ignored --nocapture aot_nod_ide_save_and_recent`."]
#[serial]
fn aot_nod_ide_save_and_recent() {
    // Sprint 44 — the IDE is now split across 5 .dylan files in the
    // fixtures directory (all sharing `Module: nod-ide`). Earlier
    // sprints used `include_str!` to inline the single monolithic
    // fixture into the test binary; now we hand the on-disk paths
    // straight to `nod-driver build` so the multi-file front-end
    // pipeline (compile_files_for_aot → per-file lower → merge →
    // codegen) gets exercised end-to-end through the headline test.
    //
    // The pre-split monolithic copy is preserved as
    // `unified_ide.dylan` (not loaded — just a safety/diff reference).
    //
    // Order matters: ide_win_calls.dylan first so its c-function
    // bindings are visible to later files; ide_rope.dylan next so
    // every later file can refer to the rope classes; then helpers,
    // syntax, and finally nod-ide.dylan with main + WNDPROC.
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let fixture_paths: Vec<PathBuf> = [
        "ide_win_calls.dylan",
        "ide_rope.dylan",
        "ide_helpers.dylan",
        "ide_syntax.dylan",
        "nod-ide.dylan",
    ]
    .iter()
    .map(|name| fixtures_dir.join(name))
    .collect();

    let (dir, exe_path) =
        build_exe_from_fixtures("nod-ide-save-recent", &fixture_paths);

    let scratch_root = PathBuf::from(r"F:\scratch");
    if !scratch_root.is_dir() {
        eprintln!(
            "[sprint-41g headline] F:\\scratch is missing on this machine — \
             this test requires F:\\scratch to exist (hard project rule: \
             test inputs live there, not inside the repo). Skipping."
        );
        return;
    }

    // Two fixtures so the user can pick something different via
    // File → Save As. Both are small files in the test root.
    let initial_fixture = scratch_root.join("nod-ide-41g-initial.dylan");
    std::fs::write(
        &initial_fixture,
        "Module: initial-sample\n\n\
         // initial-sample.dylan - Sprint 41g fixture for the Save/Recent IDE.\n\
         //\n\
         // The IDE opens with THIS file (passed via argv[1]).\n\
         // Try File > Save As to pick a new filename, then File > Recent\n\
         // to see the recent-list submenu populate.\n\n\
         define function hello () => ()\n  \
             format-out(\"hello from the initial buffer\\n\");\n\
         end function;\n\n\
         define function main () => ()\n  \
             hello();\n\
         end function main;\n",
    )
    .expect("write initial fixture");

    eprintln!(
        "[sprint-41g headline] AOT nod-ide (save+recent) EXE built at {}; \
         spawning with argv[1] = {}.\n  \
         A WINDOW WILL APPEAR; the title bar shows the file's basename \
         (e.g. \"nod-ide-41g-initial.dylan\").\n  \
         * Click File. The menu now shows Open / Save / Save As... / Recent / Exit.\n  \
         * Click File > Save As. Choose F:\\scratch\\nod-ide-41g-saved.txt (or any other path).\n  \
         * Title updates to the new basename. The file is created on disk\n  \
           (it's a byte-identical copy of the current buffer).\n  \
         * Click File > Recent. The submenu now shows the file you just\n  \
           saved at position 1, plus any earlier entries from previous runs.\n  \
         * Click a recent item. The buffer reloads from that path; title updates.\n  \
         * Optional: close the window, then re-launch the EXE (it'll use\n  \
           the same argv[1]). Open File > Recent — the previously-saved\n  \
           file is STILL there (persisted across runs).\n  \
         * Close the window. The test will then validate exit code 0.",
        exe_path.display(),
        initial_fixture.display(),
    );

    let mut child = Command::new(&exe_path)
        .arg(&initial_fixture)
        .spawn()
        .expect("spawn AOT nod-ide (save+recent) EXE");
    let status = child.wait().expect("wait for AOT nod-ide (save+recent) EXE");
    let code = status.code().unwrap_or(-1);
    eprintln!(
        "[sprint-41g headline] AOT nod-ide (save+recent) EXE exited with code {code}"
    );

    assert_eq!(
        code, 0,
        "AOT nod-ide (save+recent) EXE must exit cleanly with code 0; exe={}",
        exe_path.display()
    );

    let _ = remove_dir_all_best_effort(&dir);
    // Leave F:\scratch fixtures + nod-ide-recent.txt in place so the
    // user can rerun manually and observe persistence behaviour.
}
