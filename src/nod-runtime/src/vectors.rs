//! `<simple-object-vector>` — Dylan's contiguous object-slot vector.
//!
//! Layout:
//!
//! ```text
//!   [Wrapper 8B] [len: u64] [slot[0]: Word] [slot[1]: Word] ...
//! ```
//!
//! Every slot is exactly one `Word` (8 bytes). Slot count is fixed at
//! allocation; `<stretchy-vector>` arrives in Sprint 20.

use crate::classes::{ClassId, ClassTable};
use crate::heap::Heap;
use crate::word::Word;
use crate::wrapper::Wrapper;

#[repr(C)]
pub struct SimpleObjectVector {
    pub wrapper: Wrapper,
    pub len: u64,
    // slots: Word follow inline.
}

impl SimpleObjectVector {
    /// Read the slot run as a slice of `Word`.
    ///
    /// # Safety
    ///
    /// `self` must point at a real `<simple-object-vector>` allocation.
    /// Returned slice borrows the heap memory; the heap must outlive it
    /// and no mutator may resize the vector (Sprint 10 vectors are
    /// fixed-length so this is always sound here).
    pub unsafe fn slots(&self) -> &[Word] {
        let base = (self as *const SimpleObjectVector as *const u8)
            .wrapping_add(size_of::<SimpleObjectVector>()) as *const Word;
        // SAFETY: layout invariant from `Heap::alloc_simple_object_vector`.
        unsafe { std::slice::from_raw_parts(base, self.len as usize) }
    }

    /// Mutable view of the slot run.
    ///
    /// # Safety
    ///
    /// Same as `slots`, plus: caller must guarantee no concurrent
    /// readers (Sprint 10 single-threaded mutator makes this trivial).
    pub unsafe fn slots_mut(&mut self) -> &mut [Word] {
        let base = (self as *mut SimpleObjectVector as *mut u8)
            .wrapping_add(size_of::<SimpleObjectVector>()) as *mut Word;
        // SAFETY: layout invariant.
        unsafe { std::slice::from_raw_parts_mut(base, self.len as usize) }
    }
}

impl Heap {
    /// Allocate a `<simple-object-vector>` of `len` zero-initialised
    /// slots. Caller fills them in via `try_simple_object_vector_mut`
    /// + `slots_mut`. Returns a pointer-tagged Word.
    pub fn alloc_simple_object_vector(&self, len: usize, classes: &ClassTable) -> Word {
        let payload_bytes = 8 + len * size_of::<Word>();
        let w = self.alloc_object(classes.simple_object_vector(), payload_bytes);
        // SAFETY: payload was zeroed by `alloc_object`; we set len. Slots
        // remain Word(0) which is fixnum-tagged zero — a valid Dylan
        // value to leave in the vector until the caller fills it in.
        unsafe {
            let p = w.as_mut_ptr::<u8>().expect("alloc_simple_object_vector returned pointer-tagged Word");
            let v = p as *mut SimpleObjectVector;
            (*v).len = len as u64;
        }
        w
    }
}

/// Read view onto a vector-classed `Word`.
///
/// # Safety
///
/// See `strings::try_byte_string`.
pub unsafe fn try_simple_object_vector(
    w: Word,
    vector_class: ClassId,
) -> Option<&'static SimpleObjectVector> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: wrapper-first invariant.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() == vector_class {
        // SAFETY: class match implies SimpleObjectVector layout.
        Some(unsafe { &*(p as *const SimpleObjectVector) })
    } else {
        None
    }
}

/// Mutable view onto a vector-classed `Word`. Caller is responsible
/// for absence of aliasing — Sprint 10 single-threaded mutator gives
/// us this for free.
///
/// # Safety
///
/// See `try_simple_object_vector`; additionally, no other reference
/// (mutable or shared) may exist for the lifetime of the returned one.
pub unsafe fn try_simple_object_vector_mut(
    w: Word,
    vector_class: ClassId,
) -> Option<&'static mut SimpleObjectVector> {
    let p = w.as_mut_ptr::<u8>()?;
    // SAFETY: wrapper-first invariant.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() == vector_class {
        // SAFETY: class match implies SimpleObjectVector layout.
        Some(unsafe { &mut *(p as *mut SimpleObjectVector) })
    } else {
        None
    }
}

// ─── Sprint 20 JIT-callable shims ──────────────────────────────────────────
//
// `nod_sov_size(v) -> <integer>`, `nod_sov_element(v, idx) -> <object>`,
// `nod_sov_element_setter(v, idx, value) -> <object>`. The setter writes
// through `write_barrier` so the GC's card-mark table stays in sync.

/// JIT-callable `size(v :: <simple-object-vector>) -> <integer>`. Reads
/// the on-heap `len` slot and returns it as a fixnum-tagged Word.
///
/// # Safety
///
/// `v_raw` must be a pointer-tagged `<simple-object-vector>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_sov_size(v_raw: u64) -> u64 {
    let v = Word::from_raw(v_raw);
    // SAFETY: caller asserts class match.
    let sov = unsafe { try_simple_object_vector(v, ClassId::SIMPLE_OBJECT_VECTOR) };
    match sov {
        Some(s) => Word::from_fixnum(s.len as i64)
            .unwrap_or(Word::from_raw(0))
            .raw(),
        None => 0,
    }
}

/// JIT-callable `element(v :: <simple-object-vector>, idx :: <integer>)`.
/// Returns the slot at `idx`. Out-of-bounds: signals an
/// `<out-of-range-error>` via `nod_signal`, which diverges; on the
/// happy path returns the slot's Word.
///
/// # Safety
///
/// `v_raw` must be a pointer-tagged `<simple-object-vector>` Word;
/// `idx_raw` must be a fixnum-tagged Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_sov_element(v_raw: u64, idx_raw: u64) -> u64 {
    let v = Word::from_raw(v_raw);
    let idx = Word::from_raw(idx_raw).as_fixnum().unwrap_or(-1);
    // SAFETY: caller asserts class match.
    let sov = match unsafe { try_simple_object_vector(v, ClassId::SIMPLE_OBJECT_VECTOR) } {
        Some(s) => s,
        None => return 0,
    };
    let len = sov.len as i64;
    if idx < 0 || idx >= len {
        // Signal an <out-of-range-error>.
        let cond =
            crate::collections::make_out_of_range_error(v, len, "vector index out of range");
        // SAFETY: cond is a pointer-tagged condition Word; diverges.
        unsafe {
            crate::nod_signal(cond.raw());
        }
    }
    // SAFETY: bounds checked.
    let slots = unsafe { sov.slots() };
    slots[idx as usize].raw()
}

/// JIT-callable `element-setter(value, v, idx)`. Writes `value` into
/// the slot at `idx` through the GC write barrier. Returns the value
/// (Dylan's setter convention).
///
/// # Safety
///
/// `v_raw` must be a pointer-tagged `<simple-object-vector>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_sov_element_setter(
    value_raw: u64,
    v_raw: u64,
    idx_raw: u64,
) -> u64 {
    let v = Word::from_raw(v_raw);
    let idx = Word::from_raw(idx_raw).as_fixnum().unwrap_or(-1);
    let value = Word::from_raw(value_raw);
    // SAFETY: caller asserts class match.
    let sov = match unsafe { try_simple_object_vector_mut(v, ClassId::SIMPLE_OBJECT_VECTOR) } {
        Some(s) => s,
        None => return value_raw,
    };
    let len = sov.len as i64;
    if idx < 0 || idx >= len {
        let cond =
            crate::collections::make_out_of_range_error(v, len, "vector index out of range");
        // SAFETY: cond is a pointer-tagged condition Word; diverges.
        unsafe {
            crate::nod_signal(cond.raw());
        }
    }
    // SAFETY: bounds checked.
    let slots = unsafe { sov.slots_mut() };
    let slot_ptr = &mut slots[idx as usize] as *mut Word;
    // SAFETY: slot_ptr is inside the live SOV allocation.
    unsafe { crate::write_barrier(slot_ptr, value) };
    value_raw
}

/// JIT-callable `vector(elem0, elem1, ..., elemN, count)` literal
/// builder. The fixed-arity Sprint 20 shape: up to 8 elements packed
/// into positional args, plus a leading `count` Word. Sprint 23 lifts
/// the limit via `c-ffi` for vector literals of any length.
///
/// The DFM lowering for `#(1, 2, 3)` (Sprint 20+) emits a call to this
/// shim. Earlier sprints didn't lower the literal at all — the parser
/// kept `#list(...)` as an unbound `Call(Ident("#list"))` for the
/// pretty-printer. See `nod-sema::lower::lower_list_builtin` for the
/// list-literal path; this is the vector counterpart.
///
/// # Safety
///
/// `count_raw` is a fixnum-tagged Word; each element is any Dylan Word.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_make_sov_literal(
    count_raw: u64,
    e0: u64,
    e1: u64,
    e2: u64,
    e3: u64,
    e4: u64,
    e5: u64,
    e6: u64,
    e7: u64,
) -> u64 {
    let count = Word::from_raw(count_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    let count = count.min(8);
    let elems = [e0, e1, e2, e3, e4, e5, e6, e7];
    // Root the element Words across the heap allocation.
    let elem_words: Vec<Word> = elems[..count]
        .iter()
        .map(|&r| Word::from_raw(r))
        .collect();
    let _guards: Vec<crate::make::RootGuard> =
        elem_words.iter().map(crate::make::RootGuard::new).collect();
    let v = crate::with_literal_pool(|pool| {
        pool.heap.alloc_simple_object_vector(count, &pool.classes)
    });
    // SAFETY: v is freshly allocated.
    if let Some(sov) =
        unsafe { try_simple_object_vector_mut(v, ClassId::SIMPLE_OBJECT_VECTOR) }
    {
        // SAFETY: same.
        let slots = unsafe { sov.slots_mut() };
        for (i, w) in elem_words.iter().enumerate() {
            let slot_ptr = &mut slots[i] as *mut Word;
            // SAFETY: slot_ptr is inside the live SOV allocation.
            unsafe { crate::write_barrier(slot_ptr, *w) };
        }
    }
    v.raw()
}

/// Sprint 21: allocate a zero-filled `<simple-object-vector>` of the
/// requested length. The fixnum-tagged length Word is decoded; out-of-
/// range values clamp to 0.
///
/// # Safety
///
/// `len_raw` must be a fixnum-tagged Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_make_sov_len(len_raw: u64) -> u64 {
    let len = Word::from_raw(len_raw).as_fixnum().unwrap_or(0).max(0) as usize;
    crate::with_literal_pool(|pool| pool.heap.alloc_simple_object_vector(len, &pool.classes)).raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_round_trip() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_simple_object_vector(4, &ct);
        // SAFETY: `w` came back from alloc.
        let v = unsafe { try_simple_object_vector(w, ct.simple_object_vector()) }
            .expect("class matches");
        assert_eq!(v.len, 4);
        // SAFETY: v points at live allocation.
        let slots = unsafe { v.slots() };
        // All slots default to fixnum-tagged 0 (Word(0)).
        assert!(slots.iter().all(|s| s.raw() == 0));
    }

    #[test]
    fn slot_round_trip_fixnums() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_simple_object_vector(3, &ct);
        // SAFETY: w is unique, mutator is single-threaded.
        let v = unsafe { try_simple_object_vector_mut(w, ct.simple_object_vector()) }
            .expect("class matches");
        // SAFETY: same — single-threaded, no aliases.
        let slots = unsafe { v.slots_mut() };
        slots[0] = Word::from_fixnum(11).unwrap();
        slots[1] = Word::from_fixnum(22).unwrap();
        slots[2] = Word::from_fixnum(33).unwrap();
        // SAFETY: reload as shared after writes.
        let v2 = unsafe { try_simple_object_vector(w, ct.simple_object_vector()) }
            .expect("class matches");
        // SAFETY: same.
        let s = unsafe { v2.slots() };
        assert_eq!(s[0].as_fixnum(), Some(11));
        assert_eq!(s[1].as_fixnum(), Some(22));
        assert_eq!(s[2].as_fixnum(), Some(33));
    }
}
