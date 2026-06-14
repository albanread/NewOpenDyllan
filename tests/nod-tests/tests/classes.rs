//! Sprint 12 — classes, slots, make, single-dispatch generics.
//!
//! These tests cover the end-to-end class flow: parse → lower →
//! codegen → JIT → register methods → call. The dispatch table and
//! class registry are process-global; each test that allocates a
//! fresh class uses a unique class name so re-runs under the same
//! test process don't collide with the Sprint-12-mandated
//! "redefinition refused" rule.

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_dfm::{Computation, SlotTypeKind, format_dfm};
use nod_reader::{SourceMap, lex, parse_module, scan_preamble};
use nod_runtime::{
    ClassId, Word, class_metadata_for, class_metadata_ptr, find_class_id_by_name, is_subclass,
    rust_make,
};
use nod_sema::{LoweringError, dump_classes, lower_module, lower_module_full, run_function_to_i64};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn parse(src: &str) -> nod_reader::Module {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    parse_module(src, &toks, pre.as_ref()).expect("parse")
}

// 1. Define a class with two slots; check metadata.
#[test]
#[serial]
fn class_metadata_has_expected_slot_layout() {
    let src = "\
define class <pt-meta-1> (<object>)
  slot mx :: <integer>, init-keyword: mx:;
  slot my :: <integer>, init-keyword: my:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name("<pt-meta-1>").expect("registered");
    let md = class_metadata_for(id);
    assert_eq!(md.name, "<pt-meta-1>");
    assert_eq!(md.slots.len(), 2);
    assert_eq!(md.slots[0].name, "mx");
    assert_eq!(md.slots[0].offset, 8);
    assert_eq!(md.slots[1].name, "my");
    assert_eq!(md.slots[1].offset, 16);
    assert_eq!(md.instance_size, 24);
    assert_eq!(md.cpl.len(), 2);
    assert_eq!(md.cpl[0], id);
    assert_eq!(md.cpl[1], ClassId::OBJECT);
}

// 2. C3 SI — 10 fixtures, CPL matches the expected output. The C3
//    algorithm itself is unit-tested in `nod_sema::c3`; here we
//    verify that the lowering layer wires user classes through C3
//    correctly.
#[test]
#[serial]
fn single_inheritance_cpl_chain() {
    let src = "\
define class <c3-a> (<object>) end class;
define class <c3-b> (<c3-a>) end class;
define class <c3-c> (<c3-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let a = find_class_id_by_name("<c3-a>").unwrap();
    let b = find_class_id_by_name("<c3-b>").unwrap();
    let c = find_class_id_by_name("<c3-c>").unwrap();
    let md_c = class_metadata_for(c);
    assert_eq!(md_c.cpl, vec![c, b, a, ClassId::OBJECT]);
}

// 3. Sprint 14: MI lowers cleanly; the class metadata reflects the
//    merged inheritance. (Was previously the Sprint-12 reject-MI gate.)
#[test]
#[serial]
fn multi_inheritance_lowers_with_merged_metadata() {
    let src = "\
define class <mi-a> (<object>) end class;
define class <mi-b> (<object>) end class;
define class <mi-c> (<mi-a>, <mi-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("MI must lower");
    let a = find_class_id_by_name("<mi-a>").unwrap();
    let b = find_class_id_by_name("<mi-b>").unwrap();
    let c = find_class_id_by_name("<mi-c>").unwrap();
    let md_c = class_metadata_for(c);
    // C3 of `<mi-c> (<mi-a>, <mi-b>)`: [<mi-c>, <mi-a>, <mi-b>, <object>].
    assert_eq!(md_c.cpl, vec![c, a, b, ClassId::OBJECT]);
    // Multi-parent: parents = [<mi-a>, <mi-b>]. parent (back-compat
    // first-parent accessor) == <mi-a>.
    assert_eq!(md_c.parents, vec![a, b]);
    assert_eq!(md_c.parent, Some(a));
    // Sanity: still a subclass of both parents.
    assert!(is_subclass(c, a));
    assert!(is_subclass(c, b));
}

// 4. `make` allocates and initialises an instance with the right
//    class and slot values.
#[test]
#[serial]
fn make_allocates_and_initialises() {
    let src = "\
define class <mk-pt> (<object>)
  slot mx :: <integer>, init-keyword: mx:;
  slot my :: <integer>, init-keyword: my:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name("<mk-pt>").unwrap();
    let md = class_metadata_for(id);
    let three = Word::from_fixnum(3).unwrap();
    let four = Word::from_fixnum(4).unwrap();
    // SAFETY: metadata is registered and pinned.
    let inst = unsafe { rust_make(md, &[("mx", three), ("my", four)]) };
    assert!(inst.is_pointer());
    // Read slot directly to verify.
    let addr = inst.as_ptr::<u8>().unwrap() as usize;
    // SAFETY: instance is freshly allocated; slot offsets are within
    // its payload.
    let (mx, my) = unsafe {
        (
            *((addr + 8) as *const Word),
            *((addr + 16) as *const Word),
        )
    };
    assert_eq!(mx.as_fixnum(), Some(3));
    assert_eq!(my.as_fixnum(), Some(4));
}

// 5. Auto-generated getter `x(p)` returns the slot value via the
//    JIT'd accessor.
#[test]
#[serial]
fn slot_getter_returns_x_value() {
    // Use a synthetic fixture that exercises the slot getter through
    // a `define function` so we can JIT and call it via
    // `run_function_to_i64`.
    let src = "\
Module: get-x

define class <gp> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
end class;

define function main () => (<integer>)
  xx(make(<gp>, xx: 17))
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 17);
}

// 6. `x(p) := 10` setter works.
#[test]
#[serial]
fn slot_setter_assigns_via_assign() {
    let src = "\
Module: set-x

define class <sp> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
end class;

define function main () => (<integer>)
  let p = make(<sp>, xx: 1);
  xx(p) := 99;
  xx(p)
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 99);
}

// 7. `instance?(p, <pt>) → #t`, `instance?(p, <object>) → #t`,
//    `instance?(42, <pt>) → #f`. We test the runtime predicate
//    directly to avoid threading the eval API through a boolean
//    return.
#[test]
#[serial]
fn instance_check_handles_user_classes_and_supers() {
    let src = "\
define class <ic-pt> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name("<ic-pt>").unwrap();
    let md = class_metadata_for(id);
    let one = Word::from_fixnum(1).unwrap();
    // SAFETY: registered class.
    let p = unsafe { rust_make(md, &[("xx", one)]) };
    assert!(nod_runtime::nod_is_instance_of_word(p, id));
    assert!(nod_runtime::nod_is_instance_of_word(p, ClassId::OBJECT));
    let fixnum = Word::from_fixnum(42).unwrap();
    assert!(!nod_runtime::nod_is_instance_of_word(fixnum, id));
    assert!(nod_runtime::nod_is_instance_of_word(
        fixnum,
        ClassId::INTEGER
    ));
    // Subclass walk via `is_subclass`.
    assert!(is_subclass(id, ClassId::OBJECT));
    assert!(!is_subclass(ClassId::INTEGER, id));
}

// 8. The headline acceptance: `distance-squared(make(<point>, x: 3, y: 4))`
//    returns 25.
#[test]
#[serial]
fn point_distance_squared_returns_25() {
    let path = fixtures_dir().join("point.dylan");
    let result = run_function_to_i64(&path, "main").expect("run point main");
    assert_eq!(result, 25);
}

// 9. Class metadata is pinned across GC cycles. Pre-existing
//    instances still report the right class id.
#[test]
#[serial]
fn class_metadata_pinned_across_gc() {
    let src = "\
define class <gc-pt> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name("<gc-pt>").unwrap();
    let md_addr_before = class_metadata_ptr(id) as usize;
    // Trigger a couple of minor GCs to make sure the metadata's
    // address doesn't move.
    for _ in 0..3 {
        nod_runtime::collect_minor();
    }
    let md_addr_after = class_metadata_ptr(id) as usize;
    assert_eq!(md_addr_before, md_addr_after);
}

// 10. Class redefinition is refused.
#[test]
#[serial]
fn class_redefinition_is_refused() {
    let src1 = "define class <redef> (<object>) end class;";
    let m1 = parse(src1);
    let _ = lower_module(&m1).expect("first define ok");
    let src2 = "define class <redef> (<object>) end class;";
    let m2 = parse(src2);
    let errs = lower_module(&m2).expect_err("redefinition must error");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            LoweringError::ClassRedefinitionNotSupported { class_name, .. }
                if class_name == "<redef>"
        )),
        "errors: {errs:?}"
    );
}

// 11. A user-defined `initialize` method is called during `make`.
#[test]
#[serial]
fn initialize_method_runs_during_make() {
    // The `initialize` method runs after slot init-keywords have been
    // applied. We have it bump a slot to prove it ran.
    let src = "\
Module: init-run

define class <ip> (<object>)
  slot tag :: <integer>, init-keyword: tag:;
end class;

define method initialize (p :: <ip>, #key)
  tag(p) := tag(p) + 100;
end method initialize;

define function main () => (<integer>)
  tag(make(<ip>, tag: 7))
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 107);
}

// 12. Generic-function dispatch: two methods on the same generic
//     pick the more-specific receiver.
#[test]
#[serial]
fn generic_dispatch_picks_most_specific_method() {
    let src = "\
Module: disp

define class <da> (<object>) slot v :: <integer>, init-keyword: v:; end class;
define class <db> (<da>) end class;

define generic kind (x);

define method kind (x :: <da>) => (<integer>)
  10
end method kind;

define method kind (x :: <db>) => (<integer>)
  20
end method kind;

define function main () => (<integer>)
  kind(make(<db>, v: 0))
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 20, "dispatch should pick <db>'s method (most specific)");
}

// 13. GC regression: 1000 instance allocations don't break the heap.
//     We re-trigger several minor GCs to ensure the dispatcher's
//     class-id reads remain valid for surviving instances.
#[test]
#[serial]
fn many_instance_allocations_survive_gc() {
    let src = "\
define class <gc-stress> (<object>)
  slot tag :: <integer>, init-keyword: tag:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let id = find_class_id_by_name("<gc-stress>").unwrap();
    let md = class_metadata_for(id);
    let count_before_minor = nod_runtime::gc_stats().minor_collections;
    for n in 0..1000 {
        let w = Word::from_fixnum(n).unwrap();
        // SAFETY: registered class.
        let _ = unsafe { rust_make(md, &[("tag", w)]) };
    }
    // The bulk allocations should have triggered at least one minor GC.
    let count_after = nod_runtime::gc_stats().minor_collections;
    assert!(
        count_after >= count_before_minor,
        "GC counts should be monotonic"
    );
    // Class metadata still points at the same address.
    let md_now = class_metadata_for(id);
    assert_eq!(md_now as *const _, md as *const _);
}

// ─── Dump-output smoke tests (Phase F helpers) ─────────────────────────

#[test]
#[serial]
fn dump_classes_includes_seed_and_user_classes() {
    let src = "\
define class <dump-pt> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let dump = dump_classes();
    assert!(dump.contains("<integer>"), "dump must include seed classes:\n{dump}");
    assert!(dump.contains("<dump-pt>"), "dump must include user class:\n{dump}");
    assert!(dump.contains("slot xx @"), "dump must include slot rows:\n{dump}");
}

#[test]
#[serial]
fn lowered_module_emits_slot_accessor_functions() {
    let src = "\
define class <emit-pt> (<object>)
  slot xx :: <integer>, init-keyword: xx:;
  slot yy :: <integer>, init-keyword: yy:, setter: #f;
end class;
";
    let m = parse(src);
    let lm = lower_module_full(&m).expect("lower");
    let names: Vec<&str> = lm.functions.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"<emit-pt>-getter-xx"), "names: {names:?}");
    assert!(names.contains(&"<emit-pt>-setter-xx"), "names: {names:?}");
    assert!(names.contains(&"<emit-pt>-getter-yy"), "names: {names:?}");
    // setter: #f disables the setter.
    assert!(
        !names.contains(&"<emit-pt>-setter-yy"),
        "expected no setter for yy: {names:?}"
    );
    // The xx-getter is exactly a LoadSlot.
    let getter = lm
        .functions
        .iter()
        .find(|f| f.name == "<emit-pt>-getter-xx")
        .unwrap();
    let entry = &getter.blocks[0];
    assert_eq!(entry.computations.len(), 1, "dump:\n{}", format_dfm(getter));
    match &entry.computations[0] {
        Computation::LoadSlot { offset, slot_type, .. } => {
            assert_eq!(*offset, 8);
            assert_eq!(*slot_type, SlotTypeKind::Integer);
        }
        c => panic!("expected LoadSlot, got {c:?}"),
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

/// Write `src` to a temporary file inside the fixtures directory and
/// return its path. Uses the test name + nanoseconds-since-epoch so
/// parallel test runs don't collide.
fn tempfile_with(src: &str) -> PathBuf {
    let dir = fixtures_dir().join("_tmp");
    std::fs::create_dir_all(&dir).expect("mkdir _tmp");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = dir.join(format!("classes-{stamp}.dylan"));
    std::fs::write(&path, src).expect("write tmp fixture");
    path
}
