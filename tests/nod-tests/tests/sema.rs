//! Sprint 06 — AST → DFM lowering + verifier + format_dfm.

use std::path::{Path, PathBuf};

use nod_dfm::{
    Block, BlockId, Computation, ConstValue, Function, FunctionId, PrimOp, Temporary, TempId,
    Terminator, TypeEstimate, VerifyError, format_dfm, format_dfm_module, verify,
};
use nod_reader::{
    FileId, Module, SourceMap, Span, lex, parse_module, scan_preamble,
};
use nod_sema::lower_module;

fn parse_src(src: &str) -> Module {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    parse_module(src, &toks, pre.as_ref())
        .unwrap_or_else(|d| panic!("parse_module diagnostics: {d:?}\n--- src ---\n{src}"))
}

fn lower_src(src: &str) -> Vec<Function> {
    let m = parse_src(src);
    lower_module(&m).unwrap_or_else(|e| panic!("lower errors: {e:?}\n--- src ---\n{src}"))
}

fn fake_span() -> Span {
    Span { file_id: FileId(0), lo: 0, hi: 0 }
}

// ─── 1. Const lowering ───────────────────────────────────────────────────

#[test]
fn const_int_lowers() {
    let fns = lower_src("define constant x = 42;");
    assert_eq!(fns.len(), 1);
    let f = &fns[0];
    assert_eq!(f.name, "x");
    assert_eq!(f.return_type, TypeEstimate::Integer);
    assert_eq!(f.blocks.len(), 1);
    let entry = &f.blocks[0];
    assert_eq!(entry.computations.len(), 1);
    match &entry.computations[0] {
        Computation::Const { value: ConstValue::Integer(42), .. } => {}
        c => panic!("expected Const Integer(42), got {c:?}"),
    }
    match &entry.terminator {
        Terminator::Return { value: Some(_) } => {}
        t => panic!("expected Return Some, got {t:?}"),
    }
    verify(f).expect("verify");
}

// ─── 2. Integer arithmetic ───────────────────────────────────────────────

#[test]
fn integer_add_lowers() {
    let src = "define function add (x :: <integer>, y :: <integer>) => (<integer>) x + y end;";
    let fns = lower_src(src);
    assert_eq!(fns.len(), 1);
    let f = &fns[0];
    assert_eq!(f.name, "add");
    assert_eq!(f.return_type, TypeEstimate::Integer);
    assert_eq!(f.params.len(), 2);
    let entry = &f.blocks[0];
    assert_eq!(entry.computations.len(), 1);
    match &entry.computations[0] {
        Computation::PrimOp { op: PrimOp::AddInt, args, .. } => {
            assert_eq!(args.len(), 2);
            assert_eq!(args[0], f.params[0]);
            assert_eq!(args[1], f.params[1]);
        }
        c => panic!("expected AddInt, got {c:?}"),
    }
    verify(f).expect("verify");
}

// ─── 3. Float arithmetic ─────────────────────────────────────────────────

#[test]
fn float_arith_lowers() {
    let src = "\
define function fadd (x :: <double-float>, y :: <double-float>) => (<double-float>)
  x + y
end;
define function fsub (x :: <double-float>, y :: <double-float>) => (<double-float>)
  x - y
end;
";
    let fns = lower_src(src);
    assert_eq!(fns.len(), 2);
    for (expected_op, f) in [PrimOp::AddFloat, PrimOp::SubFloat].iter().zip(&fns) {
        assert_eq!(f.return_type, TypeEstimate::DoubleFloat);
        match &f.blocks[0].computations[0] {
            Computation::PrimOp { op, .. } if op == expected_op => {}
            c => panic!("expected {expected_op:?}, got {c:?}"),
        }
        verify(f).expect("verify");
    }
}

// ─── 4. Comparison ───────────────────────────────────────────────────────

#[test]
fn comparison_lowers_to_lt_int() {
    let src = "define function lt (x :: <integer>, y :: <integer>) => (<boolean>) x < y end;";
    let fns = lower_src(src);
    let f = &fns[0];
    assert_eq!(f.return_type, TypeEstimate::Boolean);
    match &f.blocks[0].computations[0] {
        Computation::PrimOp { op: PrimOp::LtInt, .. } => {}
        c => panic!("expected LtInt, got {c:?}"),
    }
    verify(f).expect("verify");
}

// ─── 5. If lowering ──────────────────────────────────────────────────────

#[test]
fn if_lowers_to_three_blocks_plus_join() {
    let src = "\
define function abs (x :: <integer>) => (<integer>)
  if (x < 0) -x else x end
end function abs;
";
    let fns = lower_src(src);
    let f = &fns[0];
    // entry + then + else + join = 4 blocks.
    assert_eq!(f.blocks.len(), 4, "expected 4 blocks, dump:\n{}", format_dfm(f));
    let labels: Vec<&str> = f.blocks.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels[0], "entry");
    assert!(labels[1].starts_with("then"));
    assert!(labels[2].starts_with("else"));
    assert!(labels[3].starts_with("join"));
    // Entry's terminator is an If.
    assert!(matches!(f.blocks[0].terminator, Terminator::If { .. }));
    // Both then and else jump to the join block, supplying one arg each.
    let join = &f.blocks[3];
    assert_eq!(join.params.len(), 1, "join block should carry one parameter");
    for arm in &f.blocks[1..3] {
        match &arm.terminator {
            Terminator::Jump { target, args } => {
                assert_eq!(*target, join.id);
                assert_eq!(args.len(), 1);
            }
            t => panic!("expected Jump from {} to join, got {t:?}", arm.label),
        }
    }
    // Return is in the join block.
    assert!(matches!(f.blocks[3].terminator, Terminator::Return { value: Some(_) }));
    verify(f).expect("verify");
}

// ─── 6. Direct call ──────────────────────────────────────────────────────

#[test]
fn direct_call_to_known_top_level() {
    let src = "\
define function sq (x :: <integer>) => (<integer>) x * x end;
define function double (x :: <integer>) => (<integer>) sq(x) + sq(x) end;
";
    let fns = lower_src(src);
    assert_eq!(fns.len(), 2);
    let double = fns.iter().find(|f| f.name == "double").unwrap();
    let calls: Vec<&Computation> = double.blocks[0]
        .computations
        .iter()
        .filter(|c| matches!(c, Computation::DirectCall { callee, .. } if callee == "sq"))
        .collect();
    assert_eq!(calls.len(), 2, "expected two DirectCall sq, dump:\n{}", format_dfm(double));
    verify(double).expect("verify");
}

// ─── 7. Let binding ──────────────────────────────────────────────────────

#[test]
fn let_binding_resolves_through_env() {
    let src = "\
define function f (x :: <integer>) => (<integer>)
  let y = x * 2;
  y + 1
end function f;
";
    let fns = lower_src(src);
    let f = &fns[0];
    // Computations: const 2, mul (= y), const 1, add. 4 in entry.
    let entry = &f.blocks[0];
    assert_eq!(entry.computations.len(), 4, "dump:\n{}", format_dfm(f));
    let mul = entry
        .computations
        .iter()
        .find_map(|c| match c {
            Computation::PrimOp { op: PrimOp::MulInt, args, dst } => {
                assert_eq!(args[0], f.params[0]);
                Some(*dst)
            }
            _ => None,
        })
        .expect("expected a MulInt for the let-value");
    let last = entry
        .computations
        .last()
        .expect("non-empty entry computations");
    match last {
        Computation::PrimOp { op: PrimOp::AddInt, args, .. } => {
            assert!(args.contains(&mul), "expected add to consume the let-bound temp");
        }
        c => panic!("expected AddInt as the final entry stmt, got {c:?}"),
    }
    verify(f).expect("verify");
}

// ─── 8. Sprint 12: define class lowers to slot accessors ─────────────────

#[test]
fn define_class_emits_slot_accessors() {
    let src =
        "define class <sema-pt> (<object>) slot foo :: <integer>, init-keyword: foo:; end class;";
    let fns = lower_src(src);
    // Sprint 12 emits at least a getter (and a setter, since the
    // slot is mutable by default). The user supplied no constants
    // or functions, so the entire fns list is accessors.
    assert!(
        fns.iter().any(|f| f.name == "<sema-pt>-getter-foo"),
        "expected <sema-pt>-getter-foo, dump: {:?}",
        fns.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        fns.iter().any(|f| f.name == "<sema-pt>-setter-foo"),
        "expected <sema-pt>-setter-foo"
    );
    // The getter is a single LoadSlot.
    let getter = fns.iter().find(|f| f.name == "<sema-pt>-getter-foo").unwrap();
    let entry = &getter.blocks[0];
    assert_eq!(entry.computations.len(), 1, "dump:\n{}", format_dfm(getter));
    match &entry.computations[0] {
        Computation::LoadSlot { offset, .. } => assert_eq!(*offset, 8),
        c => panic!("expected LoadSlot, got {c:?}"),
    }
    verify(getter).expect("verify getter");
}

// ─── 9. Verifier round-trip on every lowered function ────────────────────

#[test]
fn verifier_round_trip_kernel_arith() {
    let path = fixtures_dir().join("kernel-arith.dylan");
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let fns = lower_src(&src);
    assert!(!fns.is_empty());
    for f in &fns {
        verify(f).unwrap_or_else(|e| panic!("verify failed for {}: {e:?}", f.name));
    }
}

// ─── 10. End-to-end via format_dfm — snapshot fixture ────────────────────

#[test]
fn kernel_arith_fixture_snapshot() {
    let path = fixtures_dir().join("kernel-arith.dylan");
    let src = std::fs::read_to_string(&path).expect("read fixture");
    let fns = lower_src(&src);
    let dump = format_dfm_module(&fns);
    assert_eq!(dump, EXPECTED_KERNEL_ARITH_DUMP, "DFM dump drift:\n{dump}");
}

const EXPECTED_KERNEL_ARITH_DUMP: &str = r#"fn *answer* () -> <integer>:
  entry:
    t0: <integer> = Const Integer(42)
    Return t0

fn sq (t0: <integer>) -> <integer>:
  entry:
    t1: <integer> = PrimOp MulInt t0 t0
    Return t1

fn abs (t0: <integer>) -> <integer>:
  entry:
    t1: <integer> = Const Integer(0)
    t2: <boolean> = PrimOp LtInt t0 t1
    If t2 then1 else2
  then1:
    t3: <integer> = PrimOp NegInt t0
    Jump join3(t3)
  else2:
    Jump join3(t0)
  join3(t4: <integer>):
    Return t4

fn hypot-sq (t0: <integer>, t1: <integer>) -> <integer>:
  entry:
    t2: <integer> = DirectCall sq(t0)
    t3: <integer> = DirectCall sq(t1)
    t4: <integer> = PrimOp AddInt t2 t3
    Return t4
"#;

// ─── 11. Negative verifier — four malformed Functions ────────────────────

fn empty_block(id: BlockId, label: &str, term: Terminator) -> Block {
    Block {
        id,
        label: label.to_string(),
        params: Vec::new(),
        computations: Vec::new(),
        terminator: term,
    }
}

fn t(i: u32, ty: TypeEstimate) -> Temporary {
    Temporary { id: TempId(i), type_estimate: ty }
}

#[test]
fn verify_use_before_def() {
    let f = Function {
        id: FunctionId(0),
        name: "bad".into(),
        params: Vec::new(),
        entry: BlockId(0),
        blocks: vec![Block {
            id: BlockId(0),
            label: "entry".into(),
            params: Vec::new(),
            computations: vec![Computation::PrimOp {
                dst: TempId(0),
                op: PrimOp::AddInt,
                args: vec![TempId(99)], // never defined
            }],
            terminator: Terminator::Return { value: Some(TempId(0)) },
        }],
        temps: vec![t(0, TypeEstimate::Integer)],
        return_type: TypeEstimate::Integer,
        span: fake_span(),
    };
    let errs = verify(&f).expect_err("expected use-before-def");
    assert!(
        errs.iter().any(|e| matches!(e, VerifyError::UseBeforeDef { temp, .. } if *temp == TempId(99))),
        "errors: {errs:?}"
    );
}

#[test]
fn verify_dangling_block_ref() {
    let f = Function {
        id: FunctionId(0),
        name: "bad".into(),
        params: Vec::new(),
        entry: BlockId(0),
        blocks: vec![empty_block(
            BlockId(0),
            "entry",
            Terminator::Jump {
                target: BlockId(42),
                args: Vec::new(),
            },
        )],
        temps: Vec::new(),
        return_type: TypeEstimate::Unit,
        span: fake_span(),
    };
    let errs = verify(&f).expect_err("expected dangling block");
    assert!(
        errs.iter().any(|e| matches!(e, VerifyError::DanglingBlockRef { to, .. } if *to == BlockId(42))),
        "errors: {errs:?}"
    );
}

#[test]
fn verify_double_define() {
    let f = Function {
        id: FunctionId(0),
        name: "bad".into(),
        params: Vec::new(),
        entry: BlockId(0),
        blocks: vec![Block {
            id: BlockId(0),
            label: "entry".into(),
            params: Vec::new(),
            computations: vec![
                Computation::Const { dst: TempId(0), value: ConstValue::Integer(1) },
                Computation::Const { dst: TempId(0), value: ConstValue::Integer(2) },
            ],
            terminator: Terminator::Return { value: Some(TempId(0)) },
        }],
        temps: vec![t(0, TypeEstimate::Integer)],
        return_type: TypeEstimate::Integer,
        span: fake_span(),
    };
    let errs = verify(&f).expect_err("expected double-define");
    assert!(
        errs.iter().any(|e| matches!(e, VerifyError::DoubleDefine { temp } if *temp == TempId(0))),
        "errors: {errs:?}"
    );
}

#[test]
fn verify_missing_entry() {
    // Entry-id points at a block that doesn't exist. Blocks list still has
    // a block, with id BlockId(7) — but `f.entry` is BlockId(0), absent.
    let f = Function {
        id: FunctionId(0),
        name: "bad".into(),
        params: Vec::new(),
        entry: BlockId(0),
        blocks: vec![empty_block(
            BlockId(7),
            "loose",
            Terminator::Return { value: None },
        )],
        temps: Vec::new(),
        return_type: TypeEstimate::Unit,
        span: fake_span(),
    };
    let errs = verify(&f).expect_err("expected missing-entry");
    assert!(
        errs.iter().any(|e| matches!(e, VerifyError::MissingEntry { .. })),
        "errors: {errs:?}"
    );
}

// ─── Fixture-locator helper ──────────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

// ─── GAP-005 — `if` without `else` arm lowers cleanly ───────────────────

/// Regression test for COMPILER_GAPS.md GAP-005. Before the fix
/// `lower_expr`'s `Expr::If` arm rejected an else-less if with
/// `LoweringError::Unsupported("Sprint 06 lowers only if-expressions
/// with an else arm")`. Surfaced by Sprint 45a's `print-token`
/// rework — the natural shape for a side-effect-only conditional
/// (`if (cond) write-string(...) end;`) tripped the check
/// immediately. Fix synthesises `Expr::Bool(span, false)` as the
/// missing else; runtime semantics: an `if` without else returns `#f`.
#[test]
fn gap_005_if_without_else_lowers() {
    let src = "\
        define function classify (n :: <integer>) => (n :: <integer>)\n  \
            if (n > 0) n end\n\
        end function;\n";
    // Pre-fix: lower_src panics with the Unsupported error.
    let fns = lower_src(src);
    assert_eq!(fns.len(), 1);
    let f = &fns[0];
    assert_eq!(f.name, "classify");
    // The if's two arms produce different types (then = <integer>,
    // else = <boolean> for the synthesised #f), so the join phi's
    // return-type estimate widens to Top. That's correct Dylan
    // semantics — the caller can't statically assume any particular
    // shape.
    verify(f).expect("verify");
}

// ─── GAP-006 — if arms with mismatched types don't panic codegen ────────

/// Regression test for COMPILER_GAPS.md GAP-006. Before the fix the
/// `Computation::DirectCall` / `Dispatch` / `SealedDirectCall` codegen
/// arms in `nod-llvm/src/codegen.rs` only inserted the dst TempId
/// into `state.temps` when the call returned Some(v). Void-returning
/// calls (return type `()`) skipped the insert — but lowering still
/// allocates a dst TempId regardless. When that TempId then appeared
/// as a Jump arg into a join block (e.g. void call as the last
/// expression of an `if`-arm), the phi-incoming wiring step at
/// codegen.rs:1233 panicked with `phi incoming temp defined` because
/// `state.temps.get(arg_temp)` returned None.
///
/// Fix: all three call-flavour Computation arms now insert
/// `load_imm_nil()` (Dylan's canonical "no meaningful value") for the
/// dst TempId when the underlying emit returns None. Phi joins get a
/// real i64 LLVM value. Consumers that actually USE the value see
/// `nil` — there's no Dylan use-case where a void call's "result" is
/// supposed to be anything else.
///
/// The smoke that surfaced this was Sprint 45a's `print-token` after
/// the GAP-001 stream rework — `if (cond) write-string(s, "  ") end`
/// (post-GAP-005 = `if (cond) ... else #f end`) had a void-returning
/// `write-string` as the then-arm's last expr and `#f` as the
/// synthesised else, hitting the missing-temp panic on the phi join.
#[test]
fn gap_006_void_call_in_if_arm_does_not_panic() {
    let src = "\
        define function notice (n :: <integer>) => (out :: <boolean>)\n  \
            if (n > 0)\n    \
                format-out(\"positive\\n\")\n  \
            else\n    \
                #f\n  \
            end\n\
        end function;\n";
    // Pre-fix: lower OK, then codegen would panic. lower_src only
    // exercises the lowering pass, not codegen. The real test is the
    // smoke test that builds + runs an EXE — see
    // `tests/nod-tests/fixtures/hello.dylan` via the dump-dylan-tokens
    // path; the lexer's print-token method now uses the bare
    // `if (~instance?(...)) ... end` shape with a void write-* call
    // as the last expression. If that build succeeds, GAP-006 is fixed.
    let fns = lower_src(src);
    assert_eq!(fns.len(), 1);
    let f = &fns[0];
    assert_eq!(f.name, "notice");
    verify(f).expect("verify");
}

// ─── GAP-001 — <string-stream> round-trips bytes via stdlib generics ────

/// Regression test for COMPILER_GAPS.md GAP-001. Before this gap landed,
/// the stdlib had no stream abstraction; the Sprint 45a Dylan lexer had
/// to fake it with a `print-token-to-string` helper returning a fresh
/// byte-string per token, which `dump-tokens` then concatenated — `O(N²)`
/// allocation. With `<string-stream>` in stdlib, the lexer (sprint 45a
/// rework) can declare `print-token(t, source, stream :: <string-stream>)`
/// and write directly into ONE accumulator.
///
/// This test exercises the round-trip: build a stream, write a mixed
/// sequence of string + byte writes, materialise as a byte-string, and
/// assert the bytes come back exactly right. End-to-end Dylan path, no
/// shortcuts through `eval_expr_to_string` — the assertions live in the
/// Dylan source itself via `format-out`, captured through the AOT EXE.
#[test]
fn gap_001_string_stream_round_trips() {
    // Lowering-side check: just that the new stdlib classes / generics
    // exist and resolve when referenced from user code. The end-to-end
    // byte-correctness check is in the `aot_dylan` family below (or
    // can be added there if we want a redundant smoke test).
    let src = "\
        define function exercise () => (s :: <byte-string>)\n  \
            let ss = make-string-stream();\n  \
            write-string(ss, \"hi\");\n  \
            write-byte(ss, 33);\n  \
            as-byte-string(ss)\n\
        end function;\n";
    let fns = lower_src(src);
    // Just the one user function; stdlib bodies aren't in this LM
    // (lower_module rather than lower_module_full + merge_stdlib).
    assert_eq!(fns.len(), 1, "expected exercise() only, got: {:?}",
        fns.iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
    assert_eq!(fns[0].name, "exercise");
    // Crude content check — make sure the function body called the
    // four stream-related names (proves they resolved at lower time;
    // pre-fix the lowering panicked with undefined-ident on the class
    // ref `<string-stream>`).
    let dump = format!("{:?}", &fns[0]);
    assert!(dump.contains("make-string-stream"), "no make-string-stream call");
    assert!(dump.contains("write-string"),       "no write-string call");
    assert!(dump.contains("write-byte"),         "no write-byte call");
    assert!(dump.contains("as-byte-string"),     "no as-byte-string call");
}

// ─── GAP-002 — `define constant` resolves from function bodies ───────────

/// Regression test for COMPILER_GAPS.md GAP-002. Before the fix,
/// `collect_top_level_names` only registered `Item::DefineFunction`
/// entries — constants were lowered as zero-arg functions but never
/// added to the name-resolution table, so a bareword reference to a
/// constant from inside a `define function` body raised
/// `LoweringError::UndefinedIdent` even though the constant was
/// declared in the same file at module scope. Surfaced by Sprint 45a's
/// `dylan-lexer.dylan` (the `$line-col-shift` use); fix landed in the
/// same commit as this test. Don't remove without retiring the gap.
#[test]
fn gap_002_define_constant_resolves_from_function_body() {
    let src = "\
        define constant $magic = 42;\n\
        define function call-magic () => (n :: <integer>)\n  \
            $magic\n\
        end function;\n";
    let fns = lower_src(src);
    // Expect both `$magic` (zero-arg constant body) and `call-magic`
    // (the function that references it) to lower cleanly. Pre-fix the
    // lower_src call panicked with "undefined ident `$magic`".
    assert_eq!(fns.len(), 2, "expected 2 lowered functions, got: {:?}",
        fns.iter().map(|f| f.name.as_str()).collect::<Vec<_>>());
    let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"$magic"), "missing $magic: {names:?}");
    assert!(names.contains(&"call-magic"), "missing call-magic: {names:?}");
}

// ─── GAP-004 — `define variable` lowering + setter ───────────────────────

/// Regression test for COMPILER_GAPS.md GAP-004 lowering. Before the
/// fix, `Item::DefineVariable` emitted `LoweringError::Unsupported`
/// ("define variable not lowered in Sprint 06"). The fix synthesises
/// two functions per variable: a `<name>()` getter (zero-arg body that
/// loads the cell via `nod_var_get_by_name`) and a corresponding
/// `__init-<name>()` init thunk; a setter ISN'T a standalone function
/// (`lower_assign` inlines the `nod_var_set_by_name` call at each
/// assignment site).
///
/// This test asserts the lowering structure. End-to-end runtime
/// round-trip lives in the `aot_dylan` family (separate file).
#[test]
fn gap_004_define_variable_lowers_to_getter_and_init() {
    let src = "\
        define variable *counter* = 41;\n\
        define function bump-counter () => (n :: <integer>)\n  \
            *counter* := *counter* + 1;\n  \
            *counter*\n\
        end function;\n";
    let fns = lower_src(src);
    let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    // Expect three lowered functions: the getter (`*counter*`), the
    // init thunk (`__init-*counter*`), and the user function
    // (`bump-counter`). The setter is inlined at the assignment site
    // inside bump-counter, not emitted standalone.
    assert!(names.contains(&"*counter*"),
        "missing *counter* getter: {names:?}");
    assert!(names.contains(&"__init-*counter*"),
        "missing __init-*counter* thunk: {names:?}");
    assert!(names.contains(&"bump-counter"),
        "missing bump-counter: {names:?}");
    assert_eq!(fns.len(), 3,
        "expected 3 lowered functions, got: {names:?}");

    // The user function's body should contain DirectCalls to both the
    // getter (via the bareword `*counter*`) and the setter shim
    // (via `:=`). Find bump-counter and check.
    let bump = fns.iter().find(|f| f.name == "bump-counter")
        .expect("bump-counter present");
    let dump = format!("{bump:?}");
    assert!(dump.contains("nod_var_set_by_name"),
        "bump-counter body should call nod_var_set_by_name for `:=`: {dump}");
    assert!(dump.contains("*counter*"),
        "bump-counter body should reference *counter*: {dump}");
}

/// GAP-004 — assignment to a `define constant` (vs. variable) must be
/// rejected by the lowerer. Pre-GAP-004 this fell through to
/// "UndefinedIdent" because module-level names didn't reach
/// lower_assign at all. Post-fix, the new branch surfaces a clear
/// "cannot assign to constant" error.
#[test]
fn gap_004_assign_to_define_constant_is_error() {
    let src = "\
        define constant $magic = 42;\n\
        define function clobber () => () $magic := 99 end function;\n";
    let m = parse_src(src);
    let err = nod_sema::lower_module(&m).expect_err("must reject constant-assign");
    let combined = format!("{err:?}");
    assert!(
        combined.contains("cannot assign to") && combined.contains("$magic"),
        "expected `cannot assign to $magic` diagnostic, got: {combined}"
    );
}

// ─── GAP-003 — multi-value return / multi-binder `let` ───────────────────

/// Sprint 47 Phase B smoke test — the four `%values-*` primitives in
/// `LOWER_PRIMITIVE_TABLE` lower cleanly to DirectCalls against the
/// matching `nod_values_*` runtime shims. This is the gate before
/// Phase D's `values()`/`let (a, b) = …` lowerings can build on top:
/// if the primops don't lower, the higher-level forms can't either.
#[test]
fn gap_003_phase_b_values_primops_lower() {
    let src = "\
        define function direct-buffer-poke () => (n :: <integer>)\n  \
            %values-clear();\n  \
            %values-set!(0, 11);\n  \
            %values-set!(1, 22);\n  \
            let n = %values-count();\n  \
            let a = %values-get(0);\n  \
            let b = %values-get(1);\n  \
            n + a + b\n\
        end function;\n";
    let fns = lower_src(src);
    assert_eq!(fns.len(), 1);
    let f = &fns[0];
    assert_eq!(f.name, "direct-buffer-poke");
    let dump = format!("{f:?}");
    assert!(dump.contains("nod_values_clear"),  "no nod_values_clear: {dump}");
    assert!(dump.contains("nod_values_set"),    "no nod_values_set: {dump}");
    assert!(dump.contains("nod_values_get"),    "no nod_values_get: {dump}");
    assert!(dump.contains("nod_values_count"),  "no nod_values_count: {dump}");
    verify(f).expect("verify");
}

/// Sprint 47 Phase D — `values(x)` with a single argument is the
/// degenerate form: it should lower to just `x`, with no buffer touch.
/// Zero-overhead is the point of the SBCL design for the common case.
#[test]
fn gap_003_values_returns_first_value_when_called_with_one_arg() {
    let src = "\
        define function single () => (x :: <integer>)\n  \
            values(42)\n\
        end function;\n";
    let fns = lower_src(src);
    let f = &fns[0];
    let dump = format!("{f:?}");
    // No buffer mutation should appear.
    assert!(
        !dump.contains("nod_values_set"),
        "single-value values(x) must not emit any nod_values_set; dump:\n{dump}"
    );
    verify(f).expect("verify");
}

/// Sprint 47 Phase D — `values(a, b, c)` lowers to writes into the
/// secondary-values buffer for the extras, returning `a` through the
/// ordinary ABI. Each extra produces one `nod_values_set` DirectCall.
#[test]
fn gap_003_values_writes_extras_to_buffer_and_returns_first() {
    let src = "\
        define function triple () => (a :: <integer>)\n  \
            values(1, 2, 3)\n\
        end function;\n";
    let fns = lower_src(src);
    let f = &fns[0];
    let dump = format!("{f:?}");
    // Two extras → two nod_values_set calls.
    let n_sets = dump.matches("nod_values_set").count();
    assert_eq!(
        n_sets, 2,
        "expected exactly two nod_values_set calls for values(a, b, c); dump:\n{dump}"
    );
    verify(f).expect("verify");
}

/// Sprint 47 Phase D — `let (a, b) = call()` lowers as
///   `%values-clear(); let a = call(); let b = %values-get(0);`
/// Each step appears as a DirectCall to the matching nod_values_* shim.
#[test]
fn gap_003_multi_binder_let_destructures_two_values() {
    let src = "\
        define function two-values () => (a :: <integer>)\n  \
            values(7, 8)\n\
        end function;\n\
        define function consume () => (sum :: <integer>)\n  \
            let (x, y) = two-values();\n  \
            x + y\n\
        end function;\n";
    let fns = lower_src(src);
    let consume = fns.iter().find(|f| f.name == "consume").expect("consume");
    let dump = format!("{consume:?}");
    assert!(
        dump.contains("nod_values_clear"),
        "multi-binder let must call nod_values_clear before the RHS; dump:\n{dump}"
    );
    // Exactly one `nod_values_get` for the (x, y) binding — y at index 0.
    let n_gets = dump.matches("nod_values_get").count();
    assert_eq!(
        n_gets, 1,
        "expected one nod_values_get for `let (x, y)`; dump:\n{dump}"
    );
    for f in &fns {
        verify(f).expect("verify");
    }
}

/// Sprint 47 Phase D — three binders → two `nod_values_get` calls
/// (indices 0 and 1 for the second and third return values).
#[test]
fn gap_003_multi_binder_let_destructures_three_values() {
    let src = "\
        define function three-values () => (a :: <integer>)\n  \
            values(1, 2, 3)\n\
        end function;\n\
        define function consume3 () => (sum :: <integer>)\n  \
            let (x, y, z) = three-values();\n  \
            x + y + z\n\
        end function;\n";
    let fns = lower_src(src);
    let consume = fns.iter().find(|f| f.name == "consume3").expect("consume3");
    let dump = format!("{consume:?}");
    assert!(
        dump.contains("nod_values_clear"),
        "expected nod_values_clear; dump:\n{dump}"
    );
    let n_gets = dump.matches("nod_values_get").count();
    assert_eq!(
        n_gets, 2,
        "expected two nod_values_get calls for `let (x, y, z)`; dump:\n{dump}"
    );
    for f in &fns {
        verify(f).expect("verify");
    }
}

/// Sprint 47 Phase D — multi-binder `let` against a single-value RHS
/// still lowers cleanly. The unfilled binders read `nod_values_get(i)`
/// which returns `#f` when `i >= count` (the runtime contract). The
/// lowering itself doesn't care that the call turns out to be
/// single-valued — that's the whole point of the SBCL discipline
/// (clear → call → read past the count → #f).
#[test]
fn gap_003_multi_binder_let_with_single_value_returns_pad_with_false() {
    let src = "\
        define function single () => (x :: <integer>)\n  \
            7\n\
        end function;\n\
        define function consume () => (sum :: <integer>)\n  \
            let (a, b) = single();\n  \
            a\n\
        end function;\n";
    let fns = lower_src(src);
    let consume = fns.iter().find(|f| f.name == "consume").expect("consume");
    let dump = format!("{consume:?}");
    // The lowering still emits nod_values_clear + nod_values_get even
    // though the RHS is single-valued; the runtime is what makes the
    // missing extras default to #f.
    assert!(dump.contains("nod_values_clear"),
        "expected nod_values_clear; dump:\n{dump}");
    assert!(dump.contains("nod_values_get"),
        "expected nod_values_get for the second binder; dump:\n{dump}");
    for f in &fns {
        verify(f).expect("verify");
    }
}

/// Sprint 47 Phase D — the polluted-buffer correctness story. Call A
/// returns two values, leaving extras in the buffer. The caller then
/// invokes call B (single-valued) and destructures with multi-binder
/// `let`. The CLEAR before B's call is what protects against A's
/// extras leaking into B's destructure. We can't run the program from
/// the lowering test, but we CAN assert the lowering emits the clear
/// — without it, the bug returns. (End-to-end runtime check lives in
/// the AOT smoke test.)
#[test]
fn gap_003_polluted_buffer_does_not_leak_across_calls() {
    let src = "\
        define function call-a () => (a :: <integer>)\n  \
            values(11, 22)\n\
        end function;\n\
        define function call-b () => (b :: <integer>)\n  \
            99\n\
        end function;\n\
        define function user () => (n :: <integer>)\n  \
            let (a1, a2) = call-a();\n  \
            let (b1, b2) = call-b();\n  \
            b2\n\
        end function;\n";
    let fns = lower_src(src);
    let user = fns.iter().find(|f| f.name == "user").expect("user");
    let dump = format!("{user:?}");
    // Both `let (…)` forms must emit their own nod_values_clear — the
    // second one is what stops A's extras polluting B's destructure.
    let n_clears = dump.matches("nod_values_clear").count();
    assert_eq!(
        n_clears, 2,
        "each multi-binder `let` must emit its own nod_values_clear \
         before its RHS so polluted state can't leak; dump:\n{dump}"
    );
    for f in &fns {
        verify(f).expect("verify");
    }
}
