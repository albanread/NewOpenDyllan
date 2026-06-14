//! **Stdlib boundary**: new list APIs go in
//! `src/nod-dylan/dylan-sources/stdlib.dylan`, not here. This file
//! hosts the `<pair>` allocation primitive and head/tail accessors.
//! Higher-level list operations (map, fold, reverse, take, drop, …)
//! belong in Dylan. See `docs/STDLIB_BOUNDARY.md`.
//!
//! `<pair>` — Dylan cons cell. Sprint 16 adds linked-list support so
//! Richards-shape fixtures can carry task lists without `<vector>` /
//! `<simple-object-vector>` macros that Sprint 10 didn't ship.
//!
//! Layout:
//!
//! ```text
//!   [Wrapper 8B] [head: Word] [tail: Word]
//! ```
//!
//! The two slots are at offsets 8 (head) and 16 (tail) — matching the
//! single-inheritance user-class layout convention so the GC's
//! data-driven scanner walks both slots without special-casing.
//!
//! The empty list is the pinned `<empty-list>` immediate (already
//! provided by Sprint 10 as `Immediates::nil`). `empty?(p)` compares a
//! Word against that singleton — identity test, no per-call allocation.

use crate::classes::{ClassId, ClassTable};
use crate::heap::Heap;
use crate::make::RootGuard;
use crate::with_literal_pool;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// On-heap layout of a `<pair>`. Eight-byte Wrapper followed by two
/// `Word` slots. `repr(C)` so the GC's data-driven scanner walks the
/// two slots at the offsets the metadata advertises.
#[repr(C)]
pub struct Pair {
    pub wrapper: Wrapper,
    pub head: Word,
    pub tail: Word,
}

/// Byte offset of the `head` slot inside a `<pair>` allocation.
pub const PAIR_HEAD_OFFSET: usize = 8;
/// Byte offset of the `tail` slot inside a `<pair>` allocation.
pub const PAIR_TAIL_OFFSET: usize = 16;

impl Heap {
    /// Allocate a fresh `<pair>` with the given head + tail Words.
    /// Returns a pointer-tagged `Word`. Sprint 16: caller must root
    /// `head` and `tail` if a GC might fire between their construction
    /// and this call — `nod_pair_alloc` does that bracketing on the
    /// JIT side.
    pub fn alloc_pair(&self, head: Word, tail: Word, classes: &ClassTable) -> Word {
        // Payload = head (8B) + tail (8B) = 16 bytes after the Wrapper.
        let w = self.alloc_object(classes.pair(), 16);
        // SAFETY: `w` is a freshly-bumped pointer-tagged Word; its first
        // 8 bytes are a Wrapper installed by `alloc_object`; the next 16
        // bytes are zero-filled payload we now fill in.
        unsafe {
            let p = w
                .as_mut_ptr::<u8>()
                .expect("alloc_pair returned pointer-tagged Word");
            let pair = p as *mut Pair;
            (*pair).head = head;
            (*pair).tail = tail;
        }
        w
    }
}

/// JIT-callable `pair(head, tail) -> <pair>`. Roots both arg Words
/// across the heap allocation so a triggered minor GC doesn't strand
/// pointers in the slots we're about to write. Returns the new pair as
/// a raw tagged `Word`.
///
/// # Safety
///
/// `head_raw` and `tail_raw` are any valid Dylan Words (fixnums or
/// pointer-tagged). The runtime's literal pool must be initialised
/// (`with_literal_pool` lazy-inits it).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_pair_alloc(head_raw: u64, tail_raw: u64) -> u64 {
    // Sprint 11b discipline: stable-stack-bind both arg Words and
    // register them as GC roots BEFORE the `alloc_pair` call can fire a
    // minor GC. Without this, a `pair(make(<idler>, …), nil())` chain
    // could land the first arg in a stale-pointer state when its target
    // gets evacuated during pair allocation.
    let head = Word::from_raw(head_raw);
    let tail = Word::from_raw(tail_raw);
    let _h = RootGuard::new(&head);
    let _t = RootGuard::new(&tail);
    let pair_word = with_literal_pool(|pool| pool.heap.alloc_pair(head, tail, &pool.classes));
    pair_word.raw()
}

/// JIT-callable `head(p :: <pair>) -> <object>`. Reads the head slot at
/// offset 8 from the untagged pointer. If `p` is fixnum-tagged or the
/// wrapper class isn't `<pair>`, returns `0` (fixnum 0) — the lowering
/// layer guarantees the type via the `:: <pair>` annotation; this shim
/// stays defensive.
///
/// # Safety
///
/// `p_raw` is any valid Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_pair_head(p_raw: u64) -> u64 {
    let w = Word::from_raw(p_raw);
    // SAFETY: try_pair walks the wrapper-first invariant the runtime
    // upholds for every pointer-tagged Word it hands out.
    match unsafe { try_pair(w, ClassId::PAIR) } {
        Some(p) => p.head.raw(),
        None => 0,
    }
}

/// JIT-callable `tail(p :: <pair>) -> <object>`. Sibling of
/// `nod_pair_head` for the tail slot.
///
/// # Safety
///
/// Same as `nod_pair_head`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_pair_tail(p_raw: u64) -> u64 {
    let w = Word::from_raw(p_raw);
    // SAFETY: see `nod_pair_head`.
    match unsafe { try_pair(w, ClassId::PAIR) } {
        Some(p) => p.tail.raw(),
        None => 0,
    }
}

/// JIT-callable `empty?(p) -> <boolean>`. Identity-compares the arg
/// against the pinned `nil` immediate. Returns the pinned `#t` / `#f`
/// Word per the Sprint 10 boolean ABI.
///
/// # Safety
///
/// `p_raw` is any valid Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_empty_p(p_raw: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    if p_raw == imm.nil.raw() {
        imm.true_.raw()
    } else {
        imm.false_.raw()
    }
}

/// JIT-callable `nil() -> <empty-list>`. Returns the pinned `nil`
/// immediate's tagged Word. Used by fixtures that want to terminate a
/// pair chain without going through `make(<empty-list>)`.
///
/// # Safety
///
/// Trivially safe — only reads the process-global immediate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_nil() -> u64 {
    crate::literal_pool_immediates().nil.raw()
}

/// Sprint 20 JIT-callable `size(l :: <list>) -> <integer>`. Walks the
/// pair spine counting elements. O(n) — lists are unbounded in Dylan;
/// the runtime doesn't cache size on the pair header.
///
/// # Safety
///
/// `l_raw` must be a pointer-tagged `<list>` Word (a `<pair>` or
/// `<empty-list>`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_list_size(l_raw: u64) -> u64 {
    let imm = crate::literal_pool_immediates();
    let mut cur = Word::from_raw(l_raw);
    let mut count: i64 = 0;
    loop {
        if cur == imm.nil {
            break;
        }
        // SAFETY: cur is a pointer-tagged Word; ask if it's a pair.
        match unsafe { try_pair(cur, ClassId::PAIR) } {
            Some(p) => {
                count += 1;
                cur = p.tail;
            }
            None => break, // improper list — count what we've seen
        }
    }
    Word::from_fixnum(count)
        .unwrap_or(Word::from_raw(0))
        .raw()
}

/// Read view onto a pair-classed `Word`. Returns `None` if the wrapper
/// class doesn't match `<pair>`.
///
/// # Safety
///
/// `w` must either be a fixnum (in which case `None` is returned) or a
/// pointer-tagged Word whose first 8 bytes are a valid `Wrapper`.
pub unsafe fn try_pair(w: Word, pair_class: ClassId) -> Option<&'static Pair> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: wrapper-first invariant.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() == pair_class {
        // SAFETY: class match implies Pair layout.
        Some(unsafe { &*(p as *const Pair) })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_round_trip() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let a = Word::from_fixnum(11).unwrap();
        let b = Word::from_fixnum(22).unwrap();
        let w = heap.alloc_pair(a, b, &ct);
        // SAFETY: `w` came back from alloc.
        let p = unsafe { try_pair(w, ct.pair()) }.expect("class matches");
        assert_eq!(p.head.as_fixnum(), Some(11));
        assert_eq!(p.tail.as_fixnum(), Some(22));
    }

    #[test]
    fn linked_list_three_elements() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        // Build (1 . (2 . (3 . nil)))
        let nil = Word::from_fixnum(0).unwrap(); // sentinel — real `nil`
        // is the static immediate; for this micro-test any tail Word
        // would do as long as we don't traverse past it.
        let p3 = heap.alloc_pair(Word::from_fixnum(3).unwrap(), nil, &ct);
        let p2 = heap.alloc_pair(Word::from_fixnum(2).unwrap(), p3, &ct);
        let p1 = heap.alloc_pair(Word::from_fixnum(1).unwrap(), p2, &ct);
        // Walk the spine.
        // SAFETY: every pair was just allocated; class id is <pair>.
        let n1 = unsafe { try_pair(p1, ct.pair()) }.unwrap();
        assert_eq!(n1.head.as_fixnum(), Some(1));
        let n2 = unsafe { try_pair(n1.tail, ct.pair()) }.unwrap();
        assert_eq!(n2.head.as_fixnum(), Some(2));
        let n3 = unsafe { try_pair(n2.tail, ct.pair()) }.unwrap();
        assert_eq!(n3.head.as_fixnum(), Some(3));
    }
}
