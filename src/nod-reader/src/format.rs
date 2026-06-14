//! `format_tokens` — stable line-oriented dump used by `dump-tokens` and
//! by snapshot tests. Schema fixed by `specs/01-lexer.md` §5.

use std::fmt::Write;

use crate::span::{FileId, SourceMap};
use crate::token::{Token, TokenKind};

/// Render a token stream as a stable text dump.
///
/// Each token is one line:
/// ```text
/// <line>:<col>-<line>:<col>  <KIND>  <text-display>
/// ```
///
/// - Lines and columns are 1-based.
/// - Kind is the screaming-snake-case name from [`TokenKind::name`].
/// - `text-display` is a Rust-style debug string of the source slice; the
///   `Eof` token has no text.
pub fn format_tokens(tokens: &[Token], file_id: FileId, source_map: &SourceMap) -> String {
    let mut out = String::new();
    for tok in tokens {
        let (lo_l, lo_c) = source_map.line_col(file_id, tok.span.lo);
        let (hi_l, hi_c) = source_map.line_col(file_id, tok.span.hi);
        write!(out, "{lo_l}:{lo_c}-{hi_l}:{hi_c}").unwrap();
        // Pad the position column to ~12 visual width so the kind column lines up.
        let pos_len =
            digit_count(lo_l) + digit_count(lo_c) + digit_count(hi_l) + digit_count(hi_c) + 3;
        for _ in pos_len..14 {
            out.push(' ');
        }
        write!(out, "{}", tok.kind.name()).unwrap();
        // Pad kind to 22 cols.
        for _ in tok.kind.name().len()..22 {
            out.push(' ');
        }
        if tok.kind != TokenKind::Eof {
            let slice = source_map.slice(tok.span);
            write!(out, "{slice:?}").unwrap();
        }
        out.push('\n');
    }
    out
}

fn digit_count(mut n: u32) -> usize {
    if n == 0 {
        return 1;
    }
    let mut c = 0;
    while n > 0 {
        c += 1;
        n /= 10;
    }
    c
}
