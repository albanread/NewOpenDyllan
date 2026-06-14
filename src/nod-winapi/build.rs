//! Build-time pipeline: SQLite ➜ Rust structs ➜ postcard ➜ zstd-19 ➜
//! `$OUT_DIR/winapi_data.bin.zst`.
//!
//! See `src/data_schema.rs` for the wire format.
//!
//! ## Type classification
//!
//! The vendored DB has these `kind` values in the `types` table:
//!
//!   - `primitive` — Rust-style names: `u32`, `i32`, `void`, `bool`, …
//!   - `reference` — opaque named typedef (BOOL, HRESULT, HANDLE,
//!     DWORD, …). The DB does *not* carry a `target_type_id` for
//!     these, so we resolve by NAME against a static table.
//!   - `pointer` — has `pointee_type_id` pointing at another row.
//!   - `enum` — Win32-style enum (almost always backed by `u32`).
//!   - `struct` / `union` / `interface` / `delegate` /
//!     `apis-container` — out of scope for Sprint 27.
//!   - `type` — `Native*Attribute` metadata rows, ignored.
//!
//! Sprint 27 acceptance criterion is "primitive-typed signatures":
//! every parameter + return value must resolve into the
//! [`TypeRef`] enum. References we don't recognise by name are
//! treated as opaque handles when their name starts with `H`, and
//! rejected otherwise.
//!
//! ## Embedded blob budget
//!
//! Sprint 27 budget: < 3 MB. Current projected subset weighs in
//! around ~12 KB after zstd-19 — three orders of magnitude under.

use std::env;
use std::fs;
use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags, params};

include!("src/data_schema.rs");

/// Resolve a `reference`-kind type by its name. The DB doesn't carry
/// the underlying integer for these rows; we hardcode the well-known
/// Windows typedefs.
fn resolve_named_reference(name: &str) -> Option<TypeRef> {
    // Boolean-typed typedefs.
    if matches!(name, "BOOL" | "BOOLEAN") {
        return Some(TypeRef::Bool32);
    }
    // Narrow / wide string typedefs (passed as pointers to char data).
    if matches!(
        name,
        "PSTR" | "LPSTR" | "PCSTR" | "LPCSTR" | "PCHAR" | "LPCH" | "LPCCH"
    ) {
        return Some(TypeRef::NarrowString);
    }
    if matches!(
        name,
        "PWSTR" | "LPWSTR" | "PCWSTR" | "LPCWSTR" | "PWCHAR" | "PCNZWCH"
    ) {
        return Some(TypeRef::WideString);
    }
    // Common integer typedefs.
    match name {
        "DWORD" | "ULONG" | "UINT" | "UINT32" | "ULONG32" | "COLORREF" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U32) }),
        "LONG" | "INT" | "INT32" | "LONG32" | "HRESULT" | "NTSTATUS" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::I32) }),
        "WORD" | "USHORT" | "UINT16" | "WCHAR" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U16) }),
        "SHORT" | "INT16" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::I16) }),
        "BYTE" | "UCHAR" | "UINT8" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U8) }),
        "CHAR" | "INT8" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::I8) }),
        "DWORDLONG" | "ULONGLONG" | "UINT64" | "ULONG64" | "DWORD64" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U64) }),
        "LONGLONG" | "INT64" | "LONG64" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::I64) }),
        // Pointer-sized integers — i64/u64 on Win64.
        "SIZE_T" | "ULONG_PTR" | "DWORD_PTR" | "UINT_PTR" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U64) }),
        "SSIZE_T" | "LONG_PTR" | "INT_PTR" | "LRESULT" | "LPARAM" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::I64) }),
        "WPARAM" => Some(TypeRef::Alias { name: name.into(), base: Box::new(TypeRef::U64) }),
        _ => None,
    }
}

/// Map a `primitive`-kind row's `type_name` to a `TypeRef`.
fn resolve_primitive(name: &str) -> Option<TypeRef> {
    Some(match name {
        "void" => TypeRef::Void,
        "bool" => TypeRef::Bool32, // C `bool` is u8 in reality but Win32 rarely uses it; we keep Bool32 as the catch-all
        "u8" | "char" => TypeRef::U8,
        "i8" => TypeRef::I8,
        "u16" => TypeRef::U16,
        "i16" => TypeRef::I16,
        "u32" => TypeRef::U32,
        "i32" => TypeRef::I32,
        "u64" => TypeRef::U64,
        "i64" => TypeRef::I64,
        "usize" | "isize" => TypeRef::U64, // Win64 — pointer-sized
        _ => return None,
    })
}

/// Map a SQLite `types` row to a `TypeRef`. Returns `None` when the
/// type is not primitive-typed (struct-by-value, union, fn-pointer,
/// COM, …) — the enclosing function is then dropped from the
/// projected subset.
fn classify_type(conn: &Connection, type_id: i64, depth: u32) -> Option<TypeRef> {
    if depth > 8 {
        return None;
    }

    let row: (String, String, Option<i64>, Option<i64>, Option<i64>) = {
        let mut stmt = conn
            .prepare_cached(
                "SELECT kind, type_name, pointee_type_id, target_type_id, element_type_id \
                 FROM types WHERE type_id = ?1",
            )
            .ok()?;
        stmt.query_row(params![type_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .ok()?
    };
    let (kind, name, pointee, target, _element) = row;

    match kind.as_str() {
        "primitive" => resolve_primitive(&name),
        "reference" => {
            if let Some(t) = resolve_named_reference(&name) {
                return Some(t);
            }
            // Heuristic: handle types start with `H` (HWND, HICON, …).
            // Anything else opaque-named is rejected.
            if name.starts_with('H') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Some(TypeRef::Handle);
            }
            // Other named references (e.g. PSID, NTSTATUS,
            // SECURITY_ATTRIBUTES) — handled above where known;
            // otherwise reject.
            None
        }
        "pointer" => {
            // Sprint 27 carries the pointee one level deep. A pointer
            // to a primitive/handle/string surfaces as `Pointer { Some(…) }`;
            // a pointer to an opaque struct/union/interface surfaces
            // as `Pointer { None }`. The pointer parameter is
            // ACCEPTED in both cases.
            let pointee_id = pointee?;
            let pointee_ref =
                classify_pointee(conn, pointee_id, depth + 1).map(Box::new);
            Some(TypeRef::Pointer { pointee_type_ref: pointee_ref })
        }
        "enum" => {
            let base = target
                .and_then(|t| classify_type(conn, t, depth + 1))
                .unwrap_or(TypeRef::U32);
            Some(TypeRef::Enum { base: Box::new(base) })
        }
        // Sprint 40d — callable types (WNDPROC, WNDENUMPROC,
        // HOOKPROC, DLGPROC, TIMERPROC, ENUMRESLANGPROCW, …) and the
        // sibling `delegate` family project as an opaque
        // `<c-pointer>`. The Dylan side passes a value it got from
        // `as-wndproc-callback` / `as-wndenumproc-callback` / etc.,
        // or a `$NULL` literal. The Sprint 28/30 marshaling treats
        // these identically to any other opaque pointer, so the
        // enclosing function (EnumChildWindows, EnumThreadWindows,
        // SetWindowsHookExW, …) is now ACCEPTED into the projected
        // subset.
        "function_pointer" | "delegate" => {
            Some(TypeRef::Pointer { pointee_type_ref: None })
        }
        // Sprint 40d — the vendored DB inconsistently stores a few
        // pointer-sized integer typedefs (`LPARAM`, `WPARAM`,
        // `LRESULT`, `INT_PTR`, `UINT_PTR`, …) and handle typedefs
        // (`HINSTANCE`, `HMODULE`, …) as `kind = "struct"` rather
        // than `kind = "reference"`. They are NOT real structs — at
        // the C level they're typedef'd integers / handles. When the
        // name matches a known typedef table entry, accept it; this
        // unblocks `EnumWindows` (LPARAM param), `CreateWindowExW`
        // (HINSTANCE param), and friends, none of which were
        // reachable in Sprint 27. The `H`-prefixed opaque-handle
        // fallback (mirroring the `reference` arm) catches the rest.
        "struct" | "union" => {
            if let Some(t) = resolve_named_reference(&name) {
                return Some(t);
            }
            if name.starts_with('H')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Some(TypeRef::Handle);
            }
            None
        }
        // Anything else — interface, apis-container,
        // type (= Native*Attribute) — is explicitly out of scope.
        _ => None,
    }
}

/// Classify in pointee position: a pointer-to-struct is acceptable
/// (we collapse to opaque pointer via `None`); a pointer-to-primitive
/// surfaces the primitive.
fn classify_pointee(conn: &Connection, type_id: i64, depth: u32) -> Option<TypeRef> {
    if depth > 8 {
        return None;
    }
    let kind: String = {
        let mut stmt = conn
            .prepare_cached("SELECT kind FROM types WHERE type_id = ?1")
            .ok()?;
        stmt.query_row(params![type_id], |r| r.get(0)).ok()?
    };
    match kind.as_str() {
        "struct" | "union" | "interface" | "delegate" | "apis-container" => {
            // Opaque-pointer collapse — the caller wraps as
            // `Pointer { None }`. The enclosing function is still
            // accepted.
            None
        }
        _ => classify_type(conn, type_id, depth),
    }
}

fn classify_param_dir(s: Option<&str>) -> Direction {
    match s.map(|x| x.to_ascii_lowercase()) {
        Some(ref v) if v == "in" => Direction::In,
        Some(ref v) if v == "out" => Direction::Out,
        Some(ref v) if v == "inout" => Direction::InOut,
        _ => Direction::Unknown,
    }
}

fn project_functions(conn: &Connection) -> rusqlite::Result<Vec<FunctionInfo>> {
    let mut fn_stmt = conn.prepare(
        "SELECT function_id, function_name, dll_name, callconv, return_type_id, \
                is_variadic, aw_family, set_last_error \
         FROM functions \
         WHERE dll_name IS NOT NULL \
         ORDER BY dll_name, function_name",
    )?;
    let mut param_stmt = conn.prepare_cached(
        "SELECT ordinal, param_name, type_id, direction, is_optional \
         FROM function_params WHERE function_id = ?1 ORDER BY ordinal",
    )?;

    let rows = fn_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    let mut out = Vec::new();
    let mut skipped_variadic = 0usize;
    let mut skipped_no_dll = 0usize;
    let mut skipped_no_return = 0usize;
    let mut skipped_bad_type = 0usize;
    for row in rows {
        let (fid, name, dll, callconv, ret_id, is_variadic, aw_family, sle) = row?;
        if is_variadic != 0 {
            skipped_variadic += 1;
            continue;
        }
        let Some(dll) = dll else {
            skipped_no_dll += 1;
            continue;
        };
        let Some(ret_id) = ret_id else {
            skipped_no_return += 1;
            continue;
        };
        let Some(ret_ty) = classify_type(conn, ret_id, 0) else {
            skipped_bad_type += 1;
            continue;
        };

        let mut params: Vec<ParamInfo> = Vec::new();
        let mut bad = false;
        let mut p_iter = param_stmt.query(params![fid])?;
        while let Some(prow) = p_iter.next()? {
            let _ord: i64 = prow.get(0)?;
            let pname: Option<String> = prow.get(1)?;
            let ptype_id: Option<i64> = prow.get(2)?;
            let pdir: Option<String> = prow.get(3)?;
            let popt: i64 = prow.get(4)?;
            let Some(ptype_id) = ptype_id else {
                bad = true;
                break;
            };
            let Some(ptype_ref) = classify_type(conn, ptype_id, 0) else {
                bad = true;
                break;
            };
            params.push(ParamInfo {
                name: pname,
                type_ref: ptype_ref,
                direction: classify_param_dir(pdir.as_deref()),
                is_optional: popt != 0,
            });
        }
        drop(p_iter);
        if bad {
            skipped_bad_type += 1;
            continue;
        }

        let aw = aw_family
            .as_deref()
            .and_then(|s| s.bytes().next())
            .filter(|b| *b == b'A' || *b == b'W');

        out.push(FunctionInfo {
            name,
            // Lower-case the DLL name at projection time so all
            // lookups are case-stable. The DB has inconsistent
            // casing (`KERNEL32.dll` vs `kernel32.dll`).
            dll: dll.to_ascii_lowercase(),
            callconv: callconv.unwrap_or_else(|| "stdcall".into()),
            return_type: ret_ty,
            params,
            aw_family: aw,
            set_last_error: sle != 0,
        });
    }

    println!(
        "cargo:warning=nod-winapi: projected {} functions; skipped variadic={} no_dll={} no_return={} bad_type={}",
        out.len(),
        skipped_variadic,
        skipped_no_dll,
        skipped_no_return,
        skipped_bad_type
    );

    Ok(out)
}

/// Sprint 29 — load constants from the hand-curated `data/win32_constants.txt`.
///
/// The vendored `windows_api.db` (schema v5) carries enum *type*
/// declarations (MESSAGEBOX_STYLE, WIN32_ERROR, …) but NOT the
/// integer values of their members; that data isn't projected from
/// the upstream WinMD into this DB. Sprint 29 therefore curates the
/// constants Dylan FFI code actually needs by hand. We DO still
/// take the `_conn` parameter so a future sprint that lands a
/// constants table upstream can extend this function to merge
/// DB-extracted rows with the curated set.
///
/// File format (purely line-based — no TOML dep needed):
///
///   # line comment
///   # category: <name>     section header (drops a comment in the
///                          generated Dylan file)
///   NAME = <value>         integer constant (decimal, 0x… hex,
///                          optionally negative)
///   NAME = <value>  ; <dll>  optional source-DLL annotation
///
/// Trailing whitespace and blank lines ignored.
fn project_constants(
    _conn: &Connection,
    workspace_root: &std::path::Path,
) -> rusqlite::Result<Vec<ConstantInfo>> {
    let path = workspace_root.join("data").join("win32_constants.txt");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "Sprint 29 invariant: data/win32_constants.txt must be readable at {} ({e})",
            path.display()
        )
    });
    let mut out: Vec<ConstantInfo> = Vec::new();
    let mut dup_seen = std::collections::HashSet::new();
    for (lineno, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Split off optional `; <dll>` trailer.
        let (lhs, trailer) = match line.find(';') {
            Some(i) => (line[..i].trim_end(), Some(line[i + 1..].trim().to_string())),
            None => (line, None),
        };
        let Some(eq) = lhs.find('=') else {
            panic!(
                "Sprint 29: bad line {} in {} — missing `=`: {raw_line:?}",
                lineno + 1,
                path.display()
            );
        };
        let name = lhs[..eq].trim().to_string();
        let value_str = lhs[eq + 1..].trim();
        let value = parse_constant_value(value_str).unwrap_or_else(|| {
            panic!(
                "Sprint 29: bad value on line {} in {} — {value_str:?} (expected dec / 0x… / negative)",
                lineno + 1,
                path.display()
            )
        });
        if !dup_seen.insert(name.clone()) {
            // Multiple entries with the same NAME are allowed in the
            // curated file (e.g., MB_ICONERROR == MB_ICONSTOP == 0x10
            // — three Win32 spellings for the same flag value), but
            // they MUST agree on the value. The first occurrence
            // wins in the index lookup; we already pushed it.
            let prior = out
                .iter()
                .find(|c| c.name == name)
                .expect("dup_seen membership implies a prior push");
            assert_eq!(
                prior.value,
                value,
                "Sprint 29: line {} in {} repeats {name} with value {value} but it was previously defined as {}",
                lineno + 1,
                path.display(),
                prior.value
            );
            continue;
        }
        out.push(ConstantInfo {
            name,
            value,
            source_dll: trailer,
        });
    }
    Ok(out)
}

/// Parse an integer literal in the curated constants file. Accepts
/// optional `-` sign, optional `0x` / `0X` prefix for hex, decimal
/// otherwise. Values 0..=2^32-1 (e.g. `0xFFFFFFFF` = WAIT_FAILED) are
/// accepted as unsigned and sign-extended into i64 — the curated file
/// expresses them as Win32 headers do (positive hex).
fn parse_constant_value(s: &str) -> Option<i64> {
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r.trim())
    } else {
        (false, s)
    };
    let (radix, body) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, r)
    } else {
        (10, rest)
    };
    // Parse as u64 first to admit 0xFFFFFFFF without overflowing i64.
    let unsigned = u64::from_str_radix(body, radix).ok()?;
    if neg {
        // For negative literals, the input is the magnitude.
        let magnitude_i64 = i64::try_from(unsigned).ok()?;
        Some(-magnitude_i64)
    } else {
        // Reinterpret as i64. u64::MAX → -1 in i64 is acceptable; the
        // sema layer marshals via i64.
        Some(unsigned as i64)
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("nod-winapi crate must live two levels under the workspace root");
    let db_path = workspace_root.join("data").join("windows_api.db");

    let constants_path = workspace_root.join("data").join("win32_constants.txt");

    println!("cargo:rerun-if-changed={}", db_path.display());
    println!("cargo:rerun-if-changed={}", constants_path.display());
    println!("cargo:rerun-if-changed=src/data_schema.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|e| panic!("opening vendored windows_api.db at {}: {e}", db_path.display()));

    let functions = project_functions(&conn).expect("project functions");
    let constants = project_constants(&conn, workspace_root).expect("project constants");
    let mut dll_names: Vec<String> = functions.iter().map(|f| f.dll.clone()).collect();
    dll_names.sort();
    dll_names.dedup();

    let idx = WinApiIndex { functions, constants, dll_names };
    let bytes = postcard::to_allocvec(&idx).expect("postcard serialise");
    let compressed = zstd::stream::encode_all(&*bytes, 19).expect("zstd encode");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("winapi_data.bin.zst");
    fs::write(&out_path, &compressed).expect("write blob");

    println!(
        "cargo:warning=nod-winapi: postcard {} bytes; zstd-19 {} bytes ({}.{}% of postcard); {} functions, {} constants, {} dlls",
        bytes.len(),
        compressed.len(),
        compressed.len() * 100 / bytes.len().max(1),
        (compressed.len() * 1000 / bytes.len().max(1)) % 10,
        idx.functions.len(),
        idx.constants.len(),
        idx.dll_names.len(),
    );

    println!("cargo:rustc-env=WINAPI_DATA_BIN={}", out_path.display());
}
