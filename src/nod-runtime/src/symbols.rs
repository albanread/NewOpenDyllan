//! `<symbol>` — interned Dylan symbol.
//!
//! Layout:
//!
//! ```text
//!   [Wrapper 8B] [hash: u32] [_pad: u32] [name: Word]   (24 bytes)
//! ```
//!
//! `name` is a pointer-tagged `Word` referencing a `<byte-string>`.
//! Two symbols are `==` iff they share storage; the global intern
//! table guarantees `intern("foo")` always returns the same Word.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::classes::ClassTable;
use crate::heap::Heap;
use crate::static_area::StaticArea;
use crate::word::Word;
use crate::wrapper::Wrapper;

#[repr(C)]
pub struct Symbol {
    pub wrapper: Wrapper,
    pub hash: u32,
    pub _pad: u32,
    pub name: Word,
}

impl Symbol {
    /// Borrow the symbol's name (`<byte-string>` pointer).
    pub fn name_word(&self) -> Word {
        self.name
    }
}

/// FNV-1a 32-bit hash of `name`. Symbols use a stable hash so the
/// intern table's lookup behaviour is deterministic across runs.
pub(crate) fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in s.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Global intern table. Maps name string → already-interned `Word`.
pub struct SymbolTable {
    inner: Mutex<HashMap<String, Word>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Look up or freshly allocate a symbol with the given name in
    /// `heap` (a moveable heap). The returned Word is pointer-tagged
    /// and stable for the table's lifetime — repeated `intern("foo")`
    /// calls return the same Word.
    ///
    /// **Sprint 11 caveat**: addresses produced by this path live in
    /// the moveable heap and are invalidated by minor GC. For literal-
    /// pool use (where codegen bakes the address into LLVM IR), call
    /// `intern_static` instead.
    pub fn intern(&self, name: &str, heap: &Heap, classes: &ClassTable) -> Word {
        let mut guard = self.inner.lock().expect("symbol table poisoned");
        if let Some(&w) = guard.get(name) {
            return w;
        }
        let name_word = heap.alloc_byte_string(name, classes);
        let payload_bytes = 4 + 4 + size_of::<Word>();
        let sym_word = heap.alloc_object(classes.symbol(), payload_bytes);
        // SAFETY: `alloc_object` zeroed the payload and installed the
        // wrapper. We populate hash, pad, and name.
        unsafe {
            let p = sym_word.as_mut_ptr::<u8>().expect("symbol is pointer-tagged");
            let s = p as *mut Symbol;
            (*s).hash = fnv1a(name);
            (*s)._pad = 0;
            (*s).name = name_word;
        }
        guard.insert(name.to_string(), sym_word);
        sym_word
    }

    /// Same as `intern` but allocates the symbol's storage in pinned
    /// static memory. Sprint 11 routes the literal pool through this
    /// path so codegen-baked symbol addresses survive every GC cycle.
    pub fn intern_static(
        &self,
        name: &str,
        static_area: &StaticArea,
        classes: &ClassTable,
    ) -> Word {
        let mut guard = self.inner.lock().expect("symbol table poisoned");
        if let Some(&w) = guard.get(name) {
            return w;
        }
        let name_word = static_area.alloc_byte_string(name, classes);
        let sym_word = static_area.alloc_symbol(name_word, fnv1a(name), classes);
        guard.insert(name.to_string(), sym_word);
        sym_word
    }

    /// Number of distinct symbols in the table.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("symbol table poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode `w` to `&Symbol` if its wrapper class is `<symbol>`. Same
/// safety story as `try_byte_string`.
///
/// # Safety
///
/// See `strings::try_byte_string` — caller asserts `w` is either a
/// fixnum or a valid heap-object pointer.
pub unsafe fn try_symbol(w: Word, symbol: crate::classes::ClassId) -> Option<&'static Symbol> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: wrapper-first invariant for any pointer-tagged Word.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() == symbol {
        // SAFETY: class match implies Symbol layout.
        Some(unsafe { &*(p as *const Symbol) })
    } else {
        None
    }
}

/// `write-to-string` shim — converts a Dylan value to a `<byte-string>`.
///
/// Currently handles `<symbol>` (returns the symbol name) and falls back to
/// `"<object>"` for any other type.  The returned Word is pointer-tagged and
/// GC-tracked in the literal pool.
///
/// # Safety
///
/// `val_raw` must be a valid Dylan Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_write_to_string(val_raw: u64) -> u64 {
    let val = Word::from_raw(val_raw);
    if let Some(sym) = unsafe { try_symbol(val, crate::classes::ClassId::SYMBOL) } {
        // Return the symbol's own name `<byte-string>` directly.
        return sym.name.raw();
    }
    // Fallback: return a static `"<object>"` literal.
    crate::intern_string_literal("<object>").raw()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::try_byte_string;

    #[test]
    fn intern_returns_same_word_for_same_name() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let st = SymbolTable::new();
        let a = st.intern("foo", &heap, &ct);
        let b = st.intern("foo", &heap, &ct);
        assert_eq!(a, b);
    }

    #[test]
    fn intern_distinct_for_distinct_names() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let st = SymbolTable::new();
        let a = st.intern("foo", &heap, &ct);
        let b = st.intern("bar", &heap, &ct);
        assert_ne!(a, b);
    }

    #[test]
    fn symbol_name_points_at_byte_string() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let st = SymbolTable::new();
        let w = st.intern("hello", &heap, &ct);
        // SAFETY: `w` came back from `intern`.
        let sym = unsafe { try_symbol(w, ct.symbol()) }.expect("class matches");
        // SAFETY: `sym.name` is the byte-string pointer planted by intern.
        let bs = unsafe { try_byte_string(sym.name, ct.byte_string()) }.expect("name is bytestr");
        // SAFETY: bs points at live allocation.
        assert_eq!(unsafe { bs.as_str() }, Some("hello"));
    }
}
