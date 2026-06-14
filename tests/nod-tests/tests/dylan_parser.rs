//! Sprint 46 — Dylan-in-Dylan parser tests.
//!
//! The parser source lives at
//! `tests/nod-tests/fixtures/dylan-parser.dylan` and is compiled
//! together with `dylan-lexer.dylan` into a cached EXE by the driver
//! subcommand `nod-driver parse-dylan <path>`. That subcommand AOT-builds
//! [lexer, parser] once into the OS tempdir, then runs the EXE with
//! `<path>` as argv[1] and forwards stdout (the indented AST dump).
//!
//! These tests mirror `dylan_lexer.rs`: each is `#[ignore]` + `#[serial]`
//! because the pipeline shells out to `cargo run --bin nod-driver` plus
//! MSVC's `link.exe`, and concurrent invocations would stall on Cargo's
//! build-system lock.
//!
//! Run with:
//!
//! ```text
//! cargo test --test dylan_parser -- --ignored --nocapture
//! ```

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nod_tests::test_support::run_command_with_watchdog;
use serial_test::serial;

/// Workspace root inferred from `CARGO_MANIFEST_DIR`. Mirrors the helper
/// in `dylan_lexer.rs`.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Per-test scratch dir for the snippets we'll parse. All snippet files go
/// under `<target>/dylan-parser-test-snippets/<name>.dylan`.
fn snippet_dir() -> PathBuf {
    let workspace = workspace_root();
    let dir = workspace
        .join("target")
        .join("dylan-parser-test-snippets");
    std::fs::create_dir_all(&dir).expect("create snippet dir");
    dir
}

/// Write a snippet to a temp file and return its absolute path.
fn write_snippet(name: &str, contents: &str) -> PathBuf {
    let path = snippet_dir().join(format!("{name}.dylan"));
    std::fs::write(&path, contents).expect("write snippet");
    path
}

/// Pre-build `nod-driver` + `nod-runtime` once per test to avoid races
/// against Cargo's build lock when the parser EXE is built.
fn prebuild_driver(workspace: &Path) {
    let mut build = Command::new("cargo");
    build
        .current_dir(workspace)
        .args(["build", "-p", "nod-driver", "-p", "nod-runtime"]);
    let build = run_command_with_watchdog(
        "dylan_parser",
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

/// Parse one snippet through the cached parser EXE; return stdout as a
/// UTF-8 String (the AST dump).
fn parse_snippet(snippet: &Path) -> String {
    let workspace = workspace_root();
    prebuild_driver(&workspace);
    let mut driver = Command::new("cargo");
    driver.current_dir(&workspace).args([
        "run",
        "--quiet",
        "--bin",
        "nod-driver",
        "--",
        "parse-dylan",
        snippet.to_str().unwrap(),
    ]);
    let driver = run_command_with_watchdog(
        "dylan_parser",
        "parse-dylan",
        Duration::from_secs(180),
        &mut driver,
    );
    let stdout = driver.stdout.clone();
    let stderr = driver.stderr.clone();
    assert!(
        driver.status.success(),
        "parse-dylan on {} failed: {}\nstdout:\n{}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
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

/// Parse a snippet and assert the dump contains all of `expected` and no
/// `ERROR` lines.
fn assert_dump(name: &str, source: &str, expected: &[&str]) {
    let snippet = write_snippet(name, source);
    let dump = parse_snippet(&snippet);
    assert!(
        !dump.lines().any(|l| l.contains("ERROR")),
        "test {name}: unexpected ERROR in dump:\n{dump}"
    );
    for want in expected {
        assert!(
            dump.contains(want),
            "test {name}: expected substring {want:?}\ndump:\n{dump}"
        );
    }
}

// ─── headline: define method with typed params + return ───────────────────

/// Sprint 46 headline acceptance — `define method foo (x :: <integer>) =>
/// (y :: <integer>)` must parse into a structured signature instead of
/// crashing with "expected ) after arguments".
#[test]
#[ignore]
#[serial]
fn define_method_typed_signature() {
    let source = "\
define method foo (x :: <integer>) => (y :: <integer>)
  x + 1
end method;
";
    assert_dump(
        "define_method_typed_signature",
        source,
        &[
            "DEFINE-BODY method foo",
            "PARAMS",
            "PARAM x",
            "RETURNS",
            "VALUE y",
            "NAME <integer>",
        ],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_multiple_params() {
    let source = "\
define method add (x :: <integer>, y :: <integer>) => (z :: <integer>)
  x + y
end method;
";
    assert_dump(
        "define_method_multiple_params",
        source,
        &["DEFINE-BODY method add", "PARAM x", "PARAM y", "VALUE z"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_untyped_param_and_value() {
    let source = "\
define method id (x) => (y)
  x
end method;
";
    assert_dump(
        "define_method_untyped",
        source,
        &["DEFINE-BODY method id", "PARAM x", "VALUE y"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_rest_param() {
    let source = "\
define method collect (#rest more) => ()
  more
end method;
";
    assert_dump(
        "define_method_rest",
        source,
        &["DEFINE-BODY method collect", "PARAMS", "REST more", "RETURNS"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_key_params() {
    let source = "\
define method opts (x, #key a b) => ()
  x
end method;
";
    assert_dump(
        "define_method_key",
        source,
        &["PARAM x", "KEY", "KEY-PARAM a", "KEY-PARAM b"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_all_keys() {
    let source = "\
define method anyopts (#key a, #all-keys) => ()
  a
end method;
";
    assert_dump(
        "define_method_all_keys",
        source,
        &["KEY", "KEY-PARAM a", "ALL-KEYS"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_empty_return() {
    let source = "\
define method noret (x :: <integer>) => ()
  x
end method;
";
    // `=> ()` is present but carries no values: RETURNS block with no VALUE.
    assert_dump(
        "define_method_empty_return",
        source,
        &["DEFINE-BODY method noret", "PARAM x", "RETURNS"],
    );
}

#[test]
#[ignore]
#[serial]
fn define_method_bare_return_name() {
    let source = "\
define method bare (x) => name
  x
end method;
";
    assert_dump(
        "define_method_bare_return",
        source,
        &["DEFINE-BODY method bare", "RETURNS", "VALUE name"],
    );
}

// ─── anonymous method literal in expression position ───────────────────────

/// `method (x :: <integer>) => (<integer>) x end` as an anonymous literal.
#[test]
#[ignore]
#[serial]
fn anonymous_method_literal() {
    let source = "let f = method (x :: <integer>) => (<integer>) x end;\n";
    assert_dump(
        "anonymous_method_literal",
        source,
        &["STMT method", "PARAMS", "PARAM x", "RETURNS"],
    );
}

// ─── local method ──────────────────────────────────────────────────────────

#[test]
#[ignore]
#[serial]
fn local_method_signature() {
    let source = "\
local method helper (a :: <integer>, b) => (s :: <integer>)
  a + b
end method helper;
";
    assert_dump(
        "local_method_signature",
        source,
        &["LOCAL", "STMT method helper", "PARAM a", "PARAM b", "VALUE s"],
    );
}

// ─── headline: define class ────────────────────────────────────────────────

/// Sprint 46 (class chunk) headline acceptance — `define class <point>
/// (<object>) slot ... end class;` must parse into a structured class node
/// with superclass and slot specs instead of crashing with "unexpected token
/// in expression".
#[test]
#[ignore]
#[serial]
fn define_class_headline() {
    let source = "\
define class <point> (<object>)
  slot point-x :: <integer>, init-keyword: x:;
  slot point-y :: <integer> = 0;
  constant slot point-name :: <string>, required-init-keyword: name:;
end class;
";
    assert_dump(
        "define_class_headline",
        source,
        &[
            "DEFINE-CLASS <point>",
            "SUPER",
            "NAME <object>",
            "SLOT point-x",
            "TYPE",
            "NAME <integer>",
            "INIT-KEYWORD x",
            "SLOT point-y",
            "INIT",
            "INT 0",
            "SLOT point-name",
            "ADJ constant",
            "REQUIRED-INIT-KEYWORD name",
        ],
    );
}

/// Multiple superclasses `(<a>, <b>)` — each becomes its own SUPER subtree.
#[test]
#[ignore]
#[serial]
fn define_class_multiple_supers() {
    let source = "\
define class <c> (<a>, <b>)
  slot x;
end class;
";
    assert_dump(
        "define_class_multiple_supers",
        source,
        &[
            "DEFINE-CLASS <c>",
            "SUPER",
            "NAME <a>",
            "NAME <b>",
            "SLOT x",
        ],
    );
}

/// Slot adjectives — `each-subclass slot`, `class slot`, `virtual slot` and a
/// `constant slot` all surface as ADJ lines before the slot name.
#[test]
#[ignore]
#[serial]
fn define_class_slot_adjectives() {
    let source = "\
define class <thing> (<object>)
  each-subclass slot count :: <integer> = 0;
  class slot total :: <integer>;
  constant slot tag :: <symbol>;
end class;
";
    assert_dump(
        "define_class_slot_adjectives",
        source,
        &[
            "DEFINE-CLASS <thing>",
            "ADJ each-subclass",
            "SLOT count",
            "ADJ class",
            "SLOT total",
            "ADJ constant",
            "SLOT tag",
        ],
    );
}

/// Empty class body — `define class <c> (<object>) end class;` parses to a
/// class node with one super and no slots.
#[test]
#[ignore]
#[serial]
fn define_class_empty_body() {
    let source = "define class <empty> (<object>) end class;\n";
    assert_dump(
        "define_class_empty_body",
        source,
        &["DEFINE-CLASS <empty>", "SUPER", "NAME <object>"],
    );
}

/// A bare slot with no type and no init options still parses.
#[test]
#[ignore]
#[serial]
fn define_class_bare_slot() {
    let source = "\
define class <c> (<object>)
  slot x;
end class;
";
    assert_dump(
        "define_class_bare_slot",
        source,
        &["DEFINE-CLASS <c>", "SLOT x"],
    );
}

/// `init-value:` option (explicit, not the `=` shorthand).
#[test]
#[ignore]
#[serial]
fn define_class_init_value_option() {
    let source = "\
define class <c> (<object>)
  slot x :: <integer>, init-value: 42;
end class;
";
    assert_dump(
        "define_class_init_value_option",
        source,
        &["SLOT x", "INIT", "INT 42"],
    );
}

// ─── no-regression: simpler shapes still parse ─────────────────────────────

#[test]
#[ignore]
#[serial]
fn simple_call_and_let_still_parse() {
    let source = "format-out(\"hi\");\nlet x = 1 + 2;\n";
    assert_dump(
        "simple_call_and_let",
        source,
        &["CALL", "NAME format-out", "LET", "BINOP"],
    );
}

// ─── multi-clause statements (if / block / select) ─────────────────────────

/// `if (c) ... elseif (c) ... else ...` parses into a STMT with the leading
/// body plus one CLAUSE per `elseif` / `else`.  Previously everything after
/// the first clause was silently dropped (parse-statement parsed one body
/// then looked straight for `end`).
#[test]
#[ignore]
#[serial]
fn if_elseif_else_clauses() {
    let source = "\
define function classify (n :: <integer>) => (s :: <byte-string>)
  if (n < 0)
    \"negative\"
  elseif (n = 0)
    \"zero\"
  else
    \"positive\"
  end if
end function;
";
    assert_dump(
        "if_elseif_else_clauses",
        source,
        &[
            "STMT if",
            "STRING \"negative\"",
            "CLAUSE elseif",
            "STRING \"zero\"",
            "CLAUSE else",
            "STRING \"positive\"",
        ],
    );
}

/// `block (return) ... exception (e :: <error>) ... cleanup ... end block`
/// parses into a STMT with CLAUSE exception / CLAUSE cleanup.  The typed
/// exception head `(e :: <error>)` becomes a PAREN-LIST holding a TYPED-NAME.
#[test]
#[ignore]
#[serial]
fn block_cleanup_exception_clauses() {
    let source = "\
define function risky () => (r :: <object>)
  block (return)
    do-work();
    return(42);
  exception (e :: <error>)
    handle-it(e);
  cleanup
    close-things();
  end block
end function;
";
    assert_dump(
        "block_cleanup_exception_clauses",
        source,
        &[
            "STMT block",
            "CLAUSE exception",
            "PAREN-LIST",
            "TYPED-NAME e",
            "NAME <error>",
            "CLAUSE cleanup",
            "NAME close-things",
        ],
    );
}

/// `select (n) 1 => "one"; ... otherwise => "many"; end select` parses each
/// arm as BINOP(key, =>, body); `otherwise` stays in the body as an arm key
/// (NAME otherwise) rather than splitting the statement.
#[test]
#[ignore]
#[serial]
fn select_arms_and_otherwise() {
    let source = "\
define function name-of (n :: <integer>) => (s :: <byte-string>)
  select (n)
    1 => \"one\";
    2 => \"two\";
    otherwise => \"many\";
  end select
end function;
";
    assert_dump(
        "select_arms_and_otherwise",
        source,
        &[
            "STMT select",
            "BINOP",
            "INT 1",
            "arrow",
            "STRING \"one\"",
            "NAME otherwise",
            "STRING \"many\"",
        ],
    );
}

/// Numeric `for (i from 1 to n)` header parses into a FOR-CLAUSE with `from`
/// and `to` connector parts.  Previously the `from`/`to` keywords broke the
/// parenthesised-head parser with "expected ) after parenthesised expression".
#[test]
#[ignore]
#[serial]
fn for_header_numeric_range() {
    let source = "\
define function sum-to (n :: <integer>) => (total :: <integer>)
  let total = 0;
  for (i from 1 to n)
    total := total + i;
  end for;
  total
end function;
";
    assert_dump(
        "for_header_numeric_range",
        source,
        &[
            "STMT for",
            "FOR-CLAUSE i",
            "PART from",
            "INT 1",
            "PART to",
            "NAME n",
        ],
    );
}

/// Multi-clause `for` header: explicit iteration (`item in items`), explicit
/// step (`count = 0 then count + 1`), and a `until` end-test clause (no loop
/// variable) — each is its own FOR-CLAUSE with connector parts.
#[test]
#[ignore]
#[serial]
fn for_header_in_step_and_until() {
    let source = "\
define function walk (items :: <object>) => ()
  for (item in items,
       count = 0 then count + 1,
       until count > 100)
    process(item, count);
  end for;
end function;
";
    assert_dump(
        "for_header_in_step_and_until",
        source,
        &[
            "STMT for",
            "FOR-CLAUSE item",
            "PART in",
            "NAME items",
            "FOR-CLAUSE count",
            "PART equal",
            "PART then",
            "PART until",
        ],
    );
}

// ─── define generic ────────────────────────────────────────────────────────

/// `define generic NAME (params) => (returns);` parses into a DEFINE-GENERIC
/// node with a parameter list and return spec, no body and no `end`.
/// Previously routed through the body-word path, which tried to parse a body
/// and crashed ("expected ) after arguments").
#[test]
#[ignore]
#[serial]
fn define_generic_signature() {
    let source = "define generic area (shape :: <shape>) => (a :: <float>);\n";
    assert_dump(
        "define_generic_signature",
        source,
        &[
            "DEFINE-GENERIC area",
            "PARAMS",
            "PARAM shape",
            "NAME <shape>",
            "RETURNS",
            "VALUE a",
            "NAME <float>",
        ],
    );
}

/// `define sealed generic ... => (<integer>);` — the `sealed` adjective is a
/// reserved keyword token, so it must still be collected as a MOD (the old
/// identifier-only modifier scan dropped it and then failed). The bare-type
/// return `(<integer>)` carries no value name.
#[test]
#[ignore]
#[serial]
fn define_generic_sealed_bare_return() {
    let source =
        "define sealed generic run-task (t :: <task>, packet :: <integer>) => (<integer>);\n";
    assert_dump(
        "define_generic_sealed_bare_return",
        source,
        &[
            "DEFINE-GENERIC run-task",
            "MOD sealed",
            "PARAM t",
            "PARAM packet",
            "RETURNS",
            "VALUE <integer>",
        ],
    );
}

// ─── infix word operators ──────────────────────────────────────────────────

/// `mod` and `rem` are the only identifier (word) infix operators (nod-reader
/// parse_mul). They previously weren't recognized, so `a mod b` (e.g. inside
/// a call: `gcd(b, a mod b)`) crashed with "expected ) after arguments".
#[test]
#[ignore]
#[serial]
fn infix_mod_rem_operators() {
    let source = "\
define function f (a :: <integer>, b :: <integer>) => (<integer>)
  (a mod b) + (a rem b)
end function;
";
    assert_dump(
        "infix_mod_rem_operators",
        source,
        &["BINOP", "mod", "rem", "plus", "NAME a", "NAME b"],
    );
}
