//! Sprint 52.4 — substitution + hygiene parity gate.
//!
//! Runs a fixed set of (define-macro, call-site) cases through BOTH the
//! Dylan-side macro engine (`dylan-macro-expand.exe`, via `substitute-hyg`)
//! and the Rust engine (`nod_macro::substitute`), under a PINNED hygiene
//! nonce (42), and asserts the resulting expansion text is byte-identical
//! after whitespace normalisation.
//!
//! Cases cover substitution-only (no binders), a `let`-introduced binder,
//! a `method (…)` param binder, and the real stdlib `for-each` (whose
//! `%fip-state` binder is renamed while the `?var`/`?coll`/`?body`
//! pattern variables are not).
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_expand -- --nocapture

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::process::Command;

use nod_macro::PatternElem;
use nod_reader::SourceMap;
use serial_test::serial;

/// Pinned hygiene nonce — MUST match the `"42"` the Dylan driver passes
/// to `substitute-hyg`.
const NONCE: u64 = 42;

/// (case-name, define-macro source, call-site source). MUST stay in sync
/// with `tests/nod-tests/fixtures/dylan-macro-expand.dylan`.
const CASES: &[(&str, &str, &str)] = &[
    (
        "unless",
        "define macro unless { unless ?cond:expression ?body:body end } => { if (~ ?cond) ?body else #f end } end macro;",
        "unless x (foo) end",
    ),
    (
        "let-binder",
        "define macro lt { lt ?e:expression end } => { let tmp = ?e ; tmp end } end macro;",
        "lt (foo) end",
    ),
    (
        "method-param",
        "define macro mm { mm ?e:expression end } => { method (q) q end } end macro;",
        "mm (z) end",
    ),
    (
        "for-each",
        "define macro for-each { for-each (?var:name in ?coll:expression) ?body:body end } => { begin let %fip-state = %fip-init(?coll); until (%fip-finished?(%fip-state)) let ?var = %fip-current-element(%fip-state); ?body; %fip-advance!(%fip-state) end end } end macro;",
        "for-each (i in xs) (work) end",
    ),
    (
        "cond-1arm",
        "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { if (?t1) ?b1 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression ?t4:expression ?b4:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end } end macro;",
        "cond (x) (y) otherwise (z) end",
    ),
    (
        "cond-2arm",
        "define macro cond { cond ?t1:expression ?b1:expression otherwise ?d:expression end } => { if (?t1) ?b1 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 else ?d end } { cond ?t1:expression ?b1:expression ?t2:expression ?b2:expression ?t3:expression ?b3:expression ?t4:expression ?b4:expression otherwise ?d:expression end } => { if (?t1) ?b1 elseif (?t2) ?b2 elseif (?t3) ?b3 elseif (?t4) ?b4 else ?d end } end macro;",
        "cond (a) (b) (c) (d) otherwise (e) end",
    ),
];

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

/// Collapse all whitespace runs to a single space and trim — the two
/// emitters differ only in trailing/uniform spacing, not token order.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_pattern_var_names(pattern: &[PatternElem]) -> HashSet<String> {
    let mut out = HashSet::new();
    fn go(pat: &[PatternElem], out: &mut HashSet<String>) {
        for p in pat {
            match p {
                PatternElem::Variable { name, .. } => {
                    out.insert(name.clone());
                }
                PatternElem::Group { body, .. } => go(body, out),
                PatternElem::Literal { .. } => {}
            }
        }
    }
    go(pattern, &mut out);
    out
}

/// Rust ground truth: parse def, match call, substitute rule 0's template
/// with the bindings under nonce 42; return the normalised expansion.
fn rust_expand(def_src: &str, call_src: &str) -> String {
    let headered = format!("Module: macro-expand\n\n{def_src}");
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
        &HashSet::new(),
    )
    .unwrap_or_else(|d| panic!("rust parse of def `{def_src}` failed: {d:?}"));
    let mut table = nod_macro::MacroTable::default();
    nod_macro::collect_macros(&module, &def_sm, &mut table)
        .unwrap_or_else(|e| panic!("rust collect of `{def_src}` failed: {e:?}"));
    let def = table.defs.values().next().expect("a macro");

    let mut call_sm = SourceMap::new();
    let call_file = call_sm
        .add(PathBuf::from("call.dylan"), call_src.to_string())
        .expect("call source map");
    let call_toks = nod_reader::lex_rust(call_src, call_file);
    let call_frags = nod_reader::build_fragments(&call_toks)
        .unwrap_or_else(|e| panic!("rust fragments of `{call_src}` failed: {e:?}"));

    // Multi-rule selection: first rule whose pattern matches wins
    // (mirrors expand_one's loop and the Dylan engine's expand-call).
    let rule = def
        .rules
        .iter()
        .find(|r| nod_macro::match_pattern_with_source(&r.pattern, &call_frags, call_src).is_some())
        .unwrap_or_else(|| panic!("no rule matched for `{call_src}`"));
    let bindings = nod_macro::match_pattern_with_source(&rule.pattern, &call_frags, call_src)
        .expect("matched rule re-matches");
    let pvars = collect_pattern_var_names(&rule.pattern);
    let out = nod_macro::substitute(
        &rule.template,
        &bindings,
        NONCE,
        call_src,
        &def_sm,
        def_file,
        &pvars,
    );
    normalize(&out.text)
}

/// Parse the Dylan expand driver's stdout into `case -> normalised expansion`.
fn parse_dylan_output(stdout: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("EXPAND ") {
            let (name, text) = rest.split_once(" = ").unwrap_or_else(|| {
                panic!("malformed EXPAND line: {line:?}")
            });
            map.insert(name.trim().to_string(), normalize(text));
        }
    }
    map
}

fn build_expand_exe() -> PathBuf {
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
    let prj = fixtures_dir().join("dylan-macro-expand.prj");
    let exe = fixtures_dir().join("dylan-macro-expand.exe");
    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build of dylan-macro-expand.prj failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );
    assert!(exe.is_file(), "expected {} after build", exe.display());
    exe
}

#[test]
#[serial]
fn dylan_substitute_hygiene_matches_rust() {
    let exe = build_expand_exe();
    let run = Command::new(&exe).output().expect("spawn expand exe");
    assert!(
        run.status.success(),
        "dylan-macro-expand.exe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    let dylan = parse_dylan_output(&stdout);

    for (name, def_src, call_src) in CASES {
        let rust = rust_expand(def_src, call_src);
        let got = dylan
            .get(*name)
            .unwrap_or_else(|| panic!("dylan driver produced no EXPAND {name}\n{stdout}"));
        assert_eq!(
            got, &rust,
            "expansion divergence for case `{name}`:\n  rust  = {rust:?}\n  dylan = {got:?}",
        );
    }

    // Anchor the hygiene rename so a regression that disables it (or
    // changes the gensym spelling) trips even if both engines agree.
    assert_eq!(
        dylan.get("let-binder").unwrap(),
        "let tmp__nod_hyg_42 = ( foo ) ; tmp__nod_hyg_42 end"
    );
    assert!(
        dylan.get("for-each").unwrap().contains("%fip-state__nod_hyg_42"),
        "for-each should rename the %fip-state binder"
    );
    assert!(
        !dylan.get("unless").unwrap().contains("__nod_hyg_"),
        "unless has no binders — nothing should be renamed"
    );
}
