//! Sprint 34 — C-struct heap classes for IDE-essential Win32 shapes.
//!
//! Each registered struct class has:
//!   * a fixed byte size matching the C struct's sizeof
//!   * `is_byte_payload: true` so GC scans the payload as opaque
//!   * a per-field layout table for code-emitted accessors
//!
//! Sprint 34 covers POINT, RECT, SIZE, FILETIME, SYSTEMTIME, MSG —
//! the keystone shapes for the IDE message loop. Sprint 34+ extends
//! to `define c-struct` Dylan-side declarations and to struct-by-value
//! marshaling. Sprint 36 adds WNDCLASSEXW (80 bytes) and PAINTSTRUCT
//! (72 bytes) — the last two structs the IDE shell needs.
//!
//! ## Auto-coerce in Win32 marshaling
//!
//! When a `<c-function>` parameter is declared `<c-pointer>` (or
//! `<c-handle>`) AND the actual Dylan arg is a pointer-tagged
//! `<c-struct>` subclass instance, [`is_c_struct_instance`] returns
//! `true` and [`winffi::unbox_arg`] passes the address of the struct's
//! byte payload (the bytes after the 8-byte `Wrapper`). The
//! `<c-struct>` parent class is registered at process boot via
//! [`ensure_structs_registered`]; concrete struct classes carry it in
//! their CPL.
//!
//! ## Field accessor primitives
//!
//! `nod_struct_get_*` / `nod_struct_set_*` read and write a typed
//! field at a given byte offset from the struct's payload start. The
//! offsets are baked into stdlib accessors by hand (e.g. `point-x`
//! lowers to `%struct-get-i32(p, 0)`).

use std::sync::OnceLock;

use crate::classes::{ClassId, ClassMetadata, class_metadata_for};
use crate::word::Word;
use crate::wrapper::Wrapper;

// ─── Field layout ──────────────────────────────────────────────────────────

/// Width + signedness of a single C struct field. Drives the
/// codegen-side choice of `%struct-get-*` / `%struct-set-*` primitive.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Some kinds (I16, NestedStruct) aren't exercised by the
                    // Sprint 34 seed structs but are part of the layout
                    // vocabulary for Sprint 35+ extensions.
pub enum StructFieldKind {
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Pointer,
    /// A field whose value is itself a struct embedded inline. Sprint 34
    /// records the embedded struct's byte size but doesn't expose nested
    /// accessors — the stdlib accesses nested fields by computing the
    /// flat offset by hand (e.g. `msg-pt-x(m) == %struct-get-i32(m,
    /// MSG_PT_OFFSET + 0)`).
    NestedStruct { byte_size: usize },
}

/// Per-field record used by the Sprint 34 struct layout table. Held in
/// a static map keyed by `ClassId`; the stdlib accessor generation reads
/// `offset` and `kind` to choose the primitive call to emit.
#[derive(Copy, Clone, Debug)]
pub struct StructFieldInfo {
    pub name: &'static str,
    pub offset: usize,
    pub kind: StructFieldKind,
}

// ─── Layout registry ───────────────────────────────────────────────────────

struct StructLayout {
    class_id: ClassId,
    byte_size: usize,
    fields: &'static [StructFieldInfo],
}

struct StructClasses {
    c_struct: ClassId,
    point: ClassId,
    rect: ClassId,
    size: ClassId,
    filetime: ClassId,
    systemtime: ClassId,
    msg: ClassId,
    // Sprint 36 — IDE-shell struct types.
    wndclassexw: ClassId,
    paintstruct: ClassId,
    layouts: Vec<StructLayout>,
}

static STRUCT_CLASSES: OnceLock<StructClasses> = OnceLock::new();

// ── Sprint 34 seed-struct field tables ──────────────────────────────────────
//
// Offsets match the Win64 `MSDN`-documented layouts. Confirmed against
// `windows-sys` struct sizes in the build (POINT = 8, RECT = 16,
// SIZE = 8, FILETIME = 8, SYSTEMTIME = 16, MSG = 48).

const POINT_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "point-x", offset: 0, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "point-y", offset: 4, kind: StructFieldKind::I32 },
];

const RECT_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "rect-left",   offset: 0,  kind: StructFieldKind::I32 },
    StructFieldInfo { name: "rect-top",    offset: 4,  kind: StructFieldKind::I32 },
    StructFieldInfo { name: "rect-right",  offset: 8,  kind: StructFieldKind::I32 },
    StructFieldInfo { name: "rect-bottom", offset: 12, kind: StructFieldKind::I32 },
];

const SIZE_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "size-cx", offset: 0, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "size-cy", offset: 4, kind: StructFieldKind::I32 },
];

const FILETIME_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "filetime-dwLowDateTime",  offset: 0, kind: StructFieldKind::U32 },
    StructFieldInfo { name: "filetime-dwHighDateTime", offset: 4, kind: StructFieldKind::U32 },
];

const SYSTEMTIME_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "systemtime-wYear",         offset: 0,  kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wMonth",        offset: 2,  kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wDayOfWeek",    offset: 4,  kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wDay",          offset: 6,  kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wHour",         offset: 8,  kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wMinute",       offset: 10, kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wSecond",       offset: 12, kind: StructFieldKind::U16 },
    StructFieldInfo { name: "systemtime-wMilliseconds", offset: 14, kind: StructFieldKind::U16 },
];

// MSG layout on Win64 (`tagMSG`):
//   HWND   hwnd     :  8 bytes @ 0
//   UINT   message  :  4 bytes @ 8
//   <pad>           :  4 bytes @ 12  (alignment for WPARAM)
//   WPARAM wParam   :  8 bytes @ 16
//   LPARAM lParam   :  8 bytes @ 24
//   DWORD  time     :  4 bytes @ 32
//   <pad>           :  4 bytes @ 36  (alignment for POINT block? actually POINT is 4-byte aligned;
//                                     pt sits at offset 36 followed by 4 bytes of padding —
//                                     `tagMSG` is actually `time, POINT pt, DWORD lPrivate` packed)
//   POINT  pt       :  8 bytes @ 36
//   DWORD  lPrivate :  4 bytes @ 44
//   total            : 48 bytes
//
// Confirmed: `windows-sys::Win32::UI::WindowsAndMessaging::MSG` is 48
// bytes with `lPrivate` at offset 44. We expose `pt.x` and `pt.y` as
// flat-offset accessors (`msg-pt-x`, `msg-pt-y`) — Sprint 35+ adds
// nested-field syntax `msg.pt.x`.
const MSG_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "msg-hwnd",     offset: 0,  kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "msg-message",  offset: 8,  kind: StructFieldKind::U32 },
    StructFieldInfo { name: "msg-wParam",   offset: 16, kind: StructFieldKind::U64 },
    StructFieldInfo { name: "msg-lParam",   offset: 24, kind: StructFieldKind::I64 },
    StructFieldInfo { name: "msg-time",     offset: 32, kind: StructFieldKind::U32 },
    StructFieldInfo { name: "msg-pt-x",     offset: 36, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "msg-pt-y",     offset: 40, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "msg-lPrivate", offset: 44, kind: StructFieldKind::U32 },
];

// Sprint 36 — WNDCLASSEXW layout on Win64 (80 bytes). Used by
// `RegisterClassExW`. We mostly construct one of these in the Rust
// `nod_register_window_class` helper rather than on the Dylan side,
// because the `lpszClassName` field needs a wide-string buffer with
// process-lifetime; doing that purely in Dylan source would require a
// pinning helper we don't have yet. The struct itself is still
// registered so user code that wants to roll its own WNDCLASSEXW can.
//
// Layout (Win64):
//   UINT      cbSize         :  4 bytes @  0
//   UINT      style          :  4 bytes @  4
//   WNDPROC   lpfnWndProc    :  8 bytes @  8  (pointer; alignment-driven offset)
//   int       cbClsExtra     :  4 bytes @ 16
//   int       cbWndExtra     :  4 bytes @ 20
//   HINSTANCE hInstance      :  8 bytes @ 24
//   HICON     hIcon          :  8 bytes @ 32
//   HCURSOR   hCursor        :  8 bytes @ 40
//   HBRUSH    hbrBackground  :  8 bytes @ 48
//   LPCWSTR   lpszMenuName   :  8 bytes @ 56
//   LPCWSTR   lpszClassName  :  8 bytes @ 64
//   HICON     hIconSm        :  8 bytes @ 72
//   total                    : 80 bytes
const WNDCLASSEXW_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "wndclassexw-cbSize",        offset: 0,  kind: StructFieldKind::U32 },
    StructFieldInfo { name: "wndclassexw-style",         offset: 4,  kind: StructFieldKind::U32 },
    StructFieldInfo { name: "wndclassexw-lpfnWndProc",   offset: 8,  kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-cbClsExtra",    offset: 16, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "wndclassexw-cbWndExtra",    offset: 20, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "wndclassexw-hInstance",     offset: 24, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-hIcon",         offset: 32, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-hCursor",       offset: 40, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-hbrBackground", offset: 48, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-lpszMenuName",  offset: 56, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-lpszClassName", offset: 64, kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "wndclassexw-hIconSm",       offset: 72, kind: StructFieldKind::Pointer },
];

// Sprint 36 — PAINTSTRUCT layout on Win64 (72 bytes). Used by
// `BeginPaint` / `EndPaint`. The OS writes the struct on
// `BeginPaint`; the IDE shell only really cares about `hdc` (offset 0)
// and reads `rcPaint` (offset 16, an inline RECT) when it wants to
// know the dirty rectangle. The reserved tail (`rgbReserved`) is the
// OS's scratch area and the Dylan side never touches it.
//
// Layout (Win64):
//   HDC   hdc          :  8 bytes @  0
//   BOOL  fErase       :  4 bytes @  8
//   <pad>              :  4 bytes @ 12  (alignment for nested RECT)
//   RECT  rcPaint      : 16 bytes @ 16  (left @ 16, top @ 20, right @ 24, bottom @ 28)
//   BOOL  fRestore     :  4 bytes @ 32
//   BOOL  fIncUpdate   :  4 bytes @ 36
//   BYTE  rgbReserved  : 32 bytes @ 40
//   total              : 72 bytes
//
// We expose flat-offset accessors for the rcPaint sub-fields, matching
// the MSG-pt approach (Sprint 34).
const PAINTSTRUCT_FIELDS: &[StructFieldInfo] = &[
    StructFieldInfo { name: "paintstruct-hdc",          offset: 0,  kind: StructFieldKind::Pointer },
    StructFieldInfo { name: "paintstruct-fErase",       offset: 8,  kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-rc-left",      offset: 16, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-rc-top",       offset: 20, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-rc-right",     offset: 24, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-rc-bottom",    offset: 28, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-fRestore",     offset: 32, kind: StructFieldKind::I32 },
    StructFieldInfo { name: "paintstruct-fIncUpdate",   offset: 36, kind: StructFieldKind::I32 },
];

// ─── Registration ──────────────────────────────────────────────────────────

/// Idempotently register the Sprint 34 seed structs at process boot.
/// Mirrors the `conditions.rs` / `tables.rs` / `c_types.rs` pattern.
///
/// The first call:
///   1. Allocates a fresh `ClassMetadata` for `<c-struct>` (parent =
///      `<object>`, no slots).
///   2. For each concrete struct (`<point>`, `<rect>`, …), allocates a
///      metadata with `instance_size = 8 (wrapper) + byte_size` and
///      `is_byte_payload = true`, then records the field layout in the
///      layout table.
///
/// Subsequent calls observe the `OnceLock` initialised and return
/// immediately.
pub fn ensure_structs_registered() {
    let _ = STRUCT_CLASSES.get_or_init(|| {
        // Parent class for every struct. `<c-struct>` is itself a Dylan
        // class users can name in source (`make(<c-struct>)` is rejected
        // by sema because it has no slots and would be useless, but the
        // name resolves so `is-instance?(p, <c-struct>)` works).
        let (c_struct, _) =
            crate::register_simple_user_class("<c-struct>", None, Vec::new());

        let (point, _) = register_struct("<point>", c_struct, 8);
        let (rect, _) = register_struct("<rect>", c_struct, 16);
        let (size, _) = register_struct("<size>", c_struct, 8);
        let (filetime, _) = register_struct("<filetime>", c_struct, 8);
        let (systemtime, _) = register_struct("<systemtime>", c_struct, 16);
        let (msg, _) = register_struct("<msg>", c_struct, 48);
        // Sprint 36 — IDE-shell struct types. WNDCLASSEXW = 80 bytes,
        // PAINTSTRUCT = 72 bytes (both Win64).
        let (wndclassexw, _) = register_struct("<wndclassexw>", c_struct, 80);
        let (paintstruct, _) = register_struct("<paintstruct>", c_struct, 72);

        let layouts = vec![
            StructLayout { class_id: point,      byte_size: 8,  fields: POINT_FIELDS },
            StructLayout { class_id: rect,       byte_size: 16, fields: RECT_FIELDS },
            StructLayout { class_id: size,      byte_size: 8,  fields: SIZE_FIELDS },
            StructLayout { class_id: filetime,   byte_size: 8,  fields: FILETIME_FIELDS },
            StructLayout { class_id: systemtime, byte_size: 16, fields: SYSTEMTIME_FIELDS },
            StructLayout { class_id: msg,        byte_size: 48, fields: MSG_FIELDS },
            StructLayout { class_id: wndclassexw, byte_size: 80, fields: WNDCLASSEXW_FIELDS },
            StructLayout { class_id: paintstruct, byte_size: 72, fields: PAINTSTRUCT_FIELDS },
        ];

        StructClasses {
            c_struct,
            point,
            rect,
            size,
            filetime,
            systemtime,
            msg,
            wndclassexw,
            paintstruct,
            layouts,
        }
    });
}

/// Allocate a concrete struct class metadata: parent = `<c-struct>`,
/// `instance_size = 8 (wrapper) + byte_size`, `is_byte_payload = true`.
///
/// We bypass `register_simple_user_class` because that helper hard-codes
/// `instance_size` from a slot count and forces `is_byte_payload = false`.
/// Instead we walk the same general path as `register_user_class_metadata`
/// but with our own `instance_size` and `is_byte_payload`.
fn register_struct(name: &str, parent: ClassId, byte_size: usize) -> (ClassId, *const ClassMetadata) {
    use std::sync::atomic::AtomicBool;
    use std::sync::RwLock;

    let id = crate::classes::allocate_user_class_id_named(name);
    // CPL: [self, parent, ...parent's cpl tail]
    let parent_md = class_metadata_for(parent);
    let mut cpl = vec![id];
    cpl.extend(parent_md.cpl.iter().copied());

    let instance_size = std::mem::size_of::<Wrapper>() + byte_size;

    let md = ClassMetadata {
        id,
        name: name.to_string(),
        parent: Some(parent),
        parents: vec![parent],
        cpl,
        slots: Vec::new(),
        own_slot_count: 0,
        inherited_slot_count: 0,
        slot_origin: Vec::new(),
        instance_size,
        // Byte-payload scan is a no-op — see `noop_scan` semantics. The
        // user-class scan/size/layout fns also work because they read
        // `metadata.instance_size` directly, but the byte-payload flag
        // tells `DylanLayout` to report an opaque payload to the GC.
        scan: crate::user_class_scan_fn(),
        size_of: crate::user_class_size_fn(),
        layout: crate::user_class_layout_fn(),
        is_byte_payload: true,
        sealed: AtomicBool::new(false),
        direct_subclasses: RwLock::new(Vec::new()),
    };

    let static_ref: &'static ClassMetadata =
        crate::with_literal_pool(|pool| pool.static_area.alloc(md));
    // SAFETY: static_ref lives in the static area (process-lifetime).
    let _ = unsafe { crate::register_user_class(static_ref) };
    (id, static_ref as *const ClassMetadata)
}

// ─── Public accessors ──────────────────────────────────────────────────────

/// `<c-struct>` ClassId — the parent of every concrete struct class.
/// Used by [`is_c_struct_instance`] for the marshaling auto-coerce.
pub fn c_struct_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").c_struct
}

pub fn point_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").point
}

pub fn rect_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").rect
}

pub fn size_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").size
}

pub fn filetime_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").filetime
}

pub fn systemtime_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").systemtime
}

pub fn msg_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES.get().expect("struct classes registered").msg
}

/// `<wndclassexw>` ClassId — the WNDCLASSEXW Win32 struct (80 bytes
/// on Win64), used by `RegisterClassExW`. Sprint 36.
pub fn wndclassexw_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES
        .get()
        .expect("struct classes registered")
        .wndclassexw
}

/// `<paintstruct>` ClassId — the PAINTSTRUCT Win32 struct (72 bytes
/// on Win64), used by `BeginPaint` / `EndPaint`. Sprint 36.
pub fn paintstruct_class_id() -> ClassId {
    ensure_structs_registered();
    STRUCT_CLASSES
        .get()
        .expect("struct classes registered")
        .paintstruct
}

/// True iff `w` is a pointer-tagged Word whose wrapper class is a
/// subclass of `<c-struct>`. Used by [`winffi::unbox_arg`] to decide
/// whether to pass the wrapper address (default) or the payload address
/// (struct case) to a Win32 API expecting a struct pointer.
///
/// Fast path: if the structs haven't been registered yet (e.g. on a
/// build that never references a struct class), return false without
/// taking the registry lock. The `OnceLock` get is non-locking.
pub fn is_c_struct_instance(w: Word) -> bool {
    let Some(p) = w.as_ptr::<u8>() else {
        return false;
    };
    // SAFETY: pointer-tagged Word; first 8 bytes are a Wrapper.
    let wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.is_forwarded() {
        return false;
    }
    let Some(classes) = STRUCT_CLASSES.get() else {
        return false;
    };
    crate::classes::is_subclass(wrapper.class(), classes.c_struct)
}

/// Look up the field layout for a registered struct class. Returns the
/// `(byte_size, fields)` tuple, or `None` if `id` isn't a registered
/// struct. Used by tests and `dump_classes` diagnostics.
pub fn struct_layout_for(id: ClassId) -> Option<(usize, &'static [StructFieldInfo])> {
    let classes = STRUCT_CLASSES.get()?;
    classes
        .layouts
        .iter()
        .find(|l| l.class_id == id)
        .map(|l| (l.byte_size, l.fields))
}

// ─── Payload pointer helper ───────────────────────────────────────────────

/// Resolve the byte-payload start of a pointer-tagged `<c-struct>` Word.
/// Returns `wrapper_ptr + sizeof(Wrapper)`. Panics if `w` isn't a heap
/// pointer.
///
/// # Safety
/// `w` must be a pointer-tagged Word whose wrapper class is a registered
/// `<c-struct>` subclass with at least `8 + offset + width` bytes of
/// allocation.
#[inline]
unsafe fn payload_ptr_mut(w: Word) -> *mut u8 {
    let p = w.as_ptr::<u8>().expect("struct pointer");
    // SAFETY: payload starts immediately after the 8-byte wrapper.
    unsafe { (p as *mut u8).add(std::mem::size_of::<Wrapper>()) }
}

// ─── Field accessor primitives (JIT externs) ──────────────────────────────
//
// Each primitive comes in a (get, set) pair. Get: extract typed bytes
// at `offset`, sign- or zero-extend, return as a fixnum Word. Set:
// take a fixnum value Word, truncate to the field's width, write at
// `offset`. Setters return the value Word (Dylan setter convention).
//
// `offset` is passed as a raw `u64` (fixnum-untagged) because the
// stdlib accessors are hand-generated and pass plain integer literals;
// see the stdlib.dylan accessor body for the source.

/// Decode the offset arg of a `nod_struct_*` primitive. JIT-emitted
/// code passes a fixnum-tagged Word (`n << 1`). Rust callers (the unit
/// tests below and `winffi.rs` diagnostics) build the Word with
/// `Word::fixnum_unchecked(n).raw()` before calling. Both paths funnel
/// through here.
///
/// Panics if `raw` isn't a fixnum-shaped Word (low bit unset).
#[inline]
fn decode_offset(raw: u64) -> usize {
    let w = Word::from_raw(raw);
    w.as_fixnum().expect("offset must be a fixnum Word") as usize
}

/// `%struct-get-i32(s, offset) -> <integer>`. Sprint 34.
///
/// # Safety
/// `s` must be a pointer-tagged Word at a `<c-struct>` subclass. `offset`
/// is a byte offset in `[0, byte_size - 4]`. Caller (the stdlib accessor)
/// passes a literal that matches the field table.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_i32(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: ditto — offset is in bounds per the field table.
    let field_ptr = unsafe { payload.add(off) } as *const i32;
    // SAFETY: aligned read of i32 (the offset is a multiple of 4 by
    // construction in the field tables above).
    let val = unsafe { field_ptr.read_unaligned() };
    Word::fixnum_unchecked(val as i64).raw()
}

/// `%struct-set-i32(value, s, offset) -> value`. Sprint 34.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_i32(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded by field table.
    let field_ptr = unsafe { payload.add(off) } as *mut i32;
    // SAFETY: unaligned write to avoid alignment fault on packed
    // structs (e.g. WPARAM at MSG offset 16 is 8-byte aligned, but we
    // use unaligned writes uniformly to keep the primitive simple).
    unsafe { field_ptr.write_unaligned(v as i32) };
    value
}

/// `%struct-get-i64(s, offset) -> <integer>`.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_i64(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: ditto.
    let field_ptr = unsafe { payload.add(off) } as *const i64;
    // SAFETY: unaligned read.
    let val = unsafe { field_ptr.read_unaligned() };
    Word::fixnum_unchecked(val).raw()
}

/// `%struct-set-i64(value, s, offset) -> value`.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_i64(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *mut i64;
    // SAFETY: unaligned write.
    unsafe { field_ptr.write_unaligned(v) };
    value
}

/// `%struct-get-u16(s, offset) -> <integer>`. Zero-extends to i64.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_u16(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *const u16;
    // SAFETY: unaligned read of u16.
    let val = unsafe { field_ptr.read_unaligned() };
    Word::fixnum_unchecked(val as i64).raw()
}

/// `%struct-set-u16(value, s, offset) -> value`. Truncates to 16 bits.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_u16(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *mut u16;
    // SAFETY: unaligned write of u16.
    unsafe { field_ptr.write_unaligned(v as u16) };
    value
}

/// `%struct-get-u32(s, offset) -> <integer>`. Zero-extends to i64 so a
/// 0xFFFFFFFF DWORD round-trips as +4294967295 rather than -1.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_u32(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *const u32;
    // SAFETY: unaligned read of u32.
    let val = unsafe { field_ptr.read_unaligned() };
    Word::fixnum_unchecked(val as i64).raw()
}

/// `%struct-set-u32(value, s, offset) -> value`.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_u32(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *mut u32;
    // SAFETY: unaligned write of u32.
    unsafe { field_ptr.write_unaligned(v as u32) };
    value
}

/// `%struct-get-u64(s, offset) -> <integer>`. The result is masked into
/// the fixnum's 62-bit positive range — values with the high bits set
/// (e.g. an HWND > 2^62) are surfaced as truncated fixnums. Dylan-side
/// equality against a small constant still works.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_u64(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *const u64;
    // SAFETY: unaligned read of u64.
    let val = unsafe { field_ptr.read_unaligned() };
    // Mask to the 62-bit fixnum positive range; bits above are
    // surfaced as truncation. See `winffi::box_return` UInt64 for the
    // same convention.
    let masked = (val & ((1u64 << 62) - 1)) as i64;
    Word::fixnum_unchecked(masked).raw()
}

/// `%struct-set-u64(value, s, offset) -> value`.
///
/// # Safety
/// See [`nod_struct_get_i32`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_u64(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *mut u64;
    // SAFETY: unaligned write of u64.
    unsafe { field_ptr.write_unaligned(v as u64) };
    value
}

/// `%struct-get-pointer(s, offset) -> <integer>` — read a raw pointer
/// from a struct field as a fixnum-tagged integer (the Dylan `<c-handle>`
/// / `<c-pointer>` convention).
///
/// # Safety
/// See [`nod_struct_get_i32`]. The field at `offset` must be 8 bytes
/// wide; we read an `usize` (pointer-width).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_get_pointer(s: u64, offset: u64) -> u64 {
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *const usize;
    // SAFETY: unaligned read of pointer-width usize.
    let val = unsafe { field_ptr.read_unaligned() };
    // Match `winffi::box_return` Pointer/Handle: surface the raw value
    // as a fixnum (handles like (HANDLE)-1 sign-extend correctly).
    Word::fixnum_unchecked(val as i64).raw()
}

/// `%struct-set-pointer(value, s, offset) -> value`.
///
/// # Safety
/// See [`nod_struct_get_pointer`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_struct_set_pointer(value: u64, s: u64, offset: u64) -> u64 {
    let v = Word::from_raw(value).as_fixnum().expect("integer arg");
    let w = Word::from_raw(s);
    let off = decode_offset(offset);
    // SAFETY: caller's contract.
    let payload = unsafe { payload_ptr_mut(w) };
    // SAFETY: offset bounded.
    let field_ptr = unsafe { payload.add(off) } as *mut usize;
    // SAFETY: unaligned write of pointer-width usize.
    unsafe { field_ptr.write_unaligned(v as usize) };
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_seed_structs_with_expected_sizes() {
        ensure_structs_registered();
        // instance_size = 8 (wrapper) + byte_size
        assert_eq!(class_metadata_for(point_class_id()).instance_size, 8 + 8);
        assert_eq!(class_metadata_for(rect_class_id()).instance_size, 8 + 16);
        assert_eq!(class_metadata_for(size_class_id()).instance_size, 8 + 8);
        assert_eq!(class_metadata_for(filetime_class_id()).instance_size, 8 + 8);
        assert_eq!(class_metadata_for(systemtime_class_id()).instance_size, 8 + 16);
        assert_eq!(class_metadata_for(msg_class_id()).instance_size, 8 + 48);
    }

    #[test]
    fn all_seed_structs_have_byte_payload_flag() {
        ensure_structs_registered();
        for id in [
            point_class_id(),
            rect_class_id(),
            size_class_id(),
            filetime_class_id(),
            systemtime_class_id(),
            msg_class_id(),
        ] {
            assert!(class_metadata_for(id).is_byte_payload,
                "struct {} must have is_byte_payload=true", class_metadata_for(id).name);
        }
    }

    #[test]
    fn point_is_subclass_of_c_struct() {
        ensure_structs_registered();
        assert!(crate::classes::is_subclass(point_class_id(), c_struct_class_id()));
        assert!(crate::classes::is_subclass(rect_class_id(), c_struct_class_id()));
        assert!(crate::classes::is_subclass(msg_class_id(), c_struct_class_id()));
    }

    #[test]
    fn struct_layout_lookup_returns_fields() {
        ensure_structs_registered();
        let (size, fields) = struct_layout_for(point_class_id()).expect("point layout");
        assert_eq!(size, 8);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "point-x");
        assert_eq!(fields[0].offset, 0);
        assert_eq!(fields[1].name, "point-y");
        assert_eq!(fields[1].offset, 4);
    }

    /// Build a fixnum-tagged Word that callers pass as the `offset` arg
    /// to the struct primitives. Mirrors what the JIT emits for a Dylan
    /// integer literal (`%struct-get-i32(p, 4)` materializes `4` as
    /// `4 << 1 = 8`).
    fn tag(n: i64) -> u64 {
        Word::fixnum_unchecked(n).raw()
    }

    #[test]
    fn rust_make_zeroes_point_fields() {
        ensure_structs_registered();
        let md = class_metadata_for(point_class_id());
        // SAFETY: registered metadata, no init keywords.
        let w = unsafe { crate::rust_make(md, &[]) };
        // Read fields back via the get primitives. Offsets passed as
        // tagged Words — same shape JIT-emitted code uses.
        // SAFETY: w is a freshly-allocated <point>.
        let x = unsafe { nod_struct_get_i32(w.raw(), tag(0)) };
        let y = unsafe { nod_struct_get_i32(w.raw(), tag(4)) };
        assert_eq!(Word::from_raw(x).as_fixnum(), Some(0));
        assert_eq!(Word::from_raw(y).as_fixnum(), Some(0));
    }

    #[test]
    fn struct_field_setter_roundtrip() {
        ensure_structs_registered();
        let md = class_metadata_for(point_class_id());
        // SAFETY: registered metadata.
        let w = unsafe { crate::rust_make(md, &[]) };
        // SAFETY: w is a freshly-allocated <point>.
        unsafe {
            nod_struct_set_i32(tag(42), w.raw(), tag(0));
            nod_struct_set_i32(tag(99), w.raw(), tag(4));
            let x = Word::from_raw(nod_struct_get_i32(w.raw(), tag(0)))
                .as_fixnum()
                .unwrap();
            let y = Word::from_raw(nod_struct_get_i32(w.raw(), tag(4)))
                .as_fixnum()
                .unwrap();
            assert_eq!(x, 42);
            assert_eq!(y, 99);
        }
    }

    #[test]
    fn systemtime_u16_field_roundtrip() {
        ensure_structs_registered();
        let md = class_metadata_for(systemtime_class_id());
        // SAFETY: registered metadata.
        let w = unsafe { crate::rust_make(md, &[]) };
        // SAFETY: w is a freshly-allocated <systemtime>.
        unsafe {
            nod_struct_set_u16(tag(2026), w.raw(), tag(0));
            let y = Word::from_raw(nod_struct_get_u16(w.raw(), tag(0)))
                .as_fixnum()
                .unwrap();
            assert_eq!(y, 2026);
        }
    }

    #[test]
    fn is_c_struct_instance_recognises_point() {
        ensure_structs_registered();
        let md = class_metadata_for(point_class_id());
        // SAFETY: registered metadata.
        let w = unsafe { crate::rust_make(md, &[]) };
        assert!(is_c_struct_instance(w));
        // Fixnum is not a c-struct.
        assert!(!is_c_struct_instance(Word::fixnum_unchecked(42)));
    }

    // ── Sprint 36 — WNDCLASSEXW + PAINTSTRUCT registration ───────────────

    #[test]
    fn wndclassexw_has_win64_size_of_80() {
        ensure_structs_registered();
        // instance_size = 8 (wrapper) + 80 (struct payload).
        assert_eq!(
            class_metadata_for(wndclassexw_class_id()).instance_size,
            8 + 80
        );
    }

    #[test]
    fn paintstruct_has_win64_size_of_72() {
        ensure_structs_registered();
        // instance_size = 8 (wrapper) + 72 (struct payload).
        assert_eq!(
            class_metadata_for(paintstruct_class_id()).instance_size,
            8 + 72
        );
    }

    #[test]
    fn wndclassexw_field_offsets_match_layout() {
        ensure_structs_registered();
        let (size, fields) =
            struct_layout_for(wndclassexw_class_id()).expect("wndclassexw layout");
        assert_eq!(size, 80);
        // Cherry-pick the load-bearing offsets the Sprint 36 brief
        // documents: cbSize @ 0, lpfnWndProc @ 8, hInstance @ 24,
        // lpszClassName @ 64, hIconSm @ 72.
        let by_name = |n: &str| {
            fields
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("missing field {n}"))
                .offset
        };
        assert_eq!(by_name("wndclassexw-cbSize"), 0);
        assert_eq!(by_name("wndclassexw-lpfnWndProc"), 8);
        assert_eq!(by_name("wndclassexw-hInstance"), 24);
        assert_eq!(by_name("wndclassexw-lpszClassName"), 64);
        assert_eq!(by_name("wndclassexw-hIconSm"), 72);
    }

    #[test]
    fn paintstruct_field_offsets_match_layout() {
        ensure_structs_registered();
        let (size, fields) =
            struct_layout_for(paintstruct_class_id()).expect("paintstruct layout");
        assert_eq!(size, 72);
        let by_name = |n: &str| {
            fields
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("missing field {n}"))
                .offset
        };
        // hdc @ 0; rcPaint nested fields @ 16/20/24/28; fRestore @ 32.
        assert_eq!(by_name("paintstruct-hdc"), 0);
        assert_eq!(by_name("paintstruct-rc-left"), 16);
        assert_eq!(by_name("paintstruct-rc-top"), 20);
        assert_eq!(by_name("paintstruct-rc-right"), 24);
        assert_eq!(by_name("paintstruct-rc-bottom"), 28);
        assert_eq!(by_name("paintstruct-fRestore"), 32);
    }

    #[test]
    fn paintstruct_byte_size_matches_windows_crate_on_windows() {
        // Empirical cross-check: on Windows builds the `windows` crate
        // gives us PAINTSTRUCT and WNDCLASSEXW with the exact same
        // layout. We use std::mem::size_of via a feature-gated path so
        // CI on non-Windows still compiles this test (size_of asserts
        // execute as conditional no-ops there).
        #[cfg(windows)]
        {
            use windows::Win32::Graphics::Gdi::PAINTSTRUCT;
            use windows::Win32::UI::WindowsAndMessaging::WNDCLASSEXW;
            assert_eq!(std::mem::size_of::<PAINTSTRUCT>(), 72);
            assert_eq!(std::mem::size_of::<WNDCLASSEXW>(), 80);
        }
    }
}
