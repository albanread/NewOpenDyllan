//! Sprint 52.6 — whole-file expand-fidelity gate (locus B verify-mode,
//! test level).
//!
//! The Dylan front-end's locus-(B) job is to expand macro calls before
//! the AST reaches the host, producing the SAME kernel-shaped AST the host
//! lowers today after the Rust expander runs. This gate proves that at the
//! whole-file level, without yet rebuilding the production shim:
//!
//!   1. Run the Dylan file-expand driver on a fixture → expanded source.
//!   2. Parse that expanded source with the Rust parser → AST dump.
//!   3. Parse + expand the ORIGINAL fixture with Rust → AST dump.
//!   4. Assert the two dumps are equal, modulo the compile-time-only
//!      `(DefineMacro …)` items (locus-B output is macro-free; the Rust
//!      oracle keeps the defs but lowering ignores them).
//!
//! Run with:
//!   cargo test -p nod-tests --test macro_file_expand -- --nocapture

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serial_test::serial;

/// Self-contained macro fixtures (define AND use their own macro).
const FIXTURES: &[&str] = &["macros-unless.dylan", "macro-for-range.dylan"];

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    workspace_root().join("tests").join("nod-tests").join("fixtures")
}

/// Normalise an AST dump for comparison: drop `(DefineMacro …)` lines
/// (compile-time only, absent from locus-B output) and the `(Header …)`
/// block (module metadata the host carries separately; the Dylan expander
/// strips the preamble), then collapse each remaining line's whitespace.
fn normalize_ast_dump(dump: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut header_depth: i32 = 0; // >0 while inside a `(Header …)` subtree
    for raw in dump.replace("\r\n", "\n").lines() {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        if header_depth > 0 {
            header_depth += line.matches('(').count() as i32;
            header_depth -= line.matches(')').count() as i32;
            continue;
        }
        if line == "(Header" {
            header_depth = 1;
            continue;
        }
        if line.starts_with("(DefineMacro") {
            continue;
        }
        out.push(line);
    }
    out
}

/// Parse a (already macro-free) source with the Rust parser and format its
/// AST — the host's view of the Dylan-expanded source.
fn rust_parse_dump(src: &str) -> String {
    let mut sm = nod_reader::SourceMap::new();
    let file_id = sm
        .add(PathBuf::from("expanded.dylan"), src.to_string())
        .expect("source map add");
    let toks = nod_reader::lex_rust(src, file_id);
    let pre = nod_reader::scan_preamble(src);
    let module = nod_reader::parse_module_with_macros_rust(src, &toks, pre.as_ref(), &HashSet::new())
        .unwrap_or_else(|d| panic!("rust parse of dylan-expanded source failed: {d:?}\n{src}"));
    nod_reader::format_ast_module(&module)
}

fn build_file_expand_exe() -> PathBuf {
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
    let prj = fixtures_dir().join("dylan-macro-file.prj");
    let exe = fixtures_dir().join("dylan-macro-file.exe");
    let aot = Command::new(workspace.join("target").join("debug").join("nod-driver.exe"))
        .args(["build", "--project"])
        .arg(&prj)
        .output()
        .expect("spawn nod-driver build");
    assert!(
        aot.status.success(),
        "nod-driver build of dylan-macro-file.prj failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&aot.stdout),
        String::from_utf8_lossy(&aot.stderr),
    );
    assert!(exe.is_file(), "expected {} after build", exe.display());
    exe
}

fn dylan_expand_file(exe: &Path, fixture: &Path) -> String {
    let run = Command::new(exe)
        .arg(fixture)
        .output()
        .expect("spawn file-expand exe");
    assert!(
        run.status.success(),
        "dylan-macro-file.exe failed on {}:\nstdout:\n{}\nstderr:\n{}",
        fixture.display(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n")
}

#[test]
#[serial]
fn dylan_whole_file_expansion_matches_rust_ast() {
    let exe = build_file_expand_exe();

    for fixture in FIXTURES {
        let path = fixtures_dir().join(fixture);

        // Dylan side: expand the whole file → expanded source → Rust AST.
        let dylan_expanded = dylan_expand_file(&exe, &path);
        let dylan_ast = normalize_ast_dump(&rust_parse_dump(&dylan_expanded));

        // Rust side: parse + expand the original → AST.
        let rust_dump = nod_sema::dump_expanded_for_file(&path)
            .unwrap_or_else(|e| panic!("rust dump_expanded_for_file({fixture}) failed: {e:?}"));
        let rust_ast = normalize_ast_dump(&rust_dump);

        assert_eq!(
            dylan_ast, rust_ast,
            "whole-file expansion AST divergence for {fixture}:\n\
             --- dylan-expanded source ---\n{dylan_expanded}\n\
             --- dylan AST (macro-free) ---\n{}\n\
             --- rust AST (macro-free) ---\n{}",
            dylan_ast.join("\n"),
            rust_ast.join("\n"),
        );
    }
}
