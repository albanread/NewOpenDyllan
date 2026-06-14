//! Sprint 52.2 — macro-collection parity gate.
//!
//! Builds the Dylan-side macro-collection driver
//! (`dylan-macro-collect.prj` = dylan-lexer + dylan-macro + the collect
//! driver) and asserts that `collect-macro-defs` — the Dylan port of the
//! `define macro … end macro` extractor — finds the SAME macro
//! definitions (by name and by rule count) that the Rust front-end's
//! `nod_reader` parser + `nod_macro::collect_macros` find, over the same
//! source.
//!
//! This is the 52.2 gate: the Dylan engine's data model + `define macro`
//! body parse is promoted into the production front-end source
//! (`dylan-macro.dylan`), and its collection over `stdlib.dylan` + the
//! macro fixtures matches Rust's `collect_macros` count.
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_collect -- --nocapture

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

/// Rust ground truth: parse `path` with the canonical Rust front-end and
/// run `nod_macro::collect_macros`, returning a `name -> rule_count` map.
fn rust_collect(path: &Path) -> BTreeMap<String, usize> {
    // `stdlib_macro_names()` (called below for the parse seed) triggers
    // `stdlib::ensure_loaded()` itself, so no explicit load is needed.
    let src = std::fs::read_to_string(path).expect("read source");
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm.add(path.to_path_buf(), src.clone()).expect("source map add");
    let toks = nod_reader::lex(&src, file_id);
    let pre = nod_reader::scan_preamble(&src);
    // Seed with the stdlib's macro names so call sites parse exactly as
    // the real pipeline parses them. Definition capture is structural and
    // independent of the seed, but seeding keeps parsing of macro *uses*
    // (e.g. `unless (c) … end` in a fixture) on the real-pipeline path.
    let seed: HashSet<String> = nod_sema::stdlib_macro_names();
    let module = nod_reader::parse_module_with_macros_rust(&src, &toks, pre.as_ref(), &seed)
        .unwrap_or_else(|d| panic!("rust parse of {} failed: {:?}", path.display(), d));
    let mut table = nod_macro::MacroTable::default();
    nod_macro::collect_macros(&module, &sm, &mut table)
        .unwrap_or_else(|e| panic!("rust collect_macros on {} failed: {:?}", path.display(), e));
    table
        .defs
        .values()
        .map(|d| (d.name.clone(), d.rules.len()))
        .collect()
}

/// Dylan side: run the built `dylan-macro-collect.exe` on `path`, parse
/// its `COLLECTED n` / `MACRO name rules=k` report into a
/// `name -> rule_count` map. Asserts `COLLECTED` agrees with the number
/// of `MACRO` lines (no silent dupes).
fn dylan_collect(exe: &Path, path: &Path) -> BTreeMap<String, usize> {
    let run = Command::new(exe)
        .arg(path)
        .output()
        .expect("spawn dylan-macro-collect exe");
    assert!(
        run.status.success(),
        "dylan-macro-collect.exe failed on {}:\nstdout:\n{}\nstderr:\n{}",
        path.display(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");

    let mut collected: Option<usize> = None;
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("COLLECTED ") {
            collected = Some(rest.trim().parse().expect("COLLECTED count"));
        } else if let Some(rest) = line.strip_prefix("MACRO ") {
            // `MACRO <name> rules=<k>`
            let (name, k) = rest.split_once(" rules=").unwrap_or_else(|| {
                panic!("malformed MACRO line: {line:?} (full output:\n{stdout})")
            });
            let count: usize = k.trim().parse().expect("rule count");
            map.insert(name.to_string(), count);
        }
    }
    let collected = collected.unwrap_or_else(|| panic!("no COLLECTED line:\n{stdout}"));
    assert_eq!(
        collected,
        map.len(),
        "COLLECTED {collected} disagrees with {} MACRO lines for {}:\n{stdout}",
        map.len(),
        path.display(),
    );
    map
}

/// Build `nod-driver`, then AOT-build the collect driver via its `.prj`.
/// Returns the path to the produced exe.
fn build_collect_exe() -> PathBuf {
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

    let prj = fixtures_dir().join("dylan-macro-collect.prj");
    let exe = fixtures_dir().join("dylan-macro-collect.exe");
    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build of dylan-macro-collect.prj failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );
    assert!(exe.is_file(), "expected {} to exist after build", exe.display());
    exe
}

#[test]
#[serial]
fn dylan_macro_collect_matches_rust() {
    let exe = build_collect_exe();

    // The corpus the 52.2 gate exercises: the stdlib (5 macros, `cond`
    // with 4 rules) plus the macro fixtures. `macro-when-only` and
    // `cond_smoke` are *call sites*, not definitions → 0 macros, which
    // both engines must agree on.
    let stdlib = workspace_root()
        .join("src")
        .join("nod-dylan")
        .join("dylan-sources")
        .join("stdlib.dylan");
    let corpus = [
        stdlib.clone(),
        fixtures_dir().join("macros-unless.dylan"),
        fixtures_dir().join("macro-for-range.dylan"),
        fixtures_dir().join("macro-when-only.dylan"),
        fixtures_dir().join("cond_smoke.dylan"),
    ];

    for path in &corpus {
        let rust = rust_collect(path);
        let dylan = dylan_collect(&exe, path);
        assert_eq!(
            dylan,
            rust,
            "macro-collection parity divergence for {}:\n  rust  = {:?}\n  dylan = {:?}",
            path.display(),
            rust,
            dylan,
        );
    }

    // Spot-anchor the headline expectation so a regression that makes
    // BOTH sides wrong the same way still trips the gate.
    let stdlib_dylan = dylan_collect(&exe, &stdlib);
    assert_eq!(stdlib_dylan.get("cond"), Some(&4), "cond should have 4 rules");
    assert_eq!(stdlib_dylan.len(), 5, "stdlib should define exactly 5 macros");
    let names: HashSet<&str> = stdlib_dylan.keys().map(|s| s.as_str()).collect();
    for expected in ["for-each", "unless", "when", "cond", "with-cleanup"] {
        assert!(names.contains(expected), "stdlib missing macro {expected}");
    }
}
