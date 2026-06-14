//! Sprint 52.5 — module-walk expansion gate.
//!
//! Builds the Dylan module-walk driver (`dylan-macro-walk.prj`) and
//! checks its WALK lines against hand-verified expectations. The driver
//! collects a macro table from a def source, then `expand-module-source`
//! walks the input fragment stream, expanding every macro call (multi-rule
//! selection), re-lexing each expansion, and recursing to fixpoint.
//!
//! These expectations are hand-verified; the authoritative AST-level
//! cross-check against the Rust expander lands in 52.6 (front-end
//! integration + dump-expanded byte-identical gate over the corpus).
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_walk -- --nocapture

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serial_test::serial;

/// (case-name, expected expansion). MUST stay in sync with the cases in
/// `tests/nod-tests/fixtures/dylan-macro-walk.dylan`.
const EXPECTED: &[(&str, &str)] = &[
    // A call embedded in begin … end: surrounding fragments pass through.
    ("embedded", "begin if ( ~ x ) ( b ) else #f end end"),
    // No macro call — verbatim.
    ("passthrough", "foo ( bar )"),
    // Recursion to fixpoint: neg → unless → if (the `1` literal survives
    // the re-lex, exercising the 52.5 literal round-trip).
    ("recursion", "if ( ~ y ) ( 1 ) else #f end"),
    // Multi-rule selection inside the walk (4-rule cond, 1-pair call).
    ("cond-walk", "if ( ( x ) ) ( y ) else ( z ) end"),
    // Two sibling calls in one stream — both expand.
    (
        "siblings",
        "if ( ~ a ) ( p ) else #f end if ( ~ b ) ( q ) else #f end",
    ),
    // A `define macro` in the input is stripped from the expanded output
    // (compile-time only); the following call still expands.
    ("strip-def", "if ( ~ x ) ( y ) end"),
    // Call-shaped macro `name(args)` — no `end`.
    ("call-form", "5 + 5"),
];

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
#[serial]
fn dylan_module_walk_expands_to_fixpoint() {
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

    let prj = fixtures_dir().join("dylan-macro-walk.prj");
    let exe = fixtures_dir().join("dylan-macro-walk.exe");
    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build of dylan-macro-walk.prj failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );

    let run = Command::new(&exe).output().expect("spawn walk exe");
    assert!(
        run.status.success(),
        "dylan-macro-walk.exe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");

    let mut got: BTreeMap<String, String> = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("WALK ") {
            let (name, text) = rest
                .split_once(" = ")
                .unwrap_or_else(|| panic!("malformed WALK line: {line:?}"));
            got.insert(name.trim().to_string(), normalize(text));
        }
    }

    for (name, expected) in EXPECTED {
        let actual = got
            .get(*name)
            .unwrap_or_else(|| panic!("walk driver produced no WALK {name}\n{stdout}"));
        assert_eq!(
            actual,
            &normalize(expected),
            "module-walk divergence for `{name}`",
        );
    }
}
