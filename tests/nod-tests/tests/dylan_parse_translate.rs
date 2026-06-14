//! Sprint 51e — Dylan-parser *translation* gate + coverage dashboard.
//!
//! Sibling of `dylan_parse_coverage.rs`. Where that harness measures how
//! many AST *nodes* the Dylan emitter structures, this one measures the
//! real payoff: how many whole fixtures the `dylan_to_ast` translator
//! turns into an `ast::Module` that is **byte-identical** to the Rust
//! parser's — i.e. how many files `--parse-with-dylan` can authoritatively
//! handle, with the rest falling back to the Rust parser.
//!
//! For each fixture it runs the driver twice:
//!   * `nod-driver dump-ast FILE`                 — the Rust parser (oracle)
//!   * `nod-driver --parse-with-dylan dump-ast FILE` — Dylan-or-fallback
//!
//! The hard gate: **their stdout is byte-identical for every fixture.**
//! That holds whether the file was translated (the two parsers agree) or
//! fell back (the Dylan path literally re-runs the Rust parser). A
//! divergence — the Dylan translator emitting an AST that differs from
//! the Rust parser's — fails the test loudly. The stderr note
//! (`parse-with-dylan: translated|fell back …`) is tallied into a
//! report and we assert the count of translated files only grows
//! (currently: at least `hello.dylan`).
//!
//! Run with:
//!   cargo test -p nod-tests --test dylan_parse_translate -- --nocapture

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn driver_exe() -> PathBuf {
    workspace_root().join("target").join("debug").join("nod-driver.exe")
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

/// The relocated Dylan front-end sources (Sprint 56 reorg). Swept
/// alongside `fixtures/` so the parser keeps dogfooding its own source.
fn compiler_dir() -> PathBuf {
    workspace_root().join("compiler")
}

fn ensure_driver_built() {
    let build = Command::new("cargo")
        .current_dir(workspace_root())
        .args(["build", "-p", "nod-driver"])
        .output()
        .expect("spawn cargo build");
    assert!(
        build.status.success(),
        "cargo build -p nod-driver failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

/// Same sweep rule as the coverage harness: skip scratch files, IR
/// dumps, and the shim/entry bundle inputs.
fn is_sweepable(name: &str) -> bool {
    if name.starts_with("_tmp") {
        return false;
    }
    if name.ends_with("-ir.dylan") {
        return false;
    }
    name.ends_with(".dylan") && !matches!(name, "dylan-lex-shim.dylan" | "dylan-lexer-main.dylan")
}

#[test]
fn dylan_parser_translation_gate() {
    ensure_driver_built();
    let driver = driver_exe();
    let mut entries: Vec<PathBuf> = Vec::new();
    for dir in [fixtures_dir(), compiler_dir()] {
        let rd = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("read sweep dir {}: {err}", dir.display()));
        for e in rd {
            let p = e.expect("dir entry").path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .map(is_sweepable)
                .unwrap_or(false)
            {
                entries.push(p);
            }
        }
    }
    entries.sort();

    let mut translated: Vec<String> = Vec::new();
    let mut fell_back: Vec<(String, String)> = Vec::new(); // (name, reason)
    let mut divergences: Vec<String> = Vec::new();
    let mut skipped_shim = false;

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let rust = Command::new(&driver)
            .arg("dump-ast")
            .arg(path)
            .output()
            .expect("spawn dump-ast");
        let dylan = Command::new(&driver)
            .arg("--parse-with-dylan")
            .arg("dump-ast")
            .arg(path)
            .output()
            .expect("spawn --parse-with-dylan dump-ast");

        // If the shim isn't statically linked, the Dylan path can't run
        // at all — every file "falls back (shim init …)". Detect that
        // once and don't treat it as a divergence: the report-only
        // coverage harness has the same guard.
        let dyl_err = String::from_utf8_lossy(&dylan.stderr);
        if dyl_err.contains("shim init") {
            skipped_shim = true;
        }

        // The hard gate: identical stdout. Holds for both translated
        // (parsers agree) and fell-back (same Rust parser re-run) files.
        if rust.stdout != dylan.stdout {
            divergences.push(name.clone());
            eprintln!("\n=== DIVERGENCE on {name} ===");
            eprintln!("--- rust dump-ast ---\n{}", String::from_utf8_lossy(&rust.stdout));
            eprintln!(
                "--- --parse-with-dylan dump-ast ---\n{}",
                String::from_utf8_lossy(&dylan.stdout)
            );
            continue;
        }

        // Tally which path the Dylan run took, from its stderr note.
        if let Some(line) = dyl_err.lines().find(|l| l.contains("parse-with-dylan:")) {
            if line.contains("translated") {
                translated.push(name.clone());
            } else if let Some(idx) = line.find("fell back") {
                // reason is in parens right after "fell back "
                let reason = line[idx..]
                    .trim_start_matches("fell back")
                    .trim()
                    .trim_start_matches('(')
                    .split(')')
                    .next()
                    .unwrap_or("")
                    .to_string();
                fell_back.push((name.clone(), reason));
            }
        }
    }

    // ─── Report ──────────────────────────────────────────────────────
    eprintln!("\n=== Dylan parser translation coverage ===\n");
    eprintln!(
        "Translated by the Dylan parser (byte-identical to Rust): {}/{}",
        translated.len(),
        entries.len()
    );
    for t in &translated {
        eprintln!("  ✓  {t}");
    }
    eprintln!("\nFell back to the Rust parser ({}):", fell_back.len());
    // Group fall-back reasons by frequency — this is the punch-list for
    // the next translator increment (which kind to teach it next).
    let mut reason_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for (_, r) in &fell_back {
        *reason_counts.entry(r.clone()).or_insert(0) += 1;
    }
    let mut ranked: Vec<(&String, &usize)> = reason_counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (reason, count) in ranked {
        eprintln!("  {count:>3}  {reason}");
    }
    eprintln!();

    if skipped_shim {
        eprintln!(
            "NOTE: dylan-lex-shim.lib.obj is not statically linked — every file \
             fell back via shim-init. Build it (see docs/ARCHITECTURE.md) and \
             rebuild nod-driver to exercise the Dylan translation path. The \
             byte-identical gate still holds (fallback == Rust parser)."
        );
        // Can't assert translation happened without the shim; gate on
        // the (trivially-true) divergence check only.
        assert!(divergences.is_empty(), "stdout divergences: {divergences:?}");
        return;
    }

    // Hard gates.
    assert!(
        divergences.is_empty(),
        "the Dylan translator produced an AST that differs from the Rust parser on: {divergences:?}"
    );
    assert!(
        translated.iter().any(|n| n == "hello.dylan"),
        "expected at least hello.dylan to translate via the Dylan path; translated = {translated:?}"
    );
}
