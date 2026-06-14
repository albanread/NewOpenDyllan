//! Sprint 27 Phase A — embedded `windows_api.db` lookup tests.
//!
//! These exercise the public `nod_winapi` API end-to-end against the
//! blob built by `build.rs` at compile time. No mocking, no
//! `LazyLock` reset — the index is process-global, so each test
//! observes the same data.

use nod_winapi::{Stats, TypeRef};

/// Tiny helper: walk through a `TypeRef::Alias` chain (if any) and
/// return the innermost `TypeRef`. Lets tests assert against the
/// underlying primitive without caring whether the DB chose to wrap
/// it as `DWORD` / `UINT32` / etc.
fn unwrap_alias(t: &TypeRef) -> &TypeRef {
    match t {
        TypeRef::Alias { base, .. } => unwrap_alias(base),
        other => other,
    }
}

#[test]
fn find_kernel32_beep_returns_correct_signature() {
    let f = nod_winapi::find_function("kernel32.dll", "Beep")
        .expect("Beep must be present in the embedded windows_api index");
    assert_eq!(f.name, "Beep");
    assert_eq!(f.dll, "kernel32.dll");
    // Beep(DWORD dwFreq, DWORD dwDuration) -> BOOL.
    assert_eq!(f.params.len(), 2, "Beep takes two parameters");
    for (i, p) in f.params.iter().enumerate() {
        match unwrap_alias(&p.type_ref) {
            TypeRef::U32 => {}
            other => panic!(
                "Beep param {i} expected U32 (DWORD alias acceptable), got {other:?}"
            ),
        }
    }
    match unwrap_alias(&f.return_type) {
        TypeRef::Bool32 => {}
        other => panic!("Beep return type expected Bool32, got {other:?}"),
    }
}

#[test]
fn find_function_lookup_is_case_insensitive_on_dll() {
    let lower = nod_winapi::find_function("kernel32.dll", "Beep")
        .expect("Beep must be present (lowercased DLL)");
    let upper = nod_winapi::find_function("KERNEL32.DLL", "Beep")
        .expect("Beep must be present (uppercased DLL)");
    assert_eq!(lower.name, upper.name);
    assert_eq!(lower.dll, upper.dll);
}

#[test]
fn find_constant_mb_ok_returns_zero() {
    let c = nod_winapi::find_constant("MB_OK").expect("MB_OK in constants");
    assert_eq!(c.value, 0);
    assert_eq!(c.source_dll.as_deref(), Some("user32.dll"));
}

#[test]
fn iter_kernel32_returns_300_plus_functions() {
    // The Sprint 27 projection covers all primitive-typed
    // signatures; kernel32 has well over 300 such functions
    // (most of the win32 file / process / synchronisation API).
    // Set a healthy floor at 300; the actual count when this test
    // was written was ~1165.
    let count = nod_winapi::iter_dll("kernel32.dll").count();
    assert!(
        count >= 300,
        "expected at least 300 kernel32 functions in the projected subset, got {count}"
    );
}

#[test]
fn total_blob_size_under_3mb() {
    // Sprint 27 hard budget — the embedded zstd blob must stay
    // under 3 MB. Current size (Sprint 27 commit): ~205 KB.
    let bytes = nod_winapi::embedded_blob_bytes();
    assert!(
        bytes.len() < 3 * 1024 * 1024,
        "embedded winapi blob is {} bytes; budget is 3 MB",
        bytes.len()
    );
}

#[test]
fn stats_are_self_consistent() {
    let Stats { function_count, constant_count, dll_count, blob_bytes } = nod_winapi::stats();
    assert!(function_count > 0);
    assert!(constant_count > 0);
    assert!(dll_count > 0);
    assert_eq!(blob_bytes, nod_winapi::embedded_blob_bytes().len());
    assert_eq!(function_count, nod_winapi::functions().len());
    assert_eq!(constant_count, nod_winapi::constants().len());
    assert_eq!(dll_count, nod_winapi::dll_names().len());
}

#[test]
fn find_function_returns_none_for_unknown_name() {
    assert!(nod_winapi::find_function("kernel32.dll", "DefinitelyNotAFunction").is_none());
    assert!(nod_winapi::find_function("nonexistent.dll", "Beep").is_none());
}

/// Sprint 40d — `classify_type` now accepts `function_pointer` (and
/// `delegate`) param types by collapsing them to an opaque
/// `<c-pointer>` (`TypeRef::Pointer { pointee_type_ref: None }`).
/// Before Sprint 40d, any function whose signature mentioned a
/// callback type (WNDPROC, WNDENUMPROC, HOOKPROC, DLGPROC,
/// TIMERPROC, ENUMRESLANGPROCW, …) was dropped from the projected
/// subset because `classify_type` returned `None` for it, marking
/// the enclosing function as `bad_type`. After Sprint 40d, the
/// canonical headline `EnumWindows` (user32.dll) is in the
/// projection — which unblocks bare-name calls in both the JIT
/// (Sprint 31 materialization) and the AOT pipeline (Sprint 40b
/// callbacks).
#[test]
fn find_enum_windows_after_callback_projection() {
    let f = nod_winapi::find_function("user32.dll", "EnumWindows")
        .expect("EnumWindows must be in the projected subset after Sprint 40d");
    assert_eq!(f.name, "EnumWindows");
    assert_eq!(f.dll, "user32.dll");
    // EnumWindows(WNDENUMPROC lpEnumFunc, LPARAM lParam) -> BOOL.
    // The first param's WNDENUMPROC type now collapses to an opaque
    // `Pointer { None }`. The second is LPARAM (alias of i64) which
    // marshals fine. Return is BOOL → Bool32.
    assert_eq!(f.params.len(), 2, "EnumWindows takes two parameters");
    match &f.params[0].type_ref {
        TypeRef::Pointer { pointee_type_ref: None } => {}
        other => panic!(
            "EnumWindows param 0 expected opaque Pointer (from WNDENUMPROC \
             function_pointer collapse), got {other:?}"
        ),
    }
    match unwrap_alias(&f.return_type) {
        TypeRef::Bool32 => {}
        other => panic!("EnumWindows return expected Bool32, got {other:?}"),
    }
}

/// Sprint 40d companion — a sample of other callback-taking APIs
/// must now be reachable: `SetWindowsHookExW` (HOOKPROC),
/// `EnumChildWindows` (WNDENUMPROC), `EnumThreadWindows`
/// (WNDENUMPROC). All three are in user32.dll. This proves the
/// fix is not specific to one DLL or one callback kind.
#[test]
fn callback_taking_apis_projected_after_sprint_40d() {
    for name in [
        "SetWindowsHookExW",
        "EnumChildWindows",
        "EnumThreadWindows",
    ] {
        assert!(
            nod_winapi::find_function("user32.dll", name).is_some(),
            "{name} must be in the projected subset after Sprint 40d \
             (function_pointer params no longer rejected)"
        );
    }
}
