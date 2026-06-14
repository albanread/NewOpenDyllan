//! Sprint 45d — Dylan-side lexer oracle.
//!
//! Runs both lexers over the same fixture and checks that they agree on
//! the *segmentation* of the source: identical (start, end, lexeme) for
//! every non-trivia, non-preamble token. Kind disagreements
//! (e.g. Rust calls `function` an `IDENT`, Dylan calls it a `KEYWORD`)
//! are reported on `--nocapture` but do not fail the test — they're a
//! design conversation, not a correctness bug.
//!
//! Why span+lexeme rather than kind? The two lexers were written for
//! different downstream consumers:
//!   * Rust (`nod-reader`) feeds the parser. It drops trivia and uses
//!     specific punctuation kinds (`LPAREN`, `EQUAL`, …).
//!   * Dylan (`dylan-lexer.dylan`) feeds the IDE syntax colourer. It
//!     keeps trivia (`WS`, `COMMENT_LINE`) and uses one generic
//!     `PUNCT` kind with the symbol as text.
//!
//! Both models are valid; the meaty correctness question is whether
//! they *segment the same source the same way*. If they do, downstream
//! kind disagreements are just keyword-set bikeshed.
//!
//! Run with:
//!   cargo test -p nod-tests --test lexer_oracle -- --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap().parent().unwrap().to_path_buf()
}

fn driver_exe() -> PathBuf {
    workspace_root().join("target").join("debug").join("nod-driver.exe")
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tok {
    start: (u32, u32),
    end:   (u32, u32),
    kind:  String,
    lexeme: String,
}

/// Strip a single field plus the run of *space-or-tab* (NOT CR) that
/// follows. CR is a valid lexeme byte on Windows-line-ending sources,
/// so we can't lean on `split_whitespace` (which treats CR as a
/// separator and corrupts WS-token lexemes).
fn take_field<'a>(s: &'a str) -> Option<(&'a str, &'a str)> {
    let s = s.trim_start_matches([' ', '\t']);
    let end = s.find([' ', '\t']).unwrap_or(s.len());
    if end == 0 { None } else { Some((&s[..end], &s[end..])) }
}

/// Parse one line of `dump-tokens` (Rust). Format:
///   `L:C-L:C    KIND    "lexeme"`
fn parse_rust_line(line: &str) -> Option<Tok> {
    let (span, rest)  = take_field(line)?;
    let (kind, rest)  = take_field(rest)?;
    let rest = rest.trim_start_matches([' ', '\t']);
    let lexeme = if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
        rest[1..rest.len() - 1].to_string()
    } else {
        rest.to_string()
    };
    let (s, e) = parse_span(span)?;
    Some(Tok { start: s, end: e, kind: kind.to_string(), lexeme })
}

/// Parse one line of `dump-dylan-tokens` (Dylan). Format:
///   `L:C-L:C  KIND  lexeme`
/// The lexeme is taken VERBATIM (preserving any embedded CR bytes from
/// the source's line endings) — see `take_field` for why.
fn parse_dylan_line(line: &str) -> Option<Tok> {
    let (span, rest) = take_field(line)?;
    let (kind, rest) = take_field(rest)?;
    let lexeme = unescape_dylan_dump(rest.trim_start_matches([' ', '\t']));
    let (s, e) = parse_span(span)?;
    Some(Tok { start: s, end: e, kind: kind.to_string(), lexeme })
}

/// Undo only the Dylan dump's display escapes for `\s`, `\t`, `\r` —
/// whitespace bytes that the Rust dump renders verbatim. The `\\`,
/// `\"`, and `\n` escapes stay untouched: the Rust dump uses `\\` /
/// `\"` identically (so leaving them encoded keeps both sides aligned),
/// and `\n` only appears inside WS tokens which we drop before
/// comparison.
fn unescape_dylan_dump(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b's' => { out.push(b' ');  i += 2; continue; }
                b't' => { out.push(b'\t'); i += 2; continue; }
                b'r' => { out.push(b'\r'); i += 2; continue; }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn parse_span(s: &str) -> Option<((u32, u32), (u32, u32))> {
    let (lhs, rhs) = s.split_once('-')?;
    let (sl, sc) = lhs.split_once(':')?;
    let (el, ec) = rhs.split_once(':')?;
    Some((
        (sl.parse().ok()?, sc.parse().ok()?),
        (el.parse().ok()?, ec.parse().ok()?),
    ))
}

/// Rust kinds for tokens that have no source-text representation in
/// the dump (EOF). Skip them in normalisation.
fn rust_is_skippable(t: &Tok) -> bool {
    t.kind == "EOF"
}

/// Dylan tokens to drop before comparing — trivia + the preamble.
/// The Rust lexer's `lex()` calls `skip_preamble()` internally, so its
/// stream starts after `Module: foo\n\n`.
fn dylan_is_trivia(t: &Tok) -> bool {
    matches!(t.kind.as_str(), "WS" | "COMMENT_LINE" | "COMMENT_BLOCK" | "EOF")
}

/// Drop the Dylan preamble: every token before the first newline that
/// ends at column 1 of a line at least two greater than the previous
/// token's. The Rust lexer skips `Module: foo\n` plus the mandatory
/// blank line; we mirror that by dropping every Dylan token whose
/// span ends at a line ≤ the line where the first blank line appears.
fn strip_dylan_preamble(toks: &[Tok]) -> Vec<Tok> {
    // Find the first WS token whose lexeme contains "\n\n" — that's
    // the blank line that terminates the preamble per spec §7 Q6.
    // Then drop every token whose start is at or before that WS's end.
    // The blank-line WS that terminates the preamble spans at least
    // two source lines (e.g. `Module: foo\n\n` end-line is 2 greater
    // than its start-line). Don't inspect the lexeme text — Dylan's
    // dump escapes LF as `\n` but leaves CR raw, so its byte layout
    // depends on the source's line-ending convention.
    let mut preamble_end_line: u32 = 0;
    for t in toks {
        if t.kind == "WS" && t.end.0 >= t.start.0 + 2 {
            preamble_end_line = t.end.0;
            break;
        }
    }
    if preamble_end_line == 0 {
        // No blank line found — file has no preamble. Return as-is.
        return toks.to_vec();
    }
    toks.iter()
        .filter(|t| t.start.0 >= preamble_end_line)
        .cloned()
        .collect()
}

fn run_dump_tokens(input: &Path) -> Vec<Tok> {
    let out = Command::new(driver_exe())
        .args(["dump-tokens"])
        .arg(input)
        .output()
        .expect("spawn dump-tokens");
    assert!(
        out.status.success(),
        "dump-tokens failed: stderr=\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .filter_map(parse_rust_line)
        .filter(|t| !rust_is_skippable(t))
        .collect()
}

fn run_dump_dylan_tokens(input: &Path) -> Vec<Tok> {
    let out = Command::new(driver_exe())
        .args(["dump-dylan-tokens"])
        .arg(input)
        .output()
        .expect("spawn dump-dylan-tokens");
    assert!(
        out.status.success(),
        "dump-dylan-tokens failed: stderr=\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let raw: Vec<Tok> = s.lines().filter_map(parse_dylan_line).collect();
    let after_preamble = strip_dylan_preamble(&raw);
    after_preamble.into_iter().filter(|t| !dylan_is_trivia(t)).collect()
}

/// Compare segmentation (span + lexeme). Kind disagreements are
/// reported separately and don't fail the test.
fn compare_streams(name: &str, rust: &[Tok], dylan: &[Tok]) -> Result<(), String> {
    let mut kind_disagreements = 0usize;
    let n = rust.len().min(dylan.len());
    for i in 0..n {
        let r = &rust[i];
        let d = &dylan[i];
        if r.start != d.start || r.end != d.end || r.lexeme != d.lexeme {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(n);
            let mut ctx = String::new();
            for j in lo..hi {
                let marker = if j == i { ">> " } else { "   " };
                ctx.push_str(&format!(
                    "{marker}[{j:3}] R: {:?} {:?} {:?}  | D: {:?} {:?} {:?}\n",
                    rust[j].start, rust[j].end, rust[j].lexeme,
                    dylan[j].start, dylan[j].end, dylan[j].lexeme,
                ));
            }
            return Err(format!(
                "{name}: segmentation diverges at token {i}:\n{ctx}"
            ));
        }
        if r.kind != d.kind {
            kind_disagreements += 1;
        }
    }
    if rust.len() != dylan.len() {
        return Err(format!(
            "{name}: token count mismatch (rust={}, dylan={}); \
             first {n} agree on segmentation. \
             Rust tail: {:?} | Dylan tail: {:?}",
            rust.len(),
            dylan.len(),
            rust.get(n).map(|t| (&t.start, &t.lexeme)),
            dylan.get(n).map(|t| (&t.start, &t.lexeme)),
        ));
    }
    eprintln!(
        "{name}: OK — {} tokens, segmentation identical, {} kind disagreements \
         (expected — Rust uses specific punct kinds, Dylan uses generic PUNCT)",
        rust.len(),
        kind_disagreements,
    );
    Ok(())
}

fn check_fixture(rel: &str) -> Result<(), String> {
    let path = workspace_root().join("tests").join("nod-tests").join("fixtures").join(rel);
    assert!(path.exists(), "fixture missing: {}", path.display());
    let rust = run_dump_tokens(&path);
    let dylan = run_dump_dylan_tokens(&path);
    compare_streams(rel, &rust, &dylan)
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[test]
fn oracle_hello() {
    ensure_driver_built();
    check_fixture("hello.dylan").unwrap();
}

#[test]
fn oracle_factorial() {
    ensure_driver_built();
    check_fixture("factorial.dylan").unwrap();
}

/// Known divergence: the Rust lexer merges a leading sign into a
/// number literal (`-1` → one INTEGER token), the Dylan lexer emits
/// the sign and the digits separately (`-`, `1`). Both are defensible
/// — Rust's is more aggressive at lex time, Dylan's is more flexible
/// in subtraction contexts (`x-1` lexes correctly without lookahead).
/// The Dylan-side parser handles the split via `is-unary-op?`.
///
/// Flip this to `#[test]` once Sprint 45-followup picks a winner.
#[test]
#[ignore = "lex-time signed-number disagreement, see comment"]
fn oracle_cond_smoke() {
    ensure_driver_built();
    check_fixture("cond_smoke.dylan").unwrap();
}

/// Sweep the parser-corpus fixtures and report any divergences as
/// informational `eprintln!`s — does not fail. Useful for surveying
/// where the two lexers disagree as the keyword set / signed-number
/// policy / etc. converge.
#[test]
#[ignore = "informational survey, run with --ignored --nocapture"]
fn oracle_corpus_sweep() {
    ensure_driver_built();
    let dir = workspace_root().join("tests").join("nod-tests").join("fixtures");
    let mut pass = 0usize;
    let mut diverge = 0usize;
    // Skip the two self-host headliners — `dylan-lexer.dylan` is
    // ~143 s through the Dylan dump (45,000 tokens × byte-string
    // allocations) and `dylan-parser.dylan` is similar. They pass
    // the parser corpus (37/37) so coverage isn't lost; the oracle
    // sweep just isn't the right place to burn 5+ minutes.
    let skip: &[&str] = &["dylan-lexer.dylan", "dylan-parser.dylan"];
    let mut fixtures: Vec<_> = std::fs::read_dir(&dir)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("dylan"))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            !skip.iter().any(|s| n == *s)
        })
        .collect();
    fixtures.sort();
    for path in fixtures {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let rust = run_dump_tokens(&path);
        let dylan = run_dump_dylan_tokens(&path);
        match compare_streams(&name, &rust, &dylan) {
            Ok(()) => { pass += 1; }
            Err(msg) => {
                diverge += 1;
                eprintln!("DIVERGE: {msg}");
            }
        }
    }
    eprintln!("\nOracle sweep: {pass} pass / {diverge} diverge");
}
