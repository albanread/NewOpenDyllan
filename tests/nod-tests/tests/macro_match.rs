//! Sprint 52.3 — pattern-matching parity gate.
//!
//! Drives a fixed set of (define-macro, call-site) cases through BOTH
//! the Dylan-side macro engine (`dylan-macro-match.exe`, via
//! `match-pattern`) and the Rust engine (`nod_macro::match_pattern`,
//! with call fragments built by `nod_reader::build_fragments`), and
//! asserts the resulting bindings are identical — same variable names,
//! same captured-fragment text — for every case.
//!
//! The cases exercise every nod-macro `PatternKind`: expression, name,
//! body (end-delimited AND keyword-delimited via `with-cleanup`),
//! variable, macro-arg, parameter-list, constraint, plus a group pattern
//! (`for-each`). Bindings are compared as sorted `name = value` lists so
//! the Rust side's HashMap order is irrelevant.
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_match -- --nocapture

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use nod_reader::fragments::{Fragment, GroupKind};
use nod_reader::SourceMap;
use serial_test::serial;

/// (case-name, define-macro source, call-site source). MUST stay in sync
/// with `tests/nod-tests/fixtures/dylan-macro-match.dylan`'s `run-case`
/// calls — same cases, same order is not required (keyed by name).
const CASES: &[(&str, &str, &str)] = &[
    (
        "unless",
        "define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;",
        "unless x (foo) end",
    ),
    (
        "when",
        "define macro when { when ?cond:expression ?body:body end } => { if (?cond) ?body else #f end } end macro;",
        "when y (g) end",
    ),
    (
        "for-each",
        "define macro for-each { for-each (?var:name in ?coll:expression) ?body:body end } => { 1 } end macro;",
        "for-each (i in xs) (work) end",
    ),
    (
        "with-cleanup",
        "define macro with-cleanup { with-cleanup ?body:body cleanup ?cleanup:body end } => { 1 } end macro;",
        "with-cleanup (a) cleanup (b) end",
    ),
    (
        "cond",
        "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { 1 } end macro;",
        "cond (x) (y) otherwise (z) end",
    ),
    (
        "name",
        "define macro nm { nm ?x:name end } => { 1 } end macro;",
        "nm foo end",
    ),
    (
        "variable",
        "define macro vv { vv ?x:variable end } => { 1 } end macro;",
        "vv a end",
    ),
    (
        "parameter-list",
        "define macro pl { pl ?p:parameter-list end } => { 1 } end macro;",
        "pl (a, b) end",
    ),
    (
        "macro-arg",
        "define macro ma { ma ?x:macro-arg end } => { 1 } end macro;",
        "ma z end",
    ),
    (
        "constraint",
        "define macro co { co ?x:constraint end } => { 1 } end macro;",
        "co w end",
    ),
];

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

fn group_delims(k: GroupKind) -> (&'static str, &'static str) {
    match k {
        GroupKind::Paren => ("(", ")"),
        GroupKind::Bracket => ("[", "]"),
        GroupKind::Brace => ("{", "}"),
        GroupKind::HashParen => ("#(", ")"),
        GroupKind::HashBracket => ("#[", "]"),
        GroupKind::HashBrace => ("#{", "}"),
    }
}

/// Flatten a fragment sequence to single-space-joined chunks, exactly as
/// the Dylan engine's `emit-frag` + `join-chunks` do: each token's text
/// is one chunk; a group is `open`, its (recursively flattened) body,
/// then `close`.
fn render_frags(frags: &[Fragment], sm: &SourceMap) -> String {
    let mut chunks = Vec::new();
    push_frags(frags, sm, &mut chunks);
    chunks.join(" ")
}

fn push_frags(frags: &[Fragment], sm: &SourceMap, chunks: &mut Vec<String>) {
    for f in frags {
        match f {
            Fragment::Token(t) => chunks.push(sm.slice(t.span).to_string()),
            Fragment::Group { kind, body, .. } => {
                let (open, close) = group_delims(*kind);
                chunks.push(open.to_string());
                push_frags(body, sm, chunks);
                chunks.push(close.to_string());
            }
        }
    }
}

/// Rust ground truth: parse the def, take rule 0's pattern, build the
/// call fragments, `match_pattern`, and render each binding to a sorted
/// `name = value` list (or `["<NOMATCH>"]`).
fn rust_bindings(def_src: &str, call_src: &str) -> Vec<String> {
    // Parse the define-macro. A `Module:` header keeps the canonical
    // parser on its normal path; spans into the headered source still
    // slice the right literal text for pattern literals.
    let headered = format!("Module: macro-match\n\n{def_src}");
    let mut def_sm = SourceMap::new();
    let def_file = def_sm
        .add(PathBuf::from("def.dylan"), headered.clone())
        .expect("def source map");
    let def_toks = nod_reader::lex_rust(&headered, def_file);
    let def_pre = nod_reader::scan_preamble(&headered);
    let module = nod_reader::parse_module_with_macros_rust(
        &headered,
        &def_toks,
        def_pre.as_ref(),
        &std::collections::HashSet::new(),
    )
    .unwrap_or_else(|d| panic!("rust parse of def `{def_src}` failed: {d:?}"));
    let mut table = nod_macro::MacroTable::default();
    nod_macro::collect_macros(&module, &def_sm, &mut table)
        .unwrap_or_else(|e| panic!("rust collect of `{def_src}` failed: {e:?}"));
    let def = table
        .defs
        .values()
        .next()
        .unwrap_or_else(|| panic!("no macro collected from `{def_src}`"));
    let pattern = &def.rules[0].pattern;

    // Build the call-site fragments the same way the Dylan side does:
    // lex (trivia-free) then group-balance.
    let mut call_sm = SourceMap::new();
    let call_file = call_sm
        .add(PathBuf::from("call.dylan"), call_src.to_string())
        .expect("call source map");
    let call_toks = nod_reader::lex_rust(call_src, call_file);
    let call_frags = nod_reader::build_fragments(&call_toks)
        .unwrap_or_else(|e| panic!("rust fragments of `{call_src}` failed: {e:?}"));

    // `match_pattern` reads literal text + Name bindings from a
    // thread-local call-site source; bind it via the oracle helper.
    match nod_macro::match_pattern_with_source(pattern, &call_frags, call_src) {
        None => vec!["<NOMATCH>".to_string()],
        Some(bindings) => {
            let mut lines: Vec<String> = bindings
                .iter()
                .map(|(name, mf)| {
                    let value = match mf {
                        nod_macro::MatchedFragment::Token(_, s) => s.clone(),
                        nod_macro::MatchedFragment::Frags(v) => render_frags(v, &call_sm),
                    };
                    format!("{name} = {value}")
                })
                .collect();
            lines.sort();
            lines
        }
    }
}

/// Parse the Dylan match driver's stdout into `case -> sorted bindings`.
fn parse_dylan_output(stdout: &str) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("CASE ") {
            current = Some(name.trim().to_string());
            map.entry(name.trim().to_string()).or_default();
        } else if let Some(bind) = line.strip_prefix("BIND ") {
            let case = current.as_ref().expect("BIND before CASE");
            map.get_mut(case).unwrap().push(bind.trim().to_string());
        } else if line.trim() == "NOMATCH" {
            let case = current.as_ref().expect("NOMATCH before CASE");
            map.get_mut(case).unwrap().push("<NOMATCH>".to_string());
        } else if line.trim() == "NODEF" {
            panic!("dylan match driver reported NODEF for case {current:?}");
        }
    }
    for v in map.values_mut() {
        v.sort();
    }
    map
}

fn build_match_exe() -> PathBuf {
    let workspace = workspace_root();
    let build = Command::new("cargo")
        .current_dir(&workspace)
        .args(["build", "-p", "nod-driver"])
        .output()
        .expect("spawn cargo build");
    assert!(
        build.status.success(),
        "cargo build -p nod-driver failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let prj = fixtures_dir().join("dylan-macro-match.prj");
    let exe = fixtures_dir().join("dylan-macro-match.exe");
    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build of dylan-macro-match.prj failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );
    assert!(exe.is_file(), "expected {} after build", exe.display());
    exe
}

#[test]
#[serial]
fn dylan_match_pattern_matches_rust() {
    let exe = build_match_exe();
    let run = Command::new(&exe).output().expect("spawn match exe");
    assert!(
        run.status.success(),
        "dylan-macro-match.exe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    let dylan = parse_dylan_output(&stdout);

    for (name, def_src, call_src) in CASES {
        let rust = rust_bindings(def_src, call_src);
        let got = dylan
            .get(*name)
            .unwrap_or_else(|| panic!("dylan driver produced no CASE {name}\n{stdout}"));
        assert_eq!(
            got, &rust,
            "binding divergence for case `{name}`:\n  rust  = {rust:?}\n  dylan = {got:?}\n--- full dylan output ---\n{stdout}",
        );
    }

    // Anchor a couple of headline values so a regression that breaks both
    // engines identically still trips.
    assert_eq!(dylan.get("unless").unwrap(), &vec!["body = ( foo )".to_string(), "cond = x".to_string()]);
    assert_eq!(
        dylan.get("parameter-list").unwrap(),
        &vec!["p = ( a , b )".to_string()]
    );
}
