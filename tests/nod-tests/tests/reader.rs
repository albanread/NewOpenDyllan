//! Sprint 02 lexer smoke tests — the ten "minimal" cases from
//! `specs/01-lexer.md` §6.

use nod_reader::{SourceMap, Token, TokenKind, lex};

/// Convenience: lex a string and return only the (kind, text) pairs.
fn kinds(src: &str) -> Vec<(TokenKind, String)> {
    let mut sm = SourceMap::new();
    let id = sm.add("<test>", src.to_string()).unwrap();
    let toks: Vec<Token> = lex(src, id);
    toks.into_iter()
        .map(|t| (t.kind, sm.slice(t.span).to_string()))
        .collect()
}

fn drop_eof(mut k: Vec<(TokenKind, String)>) -> Vec<(TokenKind, String)> {
    if let Some(last) = k.last()
        && last.0 == TokenKind::Eof
    {
        k.pop();
    }
    k
}

// ── 1. Boolean literal test (#t / #f) ───────────────────────────────────

#[test]
fn smoke_01_booleans() {
    let v = drop_eof(kinds("#t #f #T #F"));
    assert_eq!(
        v,
        vec![
            (TokenKind::HashTrue, "#t".into()),
            (TokenKind::HashFalse, "#f".into()),
            (TokenKind::HashTrue, "#T".into()),
            (TokenKind::HashFalse, "#F".into()),
        ]
    );
}

// ── 2. Decimal integers (signed and underscored) ────────────────────────

#[test]
fn smoke_02_decimal_integers() {
    let v = drop_eof(kinds("0 123 -456 +789 1_000_000"));
    assert_eq!(
        v,
        vec![
            (TokenKind::Integer, "0".into()),
            (TokenKind::Integer, "123".into()),
            (TokenKind::Integer, "-456".into()),
            (TokenKind::Integer, "+789".into()),
            (TokenKind::Integer, "1_000_000".into()),
        ]
    );
}

// ── 3. Binary integers ──────────────────────────────────────────────────

#[test]
fn smoke_03_binary_integers() {
    let v = drop_eof(kinds("#b0 #b1010 #B1111_0000"));
    assert_eq!(
        v,
        vec![
            (TokenKind::IntegerBin, "#b0".into()),
            (TokenKind::IntegerBin, "#b1010".into()),
            (TokenKind::IntegerBin, "#B1111_0000".into()),
        ]
    );
}

// ── 4. Character literals ───────────────────────────────────────────────

#[test]
fn smoke_04_character_literals() {
    let v = drop_eof(kinds(r"'a' '\n' '\\' '\<41>'"));
    assert_eq!(v.len(), 4);
    for (k, _) in &v {
        assert_eq!(*k, TokenKind::Char, "got {v:?}");
    }
}

// ── 5. Float literals ───────────────────────────────────────────────────

#[test]
fn smoke_05_floats() {
    let v = drop_eof(kinds("3.0 3. .5 3e0 1.5e-10 3.0s0 +6.0 -3.0"));
    assert!(v.iter().all(|(k, _)| *k == TokenKind::Float), "got {v:?}");
    assert_eq!(v.len(), 8);
}

// ── 6. Identifier smoke ────────────────────────────────────────────────
// `<integer>`, `name-with-dashes`, `make`, `+`, `<=`, `set?`, `add!`, `*global*`
// should each lex as exactly one ident-class token. Note the spec's wart:
// `+`, `-`, `<=` lex as *operator* tokens (Plus, Minus, LessEqual), not
// as Ident — see spec §2.7. We assert *single-token* lexing rather than
// "all Ident".

#[test]
fn smoke_06_identifier_alphabet() {
    let cases = [
        ("<integer>", TokenKind::Ident),
        ("name-with-dashes", TokenKind::Ident),
        ("make", TokenKind::Ident),
        ("set?", TokenKind::Ident),
        ("add!", TokenKind::Ident),
        ("*global*", TokenKind::Ident),
    ];
    for (src, expected_kind) in cases {
        let v = drop_eof(kinds(src));
        assert_eq!(v.len(), 1, "{src} did not yield exactly one token: {v:?}");
        assert_eq!(v[0].0, expected_kind, "{src} got {:?}", v[0]);
        assert_eq!(v[0].1, src);
    }
}

// ── 7. Operator smoke ──────────────────────────────────────────────────
// Each input must be exactly one multi-char operator token, not split.

#[test]
fn smoke_07_operator_munch() {
    let cases: &[(&str, TokenKind)] = &[
        (":=", TokenKind::ColonEqual),
        ("==", TokenKind::EqualEqual),
        ("=>", TokenKind::Arrow),
        ("~==", TokenKind::TildeEqualEqual),
        ("::", TokenKind::ColonColon),
        ("...", TokenKind::Ellipsis),
        ("<=", TokenKind::LessEqual),
        (">=", TokenKind::GreaterEqual),
    ];
    for &(src, expected) in cases {
        let v = drop_eof(kinds(src));
        assert_eq!(
            v,
            vec![(expected, src.to_string())],
            "operator {src:?} did not maximal-munch",
        );
    }
}

// ── 8. Nested block comment ────────────────────────────────────────────

#[test]
fn smoke_08_nested_block_comment() {
    let v = drop_eof(kinds("/* a /* b */ c */ x"));
    assert_eq!(v, vec![(TokenKind::Ident, "x".into())]);
}

// ── 9. End-to-end smoke on a real Dylan source file ────────────────────

#[test]
fn smoke_09_real_dylan_file() {
    // The cmu-test-suite demo file is the Sprint 02 acceptance demo.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("opendylan-tests").is_dir())
        .expect("opendylan-tests dir not found")
        .join("opendylan-tests")
        .join("sources")
        .join("testing")
        .join("cmu-test-suite")
        .join("dylan-test.dylan");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut sm = SourceMap::new();
    let id = sm.add(path.clone(), src.clone()).unwrap();
    let toks = lex(&src, id);
    // We assert structural properties rather than a byte-for-byte dump:
    //   - lexer terminates with exactly one EOF
    //   - some recognisable tokens appear
    //   - no panic.
    assert!(toks.len() > 10, "implausibly short token stream");
    assert_eq!(toks.last().unwrap().kind, TokenKind::Eof);
    assert!(
        toks.iter().any(|t| t.kind == TokenKind::KwDefine),
        "expected at least one `define` keyword"
    );
}

// ── 10. Empty-string carve-out (spec §3.8) ─────────────────────────────

#[test]
fn smoke_10_empty_string_carveout() {
    let v = drop_eof(kinds(r#""""#));
    assert_eq!(v, vec![(TokenKind::String, r#""""#.into())]);
}

// ── Bonus regressions ──────────────────────────────────────────────────

#[test]
fn keyword_colon_vs_colon_colon() {
    let v = drop_eof(kinds("foo: x::y"));
    assert_eq!(
        v,
        vec![
            (TokenKind::KeywordColon, "foo:".into()),
            (TokenKind::Ident, "x".into()),
            (TokenKind::ColonColon, "::".into()),
            (TokenKind::Ident, "y".into()),
        ]
    );
}

#[test]
fn three_hard_reserveds() {
    let v = drop_eof(kinds("define end otherwise method class library"));
    assert_eq!(
        v,
        vec![
            (TokenKind::KwDefine, "define".into()),
            (TokenKind::KwEnd, "end".into()),
            (TokenKind::KwOtherwise, "otherwise".into()),
            // Not reserved — plain identifiers per spec §2.1
            (TokenKind::Ident, "method".into()),
            (TokenKind::Ident, "class".into()),
            (TokenKind::Ident, "library".into()),
        ]
    );
}

#[test]
fn preamble_skipped() {
    // Real Dylan file headers — these should be eaten before any token.
    let src = "Module: foo\nAuthor: Anon\n\ndefine constant x = 1;\n";
    let v = drop_eof(kinds(src));
    assert_eq!(v.first().map(|(k, _)| *k), Some(TokenKind::KwDefine));
}
