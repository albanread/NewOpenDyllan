//! Stack maps for precise root walking — **Sprint 11c preparation**.
//!
//! Sprint 11b ships precise GC roots via spill-to-runtime-slots
//! (`nod_register_root` / `nod_unregister_root`); the runtime's
//! `Heap::roots` Mutex is the authoritative root list. Stack maps
//! aren't on the live path yet.
//!
//! This module is the **Sprint 11c** data shape lifted near-verbatim
//! from NCL's `ncl-runtime/src/stack_map.rs`. When the
//! `gc.statepoint`-based precise-roots story lands (replacing the
//! explicit register/unregister shim calls with a single
//! statepoint-emitted stack map per safe point), the compiler-side
//! code will populate a `StackMap` and the collector will consult it
//! via `walk_parked_frame` instead of `Heap::roots.iter()`.
//!
//! The shape is intentionally minimal — enough so plugging Sprint 11c
//! into the existing collector is a small connection job, not a
//! redesign. **Nothing in `nod-runtime` calls this code yet.** It
//! compiles, its tests pass, and that's the entire deliverable for
//! Sprint 11b.
//!
//! Lifted with attribution from
//! `E:\CL\NewCormanLisp\src\ncl-runtime\src\stack_map.rs` — algorithm
//! and ABI unchanged; the only diff is the `Word` import (Dylan's
//! 1-bit-tag scheme uses `Word::from_raw` where NCL uses
//! `unsafe { Word::from_raw(…) }`).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::word::Word;

// -- LiveSlot ----------------------------------------------------------------

/// A single live root location at one safe point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSlot {
    /// On the parked thread's stack, at `frame_pointer + offset`. The
    /// compiler may use a positive or negative offset; a small signed
    /// integer is enough for any reasonable frame.
    FpOffset(i32),
    /// In the parked thread's saved register file, at the named
    /// general-purpose register index. The mutator's `park` captures
    /// the live registers into a side buffer; this index is into that
    /// buffer.
    SavedRegister(u8),
}

// -- StackMapEntry -----------------------------------------------------------

/// All live roots at a single safe-point PC.
#[derive(Debug, Clone)]
pub struct StackMapEntry {
    /// The exact program-counter value (machine address) of the safe
    /// point. Looked up in the global `StackMap` after a mutator
    /// parks.
    pub pc: u64,
    /// Live root slots at this safe point.
    pub slots: Vec<LiveSlot>,
}

// -- StackMap ----------------------------------------------------------------

/// Collection of stack-map entries indexed by PC. Built up by the JIT
/// as it emits code; queried by the GC during stop-the-world.
#[derive(Debug, Default)]
pub struct StackMap {
    entries: HashMap<u64, StackMapEntry>,
}

impl StackMap {
    pub fn new() -> StackMap {
        StackMap::default()
    }

    pub fn register(&mut self, entry: StackMapEntry) {
        self.entries.insert(entry.pc, entry);
    }

    pub fn lookup(&self, pc: u64) -> Option<&StackMapEntry> {
        self.entries.get(&pc)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JitSafepointEntry {
    pub namespace: u64,
    pub site_id: u64,
    pub slots: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveJitSafepoint {
    namespace: u64,
    site_id: u64,
    slot_base: *mut Word,
}

fn jit_safepoint_registry(
) -> &'static std::sync::Mutex<BTreeMap<(u64, u64), JitSafepointEntry>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<BTreeMap<(u64, u64), JitSafepointEntry>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

thread_local! {
    static ACTIVE_JIT_SAFEPOINTS: RefCell<Vec<ActiveJitSafepoint>> =
        const { RefCell::new(Vec::new()) };
}

pub fn active_jit_safepoint_depth() -> usize {
    // `try_borrow` so the panic / crash-dump hook can read this without
    // double-panicking if the unwind fired while a `borrow_mut` was live
    // on this thread (reports 0 in that pathological case).
    ACTIVE_JIT_SAFEPOINTS.with(|stack| stack.try_borrow().map(|s| s.len()).unwrap_or(0))
}

pub fn truncate_active_jit_safepoints(depth: usize) {
    ACTIVE_JIT_SAFEPOINTS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.len() > depth {
            stack.truncate(depth);
        }
    });
}

pub fn register_jit_safepoints(entries: Vec<JitSafepointEntry>) {
    let mut registry = jit_safepoint_registry()
        .lock()
        .expect("jit safepoint registry poisoned");
    for entry in entries {
        registry.insert((entry.namespace, entry.site_id), entry);
    }
}

fn lookup_jit_safepoint(namespace: u64, site_id: u64) -> Option<JitSafepointEntry> {
    jit_safepoint_registry()
        .lock()
        .expect("jit safepoint registry poisoned")
        .get(&(namespace, site_id))
        .cloned()
}

fn require_jit_safepoint(namespace: u64, site_id: u64) -> JitSafepointEntry {
    lookup_jit_safepoint(namespace, site_id).unwrap_or_else(|| {
        panic!(
            "missing JIT safepoint registration for namespace={namespace:#x} site_id={site_id}"
        )
    })
}

pub fn snapshot_active_jit_roots() -> Vec<*const Word> {
    ACTIVE_JIT_SAFEPOINTS.with(|stack| {
        let active = stack.borrow();
        let mut roots = Vec::new();
        for frame in active.iter() {
            let Some(entry) = lookup_jit_safepoint(frame.namespace, frame.site_id) else {
                continue;
            };
            for &slot_idx in &entry.slots {
                // SAFETY: `slot_base` points at the first entry of the
                // active safepoint slot slab; every recorded slot index is
                // in-bounds for that slab while the safepoint is active.
                let slot = unsafe { frame.slot_base.add(slot_idx as usize) };
                roots.push(slot as *const Word);
            }
        }
        roots
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_jit_begin_safepoint(
    namespace: u64,
    site_id: u64,
    slot_base: *mut Word,
) {
    let _entry = require_jit_safepoint(namespace, site_id);
    ACTIVE_JIT_SAFEPOINTS.with(|stack| {
        stack.borrow_mut().push(ActiveJitSafepoint {
            namespace,
            site_id,
            slot_base,
        });
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn nod_jit_end_safepoint(namespace: u64, site_id: u64) {
    ACTIVE_JIT_SAFEPOINTS.with(|stack| {
        let mut stack = stack.borrow_mut();
        let Some(top) = stack.pop() else {
            return;
        };
        assert_eq!(
            (top.namespace, top.site_id),
            (namespace, site_id),
            "JIT safepoint stack mismatch"
        );
    });
}

#[cfg(test)]
fn reset_jit_safepoints_for_tests() {
    jit_safepoint_registry()
        .lock()
        .expect("jit safepoint registry poisoned")
        .clear();
    ACTIVE_JIT_SAFEPOINTS.with(|stack| stack.borrow_mut().clear());
}

// -- ParkedFrame -------------------------------------------------------------

/// State captured by a mutator at `park()` time, sufficient for the GC
/// to walk its frame. For Sprint 11c this is data-only — the actual
/// FP/PC capture from CPU state needs a small platform-specific shim
/// that lands alongside the compiler-side `gc.statepoint` emission.
#[derive(Debug)]
pub struct ParkedFrame {
    /// Frame pointer of the parked frame.
    pub fp: usize,
    /// PC at the safe point — used to look up the stack map entry.
    pub pc: u64,
    /// Saved register file. Up to 16 GPRs is enough for both x86-64
    /// (16 GPRs) and aarch64 (32 GPRs — we'd grow this if we need
    /// more slots there). Index by `LiveSlot::SavedRegister`.
    pub saved_regs: [u64; 16],
}

impl ParkedFrame {
    pub fn new(fp: usize, pc: u64) -> ParkedFrame {
        ParkedFrame {
            fp,
            pc,
            saved_regs: [0; 16],
        }
    }
}

// -- Walking -----------------------------------------------------------------

/// Walk a single parked frame and call `visit` on each live root
/// slot's address. Returns `Some(n_visited)` when an entry was found,
/// `None` when no entry matches the PC (caller may fall back to
/// conservative scanning during bring-up).
///
/// `visit` receives a `*mut u64` pointing AT the slot itself. It may
/// read the slot to extract a `Word`, and write back if the slot is
/// updated by a forwarding pointer.
///
/// # Safety
///
/// The caller asserts that `frame.fp + FpOffset` addresses are valid
/// (the parked frame is alive and exclusive to this thread for the
/// duration of the GC). `SavedRegister` slots are inside `frame`
/// itself and always safe.
pub unsafe fn walk_parked_frame(
    frame: &mut ParkedFrame,
    stack_map: &StackMap,
    mut visit: impl FnMut(*mut u64),
) -> Option<usize> {
    let entry = stack_map.lookup(frame.pc)?;
    let mut count = 0;
    for slot in &entry.slots {
        match *slot {
            LiveSlot::FpOffset(off) => {
                let addr = frame.fp.wrapping_add_signed(off as isize) as *mut u64;
                visit(addr);
                count += 1;
            }
            LiveSlot::SavedRegister(idx) => {
                let addr = &mut frame.saved_regs[idx as usize] as *mut u64;
                visit(addr);
                count += 1;
            }
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn fixnum_word(n: i64) -> Word {
        Word::from_fixnum(n).expect("test fixnum in range")
    }

    #[test]
    fn empty_map_returns_none() {
        let map = StackMap::new();
        let mut frame = ParkedFrame::new(0, 100);
        // SAFETY: no entry, `visit` never invoked.
        let r =
            unsafe { walk_parked_frame(&mut frame, &map, |_| panic!("should not visit")) };
        assert!(r.is_none());
    }

    #[test]
    fn unknown_pc_returns_none() {
        let mut map = StackMap::new();
        map.register(StackMapEntry { pc: 100, slots: vec![] });
        let mut frame = ParkedFrame::new(0, 999);
        // SAFETY: PC unknown, visit not invoked.
        let r =
            unsafe { walk_parked_frame(&mut frame, &map, |_| panic!("should not visit")) };
        assert!(r.is_none());
    }

    #[test]
    fn walks_fp_offset_slots() {
        // Simulate a stack frame with three Words at known offsets.
        // The frame is a fixed-size array on our local stack; we use
        // its address as the "frame pointer" in the parked frame.
        let mut frame_storage: [u64; 8] = [0; 8];
        frame_storage[2] = fixnum_word(11).raw();
        frame_storage[5] = fixnum_word(22).raw();
        frame_storage[7] = fixnum_word(33).raw();
        let fp = frame_storage.as_ptr() as usize;

        let mut map = StackMap::new();
        map.register(StackMapEntry {
            pc: 0xDEAD_BEEF,
            slots: vec![
                LiveSlot::FpOffset(2 * 8),
                LiveSlot::FpOffset(5 * 8),
                LiveSlot::FpOffset(7 * 8),
            ],
        });

        let mut frame = ParkedFrame::new(fp, 0xDEAD_BEEF);
        let mut visited = Vec::new();
        // SAFETY: frame_storage is alive for the duration of the walk;
        // every visited address is into that live storage.
        let n = unsafe {
            walk_parked_frame(&mut frame, &map, |addr| {
                visited.push(Word::from_raw(*addr));
            })
        }
        .unwrap();

        assert_eq!(n, 3);
        assert_eq!(visited.len(), 3);
        assert_eq!(visited[0].as_fixnum(), Some(11));
        assert_eq!(visited[1].as_fixnum(), Some(22));
        assert_eq!(visited[2].as_fixnum(), Some(33));
    }

    #[test]
    fn walks_saved_register_slots() {
        let mut map = StackMap::new();
        map.register(StackMapEntry {
            pc: 1,
            slots: vec![LiveSlot::SavedRegister(3), LiveSlot::SavedRegister(7)],
        });
        let mut frame = ParkedFrame::new(0, 1);
        frame.saved_regs[3] = fixnum_word(100).raw();
        frame.saved_regs[7] = fixnum_word(200).raw();

        let mut seen = Vec::new();
        // SAFETY: register slots live inside `frame` itself.
        let n = unsafe {
            walk_parked_frame(&mut frame, &map, |addr| {
                seen.push(Word::from_raw(*addr));
            })
        }
        .unwrap();

        assert_eq!(n, 2);
        assert_eq!(seen[0].as_fixnum(), Some(100));
        assert_eq!(seen[1].as_fixnum(), Some(200));
    }

    #[test]
    fn visit_can_update_slot_in_place() {
        // The whole point: a forwarding-pointer update writes back.
        let mut frame_storage: [u64; 4] = [0; 4];
        frame_storage[1] = fixnum_word(0).raw();
        let fp = frame_storage.as_ptr() as usize;

        let mut map = StackMap::new();
        map.register(StackMapEntry {
            pc: 1,
            slots: vec![LiveSlot::FpOffset(8)], // cell index 1
        });
        let mut frame = ParkedFrame::new(fp, 1);
        // SAFETY: frame_storage is alive for the call; visit writes
        // into cell 1 which is inside frame_storage.
        unsafe {
            walk_parked_frame(&mut frame, &map, |addr| {
                *addr = fixnum_word(42).raw();
            });
        }

        // The original slot was mutated.
        assert_eq!(Word::from_raw(frame_storage[1]).as_fixnum(), Some(42));
    }

    #[test]
    fn negative_fp_offset_works() {
        // Compilers often place locals at negative FP offsets.
        // Build a frame with a "below FP" slot.
        let mut frame_storage: [u64; 8] = [0; 8];
        frame_storage[1] = fixnum_word(77).raw();
        // Treat cell 4 as the FP — slot at offset -3*8 = cell 1.
        // SAFETY: cell 4 is inside frame_storage.
        let fp = unsafe { frame_storage.as_ptr().add(4) } as usize;

        let mut map = StackMap::new();
        map.register(StackMapEntry {
            pc: 1,
            slots: vec![LiveSlot::FpOffset(-(3 * 8))],
        });
        let mut frame = ParkedFrame::new(fp, 1);
        let mut seen = None;
        // SAFETY: frame_storage is alive for the walk; visited addr
        // is into that storage.
        unsafe {
            walk_parked_frame(&mut frame, &map, |addr| {
                seen = Some(Word::from_raw(*addr));
            });
        }
        assert_eq!(seen.unwrap().as_fixnum(), Some(77));
    }

    #[test]
    fn mixed_slots_visited_in_order() {
        let mut frame_storage: [u64; 4] = [0; 4];
        frame_storage[2] = fixnum_word(11).raw();
        let fp = frame_storage.as_ptr() as usize;

        let mut map = StackMap::new();
        map.register(StackMapEntry {
            pc: 1,
            slots: vec![
                LiveSlot::FpOffset(2 * 8),
                LiveSlot::SavedRegister(5),
            ],
        });
        let mut frame = ParkedFrame::new(fp, 1);
        frame.saved_regs[5] = fixnum_word(22).raw();

        let mut seen = Vec::new();
        // SAFETY: frame_storage is alive; register slots are inside
        // `frame` itself.
        unsafe {
            walk_parked_frame(&mut frame, &map, |addr| {
                seen.push(Word::from_raw(*addr));
            });
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].as_fixnum(), Some(11));
        assert_eq!(seen[1].as_fixnum(), Some(22));
    }

    #[test]
    fn stack_map_basic_ops() {
        let mut m = StackMap::new();
        assert!(m.is_empty());
        m.register(StackMapEntry { pc: 1, slots: vec![] });
        m.register(StackMapEntry {
            pc: 2,
            slots: vec![LiveSlot::FpOffset(0)],
        });
        assert_eq!(m.len(), 2);
        assert!(m.lookup(1).is_some());
        assert!(m.lookup(2).is_some());
        assert!(m.lookup(3).is_none());
    }

    #[test]
    #[serial]
    fn active_jit_safepoint_snapshot_uses_registered_slots() {
        reset_jit_safepoints_for_tests();
        register_jit_safepoints(vec![JitSafepointEntry {
            namespace: 0xAA,
            site_id: 7,
            slots: vec![0, 2],
        }]);
        let mut slots = [fixnum_word(11), fixnum_word(22), fixnum_word(33)];
        unsafe {
            nod_jit_begin_safepoint(0xAA, 7, slots.as_mut_ptr());
        }
        let roots = snapshot_active_jit_roots();
        assert_eq!(roots.len(), 2);
        assert_eq!(unsafe { (*roots[0]).as_fixnum() }, Some(11));
        assert_eq!(unsafe { (*roots[1]).as_fixnum() }, Some(33));
        nod_jit_end_safepoint(0xAA, 7);
        assert!(snapshot_active_jit_roots().is_empty());
    }

    #[test]
    #[serial]
    fn truncate_active_jit_safepoints_restores_checkpoint() {
        reset_jit_safepoints_for_tests();
        register_jit_safepoints(vec![
            JitSafepointEntry {
                namespace: 0xAA,
                site_id: 7,
                slots: vec![0],
            },
            JitSafepointEntry {
                namespace: 0xAA,
                site_id: 8,
                slots: vec![0],
            },
        ]);
        let mut outer_slots = [fixnum_word(11)];
        let mut inner_slots = [fixnum_word(22)];
        unsafe {
            nod_jit_begin_safepoint(0xAA, 7, outer_slots.as_mut_ptr());
        }
        let baseline = active_jit_safepoint_depth();
        unsafe {
            nod_jit_begin_safepoint(0xAA, 8, inner_slots.as_mut_ptr());
        }
        truncate_active_jit_safepoints(baseline);
        let roots = snapshot_active_jit_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(unsafe { (*roots[0]).as_fixnum() }, Some(11));
        nod_jit_end_safepoint(0xAA, 7);
        assert!(snapshot_active_jit_roots().is_empty());
    }

    #[test]
    #[serial]
    #[should_panic(expected = "missing JIT safepoint registration")]
    fn begin_jit_safepoint_requires_registration() {
        reset_jit_safepoints_for_tests();
        let _ = require_jit_safepoint(0xAA, 7);
    }
}
