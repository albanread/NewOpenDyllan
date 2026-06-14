// Postcard-serializable schema for the embedded Windows API metadata.
//
// The same definitions are consumed by `build.rs` (to write the blob
// from the vendored SQLite source DB) and by `lib.rs` (to decode the
// blob at first access). The file is `include!()`'d into both crates;
// it intentionally has no `mod` declaration of its own and uses only
// outer doc comments so it can be included anywhere.
//
// Wire-format stability: Sprint 27 commits to *no stability*. The
// blob is rebuilt from source on every `cargo build`, so changes to
// this file just require a clean rebuild of `nod-winapi`. Downstream
// consumers go through the public `nod_winapi` API; the wire format
// is private.

use serde::{Deserialize, Serialize};

/// A single Win32 API function projected from the vendored SQLite DB.
///
/// The structure is deliberately flat — no shared interning — to keep
/// the postcard schema dead-simple. We expect ≤ ~1500 functions in the
/// embedded subset, so the redundancy in `dll` / `callconv` strings is
/// cheap (zstd-19 squashes it).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionInfo {
    /// Function name as exported, e.g. `"Beep"`. Sprint 27 does NOT
    /// auto-pick A/W variants — `MessageBox` and `MessageBoxA` /
    /// `MessageBoxW` are all separate entries.
    pub name: String,
    /// DLL providing the symbol, lower-cased with `.dll` suffix,
    /// e.g. `"kernel32.dll"`. Comes from `functions.dll_name`.
    pub dll: String,
    /// Calling convention as a free-form string, e.g. `"stdcall"`.
    /// We don't normalise here — the FFI lowering layer (Sprint 28)
    /// interprets.
    pub callconv: String,
    /// Return type. `TypeRef::Void` for `void`-returning functions.
    pub return_type: TypeRef,
    /// Parameters, source order.
    pub params: Vec<ParamInfo>,
    /// A/W charset family marker. `None` for functions that don't
    /// participate in the A/W naming convention, `Some(b'A')` for
    /// the ANSI variant, `Some(b'W')` for the wide variant.
    pub aw_family: Option<u8>,
    /// `SetLastError` semantics — true if the win32 contract is that
    /// `GetLastError()` carries an error code after a failed call.
    pub set_last_error: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ParamInfo {
    pub name: Option<String>,
    pub type_ref: TypeRef,
    pub direction: Direction,
    pub is_optional: bool,
}

/// Compact type reference. Restricted to primitive-typed signatures
/// per the Sprint 27 acceptance criteria — no struct-by-value, no
/// callback function pointers, no union arguments.
///
/// `Pointer` is recursive but capped at one level of indirection by
/// the DB projection in `build.rs`; pointers-to-pointers collapse to
/// `Pointer { pointee: None }` (opaque).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TypeRef {
    Void,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    /// 32-bit boolean. `BOOL` is a typedef of `i32` in the Windows
    /// headers; we surface it separately so the Dylan side can map
    /// it to `<c-bool>` rather than `<c-int>`.
    Bool32,
    /// Pointer to `pointee_type_ref`, or opaque pointer if `None`.
    Pointer {
        pointee_type_ref: Option<Box<TypeRef>>,
    },
    /// Opaque `HANDLE` / `HWND` / `HMODULE` / etc. — pointer-sized.
    Handle,
    /// Narrow-string pointer (`LPSTR` / `LPCSTR`). Distinguished from
    /// `Pointer { I8 }` because the Dylan side wants to marshal them
    /// from `<byte-string>`.
    NarrowString,
    /// Wide-string pointer (`LPWSTR` / `LPCWSTR`).
    WideString,
    /// Enum whose representation is `base`. The named identity is
    /// dropped here — Sprint 27 doesn't carry enum case lists.
    Enum {
        base: Box<TypeRef>,
    },
    /// Typedef of `base`, with the Windows-side typedef name kept
    /// for diagnostics (e.g. `DWORD` aliasing `U32`).
    Alias {
        name: String,
        base: Box<TypeRef>,
    },
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, Eq, PartialEq)]
pub enum Direction {
    In,
    Out,
    InOut,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConstantInfo {
    /// Constant name, e.g. `"MB_OK"`.
    pub name: String,
    /// Numeric value. `i64` covers every Windows constant we project
    /// in Sprint 27 (the widest are 64-bit error codes); larger
    /// values get dropped during projection.
    pub value: i64,
    /// Origin DLL if known. Most constants don't carry one — they're
    /// header-defined.
    pub source_dll: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct WinApiIndex {
    pub functions: Vec<FunctionInfo>,
    pub constants: Vec<ConstantInfo>,
    /// Distinct DLL names present in `functions` (deduplicated,
    /// retained for `iter_dlls()` and the size-assertion test).
    pub dll_names: Vec<String>,
}
