//! Sprint 39c — broader AOT correctness tests.
//!
//! Exercises stdlib-defined Dylan features (stdlib generics dispatching
//! to seed-class instances, `for-each` macro expansion over the FIP
//! primitives, NLX `block` no-exit value flow, and an opt-in
//! MessageBoxW) through the full Dylan-source → `.obj` → linker →
//! EXE pipeline. Every test is `#[ignore]` because the pipeline
//! shells out to `cargo run --bin nod-driver` plus MSVC's
//! `link.exe`, and `serial_test::serial` keeps concurrent
//! invocations from stalling on Cargo's build-system lock.
//!
//! Run with:
//!
//! ```text
//! cargo test --test aot_dylan -- --ignored --nocapture
//! ```
//!
//! ## Why a separate file from `aot_exe.rs`?
//!
//! `aot_exe.rs` is the canonical Sprint 39a-bringup file (hello-world,
//! arithmetic, dispatch); it stays small and focused on "the AOT
//! pipeline works at all". `aot_dylan.rs` is the broader correctness
//! suite that grows as new stdlib features get AOT'd. The dispatch
//! test in `aot_exe.rs` already covers the Sprint 38c/38e relocation
//! categories; tests here add the **Dylan-side behaviour** dimension.
//!
//! ## What's covered (Sprint 39c)
//!
//! - `concatenate` on lists (stdlib `<list>` concatenation).
//! - `for-each (x in c) body end` over a `<pair>` list (the macro's
//!   `until + %fip-*` expansion).
//! - `block () body end` no-exit value flow (block-registry exercise
//!   without NLX).
//! - `MessageBoxW` interactive opt-in.
//!
//! ## User-defined classes
//!
//! Sprint 40a lands user-defined `define class` registration in the
//! AOT pipeline. The `aot_user_classes_and_dispatch` test below
//! exercises the full slot / CPL / parent metadata serialisation
//! into the EXE (a per-class `nod_aot_register_user_class` call
//! baked into `nod_aot_resolve_relocs` at startup, fed by the
//! `LoweredModule::user_classes` capture). Class-id determinism is
//! preserved by registering in compile-time order in both processes
//! (compiler-side via `register_class`; EXE-side via the resolver
//! walking the persisted list).
//!
//! ## User-defined `for (i from N to M)`
//!
//! Sprint 18's parser only lowers `while` and `until`. `for-each
//! (x in c) body end` exists as a stdlib macro; full `for` is a
//! future sprint. Tests here use only stdlib-supported surface
//! syntax.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Mirrors
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
    let dir = std::env::temp_dir().join(format!("nod-aot-dylan-test-{test_name}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Build a Dylan source string to an EXE in a fresh temp dir, run
/// the EXE, return (stdout, stderr, exit_code). On exit_code == 0 the
/// temp dir is removed; on failure it's kept for forensic inspection.
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

// ─── Sprint 39c — broader stdlib correctness tests ───────────────────────────

/// Stdlib `concatenate` on `<list>` arguments. The collection runtime
/// emits a fresh `<pair>` chain when both inputs are lists (other
/// shapes widen to `<simple-object-vector>`); we then take `size` of
/// the result to assert the merged-stdlib dispatch reaches the
/// methods needed for both `concatenate` and `size`.
#[test]
#[ignore]
#[serial]
fn aot_list_concat_size() {
    let source = "Module: concat\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", size(concatenate(#(1, 2), #(3, 4, 5))));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("concat", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "5\n", "stdout mismatch; stderr=\n{stderr}");
}

/// `for-each (x in lst) body end` over a `<pair>` list. Exercises:
///   * The stdlib's `for-each` macro expansion (registered in the
///     process-global macro table by `ensure_loaded`).
///   * The `%fip-init` / `%fip-finished?` / `%fip-current-element` /
///     `%fip-advance!` primitive dispatch against `<pair>`.
///   * Read-modify-write of the captured local `total` across the
///     loop body (Sprint 24 cell-promotion in lifted closures, even
///     for non-escaping captures).
///
/// Asserts 1+2+3+4+5+6+7+8+9+10 = 55.
#[test]
#[ignore]
#[serial]
fn aot_for_each_sum_list() {
    let source = "Module: foreach\n\n\
        define function main () => ()\n  \
            let total = 0;\n  \
            for-each (x in #(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)) total := total + x end;\n  \
            format-out(\"%d\\n\", total);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("foreach", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "55\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Sprint 19 `block` with no exit (`block () body end`) — exercises
/// the AOT block-registration path even when no NLX actually occurs.
/// Asserts the block's last-expression-value semantics survive the
/// AOT lower → codegen → register-thunks path.
///
/// Note: `let r = block () … end` (block at expression position
/// flowing into a `let`) is a known lowering deferral — the JIT
/// surfaces the same diagnostic. We use a helper function so the
/// block lives at the function body's return position, which IS
/// lowerable.
#[test]
#[ignore]
#[serial]
fn aot_block_no_early_exit() {
    let source = "Module: blk\n\n\
        define function compute () => (n :: <integer>)\n  \
            block ()\n    \
                let a = 10;\n    \
                let b = 20;\n    \
                a + b\n  \
            end block\n\
        end function;\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", compute());\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("block-no-exit", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "30\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Stdlib `reduce` over a `<pair>` list — exercises first-class
/// function dispatch (the `\+` reference goes through the
/// function-ref registry, which the AOT resolver populates at
/// startup via `nod_aot_register_jit_function` for every top-level
/// stdlib function). Asserts 1+2+3+4+5 = 15.
#[test]
#[ignore]
#[serial]
fn aot_reduce_plus() {
    let source = "Module: red\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", reduce(\\+, 0, #(1, 2, 3, 4, 5)));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("reduce", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "15\n", "stdout mismatch; stderr=\n{stderr}");
}

/// `size(<range>)` exercised through a Dylan-defined helper function
/// that wraps the call. This proves the merged-stdlib resolves the
/// `size` method body when called from a user function (i.e., across
/// the lift sink the merge produces, not just from `main`).
#[test]
#[ignore]
#[serial]
fn aot_range_size_through_helper() {
    let source = "Module: rng\n\n\
        define function range-size (lo, hi) => (n :: <integer>)\n  \
            size(make(<range>, from: lo, to: hi))\n\
        end function;\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", range-size(0, 10));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("range-helper", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "11\n", "stdout mismatch; stderr=\n{stderr}");
}

/// Sprint 40a — user-defined classes through the AOT pipeline.
/// Headline test: `<shape>` → `<circle>` / `<square>` two-level
/// hierarchy with `init-keyword:` slots, a user-declared generic
/// `area`, two specialised methods, `make` invocation, and dispatch
/// on user-class instances.
///
/// This exercises **everything** Sprint 40a added:
///   * The `LoweredModule::user_classes` capture in `register_class`.
///   * The merge path picking the user's classes through to
///     `build_aot_registrations`.
///   * The codegen-emitted `nod_aot_register_user_class` calls in
///     `nod_aot_resolve_relocs` (running BEFORE method registrations
///     so the dispatch table sees live class metadata for `<circle>`
///     / `<square>` when it walks `area`'s specialisers).
///   * The runtime shim reconstructing a `UserClassSpec` from the
///     baked C-ABI inputs and calling `register_user_class_metadata`.
///   * Slot init-keywords (`radius:` / `side:`) flowing through to
///     `nod_make`'s slot lookup so `make(<circle>, radius: 5)`
///     deposits the radius into the right slot.
///   * Dispatch on user classes (the existing Sprint 38e inline
///     cache + Sprint 12 class metadata) reaching `area`'s two
///     bodies via the merged dispatch table.
#[test]
#[ignore]
#[serial]
fn aot_user_classes_and_dispatch() {
    let source = "Module: user-class\n\n\
        define class <shape> (<object>) end class;\n\
        define class <circle> (<shape>)\n  \
            slot circle-radius :: <integer>, init-keyword: radius:;\n\
        end class;\n\
        define class <square> (<shape>)\n  \
            slot square-side :: <integer>, init-keyword: side:;\n\
        end class;\n\
        define generic area (shape :: <shape>) => (n :: <integer>);\n\
        define method area (c :: <circle>) => (n :: <integer>)\n  \
            3 * circle-radius(c) * circle-radius(c)\n\
        end method;\n\
        define method area (s :: <square>) => (n :: <integer>)\n  \
            square-side(s) * square-side(s)\n\
        end method;\n\
        define function main () => ()\n  \
            let c = make(<circle>, radius: 5);\n  \
            let s = make(<square>, side: 4);\n  \
            format-out(\"circle: %d, square: %d\\n\", area(c), area(s));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("user-class", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(
        stdout, "circle: 75, square: 16\n",
        "stdout mismatch; stderr=\n{stderr}"
    );
}

/// Sprint 30's MessageBoxW headline reborn for AOT. Shows a real
/// Win32 dialog and waits for the user to click OK; the EXE then
/// exits with the dialog's return code (IDOK = 1) printed to stdout.
///
/// `#[ignore]` AND opt-in. Run with:
///
/// ```text
/// cargo test --test aot_dylan aot_messagebox_w_ignored -- --ignored --nocapture
/// ```
///
/// The test is INTENTIONALLY interactive — it proves the full Win32
/// path (user32.dll dllimport + Sprint 30 string marshaling + Win64
/// trampoline) works in an AOT EXE that ALSO carries the merged
/// stdlib (the `format-out` call after MessageBoxW returns exercises
/// the same registered-method path the non-interactive tests do).
/// The non-interactive AOT-Win32 tests in `aot_win32.rs` already
/// cover symbol resolution and IAT imports without pulling up UI;
/// this one verifies the marshaling edge cases that need a real
/// Win32 call.
#[test]
#[ignore]
#[serial]
fn aot_messagebox_w_ignored() {
    let source = "Module: msgbox\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", MessageBoxW($NULL, \"Sprint 39c AOT MessageBoxW test\", \"NewOpenDylan\", $MB-OK));\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("msgbox", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    // MB_OK → IDOK = 1.
    assert_eq!(stdout, "1\n", "stdout mismatch; stderr=\n{stderr}");
}

// ─── Sprint 40b — Win32 callbacks in AOT EXEs ────────────────────────────────
//
// The Sprint 32 trampoline pool (`src/nod-runtime/src/callbacks.rs`) already
// statically links into every AOT EXE via `nod_runtime.lib`, and the
// `nod_register_wndproc` / `nod_register_wndenumproc` externs are
// `#[unsafe(no_mangle)]`-exported unconditionally. The codegen extern table
// in `nod-llvm/src/codegen.rs` declares both symbols against the merged
// LLVM module, so they appear in the AOT `.obj` and the linker resolves
// them out of `nod_runtime.lib` exactly the same way it does for any other
// runtime extern. The stdlib wrappers (`as-wndproc-callback` /
// `as-wndenumproc-callback` in `stdlib.dylan`) are top-level functions
// already merged into the user module by `compile_file_for_aot`, so the
// surface is reachable from user Dylan code with no AOT-specific glue.
//
// Sprint 40a verified Sprint 24 cell-promotion works through AOT (the
// `for-each` test sums a captured `total` across the loop body); the
// headline below piggy-backs on the same mechanism — `count` is captured
// by the `method` closure passed to `as-wndenumproc-callback`, promoted
// into a `<cell>` because the closure mutates it, and observed AFTER
// `EnumWindows` returns. So this test ALSO exercises Sprint 24's
// closure-environment story round-tripping through a Win32 C-ABI call.

/// **The Sprint 40b headline.** Build an AOT EXE that calls `EnumWindows`
/// with a Dylan closure callback, captures-by-reference a counter from
/// the enclosing scope, and prints the count after the OS returns.
///
/// Mirrors the Sprint 32 JIT headline `enum_windows_invokes_callback_for_each_top_level_window`
/// in `winffi_callbacks.rs`. The Dylan source is identical-modulo
/// the explicit `define c-function EnumWindows` declaration (Sprint 31's
/// JIT-time bare-name materialisation doesn't carry through the AOT
/// codegen path — bare `EnumWindows` reports `unknown callee`; that's
/// pre-existing, unrelated to callbacks, and tracked separately).
///
/// Acceptance: every Windows desktop has at least the taskbar + a few
/// always-present windows, so `count > 0` is required; the upper bound
/// is intentionally loose to avoid flaking on busy machines.
#[test]
#[ignore]
#[serial]
fn aot_enum_windows_callback() {
    let source = "Module: enum-windows\n\n\
        define c-function EnumWindows\n  \
            (callback :: <c-pointer>, lparam :: <c-pointer>)\n  \
         => (success :: <c-bool>);\n  \
            library: \"user32.dll\";\n\
        end;\n\n\
        define function main () => ()\n  \
            let count = 0;\n  \
            let cb = method (hwnd, lp) count := count + 1; #t end;\n  \
            let cb-ptr = as-wndenumproc-callback(cb);\n  \
            EnumWindows(cb-ptr, $NULL);\n  \
            format-out(\"count: %d\\n\", count);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("enum-windows", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    // Parse "count: N\n" and assert sane bounds.
    let trimmed = stdout.trim_end();
    let n_str = trimmed
        .strip_prefix("count: ")
        .unwrap_or_else(|| panic!("unexpected stdout shape: {stdout:?}; stderr=\n{stderr}"));
    let n: i64 = n_str
        .parse()
        .unwrap_or_else(|e| panic!("count parse failed for {n_str:?}: {e}; stderr=\n{stderr}"));
    assert!(
        n > 0,
        "EnumWindows must invoke the callback at least once, got count={n}; stderr=\n{stderr}"
    );
    assert!(
        n < 100_000,
        "callback count is suspiciously large ({n}); suggests runaway loop or marshaling bug; stderr=\n{stderr}"
    );
    eprintln!("[sprint-40b headline] AOT EnumWindows enumerated {n} top-level windows");
}

/// Sprint 40d — bare-name `EnumWindows` (no explicit
/// `define c-function`) in an AOT EXE. This is the Sprint 40b
/// headline minus the user-written declaration: the sema-side
/// materializer (Sprint 31) must pick `EnumWindows` out of
/// `user32.dll`, build its `ApiCallSignature` from the projected
/// `FunctionInfo`, and feed the stub-table call path so the AOT
/// pipeline emits the same `nod_aot_register_api_stub` + `dllimport`
/// + Win64 trampoline call as if the user had written the
/// declaration by hand.
///
/// Before Sprint 40d this reported "unknown callee `EnumWindows`"
/// because `nod_winapi`'s `build.rs` skipped every function whose
/// signature mentioned a `function_pointer` / `delegate` param —
/// `EnumWindows`'s first param is `WNDENUMPROC` (a `delegate`-kind
/// row in the SQL DB) and its second is `LPARAM` (a `struct`-kind
/// row that's really a `i64` typedef). Sprint 40d's two-arm
/// extension to `classify_type` accepts both kinds (the former as
/// an opaque `<c-pointer>` collapse, the latter via the known
/// typedef table) so the function lands in the projected subset
/// and bare-name materialization works.
///
/// Asserts the same `count: N` shape as
/// `aot_enum_windows_callback`; the absence of `define c-function
/// EnumWindows` in the source is the whole point.
#[test]
#[ignore]
#[serial]
fn aot_bare_enum_windows() {
    let source = "Module: bare-enum\n\n\
        define function main () => ()\n  \
            let count = 0;\n  \
            let cb = method (hwnd, lp) count := count + 1; #t end;\n  \
            let cb-ptr = as-wndenumproc-callback(cb);\n  \
            EnumWindows(cb-ptr, $NULL);\n  \
            format-out(\"count: %d\\n\", count);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("bare-enum", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    let trimmed = stdout.trim_end();
    let n_str = trimmed
        .strip_prefix("count: ")
        .unwrap_or_else(|| panic!("unexpected stdout shape: {stdout:?}; stderr=\n{stderr}"));
    let n: i64 = n_str
        .parse()
        .unwrap_or_else(|e| panic!("count parse failed for {n_str:?}: {e}; stderr=\n{stderr}"));
    assert!(
        n > 0,
        "EnumWindows must invoke the callback at least once, got count={n}; stderr=\n{stderr}"
    );
    assert!(
        n < 100_000,
        "callback count is suspiciously large ({n}); suggests runaway loop or marshaling bug; stderr=\n{stderr}"
    );
    eprintln!("[sprint-40d headline] AOT bare-name EnumWindows enumerated {n} top-level windows");
}

/// Smoke test that `as-wndproc-callback` registers a closure without
/// actually creating a window. Returns a non-null `<c-pointer>` (the
/// trampoline slot's address); we just print "ok" / "null". Doesn't
/// exercise OS callback invocation (no message loop), so safe to run
/// in CI without UI.
///
/// This proves the Sprint 32 `WNDPROC` registration path (arity-4
/// closure → 32-slot pool → fixnum-tagged address) is reachable from
/// AOT Dylan code, in addition to the `WNDENUMPROC` (arity-2) path
/// exercised by the headline.
#[test]
#[ignore]
#[serial]
fn aot_register_wndproc_smoke() {
    let source = "Module: wndproc-smoke\n\n\
        define function main () => ()\n  \
            let cb = method (hwnd, msg, wp, lp) 0 end;\n  \
            let cb-ptr = as-wndproc-callback(cb);\n  \
            if (cb-ptr = 0) format-out(\"null\\n\"); else format-out(\"ok\\n\"); end if;\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("wndproc-smoke", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "ok\n", "stdout mismatch; stderr=\n{stderr}");
}

// ─── GAP-004 — `define variable` end-to-end through AOT ──────────────────────
//
// The sema-side regression in `sema.rs::gap_004_define_variable_lowers_to_getter_and_init`
// proves the lowering shape is correct. This test proves the FULL
// pipeline works: lower → codegen → linker → EXE → runtime
// `nod_aot_register_variable` resolver → `define variable` getter +
// setter round-tripping at runtime. Without this end-to-end test, the
// lowering test alone could pass while the runtime shims silently
// bork.
//
// Coverage:
//   * Initial value (41) flows through the getter on first read.
//   * `<name> := <value>` writes are observed by subsequent reads.
//   * Mutation across function-call boundaries holds (bump-counter
//     called twice, prints intermediate + final).
//   * Two independent variables don't cross-contaminate (sanity check
//     for slot-table keying on name).

/// **The GAP-004 headline.** Build an AOT EXE around two
/// `define variable` declarations and exercise getter / setter /
/// cross-call mutation. Without the GAP-004 fix the lowering step
/// would emit `LoweringError::Unsupported` and `nod-driver` would
/// fail before producing an EXE.
#[test]
#[ignore]
#[serial]
fn aot_gap_004_define_variable_round_trip() {
    let source = "Module: var\n\n\
        define variable *counter* = 41;\n\
        define variable *other* = 7;\n\n\
        define function bump-counter () => ()\n  \
            *counter* := *counter* + 1;\n\
        end function;\n\n\
        define function main () => ()\n  \
            format-out(\"%d\\n\", *counter*);\n  \
            bump-counter();\n  \
            format-out(\"%d\\n\", *counter*);\n  \
            *counter* := 99;\n  \
            format-out(\"%d\\n\", *counter*);\n  \
            format-out(\"%d\\n\", *other*);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("gap-004-var", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    // 41 (initial) → 42 (after bump-counter) → 99 (after direct
    // assignment) → 7 (the OTHER variable, never touched, still at
    // its init value — proves slot-table keying is per-name).
    assert_eq!(stdout, "41\n42\n99\n7\n",
        "round-trip mismatch; stderr=\n{stderr}");
}

/// Sprint 47 / GAP-003 — multi-value return + multi-binder `let`
/// end-to-end through the AOT pipeline. `divmod` returns
/// `values(a / b, a mod b)`; main binds both via
/// `let (q, r) = divmod(13, 5)` and prints them. Asserts the SBCL-style
/// secondary-values protocol (caller `clear` + `get`, callee `set` +
/// ordinary return) survives codegen and runs correctly under the
/// real GC.
#[test]
#[ignore]
#[serial]
fn aot_gap_003_divmod_multi_value() {
    let source = "Module: divmod\n\n\
        define function divmod (a :: <integer>, b :: <integer>)\n \
         => (q :: <integer>, r :: <integer>)\n  \
            values(a / b, a mod b)\n\
        end function;\n\n\
        define function main () => ()\n  \
            let (q, r) = divmod(13, 5);\n  \
            format-out(\"q=%d r=%d\\n\", q, r);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("gap-003-divmod", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "q=2 r=3\n",
        "divmod multi-value mismatch; stderr=\n{stderr}");
}

/// Sprint 47 / GAP-003 — polluted-buffer correctness end-to-end. Call
/// A returns two values, then call B is single-valued, then a multi-
/// binder `let` destructures B's result. Without the `clear` before
/// B's call, B's second binder would pick up A's leftover extra and
/// the test would print A's extra instead of `#f`. With the clear, it
/// prints `#f` (Dylan formats `#f` as `#f` in `format-out` — but for
/// a clean integer compare, we sum the second binder with 100 after
/// detecting `#f` via `instance?`).
#[test]
#[ignore]
#[serial]
fn aot_gap_003_polluted_buffer_does_not_leak() {
    let source = "Module: leak\n\n\
        define function call-a () => (n :: <integer>)\n  \
            values(11, 22)\n\
        end function;\n\n\
        define function call-b () => (n :: <integer>)\n  \
            99\n\
        end function;\n\n\
        define function main () => ()\n  \
            let (a1, a2) = call-a();\n  \
            let (b1, b2) = call-b();\n  \
            // b2 should be #f (call-b is single-valued, and the multi-\n  \
            // binder `let` cleared the buffer before the call).\n  \
            // a1=11, a2=22, b1=99. Print all three plus whether b2 is #f.\n  \
            format-out(\"a1=%d a2=%d b1=%d b2-is-false=%d\\n\",\n             \
                a1, a2, b1,\n             \
                if (b2 = #f) 1 else 0 end);\n\
        end function main;\n";
    let (stdout, stderr, code) = build_and_run("gap-003-leak", source);
    assert_eq!(code, 0, "exit code; stderr=\n{stderr}");
    assert_eq!(stdout, "a1=11 a2=22 b1=99 b2-is-false=1\n",
        "polluted-buffer test mismatch; stderr=\n{stderr}");
}
