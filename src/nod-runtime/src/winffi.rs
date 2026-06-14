//! Sprint 28–30 — per-module API stub tables + Win64 marshaling
//! trampolines for `define c-function` calls.
//!
//! ## Architecture (from the Sprint 28 brief)
//!
//! Each Dylan module compiled at JIT time gets a **per-module API stub
//! table**. Each unique `(dll, symbol)` referenced in the module's
//! `define c-function`s gets ONE [`ApiStubEntry`]. Multiple call sites
//! for the same API share the same entry — PLT-like deduplication.
//!
//! At JIT-finalize, the runtime walks the table, `LoadLibrary`s each
//! unique DLL, `GetProcAddress`es each symbol, and populates the
//! entry's `fn_ptr` atomically. Lazy/PLT-style on-first-use is a
//! Sprint 38+ optimisation; eager init keeps Sprint 28 small.
//!
//! Per-call codegen lowers `Beep(440, 200)` to a `DirectCall` against
//! a synthetic `%winffi-call-N` callee (N = arg count). Codegen emits
//! `nod_winffi_call_N(entry_ptr_const, a0, …, aN-1)`. The trampoline
//! unboxes each arg per the entry's recorded [`ApiCallSignature`],
//! invokes the function pointer through an `extern "system"` (Win64)
//! signature, and reboxes the return as a Dylan [`Word`].
//!
//! ## Sprint 28 scope
//!
//! - Integer args/returns: `<c-bool>`, `<c-byte>`, `<c-short>`,
//!   `<c-int>`, `<c-long>`, `<c-dword>`, `<c-uint>`, `<c-ulong>`.
//! - Pointer/handle args/returns: `<c-pointer>`, `<c-handle>`.
//! - Up to **8 args per call** (Win64: RCX/RDX/R8/R9 + 4 stack slots).
//!
//! ## Sprint 30 additions
//!
//! - String args: `<c-string>` (narrow, UTF-8 byte sequence + null
//!   terminator, used as LPSTR/LPCSTR) and `<c-wide-string>` (wide,
//!   UTF-16LE + null u16, used as LPWSTR/LPCWSTR). Each call allocates
//!   per-arg [`TempBuf`]s that live for the duration of the call only.
//! - String returns: a returned `LPCSTR` or `LPCWSTR` (e.g. from
//!   `GetCommandLineW`) is scanned to its null terminator and copied
//!   into a fresh Dylan `<byte-string>`.
//! - NULL pointer: a fixnum `0` in a pointer / handle / string position
//!   marshals to a null pointer. The `$NULL` Dylan constant is just
//!   `0`, so `MessageBoxW($NULL, ...)` works.
//!
//! Out of scope (later sprints):
//!
//! - Structs by value (Sprint 34) and out-buffer string patterns like
//!   `GetWindowTextW(hwnd, buf, len)` — Sprint 34 territory.
//! - Callbacks / function pointers (Sprint 33).
//! - COM interfaces / BSTR (Sprint 35).
//! - Variadics, structured `GetLastError`, auto-raise on failure.
//! - Full CP_ACP conversion for non-ASCII narrow strings (deferred —
//!   ASCII subset suffices for Sprint 30's tests).

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::classes::ClassId;
use crate::conditions::condition_class_name;
use crate::make::rust_make;
use crate::word::Word;

// ─── Argument / return kinds ──────────────────────────────────────────────

/// Marshaling kind for a single C argument. Stored as `u8` inside
/// [`ApiCallSignature::arg_kinds`] so the whole signature stays
/// `Copy + #[repr(C)]`.
#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum CArgKind {
    Void = 0,
    Int8 = 1,
    Int16 = 2,
    Int32 = 3,
    Int64 = 4,
    UInt8 = 5,
    UInt16 = 6,
    UInt32 = 7,
    UInt64 = 8,
    /// `BOOL` is a Win32 32-bit integer; Dylan side maps from
    /// `<boolean>` singletons (`#t` → 1, `#f` → 0) or from a fixnum.
    Bool32 = 9,
    /// Raw `*mut u8` (opaque pointer).
    Pointer = 10,
    /// `HANDLE` — opaque pointer-sized handle. ABI-identical to
    /// [`CArgKind::Pointer`]; kept distinct so error messages /
    /// diagnostics can surface the source type.
    Handle = 11,
    /// Sprint 30: narrow string (`<c-string>` → LPSTR/LPCSTR). The
    /// trampoline copies the Dylan `<byte-string>`'s bytes, appends a
    /// null terminator, and passes a pointer to the resulting
    /// process-stable temp buffer. The temp buffer is freed when the
    /// trampoline returns.
    NarrowString = 12,
    /// Sprint 30: wide string (`<c-wide-string>` → LPWSTR/LPCWSTR).
    /// The trampoline converts the Dylan `<byte-string>` from UTF-8 to
    /// UTF-16LE, appends a null u16, and passes the resulting pointer.
    WideString = 13,
    /// Sprint 35: 32-bit IEEE float argument. Registered for Dylan
    /// `<c-float>` declarations. **Not currently exercised by any
    /// Sprint 35 shim** — the shim layer takes Dylan `<integer>` args
    /// and converts to f32 internally (see `com_shim.rs` module
    /// docs for the deviation). Trampoline support for native float
    /// args ships in Sprint 36+.
    Float32 = 14,
    /// Sprint 35: 64-bit IEEE float argument. Registered for Dylan
    /// `<c-double>` declarations. See [`CArgKind::Float32`] caveat.
    Float64 = 15,
}

impl CArgKind {
    fn from_u8(b: u8) -> CArgKind {
        match b {
            0 => CArgKind::Void,
            1 => CArgKind::Int8,
            2 => CArgKind::Int16,
            3 => CArgKind::Int32,
            4 => CArgKind::Int64,
            5 => CArgKind::UInt8,
            6 => CArgKind::UInt16,
            7 => CArgKind::UInt32,
            8 => CArgKind::UInt64,
            9 => CArgKind::Bool32,
            10 => CArgKind::Pointer,
            11 => CArgKind::Handle,
            12 => CArgKind::NarrowString,
            13 => CArgKind::WideString,
            14 => CArgKind::Float32,
            15 => CArgKind::Float64,
            _ => panic!("nod-runtime/winffi: unknown CArgKind byte {b}"),
        }
    }

    /// Resolve a Dylan c-type class name (e.g. `<c-dword>`) to its
    /// marshaling kind. Sprint 28 panics on unknown names; the Sema
    /// layer is expected to validate names up front. None means the
    /// type isn't in the Sprint 28 supported set.
    pub fn from_c_type_name(name: &str) -> Option<CArgKind> {
        Some(match name {
            "<c-bool>" => CArgKind::Bool32,
            "<c-byte>" => CArgKind::UInt8,
            "<c-short>" => CArgKind::Int16,
            "<c-ushort>" => CArgKind::UInt16,
            "<c-int>" => CArgKind::Int32,
            "<c-uint>" => CArgKind::UInt32,
            "<c-long>" => CArgKind::Int32,
            "<c-ulong>" => CArgKind::UInt32,
            "<c-longlong>" => CArgKind::Int64,
            "<c-ulonglong>" => CArgKind::UInt64,
            "<c-dword>" => CArgKind::UInt32,
            "<c-word>" => CArgKind::Int64,
            "<c-pointer>" => CArgKind::Pointer,
            "<c-handle>" => CArgKind::Handle,
            "<c-string>" => CArgKind::NarrowString,
            "<c-wide-string>" => CArgKind::WideString,
            // Sprint 35: float types registered but not yet exercised
            // by any shim. Sema accepts them in `define c-function`
            // declarations; the trampoline path for them is Sprint 36+.
            "<c-float>" => CArgKind::Float32,
            "<c-double>" => CArgKind::Float64,
            _ => return None,
        })
    }
}

/// Marshaling kind for a C return value. Same shape as [`CArgKind`]
/// but with a narrower set — there's no return-by-value `Int8`/`Int16`
/// in any Win32 API the Sprint 28 acceptance tests touch (they're
/// promoted to Int32 by the C ABI anyway).
#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum CReturnKind {
    Void = 0,
    Int32 = 1,
    Int64 = 2,
    UInt32 = 3,
    UInt64 = 4,
    Bool32 = 5,
    Pointer = 6,
    Handle = 7,
    /// Sprint 30: narrow string return (`<c-string>` returned from a
    /// Win32 API that yields LPCSTR — e.g. `lstrcatA` returns the
    /// destination pointer). The trampoline scans the returned pointer
    /// to its null terminator and copies the bytes into a fresh Dylan
    /// `<byte-string>`.
    NarrowString = 8,
    /// Sprint 30: wide string return (`<c-wide-string>` returned from a
    /// Win32 API yielding LPCWSTR — e.g. `GetCommandLineW`). Scanned to
    /// the null u16, UTF-16 → UTF-8 converted, copied into a fresh
    /// Dylan `<byte-string>`.
    WideString = 9,
    /// Sprint 35: 32-bit IEEE float return. Registered for `<c-float>`
    /// returns from future float-aware shim functions. Sprint 35 itself
    /// doesn't use this — no shim returns a native float.
    Float32 = 10,
    /// Sprint 35: 64-bit IEEE float return. Registered for `<c-double>`
    /// returns. See [`CReturnKind::Float32`] caveat.
    Float64 = 11,
}

impl CReturnKind {
    fn from_u8(b: u8) -> CReturnKind {
        match b {
            0 => CReturnKind::Void,
            1 => CReturnKind::Int32,
            2 => CReturnKind::Int64,
            3 => CReturnKind::UInt32,
            4 => CReturnKind::UInt64,
            5 => CReturnKind::Bool32,
            6 => CReturnKind::Pointer,
            7 => CReturnKind::Handle,
            8 => CReturnKind::NarrowString,
            9 => CReturnKind::WideString,
            10 => CReturnKind::Float32,
            11 => CReturnKind::Float64,
            _ => panic!("nod-runtime/winffi: unknown CReturnKind byte {b}"),
        }
    }

    /// Resolve a Dylan c-type class name to a return kind. Sprint 28
    /// only handles the kinds the acceptance tests use.
    pub fn from_c_type_name(name: &str) -> Option<CReturnKind> {
        Some(match name {
            "<c-bool>" => CReturnKind::Bool32,
            "<c-int>" => CReturnKind::Int32,
            "<c-uint>" => CReturnKind::UInt32,
            "<c-long>" => CReturnKind::Int32,
            "<c-ulong>" => CReturnKind::UInt32,
            "<c-longlong>" => CReturnKind::Int64,
            "<c-ulonglong>" => CReturnKind::UInt64,
            "<c-dword>" => CReturnKind::UInt32,
            "<c-word>" => CReturnKind::Int64,
            "<c-pointer>" => CReturnKind::Pointer,
            "<c-handle>" => CReturnKind::Handle,
            "<c-string>" => CReturnKind::NarrowString,
            "<c-wide-string>" => CReturnKind::WideString,
            // Sprint 35: float types registered for sema. Trampoline
            // path for them is Sprint 36+.
            "<c-float>" => CReturnKind::Float32,
            "<c-double>" => CReturnKind::Float64,
            _ => return None,
        })
    }
}

/// Packed Win64 marshaling signature for a single c-function. Stored
/// in [`ApiStubEntry::signature`]; the trampoline reads it to decide
/// how to unbox each arg and rebox the return.
///
/// `#[repr(C)]` so the IR can bake field offsets if needed.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ApiCallSignature {
    /// Number of arguments (0..=12). Sprint 36b raised the cap from
    /// Sprint 28's 8 to 12 to support CreateWindowExW + the rest of
    /// the IDE-shell Win32 surface. The trampoline at arity N
    /// expects `arg_count == N`.
    pub arg_count: u8,
    /// Packed arg kinds; only the first `arg_count` entries are
    /// meaningful. Indices `arg_count..12` MUST be `CArgKind::Void` (0).
    pub arg_kinds: [u8; 12],
    /// Return value kind.
    pub return_kind: u8,
}

// ─── ApiStubEntry / ApiStubTable ──────────────────────────────────────────

/// One row in a module's API stub table. Pinned in the static area
/// for the module's lifetime so its address can be baked into JIT-
/// emitted constants.
///
/// `#[repr(C)]` keeps the field order stable across Rust versions.
/// The trampolines read `fn_ptr` and `signature` directly off this
/// struct via raw pointer arithmetic.
#[repr(C)]
pub struct ApiStubEntry {
    /// DLL name as a raw UTF-8 byte slice (NOT null-terminated; the
    /// resolver builds its own `CString` on first use). Static-area
    /// storage; valid for the process lifetime.
    pub dll_name_ptr: *const u8,
    pub dll_name_len: u32,
    /// Symbol name, same lifetime + non-null-terminated storage.
    pub symbol_name_ptr: *const u8,
    pub symbol_name_len: u32,
    /// Resolved function pointer, populated at module init via
    /// [`initialize_stub_table`]. Null until then.
    ///
    /// `AtomicPtr` for safe publication across threads. The
    /// trampoline does an `Acquire` load; init does a `Release` store.
    pub fn_ptr: AtomicPtr<u8>,
    /// Marshaling signature for this c-function.
    pub signature: ApiCallSignature,
}

// SAFETY: ApiStubEntry contains only POD + AtomicPtr; the `*const u8`
// pointers point at static-area UTF-8 bytes that live for the process.
unsafe impl Send for ApiStubEntry {}
unsafe impl Sync for ApiStubEntry {}

/// A module's complete API stub table. A `'static` slice of entries
/// (each pinned in the static area). The table itself is also pinned.
pub struct ApiStubTable {
    pub entries: &'static [ApiStubEntry],
}

// ─── Process-wide registry of resolved libraries ──────────────────────────

#[cfg(windows)]
mod resolver {
    use super::*;
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

    /// (DLL name → HMODULE) cache. Once a DLL is loaded, the handle is
    /// kept alive for the process lifetime; the `Mutex` only protects
    /// the map's structure, not the handles themselves.
    static LOADED_LIBRARIES: OnceLock<Mutex<HashMap<String, isize>>> = OnceLock::new();

    fn libs() -> &'static Mutex<HashMap<String, isize>> {
        LOADED_LIBRARIES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Look up `symbol` in `dll`, loading the DLL on first reference.
    /// Returns null on failure (callers raise `<c-ffi-error>`).
    ///
    /// On Windows this calls the raw `LoadLibraryA` /
    /// `GetProcAddress` Win32 APIs through `windows-sys` (which
    /// `nod-runtime` already depends on for `Win32_System_Memory`).
    /// We considered `libloading` per the brief; on Windows it adds a
    /// dependency for code that's a thin wrapper over the two APIs we
    /// need, so we use `windows-sys` directly. This keeps the
    /// build-deps footprint identical to Sprint 27. (Documented as a
    /// deliberate deviation from the brief.)
    pub fn resolve_symbol(dll: &str, symbol: &str) -> *const u8 {
        let mut guard = libs().lock().expect("winffi libs poisoned");
        let hmodule: isize = if let Some(&h) = guard.get(dll) {
            h
        } else {
            let Ok(c) = CString::new(dll) else { return std::ptr::null() };
            // SAFETY: LoadLibraryA takes a null-terminated ASCII string.
            // CString::as_ptr returns a stable pointer for the call's
            // duration.
            let h = unsafe { LoadLibraryA(c.as_ptr() as *const u8) };
            if h.is_null() {
                return std::ptr::null();
            }
            guard.insert(dll.to_string(), h as isize);
            h as isize
        };
        let Ok(c) = CString::new(symbol) else { return std::ptr::null() };
        // SAFETY: GetProcAddress takes an HMODULE + null-terminated
        // ASCII symbol name. The HMODULE is the just-cached handle.
        let p = unsafe { GetProcAddress(hmodule as HMODULE, c.as_ptr() as *const u8) };
        match p {
            Some(f) => f as *const u8,
            None => std::ptr::null(),
        }
    }
}

#[cfg(not(windows))]
mod resolver {
    /// Non-Windows builds: `resolve_symbol` always returns null. The
    /// Sprint 28 acceptance tests are `#[cfg(windows)]`; this keeps
    /// the workspace buildable on Linux for CI smoke runs.
    pub fn resolve_symbol(_dll: &str, _symbol: &str) -> *const u8 {
        std::ptr::null()
    }
}

pub use resolver::resolve_symbol;

// ─── Statistics — for the dedupe test ────────────────────────────────────

#[derive(Copy, Clone, Debug, Default)]
pub struct WinFfiStats {
    /// Cumulative number of stub-table entries allocated across every
    /// module the process has lowered.
    pub entries: usize,
    /// Cumulative number of successful `(dll, symbol)` resolutions
    /// performed by `initialize_stub_table`.
    pub total_resolved: usize,
    /// Cumulative number of unique `(dll, symbol)` pairs that have
    /// resolved. This is `<= entries`. For Sprint 28 (one module per
    /// JIT session, eager init) we typically have `entries ==
    /// total_resolved == unique_symbols`.
    pub unique_symbols: usize,
    /// Sprint 30: cumulative count of [`TempBuf`] allocations made by
    /// the marshaling trampolines. Each `<c-string>` / `<c-wide-string>`
    /// arg in a Win32 call bumps this counter. Useful for tracking
    /// per-call allocation overhead and for the Sprint 30 acceptance
    /// reporting; the buffer itself is freed at end of call (Vec drop).
    pub tempbufs_allocated_lifetime: usize,
    /// Sprint 31: cumulative count of c-function bindings the sema
    /// layer materialized from the embedded `nod-winapi` index because
    /// a bare-name call site referenced a Win32 export the user hadn't
    /// declared with `define c-function`. Useful for diagnostics and
    /// for the Sprint 31 acceptance assertion that materialization is
    /// actually happening.
    pub materialized_lifetime: usize,
}

static STAT_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static STAT_RESOLVED: AtomicUsize = AtomicUsize::new(0);
static STAT_UNIQUE: AtomicUsize = AtomicUsize::new(0);
static STAT_TEMPBUFS: AtomicUsize = AtomicUsize::new(0);
static STAT_MATERIALIZED: AtomicUsize = AtomicUsize::new(0);

static UNIQUE_KEYS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn unique_keys() -> &'static Mutex<std::collections::HashSet<String>> {
    UNIQUE_KEYS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Snapshot the WinFFI stats counters. Used by the
/// `api_stub_table_deduplicates_call_sites` acceptance test and the
/// Sprint 30 tempbuf accounting tests.
pub fn winffi_stats() -> WinFfiStats {
    WinFfiStats {
        entries: STAT_ENTRIES.load(Ordering::Relaxed),
        total_resolved: STAT_RESOLVED.load(Ordering::Relaxed),
        unique_symbols: STAT_UNIQUE.load(Ordering::Relaxed),
        tempbufs_allocated_lifetime: STAT_TEMPBUFS.load(Ordering::Relaxed),
        materialized_lifetime: STAT_MATERIALIZED.load(Ordering::Relaxed),
    }
}

#[doc(hidden)]
pub fn _reset_winffi_stats_for_tests() {
    STAT_ENTRIES.store(0, Ordering::Relaxed);
    STAT_RESOLVED.store(0, Ordering::Relaxed);
    STAT_UNIQUE.store(0, Ordering::Relaxed);
    STAT_TEMPBUFS.store(0, Ordering::Relaxed);
    STAT_MATERIALIZED.store(0, Ordering::Relaxed);
    if let Some(m) = UNIQUE_KEYS.get() {
        m.lock().expect("unique_keys poisoned").clear();
    }
}

/// Sprint 31: bump the `materialized_lifetime` counter. Called from
/// `nod-sema` whenever the JIT materialization hook synthesizes a new
/// c-function binding for a bare-name Win32 reference.
pub fn winffi_record_materialized() {
    STAT_MATERIALIZED.fetch_add(1, Ordering::Relaxed);
}

/// Record that one stub-table entry was allocated by the sema layer.
/// Called from `nod-sema` as part of building a per-module stub table.
pub fn record_stub_entry_allocated() {
    STAT_ENTRIES.fetch_add(1, Ordering::Relaxed);
}

/// Resolve one `(dll, symbol)` pair and store the result into `entry`.
/// Bumps the WinFFI stats counters. Returns `Ok(())` on success or
/// `Err(c_ffi_error_word)` on failure.
///
/// Used by the sema-side glue when finalising a JIT module — it walks
/// the per-module stub-table specs one-by-one rather than batch-init
/// through [`initialize_stub_table`] so it can plumb the resolved
/// entry pointers back into the lowering pass directly.
///
/// # Safety
/// `entry` must be a valid pointer to a static-area [`ApiStubEntry`].
pub unsafe fn resolve_into_entry(entry: *const ApiStubEntry, dll: &str, symbol: &str) -> Result<(), Word> {
    // SAFETY: caller's invariant — pinned in the static area for the
    // process lifetime.
    let e = unsafe { &*entry };
    if !e.fn_ptr.load(Ordering::Acquire).is_null() {
        // Already resolved; this is a no-op (the dedupe path means a
        // single entry can be re-visited if multiple modules reuse
        // the same (dll, symbol)).
        return Ok(());
    }
    let p = resolve_symbol(dll, symbol);
    if p.is_null() {
        let last_err = last_os_error_code();
        return Err(make_c_ffi_error(
            dll,
            symbol,
            last_err,
            &format!(
                "winffi: LoadLibrary/GetProcAddress failed for `{symbol}@{dll}` (OS error {last_err})"
            ),
        ));
    }
    e.fn_ptr.store(p as *mut u8, Ordering::Release);
    STAT_RESOLVED.fetch_add(1, Ordering::Relaxed);
    let key = format!("{dll}::{symbol}");
    let mut keys = unique_keys().lock().expect("unique_keys poisoned");
    if keys.insert(key) {
        STAT_UNIQUE.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

// ─── <c-ffi-error> condition class ────────────────────────────────────────

struct CFfiErrorClass {
    id: ClassId,
}

static C_FFI_ERROR_CLASS: OnceLock<CFfiErrorClass> = OnceLock::new();

/// Register the `<c-ffi-error>` condition class. Idempotent. Called
/// from `nod-sema` lowering when a `define c-function` is encountered.
pub fn ensure_c_ffi_error_registered() {
    crate::conditions::ensure_registered();
    let _ = C_FFI_ERROR_CLASS.get_or_init(|| {
        let error_id = crate::conditions::error_class_id();
        let (id, _) = crate::register_simple_user_class(
            "<c-ffi-error>",
            Some(error_id),
            vec![
                slot_str("dll-name", "dll-name"),
                slot_str("symbol-name", "symbol-name"),
                slot_int("os-error-code", "os-error-code"),
                slot_str("message", "message"),
            ],
        );
        CFfiErrorClass { id }
    });
}

fn slot_str(name: &str, init_kw: &str) -> crate::classes::SlotInfo {
    crate::classes::SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: crate::classes::SlotType::String,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: crate::classes::SlotDefault::Unbound,
        has_setter: false,
    }
}

fn slot_int(name: &str, init_kw: &str) -> crate::classes::SlotInfo {
    crate::classes::SlotInfo {
        name: name.to_string(),
        offset: 0,
        type_kind: crate::classes::SlotType::Integer,
        init_keyword: Some(init_kw.to_string()),
        required_init_keyword: false,
        default_init: crate::classes::SlotDefault::Unbound,
        has_setter: false,
    }
}

/// `<c-ffi-error>` ClassId accessor. Lazily ensures registration.
pub fn c_ffi_error_class_id() -> ClassId {
    ensure_c_ffi_error_registered();
    C_FFI_ERROR_CLASS
        .get()
        .expect("c-ffi-error registered")
        .id
}

/// Build a `<c-ffi-error>` heap instance.
pub fn make_c_ffi_error(dll: &str, symbol: &str, os_code: i64, message: &str) -> Word {
    ensure_c_ffi_error_registered();
    let md = crate::class_metadata_for(c_ffi_error_class_id());
    let dll_w = crate::intern_string_literal(dll);
    let sym_w = crate::intern_string_literal(symbol);
    let msg_w = crate::intern_string_literal(message);
    let code_w = Word::fixnum_unchecked(os_code);
    // SAFETY: registered metadata + matching keyword names.
    unsafe {
        rust_make(
            md,
            &[
                ("dll-name", dll_w),
                ("symbol-name", sym_w),
                ("os-error-code", code_w),
                ("message", msg_w),
            ],
        )
    }
}

// ─── Initialize a module's stub table ────────────────────────────────────

/// Walk `table`'s entries, `LoadLibrary` each unique DLL,
/// `GetProcAddress` each symbol, populate `fn_ptr`. Returns `Ok(())` on
/// success or `Err(c_ffi_error_word)` on the first failure.
///
/// Re-running this on a table whose entries are already populated is
/// a no-op (the `fn_ptr` check short-circuits each entry).
///
/// # Safety
/// `table.entries` must point at static-area [`ApiStubEntry`] records
/// whose `dll_name_ptr` / `symbol_name_ptr` point at valid UTF-8 byte
/// runs of the recorded lengths.
pub unsafe fn initialize_stub_table(table: &ApiStubTable) -> Result<(), Word> {
    for entry in table.entries.iter() {
        // If already resolved, skip.
        if !entry.fn_ptr.load(Ordering::Acquire).is_null() {
            continue;
        }
        // SAFETY: caller's invariant — pointers + lengths describe
        // valid UTF-8 byte runs in the static area.
        let dll = unsafe { str_from_raw(entry.dll_name_ptr, entry.dll_name_len as usize) };
        let symbol =
            unsafe { str_from_raw(entry.symbol_name_ptr, entry.symbol_name_len as usize) };
        let p = resolve_symbol(dll, symbol);
        if p.is_null() {
            let last_err = last_os_error_code();
            return Err(make_c_ffi_error(
                dll,
                symbol,
                last_err,
                &format!(
                    "winffi: LoadLibrary/GetProcAddress failed for `{symbol}@{dll}` (OS error {last_err})"
                ),
            ));
        }
        entry.fn_ptr.store(p as *mut u8, Ordering::Release);
        STAT_RESOLVED.fetch_add(1, Ordering::Relaxed);
        let key = format!("{dll}::{symbol}");
        let mut keys = unique_keys().lock().expect("unique_keys poisoned");
        if keys.insert(key) {
            STAT_UNIQUE.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn last_os_error_code() -> i64 {
    // SAFETY: GetLastError is a thread-local read with no preconditions.
    unsafe { windows_sys::Win32::Foundation::GetLastError() as i64 }
}

#[cfg(not(windows))]
fn last_os_error_code() -> i64 {
    0
}

/// # Safety
/// `ptr` + `len` must describe a valid UTF-8 byte run. The returned
/// `&str` shares the input's lifetime; callers must not hold it
/// across mutations of the underlying bytes (the static area never
/// mutates).
unsafe fn str_from_raw<'a>(ptr: *const u8, len: usize) -> &'a str {
    if ptr.is_null() || len == 0 {
        return "";
    }
    // SAFETY: caller's invariant.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    // The sema layer only writes UTF-8 (Dylan source identifiers are
    // ASCII / UTF-8 anyway). On the off-chance of an upstream bug we
    // fall back to an empty string rather than panic — the resolver
    // will then fail benignly.
    std::str::from_utf8(bytes).unwrap_or("")
}

// ─── Building a stub table from sema-side metadata ────────────────────────

/// Sema-side description of one c-function reference in the module
/// being lowered. The lowerer collects one of these per *unique*
/// `(dll, symbol)` pair across every call site of every c-function;
/// the per-call-site lowering then looks up the entry index and
/// emits a `WinFfiCall` (lowered as a DirectCall to a synthetic
/// `%winffi-call-N` callee carrying the entry pointer as a constant).
#[derive(Clone, Debug)]
pub struct StubEntrySpec {
    pub dll: String,
    pub symbol: String,
    pub signature: ApiCallSignature,
}

/// Allocate a fresh module-level stub table in the static area from
/// the supplied entry specs. Returns the table address (which lives
/// for the process lifetime) plus a parallel `Vec` of per-entry
/// pointers — those pointers are what the codegen layer bakes into
/// each call site's IR constant.
pub fn allocate_stub_table(specs: &[StubEntrySpec]) -> (&'static ApiStubTable, Vec<*const ApiStubEntry>) {
    crate::with_literal_pool(|pool| {
        let mut entries: Vec<ApiStubEntry> = Vec::with_capacity(specs.len());
        let mut pinned_dlls: Vec<&'static [u8]> = Vec::with_capacity(specs.len());
        let mut pinned_syms: Vec<&'static [u8]> = Vec::with_capacity(specs.len());
        for spec in specs {
            // Pin the dll / symbol names in the static area as raw byte
            // boxes. We can't reuse the literal-pool string interning
            // because those allocations carry a `<byte-string>` header
            // — the trampolines want raw bytes only.
            let dll_box: Box<[u8]> = spec.dll.as_bytes().to_vec().into_boxed_slice();
            let sym_box: Box<[u8]> = spec.symbol.as_bytes().to_vec().into_boxed_slice();
            // SAFETY: Box::leak gives a 'static slice; we'll never drop
            // these (intentional process-lifetime leak).
            let dll_static: &'static [u8] = Box::leak(dll_box);
            let sym_static: &'static [u8] = Box::leak(sym_box);
            pinned_dlls.push(dll_static);
            pinned_syms.push(sym_static);
            entries.push(ApiStubEntry {
                dll_name_ptr: dll_static.as_ptr(),
                dll_name_len: dll_static.len() as u32,
                symbol_name_ptr: sym_static.as_ptr(),
                symbol_name_len: sym_static.len() as u32,
                fn_ptr: AtomicPtr::new(std::ptr::null_mut()),
                signature: spec.signature,
            });
            STAT_ENTRIES.fetch_add(1, Ordering::Relaxed);
        }
        // Leak the entries vec into the static area, then build the
        // table struct itself.
        let entries_boxed: Box<[ApiStubEntry]> = entries.into_boxed_slice();
        // SAFETY: Box::leak gives a 'static slice; static-area lifetime.
        let entries_static: &'static [ApiStubEntry] = Box::leak(entries_boxed);
        let entry_ptrs: Vec<*const ApiStubEntry> =
            entries_static.iter().map(|e| e as *const _).collect();
        let table = pool.static_area.alloc(ApiStubTable { entries: entries_static });
        (table, entry_ptrs)
    })
}

// ─── Win64 marshaling helpers ─────────────────────────────────────────────

/// Per-call temporary buffer holding marshaled bytes for a single
/// `<c-string>` or `<c-wide-string>` argument. The trampoline allocates
/// one of these per string arg, pushes it into a local `Vec<TempBuf>`,
/// hands its `as_ptr()` to the C call, and the Vec drops at end of
/// scope — freeing every TempBuf's heap storage exactly when the C
/// call returns.
///
/// Lifetime rule: `as_ptr()` is valid only while `self` (and the owning
/// `Vec<TempBuf>`) lives. The trampoline structure guarantees this by
/// holding the `Vec` in a stack-local across the C call.
#[derive(Debug)]
enum TempBuf {
    /// Null-terminated UTF-8 bytes for an LPSTR/LPCSTR arg. For Sprint
    /// 30 the bytes are the raw Dylan `<byte-string>` payload plus a
    /// trailing 0; this matches CP_ACP only on the ASCII subset, but
    /// every Sprint 30 acceptance test stays ASCII for the narrow path.
    /// Full CP_ACP conversion (`WideCharToMultiByte`) is deferred.
    Narrow(Vec<u8>),
    /// Null-terminated UTF-16LE u16s for an LPWSTR/LPCWSTR arg. Built
    /// via `str::encode_utf16().collect::<Vec<u16>>()` + push(0).
    Wide(Vec<u16>),
}

impl TempBuf {
    /// Pointer to the first byte/u16 of the buffer, suitable for
    /// passing to a Win32 API expecting LPSTR / LPCSTR / LPWSTR /
    /// LPCWSTR.
    ///
    /// Sprint 30 currently captures the pointer at allocation time in
    /// `marshal_narrow_string` / `marshal_wide_string` (saving a match
    /// in the hot path), but the accessor is retained for future
    /// out-buffer codepaths (Sprint 34) that will need to address the
    /// payload after the trampoline returns.
    #[allow(dead_code)]
    fn as_ptr(&self) -> *const u8 {
        match self {
            TempBuf::Narrow(b) => b.as_ptr(),
            TempBuf::Wide(w) => w.as_ptr() as *const u8,
        }
    }
}

/// Build a narrow `TempBuf` from a Dylan `<byte-string>` Word. The
/// resulting buffer is the raw bytes followed by a single null
/// terminator. Pushes onto `temps` and bumps `STAT_TEMPBUFS`.
///
/// Returns the resulting pointer as a `u64` ready for the Win64 ABI.
fn marshal_narrow_string(w: Word, temps: &mut Vec<TempBuf>) -> u64 {
    // NULL pointer: fixnum 0 → null pointer. Matches the
    // documented `$NULL = 0` convention.
    if let Some(0) = w.as_fixnum() {
        return 0;
    }
    let s = read_dylan_byte_string(w);
    let mut bytes = Vec::with_capacity(s.len() + 1);
    bytes.extend_from_slice(s.as_bytes());
    bytes.push(0);
    let p = bytes.as_ptr() as u64;
    temps.push(TempBuf::Narrow(bytes));
    STAT_TEMPBUFS.fetch_add(1, Ordering::Relaxed);
    p
}

/// Build a wide `TempBuf` from a Dylan `<byte-string>` Word — decoding
/// the source UTF-8 to UTF-16LE and appending a null u16 terminator.
fn marshal_wide_string(w: Word, temps: &mut Vec<TempBuf>) -> u64 {
    if let Some(0) = w.as_fixnum() {
        return 0;
    }
    let s = read_dylan_byte_string(w);
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0);
    let p = units.as_ptr() as u64;
    temps.push(TempBuf::Wide(units));
    STAT_TEMPBUFS.fetch_add(1, Ordering::Relaxed);
    p
}

/// Read the bytes of a Dylan `<byte-string>` Word as `&str`. Panics if
/// the Word isn't a `<byte-string>` (the sema layer enforces the type
/// before lowering the call).
pub(crate) fn read_dylan_byte_string(w: Word) -> String {
    // SAFETY: the Word is type-checked by sema as a `<byte-string>`
    // (the only Dylan-side representation of `<c-string>` /
    // `<c-wide-string>` literals in Sprint 30). We resolve the
    // wrapper class against the global BYTE_STRING constant and
    // panic on mismatch so a sema-level bug surfaces loudly rather
    // than silently passing random bytes to a Win32 API.
    let Some(bs) = (unsafe {
        crate::try_byte_string(w, crate::ClassId::BYTE_STRING)
    }) else {
        panic!(
            "winffi: expected Dylan `<byte-string>` for a string-typed arg; got raw Word {:#x}",
            w.raw()
        );
    };
    // SAFETY: `bs` points at the live allocation; `bytes()` returns
    // borrowed inline payload. Copy into an owned String so the
    // borrow ends before we touch the heap again.
    let bytes = unsafe { bs.bytes() };
    // The Dylan source-text path produces UTF-8, but if a future
    // code path produces invalid UTF-8 we surface lossy text — the
    // C API can't handle non-UTF-8 narrow input meaningfully anyway.
    String::from_utf8_lossy(bytes).into_owned()
}

/// Unbox one Dylan-side arg Word according to its `kind` to a `u64`
/// suitable for the Win64 register/stack-slot ABI. Integers are
/// sign- or zero-extended to 64 bits as required.
///
/// String kinds (`NarrowString`, `WideString`) push a [`TempBuf`] into
/// `temps`; the returned u64 is the pointer to that buffer. `temps`
/// MUST outlive the C call (the caller holds it on its stack frame).
///
/// # Panics
/// On a Word that doesn't carry the expected payload (e.g. a non-fixnum
/// for an integer kind, or a non-`<byte-string>` for a string kind).
/// The Sema layer is responsible for type-checking before emitting the
/// WinFfiCall.
fn unbox_arg(w: Word, kind: u8, temps: &mut Vec<TempBuf>) -> u64 {
    let k = CArgKind::from_u8(kind);
    match k {
        CArgKind::Void => 0,
        CArgKind::Int8
        | CArgKind::Int16
        | CArgKind::Int32
        | CArgKind::Int64
        | CArgKind::UInt8
        | CArgKind::UInt16
        | CArgKind::UInt32
        | CArgKind::UInt64 => {
            // Dylan fixnum payload — extract the i64 value. The Win64
            // ABI takes integer args in 64-bit registers regardless of
            // declared width; truncation happens at the callee.
            let v = w.as_fixnum().unwrap_or_else(|| {
                panic!(
                    "winffi: expected fixnum-shaped integer arg for kind {k:?}; got raw {:#x}",
                    w.raw()
                )
            });
            v as u64
        }
        CArgKind::Bool32 => {
            // `<c-bool>` accepts either the Dylan boolean singletons
            // (`#t` / `#f`) OR a fixnum (0 = false, anything else =
            // true). Both forms encode to a u32 the Win32 ABI treats
            // as 0 or 1.
            let imm = crate::literal_pool_immediates();
            if w == imm.true_ {
                1
            } else if w == imm.false_ {
                0
            } else if let Some(n) = w.as_fixnum() {
                if n != 0 { 1 } else { 0 }
            } else {
                // Any other pointer-shaped Word counts as true (Dylan
                // truthiness — every non-#f value is true).
                1
            }
        }
        CArgKind::Pointer | CArgKind::Handle => {
            // Pointer payloads: a Dylan fixnum is treated as a raw
            // numeric handle (so callers can pass e.g. `$NULL` = 0).
            // A pointer-tagged Word carries an 8-byte-aligned address;
            // we strip the tag bit and pass the raw address.
            //
            // Sprint 34 auto-coerce: if the pointer-tagged Word is a
            // `<c-struct>` subclass instance, pass the address of its
            // byte payload (`wrapper_ptr + 8`) instead of the wrapper
            // address itself. This is what every IDE-essential Win32
            // API expects when it declares `LPRECT`, `LPMSG`, `LPPOINT`,
            // etc. — a pointer to the caller-allocated bytes, not to a
            // header-prefixed Dylan heap object.
            //
            // SAFETY: `is_c_struct_instance` checks the wrapper class is
            // a registered <c-struct> subclass; `as_ptr` returns the
            // wrapper's address; payload starts immediately after the
            // 8-byte Wrapper header. The payload lifetime spans the
            // call because the Dylan caller's stack frame keeps the
            // struct alive (the JIT registers the struct slot as a GC
            // root, exactly as for any other heap-allocated arg).
            if let Some(n) = w.as_fixnum() {
                n as u64
            } else if let Some(p) = w.as_ptr::<u8>() {
                if crate::is_c_struct_instance(w) {
                    (p as u64) + std::mem::size_of::<crate::Wrapper>() as u64
                } else {
                    p as u64
                }
            } else {
                0
            }
        }
        CArgKind::NarrowString => marshal_narrow_string(w, temps),
        CArgKind::WideString => marshal_wide_string(w, temps),
        CArgKind::Float32 | CArgKind::Float64 => {
            // Sprint 35: float kinds are *registered* in the enum but
            // the trampoline path for them is Sprint 36+. The Sprint
            // 35 COM shim uses integer-encoded scalar conventions
            // (color channels as 0..=255, coordinates as integer
            // pixels), so no `define c-function` declaration in the
            // Sprint 35 stdlib reaches this branch. We deliberately
            // panic rather than silently corrupt a float arg.
            panic!(
                "winffi: <c-float>/<c-double> arg encountered (kind {k:?}); \
                 Sprint 35 doesn't ship float-marshaling trampolines — \
                 the COM shim uses integer-encoded scalars. \
                 Native float args land in Sprint 36+."
            );
        }
    }
}

/// Rebox a raw u64 return value as a Dylan Word per the call's
/// recorded return kind.
fn box_return(raw: u64, kind: u8) -> Word {
    let k = CReturnKind::from_u8(kind);
    match k {
        CReturnKind::Void => crate::literal_pool_immediates().nil,
        CReturnKind::Int32 => {
            let v = raw as i32 as i64;
            Word::fixnum_unchecked(v)
        }
        CReturnKind::Int64 => Word::fixnum_unchecked(raw as i64),
        CReturnKind::UInt32 => Word::fixnum_unchecked((raw as u32) as i64),
        CReturnKind::UInt64 => {
            // u64 may overflow a 63-bit signed fixnum. Sprint 28
            // truncates to fixnum range; callers that need the full
            // u64 should declare the return as `<c-pointer>` instead.
            // The mask drops the sign bit so the result is always
            // representable as a non-negative fixnum.
            let masked = (raw & ((1u64 << 62) - 1)) as i64;
            Word::fixnum_unchecked(masked)
        }
        CReturnKind::Bool32 => {
            let imm = crate::literal_pool_immediates();
            if (raw as u32) != 0 { imm.true_ } else { imm.false_ }
        }
        CReturnKind::Pointer | CReturnKind::Handle => {
            // Pointer-shaped returns come back as a raw u64. We
            // surface them as a Dylan fixnum (carrying the raw
            // numeric handle value) — the Dylan side compares
            // against zero / known pseudo-handles using integer
            // comparison. Pointer-tagging the raw address only works
            // if it's 8-byte-aligned and not zero; Win32 pseudo-
            // handles like `(HANDLE)-1` aren't aligned, so we use the
            // numeric form which is robust for every value.
            //
            // 63-bit fixnum range can hold any 0x0..=0x7FFFFFFFFFFFFFFF
            // address; values with the sign bit set (kernel addresses,
            // pseudo-handles like -1) sign-extend correctly because
            // `as i64` reinterprets the bit pattern.
            Word::fixnum_unchecked(raw as i64)
        }
        CReturnKind::NarrowString => {
            // Sprint 30: API returned an LPCSTR pointer to a
            // process-owned (often static) UTF-8/ANSI byte run. Scan
            // to the null terminator and copy into a fresh Dylan
            // `<byte-string>`. NULL → empty Dylan string (the formatter
            // surfaces it as `""`).
            if raw == 0 {
                return crate::intern_string_literal("");
            }
            // SAFETY: `raw` is a pointer into process-stable memory
            // (per the Win32 API's contract — e.g. lstrcatA returns
            // its destination buffer). Bound the scan at 1MiB to
            // protect against unterminated strings from a buggy API.
            let bytes = unsafe { scan_cstr_bytes(raw as *const u8, 1 << 20) };
            let s = String::from_utf8_lossy(bytes).into_owned();
            crate::intern_string_literal(&s)
        }
        CReturnKind::Float32 | CReturnKind::Float64 => {
            // Sprint 35: see CArgKind::Float32 note. No Sprint 35
            // shim returns a native float; the kinds are registered
            // for sema-side acceptance of `<c-float>` / `<c-double>`
            // in `define c-function` declarations only.
            panic!(
                "winffi: <c-float>/<c-double> return encountered (kind {k:?}); \
                 Sprint 35 doesn't ship float-marshaling trampolines."
            );
        }
        CReturnKind::WideString => {
            // Sprint 30: API returned an LPCWSTR pointer (e.g.
            // GetCommandLineW). Scan to the null u16, convert
            // UTF-16LE → UTF-8 (lossy on any unpaired surrogates),
            // and allocate a Dylan `<byte-string>`.
            if raw == 0 {
                return crate::intern_string_literal("");
            }
            // SAFETY: same as the narrow path — bound the scan at
            // 1MiB worth of u16s.
            let units = unsafe { scan_wcstr_units(raw as *const u16, 1 << 20) };
            let s = String::from_utf16_lossy(&units);
            crate::intern_string_literal(&s)
        }
    }
}

/// Read bytes from `ptr` until a null terminator, capping at `max`
/// bytes. Returns a borrowed slice into the input memory — the caller
/// must not mutate the underlying region during the borrow.
///
/// # Safety
/// `ptr` must point at a readable region of at least `max` bytes (or be
/// null — in which case caller already handled the early-exit). The
/// returned slice's lifetime is tied to `ptr`'s validity.
unsafe fn scan_cstr_bytes<'a>(ptr: *const u8, max: usize) -> &'a [u8] {
    if ptr.is_null() {
        return &[];
    }
    let mut len = 0usize;
    while len < max {
        // SAFETY: caller's invariant — readable for `max` bytes.
        let b = unsafe { *ptr.add(len) };
        if b == 0 {
            break;
        }
        len += 1;
    }
    // SAFETY: ditto; len is bounded by max.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Read u16s from `ptr` until a null u16, capping at `max` units.
/// Returns an owned `Vec<u16>` because UTF-16 decoding requires owned
/// data on the caller side anyway (`String::from_utf16_lossy(&[u16])`).
///
/// # Safety
/// `ptr` must point at a readable region of at least `max` u16s (or be
/// null).
unsafe fn scan_wcstr_units(ptr: *const u16, max: usize) -> Vec<u16> {
    if ptr.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut len = 0usize;
    while len < max {
        // SAFETY: caller's invariant.
        let u = unsafe { *ptr.add(len) };
        if u == 0 {
            break;
        }
        out.push(u);
        len += 1;
    }
    out
}

/// Common prelude for all trampolines: load the resolved function
/// pointer, validate it's been populated, and capture the signature.
/// Returns the function pointer or panics with a deliberate error
/// (the lowering layer guarantees module init happens before any
/// call).
#[inline(always)]
unsafe fn trampoline_prelude(entry: *const ApiStubEntry) -> (*mut u8, ApiCallSignature) {
    // SAFETY: caller's invariant — entry is the leaked static-area
    // pointer baked into the IR constant.
    let entry_ref = unsafe { &*entry };
    let fn_ptr = entry_ref.fn_ptr.load(Ordering::Acquire);
    if fn_ptr.is_null() {
        // SAFETY: ditto — we just need the names for the panic
        // message.
        let dll = unsafe {
            str_from_raw(entry_ref.dll_name_ptr, entry_ref.dll_name_len as usize)
        };
        let sym = unsafe {
            str_from_raw(entry_ref.symbol_name_ptr, entry_ref.symbol_name_len as usize)
        };
        panic!(
            "winffi: c-function `{sym}@{dll}` called before initialize_stub_table populated its entry"
        );
    }
    // Sprint 41f — `NOD_TRACE_FNPTR=1` opt-in trace: log the resolved
    // (dll/symbol → fn_ptr → owning module path) tuple once per unique
    // function pointer. Default OFF; the env-var check is cached so
    // hot-path overhead is one atomic load + one branch when the trace
    // is disabled. The point is to confirm that the stub-table
    // resolution binds each `define c-function` to a plausible address
    // inside the expected DLL (e.g. that `MessageBoxW` resolves to
    // `C:\Windows\System32\user32.dll`, not somewhere bogus).
    if trace_fnptr_enabled() {
        // SAFETY: name pointers + lengths are static-area UTF-8 runs.
        let dll = unsafe {
            str_from_raw(entry_ref.dll_name_ptr, entry_ref.dll_name_len as usize)
        };
        let sym = unsafe {
            str_from_raw(entry_ref.symbol_name_ptr, entry_ref.symbol_name_len as usize)
        };
        record_fnptr_trace(dll, sym, fn_ptr);
    }
    (fn_ptr, entry_ref.signature)
}

// ─── Sprint 41f: NOD_TRACE_FNPTR diagnostic ──────────────────────────────
//
// One-shot env-var lookup (cached) + dedupe set keyed by fn_ptr address.
// The trace logs one line per unique resolved fn_ptr, to stderr, with the
// dll/symbol names from the stub entry, the raw address, and the module
// path returned by `GetModuleHandleEx(FROM_ADDRESS) + GetModuleFileNameW`.
//
// A unique-address dedupe set is sufficient: even though many call sites
// may share a single stub entry, every entry resolves to exactly one
// fn_ptr, so logging per address gives one line per `(dll, symbol)` over
// the lifetime of the process. The set is keyed by `usize` (the address)
// rather than by string to avoid allocating on the hot path.

/// Caches the `NOD_TRACE_FNPTR` env-var lookup. `0` = unknown, `1` =
/// disabled, `2` = enabled.
static TRACE_FNPTR_STATE: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn trace_fnptr_enabled() -> bool {
    match TRACE_FNPTR_STATE.load(Ordering::Relaxed) {
        2 => true,
        1 => false,
        _ => {
            let on = std::env::var_os("NOD_TRACE_FNPTR")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            TRACE_FNPTR_STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

static FNPTR_SEEN: OnceLock<Mutex<std::collections::HashSet<usize>>> = OnceLock::new();

fn fnptr_seen() -> &'static Mutex<std::collections::HashSet<usize>> {
    FNPTR_SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn record_fnptr_trace(dll: &str, symbol: &str, fn_ptr: *mut u8) {
    let key = fn_ptr as usize;
    {
        let mut seen = match fnptr_seen().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !seen.insert(key) {
            return;
        }
    }
    // Sprint 41f — AOT-emitted stub entries leave dll_name_ptr /
    // symbol_name_ptr NULL (see nod-llvm/aot.rs ~line 568, "Initialiser:
    // zero strings, null fn_ptr"), because the Windows loader resolves
    // imports through the IAT, not through our runtime resolver. In
    // that case `str_from_raw` returns "" and the user sees the
    // module-address mapping but no symbolic name. The module path
    // (from GetModuleHandleEx) is the meaningful field in either mode.
    let dll = if dll.is_empty() { "<aot>" } else { dll };
    let symbol = if symbol.is_empty() { "<aot>" } else { symbol };
    let module = module_path_for_address(fn_ptr as *const u8)
        .unwrap_or_else(|| String::from("<unknown — GetModuleHandleEx failed>"));
    eprintln!(
        "[trampoline] symbol={dll}/{symbol} fn_ptr=0x{:016x} module={module}",
        key
    );
}

/// Resolve a code address back to the path of the DLL/EXE that contains
/// it. Returns `None` if `GetModuleHandleEx` fails (which is a
/// VERY interesting finding by itself — it means the address is not in
/// any loaded module, i.e. the fn_ptr is bogus).
///
/// Only meaningful on Windows; non-Windows builds always return `None`.
#[cfg(windows)]
fn module_path_for_address(addr: *const u8) -> Option<String> {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW,
    };
    let mut hmod: HMODULE = std::ptr::null_mut();
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    // SAFETY: GetModuleHandleExW with FROM_ADDRESS takes the address
    // cast to PCWSTR (a documented overload — the flag tells the API
    // to treat the pointer as a code address, not a string).
    let ok = unsafe { GetModuleHandleExW(flags, addr as *const u16, &mut hmod) };
    if ok == 0 || hmod.is_null() {
        return None;
    }
    let mut buf = [0u16; 260];
    // SAFETY: hmod is a valid module handle from the call above;
    // buf is a writable 260-u16 array.
    let len = unsafe { GetModuleFileNameW(hmod, buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

#[cfg(not(windows))]
fn module_path_for_address(_addr: *const u8) -> Option<String> {
    None
}

// ─── Trampolines: arity 0..=8 ─────────────────────────────────────────────
//
// One trampoline per arity. The codegen emits a DirectCall against
// `nod_winffi_call_N` with the entry pointer as the first arg (baked
// as an `i64` WordBits constant) followed by the user args.
//
// Each trampoline is `extern "C-unwind"` so a panic from the
// prelude can propagate via the Sprint 19 unwinder. The inner
// invocation of the resolved function uses `extern "system"` — the
// Win64 ABI uses `cdecl`-like rules with RCX/RDX/R8/R9 + stack
// slots, which `extern "system"` selects on Windows.

/// 0-arg trampoline: `nod_winffi_call_0(entry) -> u64`.
///
/// # Safety
/// `entry` must be the raw `u64` address of a fully-populated
/// [`ApiStubEntry`] in the static area (i.e. one that has gone
/// through `initialize_stub_table` / `resolve_into_entry`). The
/// entry's recorded signature must match this trampoline's arity.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_0(entry: u64) -> u64 {
    // SAFETY: `entry` is the static-area pointer the codegen baked.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 0);
    // Arity 0 has no string args; the empty Vec<TempBuf> incurs no
    // allocation. Kept here for symmetry with N>0 trampolines.
    let _temps: Vec<TempBuf> = Vec::new();
    // SAFETY: by sema invariant, sig.return_kind matches the actual
    // function's return shape, and arity 0 means no args.
    let raw = unsafe {
        let f: extern "system" fn() -> u64 = std::mem::transmute(fn_ptr);
        f()
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(_temps);
    boxed
}

// Concat-paste meta-variable expressions aren't stable on the workspace
// edition; we expand each arity explicitly below.

/// 1-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_1(entry: u64, a0: u64) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 1);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    // SAFETY: sema-validated; Win64 ABI for one 64-bit arg.
    let raw = unsafe {
        let f: extern "system" fn(u64) -> u64 = std::mem::transmute(fn_ptr);
        f(c0)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 2-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_2(entry: u64, a0: u64, a1: u64) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 2);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    // SAFETY: sema-validated; two-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 3-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_3(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 3);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    // SAFETY: sema-validated; three-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1, c2)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 4-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_4(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 4);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    // SAFETY: sema-validated; four-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 5-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_5(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 5);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    // SAFETY: sema-validated; five-arg Win64 ABI (RCX/RDX/R8/R9 +
    // one stack slot above shadow space).
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64, u64) -> u64 =
            std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 6-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_6(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 6);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    // SAFETY: sema-validated; six-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64, u64, u64) -> u64 =
            std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 7-arg trampoline.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_7(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    a6: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 7);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    // SAFETY: sema-validated; seven-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64, u64, u64, u64) -> u64 =
            std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 8-arg trampoline — the Sprint 28 max.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_8(
    entry: u64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    a6: u64,
    a7: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 8);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    let c7 = unbox_arg(Word::from_raw(a7), sig.arg_kinds[7], &mut temps);
    // SAFETY: sema-validated; eight-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> u64 =
            std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6, c7)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 9-arg trampoline. Sprint 36b: needed for Win32 APIs like
/// `CreateWindowExA`/`CreateWindowExW` (12 args), `CreateProcessW`
/// (10 args). Mechanical extension of the 8-arg pattern.
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_9(
    entry: u64,
    a0: u64, a1: u64, a2: u64, a3: u64,
    a4: u64, a5: u64, a6: u64, a7: u64, a8: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 9);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    let c7 = unbox_arg(Word::from_raw(a7), sig.arg_kinds[7], &mut temps);
    let c8 = unbox_arg(Word::from_raw(a8), sig.arg_kinds[8], &mut temps);
    // SAFETY: sema-validated; nine-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(u64, u64, u64, u64, u64, u64, u64, u64, u64) -> u64 =
            std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6, c7, c8)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 10-arg trampoline. Sprint 36b. See [`nod_winffi_call_0`].
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_10(
    entry: u64,
    a0: u64, a1: u64, a2: u64, a3: u64, a4: u64,
    a5: u64, a6: u64, a7: u64, a8: u64, a9: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 10);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    let c7 = unbox_arg(Word::from_raw(a7), sig.arg_kinds[7], &mut temps);
    let c8 = unbox_arg(Word::from_raw(a8), sig.arg_kinds[8], &mut temps);
    let c9 = unbox_arg(Word::from_raw(a9), sig.arg_kinds[9], &mut temps);
    // SAFETY: sema-validated; ten-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(
            u64, u64, u64, u64, u64, u64, u64, u64, u64, u64,
        ) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6, c7, c8, c9)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 11-arg trampoline. Sprint 36b. See [`nod_winffi_call_0`].
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_11(
    entry: u64,
    a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
    a6: u64, a7: u64, a8: u64, a9: u64, a10: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 11);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    let c7 = unbox_arg(Word::from_raw(a7), sig.arg_kinds[7], &mut temps);
    let c8 = unbox_arg(Word::from_raw(a8), sig.arg_kinds[8], &mut temps);
    let c9 = unbox_arg(Word::from_raw(a9), sig.arg_kinds[9], &mut temps);
    let c10 = unbox_arg(Word::from_raw(a10), sig.arg_kinds[10], &mut temps);
    // SAFETY: sema-validated; eleven-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(
            u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64,
        ) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

/// 12-arg trampoline. Sprint 36b: this is the CreateWindowExW arity —
/// the IDE-shell-blocker. See [`nod_winffi_call_0`].
///
/// # Safety
/// See [`nod_winffi_call_0`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_winffi_call_12(
    entry: u64,
    a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
    a6: u64, a7: u64, a8: u64, a9: u64, a10: u64, a11: u64,
) -> u64 {
    // SAFETY: `entry` is the baked static-area pointer.
    let (fn_ptr, sig) = unsafe { trampoline_prelude(entry as *const ApiStubEntry) };
    debug_assert_eq!(sig.arg_count, 12);
    let mut temps: Vec<TempBuf> = Vec::new();
    let c0 = unbox_arg(Word::from_raw(a0), sig.arg_kinds[0], &mut temps);
    let c1 = unbox_arg(Word::from_raw(a1), sig.arg_kinds[1], &mut temps);
    let c2 = unbox_arg(Word::from_raw(a2), sig.arg_kinds[2], &mut temps);
    let c3 = unbox_arg(Word::from_raw(a3), sig.arg_kinds[3], &mut temps);
    let c4 = unbox_arg(Word::from_raw(a4), sig.arg_kinds[4], &mut temps);
    let c5 = unbox_arg(Word::from_raw(a5), sig.arg_kinds[5], &mut temps);
    let c6 = unbox_arg(Word::from_raw(a6), sig.arg_kinds[6], &mut temps);
    let c7 = unbox_arg(Word::from_raw(a7), sig.arg_kinds[7], &mut temps);
    let c8 = unbox_arg(Word::from_raw(a8), sig.arg_kinds[8], &mut temps);
    let c9 = unbox_arg(Word::from_raw(a9), sig.arg_kinds[9], &mut temps);
    let c10 = unbox_arg(Word::from_raw(a10), sig.arg_kinds[10], &mut temps);
    let c11 = unbox_arg(Word::from_raw(a11), sig.arg_kinds[11], &mut temps);
    // SAFETY: sema-validated; twelve-arg Win64 ABI.
    let raw = unsafe {
        let f: extern "system" fn(
            u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64,
        ) -> u64 = std::mem::transmute(fn_ptr);
        f(c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11)
    };
    let boxed = box_return(raw, sig.return_kind).raw();
    drop(temps);
    boxed
}

// ─── Sema helper: signature from c-type names ─────────────────────────────

/// Build an [`ApiCallSignature`] from a list of param c-type names
/// (e.g. `["<c-dword>", "<c-dword>"]`) and a return c-type name
/// (e.g. `"<c-bool>"`). Returns `Err(name)` if any name isn't in the
/// Sprint 28 supported set.
pub fn signature_from_names(
    arg_names: &[&str],
    return_name: Option<&str>,
) -> Result<ApiCallSignature, String> {
    if arg_names.len() > 12 {
        return Err(format!(
            "winffi: arity {} exceeds Sprint 36b cap of 12",
            arg_names.len()
        ));
    }
    let mut arg_kinds = [CArgKind::Void as u8; 12];
    for (i, n) in arg_names.iter().enumerate() {
        let k = CArgKind::from_c_type_name(n).ok_or_else(|| n.to_string())?;
        arg_kinds[i] = k as u8;
    }
    let return_kind = match return_name {
        None => CReturnKind::Void as u8,
        Some(n) => CReturnKind::from_c_type_name(n).ok_or_else(|| n.to_string())? as u8,
    };
    Ok(ApiCallSignature {
        arg_count: arg_names.len() as u8,
        arg_kinds,
        return_kind,
    })
}

// Silence unused-import warnings for non-Windows builds where the
// stat counter `AtomicU64` import isn't needed in practice.
const _: fn() = || {
    let _ = std::marker::PhantomData::<AtomicU64>;
};

// Suppress unused-helper warnings; `condition_class_name` is part of
// the `<c-ffi-error>` diagnostics chain referenced by tests through
// the public `condition_class_name` API in `conditions.rs`.
const _: fn(Word) -> Option<String> = condition_class_name;
