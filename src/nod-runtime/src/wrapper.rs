//! The 8-byte `<wrapper>` header every heap object carries as its
//! first cell. Modelled on Dylan's `dfmc/runtime` wrapper word; adapted
//! from NCL's `HeapHeader` shape but reorganised because Dylan
//! identifies an object's class through a `ClassId`, not a 5-bit type
//! tag.
//!
//! Layout (Sprint 11):
//!
//! ```text
//!   bits  0..32   ClassId   — identity of the object's class.
//!   bits 32..48   reserved  — Sprint 12 slot-count cache.
//!   bits 48..64   gc bits   — mark / age / pinned / forwarded.
//! ```
//!
//! Sprint 11 carves the 16 GC bits as follows:
//!
//! ```text
//!   bit 48: Mark      — set when the collector has visited this object
//!                       in the current cycle (full-GC only; minor GC
//!                       uses copy-and-forward, not mark).
//!   bit 49: Tenured   — set when the object has been promoted from
//!                       young into old.
//!   bit 50: Pinned    — set when conservative stack scanning found a
//!                       potentially-live pointer to this object. The
//!                       minor collector leaves pinned objects in place.
//!   bit 51: Forwarded — set on the from-space copy after evacuation.
//!                       When set, bits 0..48 hold the new (untagged)
//!                       address >> 8 of the to-space copy. See
//!                       `set_forwarded` / `forwarding_addr` below.
//! ```
//!
//! Forwarding encoding deserves a note. NCL writes a forward by tagging
//! the entire header word with `Tag::Forward(7)`. Dylan has no 3-bit
//! tag; we use the `Forwarded` GC bit at position 51 plus bits 0..48
//! for the new address verbatim:
//!
//! ```text
//!   bit 51 = 1 (Forwarded)
//!   bits 0..48 = new_addr        (x64 user-space pointers fit in 48
//!                                  bits; 8-byte alignment means the
//!                                  low 3 bits are always zero, but we
//!                                  store them anyway so the decode is
//!                                  a simple mask)
//! ```
//!
//! The other GC bits (mark / tenured / pinned at 0..2) are zero on a
//! forwarded wrapper — from-space objects don't participate in further
//! GC bit checks; only the Forwarded indicator is read.

use crate::classes::ClassId;

const CLASS_SHIFT: u32 = 0;
const CLASS_BITS: u32 = 32;
const CLASS_MASK: u64 = (1 << CLASS_BITS) - 1;

const GC_SHIFT: u32 = 48;
const GC_BITS: u32 = 16;
const GC_MASK: u64 = (1 << GC_BITS) - 1;

/// Mask covering the low 48 bits — used by the forwarding encoding to
/// store a full x64 user-space pointer verbatim while the GC bits sit
/// above it.
const ADDR_MASK_48: u64 = (1 << 48) - 1;

// Individual GC bit positions WITHIN the 16-bit gc field.
const GC_MARK_BIT: u32 = 0;
const GC_TENURED_BIT: u32 = 1;
const GC_PINNED_BIT: u32 = 2;
const GC_FORWARDED_BIT: u32 = 3;

/// Symbolic identifier for a GC-bit flag in the wrapper's 16-bit GC
/// field. Sprint 11 carves four; bits 4..15 are reserved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum GcBit {
    /// Set when the full-GC tracer has visited this object in the
    /// current cycle. Cleared at cycle start.
    Mark = 1 << GC_MARK_BIT,
    /// Set after the object has been promoted from young into old.
    Tenured = 1 << GC_TENURED_BIT,
    /// Set when conservative stack scanning has found a potentially-
    /// live pointer at the object. The minor collector leaves pinned
    /// objects in place.
    Pinned = 1 << GC_PINNED_BIT,
    /// Set on the from-space copy after evacuation. Bits 0..48 of the
    /// wrapper hold the new address verbatim; use
    /// `Wrapper::forwarding_addr` to decode.
    Forwarded = 1 << GC_FORWARDED_BIT,
}

/// `<wrapper>` header. Every heap-allocated Dylan object starts with one.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Wrapper {
    pub raw: u64,
}

impl Wrapper {
    /// Build a fresh wrapper for a heap object of the given class.
    /// GC bits are zero on construction — populated by the collector.
    pub const fn new(class: ClassId) -> Self {
        Wrapper {
            raw: ((class.0 as u64) & CLASS_MASK) << CLASS_SHIFT,
        }
    }

    /// Recover the class id this object claims. Returns garbage if the
    /// wrapper has been overwritten with a forwarding pointer — callers
    /// MUST check `is_forwarded` first if they're scanning a from-space
    /// region post-evacuation.
    pub const fn class(self) -> ClassId {
        ClassId(((self.raw >> CLASS_SHIFT) & CLASS_MASK) as u32)
    }

    /// Raw 16-bit gc bit-field.
    pub const fn gc_bits(self) -> u16 {
        ((self.raw >> GC_SHIFT) & GC_MASK) as u16
    }

    /// Test whether a given gc flag is set.
    pub const fn has_gc_bit(self, bit: GcBit) -> bool {
        (self.gc_bits() & (bit as u16)) != 0
    }

    /// Set a gc flag (returns the new wrapper).
    pub const fn with_gc_bit(self, bit: GcBit) -> Self {
        Wrapper {
            raw: self.raw | ((bit as u64) << GC_SHIFT),
        }
    }

    /// Clear a gc flag (returns the new wrapper).
    pub const fn without_gc_bit(self, bit: GcBit) -> Self {
        Wrapper {
            raw: self.raw & !((bit as u64) << GC_SHIFT),
        }
    }

    /// True iff this wrapper is a forwarding pointer (set on a
    /// from-space cell after the live object has been evacuated).
    pub const fn is_forwarded(self) -> bool {
        self.has_gc_bit(GcBit::Forwarded)
    }

    /// Build a forwarding wrapper. `new_addr` is the to-space address
    /// of the evacuated object (untagged). Stored verbatim in bits
    /// 0..48 — x64 user-space pointers fit in 48 bits, so the encoding
    /// is lossless for any heap address.
    pub const fn forward_to(new_addr: usize) -> Self {
        Wrapper {
            raw: ((GcBit::Forwarded as u64) << GC_SHIFT) | ((new_addr as u64) & ADDR_MASK_48),
        }
    }

    /// Decode a forwarding pointer's target address. Caller must check
    /// `is_forwarded()` first.
    pub const fn forwarding_addr(self) -> usize {
        (self.raw & ADDR_MASK_48) as usize
    }

    /// Raw-bits helper: is this 64-bit word a forwarding marker?
    /// Sprint 23: used by `DylanLayout::classify` on the `newgc-core`
    /// hot path so we don't have to materialise a `Wrapper` to test
    /// the bit. Inlined to a single mask-and-test in release.
    #[inline(always)]
    pub const fn raw_is_forwarded(raw: u64) -> bool {
        (raw >> GC_SHIFT) & (GcBit::Forwarded as u64) != 0
    }

    /// Raw-bits helper: decode the forwarding target encoded by
    /// [`forward_to`]. Caller must check `raw_is_forwarded` first.
    #[inline(always)]
    pub const fn raw_forward_target(raw: u64) -> usize {
        (raw & ADDR_MASK_48) as usize
    }
}

impl std::fmt::Debug for Wrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_forwarded() {
            f.debug_struct("Wrapper")
                .field("forwarded_to", &format_args!("{:#x}", self.forwarding_addr()))
                .finish()
        } else {
            f.debug_struct("Wrapper")
                .field("class", &self.class())
                .field("gc_bits", &format_args!("{:#018b}", self.gc_bits()))
                .finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::ClassTable;

    #[test]
    fn wrapper_size_is_eight_bytes() {
        assert_eq!(std::mem::size_of::<Wrapper>(), 8);
        assert_eq!(std::mem::align_of::<Wrapper>(), 8);
    }

    #[test]
    fn wrapper_round_trips_class_id() {
        let ct = ClassTable::new();
        let w = Wrapper::new(ct.integer());
        assert_eq!(w.class(), ct.integer());
        assert_eq!(w.gc_bits(), 0);
        assert!(!w.is_forwarded());
    }

    #[test]
    fn gc_bits_set_and_clear() {
        let ct = ClassTable::new();
        let w = Wrapper::new(ct.byte_string());
        assert!(!w.has_gc_bit(GcBit::Tenured));
        let w2 = w.with_gc_bit(GcBit::Tenured);
        assert!(w2.has_gc_bit(GcBit::Tenured));
        // Class still readable.
        assert_eq!(w2.class(), ct.byte_string());
        let w3 = w2.without_gc_bit(GcBit::Tenured);
        assert!(!w3.has_gc_bit(GcBit::Tenured));
    }

    #[test]
    fn forwarding_round_trip() {
        // Every 8-byte-aligned address must round-trip exactly; heap
        // bumps land on cell boundaries, not on 256-byte boundaries.
        for &new_addr in &[
            0x0001_2345_6700_usize,
            0x0001_2345_6708,
            0x0001_2345_6710,
            0x0001_2345_6738,
            0x0000_7FF8_DEAD_BEE0,
        ] {
            let f = Wrapper::forward_to(new_addr);
            assert!(f.is_forwarded());
            assert_eq!(f.forwarding_addr(), new_addr, "lossy at {new_addr:#x}");
        }
    }

    #[test]
    fn pinned_bit_independent_of_other_bits() {
        let ct = ClassTable::new();
        let w = Wrapper::new(ct.simple_object_vector())
            .with_gc_bit(GcBit::Pinned)
            .with_gc_bit(GcBit::Mark);
        assert!(w.has_gc_bit(GcBit::Pinned));
        assert!(w.has_gc_bit(GcBit::Mark));
        assert!(!w.has_gc_bit(GcBit::Tenured));
        assert!(!w.has_gc_bit(GcBit::Forwarded));
        assert_eq!(w.class(), ct.simple_object_vector());
    }
}
