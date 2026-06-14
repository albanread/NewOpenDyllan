//! Sprint 36 — IDE shell *interactive* demo, completed in Sprint 41a
//! with a real blocking message loop.
//!
//! THIS TEST POPS A REAL WIN32 WINDOW. It is `#[ignore]`-gated so
//! routine `cargo test` doesn't disturb the user's screen. Run
//! manually with:
//!
//! ```sh
//! cargo test --test ide_shell -- --ignored
//! ```
//!
//! The headline: a Dylan-source expression assembles every Sprint 27→36
//! deliverable into a working IDE shell — `CreateWindowExW` opens a
//! titled window, the Sprint 36 HWND-bound swap chain feeds a D2D
//! device context, DirectWrite renders "hello, dylan" through D2D into
//! the window's client area on each `WM_PAINT`, the Sprint 32 WNDPROC
//! routes messages back to a Dylan closure, and the Sprint 41a
//! `%run-message-loop()` primitive blocks on `GetMessageW` until the
//! user clicks the close box (WM_DESTROY → PostQuitMessage(0) →
//! WM_QUIT exit code 0).
//!
//! Acceptance: the test returns `exit-code = 0`, meaning the window
//! came up, rendered, accepted the close gesture, and unwound the
//! pump cleanly. The infrastructure tests in `ide_shell_infra.rs`
//! cover each piece in isolation; this test is the end-to-end demo
//! that proves they compose.
//!
//! ## Sprint 41a change
//!
//! Sprint 36c's first shipping form used `Sleep(5000)` as a placeholder
//! after `ShowWindow` / `UpdateWindow` — the window appeared but didn't
//! respond to input and unconditionally vanished after five seconds.
//! Sprint 41a replaces that placeholder with `%run-message-loop()`, a
//! `GetMessageW` / `TranslateMessage` / `DispatchMessageW` blocking
//! loop that returns the `WM_QUIT` exit code (0 when the WNDPROC's
//! `WM_DESTROY` handler calls `PostQuitMessage(0)`). The window now
//! stays up until the user closes it, which is the actual Win32
//! "real window" criterion.

#![cfg(windows)]
#![allow(non_snake_case)]

use nod_sema::eval_expr_with_items_to_string;
use serial_test::serial;

fn setup() {
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::ensure_structs_registered();
    nod_runtime::ensure_com_types_registered();
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::_reset_com_registry_for_tests();
    nod_runtime::_reset_callbacks_for_tests();
}

// The c-function declarations needed for the IDE shell. Sprint 31's
// JIT-time materialization picks up bare names from `windows_api.db`
// for most calls, but explicit declarations here document the call
// shapes (and let the test fail with a clear "wrong signature" error
// instead of a marshaling miscount).
const IDE_SHELL_DECL: &str = "\
define c-function CreateWindowExW
    (dwExStyle :: <c-int>, lpClassName :: <c-pointer>, lpWindowName :: <c-wide-string>,
     dwStyle :: <c-int>, x :: <c-int>, y :: <c-int>, nWidth :: <c-int>, nHeight :: <c-int>,
     hWndParent :: <c-pointer>, hMenu :: <c-pointer>, hInstance :: <c-pointer>,
     lpParam :: <c-pointer>)
 => (hwnd :: <c-pointer>);
  library: \"user32.dll\";
end;

define c-function ShowWindow
    (hwnd :: <c-pointer>, nCmdShow :: <c-int>)
 => (was-visible :: <c-bool>);
  library: \"user32.dll\";
end;

define c-function UpdateWindow
    (hwnd :: <c-pointer>)
 => (success :: <c-bool>);
  library: \"user32.dll\";
end;

define c-function GetMessageW
    (lpMsg :: <c-pointer>, hwnd :: <c-pointer>,
     wMsgFilterMin :: <c-int>, wMsgFilterMax :: <c-int>)
 => (result :: <c-int>);
  library: \"user32.dll\";
end;

define c-function PeekMessageW
    (lpMsg :: <c-pointer>, hwnd :: <c-pointer>,
     wMsgFilterMin :: <c-int>, wMsgFilterMax :: <c-int>,
     wRemoveMsg :: <c-int>)
 => (has-message :: <c-int>);
  library: \"user32.dll\";
end;

define c-function Sleep
    (ms :: <c-int>)
 => ();
  library: \"kernel32.dll\";
end;

define c-function TranslateMessage
    (lpMsg :: <c-pointer>)
 => (success :: <c-bool>);
  library: \"user32.dll\";
end;

define c-function DispatchMessageW
    (lpMsg :: <c-pointer>)
 => (lresult :: <c-pointer>);
  library: \"user32.dll\";
end;

define c-function DefWindowProcW
    (hwnd :: <c-pointer>, msg :: <c-int>,
     wparam :: <c-pointer>, lparam :: <c-pointer>)
 => (lresult :: <c-pointer>);
  library: \"user32.dll\";
end;

define c-function PostQuitMessage
    (exit-code :: <c-int>)
 => ();
  library: \"user32.dll\";
end;
";

/// The Sprint 36 headline. Opens a real window, renders text via D2D,
/// runs the message pump, returns when the user closes the window.
///
/// On hosts without a display (CI, headless VMs) the test still
/// compiles and links cleanly; on a real desktop it pops a window
/// the developer must close to finish.
///
/// The WNDPROC body captures `swap` and `bitmap` cells via Sprint 24's
/// cell-conversion (any `:=` on a captured name promotes it to a
/// heap-allocated `<cell>` reachable through both the outer scope
/// and the closure body — no explicit `make-cell` calls needed).
#[test]
#[serial]
#[ignore = "interactive: pops a real Win32 window. Run manually with `cargo test --test ide_shell -- --ignored`."]
fn ide_shell_window_renders_hello_dylan() {
    setup();
    // Win32 constants used inline (Sprint 29 has the named forms but
    // we keep this self-contained):
    //   WM_PAINT          = 15
    //   WM_DESTROY        = 2
    //   WS_OVERLAPPEDWINDOW = 0xCF0000   = 13565952
    //   CW_USEDEFAULT     = 0x80000000   = -2147483648 (signed 32-bit)
    //   SW_SHOW           = 5
    let body = "\
        let d3d-device   = %d3d11-create-device(); \
        let dxgi-factory = %dxgi-factory-from-d3d-device(d3d-device); \
        let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device); \
        let d2d-factory  = %d2d-create-factory(); \
        let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device); \
        let dc           = %d2d-create-device-context(d2d-device); \
        let dwrite       = %dwrite-create-factory(); \
        let format       = %dwrite-create-text-format(dwrite, \"Segoe UI\", 2400, \"en-us\"); \
        \
        let swap = 0; \
        let bitmap = 0; \
        \
        let wp = method (hwnd, msg, wparam, lparam) \
                   if (msg = 15) \
                     if (swap ~= 0) \
                       if (bitmap = 0) \
                         bitmap := %d2d-create-bitmap-from-swap-chain(dc, swap); \
                       else 0 end; \
                       %d2d-set-target(dc, bitmap); \
                       %d2d-begin-draw(dc); \
                       %d2d-clear(dc, 255, 255, 255, 255); \
                       let brush  = %d2d-create-solid-color-brush(dc, 0, 0, 0, 255); \
                       let layout = %dwrite-create-text-layout(dwrite, \"hello, dylan\", format, 800, 600); \
                       %d2d-draw-text-layout(dc, 50, 50, layout, brush); \
                       %d2d-end-draw(dc); \
                       %com-release(brush); \
                       %com-release(layout); \
                       %dxgi-swap-chain-present(swap); \
                     else 0 end; \
                     0 \
                   elseif (msg = 2) \
                     PostQuitMessage(0); \
                     0 \
                   else \
                     DefWindowProcW(hwnd, msg, wparam, lparam) \
                   end \
                 end; \
        let cb = as-wndproc-callback(wp); \
        let atom = %register-window-class(cb, \"NodIdeShell\"); \
        \
        let hwnd = CreateWindowExW(0, atom, \"NewOpenDylan IDE\", \
                                    13565952, -2147483648, -2147483648, 800, 600, \
                                    0, 0, 0, 0); \
        \
        swap := %dxgi-create-swap-chain-for-hwnd(dxgi-factory, d3d-device, hwnd, 800, 600); \
        \
        ShowWindow(hwnd, 5); \
        UpdateWindow(hwnd); \
        \
        %run-message-loop()";
    let s = eval_expr_with_items_to_string(IDE_SHELL_DECL, body)
        .unwrap_or_else(|e| panic!("IDE shell test failed: {e:?}"));
    assert_eq!(
        s, "0",
        "IDE shell must exit cleanly with code 0 (WM_QUIT received); got `{s}`"
    );
}
