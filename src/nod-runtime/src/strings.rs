//! **Stdlib boundary**: new string APIs go in
//! `stdlib/*.dylan`, not here. This file
//! hosts byte-string PRIMITIVES (allocation, byte-get/set, bulk-copy)
//! per `docs/STDLIB_BOUNDARY.md` Rule 2 (tag/layout + GC integration).
//! Higher-level string operations (split, format, search, replace)
//! belong in Dylan composed over these primitives.
//!
//! `<byte-string>` — UTF-8-encoded heap-allocated string.
//!
//! Layout:
//!
//! ```text
//!   [Wrapper 8B] [len: u32] [_pad: u32] [bytes ...] [pad to 8B align]
//! ```
//!
//! The `len` is the byte length, NOT a codepoint count. Bytes are
//! stored inline starting at offset 16. Sprint 10 only writes UTF-8;
//! `<unicode-string>` (UTF-16) lands in Sprint 27.

use crate::classes::{ClassId, ClassTable};
use crate::heap::Heap;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// In-memory layout of a `<byte-string>`. The `bytes` field is the
/// header for the inline byte payload; the actual byte run follows
/// the struct in memory. Always access via the accessor methods.
#[repr(C)]
pub struct ByteString {
    pub wrapper: Wrapper,
    pub len: u32,
    pub _pad: u32,
    // bytes follow inline; size = len, padded to 8-byte alignment.
}

impl ByteString {
    /// Read the inline byte payload.
    ///
    /// # Safety
    ///
    /// `self` must point at a real `<byte-string>` allocation produced
    /// by `Heap::alloc_byte_string` (or a structurally identical pool).
    /// The returned slice borrows from `self`; the caller must not
    /// mutate the underlying memory through any other reference for
    /// the lifetime of the borrow.
    pub unsafe fn bytes(&self) -> &[u8] {
        let base = (self as *const ByteString as *const u8).wrapping_add(size_of::<ByteString>());
        // SAFETY: documented above — caller asserts the layout invariant.
        unsafe { std::slice::from_raw_parts(base, self.len as usize) }
    }

    /// If the inline bytes are valid UTF-8, return them as `&str`.
    ///
    /// # Safety
    ///
    /// Same as `bytes`.
    pub unsafe fn as_str(&self) -> Option<&str> {
        // SAFETY: forwarded to `bytes`.
        let b = unsafe { self.bytes() };
        std::str::from_utf8(b).ok()
    }
}

impl Heap {
    /// Allocate a `<byte-string>` and copy `s.as_bytes()` into the
    /// inline payload. Returns a pointer-tagged `Word`. The class on
    /// the wrapper is `classes.byte_string()`.
    pub fn alloc_byte_string(&self, s: &str, classes: &ClassTable) -> Word {
        let bytes = s.as_bytes();
        // Payload = 4 (len) + 4 (pad) + bytes.len; the heap rounds up
        // to 8-byte alignment.
        let payload_bytes = 8 + bytes.len();
        let w = self.alloc_object(classes.byte_string(), payload_bytes);
        // SAFETY: `alloc_object` returned a freshly initialised wrapper
        // plus a zeroed payload. We now overwrite the first 8 bytes of
        // payload (len, pad) and copy `bytes` into the inline body.
        unsafe {
            let p = w.as_mut_ptr::<u8>().expect("alloc_byte_string returned pointer-tagged Word");
            let bs = p as *mut ByteString;
            (*bs).len = bytes.len() as u32;
            (*bs)._pad = 0;
            if !bytes.is_empty() {
                let dst = p.add(size_of::<ByteString>());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            }
        }
        w
    }

    /// Sprint 42a — allocate a fresh `<byte-string>` of `n` zero-filled
    /// bytes. Used by `nod_byte_string_allocate` (the `%byte-string-allocate`
    /// primitive) as the building block for Dylan-side `concatenate`,
    /// `copy-sequence`, `subsequence`, `as-uppercase`, etc.
    ///
    /// `alloc_object` already zero-fills the payload, so the inline byte
    /// run is `\0` * n at return.
    pub fn alloc_byte_string_uninit(&self, n: usize, classes: &ClassTable) -> Word {
        // Payload = 4 (len) + 4 (pad) + n bytes; heap rounds up to 8-B align.
        let payload_bytes = 8 + n;
        let w = self.alloc_object(classes.byte_string(), payload_bytes);
        // SAFETY: `alloc_object` returned a freshly initialised wrapper
        // plus a zeroed payload. Set the `len` header field; the byte
        // run is already \0.
        unsafe {
            let p = w
                .as_mut_ptr::<u8>()
                .expect("alloc_byte_string_uninit returned pointer-tagged Word");
            let bs = p as *mut ByteString;
            (*bs).len = n as u32;
            (*bs)._pad = 0;
        }
        w
    }
}

// ─── Sprint 42a — JIT-callable byte-string primitive shims ────────────────
//
// These are the five minimum-surface primitives that unlock Dylan-side
// `size`, `element`, `concatenate`, `copy-sequence`, `subsequence`,
// `starts-with?`, `ends-with?`, `find-substring`, `as-uppercase`,
// `as-lowercase`, and `empty?`. Every higher-level method lives in
// `stdlib.dylan` and threads through these. The architectural rule is
// "Rust owns heap layout + GC traversal + this minimum primitive
// surface; everything else is Dylan".
//
// Bounds checking signals `<out-of-range-error>` through the standard
// Dylan condition path (`make_out_of_range_error` + `nod_signal`) so
// user code can `block/exception` around bad indices instead of
// crashing the process.
//
// All five take and return `u64` (raw Word bits) so the JIT's
// `DirectCall` against the `nod_*` symbol composes without trampolines.

fn raise_byte_string_out_of_range(s: Word, len: i64, i: i64, op: &'static str) -> ! {
    let msg = format!(
        "<byte-string> index {i} out of range for size {len} (op: {op})"
    );
    let cond = crate::collections::make_out_of_range_error(s, len, &msg);
    // SAFETY: cond is a freshly-allocated condition Word; nod_signal
    // diverges (either via NLX or unhandled-condition panic).
    unsafe {
        crate::conditions::nod_signal(cond.raw());
    }
    // nod_signal diverges; this line is unreachable.
    unreachable!("nod_signal returned");
}

/// Return the live `&ByteString` view of a pointer-tagged Word, or
/// panic if the class doesn't match.
///
/// # Safety
/// `w` must be a pointer-tagged `<byte-string>` Word.
unsafe fn bs_ref(w: Word, op: &'static str) -> &'static ByteString {
    // SAFETY: forwarded to the caller.
    match unsafe { try_byte_string(w, ClassId::BYTE_STRING) } {
        Some(bs) => bs,
        None => panic!(
            "{op}: expected <byte-string> Word; got raw {:#x}",
            w.raw()
        ),
    }
}

/// `%byte-string-allocate(n :: <integer>) => <byte-string>` — allocate a
/// fresh `n`-byte zero-filled byte-string in the moveable heap.
///
/// # Safety
/// `n_raw` must be a fixnum-tagged Word; negative values are treated
/// as 0. JIT-callable extern.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_byte_string_allocate(n_raw: u64) -> u64 {
    let n = Word::from_raw(n_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    crate::with_literal_pool(|pool| {
        pool.heap.alloc_byte_string_uninit(n, &pool.classes).raw()
    })
}

/// `%byte-string-size(s :: <byte-string>) => <integer>` — byte length.
///
/// # Safety
/// `s_raw` must be a pointer-tagged `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_byte_string_size(s_raw: u64) -> u64 {
    let s = Word::from_raw(s_raw);
    // SAFETY: caller's precondition.
    let bs = unsafe { bs_ref(s, "%byte-string-size") };
    Word::from_fixnum(bs.len as i64).expect("byte-string len fits").raw()
}

/// `%byte-string-element(s, i) => <integer>` — read byte at index `i`
/// (0..255 fixnum). Bounds-checked; out-of-range signals
/// `<out-of-range-error>`.
///
/// # Safety
/// `s_raw` is a pointer-tagged `<byte-string>` Word; `i_raw` is fixnum.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_byte_string_element(s_raw: u64, i_raw: u64) -> u64 {
    let s = Word::from_raw(s_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(-1);
    // SAFETY: caller's precondition.
    let bs = unsafe { bs_ref(s, "%byte-string-element") };
    let len = bs.len as i64;
    if i < 0 || i >= len {
        raise_byte_string_out_of_range(s, len, i, "%byte-string-element");
    }
    // SAFETY: `bs` points at the live allocation; bounds checked above.
    let byte = unsafe { bs.bytes() }[i as usize];
    Word::from_fixnum(byte as i64).expect("byte fits fixnum").raw()
}

/// `%byte-string-element-setter(v, s, i) => v` — write `v` (0..255) at
/// index `i`. Bounds-checked; out-of-range signals
/// `<out-of-range-error>`. Out-of-byte-range values are masked to 8 bits
/// (consistent with C-style byte writes — Dylan doesn't have a
/// `<byte>` type yet so we don't reject 256+).
///
/// # Safety
/// `s_raw` is a pointer-tagged `<byte-string>` Word; `v_raw` and
/// `i_raw` are fixnum Words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_byte_string_element_setter(
    v_raw: u64,
    s_raw: u64,
    i_raw: u64,
) -> u64 {
    let s = Word::from_raw(s_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(-1);
    let v = Word::from_raw(v_raw).as_fixnum().unwrap_or(0);
    // SAFETY: caller's precondition.
    let bs = unsafe { bs_ref(s, "%byte-string-element-setter") };
    let len = bs.len as i64;
    if i < 0 || i >= len {
        raise_byte_string_out_of_range(s, len, i, "%byte-string-element-setter");
    }
    // SAFETY: we have `&ByteString`; cast back to a raw byte pointer at
    // the inline payload and write byte `i`. The byte payload is
    // opaque to the GC (`is_byte_payload: true` in the class metadata,
    // `byte_string_layout` reports zero scan range), so no write
    // barrier is needed — the byte run isn't a Word slot.
    unsafe {
        let base = (bs as *const ByteString as *mut u8)
            .add(size_of::<ByteString>());
        *base.add(i as usize) = (v & 0xff) as u8;
    }
    v_raw
}

/// `%byte-string-copy!(dst, dst-off, src, src-off, count) => 0` —
/// memcpy `count` bytes from `src[src-off..src-off+count]` to
/// `dst[dst-off..dst-off+count]`. Both ends bounds-checked; either
/// out-of-range signals `<out-of-range-error>`. `count = 0` is a no-op.
///
/// `src` and `dst` may be the same byte-string; the underlying copy
/// uses `ptr::copy` (overlap-safe), not `ptr::copy_nonoverlapping`.
///
/// # Safety
/// All five args must be Words of the right shape (byte-strings + fixnums).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_byte_string_copy_bytes(
    dst_raw: u64,
    dst_off_raw: u64,
    src_raw: u64,
    src_off_raw: u64,
    count_raw: u64,
) -> u64 {
    let dst = Word::from_raw(dst_raw);
    let src = Word::from_raw(src_raw);
    let dst_off = Word::from_raw(dst_off_raw).as_fixnum().unwrap_or(-1);
    let src_off = Word::from_raw(src_off_raw).as_fixnum().unwrap_or(-1);
    let count = Word::from_raw(count_raw).as_fixnum().unwrap_or(-1);
    if count == 0 {
        return Word::from_fixnum(0).expect("0 fits").raw();
    }
    // SAFETY: caller's precondition.
    let dst_bs = unsafe { bs_ref(dst, "%byte-string-copy!") };
    let src_bs = unsafe { bs_ref(src, "%byte-string-copy!") };
    let dst_len = dst_bs.len as i64;
    let src_len = src_bs.len as i64;
    if count < 0 {
        raise_byte_string_out_of_range(dst, dst_len, count, "%byte-string-copy! (count)");
    }
    if dst_off < 0 || dst_off + count > dst_len {
        raise_byte_string_out_of_range(dst, dst_len, dst_off + count, "%byte-string-copy! (dst)");
    }
    if src_off < 0 || src_off + count > src_len {
        raise_byte_string_out_of_range(src, src_len, src_off + count, "%byte-string-copy! (src)");
    }
    // SAFETY: bounds checked above; payload is opaque bytes, no GC
    // pointers to update. ptr::copy handles overlap (src == dst).
    unsafe {
        let dst_base = (dst_bs as *const ByteString as *mut u8)
            .add(size_of::<ByteString>())
            .add(dst_off as usize);
        let src_base = (src_bs as *const ByteString as *const u8)
            .add(size_of::<ByteString>())
            .add(src_off as usize);
        std::ptr::copy(src_base, dst_base, count as usize);
    }
    Word::from_fixnum(0).expect("0 fits").raw()
}

/// Decode `w` to a `&ByteString` if its wrapper class matches
/// `<byte-string>`. Returns `None` for any other shape.
///
/// # Safety
///
/// `w`, if pointer-tagged, must point at a valid 8-byte-aligned heap
/// object whose first cell is a `Wrapper`. The wrapper class is the
/// gate: only `<byte-string>`-classed objects are dereferenced as
/// `ByteString`. Sprint 11 will add a heap-membership check; today
/// the caller (the tracer, the format-out shim) is responsible.
pub unsafe fn try_byte_string(w: Word, byte_string: ClassId) -> Option<&'static ByteString> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: wrapper-first invariant — every heap object's first 8
    // bytes are a Wrapper.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() == byte_string {
        // SAFETY: class match implies the object's layout is ByteString.
        // The `'static` lifetime here is a lie shrunk by callers — the
        // pointer is only valid for the heap's lifetime, but the heap
        // is process-lived in Sprint 10.
        Some(unsafe { &*(p as *const ByteString) })
    } else {
        None
    }
}

/// `integer-to-string(n :: <integer>) -> <byte-string>`.
///
/// Converts a Dylan fixnum to its decimal string representation.  The
/// result is allocated on the GC heap.
///
/// # Safety
///
/// `n_raw` must be a fixnum-tagged Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_integer_to_string(n_raw: u64) -> u64 {
    let n = Word::from_raw(n_raw).as_fixnum().unwrap_or(0);
    let s = n.to_string();
    crate::with_literal_pool(|pool| pool.heap.alloc_byte_string(&s, &pool.classes)).raw()
}

/// `%char-code(ch :: <character>) -> <integer>` — the integer code
/// point of a character.
///
/// `<character>` lowers to a raw i32 holding its code (see
/// `nod-llvm::codegen` — `TypeEstimate::Character => i32`). The codegen
/// boundary sign-extends that i32 to the i64 Word ABI before the call,
/// so `ch_raw`'s low 32 bits are the code. We mask to 32 bits (a char
/// code is non-negative) and retag as a Dylan fixnum so the result is a
/// first-class `<integer>` that compares/arithmetics against byte codes.
///
/// # Safety
/// `ch_raw` is the sign-extended i32 char code in an i64 register.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn nod_char_code(ch_raw: u64) -> u64 {
    // Low 32 bits are the code; char codes are non-negative so a plain
    // mask drops any sign-extension high bits.
    let code = (ch_raw & 0xFFFF_FFFF) as i64;
    Word::from_fixnum(code).expect("char code fits fixnum").raw()
}

/// `%code-char(code :: <integer>) -> <character>` — the character with
/// the given integer code point.
///
/// `code_raw` is a fixnum-tagged `<integer>` Word. We untag to recover
/// the raw code and return it in the low 32 bits of the result Word;
/// the codegen boundary truncates that to the i32 `<character>` register
/// shape. Out-of-range codes are masked to 32 bits (Dylan has no
/// `<character>` bounds class yet — see DEFERRED). This is the exact
/// inverse of `nod_char_code`.
///
/// # Safety
/// `code_raw` is a fixnum-tagged Dylan Word.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn nod_code_char(code_raw: u64) -> u64 {
    let code = Word::from_raw(code_raw).as_fixnum().unwrap_or(0);
    // Return the raw code in the low 32 bits; codegen trunc's to i32.
    (code as u64) & 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_round_trip_ascii() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_byte_string("hello", &ct);
        assert!(w.is_pointer());
        let wrap = heap.wrapper_of(w).expect("inside heap");
        assert_eq!(wrap.class(), ct.byte_string());
        // SAFETY: `w` came straight back from `alloc_byte_string`.
        let bs = unsafe { try_byte_string(w, ct.byte_string()) }.expect("class matches");
        assert_eq!(bs.len, 5);
        // SAFETY: `bs` points at the live allocation.
        let bytes = unsafe { bs.bytes() };
        assert_eq!(bytes, b"hello");
        // SAFETY: forwarded.
        assert_eq!(unsafe { bs.as_str() }, Some("hello"));
    }

    #[test]
    fn alloc_empty_string() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_byte_string("", &ct);
        // SAFETY: same as above.
        let bs = unsafe { try_byte_string(w, ct.byte_string()) }.expect("class matches");
        assert_eq!(bs.len, 0);
        // SAFETY: same as above.
        assert_eq!(unsafe { bs.bytes() }, b"");
    }

    #[test]
    fn alloc_utf8_string() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let s = "héllo, 世界";
        let w = heap.alloc_byte_string(s, &ct);
        // SAFETY: same as above.
        let bs = unsafe { try_byte_string(w, ct.byte_string()) }.expect("class matches");
        assert_eq!(bs.len as usize, s.len());
        // SAFETY: same as above.
        assert_eq!(unsafe { bs.as_str() }, Some(s));
    }

    // `<character>` ↔ `<integer>` code-point conversion primitives.
    // `nod_char_code` takes a sign-extended i32 char code in an i64
    // register and returns a tagged fixnum; `nod_code_char` is its
    // inverse, returning the raw code in the low 32 bits.

    #[test]
    fn char_code_returns_tagged_fixnum() {
        // 'A' = 65; codegen sign-extends the i32 65 to i64 65.
        let code = nod_char_code(65);
        assert_eq!(Word::from_raw(code).as_fixnum(), Some(65));
        // Even-coded char (bit 0 = 0) and odd-coded char (bit 0 = 1)
        // both round-trip to a proper fixnum, not a stray pointer.
        assert_eq!(Word::from_raw(nod_char_code(66)).as_fixnum(), Some(66));
        assert_eq!(Word::from_raw(nod_char_code(53)).as_fixnum(), Some(53)); // '5'
    }

    #[test]
    fn code_char_recovers_raw_code() {
        // Input is a tagged fixnum 66; output is the raw code in low 32.
        let tagged_66 = Word::from_fixnum(66).unwrap().raw();
        assert_eq!(nod_code_char(tagged_66) & 0xFFFF_FFFF, 66);
        let tagged_53 = Word::from_fixnum(53).unwrap().raw();
        assert_eq!(nod_code_char(tagged_53) & 0xFFFF_FFFF, 53);
    }

    #[test]
    fn char_code_round_trip_is_identity() {
        // %char-code(%code-char(code)) == code for the full ASCII range.
        for code in 0..=127i64 {
            let tagged = Word::from_fixnum(code).unwrap().raw();
            // `nod_code_char` yields the raw code (what an i32 char holds);
            // a real call would trunc to i32 then sign-extend back — for
            // 0..127 that's a no-op, so feed it straight to `nod_char_code`.
            let raw_char = nod_code_char(tagged);
            let back = nod_char_code(raw_char);
            assert_eq!(Word::from_raw(back).as_fixnum(), Some(code));
        }
    }
}
