//! Sprint 45 — Dylan lexer tests.
//!
//! 45a covered the dump-infrastructure acceptance; 45b fills out
//! per-token-kind tests against the real `lex` implementation.
//! The 45d oracle test against `nod-reader::lex` and the 45e IDE
//! wiring are still pending.
//!
//! The Dylan-in-Dylan lexer source lives at
//! `tests/nod-tests/fixtures/dylan-lexer.dylan`. The driver subcommand
//! `nod-driver dump-dylan-tokens <path>` AOT-compiles the lexer source
//! once into a cached EXE under the OS tempdir, then runs the EXE with
//! `<path>` as argv[1] and forwards stdout. We shell out to that
//! subcommand from each test below.
//!
//! Each test is `#[ignore]` + `serial_test::serial` because the AOT
//! pipeline shells out to `cargo run --bin nod-driver` plus MSVC's
//! `link.exe`, and concurrent invocations would stall on Cargo's
//! build-system lock.
//!
//! Note: GAP-007 (phi-wiring / stale-pointer) and the full cascade
//! through GAP-013 are fixed as of Sprint 45c–45h. The `*tokens*` /
//! `*dump-stream*` module-variable workaround in the lexer fixture is
//! still in tree pending a retirement commit; the acceptance gate is
//! `nod-driver dump-dylan-tokens` on the lexer's own source completing
//! without error. Tests below use short snippets for speed, not because
//! of any remaining root-tracking limitation.
//!
//! Run with:
//!
//! ```text
//! cargo test --test dylan_lexer -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nod_tests::test_support::run_command_with_watchdog;
use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Mirrors the
/// helper in `aot_dylan.rs`.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Path inside the per-test scratch dir for the snippet we'll lex.
/// All snippet files go under
/// `<target>/dylan-lexer-test-snippets/<name>.dylan`.
fn snippet_dir() -> PathBuf {
    let workspace = workspace_root();
    let dir = workspace.join("target").join("dylan-lexer-test-snippets");
    std::fs::create_dir_all(&dir).expect("create snippet dir");
    dir
}

fn gc_stats_dir() -> PathBuf {
    let workspace = workspace_root();
    let dir = workspace.join("target").join("gc-stats");
    std::fs::create_dir_all(&dir).expect("create gc stats dir");
    dir
}

/// Write a snippet to a temp file and return its absolute path.
fn write_snippet(name: &str, contents: &str) -> PathBuf {
    let path = snippet_dir().join(format!("{name}.dylan"));
    std::fs::write(&path, contents).expect("write snippet");
    path
}

/// Pre-build `nod-driver` + `nod-runtime` once per test to avoid
/// races against Cargo's build lock when the lexer EXE is built.
fn prebuild_driver(workspace: &Path) {
    let mut build = Command::new("cargo");
    build
        .current_dir(workspace)
        .args(["build", "-p", "nod-driver", "-p", "nod-runtime"]);
    let build = run_command_with_watchdog(
        "dylan_lexer",
        "cargo-build",
        Duration::from_secs(300),
        &mut build,
    );
    assert!(
        build.status.success(),
        "cargo build failed: {}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
        build.status,
        build.stderr,
        build.stdout_path.display(),
        build.stderr_path.display(),
        build.meta_path.display()
    );
}

fn driver_exe(workspace: &Path) -> PathBuf {
    workspace.join("target").join("debug").join("nod-driver.exe")
}

/// Lex one snippet through the cached lexer EXE; return stdout as
/// a UTF-8 String.
fn lex_snippet(snippet: &Path) -> String {
    let workspace = workspace_root();
    prebuild_driver(&workspace);
    let mut driver = Command::new("cargo");
    driver
        .current_dir(&workspace)
        .args([
            "run",
            "--quiet",
            "--bin",
            "nod-driver",
            "--",
            "dump-dylan-tokens",
            snippet.to_str().unwrap(),
        ]);
    let driver = run_command_with_watchdog(
        "dylan_lexer",
        "dump-dylan-tokens",
        Duration::from_secs(180),
        &mut driver,
    );
    let stdout = driver.stdout.clone();
    let stderr = driver.stderr.clone();
    assert!(
        driver.status.success(),
        "dump-dylan-tokens on {} failed: {}\nstdout:\n{}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
        snippet.display(),
        driver.status,
        stdout,
        stderr,
        driver.stdout_path.display(),
        driver.stderr_path.display(),
        driver.meta_path.display()
    );
    stdout
}

fn lex_snippet_with_gc_stats(snippet: &Path, stats_file_name: &str) -> (String, String) {
    let workspace = workspace_root();
    prebuild_driver(&workspace);
    let driver = driver_exe(&workspace);
    assert!(driver.is_file(), "driver exe missing: {}", driver.display());
    let mut out = Command::new(&driver);
    out.args([
            "dump-dylan-tokens",
            snippet.to_str().unwrap(),
            "--gc-stats",
        ]);
    let out = run_command_with_watchdog(
        "dylan_lexer",
        stats_file_name,
        Duration::from_secs(180),
        &mut out,
    );
    let stdout = out.stdout.clone();
    let stderr = out.stderr.clone();
    std::fs::write(gc_stats_dir().join(stats_file_name), &stderr)
        .expect("write lexer gc stats report");
    assert!(
        out.status.success(),
        "dump-dylan-tokens --gc-stats on {} failed: {}\nstdout:\n{}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
        snippet.display(),
        out.status,
        stdout,
        stderr,
        out.stdout_path.display(),
        out.stderr_path.display(),
        out.meta_path.display()
    );
    (stdout, stderr)
}

fn parse_gc_counter(report: &str, label: &str) -> u64 {
    report
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let (lhs, rhs) = trimmed.split_once(':')?;
            if lhs.trim() != label {
                return None;
            }
            rhs.split_whitespace().next()?.parse::<u64>().ok()
        })
        .unwrap_or_else(|| panic!("missing GC counter `{label}` in report:\n{report}"))
}

/// Build assertion helper: lex a snippet and assert the dump contains
/// the expected substring (kind + text). The snippet is written to a
/// uniquely named file derived from `name` to avoid races between
/// tests sharing the snippet directory.
fn assert_dump_contains(name: &str, source: &str, expected_lines: &[&str]) {
    let snippet = write_snippet(name, source);
    let stdout = lex_snippet(&snippet);
    for expected in expected_lines {
        assert!(
            stdout.contains(expected),
            "test {name}: expected substring {expected:?}\ndump:\n{stdout}"
        );
    }
}

/// Build assertion helper: every line of the dump must be NON-ERROR.
fn assert_no_errors(name: &str, source: &str) {
    let snippet = write_snippet(name, source);
    let stdout = lex_snippet(&snippet);
    let errors: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("ERROR"))
        .collect();
    assert!(
        errors.is_empty(),
        "test {name}: unexpected error tokens:\n{}\nfull dump:\n{stdout}",
        errors.join("\n")
    );
}

// ─── headline: hello.dylan round-trip ─────────────────────────────────────

/// Sprint 45b headline acceptance — the existing Sprint 45a fixture
/// `hello.dylan` lexes cleanly. The old 45a stub returned just
/// `1:1-1:1  EOF\n`; the real lex produces 46 token lines covering
/// the Module header, comment, define, function body, etc.
#[test]
#[ignore]
#[serial]
fn dump_dylan_tokens_for_hello_produces_full_token_stream() {
    let workspace = workspace_root();
    let hello = workspace.join("tests/nod-tests/fixtures/hello.dylan");
    assert!(hello.is_file(), "hello.dylan fixture missing");
    let stdout = lex_snippet(&hello);
    // Must have many lines (real lex output, not the 45a stub).
    let line_count = stdout.lines().count();
    assert!(
        line_count > 20,
        "expected many tokens for hello.dylan, got {line_count}:\n{stdout}"
    );
    // Must end in EOF (the real lex always appends one).
    let last = stdout.lines().last().unwrap_or("");
    assert!(
        last.contains("EOF"),
        "expected final line to be EOF, got {last:?}\nfull dump:\n{stdout}"
    );
    // No error tokens on real Dylan.
    let errors: Vec<&str> = stdout.lines().filter(|l| l.contains("ERROR")).collect();
    assert!(
        errors.is_empty(),
        "hello.dylan should lex without errors; got:\n{}\nfull dump:\n{stdout}",
        errors.join("\n")
    );
    // Spot-check several specific tokens from hello.dylan's known shape.
    for expected in [
        "KEYWORD_NAME  Module:",
        "IDENTIFIER  hello",
        "COMMENT_LINE",
        "KEYWORD  define",
        "KEYWORD  function",
        "IDENTIFIER  main",
        "IDENTIFIER  format-out",
        "STRING",
        "KEYWORD  end",
        "PUNCT  (",
        "PUNCT  )",
        "PUNCT  ;",
        "PUNCT  =>",
    ] {
        assert!(
            stdout.contains(expected),
            "hello.dylan dump missing {expected:?}\nfull dump:\n{stdout}"
        );
    }
}

#[test]
#[ignore]
#[serial]
fn dump_dylan_tokens_gc_stats_for_repeated_hello_workload() {
    let hello = std::fs::read_to_string(
        workspace_root().join("tests/nod-tests/fixtures/hello.dylan"),
    )
    .expect("read hello.dylan");
    let source = (0..32)
        .map(|_| hello.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let snippet = write_snippet("hello-gc-workload", &source);
    let (_stdout, stderr) = lex_snippet_with_gc_stats(
        &snippet,
        "dylan-lexer-repeated-hello.stats.txt",
    );
    assert!(stderr.contains("GC stats (backend ="), "missing GC stats:\n{stderr}");
    let minor = parse_gc_counter(&stderr, "minor collections");
    let major = parse_gc_counter(&stderr, "major collections");
    assert!(minor > 0 || major > 0, "GC was not exercised:\n{stderr}");
}

// ─── integer literals ─────────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn integer_decimal_zero() {
    assert_dump_contains("int_decimal_zero", "0", &["1:1-1:2  INTEGER  0"]);
}

#[test]
#[ignore]
#[serial]
fn integer_decimal_multi_digit() {
    assert_dump_contains("int_decimal_multi", "12345", &["1:1-1:6  INTEGER  12345"]);
}

#[test]
#[ignore]
#[serial]
fn integer_hex_lowercase() {
    assert_dump_contains("int_hex_lower", "#xff", &["1:1-1:5  INTEGER  #xff"]);
}

#[test]
#[ignore]
#[serial]
fn integer_hex_uppercase() {
    assert_dump_contains("int_hex_upper", "#xDEADBEEF", &["1:1-1:11  INTEGER  #xDEADBEEF"]);
}

#[test]
#[ignore]
#[serial]
fn integer_binary() {
    assert_dump_contains("int_binary", "#b1010", &["1:1-1:7  INTEGER  #b1010"]);
}

#[test]
#[ignore]
#[serial]
fn integer_octal() {
    assert_dump_contains("int_octal", "#o755", &["1:1-1:6  INTEGER  #o755"]);
}

// ─── string literals + escapes ────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn string_simple() {
    // The dump escapes ' ' as '\s' and '"' as '\"'.
    assert_dump_contains(
        "string_simple",
        "\"hello\"",
        &["1:1-1:8  STRING  \\\"hello\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn string_with_spaces() {
    assert_dump_contains(
        "string_spaces",
        "\"a b c\"",
        &["1:1-1:8  STRING  \\\"a\\sb\\sc\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn string_escape_newline() {
    // Source contains a literal backslash followed by 'n'. The dump
    // shows the raw source bytes, which include the backslash escaped
    // as `\\` and the 'n' as itself: the raw text is `"\n"` which in
    // the dump becomes `\"\\n\"`.
    assert_dump_contains(
        "string_esc_n",
        "\"\\n\"",
        &["1:1-1:5  STRING  \\\"\\\\n\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn string_escape_tab() {
    assert_dump_contains(
        "string_esc_t",
        "\"\\t\"",
        &["1:1-1:5  STRING  \\\"\\\\t\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn string_escape_backslash() {
    assert_dump_contains(
        "string_esc_backslash",
        "\"\\\\\"",
        &["1:1-1:5  STRING  \\\"\\\\\\\\\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn string_escape_quote() {
    // Source is `"\""` — six bytes? no, four: ", \, ", ".
    assert_dump_contains(
        "string_esc_quote",
        "\"\\\"\"",
        &["1:1-1:5  STRING  \\\"\\\\\\\"\\\""],
    );
}

// ─── identifiers vs keywords ──────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn identifier_simple() {
    assert_dump_contains("ident_simple", "foo", &["1:1-1:4  IDENTIFIER  foo"]);
}

#[test]
#[ignore]
#[serial]
fn identifier_with_dash() {
    assert_dump_contains(
        "ident_dash",
        "format-out",
        &["1:1-1:11  IDENTIFIER  format-out"],
    );
}

#[test]
#[ignore]
#[serial]
fn identifier_with_question_mark() {
    assert_dump_contains(
        "ident_qmark",
        "empty?",
        &["1:1-1:7  IDENTIFIER  empty?"],
    );
}

#[test]
#[ignore]
#[serial]
fn identifier_angle_brackets() {
    // Class-style names like `<integer>` are ordinary identifiers
    // from the lexer's POV — `<` and `>` are name-start / name-cont
    // bytes.
    assert_dump_contains(
        "ident_angle",
        "<integer>",
        &["1:1-1:10  IDENTIFIER  <integer>"],
    );
}

#[test]
#[ignore]
#[serial]
fn keyword_define() {
    assert_dump_contains("kw_define", "define", &["1:1-1:7  KEYWORD  define"]);
}

#[test]
#[ignore]
#[serial]
fn keyword_method() {
    assert_dump_contains("kw_method", "method", &["1:1-1:7  KEYWORD  method"]);
}

#[test]
#[ignore]
#[serial]
fn keyword_end() {
    assert_dump_contains("kw_end", "end", &["1:1-1:4  KEYWORD  end"]);
}

#[test]
#[ignore]
#[serial]
fn keyword_if_else_then() {
    let stdout = {
        let snippet = write_snippet("kw_if_else_then", "if else then");
        lex_snippet(&snippet)
    };
    for expected in [
        "1:1-1:3  KEYWORD  if",
        "1:4-1:8  KEYWORD  else",
        "1:9-1:13  KEYWORD  then",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?}\n{stdout}"
        );
    }
}

#[test]
#[ignore]
#[serial]
fn keyword_name_module() {
    // `Module:` is one keyword-name-token. The trailing colon is
    // folded into the same span.
    assert_dump_contains(
        "kw_name_module",
        "Module:",
        &["1:1-1:8  KEYWORD_NAME  Module:"],
    );
}

// ─── punctuation ──────────────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn punctuation_parens_brackets_braces() {
    let stdout = {
        let snippet = write_snippet("punct_brackets", "(){}[]");
        lex_snippet(&snippet)
    };
    for expected in [
        "1:1-1:2  PUNCT  (",
        "1:2-1:3  PUNCT  )",
        "1:3-1:4  PUNCT  {",
        "1:4-1:5  PUNCT  }",
        "1:5-1:6  PUNCT  [",
        "1:6-1:7  PUNCT  ]",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}\n{stdout}");
    }
}

#[test]
#[ignore]
#[serial]
fn punctuation_semi_comma_dot() {
    let stdout = {
        let snippet = write_snippet("punct_separators", ";,.");
        lex_snippet(&snippet)
    };
    for expected in [
        "1:1-1:2  PUNCT  ;",
        "1:2-1:3  PUNCT  ,",
        "1:3-1:4  PUNCT  .",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}\n{stdout}");
    }
}

#[test]
#[ignore]
#[serial]
fn punctuation_double_colon() {
    assert_dump_contains("punct_dcolon", "::", &["1:1-1:3  PUNCT  ::"]);
}

#[test]
#[ignore]
#[serial]
fn punctuation_assign() {
    assert_dump_contains("punct_assign", ":=", &["1:1-1:3  PUNCT  :="]);
}

#[test]
#[ignore]
#[serial]
fn punctuation_arrow() {
    assert_dump_contains("punct_arrow", "=>", &["1:1-1:3  PUNCT  =>"]);
}

#[test]
#[ignore]
#[serial]
fn punctuation_ellipsis() {
    assert_dump_contains("punct_ellipsis", "...", &["1:1-1:4  PUNCT  ..."]);
}

#[test]
#[ignore]
#[serial]
fn punctuation_minus_then_digit() {
    // §9 of SPRINT_45_DYLAN_LEXER.md: negative integers lex as two
    // tokens. `-7` → PUNCT `-` then INTEGER `7`.
    let stdout = {
        let snippet = write_snippet("punct_minus_digit", "-7");
        lex_snippet(&snippet)
    };
    assert!(
        stdout.contains("1:1-1:2  PUNCT  -"),
        "missing minus punct: {stdout}"
    );
    assert!(
        stdout.contains("1:2-1:3  INTEGER  7"),
        "missing integer 7: {stdout}"
    );
}

// ─── comments ─────────────────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn comment_line() {
    assert_dump_contains(
        "comment_line",
        "// hello",
        &["1:1-1:9  COMMENT_LINE  //\\shello"],
    );
}

#[test]
#[ignore]
#[serial]
fn comment_block_single_line() {
    assert_dump_contains(
        "comment_block_single",
        "/* x */",
        &["1:1-1:8  COMMENT_BLOCK  /*\\sx\\s*/"],
    );
}

#[test]
#[ignore]
#[serial]
fn comment_block_does_not_nest() {
    // §9: first `*/` closes the block. So `/* a /* b */ c */` is
    // `COMMENT_BLOCK /* a /* b */` then the rest is regular tokens.
    let stdout = {
        let snippet = write_snippet("comment_block_no_nest", "/* a /* b */ c */");
        lex_snippet(&snippet)
    };
    // The first `*/` at byte 11 closes the block; the comment is bytes 0..12.
    assert!(
        stdout.contains("COMMENT_BLOCK"),
        "expected COMMENT_BLOCK in: {stdout}"
    );
    // After the block close, we should see an identifier `c` and more punct.
    assert!(stdout.contains("IDENTIFIER  c"), "expected ident c: {stdout}");
}

// ─── whitespace ───────────────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn whitespace_run() {
    let stdout = {
        let snippet = write_snippet("ws_run", "a   b");
        lex_snippet(&snippet)
    };
    assert!(stdout.contains("1:1-1:2  IDENTIFIER  a"), "{stdout}");
    assert!(stdout.contains("1:2-1:5  WS  \\s\\s\\s"), "{stdout}");
    assert!(stdout.contains("1:5-1:6  IDENTIFIER  b"), "{stdout}");
}

#[test]
#[ignore]
#[serial]
fn whitespace_newline_advances_line() {
    let stdout = {
        let snippet = write_snippet("ws_newline", "a\nb");
        lex_snippet(&snippet)
    };
    assert!(stdout.contains("1:1-1:2  IDENTIFIER  a"), "{stdout}");
    // The newline run starts at 1:2 and ends at 2:1.
    assert!(stdout.contains("1:2-2:1  WS  \\n"), "{stdout}");
    assert!(stdout.contains("2:1-2:2  IDENTIFIER  b"), "{stdout}");
}

// ─── error / edge cases ───────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn error_unterminated_string() {
    let stdout = {
        let snippet = write_snippet("err_unterm_string", "\"abc");
        lex_snippet(&snippet)
    };
    assert!(
        stdout.contains("ERROR"),
        "expected ERROR token for unterminated string: {stdout}"
    );
}

#[test]
#[ignore]
#[serial]
fn error_stray_at_sign() {
    // '@' is not in any of the lexer's accept sets — it becomes an
    // ERROR token that consumes exactly one byte.
    let stdout = {
        let snippet = write_snippet("err_at_sign", "@");
        lex_snippet(&snippet)
    };
    assert!(
        stdout.contains("1:1-1:2  ERROR"),
        "expected one-byte ERROR for `@`: {stdout}"
    );
}

#[test]
#[ignore]
#[serial]
fn empty_input_produces_eof_only() {
    let stdout = {
        let snippet = write_snippet("edge_empty", "");
        lex_snippet(&snippet)
    };
    assert_eq!(stdout, "1:1-1:1  EOF\n", "empty input dump mismatch");
}

#[test]
#[ignore]
#[serial]
fn trailing_line_comment_then_eof() {
    let stdout = {
        let snippet = write_snippet("edge_trailing_comment", "// last comment");
        lex_snippet(&snippet)
    };
    assert!(
        stdout.contains("COMMENT_LINE"),
        "expected COMMENT_LINE: {stdout}"
    );
    // EOF must come immediately after the comment.
    let last = stdout.lines().last().unwrap_or("");
    assert!(last.contains("EOF"), "expected final EOF: {stdout}");
}

#[test]
#[ignore]
#[serial]
fn boolean_true_false() {
    let stdout = {
        let snippet = write_snippet("bool_tf", "#t #f");
        lex_snippet(&snippet)
    };
    assert!(stdout.contains("BOOLEAN  #t"), "{stdout}");
    assert!(stdout.contains("BOOLEAN  #f"), "{stdout}");
}

#[test]
#[ignore]
#[serial]
fn symbol_literal() {
    assert_dump_contains(
        "symbol_literal",
        "#\"sym\"",
        &["1:1-1:7  SYMBOL  #\\\"sym\\\""],
    );
}

#[test]
#[ignore]
#[serial]
fn no_errors_in_define_method_snippet() {
    // A realistic small method definition. Should produce many
    // token kinds and ZERO error tokens.
    let source = "\
define method foo (x :: <integer>) => (y :: <integer>)
  x + 1
end method;
";
    assert_no_errors("no_err_define_method", source);
}
