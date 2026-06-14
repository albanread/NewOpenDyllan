//! Sprint 17 — macro expander integration tests.

use std::path::{Path, PathBuf};

use nod_macro::{
    MacroDef, MacroError, MacroTable, PatternElem, PatternKind, TemplateElem, collect_and_expand,
    collect_macros, expand_module,
};
use nod_reader::{Expr, Item, Module, SourceMap, format_ast_module, lex, parse_module};
use nod_sema::{dump_expanded_for_file, run_function_to_i64};
use serial_test::serial;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn parse(src: &str) -> (Module, SourceMap) {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = nod_reader::scan_preamble(src);
    let m = parse_module(src, &toks, pre.as_ref())
        .unwrap_or_else(|d| panic!("parse: {d:?}\n--- src ---\n{src}"));
    (m, sm)
}

// ─── 1. Headline acceptance: `unless` macro expands + JIT returns 42 ───

#[test]
#[serial]
fn unless_macro_end_to_end_returns_42() {
    let path = fixtures_dir().join("macros-unless.dylan");
    let r = run_function_to_i64(&path, "test").expect("run test()");
    assert_eq!(r, 42, "unless (1 = 0) 42 end should return 42 after expansion");
}

// ─── 2. Hygiene: macro-introduced `let x` doesn't capture user's `x` ───

#[test]
#[serial]
fn hygiene_let_no_capture() {
    // Macro template introduces a `let x = …`; user code has its own `x`.
    // After expansion, the two `x`s should resolve to distinct bindings,
    // i.e. the user's `x` reads the user's value (10), not the macro's
    // intermediate.
    let src = "\
Module: hyg

define macro stash
  { stash ?val:expression }
    => { begin let x = ?val; x + x end }
end macro;

define function f () => (<integer>)
  let x = 10;
  stash(7) + x
end function f;
";
    let (mut m, sm) = parse(src);
    let mut table = MacroTable::default();
    collect_macros(&m, &sm, &mut table).expect("collect");
    expand_module(&mut m, &table, &sm).expect("expand");
    let dump = format_ast_module(&m);
    // The expanded body should reference the user's `x` and the
    // hygiene-renamed template `x` distinctly.
    assert!(
        dump.contains("x__nod_hyg_"),
        "expected hygiene-renamed identifier in expansion, got:\n{dump}"
    );
}

// ─── 3. Source span: pattern-bound subtree keeps call-site span ─────────

#[test]
#[serial]
fn span_pattern_bound_keeps_call_site() {
    let src = "\
Module: spans

define macro unless
  { unless ?cond:expression ?body:expression end } => { if (~ ?cond) ?body else 0 end }
end macro;

define function g () => (<integer>)
  unless (1 = 0) 42 end
end function g;
";
    let (mut m, sm) = parse(src);
    nod_macro::collect_and_expand(&mut m, &sm).expect("collect+expand");
    // After expansion, the `42` literal's span should still point at
    // the user's `42`, not at the macro template's `?body` reference.
    let body_42_span = find_integer_span(&m, 42).expect("integer 42 missing");
    let user_42_offset = src.find("42").unwrap() as u32;
    assert_eq!(
        body_42_span.lo, user_42_offset,
        "pattern-bound `42` should keep call-site lo offset"
    );
}

// ─── 4. Source span: template-introduced tokens carry template span ────

#[test]
#[serial]
fn span_template_keeps_template_span() {
    // The template emits `~ ?cond` — the `~` UnOp is a purely
    // template-introduced subexpression. Its span should anchor in
    // the macro DEFINITION's source, not the call site.
    let src = "\
Module: spans2

define macro unless
  { unless ?cond:expression ?body:expression end } => { if (~ ?cond) ?body else 0 end }
end macro;

define function g () => (<integer>)
  unless (1 = 0) 42 end
end function g;
";
    let (mut m, sm) = parse(src);
    nod_macro::collect_and_expand(&mut m, &sm).expect("collect+expand");
    let tilde_span = find_first_unop_span(&m).expect("unary `~` span missing");
    // The macro DEFINITION's `~` is at `(~ ?cond)` inside the brace-
    // template. Inner template-only nodes should anchor at the
    // template, not at the call site (which lives below "define
    // function g").
    let def_body_start = src.find("define macro unless").unwrap() as u32;
    let call_site_start = src.find("define function g").unwrap() as u32;
    assert!(
        tilde_span.lo >= def_body_start && tilde_span.lo < call_site_start,
        "expected `~`'s span at lo={} to fall inside the macro definition \
         (def starts at {}, call starts at {})",
        tilde_span.lo,
        def_body_start,
        call_site_start
    );
}

// ─── 5. Recursive macro hits depth limit ───────────────────────────────

#[test]
#[serial]
fn recursive_macro_depth_exceeded() {
    let src = "\
Module: rec

define macro forever
  { forever ?x:expression } => { forever ?x }
end macro;

define function loops () => (<integer>)
  forever (1)
end function loops;
";
    let (mut m, sm) = parse(src);
    let mut table = MacroTable::default();
    collect_macros(&m, &sm, &mut table).expect("collect");
    let err = expand_module(&mut m, &table, &sm).expect_err("recursive must error");
    let saw_depth = err
        .iter()
        .any(|e| matches!(e, MacroError::ExpansionDepthExceeded { .. }));
    assert!(saw_depth, "expected ExpansionDepthExceeded, got: {err:?}");
}

// ─── 6. (Sprint 17) multi-rule rejection — superseded by Sprint 18 ─────
//
// Sprint 18 ships first-match multi-rule selection. The Sprint 17 test
// that asserted `MultipleRulesNotSupported` no longer applies; the
// Sprint 18 success path is covered by `macros_sprint18::multi_rule_*`.
//
// `MacroError::MultipleRulesNotSupported` remains in the enum to keep
// downstream `match`-arms exhaustive without churn.

// ─── 7. `?x:body` smoke test ───────────────────────────────────────────

#[test]
#[serial]
fn body_kind_matches_remainder() {
    let src = "\
Module: bodyk

define macro for-each-task
  { for-each-task ?body:body end } => { begin ?body end }
end macro;
";
    let (m, sm) = parse(src);
    let mut table = MacroTable::default();
    collect_macros(&m, &sm, &mut table).expect("collect");
    let def = table.get("for-each-task").expect("for-each-task registered");
    // Pattern: [Literal(for-each-task), Variable(?body, Body), Literal(end)]
    let p = &def.rules[0].pattern;
    let saw_body_kind = p.iter().any(|pe| {
        matches!(
            pe,
            PatternElem::Variable {
                kind: PatternKind::Body,
                ..
            }
        )
    });
    assert!(saw_body_kind, "expected a body-kind pattern element: {p:?}");
}

// ─── 8. Regression: existing kernel-arith fixture still lowers ─────────

#[test]
#[serial]
fn regression_kernel_arith_unaffected_by_expander() {
    // A fixture with NO macro use; expansion must be a no-op.
    let path = fixtures_dir().join("kernel-arith.dylan");
    let ir = nod_sema::dump_llvm_for_file(&path).expect("dump kernel-arith IR");
    assert!(ir.contains("hypot-sq"));
}

// ─── 9. `dump_expanded_for_file` produces sensible output ──────────────

#[test]
#[serial]
fn dump_expanded_shows_post_expansion_ast() {
    let path = fixtures_dir().join("macros-unless.dylan");
    let s = dump_expanded_for_file(&path).expect("dump expanded");
    // After expansion, the AST should contain `If`, not `Unless`, and
    // `unless`-related Define entries should be gone (only `test`
    // remains as a DefineFunction).
    assert!(
        s.contains("(If"),
        "expected `(If` in expanded AST dump:\n{s}"
    );
    assert!(
        !s.contains("(Unless"),
        "did NOT expect `(Unless` in expanded AST dump:\n{s}"
    );
    assert!(
        !s.contains("(DefineMacro"),
        "did NOT expect `(DefineMacro` in expanded AST dump:\n{s}"
    );
}

// ─── 10. Built-in names inside templates are NOT hygiene-renamed ───────

#[test]
#[serial]
fn template_builtin_names_not_renamed() {
    // The `unless` template uses `if`, which is a built-in form. The
    // hygiene policy must NOT rename `if` because then `if (...) ... end`
    // would no longer be recognised by the parser and the expansion
    // wouldn't re-parse.
    let src = "\
Module: noren

define macro unless
  { unless ?cond:expression ?body:expression end } => { if (~ ?cond) ?body else 0 end }
end macro;

define function h () => (<integer>)
  unless (1 = 0) 7 end
end function h;
";
    let (mut m, sm) = parse(src);
    collect_and_expand(&mut m, &sm).expect("collect+expand");
    let dump = format_ast_module(&m);
    // `(If` must appear in the lowered AST — confirms the `if` token
    // wasn't hygiene-renamed (otherwise the parser would have rejected
    // `if__nod_hyg_1 (~ …) … end`).
    assert!(
        dump.contains("(If"),
        "expected `(If` after expansion (hygiene shouldn't rename `if`):\n{dump}"
    );
}

// ─── helpers ───────────────────────────────────────────────────────────

fn find_integer_span(m: &Module, target: i128) -> Option<nod_reader::Span> {
    for it in &m.items {
        if let Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } = it {
            for s in body {
                if let Some(sp) = find_integer_in_stmt(s, target) {
                    return Some(sp);
                }
            }
        }
    }
    None
}

fn find_integer_in_stmt(s: &nod_reader::Statement, target: i128) -> Option<nod_reader::Span> {
    use nod_reader::Statement;
    match s {
        Statement::Expr(e) => find_integer_in_expr(e, target),
        Statement::Let { value, .. } => find_integer_in_expr(value, target),
        Statement::For { body, .. }
        | Statement::While { body, .. }
        | Statement::Until { body, .. } => {
            for s2 in body {
                if let Some(sp) = find_integer_in_stmt(s2, target) {
                    return Some(sp);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_integer_in_expr(e: &Expr, target: i128) -> Option<nod_reader::Span> {
    match e {
        Expr::Integer(sp, v) if *v == target => Some(*sp),
        Expr::BinOp { lhs, rhs, .. } => {
            find_integer_in_expr(lhs, target).or_else(|| find_integer_in_expr(rhs, target))
        }
        Expr::UnOp { operand, .. } => find_integer_in_expr(operand, target),
        Expr::Paren { inner, .. } => find_integer_in_expr(inner, target),
        Expr::If { cond, then_, else_, .. } => find_integer_in_expr(cond, target)
            .or_else(|| find_integer_in_expr(then_, target))
            .or_else(|| else_.as_ref().and_then(|b| find_integer_in_expr(b, target))),
        Expr::Begin { body, .. } => body.iter().find_map(|b| find_integer_in_expr(b, target)),
        Expr::Call { callee, args, .. } => find_integer_in_expr(callee, target)
            .or_else(|| args.iter().find_map(|a| find_integer_in_expr(a, target))),
        _ => None,
    }
}

fn find_first_unop_span(m: &Module) -> Option<nod_reader::Span> {
    for it in &m.items {
        if let Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } = it {
            for s in body {
                if let Some(sp) = find_unop_in_stmt(s) {
                    return Some(sp);
                }
            }
        }
    }
    None
}

fn find_unop_in_stmt(s: &nod_reader::Statement) -> Option<nod_reader::Span> {
    use nod_reader::Statement;
    match s {
        Statement::Expr(e) => find_unop_in_expr(e),
        Statement::Let { value, .. } => find_unop_in_expr(value),
        _ => None,
    }
}

fn find_unop_in_expr(e: &Expr) -> Option<nod_reader::Span> {
    match e {
        Expr::UnOp { span, .. } => Some(*span),
        Expr::Paren { inner, .. } => find_unop_in_expr(inner),
        Expr::Begin { body, .. } => body.iter().find_map(find_unop_in_expr),
        Expr::If { cond, then_, else_, .. } => find_unop_in_expr(cond)
            .or_else(|| find_unop_in_expr(then_))
            .or_else(|| else_.as_ref().and_then(|b| find_unop_in_expr(b))),
        Expr::BinOp { lhs, rhs, .. } => find_unop_in_expr(lhs).or_else(|| find_unop_in_expr(rhs)),
        Expr::Call { callee, args, .. } => find_unop_in_expr(callee)
            .or_else(|| args.iter().find_map(find_unop_in_expr)),
        _ => None,
    }
}

// ─── Sprint 25 — stdlib `unless` as a body-shaped macro call ───────────

#[test]
#[serial]
fn sprint25_unless_macro_false_cond_runs_body() {
    // `unless (#f) <body> end` ≡ `if (~ #f) <body> end` — the body
    // runs. After Sprint 25, this surface goes through the stdlib's
    // `define macro unless` (no parser-side hardcoded `Expr::Unless`).
    let s = nod_sema::eval_expr_to_string("unless (#f) 42 end")
        .expect("eval `unless (#f) 42 end`");
    assert_eq!(s, "42", "unless (#f) 42 end must evaluate the body");
}

#[test]
#[serial]
fn sprint25_unless_macro_true_cond_skips_body() {
    // `unless (#t) <body> end` ≡ `if (~ #t) <body> end` — the body
    // doesn't run; the `if` has no else, so the result is the
    // canonical-false value (`#f`).
    let s = nod_sema::eval_expr_to_string("unless (#t) 1 end")
        .expect("eval `unless (#t) 1 end`");
    assert_eq!(s, "#f", "unless (#t) 1 end must skip the body");
}

#[test]
#[serial]
fn sprint25_unless_via_stdlib_replaces_hardcoded_form() {
    // Sprint 17/18 fixture: `unless (1 = 0) 42 end` — confirms the
    // stdlib `unless` macro (the body-shaped path) yields the same
    // result the now-deleted `Expr::Unless` hardcoded form produced.
    let s = nod_sema::eval_expr_to_string("unless (1 = 0) 42 end")
        .expect("eval `unless (1 = 0) 42 end`");
    assert_eq!(s, "42");
}

// WHY: silence unused-import warning on MacroDef / TemplateElem — these
// types are part of the public surface and we want them tested via the
// `MacroTable::get` return shape, but tests don't directly construct them.
#[allow(dead_code)]
fn _surface(d: &MacroDef, t: &TemplateElem) {
    let _ = (d, t);
}
