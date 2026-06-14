//! Sprint 31 — JIT-time Win32 API materialization.
//!
//! Sprint 28 wired `define c-function` end-to-end: with a declaration in
//! scope, a Win32 call goes parse → sema → DFM → LLVM → JIT → real Win32
//! invocation. Sprint 31 drops the declaration. When the sema layer sees
//! a bare-name call (`GetTickCount64()` with no `define c-function`
//! above it) it consults the embedded `nod_winapi` index, synthesizes
//! the c-function binding on the fly, allocates a stub-table slot, and
//! lowers the call through the existing Sprint 28 machinery.
//!
//! The acceptance set covers four behavioral guarantees:
//!
//! 1. **Bare-name resolution** — `GetTickCount64()`, `GetCurrentProcessId()`,
//!    `Sleep(0)`, `lstrlenW("héllo")` all run without a prior declaration.
//! 2. **A/W default to W** — bare `MessageBox` materializes as
//!    `MessageBoxW` from `user32.dll`; bare `MessageBoxA` resolves as
//!    explicitly ANSI.
//! 3. **User declarations win** — an explicit `define c-function`
//!    overrides materialization (verified via the `BindingSource` field).
//! 4. **Unsupported signatures decline gracefully** — a Win32 export with
//!    a function-pointer / callback parameter (e.g. `EnumWindows`) yields
//!    an informative error, not a silent fall-through to "unknown
//!    identifier".
//!
//! Every test is `#[serial]`: the WinFFI stats counters and the global
//! library cache are process-global state, and these tests share that
//! state with Sprint 28 + Sprint 30's FFI tests.

#![cfg(windows)]
// Test fn names mirror the Win32 export names they exercise (e.g.
// `bare_GetTickCount64_resolves_to_kernel32`). Snake-casing would
// hide the API names being tested and confuse search.
#![allow(non_snake_case)]

use nod_sema::{BindingSource, eval_expr_to_string, introspect_bindings};
use serial_test::serial;

fn setup() {
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::_reset_handler_stack_for_tests();
    // Sprint 38f: the materialization tests measure sema-side counters
    // (`materialized_lifetime`). The disk-replay path skips sema, so a
    // leftover bitcode + sidecar trio from a prior run would short-
    // circuit the materialization. Clear both the in-process and
    // on-disk caches so each test reliably observes a cold compile.
    nod_llvm::in_process_clear();
    let dir = nod_llvm::default_cache_dir();
    nod_llvm::clear_cache_dir(&dir);
}

// ─── 1. Headline: bare GetTickCount64 ─────────────────────────────────────

/// **The Sprint 31 headline.** Without any `define c-function` in scope,
/// the bare-name call `GetTickCount64()` materializes a binding from
/// the embedded index, resolves through Sprint 28's stub table, and
/// returns the system uptime in milliseconds. The lower bound of 1000
/// proves the function actually ran and the marshaling produced a
/// realistic value (any non-trivial Windows session has been booted
/// for at least a second).
#[test]
#[serial]
fn bare_GetTickCount64_resolves_to_kernel32() {
    setup();
    let s = eval_expr_to_string("GetTickCount64()")
        .unwrap_or_else(|e| panic!("bare GetTickCount64 eval failed: {e:?}"));
    eprintln!("[sprint31 headline] GetTickCount64() => {s}");
    let n: i64 = s.parse().expect("integer return from GetTickCount64");
    assert!(
        n > 1_000,
        "GetTickCount64() must return uptime > 1000 ms (proves the call really ran); \
         got {n}. If 0 the marshaling never reached kernel32."
    );
}

// ─── 2. Bare GetCurrentProcessId ──────────────────────────────────────────

/// `GetCurrentProcessId()` returns a positive u32. The materializer
/// must pick up `kernel32.dll` as the owning DLL automatically.
#[test]
#[serial]
fn bare_GetCurrentProcessId_resolves_correctly() {
    setup();
    let s = eval_expr_to_string("GetCurrentProcessId()")
        .unwrap_or_else(|e| panic!("bare GetCurrentProcessId eval failed: {e:?}"));
    eprintln!("[sprint31] GetCurrentProcessId() => {s}");
    let n: i64 = s.parse().expect("integer return from GetCurrentProcessId");
    assert!(
        n > 0 && n < (1 << 32),
        "GetCurrentProcessId() must be a positive u32; got {n}"
    );
}

// ─── 3. Bare Sleep — void return ──────────────────────────────────────────

/// `Sleep(0)` is the standard way to yield. The materialized binding
/// must surface a void return (zero-arg-count return-kind in the stub
/// signature). The Dylan side then yields a generic nil-shaped value.
#[test]
#[serial]
fn bare_Sleep_resolves_to_void_returning() {
    setup();
    // Sleep returns void; the Dylan side serializes a #f / nil-shaped
    // word as either `#f` or `#()` depending on the eval-entry's return
    // shape. Accept either as "successfully invoked, no return value".
    let s = eval_expr_to_string("Sleep(0)")
        .unwrap_or_else(|e| panic!("bare Sleep eval failed: {e:?}"));
    eprintln!("[sprint31] Sleep(0) => {s}");
    // We don't pin the exact serialization here; the important thing is
    // that the call dispatched and didn't panic. The non-empty result
    // and the absence of an error class confirm both.
    assert!(
        !s.is_empty(),
        "Sleep(0) must produce SOME formatted result; got empty string"
    );
}

// ─── 4. Bare lstrlenW with string marshaling ──────────────────────────────

/// Materialization must also flow strings through correctly. With no
/// `define c-function` declared for `lstrlenW`, the materialization
/// layer derives its signature from the index (one `WideString` arg,
/// `Int32` return) and the call returns the codepoint count. `"héllo"`
/// = 5 UTF-16 code units, exercising the same UTF-8 → UTF-16
/// transcoding Sprint 30 proved out — but now with a JIT-synthesized
/// binding instead of a hand-written declaration.
#[test]
#[serial]
fn bare_lstrlenW_resolves_with_string_marshaling() {
    setup();
    let s = eval_expr_to_string("lstrlenW(\"héllo\")")
        .unwrap_or_else(|e| panic!("bare lstrlenW eval failed: {e:?}"));
    eprintln!("[sprint31] lstrlenW(\"héllo\") => {s}");
    assert_eq!(
        s, "5",
        "bare lstrlenW(\"héllo\") must return 5 (UTF-16 code unit count); got {s}"
    );
}

// ─── 5. A/W default to W ──────────────────────────────────────────────────

/// Bare `MessageBox` (no suffix) must materialize as `MessageBoxW` from
/// `user32.dll` — Sprint 31's A/W disambiguation rule. We DO NOT invoke
/// MessageBox here (no popping dialogs in `cargo test`); instead we
/// introspect the synthesized binding via [`introspect_bindings`].
#[test]
#[serial]
fn bare_MessageBox_resolves_to_W_variant() {
    setup();
    let bindings = introspect_bindings("", "MessageBox(0, \"\", \"\", 0)")
        .unwrap_or_else(|e| panic!("MessageBox introspection failed: {e:?}"));
    let mb = bindings
        .iter()
        .find(|b| b.dylan_name == "MessageBox")
        .unwrap_or_else(|| panic!("no MessageBox binding materialized; saw {bindings:#?}"));
    eprintln!(
        "[sprint31] MessageBox introspection: c_name={} library={} source={:?}",
        mb.c_name, mb.library, mb.source
    );
    assert_eq!(
        mb.source,
        BindingSource::JitMaterialized,
        "expected JIT-materialized; got {:?}",
        mb.source
    );
    assert_eq!(
        mb.c_name, "MessageBoxW",
        "bare MessageBox must materialize as MessageBoxW; got {}",
        mb.c_name
    );
    assert_eq!(
        mb.library, "user32.dll",
        "MessageBoxW must come from user32.dll; got {}",
        mb.library
    );
}

// ─── 6. A/W explicit A still works ────────────────────────────────────────

/// Explicit `MessageBoxA` resolves to the ANSI variant — proves the
/// A/W default doesn't blindly rewrite suffixed names.
#[test]
#[serial]
fn bare_MessageBoxA_resolves_explicitly() {
    setup();
    let bindings = introspect_bindings("", "MessageBoxA(0, \"\", \"\", 0)")
        .unwrap_or_else(|e| panic!("MessageBoxA introspection failed: {e:?}"));
    let mb = bindings
        .iter()
        .find(|b| b.dylan_name == "MessageBoxA")
        .unwrap_or_else(|| panic!("no MessageBoxA binding materialized; saw {bindings:#?}"));
    assert_eq!(mb.source, BindingSource::JitMaterialized);
    assert_eq!(
        mb.c_name, "MessageBoxA",
        "explicit MessageBoxA must keep the A variant; got {}",
        mb.c_name
    );
    assert_eq!(mb.library, "user32.dll");
}

// ─── 7. User declarations override materialization ───────────────────────

const GETTICKCOUNT_USER_DECL: &str = "\
define c-function GetTickCount () => (ticks :: <c-dword>);
  library: \"kernel32.dll\";
end;
";

/// When the user explicitly declares a c-function, the JIT
/// materialization path must decline (user wins). Verify the binding
/// in the lowered module carries `source: UserCFunction`, not
/// `JitMaterialized`.
#[test]
#[serial]
fn user_define_c_function_overrides_materialization() {
    setup();
    let bindings = introspect_bindings(GETTICKCOUNT_USER_DECL, "GetTickCount()")
        .unwrap_or_else(|e| panic!("introspect failed: {e:?}"));
    let gtc = bindings
        .iter()
        .find(|b| b.dylan_name == "GetTickCount")
        .unwrap_or_else(|| panic!("no GetTickCount binding; saw {bindings:#?}"));
    eprintln!(
        "[sprint31] user-declared GetTickCount: c_name={} library={} source={:?}",
        gtc.c_name, gtc.library, gtc.source
    );
    assert_eq!(
        gtc.source,
        BindingSource::UserCFunction,
        "user `define c-function` must win over JIT materialization; got {:?}",
        gtc.source
    );
    // Exactly one binding for `GetTickCount` — the user's. No
    // duplicate JIT-materialized binding may exist.
    let count = bindings
        .iter()
        .filter(|b| b.dylan_name == "GetTickCount")
        .count();
    assert_eq!(
        count, 1,
        "expected exactly one GetTickCount binding; got {count}: {bindings:#?}"
    );
}

// ─── 8. Unsupported-signature decline ─────────────────────────────────────

/// Bare-name calls to functions whose signature isn't supported must
/// surface a clean diagnostic, not silently succeed. Sprint 36b raised
/// the arity cap from 8 → 12, so functions like `CreateProcessW`
/// (arity 10) now materialise successfully; the test was updated to
/// use a clearly-non-existent name that drives the UnknownCallee
/// fallback path.
///
/// (The embedded blob's build.rs filter drops ~3,200 functions for
/// `bad_type` — struct-by-value, COM interfaces, etc. — so any
/// genuine "unsupported type" Win32 entry is already absent from the
/// index. Sprint 40d folded `function_pointer` / `delegate` params
/// into the accepted set so the ~1,987 callback-taking APIs are no
/// longer rejected; the residual ~3,200 are mostly struct-by-value
/// signatures. Sprint 37+ may add a path that surfaces those with a
/// richer diagnostic rather than UnknownCallee.)
#[test]
#[serial]
fn unsupported_name_declines_materialization() {
    setup();
    let result = eval_expr_to_string("NotARealWin32FunctionEverXyzzy42()");
    let err = result.expect_err("nonexistent name must reject");
    let msg = format!("{err:?}");
    let mentions_name = msg.contains("NotARealWin32FunctionEverXyzzy42");
    let unknown_or_unbound =
        msg.contains("UnknownCallee") || msg.contains("Unbound") || msg.contains("undefined");
    assert!(
        mentions_name && unknown_or_unbound,
        "expected an UnknownCallee/Unbound error mentioning the name; got: {msg}"
    );
}

// ─── 9. Cross-DLL ambiguity priority ──────────────────────────────────────

/// Cross-DLL name collisions break by priority order. The embedded
/// ~15,067-function subset (post-Sprint 40d) has no Win32 names
/// appearing in multiple DLLs the materializer would consider
/// equally good — the
/// `WINAPI_DLL_PRIORITY` table is more interesting as a unit-tested
/// pure function. We exercise it via a sema-side direct check on the
/// already-resolved bindings below.
///
/// If a future expansion of the embedded index DOES surface a genuine
/// collision, the test would flag it via a non-deterministic dll pick;
/// adjust the priority table or this test then.
#[test]
#[serial]
#[ignore = "no actual cross-DLL collisions in the current embedded blob; \
            priority ordering covered by the pure-function unit test in nod-sema"]
fn ambiguous_name_picks_kernel32_first() {
    setup();
    // Intentionally empty — left as a marker for future regression
    // coverage if the embedded blob ever surfaces a genuine collision.
}

// ─── 10. Stats: materialization count tracking ────────────────────────────

/// The `winffi_stats().materialized_lifetime` counter must bump every
/// time the sema layer synthesizes a binding. Two distinct bare-name
/// calls in the same module materialize two bindings.
#[test]
#[serial]
fn stats_show_materialization_count() {
    setup();
    nod_runtime::_reset_winffi_stats_for_tests();
    let s = eval_expr_to_string("GetTickCount64() + GetCurrentProcessId()")
        .unwrap_or_else(|e| panic!("two-materializations eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer sum");
    assert!(n > 0, "sum must be positive; got {n}");
    let stats = nod_runtime::winffi_stats();
    assert_eq!(
        stats.materialized_lifetime, 2,
        "expected 2 materializations (GetTickCount64 + GetCurrentProcessId), got {}",
        stats.materialized_lifetime
    );
}

// ─── 11. Repeated bare-name calls dedupe ──────────────────────────────────

/// Two bare-name calls to the SAME function share one stub-table slot
/// and one materialization (the Sprint 31 dedupe path piggybacks on
/// Sprint 28's `spec_dedupe`). Only one materialization counter bump.
#[test]
#[serial]
fn duplicate_bare_calls_share_one_materialization() {
    setup();
    nod_runtime::_reset_winffi_stats_for_tests();
    let s = eval_expr_to_string("GetTickCount64() + GetTickCount64()")
        .unwrap_or_else(|e| panic!("dedupe eval failed: {e:?}"));
    let n: i64 = s.parse().expect("integer sum");
    assert!(n > 2_000, "two-uptime sum > 2000ms; got {n}");
    let stats = nod_runtime::winffi_stats();
    assert_eq!(
        stats.materialized_lifetime, 1,
        "two calls to the same materialized function must share one slot; got {}",
        stats.materialized_lifetime
    );
}

// ─── 12. Sprint 40d — bare-name callback-taking APIs materialize ──────────

/// Sprint 40d headline (JIT side). Before Sprint 40d the
/// `nod_winapi` projection skipped any function whose signature
/// mentioned a `function_pointer` / `delegate` param (the SQL `kind`
/// `classify_type` rejected outright) — including the entire family
/// of callback-taking Win32 APIs (`EnumWindows`, `EnumChildWindows`,
/// `EnumThreadWindows`, `SetWindowsHookExW`, `CallWindowProcW`, …)
/// and every function with an `LPARAM` / `WPARAM` / `HINSTANCE`
/// parameter (stored as `kind = "struct"` in the DB but really a
/// typedef'd integer/handle). Sprint 40d extends `classify_type` to
/// (a) collapse `function_pointer` / `delegate` to an opaque
/// `<c-pointer>`, and (b) fall through `struct`-kind rows to the
/// named-typedef table so `LPARAM` resolves as `i64`, `HINSTANCE`
/// as a handle, etc. The result is that bare-name `EnumWindows`
/// (which exercises BOTH arms in its 2-param signature) now
/// materializes successfully from `user32.dll`.
///
/// This test exercises the materialization path only — we DO NOT
/// invoke the callback through the OS (that's `winffi_callbacks.rs`
/// which uses the explicit `define c-function` form). We pass a
/// `$NULL` callback (the OS would crash trying to invoke it, so
/// we don't actually call EnumWindows here either). Instead we
/// introspect the bindings after a parse + sema pass and confirm
/// `EnumWindows` materialized from `user32.dll` as a
/// `BindingSource::JitMaterialized` entry. That proves the
/// `classify_type` extension flowed all the way through.
#[test]
#[serial]
fn bare_EnumWindows_materializes_from_user32() {
    setup();
    let bindings = introspect_bindings(
        "",
        "let cb-ptr = $NULL; EnumWindows(cb-ptr, $NULL)",
    )
    .unwrap_or_else(|e| panic!("EnumWindows introspection failed: {e:?}"));
    let ew = bindings
        .iter()
        .find(|b| b.dylan_name == "EnumWindows")
        .unwrap_or_else(|| panic!("no EnumWindows binding materialized; saw {bindings:#?}"));
    eprintln!(
        "[sprint40d JIT] EnumWindows introspection: c_name={} library={} source={:?}",
        ew.c_name, ew.library, ew.source
    );
    assert_eq!(
        ew.c_name, "EnumWindows",
        "EnumWindows must resolve to its own name (no A/W suffix)"
    );
    assert_eq!(
        ew.library, "user32.dll",
        "EnumWindows lives in user32.dll, not {}",
        ew.library
    );
    assert!(
        ew.source == BindingSource::JitMaterialized,
        "EnumWindows came from the projected index, not a user declaration; \
         source = {:?}",
        ew.source
    );
}

/// Sprint 40d companion — `SetWindowsHookExW` is another canonical
/// callback-taking API. Its first param is a `HOOKPROC` (different
/// `delegate` row than `WNDENUMPROC`), proving the fix isn't
/// specific to one callback type. Same introspection-only approach
/// — we don't actually install a hook in `cargo test`. The `$NULL`
/// callback is rejected at the OS level if invoked, but we never
/// invoke it; the goal is to prove materialization succeeded.
#[test]
#[serial]
fn bare_SetWindowsHookExW_materializes_from_user32() {
    setup();
    let bindings = introspect_bindings(
        "",
        "SetWindowsHookExW(0, $NULL, $NULL, 0)",
    )
    .unwrap_or_else(|e| panic!("SetWindowsHookExW introspection failed: {e:?}"));
    let sh = bindings
        .iter()
        .find(|b| b.dylan_name == "SetWindowsHookExW")
        .unwrap_or_else(|| panic!(
            "no SetWindowsHookExW binding materialized; saw {bindings:#?}"
        ));
    eprintln!(
        "[sprint40d JIT] SetWindowsHookExW introspection: c_name={} library={} source={:?}",
        sh.c_name, sh.library, sh.source
    );
    assert_eq!(sh.library, "user32.dll");
    assert_eq!(
        sh.source,
        BindingSource::JitMaterialized,
        "expected JitMaterialized; got {:?}",
        sh.source
    );
}
