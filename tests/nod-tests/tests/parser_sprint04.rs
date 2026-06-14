//! Sprint 04 — top-level definition parser + statement-level grammar.

use nod_reader::{
    Expr, Item, Module, SourceMap, Statement, format_dylan, lex, parse_module, scan_preamble,
};

fn parse_src(src: &str) -> Module {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    parse_module(src, &toks, pre.as_ref()).unwrap_or_else(|d| {
        panic!("parse_module diagnostics: {d:?}\n--- src ---\n{src}");
    })
}

fn round_trip_shape_equal(src: &str) -> Result<(usize, usize), String> {
    let mut sm = SourceMap::new();
    let id = sm.add("<rt>", src.to_string()).unwrap();
    let toks = lex(src, id);
    let pre = scan_preamble(src);
    let m1 = parse_module(src, &toks, pre.as_ref()).map_err(|d| format!("first parse: {d:?}"))?;
    let printed = format_dylan(&m1);
    let mut sm2 = SourceMap::new();
    let id2 = sm2.add("<rt2>", printed.clone()).unwrap();
    let toks2 = lex(&printed, id2);
    let pre2 = scan_preamble(&printed);
    let m2 = parse_module(&printed, &toks2, pre2.as_ref()).map_err(|d| {
        format!(
            "second parse failed: {d:?}\n--- printed ---\n{printed}\n--- original ---\n{src}"
        )
    })?;
    if m1.items.len() != m2.items.len() {
        return Err(format!(
            "item count mismatch: {} vs {}\n--- printed ---\n{printed}",
            m1.items.len(),
            m2.items.len()
        ));
    }
    for (a, b) in m1.items.iter().zip(m2.items.iter()) {
        if a.kind_tag() != b.kind_tag() {
            return Err(format!(
                "item-kind mismatch: {} vs {}",
                a.kind_tag(),
                b.kind_tag()
            ));
        }
    }
    Ok((m1.items.len(), m2.items.len()))
}

// ─── 1. define constant ─────────────────────────────────────────────────

#[test]
fn define_constant_basic() {
    let m = parse_src("define constant x = 1;");
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineConstant { name, value: Expr::Integer(_, 1), .. } => {
            assert_eq!(name, "x");
        }
        other => panic!("expected DefineConstant, got {other:?}"),
    }
    let _ = round_trip_shape_equal("define constant x = 1;").unwrap();
}

// ─── 2. define variable with type ───────────────────────────────────────

#[test]
fn define_variable_typed() {
    let src = "define variable *count* :: <integer> = 0;";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineVariable { name, type_, .. } => {
            assert_eq!(name, "*count*");
            assert!(type_.is_some(), "expected type annotation");
        }
        other => panic!("expected DefineVariable, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 3. define function with one body ───────────────────────────────────

#[test]
fn define_function_basic() {
    let src = "define function sq (x :: <integer>) => (<integer>) x * x end function sq;";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineFunction { name, params, body, return_, .. } => {
            assert_eq!(name, "sq");
            assert_eq!(params.len(), 1);
            assert_eq!(body.len(), 1);
            assert!(return_.is_some());
        }
        other => panic!("expected DefineFunction, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 4. define method ───────────────────────────────────────────────────

#[test]
fn define_method_basic() {
    let src = "define method square (x :: <integer>) => (<integer>) x * x end;";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineMethod { name, .. } => assert_eq!(name, "square"),
        other => panic!("expected DefineMethod, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 5. define generic ──────────────────────────────────────────────────

#[test]
fn define_generic_basic() {
    let src = "define generic name (x, y) => (z :: <integer>);";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineGeneric { name, params, .. } => {
            assert_eq!(name, "name");
            assert_eq!(params.len(), 2);
        }
        other => panic!("expected DefineGeneric, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 6. define class with slots ────────────────────────────────────────

#[test]
fn define_class_with_slots() {
    let src = "\
define class <point> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;
";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineClass { name, supers, slots, .. } => {
            assert_eq!(name, "<point>");
            assert_eq!(supers.len(), 1);
            assert_eq!(slots.len(), 2);
            assert_eq!(slots[0].init_keyword.as_deref(), Some("x"));
            assert_eq!(slots[1].init_keyword.as_deref(), Some("y"));
        }
        other => panic!("expected DefineClass, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 7. define library ─────────────────────────────────────────────────

#[test]
fn define_library_basic() {
    let src = "\
define library foo
  use dylan;
  export bar;
end library foo;
";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineLibrary { name, uses, exports, .. } => {
            assert_eq!(name, "foo");
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].name, "dylan");
            assert_eq!(exports, &vec!["bar".to_string()]);
        }
        other => panic!("expected DefineLibrary, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 8. define module with import options ──────────────────────────────

#[test]
fn define_module_with_imports() {
    let src = "\
define module foo
  use dylan, import: { make, format-out };
  export do-it;
end module;
";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineModule { name, uses, exports, .. } => {
            assert_eq!(name, "foo");
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].name, "dylan");
            assert!(uses[0].import.is_some());
            assert_eq!(exports, &vec!["do-it".to_string()]);
        }
        other => panic!("expected DefineModule, got {other:?}"),
    }
    round_trip_shape_equal(src).unwrap();
}

// ─── 9. define macro — fragments captured ─────────────────────────────

#[test]
fn define_macro_captures_body() {
    let src = "\
define macro unless
  { unless ?cond ?body end } => { if (~ ?cond) ?body end }
end macro;
";
    let m = parse_src(src);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::DefineMacro { name, body_fragments, .. } => {
            assert_eq!(name, "unless");
            assert!(
                !body_fragments.is_empty(),
                "macro body fragments should be non-empty"
            );
        }
        other => panic!("expected DefineMacro, got {other:?}"),
    }
}

// ─── 10. multi-binder let ─────────────────────────────────────────────

#[test]
fn let_multi_binder() {
    let src = "\
define method demo ()
  let (a, b) = values(1, 2);
  a + b
end;
";
    let m = parse_src(src);
    let body = match &m.items[0] {
        Item::DefineMethod { body, .. } => body,
        other => panic!("expected DefineMethod, got {other:?}"),
    };
    let let_stmt = body
        .iter()
        .find_map(|s| match s {
            Statement::Let { binders, .. } => Some(binders),
            _ => None,
        })
        .expect("let statement");
    assert_eq!(let_stmt.len(), 2);
    assert_eq!(let_stmt[0].name, "a");
    assert_eq!(let_stmt[1].name, "b");
}

// ─── 11. for loop with finally ────────────────────────────────────────

#[test]
fn for_with_finally() {
    let src = "\
define method demo ()
  for (i from 1 to 10)
    format-out(\"%d\", i);
  finally
    i
  end for;
end;
";
    let m = parse_src(src);
    let body = match &m.items[0] {
        Item::DefineMethod { body, .. } => body,
        other => panic!("got {other:?}"),
    };
    let mut saw = false;
    for s in body {
        if let Statement::For { clauses, finally_, .. } = s {
            assert_eq!(clauses.len(), 1);
            assert!(!finally_.is_empty(), "finally body should be present");
            saw = true;
        }
    }
    assert!(saw, "expected Statement::For");
}

// ─── 12. while loop ───────────────────────────────────────────────────

#[test]
fn while_loop_basic() {
    let src = "\
define method demo ()
  while (x < 10)
    x := x + 1
  end
end;
";
    let m = parse_src(src);
    let body = match &m.items[0] {
        Item::DefineMethod { body, .. } => body,
        other => panic!("got {other:?}"),
    };
    let mut saw = false;
    for s in body {
        match s {
            Statement::While { .. } => saw = true,
            // The `while` form when in expression position may wrap as
            // `Statement::Expr(Expr::Stmt(While))` — accept that too.
            Statement::Expr(Expr::Stmt(inner)) => {
                if matches!(inner.as_ref(), Statement::While { .. }) {
                    saw = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw, "expected Statement::While");
}

// ─── 13. block with exception + cleanup ───────────────────────────────

#[test]
fn block_exception_cleanup() {
    let src = "\
define method demo ()
  block (return)
    maybe-throw();
  exception (c :: <error>)
    return(#f);
  cleanup
    cleanup-thing()
  end block
end;
";
    let m = parse_src(src);
    let body = match &m.items[0] {
        Item::DefineMethod { body, .. } => body,
        other => panic!("got {other:?}"),
    };
    let mut saw = false;
    for s in body {
        let block_ref: Option<&Statement> = match s {
            Statement::Block { .. } => Some(s),
            Statement::Expr(Expr::Stmt(inner))
                if matches!(inner.as_ref(), Statement::Block { .. }) =>
            {
                Some(inner.as_ref())
            }
            _ => None,
        };
        if let Some(Statement::Block { exit_var, handlers, cleanup, .. }) = block_ref {
            assert_eq!(exit_var.as_deref(), Some("return"));
            assert!(!handlers.is_empty(), "expected exception handlers");
            assert!(!cleanup.is_empty(), "expected cleanup body");
            saw = true;
        }
    }
    assert!(saw, "expected a Statement::Block");
}

// ─── 14. fixture round-trip fixed point ───────────────────────────────

fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("opendylan-tests").is_dir())
        .expect("opendylan-tests dir not found")
        .join("opendylan-tests")
}

#[test]
fn round_trip_fixed_point_corpus() {
    let root = fixtures_root();
    // 7 hand-picked files (covering constants/classes/functions/library/
    // module/numbers/CMU), then a sweep of every .dylan under
    // `opendylan-tests/sources/dylan/tests/` and `cmu-test-suite/`. The
    // Sprint 04 acceptance criterion is ≥20 round-trip-clean files.
    let mut picks: Vec<std::path::PathBuf> = vec![
        root.join("sources/dylan/tests/constants.dylan"),
        root.join("sources/dylan/tests/classes.dylan"),
        root.join("sources/dylan/tests/functions.dylan"),
        root.join("sources/dylan/tests/library.dylan"),
        root.join("sources/dylan/tests/module.dylan"),
        root.join("sources/dylan/tests/numbers.dylan"),
        root.join("sources/testing/cmu-test-suite/dylan-test.dylan"),
    ];
    let extra_dirs = [
        root.join("sources/dylan/tests"),
        root.join("sources/testing/cmu-test-suite"),
        root.join("sources/collections/tests"),
        root.join("sources/common-dylan/tests"),
        root.join("sources/io/tests"),
    ];
    for d in extra_dirs.iter().filter(|d| d.is_dir()) {
        for entry in std::fs::read_dir(d).expect("readdir") {
            let entry = entry.expect("dir entry");
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("dylan") && !picks.contains(&p) {
                picks.push(p);
            }
        }
    }
    let mut ok = 0;
    let mut skipped: Vec<String> = Vec::new();
    for p in &picks {
        let src = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        match round_trip_shape_equal(&src) {
            Ok((n1, n2)) => {
                assert_eq!(n1, n2);
                ok += 1;
            }
            Err(msg) => {
                skipped.push(format!(
                    "{}: {}",
                    p.display(),
                    &msg[..msg.len().min(200)]
                ));
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "round_trip_fixed_point — {} clean, {} skipped:\n{}",
            ok,
            skipped.len(),
            skipped.join("\n")
        );
    }
    assert!(
        ok >= 20,
        "expected >=20 fixtures to round-trip cleanly, got {ok}; skipped={}",
        skipped.len()
    );
}

// ─── 15. CMU corpus smoke ─────────────────────────────────────────────

#[test]
fn cmu_corpus_smoke() {
    let root = fixtures_root();
    let dir = root.join("sources").join("testing").join("cmu-test-suite");
    let mut tried = 0;
    let mut parsed = 0;
    let mut failed: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read cmu-test-suite dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("dylan") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read fixture");
        tried += 1;
        let mut sm = SourceMap::new();
        let id = sm.add(path.clone(), src.clone()).unwrap();
        let toks = lex(&src, id);
        let pre = scan_preamble(&src);
        match parse_module(&src, &toks, pre.as_ref()) {
            Ok(_) => parsed += 1,
            Err(d) => failed.push(format!("{}: {} diag(s)", path.display(), d.len())),
        }
    }
    assert!(tried >= 2, "no .dylan files found in cmu-test-suite");
    if !failed.is_empty() {
        eprintln!("CMU corpus — files that failed parse_module:\n{}", failed.join("\n"));
    }
    // We don't require everything to parse — fixture files exercise grammar
    // outside the Sprint 04 contract — but at least the small ones should.
    assert!(
        parsed >= 1,
        "no CMU fixture parsed cleanly (tried={tried}, failed={failed:?})"
    );
}

// ─── Smoke: preamble lands on the AST ─────────────────────────────────

#[test]
fn preamble_attaches_to_module() {
    let src = "Module: foo\nAuthor: Anon\n\ndefine constant x = 1;\n";
    let m = parse_src(src);
    assert!(
        m.header.iter().any(|(k, _)| k == "Module"),
        "header missing Module: key: {:?}",
        m.header
    );
}
