//! Heap helpers shared by the semispace generational design.
//!
//! Sprint 23: `StartBits` and the start-bit primitives are only used
//! by the legacy semispace backend; under the default `newgc-backend`
//! the page heap supplies its own atomic-OR start-bit handling. The
//! `CardTable` type is still publicly used (the `HeapStats` snapshot
//! exposes a card-table interface and the JIT-side write barrier
//! marks cards through the page heap's `mark_card_at`), so we keep
//! the type definitions unconditional and gate only the unused
//! semispace-side helpers.
#![cfg_attr(not(feature = "semispace-backend"), allow(dead_code))]
//!
//! Lifted from NCL's `ncl-runtime/src/heap_common.rs`:
//!
//!   - `CardTable` — soft-card write-barrier bookkeeping. One byte per
//!     `CARD_SIZE_BYTES` of heap storage. Mutators mark cards via
//!     atomic byte stores from the write barrier; the GC reads them
//!     during minor cycles to skip clean regions.
//!   - `StartBits` — lock-free `Arc<[AtomicU64]>` bitmap. One bit per
//!     8-byte cell, set when the cell is the start of a heap object.
//!     Lets the GC walk a semispace linearly without re-reading the
//!     object headers it just wrote. Dylan's version is simpler than
//!     NCL's because we have no headerless cons cells — every start
//!     is a wrapper-bearing object, so one bit suffices instead of
//!     NCL's two.
//!
//! Geometry constants are lifted identically. See NCL `heap_common.rs`
//! for the design rationale.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

// -- Card table --------------------------------------------------------------
//
// Software card-marking for the write barrier. One byte per
// CARD_SIZE_BYTES of heap storage. Mutators write to it lock-free
// via atomic byte stores; the GC reads it during minor cycles to
// scan only regions known to (possibly) hold young pointers.
//
// False positives are fine — the scanner filters non-pointers and
// pointers that aren't into young. False negatives (an unmarked card
// containing a young pointer) are NOT fine; the discipline is that
// every old-heap store must mark.

/// One card covers 512 bytes of heap (= 64 cells of 8 bytes).
pub const CARD_SIZE_BYTES: usize = 512;
pub const CARD_SIZE_CELLS: usize = CARD_SIZE_BYTES / 8;

/// Card-marking write-barrier table. Address is the heap base; one
/// byte tracks each `CARD_SIZE_BYTES` chunk of coverage.
pub struct CardTable {
    bytes: Box<[AtomicU8]>,
}

impl CardTable {
    pub fn new(coverage_bytes: usize) -> CardTable {
        let n_cards = coverage_bytes.div_ceil(CARD_SIZE_BYTES);
        let v: Vec<AtomicU8> = (0..n_cards).map(|_| AtomicU8::new(0)).collect();
        CardTable {
            bytes: v.into_boxed_slice(),
        }
    }

    pub fn n_cards(&self) -> usize {
        self.bytes.len()
    }

    /// Mark the card containing `byte_offset` (relative to the start
    /// of the covered region). Lock-free, single byte store.
    pub fn mark_offset(&self, byte_offset: usize) {
        let card = byte_offset / CARD_SIZE_BYTES;
        if let Some(b) = self.bytes.get(card) {
            b.store(1, Ordering::Relaxed);
        }
    }

    pub fn is_dirty(&self, card: usize) -> bool {
        self.bytes
            .get(card)
            .is_some_and(|b| b.load(Ordering::Relaxed) != 0)
    }

    pub fn clear(&self, card: usize) {
        if let Some(b) = self.bytes.get(card) {
            b.store(0, Ordering::Relaxed);
        }
    }

    pub fn clear_all(&self) {
        for b in self.bytes.iter() {
            b.store(0, Ordering::Relaxed);
        }
    }

    /// Count dirty cards. Useful for tests and diagnostics.
    pub fn dirty_count(&self) -> usize {
        self.bytes
            .iter()
            .filter(|b| b.load(Ordering::Relaxed) != 0)
            .count()
    }
}

// -- Start bits --------------------------------------------------------------
//
// Lock-free Arc<[AtomicU64]> alias. One bit per 8-byte cell. Dylan
// has no headerless cons cells (everything carries a Wrapper) so we
// only need ONE bit per cell — `1 = this cell is the start of an
// object`. That's simpler than NCL's 2-bits-per-cell encoding.

/// One u64 packs the start-bits for 64 consecutive cells.
pub const CELLS_PER_STARTS_WORD: usize = 64;

pub type StartBits = Arc<[AtomicU64]>;

/// Allocate a fresh start-bit bitmap covering `n_cells` cells.
pub fn new_start_bits(n_cells: usize) -> StartBits {
    let n_words = n_cells.div_ceil(CELLS_PER_STARTS_WORD);
    let v: Vec<AtomicU64> = (0..n_words).map(|_| AtomicU64::new(0)).collect();
    Arc::from(v.into_boxed_slice())
}

#[inline]
fn start_bit_position(idx: usize) -> (usize, u32) {
    let w = idx / CELLS_PER_STARTS_WORD;
    let b = (idx % CELLS_PER_STARTS_WORD) as u32;
    (w, b)
}

/// Mark cell `idx` as an object start.
pub fn set_start_bit(starts: &[AtomicU64], idx: usize) {
    let (w, bit) = start_bit_position(idx);
    starts[w].fetch_or(1u64 << bit, Ordering::Relaxed);
}

/// Clear cell `idx`'s start bit.
pub fn clear_start_bit(starts: &[AtomicU64], idx: usize) {
    let (w, bit) = start_bit_position(idx);
    starts[w].fetch_and(!(1u64 << bit), Ordering::Relaxed);
}

/// Test cell `idx`'s start bit.
pub fn is_start_bit(starts: &[AtomicU64], idx: usize) -> bool {
    let (w, bit) = start_bit_position(idx);
    (starts[w].load(Ordering::Relaxed) >> bit) & 1 != 0
}

/// Clear every start bit up to (but not including) cell `end_cells`.
pub fn clear_start_bits_below(starts: &[AtomicU64], end_cells: usize) {
    let words = end_cells.div_ceil(CELLS_PER_STARTS_WORD);
    for word in starts.iter().take(words) {
        word.store(0, Ordering::Relaxed);
    }
}

/// Visit every start bit `< end_cells` in ascending cell-index order.
pub fn for_each_start<F: FnMut(usize)>(starts: &[AtomicU64], end_cells: usize, mut f: F) {
    let n_words = end_cells.div_ceil(CELLS_PER_STARTS_WORD);
    for (w, atomic_word) in starts.iter().enumerate().take(n_words) {
        let mut word = atomic_word.load(Ordering::Relaxed);
        let base = w * CELLS_PER_STARTS_WORD;
        while word != 0 {
            let b = word.trailing_zeros() as usize;
            let idx = base + b;
            if idx >= end_cells {
                return;
            }
            f(idx);
            word &= word - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_table_marks_round_trip() {
        let ct = CardTable::new(4096);
        assert!(!ct.is_dirty(0));
        ct.mark_offset(100);
        assert!(ct.is_dirty(0));
        ct.mark_offset(1000);
        assert!(ct.is_dirty(1));
        ct.clear(0);
        assert!(!ct.is_dirty(0));
    }

    #[test]
    fn start_bits_set_and_iterate() {
        let sb = new_start_bits(200);
        set_start_bit(&sb, 0);
        set_start_bit(&sb, 5);
        set_start_bit(&sb, 100);
        set_start_bit(&sb, 199);
        let mut seen = Vec::new();
        for_each_start(&sb, 200, |i| seen.push(i));
        assert_eq!(seen, vec![0, 5, 100, 199]);
    }

    #[test]
    fn start_bits_clear_below() {
        let sb = new_start_bits(150);
        set_start_bit(&sb, 5);
        set_start_bit(&sb, 70);
        clear_start_bits_below(&sb, 100);
        assert!(!is_start_bit(&sb, 5));
        assert!(!is_start_bit(&sb, 70));
    }
}
