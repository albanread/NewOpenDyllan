//! `<bit-vector>` runtime — collection-classes lever (Part A2).
//!
//! A `<bit-vector>` is a concrete `<vector>` subclass registered as a
//! pure-Dylan `define class` in `stdlib/arrays.dylan`. Its two slots are:
//!
//!   - `%bit-vector-words`    — a `<simple-object-vector>` of fixnum words.
//!     Bit `i` lives in word `i / WORD_BITS` at bit position
//!     `i % WORD_BITS`. The SOV is a normal heap object the GC scans, so
//!     the packed words stay live; storing them as fixnums (low bit 0)
//!     means the GC's `classify` treats every word slot as an immediate
//!     and never mis-follows a packed value as a pointer.
//!   - `%bit-vector-bit-size` — the logical bit count (a fixnum).
//!
//! Word size is **60 bits**, not 63: a fixnum payload is 63-bit *signed*
//! (`-2^62 ..= 2^62 - 1`), so capping each word at 60 data bits keeps
//! every packed word a comfortably-positive fixnum (`< 2^60 < FIXNUM_MAX`)
//! and sidesteps any sign-bit subtlety when round-tripping through
//! `Word::from_fixnum` / `as_fixnum`.
//!
//! `make(<bit-vector>, size:, fill:)` is intercepted in the compiler's
//! `lower_make` and redirected to [`nod_bit_vector_allocate`] (mirrors the
//! `<table>` → `nod_make_table` arm); a Dylan `define method make` would
//! be dead code because `lower_make` intercepts at the call site.
//!
//! The word-level bitwise primitives (`nod_logand` / `nod_logior` /
//! `nod_logxor` / `nod_lognot` / `nod_ash`) live here too — they are the
//! integer building blocks the (future) Phase-B multi-value bit-vector
//! ops will compose, and are independently useful as `logand` &c.

use crate::classes::{ClassId, ClassMetadata, class_metadata_for, find_class_id_by_name};
use crate::make::{RootGuard, rust_make};
use crate::word::Word;

/// Data bits packed per backing word. See module docs for why 60, not 63.
pub const WORD_BITS: usize = 60;

/// Mask of the low `WORD_BITS` bits.
const WORD_MASK: i64 = (1_i64 << WORD_BITS) - 1;

/// Number of backing words needed to hold `nbits` bits.
fn words_for_bits(nbits: usize) -> usize {
    nbits.div_ceil(WORD_BITS)
}

/// Resolve the `<bit-vector>` class metadata registered by the stdlib
/// `define class`. Panics if the stdlib hasn't been loaded / AOT-replayed
/// yet — by the time any `make(<bit-vector>)` runs, both the JIT stdlib
/// load and the AOT class replay (`nod_aot_resolve_relocs`) have already
/// registered it, exactly as `<table>` is.
fn bit_vector_metadata() -> &'static ClassMetadata {
    let id = find_class_id_by_name("<bit-vector>")
        .expect("<bit-vector> class not registered — stdlib not loaded");
    class_metadata_for(id)
}

/// Allocate a fresh `<bit-vector>` of `size` bits, every bit initialised
/// to `fill` (0 or non-zero → 1). Returns the pointer-tagged Word.
pub fn make_bit_vector(size: usize, fill: i64) -> Word {
    let nwords = words_for_bits(size);
    // Allocate the backing words SOV (zero-filled by the allocator).
    let storage = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(nwords.max(1), &pool.classes)
    });
    // Root the storage across the next allocation (rust_make can GC).
    let storage_local = storage;
    let _guard = RootGuard::new(&storage_local);

    // If fill is set, populate every full word with all-ones, and the
    // final partial word with only its in-range bits.
    if fill != 0 && size > 0 {
        // SAFETY: storage is the freshly-allocated backing SOV.
        let sov = unsafe {
            crate::vectors::try_simple_object_vector_mut(
                storage_local,
                ClassId::SIMPLE_OBJECT_VECTOR,
            )
            .expect("bit-vector storage is a SOV")
        };
        // SAFETY: same.
        let slots = unsafe { sov.slots_mut() };
        for (wi, slot) in slots.iter_mut().enumerate().take(nwords) {
            let bits_in_word = if (wi + 1) * WORD_BITS <= size {
                WORD_BITS
            } else {
                size - wi * WORD_BITS
            };
            let val = if bits_in_word >= WORD_BITS {
                WORD_MASK
            } else {
                (1_i64 << bits_in_word) - 1
            };
            *slot = Word::from_fixnum(val).expect("packed word fits fixnum");
        }
    }

    let size_w = Word::from_fixnum(size as i64).expect("bit-vector size fits fixnum");
    let md = bit_vector_metadata();
    // SAFETY: registered metadata; init-keywords match the registered
    // slot init-keywords (`words:` / `bit-size:`); values are valid Words.
    unsafe {
        rust_make(
            md,
            &[("words", storage_local), ("bit-size", size_w)],
        )
    }
}

/// Read the (words_sov, bit_size) fields of a `<bit-vector>`. Returns
/// `None` if `bv` is not a `<bit-vector>` instance.
fn bit_vector_fields(bv: Word) -> Option<(Word, usize)> {
    let id = find_class_id_by_name("<bit-vector>")?;
    let p = bv.as_ptr::<u8>()?;
    // SAFETY: pointer-tagged Word; first 8 bytes are the Wrapper.
    let wrapper = unsafe { *(p as *const crate::wrapper::Wrapper) };
    if wrapper.class() != id {
        return None;
    }
    let md = class_metadata_for(id);
    let words_off = md.slot_offset("%bit-vector-words")?;
    let size_off = md.slot_offset("%bit-vector-bit-size")?;
    // SAFETY: offsets came from this instance's own metadata.
    let words = unsafe { *((p as usize + words_off) as *const Word) };
    let size = unsafe { *((p as usize + size_off) as *const Word) }
        .as_fixnum()
        .unwrap_or(0)
        .max(0) as usize;
    Some((words, size))
}

/// Get bit `i` of `bv` (0 or 1). Out-of-range indices read as 0.
pub fn bit_vector_ref(bv: Word, i: usize) -> i64 {
    let Some((words, size)) = bit_vector_fields(bv) else {
        return 0;
    };
    if i >= size {
        return 0;
    }
    // SAFETY: words is the backing SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector(words, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("bit-vector words is a SOV")
    };
    let wi = i / WORD_BITS;
    let bi = i % WORD_BITS;
    // SAFETY: wi < words_for_bits(size) <= sov.len.
    let word = unsafe { sov.slots() }[wi].as_fixnum().unwrap_or(0);
    (word >> bi) & 1
}

/// Set bit `i` of `bv` to `value` (0 clears, non-zero sets). Out-of-range
/// indices are ignored. Returns `bv`.
pub fn bit_vector_set(bv: Word, i: usize, value: i64) -> Word {
    let Some((words, size)) = bit_vector_fields(bv) else {
        return bv;
    };
    if i >= size {
        return bv;
    }
    // SAFETY: words is the backing SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector_mut(words, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("bit-vector words is a SOV")
    };
    let wi = i / WORD_BITS;
    let bi = i % WORD_BITS;
    // SAFETY: same bounds as ref.
    let slots = unsafe { sov.slots_mut() };
    let cur = slots[wi].as_fixnum().unwrap_or(0);
    let new = if value != 0 {
        cur | (1_i64 << bi)
    } else {
        cur & !(1_i64 << bi)
    };
    // The packed word is a fixnum (immediate) — no write barrier needed
    // (write_barrier only matters for old→young pointer stores). A plain
    // store is correct and the GC over-scan treats it as an immediate.
    slots[wi] = Word::from_fixnum(new & WORD_MASK).expect("packed word fits fixnum");
    bv
}

/// Logical bit count of `bv`.
pub fn bit_vector_size(bv: Word) -> usize {
    bit_vector_fields(bv).map(|(_, s)| s).unwrap_or(0)
}

/// Population count — number of 1 bits across the whole vector.
pub fn bit_vector_count(bv: Word) -> i64 {
    let Some((words, size)) = bit_vector_fields(bv) else {
        return 0;
    };
    if size == 0 {
        return 0;
    }
    // SAFETY: words is the backing SOV.
    let sov = unsafe {
        crate::vectors::try_simple_object_vector(words, ClassId::SIMPLE_OBJECT_VECTOR)
            .expect("bit-vector words is a SOV")
    };
    let nwords = words_for_bits(size);
    // SAFETY: nwords <= sov.len.
    let slots = unsafe { sov.slots() };
    let mut total: i64 = 0;
    for slot in slots.iter().take(nwords) {
        total += (slot.as_fixnum().unwrap_or(0) & WORD_MASK).count_ones() as i64;
    }
    total
}

// ─── JIT/AOT-callable shims ────────────────────────────────────────────────

/// `make(<bit-vector>, size:, fill:)` redirect target. `size_raw` is a
/// fixnum-tagged bit count; `fill_raw` is any Dylan Word (`#f`/0 → clear,
/// anything else → set).
///
/// # Safety
///
/// Both args are tagged Words; non-fixnum size collapses to 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_bit_vector_allocate(size_raw: u64, fill_raw: u64) -> u64 {
    let size = Word::from_raw(size_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    let fill_w = Word::from_raw(fill_raw);
    // Truthy fill: anything that isn't `#f` (Dylan truthiness). A fixnum
    // 0 also means "clear" so the common `fill: 0` reads as clear.
    let imm = crate::literal_pool_immediates();
    let fill = if fill_w == imm.false_ {
        0
    } else if let Some(n) = fill_w.as_fixnum() {
        if n == 0 { 0 } else { 1 }
    } else {
        1
    };
    make_bit_vector(size, fill).raw()
}

/// `%bit-vector-ref(bv, i)` → fixnum 0/1.
///
/// # Safety
///
/// `bv_raw` is any Dylan Word; non-bit-vectors return 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_bit_vector_ref(bv_raw: u64, i_raw: u64) -> u64 {
    let bv = Word::from_raw(bv_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(-1);
    if i < 0 {
        return Word::from_fixnum(0).expect("0 fits").raw();
    }
    Word::from_fixnum(bit_vector_ref(bv, i as usize))
        .expect("bit is 0/1")
        .raw()
}

/// `%bit-vector-set(bv, i, value)` → `bv`.
///
/// # Safety
///
/// `bv_raw` is any Dylan Word; non-bit-vectors are returned unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_bit_vector_set(bv_raw: u64, i_raw: u64, value_raw: u64) -> u64 {
    let bv = Word::from_raw(bv_raw);
    let i = Word::from_raw(i_raw).as_fixnum().unwrap_or(-1);
    if i < 0 {
        return bv_raw;
    }
    let value = Word::from_raw(value_raw).as_fixnum().unwrap_or(0);
    bit_vector_set(bv, i as usize, value).raw()
}

/// `%bit-vector-size(bv)` → fixnum.
///
/// # Safety
///
/// `bv_raw` is any Dylan Word; non-bit-vectors return 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_bit_vector_size(bv_raw: u64) -> u64 {
    let bv = Word::from_raw(bv_raw);
    Word::from_fixnum(bit_vector_size(bv) as i64)
        .expect("size fits")
        .raw()
}

/// `%bit-vector-count(bv)` → population count fixnum.
///
/// # Safety
///
/// `bv_raw` is any Dylan Word; non-bit-vectors return 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_bit_vector_count(bv_raw: u64) -> u64 {
    let bv = Word::from_raw(bv_raw);
    Word::from_fixnum(bit_vector_count(bv)).expect("count fits").raw()
}

// ─── Word-level bitwise integer primitives ─────────────────────────────────
//
// `logand` / `logior` / `logxor` / `lognot` / `ash` over fixnums. These
// operate on the 63-bit signed fixnum payload directly. `lognot`/`ash`
// can overflow the fixnum range for extreme inputs; we saturate the
// result back into the fixnum domain rather than panic (DRM's bignums are
// out of scope).

fn clamp_fixnum(n: i64) -> i64 {
    n.clamp(crate::word::FIXNUM_MIN, crate::word::FIXNUM_MAX)
}

/// `logand(a, b)` — bitwise AND of two fixnums.
///
/// # Safety
///
/// Both args are tagged Words; non-fixnums collapse to 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_logand(a_raw: u64, b_raw: u64) -> u64 {
    let a = Word::from_raw(a_raw).as_fixnum().unwrap_or(0);
    let b = Word::from_raw(b_raw).as_fixnum().unwrap_or(0);
    Word::from_fixnum(clamp_fixnum(a & b)).expect("in range").raw()
}

/// `logior(a, b)` — bitwise inclusive OR.
///
/// # Safety
///
/// As `nod_logand`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_logior(a_raw: u64, b_raw: u64) -> u64 {
    let a = Word::from_raw(a_raw).as_fixnum().unwrap_or(0);
    let b = Word::from_raw(b_raw).as_fixnum().unwrap_or(0);
    Word::from_fixnum(clamp_fixnum(a | b)).expect("in range").raw()
}

/// `logxor(a, b)` — bitwise exclusive OR.
///
/// # Safety
///
/// As `nod_logand`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_logxor(a_raw: u64, b_raw: u64) -> u64 {
    let a = Word::from_raw(a_raw).as_fixnum().unwrap_or(0);
    let b = Word::from_raw(b_raw).as_fixnum().unwrap_or(0);
    Word::from_fixnum(clamp_fixnum(a ^ b)).expect("in range").raw()
}

/// `lognot(a)` — bitwise complement (clamped to the fixnum domain).
///
/// # Safety
///
/// `a_raw` is a tagged Word; non-fixnum collapses to 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_lognot(a_raw: u64) -> u64 {
    let a = Word::from_raw(a_raw).as_fixnum().unwrap_or(0);
    Word::from_fixnum(clamp_fixnum(!a)).expect("in range").raw()
}

/// `ash(n, count)` — arithmetic shift. Positive `count` shifts left,
/// negative shifts right (sign-extending). Result clamped to the fixnum
/// domain; shift magnitudes >= 63 collapse to 0 / sign as appropriate.
///
/// # Safety
///
/// Both args are tagged Words; non-fixnums collapse to 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_ash(n_raw: u64, count_raw: u64) -> u64 {
    let n = Word::from_raw(n_raw).as_fixnum().unwrap_or(0);
    let count = Word::from_raw(count_raw).as_fixnum().unwrap_or(0);
    let result = if count >= 0 {
        if count >= 63 {
            // Everything shifts out except a possible overflow we clamp.
            if n == 0 { 0 } else { clamp_fixnum(if n > 0 { i64::MAX } else { i64::MIN }) }
        } else {
            clamp_fixnum(n.wrapping_shl(count as u32))
        }
    } else {
        let mag = (-count) as u32;
        if mag >= 63 {
            if n < 0 { -1 } else { 0 }
        } else {
            n >> mag
        }
    };
    Word::from_fixnum(clamp_fixnum(result)).expect("in range").raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_stdlib_bit_vector() -> bool {
        // The runtime crate's tests don't load the Dylan stdlib, so
        // `<bit-vector>` may be unregistered. Register a stand-in with the
        // same slot shape so the allocate/ref/set/count machinery can be
        // exercised in isolation.
        if find_class_id_by_name("<bit-vector>").is_some() {
            return true;
        }
        use crate::classes::{SlotDefault, SlotInfo, SlotType};
        let words = SlotInfo {
            name: "%bit-vector-words".to_string(),
            offset: 0,
            type_kind: SlotType::Vector,
            init_keyword: Some("words".to_string()),
            required_init_keyword: false,
            default_init: SlotDefault::Unbound,
            has_setter: true,
        };
        let size = SlotInfo {
            name: "%bit-vector-bit-size".to_string(),
            offset: 0,
            type_kind: SlotType::Integer,
            init_keyword: Some("bit-size".to_string()),
            required_init_keyword: false,
            default_init: SlotDefault::Unbound,
            has_setter: true,
        };
        let (_id, _) =
            crate::register_simple_user_class("<bit-vector>", None, vec![words, size]);
        true
    }

    #[test]
    fn bit_vector_roundtrip() {
        crate::ensure_collections_registered();
        ensure_stdlib_bit_vector();
        let bv = make_bit_vector(10, 0);
        assert_eq!(bit_vector_size(bv), 10);
        assert_eq!(bit_vector_count(bv), 0);
        bit_vector_set(bv, 0, 1);
        bit_vector_set(bv, 3, 1);
        bit_vector_set(bv, 9, 1);
        assert_eq!(bit_vector_ref(bv, 0), 1);
        assert_eq!(bit_vector_ref(bv, 1), 0);
        assert_eq!(bit_vector_ref(bv, 3), 1);
        assert_eq!(bit_vector_ref(bv, 9), 1);
        assert_eq!(bit_vector_count(bv), 3);
        // clear one
        bit_vector_set(bv, 3, 0);
        assert_eq!(bit_vector_ref(bv, 3), 0);
        assert_eq!(bit_vector_count(bv), 2);
        // out-of-range read/write are no-ops
        assert_eq!(bit_vector_ref(bv, 100), 0);
        bit_vector_set(bv, 100, 1);
        assert_eq!(bit_vector_count(bv), 2);
    }

    #[test]
    fn bit_vector_fill_and_multiword() {
        crate::ensure_collections_registered();
        ensure_stdlib_bit_vector();
        // 130 bits spans 3 words (60 + 60 + 10).
        let bv = make_bit_vector(130, 1);
        assert_eq!(bit_vector_size(bv), 130);
        assert_eq!(bit_vector_count(bv), 130);
        for i in [0_usize, 59, 60, 119, 120, 129] {
            assert_eq!(bit_vector_ref(bv, i), 1, "bit {i}");
        }
        assert_eq!(bit_vector_ref(bv, 130), 0);
    }

    #[test]
    fn word_bitwise_ops() {
        // logand/logior/logxor/lognot/ash over fixnum payloads.
        let f = |n: i64| Word::from_fixnum(n).unwrap().raw();
        let r = |w: u64| Word::from_raw(w).as_fixnum().unwrap();
        unsafe {
            assert_eq!(r(nod_logand(f(0b1100), f(0b1010))), 0b1000);
            assert_eq!(r(nod_logior(f(0b1100), f(0b1010))), 0b1110);
            assert_eq!(r(nod_logxor(f(0b1100), f(0b1010))), 0b0110);
            assert_eq!(r(nod_lognot(f(0))), -1);
            assert_eq!(r(nod_ash(f(1), f(4))), 16);
            assert_eq!(r(nod_ash(f(255), f(-4))), 15);
            assert_eq!(r(nod_ash(f(-8), f(-1))), -4);
        }
    }
}
