//! Sprint 10 — heap-allocated `<byte-string>`, `<symbol>`,
//! `<simple-object-vector>`, richer immediates, `format-out` shim,
//! root set + tracer.

use nod_runtime::{
    ClassTable, Heap, Immediates, RootSet, StaticArea, SymbolTable, Word,
    install_test_writer, take_test_writer, trace_heap, try_byte_string,
    try_simple_object_vector, try_simple_object_vector_mut, try_symbol,
    uninstall_test_writer,
};
use nod_sema::eval_expr_to_string;

// ─── (1) ByteString round-trip ────────────────────────────────────────────

#[test]
fn byte_string_alloc_round_trips_bytes() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let w = heap.alloc_byte_string("hello", &ct);
    let wrap = heap.wrapper_of(w).expect("wrapper inside heap");
    assert_eq!(wrap.class(), ct.byte_string());
    // SAFETY: `w` came back from `alloc_byte_string` in the same heap.
    let bs = unsafe { try_byte_string(w, ct.byte_string()) }.expect("class matches");
    assert_eq!(bs.len, 5);
    // SAFETY: bs is a live pointer into the heap.
    assert_eq!(unsafe { bs.bytes() }, b"hello");
    // SAFETY: forwarded.
    assert_eq!(unsafe { bs.as_str() }, Some("hello"));
}

// ─── (2) Symbol intern stable ─────────────────────────────────────────────

#[test]
fn symbol_intern_returns_same_word() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let st = SymbolTable::new();
    let a = st.intern("foo", &heap, &ct);
    let b = st.intern("foo", &heap, &ct);
    assert_eq!(a, b);
    let c = st.intern("bar", &heap, &ct);
    assert_ne!(a, c);
}

// ─── (3) Vector round-trip ────────────────────────────────────────────────

#[test]
fn vector_round_trip_with_fixnums() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let w = heap.alloc_simple_object_vector(4, &ct);
    // SAFETY: `w` is unique; we have no other live reference.
    let v = unsafe { try_simple_object_vector_mut(w, ct.simple_object_vector()) }
        .expect("class matches");
    // SAFETY: same.
    let slots = unsafe { v.slots_mut() };
    for (i, slot) in slots.iter_mut().enumerate() {
        *slot = Word::from_fixnum(10 + i as i64).unwrap();
    }
    // SAFETY: re-borrow read-only.
    let v2 = unsafe { try_simple_object_vector(w, ct.simple_object_vector()) }
        .expect("class matches");
    // SAFETY: live.
    let read = unsafe { v2.slots() };
    assert_eq!(read.len(), 4);
    for (i, slot) in read.iter().enumerate() {
        assert_eq!(slot.as_fixnum(), Some(10 + i as i64));
    }
}

// ─── (4)/(5)/(6) instance? on richer immediates ──────────────────────────

#[test]
fn instance_t_is_boolean() {
    let s = eval_expr_to_string("instance?(#t, <boolean>)").expect("eval ok");
    assert_eq!(s, "#t");
}

#[test]
fn instance_f_is_boolean() {
    let s = eval_expr_to_string("instance?(#f, <boolean>)").expect("eval ok");
    assert_eq!(s, "#t");
}

#[test]
fn instance_42_is_not_boolean() {
    let s = eval_expr_to_string("instance?(42, <boolean>)").expect("eval ok");
    assert_eq!(s, "#f");
}

// ─── (7)/(8) instance? on <byte-string> ──────────────────────────────────

#[test]
fn instance_hello_is_byte_string() {
    let s = eval_expr_to_string("instance?(\"hello\", <byte-string>)").expect("eval ok");
    assert_eq!(s, "#t");
}

#[test]
fn instance_42_is_not_byte_string() {
    let s = eval_expr_to_string("instance?(42, <byte-string>)").expect("eval ok");
    assert_eq!(s, "#f");
}

// ─── (9) #t identity ──────────────────────────────────────────────────────

#[test]
fn t_equals_t_singleton_identity() {
    // Both `#t` lower to the same singleton pointer, so even integer
    // equality (`=`) on the raw words sees them as identical.
    let s = eval_expr_to_string("if (#t) 1 else 0 end").expect("eval ok");
    assert_eq!(s, "1");
    let s2 = eval_expr_to_string("if (#f) 1 else 0 end").expect("eval ok");
    assert_eq!(s2, "0");
}

// ─── (10) format-out("hello\n") via eval ─────────────────────────────────

#[test]
fn format_out_hello_prints_text() {
    install_test_writer();
    let _ = eval_expr_to_string("format-out(\"hello\\n\")").expect("eval ok");
    let buf = take_test_writer().unwrap_or_default();
    uninstall_test_writer();
    assert_eq!(&buf, b"hello\n");
}

// ─── (11) format-out("answer: %d\n", 42) ─────────────────────────────────

#[test]
fn format_out_d_directive_prints_fixnum() {
    install_test_writer();
    let _ = eval_expr_to_string("format-out(\"answer: %d\\n\", 42)").expect("eval ok");
    let buf = take_test_writer().unwrap_or_default();
    uninstall_test_writer();
    assert_eq!(&buf, b"answer: 42\n");
}

// ─── (12) Tracer smoke — three objects, right classes ────────────────────

#[test]
fn tracer_snapshot_counts_match() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    // Allocate one string, one vector (size 2), one symbol.
    let s = heap.alloc_byte_string("abc", &ct);
    let v = heap.alloc_simple_object_vector(2, &ct);
    let st = SymbolTable::new();
    let sym = st.intern("name", &heap, &ct);
    // Vector slots reference the string + symbol.
    // SAFETY: w is unique, single-threaded.
    let vec = unsafe { try_simple_object_vector_mut(v, ct.simple_object_vector()) }
        .expect("vector class");
    // SAFETY: same.
    let slots = unsafe { vec.slots_mut() };
    slots[0] = s;
    slots[1] = sym;

    let mut roots = RootSet::new();
    roots.add_static(&v as *const Word);
    let trace = trace_heap(&roots, &heap, &ct);
    // Reachable from `v`: the vector, its string slot, its symbol slot,
    // and the symbol's own name byte-string — 4 objects.
    assert_eq!(trace.objects.len(), 4);
    assert_eq!(trace.count_of(ct.byte_string()), 2);
    assert_eq!(trace.count_of(ct.simple_object_vector()), 1);
    assert_eq!(trace.count_of(ct.symbol()), 1);
}

// ─── (13) Tracer cycle handling ──────────────────────────────────────────

#[test]
fn tracer_handles_cycles_between_vectors() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let a = heap.alloc_simple_object_vector(1, &ct);
    let b = heap.alloc_simple_object_vector(1, &ct);
    // SAFETY: a, b are unique.
    let av = unsafe { try_simple_object_vector_mut(a, ct.simple_object_vector()) }.unwrap();
    // SAFETY: same.
    let aslots = unsafe { av.slots_mut() };
    aslots[0] = b;
    // SAFETY: same.
    let bv = unsafe { try_simple_object_vector_mut(b, ct.simple_object_vector()) }.unwrap();
    // SAFETY: same.
    let bslots = unsafe { bv.slots_mut() };
    bslots[0] = a;

    let mut roots = RootSet::new();
    roots.add_static(&a as *const Word);
    // If cycle detection fails, this loops forever. Plain assertion is
    // sufficient — test timeout would catch infinite recursion.
    let trace = trace_heap(&roots, &heap, &ct);
    assert_eq!(trace.objects.len(), 2);
    assert_eq!(trace.count_of(ct.simple_object_vector()), 2);
}

// ─── Bonus: trace snapshot pretty-prints ─────────────────────────────────

#[test]
fn tracer_format_contains_class_names() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let s = heap.alloc_byte_string("hi", &ct);
    let st = SymbolTable::new();
    let sym = st.intern("name", &heap, &ct);
    let v = heap.alloc_simple_object_vector(2, &ct);
    // SAFETY: vector is unique, single-threaded.
    let vp = unsafe { try_simple_object_vector_mut(v, ct.simple_object_vector()) }.unwrap();
    // SAFETY: same.
    let slots = unsafe { vp.slots_mut() };
    slots[0] = s;
    slots[1] = sym;
    let mut roots = RootSet::new();
    roots.add_static(&v as *const Word);
    let trace = trace_heap(&roots, &heap, &ct);
    let dump = trace.format(&ct);
    eprintln!("--- TRACE DUMP ---\n{dump}--- /TRACE DUMP ---");
    assert!(dump.contains("<byte-string>"), "dump:\n{dump}");
    assert!(dump.contains("<simple-object-vector>"), "dump:\n{dump}");
    assert!(dump.contains("<symbol>"), "dump:\n{dump}");
    assert!(dump.contains("4 object(s) reachable from 1 root(s)"));
}

// ─── Bonus: immediates carry the right class through the wrapper ─────────

#[test]
fn immediates_wrappers_say_boolean_and_empty_list() {
    let area = StaticArea::new();
    let ct = ClassTable::new();
    let imm = Immediates::new(&area, &ct);
    assert!(imm.true_.is_pointer());
    assert!(imm.false_.is_pointer());
    assert!(imm.nil.is_pointer());

    // SAFETY: addresses came from `StaticArea::alloc`; layout is
    // `WrapperCell` whose first cell is a Wrapper.
    let wt = unsafe { nod_runtime::wrapper_of_unchecked(imm.true_) }.unwrap();
    let wn = unsafe { nod_runtime::wrapper_of_unchecked(imm.nil) }.unwrap();
    assert_eq!(wt.class(), ct.boolean());
    assert_eq!(wn.class(), ct.empty_list());
}

// ─── Bonus: symbol decoding ──────────────────────────────────────────────

#[test]
fn symbol_decode_yields_name() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let st = SymbolTable::new();
    let w = st.intern("hi", &heap, &ct);
    // SAFETY: `w` came back from intern.
    let sym = unsafe { try_symbol(w, ct.symbol()) }.expect("class matches");
    // SAFETY: sym.name is a byte-string pointer.
    let bs = unsafe { try_byte_string(sym.name_word(), ct.byte_string()) }.expect("class matches");
    // SAFETY: bs points at live allocation.
    assert_eq!(unsafe { bs.as_str() }, Some("hi"));
}
