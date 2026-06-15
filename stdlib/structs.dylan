Module: dylan
Author: NewOpenDylan stdlib

// ─── Sprint 34: <c-struct> field accessors ────────────────────────────────
//
// One getter and one setter per field of every seed struct registered in
// `nod-runtime/src/structs.rs`. Each accessor lowers to a `%struct-get-*`
// / `%struct-set-*` primitive call that reads or writes typed bytes at
// the field's offset in the struct's payload buffer.
//
// The accessors are hand-generated for the Sprint 34 seed structs;
// Sprint 35+ adds a `define c-struct` Dylan-side parser surface that
// emits these automatically.
//
// Setter calling convention: `f(obj) := v` lowers to `f-setter(obj, v)`
// for unary getters (Sprint 12 shape — see `nod-sema/src/lower.rs`
// around line 4493). We accept `(obj, v)` and forward to the primitive
// in primitive-arg order `(value, struct, offset)`.

// ── <point> (8 bytes: LONG x @ 0, LONG y @ 4) ─────────────────────────────

define function point-x (p) => (n)
  %struct-get-i32(p, 0)
end function;

define function point-x-setter (p, v) => (v)
  %struct-set-i32(v, p, 0)
end function;

define function point-y (p) => (n)
  %struct-get-i32(p, 4)
end function;

define function point-y-setter (p, v) => (v)
  %struct-set-i32(v, p, 4)
end function;

// ── <rect> (16 bytes: LONG left @ 0, top @ 4, right @ 8, bottom @ 12) ──────

define function rect-left (r) => (n)
  %struct-get-i32(r, 0)
end function;

define function rect-left-setter (r, v) => (v)
  %struct-set-i32(v, r, 0)
end function;

define function rect-top (r) => (n)
  %struct-get-i32(r, 4)
end function;

define function rect-top-setter (r, v) => (v)
  %struct-set-i32(v, r, 4)
end function;

define function rect-right (r) => (n)
  %struct-get-i32(r, 8)
end function;

define function rect-right-setter (r, v) => (v)
  %struct-set-i32(v, r, 8)
end function;

define function rect-bottom (r) => (n)
  %struct-get-i32(r, 12)
end function;

define function rect-bottom-setter (r, v) => (v)
  %struct-set-i32(v, r, 12)
end function;

// ── <size> (8 bytes: LONG cx @ 0, LONG cy @ 4) ────────────────────────────

define function size-cx (s) => (n)
  %struct-get-i32(s, 0)
end function;

define function size-cx-setter (s, v) => (v)
  %struct-set-i32(v, s, 0)
end function;

define function size-cy (s) => (n)
  %struct-get-i32(s, 4)
end function;

define function size-cy-setter (s, v) => (v)
  %struct-set-i32(v, s, 4)
end function;

// ── <filetime> (8 bytes: DWORD dwLowDateTime @ 0, dwHighDateTime @ 4) ─────

define function filetime-low (f) => (n)
  %struct-get-u32(f, 0)
end function;

define function filetime-low-setter (f, v) => (v)
  %struct-set-u32(v, f, 0)
end function;

define function filetime-high (f) => (n)
  %struct-get-u32(f, 4)
end function;

define function filetime-high-setter (f, v) => (v)
  %struct-set-u32(v, f, 4)
end function;

// ── <systemtime> (16 bytes: WORD wYear @ 0, wMonth @ 2, …) ─────────────────

define function systemtime-year (s) => (n)
  %struct-get-u16(s, 0)
end function;

define function systemtime-year-setter (s, v) => (v)
  %struct-set-u16(v, s, 0)
end function;

define function systemtime-month (s) => (n)
  %struct-get-u16(s, 2)
end function;

define function systemtime-month-setter (s, v) => (v)
  %struct-set-u16(v, s, 2)
end function;

define function systemtime-day-of-week (s) => (n)
  %struct-get-u16(s, 4)
end function;

define function systemtime-day-of-week-setter (s, v) => (v)
  %struct-set-u16(v, s, 4)
end function;

define function systemtime-day (s) => (n)
  %struct-get-u16(s, 6)
end function;

define function systemtime-day-setter (s, v) => (v)
  %struct-set-u16(v, s, 6)
end function;

define function systemtime-hour (s) => (n)
  %struct-get-u16(s, 8)
end function;

define function systemtime-hour-setter (s, v) => (v)
  %struct-set-u16(v, s, 8)
end function;

define function systemtime-minute (s) => (n)
  %struct-get-u16(s, 10)
end function;

define function systemtime-minute-setter (s, v) => (v)
  %struct-set-u16(v, s, 10)
end function;

define function systemtime-second (s) => (n)
  %struct-get-u16(s, 12)
end function;

define function systemtime-second-setter (s, v) => (v)
  %struct-set-u16(v, s, 12)
end function;

define function systemtime-milliseconds (s) => (n)
  %struct-get-u16(s, 14)
end function;

define function systemtime-milliseconds-setter (s, v) => (v)
  %struct-set-u16(v, s, 14)
end function;

// ── <msg> (48 bytes — see structs.rs MSG_FIELDS for layout) ──────────────

define function msg-hwnd (m) => (n)
  %struct-get-pointer(m, 0)
end function;

define function msg-hwnd-setter (m, v) => (v)
  %struct-set-pointer(v, m, 0)
end function;

define function msg-message (m) => (n)
  %struct-get-u32(m, 8)
end function;

define function msg-message-setter (m, v) => (v)
  %struct-set-u32(v, m, 8)
end function;

define function msg-wparam (m) => (n)
  %struct-get-u64(m, 16)
end function;

define function msg-wparam-setter (m, v) => (v)
  %struct-set-u64(v, m, 16)
end function;

define function msg-lparam (m) => (n)
  %struct-get-i64(m, 24)
end function;

define function msg-lparam-setter (m, v) => (v)
  %struct-set-i64(v, m, 24)
end function;

define function msg-time (m) => (n)
  %struct-get-u32(m, 32)
end function;

define function msg-time-setter (m, v) => (v)
  %struct-set-u32(v, m, 32)
end function;

// MSG.pt is a nested POINT; Sprint 34 surfaces flat-offset accessors.
// Sprint 35+ adds dotted notation (`msg.pt.x`).
define function msg-pt-x (m) => (n)
  %struct-get-i32(m, 36)
end function;

define function msg-pt-x-setter (m, v) => (v)
  %struct-set-i32(v, m, 36)
end function;

define function msg-pt-y (m) => (n)
  %struct-get-i32(m, 40)
end function;

define function msg-pt-y-setter (m, v) => (v)
  %struct-set-i32(v, m, 40)
end function;

define function msg-lprivate (m) => (n)
  %struct-get-u32(m, 44)
end function;

define function msg-lprivate-setter (m, v) => (v)
  %struct-set-u32(v, m, 44)
end function;

// ─── Sprint 36: <wndclassexw> field accessors ─────────────────────────────
//
// WNDCLASSEXW carries the window-class registration shape that
// `RegisterClassExW` writes. Sprint 36's `nod-register-window-class`
// helper builds this struct in Rust (because the `lpszClassName`
// field needs a process-lifetime wide-string buffer that's awkward
// to express in pure Dylan); these accessors are here for callers
// that build their own WNDCLASSEXW. The size accessor is the most
// commonly used — calling code must store `sizeof(WNDCLASSEXW) = 80`
// in `cbSize` before passing the struct to `RegisterClassExW`.

define function wndclassexw-cbSize (w) => (n)
  %struct-get-u32(w, 0)
end function;

define function wndclassexw-cbSize-setter (w, v) => (v)
  %struct-set-u32(v, w, 0)
end function;

define function wndclassexw-style (w) => (n)
  %struct-get-u32(w, 4)
end function;

define function wndclassexw-style-setter (w, v) => (v)
  %struct-set-u32(v, w, 4)
end function;

define function wndclassexw-lpfnWndProc (w) => (n)
  %struct-get-pointer(w, 8)
end function;

define function wndclassexw-lpfnWndProc-setter (w, v) => (v)
  %struct-set-pointer(v, w, 8)
end function;

define function wndclassexw-hInstance (w) => (n)
  %struct-get-pointer(w, 24)
end function;

define function wndclassexw-hInstance-setter (w, v) => (v)
  %struct-set-pointer(v, w, 24)
end function;

define function wndclassexw-lpszClassName (w) => (n)
  %struct-get-pointer(w, 64)
end function;

define function wndclassexw-lpszClassName-setter (w, v) => (v)
  %struct-set-pointer(v, w, 64)
end function;

// ─── Sprint 36: <paintstruct> field accessors ─────────────────────────────
//
// PAINTSTRUCT is populated by `BeginPaint(hwnd, &ps)`. The IDE-shell
// WNDPROC typically only reads `hdc` and the inline `rcPaint` —
// we expose those plus a couple of the flag fields. The 32-byte
// `rgbReserved` tail is OS scratch; we don't expose it.

define function paintstruct-hdc (p) => (n)
  %struct-get-pointer(p, 0)
end function;

define function paintstruct-hdc-setter (p, v) => (v)
  %struct-set-pointer(v, p, 0)
end function;

define function paintstruct-fErase (p) => (n)
  %struct-get-i32(p, 8)
end function;

define function paintstruct-fErase-setter (p, v) => (v)
  %struct-set-i32(v, p, 8)
end function;

define function paintstruct-rc-left (p) => (n)
  %struct-get-i32(p, 16)
end function;

define function paintstruct-rc-left-setter (p, v) => (v)
  %struct-set-i32(v, p, 16)
end function;

define function paintstruct-rc-top (p) => (n)
  %struct-get-i32(p, 20)
end function;

define function paintstruct-rc-top-setter (p, v) => (v)
  %struct-set-i32(v, p, 20)
end function;

define function paintstruct-rc-right (p) => (n)
  %struct-get-i32(p, 24)
end function;

define function paintstruct-rc-right-setter (p, v) => (v)
  %struct-set-i32(v, p, 24)
end function;

define function paintstruct-rc-bottom (p) => (n)
  %struct-get-i32(p, 28)
end function;

define function paintstruct-rc-bottom-setter (p, v) => (v)
  %struct-set-i32(v, p, 28)
end function;

