//! Precise tracer over the root set.
//!
//! Sprint 10 produced an inspection snapshot only. Sprint 11 still
//! produces a snapshot but the per-class scan + size-of dispatch now
//! goes through `ClassMetadata::scan` / `::size_of` — the same path
//! the collector uses. Adding a new class adds an entry to the seed
//! table and no other code changes.

use std::collections::HashSet;

use crate::classes::{ClassId, ClassTable, class_metadata_for};
use crate::heap::Heap;
use crate::immediates::wrapper_of_unchecked;
use crate::roots::RootSet;
use crate::word::Word;
use crate::wrapper::Wrapper;

/// One row in the trace snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapObjectInfo {
    /// Untagged heap address (the wrapper sits at offset 0).
    pub addr: usize,
    pub class: ClassId,
    /// Total byte footprint INCLUDING the wrapper header.
    pub size: usize,
}

/// Result of a trace.
#[derive(Clone, Debug)]
pub struct HeapTrace {
    pub objects: Vec<HeapObjectInfo>,
    pub root_count: usize,
}

impl HeapTrace {
    /// Pretty multi-line snapshot for tests and `:dump-heap`.
    pub fn format(&self, classes: &ClassTable) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "HeapTrace: {} object(s) reachable from {} root(s)",
            self.objects.len(),
            self.root_count
        );
        for (i, info) in self.objects.iter().enumerate() {
            let name = classes.get(info.class).name.as_str();
            let _ = writeln!(
                s,
                "  [{i}] {name} @ {:#x}  ({} bytes)",
                info.addr, info.size
            );
        }
        s
    }

    pub fn count_of(&self, class: ClassId) -> usize {
        self.objects.iter().filter(|o| o.class == class).count()
    }
}

/// Trace from every static root and every heap-reachable sub-pointer.
pub fn trace_heap(roots: &RootSet, heap: &Heap, classes: &ClassTable) -> HeapTrace {
    let mut state = TraceState {
        visited: HashSet::new(),
        objects: Vec::new(),
    };
    for &root_ptr in &roots.statics {
        // SAFETY: caller pinky-promises every entry in `statics` is a
        // valid `*const Word`.
        let w = unsafe { *root_ptr };
        state.visit_word(w);
    }
    let _ = heap; // tracer doesn't actually need heap any more — all
    let _ = classes; // dispatch goes through class metadata.
    HeapTrace {
        objects: state.objects,
        root_count: roots.statics.len(),
    }
}

struct TraceState {
    visited: HashSet<usize>,
    objects: Vec<HeapObjectInfo>,
}

impl TraceState {
    fn visit_word(&mut self, w: Word) {
        let Some(addr) = pointer_addr(w) else { return };
        if !self.visited.insert(addr) {
            return;
        }
        // SAFETY: `addr` is either inside the moveable heap or in a
        // pinned static cell. Either way the first 8 bytes are a
        // valid Wrapper.
        let wrapper: Wrapper = match unsafe { wrapper_of_unchecked(w) } {
            Some(w) => w,
            None => return,
        };
        if wrapper.is_forwarded() {
            // Should never happen during a trace from live roots, but
            // guard against it anyway.
            return;
        }
        let class = wrapper.class();
        let metadata = class_metadata_for(class);
        // SAFETY: addr's first 8 bytes are a Wrapper of `class`; the
        // metadata's size_of function expects exactly that layout.
        let size = unsafe { (metadata.size_of)(addr) };
        self.objects.push(HeapObjectInfo { addr, class, size });
        // Collect sub-pointers into a local Vec so we don't keep a
        // borrow into the heap memory across the recursive visit.
        let mut children: Vec<Word> = Vec::new();
        // SAFETY: same — class matches the data at addr.
        unsafe {
            (metadata.scan)(addr, &mut |slot| {
                children.push(*slot);
            });
        }
        for w in children {
            self.visit_word(w);
        }
    }
}

fn pointer_addr(w: Word) -> Option<usize> {
    w.as_ptr::<u8>().map(|p| p as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::immediates::Immediates;
    use crate::static_area::StaticArea;
    use crate::symbols::SymbolTable;

    #[test]
    fn empty_root_set_yields_empty_trace() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let roots = RootSet::new();
        let t = trace_heap(&roots, &heap, &ct);
        assert_eq!(t.objects.len(), 0);
        assert_eq!(t.root_count, 0);
    }

    #[test]
    fn fixnum_root_doesnt_create_objects() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let mut roots = RootSet::new();
        let w = Word::from_fixnum(42).unwrap();
        roots.add_static(&w as *const Word);
        let t = trace_heap(&roots, &heap, &ct);
        assert_eq!(t.objects.len(), 0);
    }

    #[test]
    fn immediate_roots_traced_as_pinned_boolean_and_nil() {
        let heap = Heap::new();
        let area = StaticArea::new();
        let ct = ClassTable::new();
        let imm = Immediates::new(&area, &ct);
        let mut roots = RootSet::new();
        roots.add_static(&imm.true_ as *const Word);
        roots.add_static(&imm.false_ as *const Word);
        roots.add_static(&imm.nil as *const Word);
        let t = trace_heap(&roots, &heap, &ct);
        assert_eq!(t.objects.len(), 3);
        assert_eq!(t.count_of(ct.boolean()), 2);
        assert_eq!(t.count_of(ct.empty_list()), 1);
    }

    #[test]
    fn symbol_root_pulls_in_its_name_bytestring() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let st = SymbolTable::new();
        let sym = st.intern("hi", &heap, &ct);
        let mut roots = RootSet::new();
        roots.add_static(&sym as *const Word);
        let t = trace_heap(&roots, &heap, &ct);
        assert_eq!(t.objects.len(), 2);
        assert_eq!(t.count_of(ct.symbol()), 1);
        assert_eq!(t.count_of(ct.byte_string()), 1);
    }
}
