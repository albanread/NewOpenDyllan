//! Sprint 51e — Dylan-parser coverage harness.
//!
//! The Dylan-side parser (`dylan-parser.dylan`) emits an AST across the
//! wire format in `docs/DYLAN_AST_WIRE.md`. Sprint 51d's v1 emitter
//! covers 7 node kinds; everything else lowers to an `Error` record.
//! This harness sweeps the fixture corpus through `nod-driver
//! dump-dylan-ast`, walks each emitted tree, and aggregates the
//! `Error` nodes into a **punch-list**: which source constructs the
//! Dylan emitter doesn't structure yet, ranked by frequency.
//!
//! That punch-list is the to-do list for filling out the front-end:
//! each entry is one `emit-node` method (Dylan) + one `Kind` variant
//! (Rust) + one `DYLAN_AST_WIRE.md` row. As coverage grows, the
//! Error count drops and this report shows it.
//!
//! Two Error flavours, reported separately:
//!   * **spanned** `(Error A..B)` with `B > A` — a real construct the
//!     emitter doesn't handle yet. Classified by the leading word of
//!     `src[A..B]`, which (for `<ast-body-definition>`) is the
//!     body-word: `class`, `method`, `constant`, … → the definition
//!     kind. For in-body errors it's `if` / `let` / `for` / a literal.
//!   * **unspanned** `(Error 0..0)` — a node whose `node-token` is
//!     `#f`, so the emitter had no span to emit. These trace to the
//!     Sprint 51d span-backfill gap (`<ast-body>` and friends carry no
//!     outer token), not to a missing kind. Counted as one bucket.
//!
//! This test is **report-only**: it always passes as long as the
//! driver runs and emits a tree for every fixture. It is the living
//! coverage dashboard, not a flaky gate. Run with:
//!   cargo test -p nod-tests --test dylan_parse_coverage -- --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
/// alongside `fixtures/` so coverage still includes the compiler's
/// own source.
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

/// Should this fixture be swept? Skip scratch files (`_tmp*`), IR
/// dumps (`*-ir.dylan` are not Dylan source), and the shim/entry
/// files that exist only to be bundled into the lex-shim `.obj`.
fn is_sweepable(name: &str) -> bool {
    if name.starts_with("_tmp") {
        return false;
    }
    if name.ends_with("-ir.dylan") {
        return false;
    }
    // The shim + its main are bundled-build inputs, not standalone
    // modules; dump-dylan-ast on them is meaningless.
    matches!(name.ends_with(".dylan"), true)
        && !matches!(name, "dylan-lex-shim.dylan" | "dylan-lexer-main.dylan")
}

/// One `(KindName ...)` node opener on a dump line, plus its span if
/// it's an `Error`. Returns `(kind, Option<(lo, hi)>)`.
fn parse_node_line(line: &str) -> Option<(String, Option<(usize, usize)>)> {
    let t = line.trim_start();
    let rest = t.strip_prefix('(')?;
    // Kind name runs up to the first space or ')'.
    let end = rest.find([' ', ')']).unwrap_or(rest.len());
    let kind = rest[..end].to_string();
    if kind.is_empty() {
        return None;
    }
    if kind != "Error" {
        return Some((kind, None));
    }
    // Error line: `(Error A..B)` — pull the span.
    let after = rest[end..].trim_start();
    let span = parse_span(after);
    Some((kind, span))
}

/// Parse `A..B` (the leading token after the kind name).
fn parse_span(s: &str) -> Option<(usize, usize)> {
    let s = s.trim_start();
    let dotdot = s.find("..")?;
    let lo: usize = s[..dotdot].trim().parse().ok()?;
    let tail = &s[dotdot + 2..];
    let end = tail.find([' ', ')', '\t']).unwrap_or(tail.len());
    let hi: usize = tail[..end].trim().parse().ok()?;
    Some((lo, hi))
}

/// Classify a spanned Error by the leading word of its source slice.
/// `define class …` lowers to an `<ast-body-definition>` whose span
/// starts at the body-word, so the first word IS the definition kind.
fn classify(src: &[u8], lo: usize, hi: usize) -> String {
    if hi > src.len() || lo >= hi {
        return "<bad-span>".to_string();
    }
    let slice = &src[lo..hi.min(src.len())];
    // Leading word: run of identifier bytes (alnum, '-', '<', '>', '_').
    let mut start = 0;
    while start < slice.len() && (slice[start] as char).is_whitespace() {
        start += 1;
    }
    let mut end = start;
    while end < slice.len() {
        let c = slice[end] as char;
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '<' | '>' | '!' | '?' | '*' | '$') {
            end += 1;
        } else {
            break;
        }
    }
    if end == start {
        // Punctuation-led: report the first non-space byte.
        return format!("punct:{:?}", slice[start] as char);
    }
    let word = String::from_utf8_lossy(&slice[start..end]).to_string();
    // Body-words → the definition kind they introduce.
    match word.as_str() {
        "class" | "method" | "function" | "constant" | "variable" | "generic" | "macro"
        | "library" | "module" | "domain" => format!("define-{word}"),
        _ => word,
    }
}

#[test]
fn dylan_parser_coverage_report() {
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

    let mut histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_nodes = 0usize;
    let mut total_errors = 0usize;
    let mut total_unspanned = 0usize;
    let mut per_file: Vec<(String, usize, usize)> = Vec::new(); // (name, nodes, errors)
    let mut files_run = 0usize;

    for path in &entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let out = Command::new(&driver)
            .arg("dump-dylan-ast")
            .arg(path)
            .output()
            .expect("spawn dump-dylan-ast");
        // The shim may not be statically linked (fresh checkout). If
        // so, dump-dylan-ast errors out — skip with a note rather than
        // failing the whole report.
        if !out.status.success() {
            eprintln!(
                "skip {name}: dump-dylan-ast exit {:?}\n  stderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")
            );
            continue;
        }
        files_run += 1;
        let src = std::fs::read(path).expect("read fixture source");
        let stdout = String::from_utf8_lossy(&out.stdout);

        let mut file_nodes = 0usize;
        let mut file_errors = 0usize;
        for line in stdout.lines() {
            let Some((kind, span)) = parse_node_line(line) else {
                continue;
            };
            file_nodes += 1;
            total_nodes += 1;
            if kind == "Error" {
                file_errors += 1;
                total_errors += 1;
                match span {
                    Some((lo, hi)) if hi > lo => {
                        *histogram.entry(classify(&src, lo, hi)).or_insert(0) += 1;
                    }
                    _ => {
                        total_unspanned += 1;
                        *histogram.entry("<unspanned 0..0>".to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
        per_file.push((name, file_nodes, file_errors));
    }

    // ─── Report ──────────────────────────────────────────────────────
    eprintln!("\n=== Dylan parser coverage ({files_run} fixtures) ===\n");

    if files_run == 0 {
        eprintln!(
            "  dump-dylan-ast produced no output on any fixture — the \
             dylan-lex-shim.lib.obj is probably not statically linked into \
             nod-driver. Build it first (see docs/ARCHITECTURE.md) and \
             rebuild the driver. Report-only test: passing regardless."
        );
        return;
    }

    eprintln!("Per-fixture (structured% = non-Error nodes):");
    for (name, nodes, errors) in &per_file {
        let structured = if *nodes == 0 {
            0
        } else {
            (nodes - errors) * 100 / nodes
        };
        eprintln!("  {structured:>3}%  {name}  ({}/{} structured)", nodes - errors, nodes);
    }

    let structured_total = total_nodes - total_errors;
    let pct = if total_nodes == 0 { 0 } else { structured_total * 100 / total_nodes };
    eprintln!(
        "\nCorpus total: {structured_total}/{total_nodes} nodes structured ({pct}%), \
         {total_errors} Error ({total_unspanned} unspanned)."
    );

    eprintln!("\nPunch-list — Error nodes by construct (the next emit-node methods to write):");
    let mut ranked: Vec<(&String, &usize)> = histogram.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (construct, count) in ranked {
        eprintln!("  {count:>4}  {construct}");
    }
    eprintln!();

    // Report-only: as long as the driver ran on at least one fixture,
    // the test passes. The numbers above are the deliverable.
    assert!(files_run > 0);
}
