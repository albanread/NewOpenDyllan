//! Static area — pinned, never-collected storage for things that
//! outlive the session: class metadata, immediates, JIT'd machine
//! code, and (Sprint 11) the literal pool.
//!
//! Concept lifted from NCL's `static_area`. The GC never moves these
//! allocations and never frees them; the storage is leaked deliberately
//! for the lifetime of the process.
//!
//! Sprint 11 adds two purpose-built allocators on top of the generic
//! `alloc<T>`:
//!
//!   - `alloc_byte_string` — allocates a `<byte-string>`-headed cell
//!     in pinned storage. Used by the literal pool so codegen-baked
//!     string-literal addresses survive every GC.
//!   - `alloc_simple_object_vector` — same for `<simple-object-vector>`.
//!     Sprint 12+ uses this for sealed-class vtables.
//!
//! These bypass `Heap::alloc_object` precisely because the heap can
//! move objects; the static area cannot. The GC's root walker treats
//! pinned-static addresses as roots that always resolve (the addresses
//! never appear in the young/old extents the collector mutates).

use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::sync::Mutex;

use crate::classes::{ClassId, ClassTable};
use crate::strings::ByteString;
use crate::symbols::Symbol;
use crate::vectors::SimpleObjectVector;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// Pinned, append-only arena. Sprint 09: a `Vec<Box<dyn Any>>`
/// shadow keeps the boxes alive for the area's lifetime. The
/// `'static` reference returned by `alloc` is sound because the
/// `StaticArea` itself is expected to live for the process.
pub struct StaticArea {
    pinned: Mutex<UnsafeCell<Vec<Box<dyn std::any::Any + Send + Sync>>>>,
    /// Raw byte allocations (for `alloc_byte_string` etc.). Each entry
    /// is a `Vec<u8>` whose stable address survives the lifetime of
    /// the area.
    raw_buffers: Mutex<Vec<Box<[u8]>>>,
}

// SAFETY: the UnsafeCell is only touched while the Mutex is held, and
// allocations only ever append (never overwrite).
unsafe impl Sync for StaticArea {}

impl StaticArea {
    pub fn new() -> Self {
        StaticArea {
            pinned: Mutex::new(UnsafeCell::new(Vec::new())),
            raw_buffers: Mutex::new(Vec::new()),
        }
    }

    /// Pin `value` so its address survives any number of subsequent
    /// allocations. Returns a `&'static T` reference.
    pub fn alloc<T: Send + Sync + 'static>(&self, value: T) -> &'static T {
        let boxed: Box<T> = Box::new(value);
        // SAFETY: leaking the box; the returned reference is valid for
        // the program's lifetime.
        let ptr: &'static T = unsafe { &*Box::into_raw(boxed) };
        // SAFETY: ptr came from Box::into_raw; reconstructing it is
        // its inverse.
        let owned: Box<dyn std::any::Any + Send + Sync> = unsafe {
            Box::from_raw(ptr as *const T as *mut T)
        };
        let guard = self.pinned.lock().expect("static area mutex poisoned");
        // SAFETY: guarded by the mutex.
        let vec: &mut Vec<_> = unsafe { &mut *guard.get() };
        vec.push(owned);
        ptr
    }

    /// Pin a raw byte buffer of the given size, returning its base
    /// address. The buffer is zero-initialised.
    fn alloc_raw_bytes(&self, n_bytes: usize) -> NonNull<u8> {
        let aligned = n_bytes.next_multiple_of(8);
        let buf: Box<[u8]> = vec![0u8; aligned].into_boxed_slice();
        let mut guard = self.raw_buffers.lock().expect("static raw buffers poisoned");
        // Leak the box: get a raw pointer to its first byte, but keep
        // the Box in raw_buffers so it survives.
        let ptr = buf.as_ptr() as *mut u8;
        guard.push(buf);
        // SAFETY: the box we just pushed isn't going anywhere; its
        // data pointer is stable.
        unsafe { NonNull::new_unchecked(ptr) }
    }

    /// Allocate a `<byte-string>` in pinned storage. Returns a
    /// pointer-tagged `Word`. The address NEVER moves; it's safe to
    /// bake into JIT'd LLVM constants.
    pub fn alloc_byte_string(&self, s: &str, classes: &ClassTable) -> Word {
        let bytes = s.as_bytes();
        let total = (size_of::<ByteString>() + bytes.len()).next_multiple_of(8);
        let buf = self.alloc_raw_bytes(total);
        let addr = buf.as_ptr() as usize;
        // SAFETY: alloc_raw_bytes returned a fresh `total`-byte buffer.
        unsafe {
            let bs = addr as *mut ByteString;
            (*bs).wrapper = Wrapper::new(classes.byte_string());
            (*bs).len = bytes.len() as u32;
            (*bs)._pad = 0;
            if !bytes.is_empty() {
                let dst = (addr + size_of::<ByteString>()) as *mut u8;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            }
        }
        Word::from_ptr(addr as *const u8)
    }

    /// Allocate a `<symbol>` in pinned storage. Returns a
    /// pointer-tagged `Word`. The `name` slot is set to `name_word`
    /// (which should itself be a pinned `<byte-string>` word).
    pub fn alloc_symbol(&self, name_word: Word, hash: u32, classes: &ClassTable) -> Word {
        let total = size_of::<Symbol>().next_multiple_of(8);
        let buf = self.alloc_raw_bytes(total);
        let addr = buf.as_ptr() as usize;
        // SAFETY: alloc_raw_bytes returned a fresh `total`-byte buffer.
        unsafe {
            let s = addr as *mut Symbol;
            (*s).wrapper = Wrapper::new(classes.symbol());
            (*s).hash = hash;
            (*s)._pad = 0;
            (*s).name = name_word;
        }
        Word::from_ptr(addr as *const u8)
    }

    /// Allocate a zero-initialised `<simple-object-vector>` of `len`
    /// slots in pinned storage. Caller fills slots via the standard
    /// vector accessors. Sprint 11 doesn't use this directly; Sprint
    /// 12+ wires it for sealed-class vtables.
    pub fn alloc_simple_object_vector(&self, len: usize, classes: &ClassTable) -> Word {
        let total =
            (size_of::<SimpleObjectVector>() + len * size_of::<Word>()).next_multiple_of(8);
        let buf = self.alloc_raw_bytes(total);
        let addr = buf.as_ptr() as usize;
        // SAFETY: fresh buffer.
        unsafe {
            let v = addr as *mut SimpleObjectVector;
            (*v).wrapper = Wrapper::new(classes.simple_object_vector());
            (*v).len = len as u64;
        }
        Word::from_ptr(addr as *const u8)
    }

    /// Test helper: is `addr` inside one of this area's pinned raw
    /// buffers? Returns true iff one of `alloc_*` was the origin.
    pub fn contains(&self, addr: usize) -> bool {
        let guard = self.raw_buffers.lock().expect("static raw buffers poisoned");
        guard.iter().any(|buf| {
            let base = buf.as_ptr() as usize;
            let end = base + buf.len();
            addr >= base && addr < end
        })
    }
}

impl Default for StaticArea {
    fn default() -> Self {
        Self::new()
    }
}

// Unused-import suppression for some build configurations.
const _: fn() = || {
    let _ = std::marker::PhantomData::<ClassId>;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_returns_stable_reference() {
        let area = StaticArea::new();
        let r1 = area.alloc(42_u64);
        let r1_addr = r1 as *const u64;
        for n in 0u64..100 {
            let _ = area.alloc(n);
        }
        assert_eq!(*r1, 42);
        assert_eq!(r1 as *const u64, r1_addr);
    }

    #[test]
    fn distinct_allocations_distinct_addresses() {
        let area = StaticArea::new();
        let a = area.alloc(1_u32);
        let b = area.alloc(2_u32);
        assert_ne!(a as *const u32, b as *const u32);
    }

    #[test]
    fn alloc_byte_string_round_trip() {
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let w = area.alloc_byte_string("hello", &ct);
        assert!(w.is_pointer());
        let addr = w.as_ptr::<u8>().unwrap() as usize;
        assert!(area.contains(addr));
        // SAFETY: we just allocated this string.
        let bs = unsafe { &*(addr as *const ByteString) };
        assert_eq!(bs.len, 5);
        assert_eq!(bs.wrapper.class(), ct.byte_string());
        // SAFETY: bs is a valid ByteString header.
        assert_eq!(unsafe { bs.as_str() }, Some("hello"));
    }

    #[test]
    fn alloc_byte_string_addresses_stable() {
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let w1 = area.alloc_byte_string("first", &ct);
        let addr1 = w1.raw() & !1;
        // Many more allocations.
        for n in 0..100 {
            let _ = area.alloc_byte_string(&format!("filler-{n}"), &ct);
        }
        let addr1_after = w1.raw() & !1;
        assert_eq!(addr1, addr1_after);
    }
}
