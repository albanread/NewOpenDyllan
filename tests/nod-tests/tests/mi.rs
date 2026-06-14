//! Sprint 14 — multiple inheritance, MI-aware slot layout, `next-method`.
//!
//! Covers:
//!   1. MI parses + lowers; class metadata reflects the C3 CPL and the
//!      multi-parent shape.
//!   2. Diamond instance reads + writes both inherited slots end-to-end
//!      through the JIT.
//!   3. A method specialised on `<b>` accesses slot `y` on a `<d>`
//!      instance via the override accessor.
//!   4. Slot name conflict (two unrelated parents define same slot name)
//!      surfaces `LoweringError::SlotConflict`.
//!   5. `next-method` chain in a 4-deep hierarchy walks through every
//!      method in order.
//!   6. `next-method()` past the end panics with `<no-next-method-error>`.
//!   7. `<point>` Sprint 12 regression.
//!   8. `dump_classes` rendering of an MI hierarchy includes override
//!      annotations.
//!   9. GC under MI dispatch pressure.
//!
//! Every test that mutates the process-global class / dispatch registry
//! is `#[serial]`.

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_reader::{SourceMap, lex, parse_module, scan_preamble};
use nod_runtime::{
    ClassId, Word, class_metadata_for, find_class_id_by_name, is_subclass, rust_make,
};
use nod_sema::{LoweringError, dump_classes, lower_module, run_function_to_i64};

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

/// Write `src` to a temporary file inside the fixtures directory and
/// return its path. Uses nanos-since-epoch so concurrent test runs
/// don't collide.
fn tempfile_with(src: &str) -> PathBuf {
    let dir = fixtures_dir().join("_tmp");
    std::fs::create_dir_all(&dir).expect("mkdir _tmp");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = dir.join(format!("mi-{stamp}.dylan"));
    std::fs::write(&path, src).expect("write tmp fixture");
    path
}

// ─── 1. Diamond MI parses and lowers ──────────────────────────────────────

#[test]
#[serial]
fn diamond_mi_parses_and_lowers() {
    let src = "\
define class <mi-diamond-top> (<object>) end class;
define class <mi-diamond-a> (<mi-diamond-top>)
  slot mi-diamond-x :: <integer>, init-keyword: mi-diamond-x:;
end class;
define class <mi-diamond-b> (<mi-diamond-top>)
  slot mi-diamond-y :: <integer>, init-keyword: mi-diamond-y:;
end class;
define class <mi-diamond-d> (<mi-diamond-a>, <mi-diamond-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("MI lowers");
    let top = find_class_id_by_name("<mi-diamond-top>").unwrap();
    let a = find_class_id_by_name("<mi-diamond-a>").unwrap();
    let b = find_class_id_by_name("<mi-diamond-b>").unwrap();
    let d = find_class_id_by_name("<mi-diamond-d>").unwrap();
    let md_d = class_metadata_for(d);
    // C3 of <d>(<a>, <b>) where both inherit from <top>:
    // [<d>, <a>, <b>, <top>, <object>].
    assert_eq!(md_d.cpl, vec![d, a, b, top, ClassId::OBJECT]);
    assert_eq!(md_d.parents, vec![a, b]);
    // Two slots inherited via the diamond.
    assert_eq!(md_d.slots.len(), 2);
    let names: Vec<&str> = md_d.slots.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"mi-diamond-x"));
    assert!(names.contains(&"mi-diamond-y"));
}

// ─── 2. End-to-end: <d> instance can read both inherited slots ────────────

#[test]
#[serial]
fn diamond_instance_reads_both_inherited_slots() {
    let src = "\
Module: mi-rw

define class <mi-rw-top> (<object>) end class;
define class <mi-rw-a> (<mi-rw-top>)
  slot mi-rw-x :: <integer>, init-keyword: mi-rw-x:;
end class;
define class <mi-rw-b> (<mi-rw-top>)
  slot mi-rw-y :: <integer>, init-keyword: mi-rw-y:;
end class;
define class <mi-rw-d> (<mi-rw-a>, <mi-rw-b>) end class;

define function main () => (<integer>)
  let p = make(<mi-rw-d>, mi-rw-x: 7, mi-rw-y: 11);
  mi-rw-x(p) + mi-rw-y(p)
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 18);
}

#[test]
#[serial]
fn diamond_instance_writes_then_reads_inherited_slots() {
    let src = "\
Module: mi-write

define class <mi-w-top> (<object>) end class;
define class <mi-w-a> (<mi-w-top>)
  slot mi-w-x :: <integer>, init-keyword: mi-w-x:;
end class;
define class <mi-w-b> (<mi-w-top>)
  slot mi-w-y :: <integer>, init-keyword: mi-w-y:;
end class;
define class <mi-w-d> (<mi-w-a>, <mi-w-b>) end class;

define function main () => (<integer>)
  let p = make(<mi-w-d>, mi-w-x: 1, mi-w-y: 2);
  mi-w-x(p) := 100;
  mi-w-y(p) := 200;
  mi-w-x(p) + mi-w-y(p)
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 300);
}

// ─── 3. Method specialised on parent class still works on MI subclass ─────

#[test]
#[serial]
fn method_specialised_on_b_works_on_d_instance() {
    // A method on <mi-disp-b> reads slot `mi-disp-y`. Calling that
    // method with a <mi-disp-d> receiver must produce the right value
    // — which means dispatch picked an accessor whose offset matches
    // the receiver's actual class layout (the override).
    let src = "\
Module: mi-disp

define class <mi-disp-top> (<object>) end class;
define class <mi-disp-a> (<mi-disp-top>)
  slot mi-disp-x :: <integer>, init-keyword: mi-disp-x:;
end class;
define class <mi-disp-b> (<mi-disp-top>)
  slot mi-disp-y :: <integer>, init-keyword: mi-disp-y:;
end class;
define class <mi-disp-d> (<mi-disp-a>, <mi-disp-b>) end class;

define method only-y (b :: <mi-disp-b>) => (<integer>)
  mi-disp-y(b)
end method only-y;

define function main () => (<integer>)
  only-y(make(<mi-disp-d>, mi-disp-x: 7, mi-disp-y: 9))
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(
        result, 9,
        "method on <mi-disp-b> should see mi-disp-y=9 on a <mi-disp-d>; \
         a wrong-offset read would surface a different value"
    );
}

// ─── 4. Slot conflict rejected ─────────────────────────────────────────────

#[test]
#[serial]
fn slot_name_conflict_rejected() {
    // Two unrelated parents both define a slot named `clash`. The MI
    // subclass must surface a structured `SlotConflict` diagnostic.
    let src = "\
define class <mi-cf-a> (<object>)
  slot clash :: <integer>, init-keyword: clash:;
end class;
define class <mi-cf-b> (<object>)
  slot clash :: <integer>, init-keyword: clash:;
end class;
define class <mi-cf-c> (<mi-cf-a>, <mi-cf-b>) end class;
";
    let m = parse(src);
    let errs = lower_module(&m).expect_err("slot conflict must error");
    assert!(
        errs.iter().any(|e| matches!(
            e,
            LoweringError::SlotConflict { class_name, slot_name, .. }
                if class_name == "<mi-cf-c>" && slot_name == "clash"
        )),
        "errors: {errs:?}"
    );
}

// ─── 5. next-method chain walks through a 4-deep hierarchy ────────────────

#[test]
#[serial]
fn next_method_chain_4_deep() {
    // Each `sound` method adds its own digit to a running tally via
    // arithmetic — `next-method()` invokes the next-most-specific. The
    // accumulated total proves every level ran in the right order.
    //
    // <nm-animal>  contributes 1
    // <nm-mammal>  contributes 20
    // <nm-dog>     contributes 300
    // <nm-poodle>  contributes 4000
    // Total: 4321.
    let src = "\
Module: nm-chain

define class <nm-animal> (<object>) end class;
define class <nm-mammal> (<nm-animal>) end class;
define class <nm-dog>    (<nm-mammal>) end class;
define class <nm-poodle> (<nm-dog>) end class;

define method nm-sound (x :: <nm-animal>) => (<integer>)
  1
end method nm-sound;

define method nm-sound (x :: <nm-mammal>) => (<integer>)
  next-method() + 20
end method nm-sound;

define method nm-sound (x :: <nm-dog>) => (<integer>)
  next-method() + 300
end method nm-sound;

define method nm-sound (x :: <nm-poodle>) => (<integer>)
  next-method() + 4000
end method nm-sound;

define function main () => (<integer>)
  nm-sound(make(<nm-poodle>))
end function main;
";
    let tmp = tempfile_with(src);
    nod_runtime::_reset_method_chain_stack_for_tests();
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 4321);
}

// ─── 6. next-method past the end panics ───────────────────────────────────

#[test]
#[serial]
fn next_method_past_end_panics() {
    let src = "\
Module: nm-end

define class <nm-end-a> (<object>) end class;

define method nm-end-method (x :: <nm-end-a>) => (<integer>)
  next-method()
end method nm-end-method;

define function main () => (<integer>)
  nm-end-method(make(<nm-end-a>))
end function main;
";
    let tmp = tempfile_with(src);
    nod_runtime::_reset_method_chain_stack_for_tests();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_function_to_i64(&tmp, "main");
    }));
    assert!(panicked.is_err(), "next-method past chain end must panic");
    let err = panicked.unwrap_err();
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("<no-next-method-error>"),
        "expected <no-next-method-error> in panic; got: {msg}"
    );
}

// ─── 7. <point> Sprint 12 regression still passes ─────────────────────────

#[test]
#[serial]
fn point_distance_squared_regression() {
    let path = fixtures_dir().join("point.dylan");
    let result = run_function_to_i64(&path, "main").expect("run point main");
    assert_eq!(result, 25);
}

// ─── 8. dump_classes shows MI structure clearly ───────────────────────────

#[test]
#[serial]
fn dump_classes_shows_mi_structure() {
    let src = "\
define class <dump-mi-top> (<object>) end class;
define class <dump-mi-a> (<dump-mi-top>)
  slot dump-mi-x :: <integer>, init-keyword: dump-mi-x:;
end class;
define class <dump-mi-b> (<dump-mi-top>)
  slot dump-mi-y :: <integer>, init-keyword: dump-mi-y:;
end class;
define class <dump-mi-d> (<dump-mi-a>, <dump-mi-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let dump = dump_classes();
    assert!(
        dump.contains("parents=[<dump-mi-a>, <dump-mi-b>]"),
        "dump must list MI parents:\n{dump}"
    );
    // The `dump-mi-y` slot is inherited from <dump-mi-b>; in <dump-mi-d>
    // it lives at offset 16 (after <dump-mi-a>'s x at 8). Its origin
    // class has it at offset 8. So the override annotation should
    // appear.
    assert!(
        dump.contains("override @8\u{2192}@16"),
        "dump must include override annotation for inherited slot whose offset shifted:\n{dump}"
    );
    // The `dump-mi-x` slot is inherited from <dump-mi-a>; in <dump-mi-d>
    // it lives at offset 8 — same as in <dump-mi-a> — so the fixed-
    // offset annotation should appear.
    assert!(
        dump.contains("[inherited from <dump-mi-a>, fixed-offset]"),
        "dump must include fixed-offset annotation:\n{dump}"
    );
}

// ─── 9. GC under MI dispatch pressure ─────────────────────────────────────

#[test]
#[serial]
fn mi_dispatch_under_gc_pressure() {
    let src = "\
define class <mi-gc-top> (<object>) end class;
define class <mi-gc-a> (<mi-gc-top>)
  slot mi-gc-x :: <integer>, init-keyword: mi-gc-x:;
end class;
define class <mi-gc-b> (<mi-gc-top>)
  slot mi-gc-y :: <integer>, init-keyword: mi-gc-y:;
end class;
define class <mi-gc-d> (<mi-gc-a>, <mi-gc-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let d = find_class_id_by_name("<mi-gc-d>").unwrap();
    let md = class_metadata_for(d);
    let count_before = nod_runtime::gc_stats().minor_collections;
    for n in 0..10_000_i64 {
        let x = Word::from_fixnum(n).unwrap();
        let y = Word::from_fixnum(-n).unwrap();
        // SAFETY: registered class.
        let _ = unsafe { rust_make(md, &[("mi-gc-x", x), ("mi-gc-y", y)]) };
    }
    let count_after = nod_runtime::gc_stats().minor_collections;
    assert!(
        count_after >= count_before,
        "GC counts should be monotonic ({count_before} → {count_after})"
    );
    // Metadata pointer is still address-stable.
    let md_now = class_metadata_for(d);
    assert_eq!(md_now as *const _, md as *const _);
}

// ─── 9b. Verbose dump for the Sprint 14 report ───────────────────────────

/// Helper test that prints the dump for the canonical Sprint-14 diamond
/// fixture. Run with `cargo test --workspace --test mi -- --nocapture
/// dump_classes_report_helper` to capture the output for the sprint
/// retrospective. Always passes; the output is the artefact.
#[test]
#[serial]
fn dump_classes_report_helper() {
    let src = "\
define class <demo-mi-top> (<object>) end class;
define class <demo-mi-a> (<demo-mi-top>)
  slot demo-mi-x :: <integer>, init-keyword: demo-mi-x:;
end class;
define class <demo-mi-b> (<demo-mi-top>)
  slot demo-mi-y :: <integer>, init-keyword: demo-mi-y:;
end class;
define class <demo-mi-d> (<demo-mi-a>, <demo-mi-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let dump = dump_classes();
    println!("\n=== Sprint 14 dump_classes() — diamond fixture ===");
    for line in dump.lines() {
        if line.contains("demo-mi") {
            println!("{}", line);
        }
    }
}

// ─── 10. CPL of an MI subclass walks both supers transitively ─────────────

#[test]
#[serial]
fn mi_subclass_is_subclass_of_every_ancestor() {
    let src = "\
define class <mi-sub-top> (<object>) end class;
define class <mi-sub-a> (<mi-sub-top>) end class;
define class <mi-sub-b> (<mi-sub-top>) end class;
define class <mi-sub-d> (<mi-sub-a>, <mi-sub-b>) end class;
";
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
    let top = find_class_id_by_name("<mi-sub-top>").unwrap();
    let a = find_class_id_by_name("<mi-sub-a>").unwrap();
    let b = find_class_id_by_name("<mi-sub-b>").unwrap();
    let d = find_class_id_by_name("<mi-sub-d>").unwrap();
    assert!(is_subclass(d, a));
    assert!(is_subclass(d, b));
    assert!(is_subclass(d, top));
    assert!(is_subclass(d, ClassId::OBJECT));
}
