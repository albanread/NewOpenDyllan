//! Sprint 27 — c-type seed classes for the FFI surface.
//!
//! Registers `<c-bool>`, `<c-int>`, `<c-uint>`, `<c-short>`,
//! `<c-ushort>`, `<c-long>`, `<c-ulong>`, `<c-dword>`, `<c-word>`,
//! `<c-byte>`, `<c-pointer>`, `<c-handle>`, `<c-string>`, and
//! `<c-wide-string>` as Dylan classes whose parent is `<object>`.
//!
//! These classes carry no behavior in Sprint 27 — they exist solely
//! so the sema-level type checker can resolve them when validating
//! `define c-function` parameter and return type declarations.
//! Marshaling between Dylan values and the FFI representation is
//! Sprint 28's deliverable.
//!
//! Sprint 27 deliberately does NOT model:
//!
//!   - Variants for callbacks (function-pointer parameters).
//!   - Struct-by-value types (`<c-struct>` family).
//!   - COM-interface types.
//!   - Type-parametric forms like `<c-pointer-to> (<c-int>)`.
//!
//! These will appear in Sprint 28+. The Sprint 27 acceptance shape is
//! "the parser accepts these names and sema doesn't error on them".

use std::sync::OnceLock;

use crate::classes::ClassId;

struct CTypeClasses {
    c_bool: ClassId,
    c_int: ClassId,
    c_uint: ClassId,
    c_short: ClassId,
    c_ushort: ClassId,
    c_long: ClassId,
    c_ulong: ClassId,
    c_longlong: ClassId,
    c_ulonglong: ClassId,
    c_dword: ClassId,
    c_word: ClassId,
    c_byte: ClassId,
    c_pointer: ClassId,
    c_handle: ClassId,
    c_string: ClassId,
    c_wide_string: ClassId,
}

/// Sprint 35: float types are kept in a separate `OnceLock` so the
/// registration story is independent of the c-types seed set above —
/// some Sprint 35 tests want only `<c-float>` / `<c-double>` without
/// also pulling in the integer family.
struct CFloatTypes {
    c_float: ClassId,
    c_double: ClassId,
}

static C_TYPE_CLASSES: OnceLock<CTypeClasses> = OnceLock::new();
static C_FLOAT_TYPES: OnceLock<CFloatTypes> = OnceLock::new();

/// Idempotently register the FFI c-type classes. Safe to call from
/// `nod-sema` lowering before validating a `define c-function`
/// declaration.
pub fn ensure_registered() {
    let _ = C_TYPE_CLASSES.get_or_init(|| {
        let mk = |n: &str| crate::register_simple_user_class(n, None, Vec::new()).0;
        CTypeClasses {
            c_bool: mk("<c-bool>"),
            c_int: mk("<c-int>"),
            c_uint: mk("<c-uint>"),
            c_short: mk("<c-short>"),
            c_ushort: mk("<c-ushort>"),
            c_long: mk("<c-long>"),
            c_ulong: mk("<c-ulong>"),
            c_longlong: mk("<c-longlong>"),
            c_ulonglong: mk("<c-ulonglong>"),
            c_dword: mk("<c-dword>"),
            c_word: mk("<c-word>"),
            c_byte: mk("<c-byte>"),
            c_pointer: mk("<c-pointer>"),
            c_handle: mk("<c-handle>"),
            c_string: mk("<c-string>"),
            c_wide_string: mk("<c-wide-string>"),
        }
    });
}

/// Accessor for the `<c-bool>` ClassId. Used by Sprint 28's FFI
/// codegen path to identify boolean-typed parameters.
pub fn c_bool_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_bool
}

pub fn c_dword_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_dword
}

pub fn c_int_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_int
}

pub fn c_pointer_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_pointer
}

pub fn c_handle_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_handle
}

pub fn c_string_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_string
}

pub fn c_wide_string_class_id() -> ClassId {
    ensure_registered();
    C_TYPE_CLASSES.get().expect("c-type classes registered").c_wide_string
}

/// Sprint 35: idempotently register `<c-float>` (32-bit) and
/// `<c-double>` (64-bit) Dylan classes. These are the marshaling-
/// layer placeholders for float arguments — Sprint 35 shims do not
/// currently take native floats (see `com_shim.rs` module docs for
/// the deviation), but the classes are registered so Dylan sources
/// can name them in `define c-function` declarations against future
/// COM APIs that need fractional precision.
pub fn ensure_float_types_registered() {
    let _ = C_FLOAT_TYPES.get_or_init(|| {
        let mk = |n: &str| crate::register_simple_user_class(n, None, Vec::new()).0;
        CFloatTypes {
            c_float: mk("<c-float>"),
            c_double: mk("<c-double>"),
        }
    });
}

/// `<c-float>` ClassId — Sprint 35.
pub fn c_float_class_id() -> ClassId {
    ensure_float_types_registered();
    C_FLOAT_TYPES.get().expect("c-float types registered").c_float
}

/// `<c-double>` ClassId — Sprint 35.
pub fn c_double_class_id() -> ClassId {
    ensure_float_types_registered();
    C_FLOAT_TYPES.get().expect("c-float types registered").c_double
}

// Stub helpers to keep clippy happy — we'll grow them once Sprint 28
// starts emitting actual marshaling code.
#[allow(dead_code)]
fn _silence_unused(c: &CTypeClasses) -> [ClassId; 16] {
    [
        c.c_bool, c.c_int, c.c_uint, c.c_short, c.c_ushort, c.c_long, c.c_ulong,
        c.c_longlong, c.c_ulonglong,
        c.c_dword, c.c_word, c.c_byte, c.c_pointer, c.c_handle, c.c_string, c.c_wide_string,
    ]
}
