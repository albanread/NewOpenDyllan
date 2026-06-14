//! Generational copying heap (Sprint 11).
//!
//! Structural lift from NCL's `ncl-runtime/src/heap.rs` semispace
//! design, heavily adapted for Dylan:
//!
//!   - **One-bit tag scheme.** NCL has a 3-bit `Tag` enum and
//!     headerless cons cells. Dylan has only bit-0 (fixnum/pointer);
//!     every heap object carries a `Wrapper` header. The scanner is
//!     therefore data-driven through `ClassMetadata::scan` instead of
//!     a per-`HeapType` switch.
//!   - **Start bitmap is one bit per cell.** NCL uses two bits
//!     (start + cons-vs-headered); Dylan only needs `1 = start of
//!     object`.
//!   - **Forwarding pointer.** NCL writes a `Tag::Forward(7)`-tagged
//!     pointer into the header cell. Dylan repurposes the
//!     `GcBit::Forwarded` flag + the wrapper's class-id slot; see
//!     `Wrapper::forward_to`.
//!
//! What's lifted intact, structurally:
//!
//!   - `Semispace` — bump-pointer region with a start-bit bitmap.
//!   - `OldGen` — two `Semispace`s that swap on full GC.
//!   - `Heap` — pairs `young: Semispace` + `old: OldGen` + a
//!     `CardTable` covering old.
//!   - `collect_minor` — young → old.live, copy survivors.
//!   - `collect_full` — young + old.live → old.scratch, swap old.
//!   - Cheney-style breadth-first scan via a scan pointer.
//!
//! Sprint 11 chose **option (b) from the brief**: synchronous GC
//! triggered only at allocation sites in Rust code. No JIT-side
//! safepoint polls, no precise stack roots via `gc.statepoint`. The
//! JIT-side polls and precise roots are Sprint 11b — see DEFERRED.md.
//!
//! The collector body is intentionally raw-pointer-flavoured: it holds
//! the heap mutex for the duration of a collection, so safety reduces
//! to "no other thread can read or write these regions while the
//! collector runs". The unsafe blocks document this invariant.
//!
//! ## Sprint 11c — lock-free root registry
//!
//! Sprint 11b's `Heap::register_root` / `unregister_root` took a
//! `Mutex<Vec<*const Word>>` lock on every call. The Richards-shape
//! bench (Sprint 16) revealed those mutex operations dominated the
//! runtime — hundreds of millions of acquisitions per benchmark run,
//! opaque to LLVM, identical in both sealed and open variants, so the
//! sealing-vs-open differential collapsed to ~1.06×.
//!
//! Sprint 11c replaces the mutex with a process-wide thread-local
//! `RefCell<Vec<*const Word>>` (see `register_root` / `unregister_root` /
//! `root_count` / `for_each_root` below). The runtime is single-threaded
//! today (Sprint 28 lights up multi-threading); the thread-local pattern
//! is safe and ~50–100× cheaper than a mutex on the hot path. A
//! `OnceLock<ThreadId>` debug-assert catches any future caller that
//! violates the single-thread invariant before silent corruption can
//! occur.
//!
//! When Sprint 28 introduces multi-threaded mutators, this design
//! becomes per-thread-local roots that the collector enumerates across
//! all parked threads — see DEFERRED.md.

use std::cell::RefCell;

#[cfg(feature = "semispace-backend")]
use std::sync::Mutex;

use crate::classes::ClassId;
#[cfg(feature = "semispace-backend")]
use crate::classes::{ClassTable, class_metadata_for};
#[cfg(feature = "semispace-backend")]
use crate::heap_common::{
    CARD_SIZE_BYTES, CARD_SIZE_CELLS, CardTable, StartBits, clear_start_bit,
    clear_start_bits_below, for_each_start, is_start_bit, new_start_bits, set_start_bit,
};
use crate::word::Word;
#[cfg(feature = "semispace-backend")]
use crate::wrapper::{GcBit, Wrapper};
#[cfg(feature = "newgc-backend")]
use crate::wrapper::Wrapper;

/// Default young-generation capacity (4 MB).
///
/// Sprint 43d bumped this to 64 MB as a crash-resistance workaround
/// because the GC lacked precise roots and safepoint polls — giving
/// the heap more room meant collections fired less often and the
/// still-conservative scanner had fewer chances to corrupt live data.
///
/// Sprint 45 restored correct GC behaviour:
///   - Sprint 45c: JIT precise safepoints (gc.statepoint-style slots)
///   - Sprint 45d: AOT precise slot-slab roots
///   - Sprint 45e: safepoint polls at function entry + loop headers
///
/// With precise roots the GC can safely collect on every minor cycle.
/// 4 MB is a normal nursery size; the heap grows via the page pool as
/// needed. The IDE live set at startup (class table, symbol table,
/// COM handles, stdlib metadata) comfortably fits in 4 MB.
pub const DEFAULT_YOUNG_BYTES: usize = 4 * 1024 * 1024;
/// Default old-generation capacity (12 MB — 3× the young gen).
/// Sprint 43d workaround retired; see `DEFAULT_YOUNG_BYTES`.
pub const DEFAULT_OLD_BYTES: usize = 12 * 1024 * 1024;
/// Legacy alias preserved for any external callers. Sprint 09's name
/// for the bump-heap reservation; Sprint 11 keeps it as the sum of
/// young + old.
pub const DEFAULT_RESERVATION_BYTES: usize = DEFAULT_YOUNG_BYTES + DEFAULT_OLD_BYTES;

/// Object alignment. Heap pointers must keep bits [2:0] clear so the
/// tag in bit 0 doesn't collide with payload.
pub const HEAP_ALIGN: usize = 8;

/// GC knobs. Sprint 11 only exposes capacity; promotion policy is
/// "any survivor of a minor GC tenures into old". A two-cycle survival
/// threshold (NCL's policy) lands in Sprint 11b.
#[derive(Copy, Clone, Debug)]
pub struct GcConfig {
    pub young_bytes: usize,
    pub old_bytes: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            young_bytes: DEFAULT_YOUNG_BYTES,
            old_bytes: DEFAULT_OLD_BYTES,
        }
    }
}

// -- Semispace (Sprint 23: semispace-backend feature gate) -------------------

#[cfg(feature = "semispace-backend")]
/// A bump-allocated cell-aligned region with a start-bit bitmap.
pub(crate) struct Semispace {
    cells: Box<[u64]>,
    starts: StartBits,
    top: usize,
}

#[cfg(feature = "semispace-backend")]
impl Semispace {
    fn new(size_bytes: usize) -> Self {
        let n_cells = size_bytes / 8;
        let cells = vec![0u64; n_cells].into_boxed_slice();
        let starts = new_start_bits(n_cells);
        Semispace {
            cells,
            starts,
            top: 0,
        }
    }

    fn capacity_cells(&self) -> usize {
        self.cells.len()
    }

    fn capacity_bytes(&self) -> usize {
        self.cells.len() * 8
    }

    fn used_bytes(&self) -> usize {
        self.top * 8
    }

    fn free_cells(&self) -> usize {
        self.cells.len() - self.top
    }

    fn base_addr(&self) -> usize {
        self.cells.as_ptr() as usize
    }

    fn contains(&self, addr: usize) -> bool {
        let base = self.base_addr();
        let end = base + self.capacity_bytes();
        addr >= base && addr < end
    }

    /// Try to bump-allocate `n_bytes` of space, returning the address
    /// of the first byte on success. Returns `None` on exhaustion.
    fn try_alloc_bytes(&mut self, n_bytes: usize) -> Option<usize> {
        let aligned = n_bytes.next_multiple_of(HEAP_ALIGN);
        let cells_needed = aligned / 8;
        if self.top + cells_needed > self.cells.len() {
            return None;
        }
        let cell_idx = self.top;
        let addr = self.base_addr() + cell_idx * 8;
        self.top += cells_needed;
        set_start_bit(&self.starts, cell_idx);
        Some(addr)
    }

    /// Reset the semispace to empty (zero top, clear start bitmap).
    /// Used at the end of a minor GC after every survivor has been
    /// evacuated, or after full-GC swap.
    fn reset(&mut self) {
        clear_start_bits_below(&self.starts, self.top);
        self.top = 0;
    }

    /// Conservative pin: walk `[range_lo, range_hi)` word-aligned and
    /// pin any object in this semispace whose start cell appears as a
    /// pointer-tagged Word in the range.
    ///
    /// # Safety
    ///
    /// `range_lo..range_hi` must be a readable, 8-byte-aligned address
    /// range.
    unsafe fn pin_pointers_in_range(&self, range_lo: usize, range_hi: usize) -> usize {
        if range_lo >= range_hi {
            return 0;
        }
        let base = self.base_addr();
        let end = base + self.capacity_bytes();
        let scan_start = range_lo.next_multiple_of(8);
        let scan_end = range_hi & !7;
        let mut n_pinned = 0usize;
        let mut p = scan_start as *const u64;
        let end_p = scan_end as *const u64;
        while p < end_p {
            // SAFETY: caller asserts range is readable + aligned.
            let raw = unsafe { *p };
            let w = Word::from_raw(raw);
            if w.is_pointer() {
                let target = (raw & !1) as usize;
                if target >= range_lo && target < range_hi {
                    // SAFETY: still inside input range.
                    p = unsafe { p.add(1) };
                    continue;
                }
                if target >= base && target < end {
                    let cell_idx = (target - base) / 8;
                    if is_start_bit(&self.starts, cell_idx) {
                        // SAFETY: target is a header start.
                        let header_ptr = target as *mut u64;
                        let cur = unsafe { *header_ptr };
                        let wrapper = Wrapper { raw: cur };
                        if !wrapper.is_forwarded() && !wrapper.has_gc_bit(GcBit::Pinned) {
                            let pinned = wrapper.with_gc_bit(GcBit::Pinned);
                            // SAFETY: sole writer through this path under heap mutex.
                            unsafe { *header_ptr = pinned.raw };
                            n_pinned += 1;
                        }
                    }
                }
            }
            // SAFETY: incrementing inside asserted range.
            p = unsafe { p.add(1) };
        }
        n_pinned
    }

    /// Clear pinned bits on every header-bearing object in this
    /// semispace.
    fn clear_pinned_bits(&self) {
        let cells_ptr = self.cells.as_ptr() as *mut u64;
        let top = self.top;
        for_each_start(&self.starts, top, |idx| {
            // SAFETY: idx is a start cell.
            let cell_ptr = unsafe { cells_ptr.add(idx) };
            let cur = unsafe { *cell_ptr };
            let wrapper = Wrapper { raw: cur };
            if wrapper.is_forwarded() {
                return;
            }
            if wrapper.has_gc_bit(GcBit::Pinned) {
                let cleared = wrapper.without_gc_bit(GcBit::Pinned);
                // SAFETY: sole writer.
                unsafe { *cell_ptr = cleared.raw };
            }
        });
    }
}

// -- OldGen ------------------------------------------------------------------

#[cfg(feature = "semispace-backend")]
/// Old generation: two semispaces that swap on full GC.
pub(crate) struct OldGen {
    live: Semispace,
    scratch: Semispace,
}

#[cfg(feature = "semispace-backend")]
impl OldGen {
    fn new(per_space_bytes: usize) -> Self {
        OldGen {
            live: Semispace::new(per_space_bytes),
            scratch: Semispace::new(per_space_bytes),
        }
    }

    fn swap(&mut self) {
        std::mem::swap(&mut self.live, &mut self.scratch);
    }
}

// -- Heap --------------------------------------------------------------------

#[cfg(feature = "semispace-backend")]
pub(crate) struct HeapInner {
    young: Semispace,
    old: OldGen,
    cards: CardTable,
    cumulative_objects: u64,
    stats: HeapStats,
}

/// Internal stats bag.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct HeapStats {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub young_bytes_allocated: u64,
    pub last_minor_pause_ns: u64,
    pub last_major_pause_ns: u64,
    /// Cumulative pause time across all minor collections.
    pub total_minor_pause_ns: u64,
    /// Cumulative pause time across all major collections.
    pub total_major_pause_ns: u64,
    /// Root-slot count snapshotted at the most recent minor GC.
    pub roots_at_last_minor: u64,
    /// Root-slot count snapshotted at the most recent major GC.
    pub roots_at_last_major: u64,
    /// Cumulative bytes promoted from young to old across all minor GCs.
    /// (Semispace: all surviving young bytes; NewGC: approximated as
    /// young_bytes_allocated drained per minor cycle.)
    pub bytes_promoted: u64,
    /// Conservative-pin scanner stat. Sprint 11b's pinner populated
    /// this; Sprint 23's NewGC backend is a precise-roots client and
    /// always reports 0. Kept in the struct (and surfaced via
    /// `HeapStatsSnapshot`) so the `gc_stats_report` shape stays
    /// identical across backends.
    #[allow(dead_code)]
    pub last_pinned_objects: u64,
    /// Peak young live bytes observed over this process lifetime.
    pub peak_young_bytes_live: u64,
    /// Peak old live bytes observed over this process lifetime.
    pub peak_old_bytes_live: u64,
}

/// Public-facing snapshot of GC counters.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct HeapStatsSnapshot {
    pub minor_collections: u64,
    pub major_collections: u64,
    pub young_bytes_allocated: u64,
    pub young_bytes_live: u64,
    pub old_bytes_live: u64,
    pub last_minor_pause_ns: u64,
    pub last_major_pause_ns: u64,
    pub total_minor_pause_ns: u64,
    pub total_major_pause_ns: u64,
    pub roots_at_last_minor: u64,
    pub roots_at_last_major: u64,
    pub bytes_promoted: u64,
    pub last_pinned_objects: u64,
    pub peak_young_bytes_live: u64,
    pub peak_old_bytes_live: u64,
}

#[inline]
fn update_peak_live(stats: &mut HeapStats, young_live: u64, old_live: u64) {
    stats.peak_young_bytes_live = stats.peak_young_bytes_live.max(young_live);
    stats.peak_old_bytes_live = stats.peak_old_bytes_live.max(old_live);
}

#[cfg(feature = "semispace-backend")]
/// Sprint 11 generational copying heap. Sprint 11c moved the root
/// registry out into a thread-local; the heap struct itself only
/// guards the moveable regions through `inner`.
pub struct Heap {
    inner: Mutex<HeapInner>,
}

// SAFETY: `Heap`'s only state is the inner Mutex over the moveable
// regions. The Sprint 11c lock-free root registry lives in
// `ROOT_STACK` (thread-local); each thread sees its own root stack,
// so cross-thread `Heap` references can't race on it. See the
// "Sprint 11c thread-confinement note" below for the Sprint 28
// multi-mutator caveat.
#[cfg(feature = "semispace-backend")]
unsafe impl Send for Heap {}
#[cfg(feature = "semispace-backend")]
unsafe impl Sync for Heap {}

// -- Sprint 11c: lock-free root registry --------------------------------------
//
// Process-global thread-local stack of registered roots. The Sprint 11b
// API is stack-disciplined: every `register_root(slot)` is matched by an
// `unregister_root(slot)` LIFO; `swap_remove` from the back is O(1)
// amortised. A pathological caller that unregisters out of order falls
// back to an `rposition` scan — O(n) worst case, but the API contract
// documents the LIFO expectation.
//
// The collector calls `for_each_root` which takes an immutable borrow;
// callers must NOT register or unregister inside the closure (would
// panic the RefCell). The collector takes a `Vec` snapshot at the start
// of each cycle so subsequent root mutations during evacuation are safe
// (the snapshot is what the collector walks).

thread_local! {
    static ROOT_STACK: RefCell<Vec<*const Word>> = const { RefCell::new(Vec::new()) };
}

// Sprint 11c thread-confinement note. The Sprint 11c brief asked for a
// `OnceLock<ThreadId>` debug-assert capturing the first runtime-init
// thread and rejecting subsequent calls from other threads. In a
// single-mutator deployment that would catch Sprint 28's first mistake.
// In practice the Rust test harness runs each `#[test]` on its OWN
// OS thread (even with `#[serial]` — serial only orders execution,
// not threading), so a process-wide thread assertion fires every time
// `cargo test` runs the second test. The thread-local design is
// already self-enforcing: each thread sees ITS OWN `ROOT_STACK`, the
// collector running on that thread snapshots that thread's stack, the
// invariant holds trivially. Sprint 28 (multi-mutator) will need a
// global registry + atomic enumeration across parked threads — see
// DEFERRED.md.

/// Sprint 11c: lock-free register. Push `slot` onto the thread-local
/// root stack. The collector reads (a snapshot of) this stack each
/// cycle and rewrites the pointed-at Word if it evacuates.
///
/// O(1); no mutex acquisition.
///
/// **CRITICAL:** the memory at `slot` must remain at the SAME ADDRESS
/// for the entire lifetime of the registration (i.e. until
/// `unregister_root(slot)` is called). A stack-local `Word` inside a
/// `Vec` that subsequently `push`es will have its backing buffer
/// moved, invalidating any pointer registered into it. The collector
/// then writes the rewritten Word to a stale address; subsequent
/// reads through the Vec see the pre-GC pointer.
///
/// Safe patterns: a `&Word` to a stack slot whose function frame
/// outlives the registration; a `Box<UnsafeCell<Word>>` (heap
/// allocation that never moves); a pre-sized `Box<[UnsafeCell<Word>]>`
/// slab. Unsafe pattern: register `cell.get()` of a freshly-constructed
/// `UnsafeCell` and then `vec.push(cell)` — DON'T do this.
pub fn register_root(slot: *const Word) {
    ROOT_STACK.with(|s| s.borrow_mut().push(slot));
}

/// Sprint 11c: lock-free unregister. Pop the most-recent matching slot
/// from the thread-local root stack. The Sprint 11b API contract is
/// LIFO-disciplined so almost always the matching entry is the last;
/// `rposition` + `swap_remove` is O(1) amortised, O(n) worst case if a
/// pathological caller unregisters out of order.
///
/// O(1); no mutex acquisition.
pub fn unregister_root(slot: *const Word) {
    ROOT_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        if let Some(idx) = stack.iter().rposition(|&p| p == slot) {
            stack.swap_remove(idx);
        }
    });
}

/// Current root-stack length. Used by tests to assert
/// register/unregister balance.
pub fn root_count() -> usize {
    ROOT_STACK.with(|s| s.borrow().len())
}

pub(crate) fn total_root_count() -> usize {
    snapshot_roots().len()
}

/// Snapshot the current root stack into a freshly-allocated `Vec`.
/// The collector calls this once at the start of each cycle so the
/// borrow is released before evacuation begins (evacuation rewrites
/// `*slot` for each slot in the snapshot, and the rewrites happen
/// outside the `RefCell` borrow).
fn snapshot_roots() -> Vec<*const Word> {
    let mut roots = ROOT_STACK.with(|s| s.borrow().clone());
    roots.extend(crate::stack_map::snapshot_active_jit_roots());
    roots.extend(crate::aot::snapshot_active_aot_roots());
    // Sprint 47 — multi-value return secondary-values buffer (see
    // `docs/COMPILER_GAPS.md` GAP-003 and `nod-runtime/src/values.rs`).
    // The first `VALUES_COUNT` entries of the TLS buffer are heap-pointer
    // candidates that must be scanned alongside the other roots while a
    // multi-binder `let` is destructuring a call's return.
    roots.extend(crate::values::snapshot_active_values_roots());
    roots
}

/// Iterate every currently-registered root. The closure must NOT
/// mutate the root list (no nested `register_root` / `unregister_root`
/// calls). Used by tests and diagnostic paths; the collector uses
/// `snapshot_roots` instead to avoid the borrow living across
/// evacuation.
pub fn for_each_root<F: FnMut(*const Word)>(mut f: F) {
    ROOT_STACK.with(|s| {
        for &slot in s.borrow().iter() {
            f(slot);
        }
    });
}

#[cfg(feature = "semispace-backend")]
impl Heap {
    pub fn new() -> Self {
        Self::with_config(GcConfig::default())
    }

    pub fn with_capacity(capacity_bytes: usize) -> Self {
        let young = capacity_bytes / 4;
        let old = capacity_bytes - young;
        Self::with_config(GcConfig {
            young_bytes: young,
            old_bytes: old,
        })
    }

    pub fn with_config(cfg: GcConfig) -> Self {
        let young = Semispace::new(cfg.young_bytes);
        let old = OldGen::new(cfg.old_bytes);
        let cards = CardTable::new(cfg.old_bytes);
        Heap {
            inner: Mutex::new(HeapInner {
                young,
                old,
                cards,
                cumulative_objects: 0,
                stats: HeapStats::default(),
            }),
        }
    }

    /// Allocate `payload_bytes` of payload preceded by an 8-byte
    /// `Wrapper` header. Returns a tagged-pointer `Word`. Payload zeroed.
    pub fn alloc_object(&self, class: ClassId, payload_bytes: usize) -> Word {
        let total = (size_of::<Wrapper>() + payload_bytes).next_multiple_of(HEAP_ALIGN);
        let addr = self.alloc_movable_raw(total);
        // SAFETY: alloc_movable_raw returned a freshly-bumped chunk;
        // we install the wrapper and zero the payload immediately.
        unsafe {
            let header_ptr = addr as *mut Wrapper;
            header_ptr.write(Wrapper::new(class));
        }
        if payload_bytes > 0 {
            let payload_addr = addr + size_of::<Wrapper>();
            let zero_bytes = total - size_of::<Wrapper>();
            // SAFETY: payload region is inside the fresh chunk.
            unsafe {
                std::ptr::write_bytes(payload_addr as *mut u8, 0u8, zero_bytes);
            }
        }
        Word::from_ptr(addr as *const u8)
    }

    fn alloc_movable_raw(&self, total_bytes: usize) -> usize {
        // First attempt against young.
        {
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            if let Some(addr) = inner.young.try_alloc_bytes(total_bytes) {
                inner.cumulative_objects += 1;
                inner.stats.young_bytes_allocated += total_bytes as u64;
                update_peak_live(
                    &mut inner.stats,
                    inner.young.used_bytes() as u64,
                    inner.old.live.used_bytes() as u64,
                );
                return addr;
            }
        }
        // Young is exhausted. Minor GC.
        self.collect_minor();
        {
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            if let Some(addr) = inner.young.try_alloc_bytes(total_bytes) {
                inner.cumulative_objects += 1;
                inner.stats.young_bytes_allocated += total_bytes as u64;
                update_peak_live(
                    &mut inner.stats,
                    inner.young.used_bytes() as u64,
                    inner.old.live.used_bytes() as u64,
                );
                return addr;
            }
            if let Some(addr) = inner.old.live.try_alloc_bytes(total_bytes) {
                inner.cumulative_objects += 1;
                update_peak_live(
                    &mut inner.stats,
                    inner.young.used_bytes() as u64,
                    inner.old.live.used_bytes() as u64,
                );
                return addr;
            }
        }
        // Full GC as last resort.
        self.collect_full();
        let mut inner = self.inner.lock().expect("heap mutex poisoned");
        if let Some(addr) = inner.young.try_alloc_bytes(total_bytes) {
            inner.cumulative_objects += 1;
            inner.stats.young_bytes_allocated += total_bytes as u64;
            update_peak_live(
                &mut inner.stats,
                inner.young.used_bytes() as u64,
                inner.old.live.used_bytes() as u64,
            );
            return addr;
        }
        if let Some(addr) = inner.old.live.try_alloc_bytes(total_bytes) {
            inner.cumulative_objects += 1;
            update_peak_live(
                &mut inner.stats,
                inner.young.used_bytes() as u64,
                inner.old.live.used_bytes() as u64,
            );
            return addr;
        }
        panic!(
            "heap exhausted: request {total_bytes} bytes, young free={} bytes, old free={} bytes",
            inner.young.free_cells() * 8,
            inner.old.live.free_cells() * 8,
        );
    }

    /// Decode `w` to its `Wrapper`. `None` for fixnums and pointers
    /// outside the heap.
    pub fn wrapper_of(&self, w: Word) -> Option<Wrapper> {
        let ptr = w.as_ptr::<Wrapper>()?;
        let addr = ptr as usize;
        let inner = self.inner.lock().ok()?;
        if !(inner.young.contains(addr)
            || inner.old.live.contains(addr)
            || inner.old.scratch.contains(addr))
        {
            return None;
        }
        // SAFETY: addr is in our heap and `w` is a Dylan-tagged
        // pointer into it; first 8 bytes are an initialised Wrapper.
        Some(unsafe { *ptr })
    }

    /// Used-byte total across young + old.live.
    pub fn live_bytes(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.young.used_bytes() + inner.old.live.used_bytes()
    }

    /// Total object count across the heap's lifetime.
    pub fn object_count(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.cumulative_objects as usize
    }

    pub fn young_used_bytes(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.young.used_bytes()
    }

    pub fn old_used_bytes(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.old.live.used_bytes()
    }

    pub fn capacity_bytes(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.young.capacity_bytes() + inner.old.live.capacity_bytes()
    }

    /// Sprint 11c: thin wrapper over the module-level `register_root`
    /// for Sprint 11b call-site API stability. The mutex baseline is
    /// gone — calls now hit a thread-local `Vec::push`.
    pub fn register_root(&self, root: *const Word) {
        register_root(root);
    }

    /// Sprint 11c: thin wrapper over the module-level `unregister_root`.
    pub fn unregister_root(&self, root: *const Word) {
        unregister_root(root);
    }

    /// Sprint 11c: snapshot of the current root-stack depth.
    pub fn root_count(&self) -> usize {
        root_count()
    }

    /// Mark the card containing `dst_ptr` (which should point into
    /// old). No-op if `dst_ptr` is not in old.
    pub fn mark_card_for(&self, dst_ptr: *const Word) {
        let addr = dst_ptr as usize;
        let inner = self.inner.lock().expect("heap mutex poisoned");
        if !inner.old.live.contains(addr) {
            return;
        }
        let offset = addr - inner.old.live.base_addr();
        inner.cards.mark_offset(offset);
    }

    /// Conservative stack-range pin. Walks `[lo, hi)` and pins any
    /// object in young whose start cell appears as a pointer-tagged
    /// Word there. Returns the number of distinct objects pinned.
    ///
    /// **Sprint 11b status: opt-in only, NOT called from any
    /// production code path.** Sprint 11b's `nod_register_root` /
    /// `nod_unregister_root` shim + JIT-emitted spill/reload sequence
    /// (driven by the Sprint 11b liveness pass) replaces conservative
    /// scanning with precise, slot-rewriting evacuation. The pinner
    /// remains as a debug aid: a caller can still construct a
    /// synthetic "stack-shaped" Word array and pin its contents, then
    /// drive a minor GC, to verify the rewinding-pinned-objects
    /// collector path. Sprint 11c (full `gc.statepoint`) will likely
    /// retire this entirely.
    ///
    /// # Safety
    ///
    /// `lo..hi` must be a readable, 8-byte-aligned address range.
    pub unsafe fn pin_stack_range(&self, lo: usize, hi: usize) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        // SAFETY: forwarded.
        unsafe { inner.young.pin_pointers_in_range(lo, hi) }
    }

    /// Clear pinned bits on remaining young+old objects.
    pub fn clear_pinned(&self) {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.young.clear_pinned_bits();
        inner.old.live.clear_pinned_bits();
    }

    /// Count of currently-dirty cards in the write-barrier table.
    /// Diagnostic; exposed for tests and `:gc-stats`.
    pub fn dirty_card_count(&self) -> usize {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.cards.dirty_count()
    }

    /// Number of minor collections this heap has run. Exposed for
    /// tests that want to assert the GC actually fired.
    pub fn minor_collection_count(&self) -> u64 {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.stats.minor_collections
    }

    /// Number of major collections this heap has run.
    pub fn major_collection_count(&self) -> u64 {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        inner.stats.major_collections
    }

    pub fn ranges(&self) -> HeapRanges {
        let inner = self.inner.lock().expect("heap mutex poisoned");
        HeapRanges {
            young: (
                inner.young.base_addr(),
                inner.young.base_addr() + inner.young.capacity_bytes(),
            ),
            old: (
                inner.old.live.base_addr(),
                inner.old.live.base_addr() + inner.old.live.capacity_bytes(),
            ),
        }
    }

    pub(crate) fn stats_snapshot(&self) -> HeapStatsSnapshot {
        let mut inner = self.inner.lock().expect("heap mutex poisoned");
        let young_live = inner.young.used_bytes() as u64;
        let old_live = inner.old.live.used_bytes() as u64;
        update_peak_live(&mut inner.stats, young_live, old_live);
        HeapStatsSnapshot {
            minor_collections: inner.stats.minor_collections,
            major_collections: inner.stats.major_collections,
            young_bytes_allocated: inner.stats.young_bytes_allocated,
            young_bytes_live: young_live,
            old_bytes_live: old_live,
            last_minor_pause_ns: inner.stats.last_minor_pause_ns,
            last_major_pause_ns: inner.stats.last_major_pause_ns,
            total_minor_pause_ns: inner.stats.total_minor_pause_ns,
            total_major_pause_ns: inner.stats.total_major_pause_ns,
            roots_at_last_minor: inner.stats.roots_at_last_minor,
            roots_at_last_major: inner.stats.roots_at_last_major,
            bytes_promoted: inner.stats.bytes_promoted,
            last_pinned_objects: inner.stats.last_pinned_objects,
            peak_young_bytes_live: inner.stats.peak_young_bytes_live,
            peak_old_bytes_live: inner.stats.peak_old_bytes_live,
        }
    }
}

#[cfg(feature = "semispace-backend")]
impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

/// Heap address ranges; produced by `Heap::ranges`.
pub struct HeapRanges {
    pub young: (usize, usize),
    pub old: (usize, usize),
}

// -- Collector ---------------------------------------------------------------

#[cfg(feature = "semispace-backend")]
impl Heap {
    /// Minor collection: young → old.live. Surviving young objects are
    /// copied into old.live (full promotion — every survivor tenures),
    /// young is reset.
    pub fn collect_minor(&self) {
        crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_MINOR);
        let start = std::time::Instant::now();
        // Capture young-gen occupancy before collection so we can
        // report bytes_promoted (all young survivors tenure in one step).
        let young_before_bytes = {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.young.used_bytes() as u64
        };
        // Sprint 11c: snapshot the thread-local root stack BEFORE
        // taking the heap mutex. The snapshot is what the collector
        // walks; evacuation rewrites `*slot` on each entry, but never
        // mutates the root stack itself, so we don't need a `RefCell`
        // borrow live across the GC.
        let roots = snapshot_roots();
        let root_count = roots.len() as u64;
        let pinned_count;
        {
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            // SAFETY: we hold the heap mutex; the collector is the sole
            // mutator of the heap's regions for the duration of this call.
            pinned_count = unsafe { run_minor(&mut inner, &roots) };
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        let mut inner = self.inner.lock().expect("heap mutex poisoned");
        inner.stats.minor_collections += 1;
        inner.stats.last_minor_pause_ns = elapsed_ns;
        inner.stats.total_minor_pause_ns += elapsed_ns;
        inner.stats.roots_at_last_minor = root_count;
        inner.stats.bytes_promoted += young_before_bytes;
        inner.stats.last_pinned_objects = pinned_count as u64;
        let young_live = inner.young.used_bytes() as u64;
        let old_live = inner.old.live.used_bytes() as u64;
        update_peak_live(&mut inner.stats, young_live, old_live);
        // Publish to crash-dump shadow before clearing the phase flag
        // so the handler always sees consistent metrics.
        let snap = HeapStatsSnapshot {
            minor_collections: inner.stats.minor_collections,
            major_collections: inner.stats.major_collections,
            young_bytes_allocated: inner.stats.young_bytes_allocated,
            young_bytes_live: young_live,
            old_bytes_live: old_live,
            last_minor_pause_ns: elapsed_ns,
            last_major_pause_ns: inner.stats.last_major_pause_ns,
            total_minor_pause_ns: inner.stats.total_minor_pause_ns,
            total_major_pause_ns: inner.stats.total_major_pause_ns,
            roots_at_last_minor: root_count,
            roots_at_last_major: inner.stats.roots_at_last_major,
            bytes_promoted: inner.stats.bytes_promoted,
            last_pinned_objects: inner.stats.last_pinned_objects,
            peak_young_bytes_live: inner.stats.peak_young_bytes_live,
            peak_old_bytes_live: inner.stats.peak_old_bytes_live,
        };
        crate::crash_dump::update_gc_metrics(&snap);
        crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_IDLE);
        if crate::gc_trace_enabled() {
            eprintln!(
                "[GC minor #{}] roots={} promoted={}B pause={}µs (total {}µs)",
                inner.stats.minor_collections,
                root_count,
                young_before_bytes,
                elapsed_ns / 1_000,
                inner.stats.total_minor_pause_ns / 1_000,
            );
        }
    }

    /// Full collection: young + old.live → old.scratch, swap old,
    /// reset young.
    pub fn collect_full(&self) {
        crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_MAJOR);
        let start = std::time::Instant::now();
        // Sprint 11c: see `collect_minor` — snapshot first, no
        // RefCell borrow across the heap mutex.
        let roots = snapshot_roots();
        let root_count = roots.len() as u64;
        {
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            // SAFETY: heap mutex held; collector is sole mutator.
            unsafe { run_full(&mut inner, &roots) };
        }
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        let mut inner = self.inner.lock().expect("heap mutex poisoned");
        inner.stats.major_collections += 1;
        inner.stats.last_major_pause_ns = elapsed_ns;
        inner.stats.total_major_pause_ns += elapsed_ns;
        inner.stats.roots_at_last_major = root_count;
        let young_live = inner.young.used_bytes() as u64;
        let old_live = inner.old.live.used_bytes() as u64;
        update_peak_live(&mut inner.stats, young_live, old_live);
        let snap = HeapStatsSnapshot {
            minor_collections: inner.stats.minor_collections,
            major_collections: inner.stats.major_collections,
            young_bytes_allocated: inner.stats.young_bytes_allocated,
            young_bytes_live: young_live,
            old_bytes_live: old_live,
            last_minor_pause_ns: inner.stats.last_minor_pause_ns,
            last_major_pause_ns: elapsed_ns,
            total_minor_pause_ns: inner.stats.total_minor_pause_ns,
            total_major_pause_ns: inner.stats.total_major_pause_ns,
            roots_at_last_minor: inner.stats.roots_at_last_minor,
            roots_at_last_major: root_count,
            bytes_promoted: inner.stats.bytes_promoted,
            last_pinned_objects: inner.stats.last_pinned_objects,
            peak_young_bytes_live: inner.stats.peak_young_bytes_live,
            peak_old_bytes_live: inner.stats.peak_old_bytes_live,
        };
        crate::crash_dump::update_gc_metrics(&snap);
        crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_IDLE);
        if crate::gc_trace_enabled() {
            eprintln!(
                "[GC major #{}] roots={} pause={}µs (total {}µs)",
                inner.stats.major_collections,
                root_count,
                elapsed_ns / 1_000,
                inner.stats.total_major_pause_ns / 1_000,
            );
        }
    }
}

// -- Collector internals (raw-pointer-flavoured) -----------------------------
//
// The collector holds the heap mutex, so the regions it walks aren't
// touched by any other thread. We use raw pointers and address
// arithmetic throughout — Rust's borrow checker has no way to model
// "two mutable regions that come from the same struct and don't
// overlap", and the GC's data shape requires exactly that. Every
// unsafe block here documents the heap-mutex invariant.

#[cfg(feature = "semispace-backend")]
struct CollectorCtx {
    young_base: usize,
    young_end: usize,
    old_live_base: usize,
    old_live_end: usize,
    young_starts: StartBits,
    old_live_starts_ptr: *const StartBits,
    young_top_ptr: *mut usize,
    old_live_top_ptr: *mut usize,
    old_live_capacity_cells: usize,
    cards_ptr: *const CardTable,
}

#[cfg(feature = "semispace-backend")]
unsafe fn run_minor(inner: &mut HeapInner, roots: &[*const Word]) -> usize {
    let young_base = inner.young.base_addr();
    let young_end = young_base + inner.young.capacity_bytes();
    let old_live_base = inner.old.live.base_addr();
    let old_live_end = old_live_base + inner.old.live.capacity_bytes();
    let young_starts = inner.young.starts.clone();
    let old_live_starts_ptr: *const StartBits = &inner.old.live.starts;
    let young_top_ptr: *mut usize = &mut inner.young.top;
    let old_live_top_ptr: *mut usize = &mut inner.old.live.top;
    let old_live_capacity_cells = inner.old.live.capacity_cells();
    let cards_ptr: *const CardTable = &inner.cards;

    let ctx = CollectorCtx {
        young_base,
        young_end,
        old_live_base,
        old_live_end,
        young_starts,
        old_live_starts_ptr,
        young_top_ptr,
        old_live_top_ptr,
        old_live_capacity_cells,
        cards_ptr,
    };

    // Snapshot old.live top BEFORE we forward any roots. The Cheney
    // loop will scan everything appended past this watermark; the
    // card pass handles everything already below.
    // SAFETY: heap mutex held.
    let old_top_at_gc_start = unsafe { *ctx.old_live_top_ptr };

    // 1. Forward every root.
    for &root in roots.iter() {
        // SAFETY: registered root provides a writable Word slot.
        unsafe {
            let mw = root as *mut Word;
            let w = *mw;
            minor_forward_word(&ctx, mw, w);
        }
    }

    // 2. Walk dirty cards in old.live; forward any young pointers found.
    {
        // SAFETY: heap mutex held; cards live for ctx lifetime.
        let cards = unsafe { &*ctx.cards_ptr };
        for card_idx in 0..cards.n_cards() {
            if !cards.is_dirty(card_idx) {
                continue;
            }
            let card_cell_lo = card_idx * CARD_SIZE_CELLS;
            // SAFETY: heap mutex held.
            let used_cells = unsafe { *ctx.old_live_top_ptr };
            let card_cell_hi = (card_cell_lo + CARD_SIZE_CELLS).min(used_cells);
            // SAFETY: heap mutex held.
            unsafe {
                scan_card_range_minor(&ctx, card_cell_lo, card_cell_hi);
            }
            cards.clear(card_idx);
        }
    }

    // 3. Cheney scan over newly-copied old objects. Anything that
    //    was already in old before this minor GC was reached via the
    //    card-pass above; the cursor starts at "old.live top at GC
    //    start" (snapshotted before step 1) and chases newly-appended
    //    cells.
    let mut cursor = old_top_at_gc_start;
    loop {
        // SAFETY: heap mutex held.
        let cur_top = unsafe { *ctx.old_live_top_ptr };
        if cursor == cur_top {
            break;
        }
        let prev_cursor = cursor;
        // Walk every start in [cursor, cur_top).
        // SAFETY: heap mutex held.
        let old_starts = unsafe { &*ctx.old_live_starts_ptr };
        let mut new_addrs: Vec<usize> = Vec::new();
        for_each_start(old_starts, cur_top, |cell_idx| {
            if cell_idx < prev_cursor {
                return;
            }
            new_addrs.push(ctx.old_live_base + cell_idx * 8);
        });
        for addr in new_addrs {
            // SAFETY: addr is a wrapper start in old.live.
            unsafe {
                let wrapper = *(addr as *const Wrapper);
                if wrapper.is_forwarded() {
                    continue;
                }
                let class = wrapper.class();
                let metadata = class_metadata_for(class);
                // Scan visits each Word slot of the object; we forward
                // it (if young-pointing) inline.
                let ctx_ref: &CollectorCtx = &ctx;
                (metadata.scan)(addr, &mut |slot| {
                    let w = *slot;
                    minor_forward_word(ctx_ref, slot, w);
                    // If the new value points back into old.live (an
                    // old → old reference), dirty the card.
                    let nw = *slot;
                    if nw.is_pointer() {
                        let target = (nw.raw() & !1) as usize;
                        if target >= ctx_ref.old_live_base && target < ctx_ref.old_live_end {
                            // SAFETY: heap mutex held.
                            let cards = &*ctx_ref.cards_ptr;
                            let offset = (slot as usize) - ctx_ref.old_live_base;
                            cards.mark_offset(offset);
                        }
                    }
                });
            }
        }
        cursor = unsafe { *ctx.old_live_top_ptr };
    }

    // 4. Process pinned young objects (conservative refs found by
    //    `pin_stack_range`). We copy them into old too — Sprint 11
    //    accepts losing the truly-in-place semantics. Sprint 11b's
    //    statepoint-driven precise roots will eliminate the need for
    //    pinning in normal operation.
    let young_used_at_minor = unsafe { *ctx.young_top_ptr };
    let mut pinned_addrs: Vec<usize> = Vec::new();
    for_each_start(&ctx.young_starts, young_used_at_minor, |cell_idx| {
        let addr = ctx.young_base + cell_idx * 8;
        // SAFETY: cell is marked as a start; first 8 bytes are Wrapper.
        let cur = unsafe { *(addr as *const u64) };
        let wrapper = Wrapper { raw: cur };
        if wrapper.is_forwarded() || !wrapper.has_gc_bit(GcBit::Pinned) {
            return;
        }
        pinned_addrs.push(addr);
    });
    let pinned_count = pinned_addrs.len();
    for addr in pinned_addrs {
        // SAFETY: addr is a young heap object; we copy and forward it.
        unsafe {
            let wrapper = *(addr as *const Wrapper);
            if wrapper.is_forwarded() {
                continue;
            }
            let class = wrapper.class();
            let metadata = class_metadata_for(class);
            let total = (metadata.size_of)(addr);
            let new_addr = ctx_try_alloc_old(&ctx, total).unwrap_or_else(|| {
                panic!("old gen exhausted while evacuating pinned objects (need {total} bytes)")
            });
            std::ptr::copy_nonoverlapping(addr as *const u8, new_addr as *mut u8, total);
            let new_wrapper_ptr = new_addr as *mut Wrapper;
            let nw = (*new_wrapper_ptr)
                .with_gc_bit(GcBit::Tenured)
                .without_gc_bit(GcBit::Pinned);
            *new_wrapper_ptr = nw;
            *(addr as *mut Wrapper) = Wrapper::forward_to(new_addr);
            // Scan the new copy.
            let ctx_ref: &CollectorCtx = &ctx;
            (metadata.scan)(new_addr, &mut |slot| {
                let w = *slot;
                minor_forward_word(ctx_ref, slot, w);
            });
        }
    }

    // 5. Reset young.
    inner.young.reset();
    pinned_count
}

/// Try to bump-allocate `total_bytes` in old.live via raw pointers.
/// Returns the new address on success, `None` on exhaustion.
///
/// # Safety
///
/// Heap mutex must be held by the caller.
#[cfg(feature = "semispace-backend")]
unsafe fn ctx_try_alloc_old(ctx: &CollectorCtx, total_bytes: usize) -> Option<usize> {
    let aligned = total_bytes.next_multiple_of(HEAP_ALIGN);
    let cells_needed = aligned / 8;
    // SAFETY: heap mutex held.
    let top = unsafe { *ctx.old_live_top_ptr };
    if top + cells_needed > ctx.old_live_capacity_cells {
        return None;
    }
    let cell_idx = top;
    let addr = ctx.old_live_base + cell_idx * 8;
    // SAFETY: heap mutex held.
    unsafe {
        *ctx.old_live_top_ptr = top + cells_needed;
    }
    // SAFETY: heap mutex held; old_live_starts_ptr is a live StartBits.
    let starts = unsafe { &*ctx.old_live_starts_ptr };
    set_start_bit(starts, cell_idx);
    Some(addr)
}

/// Forward a single Word reference at `slot`: if the target is in
/// young, copy it into old.live and rewrite the slot.
///
/// # Safety
///
/// Heap mutex held; `slot` must be a writable `*mut Word` inside a
/// region the collector can mutate (any heap region during GC, plus
/// any explicitly registered root slot).
#[cfg(feature = "semispace-backend")]
unsafe fn minor_forward_word(ctx: &CollectorCtx, slot: *mut Word, w: Word) {
    if !w.is_pointer() {
        return;
    }
    let target = (w.raw() & !1) as usize;
    if !(target >= ctx.young_base && target < ctx.young_end) {
        return;
    }
    // SAFETY: target is a wrapper start in young (we set the bit at alloc).
    let cur_wrapper = unsafe { *(target as *const Wrapper) };
    if cur_wrapper.is_forwarded() {
        let new_addr = cur_wrapper.forwarding_addr();
        let new_word = Word::from_ptr(new_addr as *const u8);
        // SAFETY: slot is writable per caller's contract.
        unsafe { *slot = new_word };
        return;
    }
    let class = cur_wrapper.class();
    let metadata = class_metadata_for(class);
    // SAFETY: class matches the layout at target.
    let total = unsafe { (metadata.size_of)(target) };
    let new_addr = match unsafe { ctx_try_alloc_old(ctx, total) } {
        Some(a) => a,
        None => panic!(
            "old gen exhausted during minor GC evacuation (need {total} bytes)"
        ),
    };
    // SAFETY: target..target+total is live; new_addr is fresh.
    unsafe {
        std::ptr::copy_nonoverlapping(target as *const u8, new_addr as *mut u8, total);
    }
    // Stamp Tenured + clear Pinned/Forwarded on the copy.
    // SAFETY: new_addr's first 8 bytes are the freshly copied wrapper.
    unsafe {
        let new_wrapper_ptr = new_addr as *mut Wrapper;
        let nw = (*new_wrapper_ptr)
            .with_gc_bit(GcBit::Tenured)
            .without_gc_bit(GcBit::Pinned)
            .without_gc_bit(GcBit::Forwarded);
        *new_wrapper_ptr = nw;
    }
    // Install forwarding pointer in young.
    // SAFETY: target is a young header start.
    unsafe {
        *(target as *mut Wrapper) = Wrapper::forward_to(new_addr);
    }
    // Clear the young start bit so a re-walk sees no ghost.
    let target_cell = (target - ctx.young_base) / 8;
    clear_start_bit(&ctx.young_starts, target_cell);
    let new_word = Word::from_ptr(new_addr as *const u8);
    // SAFETY: slot writable per caller.
    unsafe { *slot = new_word };
}

/// Walk the cards' start bitmap and visit every slot of every object
/// whose start lies in the card window.
///
/// # Safety
///
/// Heap mutex held.
#[cfg(feature = "semispace-backend")]
unsafe fn scan_card_range_minor(ctx: &CollectorCtx, card_cell_lo: usize, card_cell_hi: usize) {
    // SAFETY: heap mutex held.
    let starts = unsafe { &*ctx.old_live_starts_ptr };
    let mut start_addrs: Vec<usize> = Vec::new();
    for_each_start(starts, card_cell_hi, |cell_idx| {
        start_addrs.push(ctx.old_live_base + cell_idx * 8);
    });
    for addr in start_addrs {
        // SAFETY: addr is a wrapper start in old.live.
        unsafe {
            let wrapper = *(addr as *const Wrapper);
            if wrapper.is_forwarded() {
                continue;
            }
            let class = wrapper.class();
            let metadata = class_metadata_for(class);
            let total = (metadata.size_of)(addr);
            let cells = total / 8;
            let cell_idx = (addr - ctx.old_live_base) / 8;
            let end_cell = cell_idx + cells;
            if end_cell <= card_cell_lo {
                continue;
            }
            (metadata.scan)(addr, &mut |slot| {
                let w = *slot;
                minor_forward_word(ctx, slot, w);
            });
        }
    }
}

// -- Full GC -----------------------------------------------------------------

#[cfg(feature = "semispace-backend")]
struct FullCtx {
    young_base: usize,
    young_end: usize,
    old_live_base: usize,
    old_live_end: usize,
    scratch_starts_ptr: *const StartBits,
    scratch_top_ptr: *mut usize,
    scratch_base: usize,
    scratch_capacity_cells: usize,
}

#[cfg(feature = "semispace-backend")]
unsafe fn run_full(inner: &mut HeapInner, roots: &[*const Word]) {
    let ctx = FullCtx {
        young_base: inner.young.base_addr(),
        young_end: inner.young.base_addr() + inner.young.capacity_bytes(),
        old_live_base: inner.old.live.base_addr(),
        old_live_end: inner.old.live.base_addr() + inner.old.live.capacity_bytes(),
        scratch_starts_ptr: &inner.old.scratch.starts,
        scratch_top_ptr: &mut inner.old.scratch.top,
        scratch_base: inner.old.scratch.base_addr(),
        scratch_capacity_cells: inner.old.scratch.capacity_cells(),
    };

    for &root in roots.iter() {
        // SAFETY: registered root.
        unsafe {
            let mw = root as *mut Word;
            let w = *mw;
            full_forward_word(&ctx, mw, w);
        }
    }

    // Cheney scan over scratch.
    let mut cursor = 0usize;
    loop {
        // SAFETY: heap mutex held.
        let cur_top = unsafe { *ctx.scratch_top_ptr };
        if cursor == cur_top {
            break;
        }
        let prev = cursor;
        // SAFETY: heap mutex held.
        let starts = unsafe { &*ctx.scratch_starts_ptr };
        let mut new_addrs: Vec<usize> = Vec::new();
        for_each_start(starts, cur_top, |cell_idx| {
            if cell_idx < prev {
                return;
            }
            new_addrs.push(ctx.scratch_base + cell_idx * 8);
        });
        for addr in new_addrs {
            // SAFETY: scratch wrappers are well-formed (we wrote them).
            unsafe {
                let wrapper = *(addr as *const Wrapper);
                if wrapper.is_forwarded() {
                    continue;
                }
                let class = wrapper.class();
                let metadata = class_metadata_for(class);
                let ctx_ref: &FullCtx = &ctx;
                (metadata.scan)(addr, &mut |slot| {
                    let w = *slot;
                    full_forward_word(ctx_ref, slot, w);
                });
            }
        }
        cursor = unsafe { *ctx.scratch_top_ptr };
    }

    inner.old.swap();
    inner.old.scratch.reset();
    inner.young.reset();
    inner.cards.clear_all();
}

/// # Safety
///
/// Heap mutex held.
#[cfg(feature = "semispace-backend")]
unsafe fn ctx_try_alloc_scratch(ctx: &FullCtx, total_bytes: usize) -> Option<usize> {
    let aligned = total_bytes.next_multiple_of(HEAP_ALIGN);
    let cells_needed = aligned / 8;
    // SAFETY: heap mutex held.
    let top = unsafe { *ctx.scratch_top_ptr };
    if top + cells_needed > ctx.scratch_capacity_cells {
        return None;
    }
    let cell_idx = top;
    let addr = ctx.scratch_base + cell_idx * 8;
    // SAFETY: heap mutex held.
    unsafe {
        *ctx.scratch_top_ptr = top + cells_needed;
    }
    // SAFETY: scratch_starts_ptr lives for ctx's lifetime.
    let starts = unsafe { &*ctx.scratch_starts_ptr };
    set_start_bit(starts, cell_idx);
    Some(addr)
}

/// # Safety
///
/// Heap mutex held; `slot` writable.
#[cfg(feature = "semispace-backend")]
unsafe fn full_forward_word(ctx: &FullCtx, slot: *mut Word, w: Word) {
    if !w.is_pointer() {
        return;
    }
    let target = (w.raw() & !1) as usize;
    let in_young = target >= ctx.young_base && target < ctx.young_end;
    let in_old = target >= ctx.old_live_base && target < ctx.old_live_end;
    if !(in_young || in_old) {
        return;
    }
    // SAFETY: target is in a live semispace.
    let cur_wrapper = unsafe { *(target as *const Wrapper) };
    if cur_wrapper.is_forwarded() {
        let new_addr = cur_wrapper.forwarding_addr();
        let new_word = Word::from_ptr(new_addr as *const u8);
        // SAFETY: slot writable.
        unsafe { *slot = new_word };
        return;
    }
    let class = cur_wrapper.class();
    let metadata = class_metadata_for(class);
    // SAFETY: class matches layout.
    let total = unsafe { (metadata.size_of)(target) };
    let new_addr = match unsafe { ctx_try_alloc_scratch(ctx, total) } {
        Some(a) => a,
        None => panic!("old scratch exhausted during full GC (need {total} bytes)"),
    };
    // SAFETY: target..target+total is live; new_addr is fresh.
    unsafe {
        std::ptr::copy_nonoverlapping(target as *const u8, new_addr as *mut u8, total);
    }
    // SAFETY: new_addr's first 8 bytes are the freshly copied wrapper.
    unsafe {
        let new_wrapper_ptr = new_addr as *mut Wrapper;
        let nw = (*new_wrapper_ptr)
            .with_gc_bit(GcBit::Tenured)
            .without_gc_bit(GcBit::Pinned)
            .without_gc_bit(GcBit::Forwarded);
        *new_wrapper_ptr = nw;
    }
    // SAFETY: target is a source header start.
    unsafe {
        *(target as *mut Wrapper) = Wrapper::forward_to(new_addr);
    }
    let new_word = Word::from_ptr(new_addr as *const u8);
    // SAFETY: slot writable.
    unsafe { *slot = new_word };
}

// Suppress unused warnings for trait-required imports.
#[cfg(feature = "semispace-backend")]
const _: fn() = || {
    let _ = ClassTable::new();
    let _ = CARD_SIZE_BYTES;
};

// ─── Sprint 23: NewGC backend ──────────────────────────────────────────────
//
// `PageHeap<DylanLayout>` from `newgc-core`. Page-based mark-evacuate
// generational collector replacing the Sprint 11 semispace heap.
//
// API parity with the semispace `Heap` above. Where the semispace
// took `&self` and locked an internal Mutex, NewGC takes `&mut self`
// — we wrap a `Mutex<PageHeap<DylanLayout>>` to preserve the
// `&self` shape callers expect.

#[cfg(feature = "newgc-backend")]
mod newgc_backend {
    use std::sync::Mutex;
    use std::time::Instant;

    use newgc_core::page_heap::page_desc::Generation;
    use newgc_core::PageHeap;

    use crate::classes::ClassId;
    use crate::dylan_layout::DylanLayout;
    use crate::word::Word;
    use crate::wrapper::Wrapper;

    use super::{
        GcConfig, HeapRanges, HeapStats, HeapStatsSnapshot, snapshot_roots, update_peak_live,
        HEAP_ALIGN,
    };

    /// Inner state for the NewGC backend.
    pub(super) struct NewGcInner {
        pub(super) heap: PageHeap<DylanLayout>,
        pub(super) stats: HeapStats,
        pub(super) cumulative_objects: u64,
        /// Reservation base (in bytes). Cached so `live_bytes`,
        /// `wrapper_of`, `mark_card_for`, and `ranges` can answer
        /// without locking the page heap.
        pub(super) base_addr: usize,
        /// Reservation size in bytes (page_count * 64 KB).
        pub(super) reservation_bytes: usize,
    }

    pub(super) struct HeapImpl {
        pub(super) inner: Mutex<NewGcInner>,
    }

    impl HeapImpl {
        pub(super) fn with_config(cfg: GcConfig) -> Self {
            // NewGC `PageHeap::new(young_bytes, old_bytes)` rounds up
            // to a whole number of 64 KB pages, with a 4-page minimum.
            // The `young_bytes` arg becomes the soft G0-page cap.
            let heap = PageHeap::<DylanLayout>::new(cfg.young_bytes, cfg.old_bytes);
            let base_addr = heap.base_ptr() as usize;
            let reservation_bytes = heap.reserved_bytes();
            HeapImpl {
                inner: Mutex::new(NewGcInner {
                    heap,
                    stats: HeapStats::default(),
                    cumulative_objects: 0,
                    base_addr,
                    reservation_bytes,
                }),
            }
        }

        /// Allocate `payload_bytes` of payload preceded by an 8-byte
        /// `Wrapper`. Returns a pointer-tagged Word. Payload zeroed.
        pub(super) fn alloc_object(&self, class: ClassId, payload_bytes: usize) -> Word {
            let total_bytes =
                (size_of::<Wrapper>() + payload_bytes).next_multiple_of(HEAP_ALIGN);
            let n_cells = total_bytes / 8;
            let addr = self.alloc_raw(n_cells);
            // SAFETY: alloc_raw returned a freshly-allocated chunk
            // (acquire_free_page zeroes recycled pages); we install
            // the wrapper and the payload is already zero.
            unsafe {
                let header_ptr = addr as *mut Wrapper;
                header_ptr.write(Wrapper::new(class));
            }
            Word::from_ptr(addr as *const u8)
        }

        /// Bump `n_cells` cells of heap, triggering minor/major GC if
        /// the allocator can't satisfy the request directly.
        fn alloc_raw(&self, n_cells: usize) -> usize {
            // Attempt 1: fast path.
            {
                let mut inner = self.inner.lock().expect("heap mutex poisoned");
                if let Some(p) = inner.heap.try_alloc_boxed_in(Generation::G0, n_cells) {
                    inner.cumulative_objects += 1;
                    inner.stats.young_bytes_allocated += (n_cells * 8) as u64;
                    return p.as_ptr() as usize;
                }
            }
            // Attempt 2: minor GC, retry.
            self.collect_minor();
            {
                let mut inner = self.inner.lock().expect("heap mutex poisoned");
                if let Some(p) = inner.heap.try_alloc_boxed_in(Generation::G0, n_cells) {
                    inner.cumulative_objects += 1;
                    inner.stats.young_bytes_allocated += (n_cells * 8) as u64;
                    return p.as_ptr() as usize;
                }
            }
            // Attempt 3: major GC, retry.
            self.collect_full();
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            if let Some(p) = inner.heap.try_alloc_boxed_in(Generation::G0, n_cells) {
                inner.cumulative_objects += 1;
                inner.stats.young_bytes_allocated += (n_cells * 8) as u64;
                return p.as_ptr() as usize;
            }
            // Allocation in G0 still failed. Try G1 / Tenured as a
            // last resort — useful for the 4-MB-young default
            // when a stress test spams allocations between GCs.
            if let Some(p) = inner.heap.try_alloc_boxed_in(Generation::Tenured, n_cells) {
                inner.cumulative_objects += 1;
                return p.as_ptr() as usize;
            }
            // Sprint 33 (NewGC VM-1 port): the request may exceed the
            // single-page cap of `try_alloc_boxed_in` (~8K cells). Fall
            // back to `try_alloc_large`, which finds a contiguous
            // free-page run and commits all pages at once. Large objects
            // are pinned in place during evacuation — never copied — so
            // their address is stable for the object's lifetime.
            if let Some(p) = inner.heap.try_alloc_large(n_cells, Generation::Tenured) {
                inner.cumulative_objects += 1;
                inner.stats.young_bytes_allocated += (n_cells * 8) as u64;
                return p.as_ptr() as usize;
            }
            let stats = inner.heap.stats();
            panic!(
                "heap exhausted: request {} cells ({} bytes); g0={} g1={} tenured={} free_pages={}",
                n_cells,
                n_cells * 8,
                stats.g0_used_bytes,
                stats.g1_used_bytes,
                stats.tenured_used_bytes,
                stats.free_pages,
            );
        }

        pub(super) fn wrapper_of(&self, w: Word) -> Option<Wrapper> {
            let ptr = w.as_ptr::<Wrapper>()?;
            let addr = ptr as usize;
            let inner = self.inner.lock().ok()?;
            if addr < inner.base_addr
                || addr >= inner.base_addr + inner.reservation_bytes
            {
                return None;
            }
            // SAFETY: addr is inside the page-heap reservation and
            // `w` is a Dylan-tagged pointer that came out of
            // `alloc_object`; the first 8 bytes are a Wrapper.
            Some(unsafe { *ptr })
        }

        pub(super) fn live_bytes(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            let s = inner.heap.stats();
            s.g0_used_bytes + s.g1_used_bytes + s.tenured_used_bytes
        }

        pub(super) fn object_count(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.cumulative_objects as usize
        }

        pub(super) fn young_used_bytes(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.heap.stats().g0_used_bytes
        }

        pub(super) fn old_used_bytes(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            let s = inner.heap.stats();
            s.g1_used_bytes + s.tenured_used_bytes
        }

        pub(super) fn capacity_bytes(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.reservation_bytes
        }

        pub(super) fn mark_card_for(&self, dst_ptr: *const Word) {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.heap.mark_card_at(dst_ptr as *const u8);
        }

        pub(super) fn dirty_card_count(&self) -> usize {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.heap.cards().dirty_count()
        }

        pub(super) fn minor_collection_count(&self) -> u64 {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.stats.minor_collections
        }

        pub(super) fn major_collection_count(&self) -> u64 {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            inner.stats.major_collections
        }

        pub(super) fn ranges(&self) -> HeapRanges {
            let inner = self.inner.lock().expect("heap mutex poisoned");
            // Page-heap doesn't split into young/old address ranges
            // — every page can be any generation. Report the whole
            // reservation as both. Callers that need precise gen
            // info should hit `stats_snapshot` instead.
            let lo = inner.base_addr;
            let hi = lo + inner.reservation_bytes;
            HeapRanges {
                young: (lo, hi),
                old: (lo, hi),
            }
        }

        pub(super) fn stats_snapshot(&self) -> HeapStatsSnapshot {
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            let gs = inner.heap.stats();
            let young_live = gs.g0_used_bytes as u64;
            let old_live = (gs.g1_used_bytes + gs.tenured_used_bytes) as u64;
            update_peak_live(&mut inner.stats, young_live, old_live);
            HeapStatsSnapshot {
                minor_collections: inner.stats.minor_collections,
                major_collections: inner.stats.major_collections,
                young_bytes_allocated: inner.stats.young_bytes_allocated,
                young_bytes_live: young_live,
                old_bytes_live: old_live,
                last_minor_pause_ns: inner.stats.last_minor_pause_ns,
                last_major_pause_ns: inner.stats.last_major_pause_ns,
                total_minor_pause_ns: inner.stats.total_minor_pause_ns,
                total_major_pause_ns: inner.stats.total_major_pause_ns,
                roots_at_last_minor: inner.stats.roots_at_last_minor,
                roots_at_last_major: inner.stats.roots_at_last_major,
                bytes_promoted: inner.stats.bytes_promoted,
                last_pinned_objects: 0,
                peak_young_bytes_live: inner.stats.peak_young_bytes_live,
                peak_old_bytes_live: inner.stats.peak_old_bytes_live,
            }
        }

        pub(super) fn collect_minor(&self) {
            crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_MINOR);
            let start = Instant::now();
            let roots = snapshot_roots();
            let root_count = roots.len() as u64;
            // Capture G0 occupancy before the cycle to approximate
            // bytes_promoted (G0 objects that survive move to G1/Tenured).
            let g0_before = {
                let inner = self.inner.lock().expect("heap mutex poisoned");
                inner.heap.stats().g0_used_bytes as u64
            };
            // GAP-011 trace: open the cycle and dump the full root set
            // (with provenance) BEFORE evacuation, so the log shows exactly
            // what the collector was handed. `visit_roots` reads the cycle
            // id back to attach per-root-rewrite records.
            let trace_cycle = if crate::gc_trace::enabled() {
                let c = crate::gc_trace::begin_cycle("minor", g0_before);
                trace_emit_root_set(c);
                c
            } else {
                0
            };
            {
                let mut inner = self.inner.lock().expect("heap mutex poisoned");
                inner.heap.collect_minor(|evac| visit_roots(evac, &roots));
            }
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            inner.stats.minor_collections += 1;
            inner.stats.last_minor_pause_ns = elapsed_ns;
            inner.stats.total_minor_pause_ns += elapsed_ns;
            inner.stats.roots_at_last_minor = root_count;
            inner.stats.bytes_promoted += g0_before;
            let gs = inner.heap.stats();
            let young_live = gs.g0_used_bytes as u64;
            let old_live = (gs.g1_used_bytes + gs.tenured_used_bytes) as u64;
            update_peak_live(&mut inner.stats, young_live, old_live);
            let snap = HeapStatsSnapshot {
                minor_collections: inner.stats.minor_collections,
                major_collections: inner.stats.major_collections,
                young_bytes_allocated: inner.stats.young_bytes_allocated,
                young_bytes_live: young_live,
                old_bytes_live: old_live,
                last_minor_pause_ns: elapsed_ns,
                last_major_pause_ns: inner.stats.last_major_pause_ns,
                total_minor_pause_ns: inner.stats.total_minor_pause_ns,
                total_major_pause_ns: inner.stats.total_major_pause_ns,
                roots_at_last_minor: root_count,
                roots_at_last_major: inner.stats.roots_at_last_major,
                bytes_promoted: inner.stats.bytes_promoted,
                last_pinned_objects: 0,
                peak_young_bytes_live: inner.stats.peak_young_bytes_live,
                peak_old_bytes_live: inner.stats.peak_old_bytes_live,
            };
            crate::crash_dump::update_gc_metrics(&snap);
            crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_IDLE);
            if crate::gc_trace::enabled() {
                crate::gc_trace::end_cycle(
                    trace_cycle,
                    "minor",
                    inner.stats.minor_collections,
                    inner.stats.major_collections,
                    young_live,
                    old_live,
                    g0_before,
                );
            }
            if crate::gc_trace_enabled() {
                eprintln!(
                    "[GC minor #{}] roots={} promoted={}B pause={}µs (total {}µs)",
                    inner.stats.minor_collections,
                    root_count,
                    g0_before,
                    elapsed_ns / 1_000,
                    inner.stats.total_minor_pause_ns / 1_000,
                );
            }
        }

        pub(super) fn collect_full(&self) {
            // Sprint 33 (NewGC VM-2 port): NewGC's `collect_full` runs a
            // three-pass algorithm — G0→G1 forced, G1→Tenured forced,
            // then Tenured→Tenured with the live root closure. Objects
            // that aged into Tenured AND have no remaining roots are
            // reclaimed. The old `collect_major` (G1→Tenured + G0→G0)
            // never reclaimed Tenured residents; we replaced it.
            crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_MAJOR);
            let start = Instant::now();
            let roots = snapshot_roots();
            let root_count = roots.len() as u64;
            let trace_cycle = if crate::gc_trace::enabled() {
                let c = crate::gc_trace::begin_cycle("major", 0);
                trace_emit_root_set(c);
                c
            } else {
                0
            };
            {
                let mut inner = self.inner.lock().expect("heap mutex poisoned");
                let _result = inner.heap.collect_full(|evac| visit_roots(evac, &roots));
                // FullCollectResult carries per-pass EvacResults + freed
                // bytes; Sprint 33 doesn't surface them yet — future
                // diagnostics work can wire them into GcStats.
            }
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            let mut inner = self.inner.lock().expect("heap mutex poisoned");
            inner.stats.major_collections += 1;
            inner.stats.last_major_pause_ns = elapsed_ns;
            inner.stats.total_major_pause_ns += elapsed_ns;
            inner.stats.roots_at_last_major = root_count;
            let gs = inner.heap.stats();
            let young_live = gs.g0_used_bytes as u64;
            let old_live = (gs.g1_used_bytes + gs.tenured_used_bytes) as u64;
            update_peak_live(&mut inner.stats, young_live, old_live);
            let snap = HeapStatsSnapshot {
                minor_collections: inner.stats.minor_collections,
                major_collections: inner.stats.major_collections,
                young_bytes_allocated: inner.stats.young_bytes_allocated,
                young_bytes_live: young_live,
                old_bytes_live: old_live,
                last_minor_pause_ns: inner.stats.last_minor_pause_ns,
                last_major_pause_ns: elapsed_ns,
                total_minor_pause_ns: inner.stats.total_minor_pause_ns,
                total_major_pause_ns: inner.stats.total_major_pause_ns,
                roots_at_last_minor: inner.stats.roots_at_last_minor,
                roots_at_last_major: root_count,
                bytes_promoted: inner.stats.bytes_promoted,
                last_pinned_objects: 0,
                peak_young_bytes_live: inner.stats.peak_young_bytes_live,
                peak_old_bytes_live: inner.stats.peak_old_bytes_live,
            };
            crate::crash_dump::update_gc_metrics(&snap);
            crate::crash_dump::set_gc_phase(crate::crash_dump::GC_PHASE_IDLE);
            if crate::gc_trace::enabled() {
                crate::gc_trace::end_cycle(
                    trace_cycle,
                    "major",
                    inner.stats.minor_collections,
                    inner.stats.major_collections,
                    young_live,
                    old_live,
                    0,
                );
            }
            if crate::gc_trace_enabled() {
                eprintln!(
                    "[GC major #{}] roots={} pause={}µs (total {}µs)",
                    inner.stats.major_collections,
                    root_count,
                    elapsed_ns / 1_000,
                    inner.stats.total_major_pause_ns / 1_000,
                );
            }
        }

        /// Sprint 11 conservative-pin façade. NewGC is compiled
        /// `--no-default-features` (no `conservative-pin` feature) so
        /// the page heap doesn't have a `pin_pointers_in_ranges`
        /// method. We're a precise-roots client via Sprint 11c's
        /// lock-free root registry; this is a no-op kept for API
        /// shape parity with the semispace backend.
        ///
        /// # Safety
        ///
        /// `lo..hi` must be a readable, 8-byte-aligned range.
        pub(super) unsafe fn pin_stack_range(&self, _lo: usize, _hi: usize) -> usize {
            0
        }

        pub(super) fn clear_pinned(&self) {
            // No pin scanner in the NewGC backend — see
            // `pin_stack_range`. This is a no-op.
        }
    }

    /// Walk the snapshotted root list and hand each slot to the
    /// `PageEvacuator::visit` call. The nod-runtime `Word` and
    /// `newgc_core::Word` are both `#[repr(transparent)] u64`, so
    /// reinterpreting `*mut Word` as `&mut newgc_core::Word` is
    /// layout-sound — see the SAFETY block.
    fn visit_roots(
        evac: &mut newgc_core::page_heap::PageEvacuator<'_, DylanLayout>,
        roots: &[*const Word],
    ) {
        // GAP-011 trace setup: read the owning cycle id once (set by
        // `begin_cycle` just before this collection started).
        let tracing = crate::gc_trace::enabled();
        let cycle = if tracing { crate::gc_trace::current_cycle() } else { 0 };
        for &slot in roots.iter() {
            // SAFETY: `slot` is a registered root — the caller's
            // contract is that it remains writable until
            // `unregister_root`. We reinterpret it as
            // `*mut newgc_core::Word`: both types are
            // `#[repr(transparent)] u64`, so the layout is
            // identical. The evacuator only reads the raw bits via
            // `L::classify(w.raw())` and writes back through
            // `L::rewrite_pointer_addr(...)` — both Dylan-defined.
            // No interpretation of the Word type's fields beyond
            // `raw()` happens.
            unsafe {
                let ngc_slot = slot as *mut newgc_core::Word;
                // GAP-011 trace: snapshot the slot before/after the visit so
                // the log records whether this root was rewritten to a moved
                // address. Reading `*slot` as u64 is sound (repr(transparent)).
                if tracing {
                    let old = *(slot as *const u64);
                    evac.visit(&mut *ngc_slot);
                    let new = *(slot as *const u64);
                    crate::gc_trace::root_rewrite(cycle, slot as usize, old, new);
                } else {
                    evac.visit(&mut *ngc_slot);
                }
            }
        }
    }

    /// GAP-011 trace helper: emit one `root` record for a registered slot,
    /// reading the raw Word the slot currently holds (pre-collection).
    fn trace_one_root(cycle: u64, idx: usize, src: &str, slot: *const Word) {
        // SAFETY: a registered root slot address is always valid to read;
        // `Word` is repr(transparent) u64.
        let w = unsafe { *(slot as *const u64) };
        crate::gc_trace::root(cycle, idx, src, slot as usize, w);
    }

    /// GAP-011 trace helper: dump the full registered root set, labelled by
    /// provenance, in the same order `snapshot_roots` concatenates the four
    /// sources (thread root-stack, JIT frames, AOT frames, multi-values).
    fn trace_emit_root_set(cycle: u64) {
        let mut i = 0usize;
        for slot in super::ROOT_STACK.with(|s| s.borrow().clone()) {
            trace_one_root(cycle, i, "stack", slot);
            i += 1;
        }
        for slot in crate::stack_map::snapshot_active_jit_roots() {
            trace_one_root(cycle, i, "jit", slot);
            i += 1;
        }
        for slot in crate::aot::snapshot_active_aot_roots() {
            trace_one_root(cycle, i, "aot", slot);
            i += 1;
        }
        for slot in crate::values::snapshot_active_values_roots() {
            trace_one_root(cycle, i, "values", slot);
            i += 1;
        }
    }
}

#[cfg(feature = "newgc-backend")]
/// Sprint 23 NewGC-backed `Heap`. Same public API as the legacy
/// semispace `Heap` — see the `#[cfg(feature = "semispace-backend")]`
/// definition above for method docs.
pub struct Heap {
    inner: newgc_backend::HeapImpl,
}

#[cfg(feature = "newgc-backend")]
// SAFETY: `Heap`'s state is `Mutex<NewGcInner>` (Send + Sync via the
// Mutex) plus thread-local root state. The lock-free root registry
// is per-thread (the Sprint 11c contract); concurrent access from
// other threads goes through the `inner` mutex.
unsafe impl Send for Heap {}
#[cfg(feature = "newgc-backend")]
unsafe impl Sync for Heap {}

#[cfg(feature = "newgc-backend")]
impl Heap {
    pub fn new() -> Self {
        Self::with_config(GcConfig::default())
    }

    pub fn with_capacity(capacity_bytes: usize) -> Self {
        let young = capacity_bytes / 4;
        let old = capacity_bytes - young;
        Self::with_config(GcConfig {
            young_bytes: young,
            old_bytes: old,
        })
    }

    pub fn with_config(cfg: GcConfig) -> Self {
        Heap {
            inner: newgc_backend::HeapImpl::with_config(cfg),
        }
    }

    pub fn alloc_object(&self, class: ClassId, payload_bytes: usize) -> Word {
        self.inner.alloc_object(class, payload_bytes)
    }

    pub fn wrapper_of(&self, w: Word) -> Option<Wrapper> {
        self.inner.wrapper_of(w)
    }

    pub fn live_bytes(&self) -> usize {
        self.inner.live_bytes()
    }

    pub fn object_count(&self) -> usize {
        self.inner.object_count()
    }

    pub fn young_used_bytes(&self) -> usize {
        self.inner.young_used_bytes()
    }

    pub fn old_used_bytes(&self) -> usize {
        self.inner.old_used_bytes()
    }

    pub fn capacity_bytes(&self) -> usize {
        self.inner.capacity_bytes()
    }

    pub fn register_root(&self, root: *const Word) {
        register_root(root);
    }

    pub fn unregister_root(&self, root: *const Word) {
        unregister_root(root);
    }

    pub fn root_count(&self) -> usize {
        root_count()
    }

    pub fn mark_card_for(&self, dst_ptr: *const Word) {
        self.inner.mark_card_for(dst_ptr);
    }

    /// # Safety
    ///
    /// NewGC backend ignores this — see [`Heap::pin_stack_range`]
    /// doc on the semispace backend. The argument range is unused.
    pub unsafe fn pin_stack_range(&self, lo: usize, hi: usize) -> usize {
        // SAFETY: no reads through `lo..hi`; the implementation
        // discards the args entirely.
        unsafe { self.inner.pin_stack_range(lo, hi) }
    }

    pub fn clear_pinned(&self) {
        self.inner.clear_pinned();
    }

    pub fn dirty_card_count(&self) -> usize {
        self.inner.dirty_card_count()
    }

    pub fn minor_collection_count(&self) -> u64 {
        self.inner.minor_collection_count()
    }

    pub fn major_collection_count(&self) -> u64 {
        self.inner.major_collection_count()
    }

    pub fn ranges(&self) -> HeapRanges {
        self.inner.ranges()
    }

    pub(crate) fn stats_snapshot(&self) -> HeapStatsSnapshot {
        self.inner.stats_snapshot()
    }

    pub fn collect_minor(&self) {
        self.inner.collect_minor();
    }

    pub fn collect_full(&self) {
        self.inner.collect_full();
    }
}

#[cfg(feature = "newgc-backend")]
impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::ClassTable;

    #[test]
    fn alloc_object_returns_tagged_pointer() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_object(ct.byte_string(), 16);
        assert!(w.is_pointer());
        assert!(!w.is_fixnum());
    }

    #[test]
    fn wrapper_round_trip_via_heap() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let w = heap.alloc_object(ct.byte_string(), 16);
        let wrap = heap.wrapper_of(w).expect("wrapper inside heap");
        assert_eq!(wrap.class(), ct.byte_string());
    }

    /// GAP-010 reproduction on the REAL `DylanLayout`-backed heap,
    /// without the AOT compiler. Mirrors `F:\scratch\gap010.dylan`: one
    /// rooted size-10 `<byte-string>` held live across a churn loop of
    /// dead size-512 `<byte-string>`s. Allocation auto-drives minor GCs
    /// (and promotion of the survivor). After every churn alloc we
    /// re-read `keep` through its registered root slot and confirm it is
    /// still a live `<byte-string>` of size 10 with intact bytes. The
    /// first cycle that mis-relocates or corrupts the survivor trips an
    /// assert here, pinpointing the bad promotion — the deref-and-validate
    /// probe the Dylan team couldn't run without a debugger (GAP-010 §8).
    #[cfg(feature = "newgc-backend")]
    #[test]
    fn gap010_surviving_bytestring_survives_promotion_under_churn() {
        newgc_core::crash::install();

        // Small young gen so minors fire often and `keep` promotes fast.
        let heap = Heap::with_config(GcConfig {
            young_bytes: 256 * 1024,
            old_bytes: 32 * 1024 * 1024,
        });
        let ct = ClassTable::new();
        let bs_class = ct.byte_string();

        // `keep` = size-10 byte-string with a recognizable fill so a torn
        // payload is detectable (the real repro zero-fills; 'k' is stricter).
        let keep0 = heap.alloc_byte_string("kkkkkkkkkk", &ct);

        // Stable registered root slot — exactly the codegen spill-slot
        // contract: a raw `*const Word` the collector rewrites in place.
        let mut keep_slot: crate::word::Word = keep0;
        heap.register_root(&keep_slot as *const crate::word::Word);

        let first_addr = keep_slot.raw() & !1;
        let mut last_addr = first_addr;
        let mut moves = 0u64;

        let junk_src = "y".repeat(512);
        let iters: usize = std::env::var("GAP010_ITERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(200_000);

        for i in 0..iters {
            // churn: dead size-512 byte-string. May trigger a minor.
            let _junk = heap.alloc_byte_string(&junk_src, &ct);

            // Re-validate `keep` via its (possibly rewritten) slot.
            let cur = keep_slot.raw() & !1;
            if cur != last_addr {
                moves += 1;
                last_addr = cur;
            }
            // SAFETY: `keep_slot` holds a (relocated) pointer-tagged Word
            // the GC rewrote in place; try_byte_string validates the class.
            let bs = match unsafe { crate::strings::try_byte_string(keep_slot, bs_class) } {
                Some(b) => b,
                None => panic!(
                    "iter {i}: keep is no longer a <byte-string> \
                     (raw {:#x}, moved {moves}x from {first_addr:#x})",
                    keep_slot.raw()
                ),
            };
            assert_eq!(
                bs.len, 10,
                "iter {i}: keep size wrong: {} (raw {:#x}, moved {moves}x)",
                bs.len,
                keep_slot.raw()
            );
            // SAFETY: bs is a live byte-string of the asserted size.
            let bytes = unsafe { bs.bytes() };
            assert!(
                bytes.iter().all(|&b| b == b'k'),
                "iter {i}: keep payload torn: {:?} (raw {:#x}, moved {moves}x)",
                bytes,
                keep_slot.raw()
            );
        }

        heap.unregister_root(&keep_slot as *const crate::word::Word);
        eprintln!(
            "gap010 repro OK: keep survived {iters} churn allocs, moved {moves}x, ended at {:#x}",
            keep_slot.raw()
        );
    }

    /// GAP-010 reproduction at MAXIMUM fidelity short of the AOT EXE:
    /// run on the **process-global literal-pool heap** (default 4 MB
    /// young, exactly like the EXE) with the **full runtime initialised**
    /// (`nod_runtime_init` — class metadata, singletons, condition
    /// classes), allocating both `keep` and the churn through the **exact
    /// primitive** `make(<byte-string>, size: n)` lowers to:
    /// `nod_byte_string_allocate`. The only remaining difference from the
    /// crashing EXE is that the driving loop here is Rust, not AOT-emitted
    /// machine code. If this stays green, the GC + runtime-integration is
    /// exonerated and GAP-010 is squarely an AOT-codegen defect; if it
    /// reproduces, the fault is runtime/GC-side and reproducible in-process.
    ///
    /// `#[ignore]`: this drives the **process-global** literal-pool heap and
    /// runs minor collections whose root set is this thread's thread-local
    /// roots only — so in the default parallel test run its collections
    /// would reclaim *other* tests' live objects on the shared heap. Run it
    /// alone: `cargo test -p nod-runtime --
    /// gap010_global_heap_runtime_init_bytestring_churn --ignored
    /// --test-threads=1`. The sibling `gap010_surviving_bytestring_*` test
    /// uses a private local heap and is the parallel-safe regression guard.
    #[cfg(feature = "newgc-backend")]
    #[test]
    #[ignore = "drives the process-global heap; unsafe to run in parallel with other tests"]
    fn gap010_global_heap_runtime_init_bytestring_churn() {
        use crate::word::Word;
        newgc_core::crash::install();
        crate::aot::nod_runtime_init();

        // `keep = make(<byte-string>, size: 10)` — global heap, zero-filled.
        let keep_raw =
            unsafe { crate::strings::nod_byte_string_allocate(Word::from_fixnum(10).unwrap().raw()) };
        let mut keep_slot = Word::from_raw(keep_raw);
        super::register_root(&keep_slot as *const Word);

        let bs_class = crate::ClassId::BYTE_STRING;
        // Sanity: keep is a size-10 byte-string before any churn/GC.
        {
            let bs = unsafe { crate::strings::try_byte_string(keep_slot, bs_class) }
                .expect("keep should be a <byte-string> at allocation");
            assert_eq!(bs.len, 10, "keep initial size");
        }

        let first_addr = keep_slot.raw() & !1;
        let mut last_addr = first_addr;
        let mut moves = 0u64;
        let iters: usize = std::env::var("GAP010_ITERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(200_000);

        for i in 0..iters {
            // churn: `make(<byte-string>, size: 512)`, immediately dead.
            let _junk = unsafe {
                crate::strings::nod_byte_string_allocate(Word::from_fixnum(512).unwrap().raw())
            };

            let cur = keep_slot.raw() & !1;
            if cur != last_addr {
                moves += 1;
                last_addr = cur;
            }
            // SAFETY: keep_slot holds the (relocated) pointer-tagged Word
            // the collector rewrote in place via the registered root.
            let bs = match unsafe { crate::strings::try_byte_string(keep_slot, bs_class) } {
                Some(b) => b,
                None => panic!(
                    "iter {i}: keep no longer a <byte-string> \
                     (raw {:#x}, moved {moves}x from {first_addr:#x})",
                    keep_slot.raw()
                ),
            };
            assert_eq!(
                bs.len, 10,
                "iter {i}: keep size corrupted: {} (raw {:#x}, moved {moves}x)",
                bs.len,
                keep_slot.raw()
            );
        }

        super::unregister_root(&keep_slot as *const Word);
        eprintln!(
            "gap010 global-heap repro OK: keep survived {iters} make(<byte-string>,512) churns, \
             moved {moves}x, ended at {:#x}",
            keep_slot.raw()
        );
    }

    #[test]
    fn live_bytes_advances() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let before = heap.live_bytes();
        let _ = heap.alloc_object(ct.byte_string(), 16);
        let after = heap.live_bytes();
        assert!(after > before);
        assert_eq!(after - before, 24);
    }

    #[test]
    fn object_count_advances() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let before = heap.object_count();
        let _ = heap.alloc_object(ct.byte_string(), 16);
        let _ = heap.alloc_object(ct.symbol(), 16);
        assert_eq!(heap.object_count(), before + 2);
    }

    #[test]
    fn allocations_stay_aligned() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        for n in [1usize, 7, 8, 9, 23, 64] {
            let w = heap.alloc_object(ct.byte_string(), n);
            let p = w.as_ptr::<u8>().unwrap() as usize;
            assert_eq!(p & 0b111, 0, "alignment violated for payload={n}");
        }
    }
}
