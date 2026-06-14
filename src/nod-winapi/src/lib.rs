//! **Platform-specific crate — Windows-only.** See
//! `docs/PLATFORMS.md`. The macOS variant will ship `nod-macapi` (or
//! equivalent) providing the same role: a vendored API projection
//! that the compiler reads at JIT-time / AOT-time to resolve bare-
//! name native calls. Likely smaller than this crate's 15,067-function
//! Win32 surface, but structurally analogous.
//!
//! Vendored Windows API metadata for NewOpenDylan FFI (Sprint 27).
//!
//! ## What this crate is
//!
//! A compile-time pipeline embeds the vendored SQLite database at
//! `data/windows_api.db` into the crate as a zstd-compressed postcard
//! blob. At runtime the blob is decompressed + parsed once on first
//! access via [`LazyLock`]; subsequent lookups hit in-memory
//! `HashMaps`.
//!
//! ## What this crate is NOT
//!
//! Sprint 27 is data plumbing only — there are no `dlopen` /
//! `LoadLibrary` calls here, no `GetProcAddress` calls, no actual API
//! invocations. The Dylan-side `define c-function` parser (in
//! `nod-reader`) and the sema-side binding registration (in
//! `nod-sema`) consult this crate to validate `c-function`
//! declarations against the DB. Sprint 28 lands the per-module API
//! stub table + the actual end-to-end call path.
//!
//! ## Wire-format stability
//!
//! None. The blob is rebuilt from source on every `cargo build` and
//! consumers must use the public API surface; the encoded layout is
//! private.

use std::collections::HashMap;
use std::sync::LazyLock;

include!("data_schema.rs");
// `include!` brings `ConstantInfo`, `Direction`, `FunctionInfo`,
// `ParamInfo`, `TypeRef`, and `WinApiIndex` into the crate root
// already — no re-export needed (and a `pub use self::…` would
// collide with the included definitions).

/// The embedded zstd-compressed postcard blob. The path is filled in
/// by `build.rs` via `cargo:rustc-env=WINAPI_DATA_BIN=…`.
static WINAPI_BLOB: &[u8] = include_bytes!(env!("WINAPI_DATA_BIN"));

/// Aggregate stats — mostly for diagnostics and tests.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub function_count: usize,
    pub constant_count: usize,
    pub dll_count: usize,
    pub blob_bytes: usize,
}

struct ResolvedIndex {
    functions: Vec<FunctionInfo>,
    constants: Vec<ConstantInfo>,
    dll_names: Vec<String>,
    /// (lower-cased dll, name) → index into `functions`. We lower-case
    /// the DLL component so callers can write `"KERNEL32.DLL"` and
    /// match the canonical `"kernel32.dll"` from the DB.
    by_dll_and_name: HashMap<(String, String), usize>,
    /// name → all matching indices (across DLLs).
    by_name: HashMap<String, Vec<usize>>,
    /// dll (lower-cased) → indices.
    by_dll: HashMap<String, Vec<usize>>,
    /// constant name → index into `constants`.
    consts_by_name: HashMap<String, usize>,
}

static INDEX: LazyLock<ResolvedIndex> = LazyLock::new(|| {
    let decompressed = zstd::stream::decode_all(WINAPI_BLOB)
        .expect("Sprint 27 invariant: embedded winapi blob is valid zstd");
    let raw: WinApiIndex = postcard::from_bytes(&decompressed)
        .expect("Sprint 27 invariant: embedded winapi blob is valid postcard");

    let WinApiIndex { functions, constants, dll_names } = raw;

    let mut by_dll_and_name: HashMap<(String, String), usize> = HashMap::with_capacity(functions.len());
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_dll: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in functions.iter().enumerate() {
        let dll_key = f.dll.to_ascii_lowercase();
        by_dll_and_name.entry((dll_key.clone(), f.name.clone())).or_insert(i);
        by_name.entry(f.name.clone()).or_default().push(i);
        by_dll.entry(dll_key).or_default().push(i);
    }
    let mut consts_by_name: HashMap<String, usize> = HashMap::with_capacity(constants.len());
    for (i, c) in constants.iter().enumerate() {
        consts_by_name.entry(c.name.clone()).or_insert(i);
    }

    ResolvedIndex {
        functions,
        constants,
        dll_names,
        by_dll_and_name,
        by_name,
        by_dll,
        consts_by_name,
    }
});

/// Look up a function by DLL + name (case-insensitive on the DLL).
/// Returns the first match in DB order — Sprint 27 doesn't try to
/// disambiguate same-DLL same-name collisions (they shouldn't exist
/// in practice).
pub fn find_function(dll: &str, name: &str) -> Option<&'static FunctionInfo> {
    let key = (dll.to_ascii_lowercase(), name.to_string());
    INDEX
        .by_dll_and_name
        .get(&key)
        .map(|&idx| &INDEX.functions[idx])
}

/// Look up by name across all DLLs. Returns an empty slice if not
/// present. The caller picks: usually you want `find_function(dll,
/// name)` after the `library:` clause has nailed down the DLL.
pub fn find_function_any_dll(name: &str) -> &'static [FunctionInfo] {
    static EMPTY: &[FunctionInfo] = &[];
    let Some(indices) = INDEX.by_name.get(name) else {
        return EMPTY;
    };
    // We promised a `&'static [FunctionInfo]` but the indices are
    // separately stored — we materialise a thread-local cache. For
    // Sprint 27 the contract is "any caller that hits the slow path
    // is fine paying once". Simpler: surface only single-DLL matches
    // through this entry-point.
    //
    // Implementation: we expose at most the first match here as a
    // single-element slice via direct array slice into `INDEX.functions`.
    // Multi-DLL collisions return their first match only; tests that
    // care about all-DLL enumeration use `iter_dll` or walk
    // `functions()`.
    if let Some(&first) = indices.first() {
        std::slice::from_ref(&INDEX.functions[first])
    } else {
        EMPTY
    }
}

/// Look up a named integer constant.
pub fn find_constant(name: &str) -> Option<&'static ConstantInfo> {
    INDEX.consts_by_name.get(name).map(|&i| &INDEX.constants[i])
}

/// Iterate functions for a DLL (case-insensitive on the input).
pub fn iter_dll(dll: &str) -> impl Iterator<Item = &'static FunctionInfo> {
    let key = dll.to_ascii_lowercase();
    INDEX
        .by_dll
        .get(&key)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|&i| &INDEX.functions[i])
}

/// All distinct DLL names known to the index.
pub fn dll_names() -> &'static [String] {
    &INDEX.dll_names
}

/// All functions, in DB order. Mostly useful for diagnostics and the
/// blob-size assertion test.
pub fn functions() -> &'static [FunctionInfo] {
    &INDEX.functions
}

/// All constants, in DB order.
pub fn constants() -> &'static [ConstantInfo] {
    &INDEX.constants
}

/// Iterator over every embedded constant. Used by the
/// `win32-constants.dylan` generator (Sprint 29) to walk the
/// curated set.
pub fn iter_constants() -> impl Iterator<Item = &'static ConstantInfo> {
    INDEX.constants.iter()
}

/// Aggregate counts.
pub fn stats() -> Stats {
    Stats {
        function_count: INDEX.functions.len(),
        constant_count: INDEX.constants.len(),
        dll_count: INDEX.dll_names.len(),
        blob_bytes: WINAPI_BLOB.len(),
    }
}

/// Raw embedded blob bytes (compressed). Tests use this to assert
/// the size cap.
pub fn embedded_blob_bytes() -> &'static [u8] {
    WINAPI_BLOB
}

/// Sprint 39b — map a Windows DLL name to its MSVC import-library name.
///
/// The mapping is purely mechanical: lowercase the DLL name and replace
/// the trailing `.dll` with `.lib`. `kernel32.dll` -> `kernel32.lib`,
/// `USER32.DLL` -> `user32.lib`, `Ole32.dll` -> `ole32.lib`.
///
/// Returns `None` if `dll` does not end with `.dll` (case-insensitive)
/// or is otherwise empty. The actual existence of the resulting `.lib`
/// on the linker's search path is **not** checked here — the linker
/// surfaces a clear error if a required import lib isn't present in
/// `%LIB%`, which is the right place for that diagnostic.
///
/// # Examples
///
/// ```
/// assert_eq!(nod_winapi::import_lib_for_dll("kernel32.dll"), Some("kernel32.lib".to_string()));
/// assert_eq!(nod_winapi::import_lib_for_dll("USER32.DLL"), Some("user32.lib".to_string()));
/// assert_eq!(nod_winapi::import_lib_for_dll("not_a_dll"), None);
/// ```
pub fn import_lib_for_dll(dll: &str) -> Option<String> {
    let lower = dll.to_ascii_lowercase();
    let stem = lower.strip_suffix(".dll")?;
    if stem.is_empty() {
        return None;
    }
    Some(format!("{stem}.lib"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_lib_for_dll_basic() {
        assert_eq!(import_lib_for_dll("kernel32.dll"), Some("kernel32.lib".to_string()));
        assert_eq!(import_lib_for_dll("user32.dll"), Some("user32.lib".to_string()));
        assert_eq!(import_lib_for_dll("ole32.dll"), Some("ole32.lib".to_string()));
    }

    #[test]
    fn import_lib_for_dll_case_insensitive() {
        assert_eq!(import_lib_for_dll("KERNEL32.DLL"), Some("kernel32.lib".to_string()));
        assert_eq!(import_lib_for_dll("User32.Dll"), Some("user32.lib".to_string()));
    }

    #[test]
    fn import_lib_for_dll_rejects_non_dll() {
        assert_eq!(import_lib_for_dll("kernel32"), None);
        assert_eq!(import_lib_for_dll(""), None);
        assert_eq!(import_lib_for_dll(".dll"), None);
        assert_eq!(import_lib_for_dll("foo.exe"), None);
    }
}
