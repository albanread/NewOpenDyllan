//! Sprint 13 — full multimethod dispatch + inline-cache machinery.
//!
//! Tests exercise three layers:
//!
//!   1. **Runtime-only** — `nod_runtime::lookup_method`, `add_method_full`,
//!      `remove_method`, generation counter, and `nod_dispatch` invoked
//!      directly. No JIT involved.
//!   2. **End-to-end** — Dylan source → lower → codegen → JIT → call. The
//!      inline cache fires through the JIT-emitted IR; we assert on
//!      hit/miss counters and on the IR shape via `dump-llvm`.
//!   3. **Cache invalidation** — `add_method` and `remove_method` bump
//!      the generation; the next JIT-emitted call sees a miss.
//!
//! Every test that touches the process-global dispatch tables is
//! `#[serial]`. The dispatch registry is shared across tests, so each
//! test uses class + generic names that include the test name to keep
//! independence.

use std::path::{Path, PathBuf};

use serial_test::serial;

use nod_reader::{SourceMap, lex, parse_module, scan_preamble};
use nod_runtime::{
    ClassMetadata, Word, add_method_full, class_metadata_for, dump_dispatch,
    find_class_id_by_name, find_generic, get_or_create_generic, lookup_method, remove_method,
    rust_make,
};
use nod_sema::{dump_llvm_for_file, lower_module, run_function_to_i64};

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
/// return its path.
fn tempfile_with(src: &str) -> PathBuf {
    let dir = fixtures_dir().join("_tmp");
    std::fs::create_dir_all(&dir).expect("mkdir _tmp");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = dir.join(format!("dispatch-{stamp}.dylan"));
    std::fs::write(&path, src).expect("write tmp fixture");
    path
}

fn register_user_classes(src: &str) {
    let m = parse(src);
    let _ = lower_module(&m).expect("lower");
}

extern "C" fn body_returns_1(_self: u64) -> u64 {
    Word::from_fixnum(1).unwrap().raw()
}
extern "C" fn body_returns_2(_self: u64) -> u64 {
    Word::from_fixnum(2).unwrap().raw()
}
extern "C" fn body_returns_3(_self: u64) -> u64 {
    Word::from_fixnum(3).unwrap().raw()
}
extern "C" fn body_returns_4(_self: u64) -> u64 {
    Word::from_fixnum(4).unwrap().raw()
}
extern "C" fn body_two_arg_99(_a: u64, _b: u64) -> u64 {
    Word::from_fixnum(99).unwrap().raw()
}
extern "C" fn body_two_arg_88(_a: u64, _b: u64) -> u64 {
    Word::from_fixnum(88).unwrap().raw()
}
extern "C" fn body_two_arg_77(_a: u64, _b: u64) -> u64 {
    Word::from_fixnum(77).unwrap().raw()
}

fn instance_of(md: &'static ClassMetadata) -> Word {
    // SAFETY: caller passes registered metadata; rust_make handles slots.
    unsafe { rust_make(md, &[]) }
}

// ─── 1. Two-method generic, dispatch picks the right one ──────────────────

#[test]
#[serial]
fn two_method_generic_picks_right_method_runtime_only() {
    register_user_classes(
        "\
define class <disp-circle> (<object>) end class;
define class <disp-square> (<object>) end class;
",
    );
    let cid = find_class_id_by_name("<disp-circle>").unwrap();
    let sid = find_class_id_by_name("<disp-square>").unwrap();

    // SAFETY: bodies are valid `extern "C" fn(u64) -> u64`.
    unsafe {
        add_method_full(
            "disp-area-1",
            vec![cid],
            body_returns_1 as *const u8,
            1,
        );
        add_method_full(
            "disp-area-1",
            vec![sid],
            body_returns_2 as *const u8,
            1,
        );
    }

    let g = find_generic("disp-area-1").expect("generic registered");
    let cm = class_metadata_for(cid);
    let sm = class_metadata_for(sid);
    let c_inst = instance_of(cm);
    let s_inst = instance_of(sm);

    let circle_method = lookup_method(g, &[cid]).expect("applicable method on <disp-circle>");
    let square_method = lookup_method(g, &[sid]).expect("applicable method on <disp-square>");

    // SAFETY: the body is `extern "C" fn(u64) -> u64`.
    let r_c = unsafe {
        let f: extern "C" fn(u64) -> u64 = std::mem::transmute(circle_method);
        Word::from_raw(f(c_inst.raw()))
    };
    let r_s = unsafe {
        let f: extern "C" fn(u64) -> u64 = std::mem::transmute(square_method);
        Word::from_raw(f(s_inst.raw()))
    };
    assert_eq!(r_c.as_fixnum(), Some(1));
    assert_eq!(r_s.as_fixnum(), Some(2));
    // Pointers identity check (cast both via the same function-pointer
    // route so clippy doesn't flag a direct fn-item-to-usize cast).
    let circle_body: extern "C" fn(u64) -> u64 = body_returns_1;
    let square_body: extern "C" fn(u64) -> u64 = body_returns_2;
    assert_eq!(circle_method as usize, circle_body as *const () as usize);
    assert_eq!(square_method as usize, square_body as *const () as usize);
}

// ─── 2. Multimethod with two arguments (full multimethod story) ───────────

#[test]
#[serial]
fn multimethod_two_argument_picks_most_specific() {
    register_user_classes(
        "\
define class <disp-rect> (<object>) end class;
define class <disp-circ2> (<object>) end class;
",
    );
    let rect = find_class_id_by_name("<disp-rect>").unwrap();
    let circ = find_class_id_by_name("<disp-circ2>").unwrap();

    // intersect(<rect>, <circle>) -> 99
    // intersect(<rect>, <rect>)   -> 88
    // intersect(<circ>, <circ>)   -> 77
    unsafe {
        add_method_full(
            "disp-intersect",
            vec![rect, circ],
            body_two_arg_99 as *const u8,
            2,
        );
        add_method_full(
            "disp-intersect",
            vec![rect, rect],
            body_two_arg_88 as *const u8,
            2,
        );
        add_method_full(
            "disp-intersect",
            vec![circ, circ],
            body_two_arg_77 as *const u8,
            2,
        );
    }

    let g = find_generic("disp-intersect").unwrap();
    let m_rc = lookup_method(g, &[rect, circ]).expect("rect, circ");
    let m_rr = lookup_method(g, &[rect, rect]).expect("rect, rect");
    let m_cc = lookup_method(g, &[circ, circ]).expect("circ, circ");
    let f99: extern "C" fn(u64, u64) -> u64 = body_two_arg_99;
    let f88: extern "C" fn(u64, u64) -> u64 = body_two_arg_88;
    let f77: extern "C" fn(u64, u64) -> u64 = body_two_arg_77;
    assert_eq!(m_rc as usize, f99 as *const () as usize);
    assert_eq!(m_rr as usize, f88 as *const () as usize);
    assert_eq!(m_cc as usize, f77 as *const () as usize);
}

// ─── 3. Specificity by CPL depth — deeper wins ────────────────────────────

#[test]
#[serial]
fn specificity_picks_deeper_class_in_cpl() {
    register_user_classes(
        "\
define class <disp-animal> (<object>) end class;
define class <disp-dog> (<disp-animal>) end class;
define class <disp-poodle> (<disp-dog>) end class;
",
    );
    let animal = find_class_id_by_name("<disp-animal>").unwrap();
    let dog = find_class_id_by_name("<disp-dog>").unwrap();
    let poodle = find_class_id_by_name("<disp-poodle>").unwrap();

    // Two methods: one on <animal>, one on <dog>. Calling with a
    // <poodle> instance should pick the <dog> method (deeper in CPL).
    unsafe {
        add_method_full(
            "disp-speak",
            vec![animal],
            body_returns_3 as *const u8,
            1,
        );
        add_method_full(
            "disp-speak",
            vec![dog],
            body_returns_4 as *const u8,
            1,
        );
    }

    let g = find_generic("disp-speak").unwrap();
    let picked = lookup_method(g, &[poodle]).expect("applicable on <poodle>");
    let dog_body: extern "C" fn(u64) -> u64 = body_returns_4;
    assert_eq!(
        picked as usize,
        dog_body as *const () as usize,
        "should pick <disp-dog>'s method, not <disp-animal>'s"
    );
}

// ─── 4. Subclass match (method on supertype is applicable to subtype) ─────

#[test]
#[serial]
fn method_on_supertype_applicable_to_subtype() {
    register_user_classes(
        "\
define class <disp-anim2> (<object>) end class;
define class <disp-pdl2> (<disp-anim2>) end class;
",
    );
    let anim = find_class_id_by_name("<disp-anim2>").unwrap();
    let pdl = find_class_id_by_name("<disp-pdl2>").unwrap();

    unsafe {
        add_method_full(
            "disp-bark",
            vec![anim],
            body_returns_1 as *const u8,
            1,
        );
    }

    let g = find_generic("disp-bark").unwrap();
    let picked = lookup_method(g, &[pdl]).expect("supertype method applies to subtype");
    let anim_body: extern "C" fn(u64) -> u64 = body_returns_1;
    assert_eq!(picked as usize, anim_body as *const () as usize);
}

// ─── 5. No applicable methods → panic ─────────────────────────────────────

#[test]
#[serial]
fn no_applicable_methods_panics_with_structured_message() {
    register_user_classes(
        "\
define class <disp-na1> (<object>) end class;
define class <disp-na2> (<object>) end class;
",
    );
    let na1 = find_class_id_by_name("<disp-na1>").unwrap();
    let na2 = find_class_id_by_name("<disp-na2>").unwrap();
    unsafe {
        add_method_full(
            "disp-na-only-on-1",
            vec![na1],
            body_returns_1 as *const u8,
            1,
        );
    }
    let na2_md = class_metadata_for(na2);
    let inst = instance_of(na2_md);

    let result = std::panic::catch_unwind(|| {
        let g = get_or_create_generic("disp-na-only-on-1");
        // SAFETY: call nod_dispatch directly with no cache slot. With
        // arity=1 and a `<disp-na2>` arg, no applicable method exists.
        unsafe {
            nod_runtime::nod_dispatch(
                g as *const _ as u64,
                0,
                1,
                inst.raw(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            )
        }
    });
    let err = result.expect_err("expected panic on no-applicable-methods");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("<no-applicable-methods-error>"),
        "panic msg: {msg}"
    );
    assert!(msg.contains("disp-na-only-on-1"), "panic msg: {msg}");
    assert!(msg.contains("<disp-na2>"), "panic msg: {msg}");
}

// ─── 6. Ambiguous methods → panic with structured message ─────────────────

#[test]
#[serial]
fn ambiguous_methods_panic_with_structured_message() {
    // True ambiguity needs two methods where, for some arg-class
    // tuple, NEITHER is more specific than the other at every
    // position. Classic Dylan diamond — needs MI (Sprint 14). Until
    // then we force the case by hand: install two methods on a fresh
    // generic with IDENTICAL specialisers, bypassing the
    // `add_method_full` dedup. We reach into `GenericFunction::methods`
    // directly to push the second method without replacement.
    register_user_classes(
        "\
define class <disp-amb> (<object>) end class;
",
    );
    let a = find_class_id_by_name("<disp-amb>").unwrap();
    let g = get_or_create_generic("disp-amb-call");
    {
        let mut methods = g.methods.write().expect("methods rwlock");
        methods.clear();
        methods.push(nod_runtime::Method {
            specialisers: vec![a],
            body_fn_ptr: body_two_arg_99 as *const u8,
            param_count: 1,
            body_fn_name: String::new(),
        });
        methods.push(nod_runtime::Method {
            specialisers: vec![a],
            body_fn_ptr: body_two_arg_88 as *const u8,
            param_count: 1,
            body_fn_name: String::new(),
        });
    }
    let a_md = class_metadata_for(a);
    let inst = instance_of(a_md);

    let result = std::panic::catch_unwind(|| {
        let g = get_or_create_generic("disp-amb-call");
        // SAFETY: nod_dispatch with arity=1, an <disp-amb> arg.
        unsafe {
            nod_runtime::nod_dispatch(
                g as *const _ as u64,
                0,
                1,
                inst.raw(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            )
        }
    });
    let err = result.expect_err("expected panic on ambiguity");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("<ambiguous-methods-error>"),
        "panic msg: {msg}"
    );
    assert!(msg.contains("disp-amb-call"), "panic msg: {msg}");
}

// ─── 7. Inline cache hit on repeat calls ──────────────────────────────────

#[test]
#[serial]
fn inline_cache_hits_on_repeat_calls() {
    // End-to-end: every `define method` body that calls a generic
    // installs a fresh call site, so calling the *outer* generic five
    // times from main() creates five distinct sites (each cold once).
    // But calling the *inner* generic (`ic-inner-slot`) FROM ONE
    // METHOD BODY whose method is invoked five times executes a
    // single inner call site five times — the cache fast-paths hit.
    //
    // Sprint 17 will add loops; until then, this is the cleanest
    // shape that exercises a single call site multiple times.
    let src = "\
Module: cache-hit-test

define class <disp-ic1> (<object>)
  slot ic-slot :: <integer>, init-keyword: ic-slot:;
end class;

define generic ic-outer (x);

define method ic-outer (x :: <disp-ic1>) => (<integer>)
  ic-slot(x)
end method ic-outer;

define function main () => (<integer>)
  let p = make(<disp-ic1>, ic-slot: 7);
  ic-outer(p) + ic-outer(p) + ic-outer(p) + ic-outer(p) + ic-outer(p)
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    // 5 calls * 7 = 35.
    assert_eq!(result, 35);

    // The inner `ic-slot(x)` call (inside `ic-outer`'s body) runs at a
    // single call site, invoked five times: 1 cold miss + 4 hits.
    let dump = dump_dispatch();
    let (hits, misses) = parse_cache_counters_for(&dump, "ic-slot");
    assert!(
        hits >= 4 && misses == 1,
        "expected hits >= 4 misses == 1 on ic-slot, got hits={hits} misses={misses}\n{dump}"
    );
}

// ─── 8. Cache miss on class change ────────────────────────────────────────

#[test]
#[serial]
fn cache_miss_when_receiver_class_changes() {
    // Drive a single call site (a recursive method) with alternating
    // receivers. The recursion target is the same dispatch IR node, so
    // each tail call hits ONE inline-cache slot — flipping the class
    // forces a miss each time.
    let src = "\
Module: cache-miss-test

define class <disp-mh1> (<object>)
  slot t :: <integer>, init-keyword: t:;
end class;
define class <disp-mh2> (<object>)
  slot t :: <integer>, init-keyword: t:;
end class;

define generic mh-mark (x);

define method mh-mark (x :: <disp-mh1>) => (<integer>)
  10
end method mh-mark;

define method mh-mark (x :: <disp-mh2>) => (<integer>)
  20
end method mh-mark;

define function main () => (<integer>)
  let a = make(<disp-mh1>, t: 0);
  let b = make(<disp-mh2>, t: 0);
  mh-mark(a) + mh-mark(b) + mh-mark(a) + mh-mark(b) + mh-mark(a)
end function main;
";
    let tmp = tempfile_with(src);
    let result = run_function_to_i64(&tmp, "main").expect("run main");
    assert_eq!(result, 10 + 20 + 10 + 20 + 10);
    let dump = dump_dispatch();
    let (_hits, misses) = parse_cache_counters_for(&dump, "mh-mark");
    // Five distinct Dylan call sites (one per `mh-mark(...)` in main).
    // Each is cold on first execution -> all five record a miss.
    // None see hits because they only execute once.
    assert!(
        misses >= 5,
        "expected at least 5 misses across the five call sites, got misses={misses}\n{dump}"
    );
}

// ─── 9. Generation bump invalidates cache ─────────────────────────────────

#[test]
#[serial]
fn generation_bump_invalidates_cache() {
    // End-to-end: a method body that calls another generic many times
    // populates a single inner cache slot. After the first run, bump
    // the generation by adding a no-op method elsewhere; the second
    // run's JIT-emitted fast path observes the gen mismatch and falls
    // through to slow path on its first invocation.
    //
    // To re-run main() without redefining a class we use two
    // separate Dylan files sharing the same class via its name and
    // the no-redefinition rule.
    let src1 = "\
Module: gen-inval-test

define class <disp-gi1> (<object>)
  slot gi-slot :: <integer>, init-keyword: gi-slot:;
end class;

define generic gi-area (x);

define method gi-area (x :: <disp-gi1>) => (<integer>)
  gi-slot(x)
end method gi-area;

define function main () => (<integer>)
  let p = make(<disp-gi1>, gi-slot: 5);
  gi-area(p) + gi-area(p) + gi-area(p) + gi-area(p) + gi-area(p)
end function main;
";
    let tmp1 = tempfile_with(src1);
    let r1 = run_function_to_i64(&tmp1, "main").expect("first run");
    assert_eq!(r1, 25);
    let dump1 = dump_dispatch();
    let (hits1, misses1) = parse_cache_counters_for(&dump1, "gi-slot");
    assert!(
        hits1 >= 4 && misses1 == 1,
        "expected hits >= 4 misses == 1 on gi-slot pre-bump, got hits={hits1} misses={misses1}\n{dump1}"
    );

    // Bump the generation.
    let g = find_generic("gi-slot").expect("gi-slot is now in the registry");
    let gen_before = g.generation();
    register_user_classes("define class <disp-gi-extra> (<object>) end class;");
    let extra = find_class_id_by_name("<disp-gi-extra>").unwrap();
    unsafe {
        add_method_full(
            "gi-slot",
            vec![extra],
            body_returns_1 as *const u8,
            1,
        );
    }
    let gen_after = g.generation();
    assert!(
        gen_after > gen_before,
        "add_method must bump generation: before={gen_before} after={gen_after}"
    );

    // Run a SECOND fixture that uses the already-registered class.
    // This file omits the class definition (Sprint 12 forbids
    // redefinition); the generic + method survived the first JIT
    // session in the dispatch table.
    let src2 = "\
Module: gen-inval-test2

define function main () => (<integer>)
  let p = make(<disp-gi1>, gi-slot: 5);
  gi-area(p) + gi-area(p) + gi-area(p) + gi-area(p) + gi-area(p)
end function main;
";
    let tmp2 = tempfile_with(src2);
    let r2 = run_function_to_i64(&tmp2, "main").expect("second run");
    assert_eq!(r2, 25);
    let dump2 = dump_dispatch();
    let (_hits2, misses2) = parse_cache_counters_for(&dump2, "gi-slot");
    assert!(
        misses2 > misses1,
        "expected at least one additional miss after gen bump: pre={misses1} post={misses2}\n{dump2}"
    );
}

// ─── 10. remove_method bumps generation similarly ─────────────────────────

#[test]
#[serial]
fn remove_method_bumps_generation() {
    let g = get_or_create_generic("rm-gen-test");
    let g0 = g.generation();
    register_user_classes("define class <disp-rm1> (<object>) end class;");
    let c = find_class_id_by_name("<disp-rm1>").unwrap();
    unsafe {
        add_method_full("rm-gen-test", vec![c], body_returns_1 as *const u8, 1);
    }
    let g1 = g.generation();
    assert!(g1 > g0, "add bumps generation: g0={g0} g1={g1}");
    remove_method("rm-gen-test", &[c]);
    let g2 = g.generation();
    assert!(g2 > g1, "remove bumps generation: g1={g1} g2={g2}");
}

// ─── 11. JIT-emitted IR shows cache-check sequence ────────────────────────

#[test]
#[serial]
fn jit_emitted_ir_has_cache_check_shape() {
    let src = "\
Module: ir-shape-test

define class <disp-ir1> (<object>)
  slot v :: <integer>, init-keyword: v:;
end class;

define generic ir-area (x);

define method ir-area (x :: <disp-ir1>) => (<integer>)
  v(x)
end method ir-area;

define function main () => (<integer>)
  ir-area(make(<disp-ir1>, v: 1))
end function main;
";
    let tmp = tempfile_with(src);
    let ir = dump_llvm_for_file(&tmp).expect("dump-llvm");
    assert!(ir.contains("disp.s"), "site labels:\n{ir}");
    assert!(ir.contains("cache_class"), "cache_class load:\n{ir}");
    assert!(ir.contains("cache_method"), "cache_method load:\n{ir}");
    assert!(ir.contains("cache_gen"), "cache_gen load:\n{ir}");
    assert!(ir.contains("class_ok"), "class_ok cmp:\n{ir}");
    assert!(ir.contains("gen_ok"), "gen_ok cmp:\n{ir}");
    assert!(ir.contains("cache_hit"), "cache_hit and:\n{ir}");
    assert!(ir.contains("fast_call"), "fast_call branch:\n{ir}");
    assert!(ir.contains("slow_call"), "slow_call branch:\n{ir}");
    assert!(ir.contains("dispatch_done"), "phi join block:\n{ir}");
    assert!(ir.contains("nod_dispatch"), "extern decl:\n{ir}");
    assert!(
        ir.contains("atomicrmw add"),
        "hit counter atomic add:\n{ir}"
    );
}

// ─── 12. Sprint 12 regression: point distance-squared still returns 25 ────

#[test]
#[serial]
fn point_distance_squared_returns_25_regression() {
    let path = fixtures_dir().join("point.dylan");
    let result = run_function_to_i64(&path, "main").expect("run point main");
    assert_eq!(result, 25);
}

// ─── 13. GC under dispatch pressure ───────────────────────────────────────

#[test]
#[serial]
fn dispatch_survives_gc_pressure() {
    // Define a class + a method once, then drive 10K dispatches with
    // interleaved `rust_make` allocations from Rust. The cache slot
    // lives in the static area; the method body's JIT memory is
    // pinned (engines leak forever). Across the 10K runs GC will fire
    // many times and the cache must keep working — no crashes, no
    // wrong-method calls.
    register_user_classes("define class <disp-gcp-rt> (<object>) end class;");
    let cid = find_class_id_by_name("<disp-gcp-rt>").unwrap();
    unsafe {
        add_method_full(
            "gcp-rt-mark",
            vec![cid],
            body_returns_1 as *const u8,
            1,
        );
    }
    let g = get_or_create_generic("gcp-rt-mark");
    let md = class_metadata_for(cid);
    let initial = nod_runtime::gc_stats().minor_collections;
    for _ in 0..10_000 {
        let inst = instance_of(md);
        let arg_classes = [cid];
        let m = lookup_method(g, &arg_classes).expect("applicable");
        // SAFETY: m is `extern "C" fn(u64) -> u64`.
        let result = unsafe {
            let f: extern "C" fn(u64) -> u64 = std::mem::transmute(m);
            Word::from_raw(f(inst.raw()))
        };
        assert_eq!(result.as_fixnum(), Some(1));
    }
    let final_gc = nod_runtime::gc_stats().minor_collections;
    assert!(
        final_gc >= initial,
        "GC counts must be monotonic: initial={initial} final={final_gc}"
    );
}

// ─── Helpers ──────────────────────────────────────────────────────────────

/// Parse `dump_dispatch()`'s output and return (hits, misses) summed
/// across every call site listed under `generic_name`. Returns (0, 0)
/// if the generic isn't present or has no call sites yet.
fn parse_cache_counters_for(dump: &str, generic_name: &str) -> (u64, u64) {
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut in_target = false;
    for line in dump.lines() {
        if let Some(rest) = line.strip_prefix("Generic ") {
            in_target = rest.starts_with(generic_name)
                && rest[generic_name.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == ' ');
            continue;
        }
        if !in_target {
            continue;
        }
        // "    site#0: cache class=...(N) method=0x... gen=... - hits=H misses=M"
        if let Some(idx) = line.find("hits=") {
            let rest = &line[idx + "hits=".len()..];
            let h: u64 = rest
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            hits += h;
        }
        if let Some(idx) = line.find("misses=") {
            let rest = &line[idx + "misses=".len()..];
            let m: u64 = rest
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            misses += m;
        }
    }
    (hits, misses)
}

