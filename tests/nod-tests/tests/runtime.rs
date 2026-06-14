//! Sprint 09 — `nod-runtime` tagged values + heap + `instance?` lowering.

use nod_runtime::{ClassId, ClassTable, FIXNUM_MAX, FIXNUM_MIN, Heap, StaticArea, Word};
use nod_sema::eval_expr_to_string;

#[test]
fn word_zero_round_trip() {
    let w = Word::from_fixnum(0).expect("zero is in range");
    assert_eq!(w.as_fixnum(), Some(0));
}

#[test]
fn word_fixnum_bounds() {
    // i64::MAX is well outside the 63-bit signed fixnum range.
    assert!(Word::from_fixnum(i64::MAX).is_err());
    // FIXNUM_MAX (= 2^62 - 1) is the last valid fixnum.
    assert!(Word::from_fixnum(FIXNUM_MAX).is_ok());
    // FIXNUM_MIN (= -2^62) is the most-negative valid fixnum.
    assert!(Word::from_fixnum(FIXNUM_MIN).is_ok());
    assert!(Word::from_fixnum(FIXNUM_MAX + 1).is_err());
    assert!(Word::from_fixnum(FIXNUM_MIN - 1).is_err());
}

#[test]
fn word_tag_invariants() {
    let w = Word::from_fixnum(5).expect("in range");
    assert!(w.is_fixnum());
    assert!(!w.is_pointer());
    // Encoding: 5 << 1 = 10
    assert_eq!(w.raw(), 10);
}

#[test]
fn heap_alloc_object_returns_tagged_pointer_with_class() {
    let heap = Heap::new();
    let ct = ClassTable::new();
    let w = heap.alloc_object(ct.string(), 16);
    assert!(w.is_pointer());
    assert!(!w.is_fixnum());
    let wrap = heap.wrapper_of(w).expect("wrapper inside heap");
    assert_eq!(wrap.class(), ct.string());
}

#[test]
fn static_area_addresses_are_stable() {
    let area = StaticArea::new();
    let r = area.alloc(7_u64);
    let addr = r as *const u64;
    // Many further allocs must not invalidate `r`.
    for n in 0u64..256 {
        let _ = area.alloc(n);
    }
    assert_eq!(*r, 7);
    assert_eq!(r as *const u64, addr);
}

#[test]
fn instance_check_integer_is_true() {
    let s = eval_expr_to_string("instance?(42, <integer>)").expect("eval ok");
    assert_eq!(s, "#t");
}

#[test]
fn instance_check_boolean_is_false_on_integer() {
    // Sprint 09: <boolean> instance? folds to `#f` for everything —
    // see codegen note. Once the immediate scheme grows (Sprint 10+)
    // this returns `#t` for #t/#f and `#f` for integers.
    let s = eval_expr_to_string("instance?(42, <boolean>)").expect("eval ok");
    assert_eq!(s, "#f");
}

#[test]
fn fixnum_arithmetic_regression() {
    // Exercises the tagged-fixnum AddInt + MulInt lowering end-to-end.
    // Dylan has NO operator precedence (flat, left-associative, per the
    // DRM), so `1 + 2 * 3` is `(1 + 2) * 3 = 9`, not C's `1 + (2*3) = 7`.
    // (Sprint 51e fixed the parser's precedence.)
    let s = eval_expr_to_string("1 + 2 * 3").expect("eval ok");
    assert_eq!(s, "9");
}

#[test]
fn fixnum_overflow_at_lowering() {
    // 2^62 is FIXNUM_MAX + 1 — the smallest literal Sprint 09 rejects.
    let src = "define constant big = 4611686018427387904;";
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add("<overflow>", src.to_string())
        .expect("source map add");
    let toks = nod_reader::lex(src, file_id);
    let pre = nod_reader::scan_preamble(src);
    let module = nod_reader::parse_module(src, &toks, pre.as_ref()).expect("parse ok");
    let errs = nod_sema::lower_module(&module).expect_err("expected overflow");
    assert!(
        errs.iter()
            .any(|e| matches!(e, nod_sema::LoweringError::IntegerOverflow { .. })),
        "expected IntegerOverflow in {errs:?}"
    );
}

#[test]
fn class_metadata_identity_stable() {
    // ClassId values are static across `ClassTable` instances. The
    // dispatch caches we build in Sprint 13 depend on this invariant.
    let a = ClassTable::new();
    let b = ClassTable::new();
    assert_eq!(a.integer(), b.integer());
    assert_eq!(a.integer(), ClassId::INTEGER);
    assert_eq!(a.boolean(), b.boolean());
    assert_eq!(a.string(), b.string());
}
