//! Sprint 03 fragments + parser tests.

use nod_reader::{
    BinOp, Expr, Fragment, GroupKind, SourceMap, build_fragments, format_ast, lex, parse_expr,
    parse_top_level_exprs,
};

fn lex_src(src: &str) -> (nod_reader::FileId, Vec<nod_reader::Token>, String) {
    let mut sm = SourceMap::new();
    let id = sm.add("<t>", src.to_string()).unwrap();
    let toks = lex(src, id);
    (id, toks, src.to_string())
}

fn parse(src: &str) -> Expr {
    let (_id, toks, owned) = lex_src(src);
    parse_expr(&owned, &toks).expect("parse")
}

fn try_parse(src: &str) -> Result<Expr, String> {
    let (_id, toks, owned) = lex_src(src);
    parse_expr(&owned, &toks).map_err(|d| d.message)
}

// ─── Atom tests ───────────────────────────────────────────────────────

#[test]
fn integer_literal() {
    let e = parse("42");
    match e {
        Expr::Integer(_, 42) => {}
        other => panic!("expected Integer(42), got {other:?}"),
    }
}

#[test]
fn identifier() {
    let e = parse("foo");
    match e {
        Expr::Ident(_, n) if n == "foo" => {}
        other => panic!("expected Ident(\"foo\"), got {other:?}"),
    }
}

// ─── Precedence ──────────────────────────────────────────────────────

#[test]
fn flat_precedence_left_assoc() {
    // Dylan has FLAT operator precedence (DRM): all binary operators share
    // one precedence and associate left. So `1 + 2 * 3` is `(1 + 2) * 3` —
    // Mul at the root with the Add on its lhs — NOT C-style `1 + (2 * 3)`.
    // (Use explicit parens to force `1 + (2 * 3)`; see parens_override_*.)
    let e = parse("1 + 2 * 3");
    match e {
        Expr::BinOp { op: BinOp::Mul, ref lhs, .. } => match lhs.as_ref() {
            Expr::BinOp { op: BinOp::Add, .. } => {}
            other => panic!("expected Add on lhs, got {other:?}"),
        },
        other => panic!("expected Mul at root, got {other:?}"),
    }
}

#[test]
fn parens_override_precedence() {
    // (1 + 2) * 3: + nested under *.
    let e = parse("(1 + 2) * 3");
    match e {
        Expr::BinOp { op: BinOp::Mul, ref lhs, .. } => {
            let mut cur = lhs.as_ref();
            if let Expr::Paren { inner, .. } = cur {
                cur = inner.as_ref();
            }
            match cur {
                Expr::BinOp { op: BinOp::Add, .. } => {}
                other => panic!("expected Add under paren, got {other:?}"),
            }
        }
        other => panic!("expected Mul at root, got {other:?}"),
    }
}

#[test]
fn comparison_parses() {
    let e = parse("x < y");
    match e {
        Expr::BinOp { op: BinOp::Lt, .. } => {}
        other => panic!("expected Lt, got {other:?}"),
    }
}

#[test]
fn assignment_right_assoc() {
    let e = parse("x := y := 1");
    match e {
        Expr::BinOp { op: BinOp::Assign, rhs, .. } => match *rhs {
            Expr::BinOp { op: BinOp::Assign, .. } => {}
            other => panic!("expected nested Assign on rhs, got {other:?}"),
        },
        other => panic!("expected Assign at root, got {other:?}"),
    }
}

#[test]
fn mod_rem_are_flat_operators() {
    // `mod`/`rem` are infix word-operators that share the single flat
    // precedence with every other binary operator (DRM). So `a + b mod c`
    // associates left as `(a + b) mod c` — Mod at the root, Add on its lhs —
    // not the C-style multiplicative `a + (b mod c)`.
    let e = parse("a + b mod c");
    match e {
        Expr::BinOp { op: BinOp::Mod, lhs, .. } => match *lhs {
            Expr::BinOp { op: BinOp::Add, .. } => {}
            other => panic!("expected Add on lhs, got {other:?}"),
        },
        other => panic!("expected Mod at root, got {other:?}"),
    }
}

// ─── Forms ──────────────────────────────────────────────────────────

#[test]
fn if_then_else() {
    let e = parse("if (x) 1 else 2 end");
    match e {
        Expr::If { else_: Some(_), .. } => {}
        other => panic!("expected If with else, got {other:?}"),
    }
}

#[test]
fn if_no_else() {
    let e = parse("if (x) 1 end");
    match e {
        Expr::If { else_: None, .. } => {}
        other => panic!("expected If without else, got {other:?}"),
    }
}

#[test]
fn begin_block() {
    let e = parse("begin x; y; z end");
    match e {
        Expr::Begin { body, .. } => assert_eq!(body.len(), 3),
        other => panic!("expected Begin, got {other:?}"),
    }
}

#[test]
fn function_call() {
    let e = parse("f(1, 2)");
    match e {
        Expr::Call { args, .. } => assert_eq!(args.len(), 2),
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn function_call_zero_args() {
    let e = parse("f()");
    match e {
        Expr::Call { args, .. } => assert_eq!(args.len(), 0),
        other => panic!("expected Call, got {other:?}"),
    }
}

#[test]
fn let_simple() {
    let e = parse("let x = 1");
    match e {
        Expr::Let { binder, .. } => assert_eq!(binder, "x"),
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn anon_method() {
    let e = parse("method (x :: <integer>) x + 1 end");
    let dump = format_ast(&e);
    assert!(dump.contains("(Method"), "dump: {dump}");
    assert!(dump.contains("(Param \"x\""), "dump: {dump}");
    assert!(dump.contains("(BinOp +"), "dump: {dump}");
}

#[test]
fn local_method_form() {
    let e = parse("local method g(x) x + x end");
    match e {
        Expr::LocalMethod { name, .. } => assert_eq!(name, "g"),
        other => panic!("expected LocalMethod, got {other:?}"),
    }
}

#[test]
fn unary_minus() {
    let e = parse("-x");
    match e {
        Expr::UnOp { op: nod_reader::UnOp::Neg, .. } => {}
        other => panic!("expected UnOp Neg, got {other:?}"),
    }
}

#[test]
fn select_stub_diagnostic() {
    let err = try_parse("select x end").expect_err("select should error");
    assert!(err.contains("select"), "diag: {err}");
}

#[test]
fn top_level_semicolon_sequence() {
    let src = "1; 2; 3";
    let (_, toks, owned) = lex_src(src);
    let exprs = parse_top_level_exprs(&owned, &toks).expect("parse seq");
    assert_eq!(exprs.len(), 3);
}

// ─── Fragments ──────────────────────────────────────────────────────

#[test]
fn fragments_smoke_groups() {
    let src = "(a, [b], {c})";
    let (_, toks, _) = lex_src(src);
    let frags = build_fragments(&toks).expect("fragments");
    assert_eq!(frags.len(), 1);
    let body = match &frags[0] {
        Fragment::Group { kind: GroupKind::Paren, body, .. } => body,
        other => panic!("outer should be paren, got {other:?}"),
    };
    let groups: Vec<_> = body
        .iter()
        .filter_map(|f| match f {
            Fragment::Group { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(groups, vec![GroupKind::Bracket, GroupKind::Brace]);
}

#[test]
fn fragments_hash_groups() {
    let src = "#(1, 2) #[3] #{4}";
    let (_, toks, _) = lex_src(src);
    let frags = build_fragments(&toks).expect("fragments");
    let kinds: Vec<_> = frags
        .iter()
        .filter_map(|f| match f {
            Fragment::Group { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![GroupKind::HashParen, GroupKind::HashBracket, GroupKind::HashBrace],
    );
}

#[test]
fn fragments_unclosed_errors() {
    let src = "(a";
    let (_, toks, _) = lex_src(src);
    let err = build_fragments(&toks).expect_err("should error");
    assert!(matches!(err, nod_reader::FragmentError::Unclosed { .. }), "got {err:?}");
}

#[test]
fn fragments_mismatched_errors() {
    let src = "(a]";
    let (_, toks, _) = lex_src(src);
    let err = build_fragments(&toks).expect_err("should error");
    assert!(matches!(err, nod_reader::FragmentError::Mismatched { .. }), "got {err:?}");
}
