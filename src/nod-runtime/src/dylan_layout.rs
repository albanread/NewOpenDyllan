//! Sprint 23 — Dylan binding for the `newgc-core` `HeapLayout` trait.
//!
//! Implements `HeapLayout` against Dylan's 1-bit tag scheme + 8-byte
//! `<wrapper>` header. The whole binding is type-level: `DylanLayout`
//! is a zero-sized marker; every method is a `fn`, never `&self`.
//! `newgc-core`'s mark/evac/scan paths inline against the concrete
//! type, so the GC engine sees no dynamic dispatch on the hot path.
//!
//! Tag classification (`classify`) is a three-way decode:
//!   - forwarding marker (the `<wrapper>`'s `Forwarded` GC bit at bit
//!     51 — see [`wrapper.rs`]'s module doc),
//!   - low bit = 0 → immediate (fixnum or a wrapper header — header
//!     cells live at 8-aligned `ClassMetadata` addresses; their first
//!     cell viewed as a u64 has its low 32 bits = ClassId, low bit of
//!     ClassId is 0 for every registered class because ClassId is a
//!     u32 starting at 0 with no skip — so headers naturally classify
//!     as Immediate, which is the safe "leave this slot alone"
//!     dispatch),
//!   - low bit = 1 → header-bearing pointer; strip the tag to recover
//!     the wrapper-start address.
//!
//! Dylan never uses NewGC's `PointerCons` variant — every heap object,
//! including `<pair>`, carries a wrapper. The cons-start bit on the
//! page heap's start bitmap therefore stays unused (one wasted bit per
//! cell — 3% memory overhead, irrelevant correctness-wise).

use newgc_core::traits::{HeapLayout, ObjectLayout, PointerKind, WordKind};

use crate::classes::class_metadata_ptr;
use crate::wrapper::Wrapper;

/// Zero-sized marker for the Dylan `HeapLayout` binding.
#[derive(Copy, Clone, Debug, Default)]
pub struct DylanLayout;

impl HeapLayout for DylanLayout {
    /// Fresh cells get raw zero — which decodes as fixnum-0
    /// (`Immediate`) under Dylan's tag scheme. Distinct from the
    /// boolean `#f`, which is a pointer-tagged Word into the static
    /// area's pinned singleton.
    const FILL_WORD: u64 = 0;

    #[inline(always)]
    fn classify(raw: u64) -> WordKind {
        // First check: forwarding marker. The GC writes
        // `Wrapper::forward_to(new_addr)` into the source object's
        // first cell after copying. The `Forwarded` bit lives at
        // wrapper bit 51 (GC_SHIFT + GC_FORWARDED_BIT = 48 + 3); the
        // low 48 bits hold the new address verbatim.
        //
        // We test the forwarding bit BEFORE the tag check because a
        // forward marker's low bits are part of the rewritten address
        // (8-aligned, so low bit = 0) — the marker would otherwise
        // misclassify as Immediate.
        if Wrapper::raw_is_forwarded(raw) {
            return WordKind::Forwarded(
                Wrapper::raw_forward_target(raw) as *const u8,
            );
        }
        if raw & 1 == 0 {
            // Fixnum, boolean static-singleton, nil, or a wrapper
            // header cell. All `Immediate` from the GC's
            // "leave-this-cell-alone" point of view.
            WordKind::Immediate
        } else {
            // Pointer-tagged Word; mask off the tag bit to recover
            // the 8-aligned wrapper-start address.
            WordKind::PointerHeader((raw & !1) as *const u8)
        }
    }

    #[inline(always)]
    fn make_forward(new_addr: *const u8) -> u64 {
        Wrapper::forward_to(new_addr as usize).raw
    }

    #[inline(always)]
    fn make_pointer(addr: *const u8, _kind: PointerKind) -> u64 {
        // Dylan only uses `PointerHeader`. `PointerCons` would be a
        // contract violation — the GC never asks make_pointer for one
        // in practice because `classify` never returns `PointerCons`
        // for a Dylan Word, but we tolerate the kind arg for trait
        // shape parity.
        (addr as u64) | 1
    }

    #[inline(always)]
    fn rewrite_pointer_addr(_old_raw: u64, new_addr: *const u8) -> u64 {
        // Dylan tag bits are a single bit at position 0; for a pointer
        // slot it's always 1. We don't need to OR in `old_raw & 1`
        // because we know we're rewriting a pointer (the evac path
        // only calls this when classify returned `PointerHeader`).
        (new_addr as u64) | 1
    }

    #[inline(always)]
    unsafe fn header_layout(header_cell: *const u64) -> ObjectLayout {
        // SAFETY: caller asserts `header_cell` points at a valid
        // `<wrapper>` cell — the GC's start-bit gate enforces this
        // before any call.
        let raw = unsafe { *header_cell };
        let w = Wrapper { raw };
        // Forwarded wrappers shouldn't reach header_layout (mark/evac
        // short-circuit on the `Forwarded` classify result before
        // dereferencing). Defensive fallback: pretend it's a one-cell
        // object so the GC skips past it.
        if w.is_forwarded() {
            return ObjectLayout::opaque(1);
        }
        // Look up the class metadata. `class_metadata_for` panics on
        // an unknown id; `class_metadata_ptr` returns null. We get the
        // null case in two real situations:
        //   1. The collector walks the heap linearly and stumbles on a
        //      stale start-bit from a recycled page (a NewGC `acquire_
        //      free_page` zeros the bitmap, but a stale bit on a page
        //      not yet recycled can persist). The wrapper is garbage.
        //   2. Cross-test contamination — Sprint 16's `_reset_user_
        //      classes_for_tests` retires user class IDs from the
        //      registry while their previously-allocated instances may
        //      still live in the heap.
        // In either case, treat the object as a one-cell opaque blob
        // so the linear walker steps past it cleanly. The GC won't
        // follow any payload pointers — safe even if the bytes that
        // follow look pointer-shaped.
        let metadata_ptr = class_metadata_ptr(w.class());
        if metadata_ptr.is_null() {
            return ObjectLayout::opaque(1);
        }
        // SAFETY: header_cell points at a live object whose class is
        // `*metadata_ptr` (registry lookup matched); we hand the
        // address to its layout function.
        let md = unsafe { &*metadata_ptr };
        let addr = header_cell as usize;
        let (total_cells, ptr_start, ptr_end) =
            unsafe { (md.layout)(addr) };
        ObjectLayout {
            total_cells,
            pointer_cells_start: ptr_start,
            pointer_cells_end: ptr_end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::{ClassId, ClassTable};
    use newgc_core::traits::WordKind;

    #[test]
    fn fixnum_zero_classifies_as_immediate() {
        // A fixnum 0 raw = 0, which is our `FILL_WORD`. Must be
        // `Immediate` so the GC doesn't try to follow.
        assert!(matches!(DylanLayout::classify(0), WordKind::Immediate));
        assert_eq!(DylanLayout::FILL_WORD, 0);
    }

    #[test]
    fn pointer_classifies_as_header() {
        let dummy: u64 = 0;
        let addr = &dummy as *const u64 as usize & !7;
        let raw = (addr as u64) | 1;
        match DylanLayout::classify(raw) {
            WordKind::PointerHeader(a) => assert_eq!(a as usize, addr),
            other => panic!("expected PointerHeader, got {other:?}"),
        }
    }

    #[test]
    fn forwarded_classifies_as_forwarded() {
        // 8-aligned target address: pick one that doesn't collide
        // with the GcBit::Forwarded encoding.
        let target: usize = 0x0001_2345_6700;
        let raw = Wrapper::forward_to(target).raw;
        match DylanLayout::classify(raw) {
            WordKind::Forwarded(a) => assert_eq!(a as usize, target),
            other => panic!("expected Forwarded, got {other:?}"),
        }
    }

    #[test]
    fn make_forward_round_trips_through_classify() {
        let target: usize = 0x0000_7FF0_DEAD_BEE0;
        let raw = DylanLayout::make_forward(target as *const u8);
        match DylanLayout::classify(raw) {
            WordKind::Forwarded(a) => assert_eq!(a as usize, target),
            other => panic!("expected Forwarded, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_pointer_preserves_tag_bit() {
        let old: u64 = 0x1000 | 1;
        let new_addr: usize = 0x2000;
        let r = DylanLayout::rewrite_pointer_addr(old, new_addr as *const u8);
        assert_eq!(r & 1, 1, "low bit must remain 1 for a pointer");
        assert_eq!(r & !1, new_addr as u64, "address must be the new one");
    }

    #[test]
    fn header_layout_for_integer_is_one_cell_opaque() {
        // `<integer>` instance is just the wrapper — one cell, no
        // pointer cells. Build a wrapper for ClassId::INTEGER on the
        // stack and ask the layout for its shape.
        let _ct = ClassTable::new();
        let w = Wrapper::new(ClassId::INTEGER);
        let cell = w.raw;
        let cell_ptr = &cell as *const u64;
        let layout = unsafe { DylanLayout::header_layout(cell_ptr) };
        assert_eq!(layout.total_cells, 1);
        assert_eq!(layout.pointer_cell_count(), 0);
    }

    #[test]
    fn header_layout_for_pair_reports_two_pointer_cells() {
        // `<pair>` has wrapper + head + tail = 3 cells, with both
        // payload cells pointer-typed.
        let _ct = ClassTable::new();
        let w = Wrapper::new(ClassId::PAIR);
        let cell = w.raw;
        let cell_ptr = &cell as *const u64;
        let layout = unsafe { DylanLayout::header_layout(cell_ptr) };
        assert_eq!(layout.total_cells, 3);
        assert_eq!(layout.pointer_cells_start, 1);
        assert_eq!(layout.pointer_cells_end, 3);
    }

    #[test]
    fn header_layout_for_symbol_skips_hash_cell() {
        // `<symbol>` has wrapper + hash/pad(non-Word) + name Word = 3
        // cells. Only cell 2 is a pointer.
        let _ct = ClassTable::new();
        let w = Wrapper::new(ClassId::SYMBOL);
        let cell = w.raw;
        let cell_ptr = &cell as *const u64;
        let layout = unsafe { DylanLayout::header_layout(cell_ptr) };
        assert_eq!(layout.total_cells, 3);
        assert_eq!(layout.pointer_cells_start, 2);
        assert_eq!(layout.pointer_cells_end, 3);
    }
}
