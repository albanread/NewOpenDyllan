//! Dylan lexer — implements `specs/01-lexer.md` §1–§3.
//!
//! Predictive (peek-driven) lexer over UTF-8 bytes. Maximal-munch is
//! enforced explicitly per accepting state. The lexer never panics; on
//! malformed input it emits a [`TokenKind::Invalid`] covering the
//! offending bytes and resynchronises at the next safe boundary.

use crate::span::{FileId, Span};
use crate::token::{Token, TokenKind};

/// Lex the given source into a token vector.
///
/// The vector always ends with exactly one [`TokenKind::Eof`] token whose
/// span is the empty range at `src.len()`.
///
/// Per spec §7 Q6: a leading header preamble (`Module: foo\nAuthor: …\n\n`)
/// is **skipped** before the state machine runs. See [`scan_preamble`].
///
/// Sprint 51b — if a side-load override has been installed via
/// [`set_lex_override`], dispatch to it instead. The override is
/// install-once-for-process and is intended for the Dylan-side lexer
/// JIT'd in via `--lex-with-dylan` in `nod-driver`. With no override
/// set, the path is identical to the pre-51b behaviour.
pub fn lex(src: &str, file_id: FileId) -> Vec<Token> {
    if let Some(&f) = LEX_OVERRIDE.get() {
        return f(src, file_id);
    }
    lex_rust(src, file_id)
}

/// The original (Rust) lexer entry point. Always available; `lex` may
/// dispatch to a Dylan-side override but `lex_rust` is the canonical
/// fallback and the oracle tests' reference path.
pub fn lex_rust(src: &str, file_id: FileId) -> Vec<Token> {
    let mut lx = Lexer::new(src, file_id);
    let mut tokens = Vec::new();
    lx.skip_preamble();
    loop {
        lx.skip_trivia();
        let tok = lx.next_token();
        let is_eof = tok.kind == TokenKind::Eof;
        tokens.push(tok);
        if is_eof {
            break;
        }
    }
    tokens
}

/// Signature of an alternate `lex` implementation that can be installed
/// at runtime via [`set_lex_override`]. Must match [`lex_rust`] semantically:
/// must skip the preamble + trivia, must terminate the vector with exactly
/// one `Eof` token, and the spans of returned tokens MUST be byte-offset
/// pairs into the original `src`.
pub type LexFn = fn(&str, FileId) -> Vec<Token>;

static LEX_OVERRIDE: std::sync::OnceLock<LexFn> = std::sync::OnceLock::new();

/// Sprint 51b — install an alternate `lex` implementation. Subsequent
/// calls to [`lex`] dispatch through it; calls to [`lex_rust`] remain
/// unaffected (oracle tests use that as the reference path).
///
/// **Install-once.** A `OnceLock` backs the slot, so the first caller
/// wins. Re-installing a different function returns `Err(existing)`
/// per the standard `OnceLock` semantics. The driver installs at
/// startup (after parsing `--lex-with-dylan`) and never replaces; this
/// matches the "load once, redirect from Rust lexer" model — there's
/// nothing to unload and no need to swap mid-process.
pub fn set_lex_override(f: LexFn) -> Result<(), LexFn> {
    LEX_OVERRIDE.set(f)
}

/// Returns `true` if an alternate `lex` implementation is currently
/// installed. Used by the driver's `--lex-with-dylan` status line and
/// the oracle test to assert the override actually took effect.
pub fn has_lex_override() -> bool {
    LEX_OVERRIDE.get().is_some()
}

/// Result of scanning the header preamble at the top of a `.dylan` file.
/// Returned by [`scan_preamble`] for callers that want the structured
/// header rather than discarding it.
#[derive(Default, Debug, Clone)]
pub struct Preamble {
    /// Number of bytes consumed by the preamble (including the blank line
    /// terminator). The lexer should resume at this offset.
    pub end: u32,
    /// Header `Key: value` pairs in declaration order.
    pub entries: Vec<(String, String)>,
}

/// Scan the optional `Key: value` header block at the start of a Dylan
/// source file. Returns `None` if the file does not begin with one.
///
/// The block is a sequence of one or more `Key: value` lines terminated
/// by a blank line. The most important key is `Module:` (consumed by
/// `nod-namespace` in Sprint 05); the rest are documentation metadata.
pub fn scan_preamble(src: &str) -> Option<Preamble> {
    let bytes = src.as_bytes();
    // First non-blank line must be `Word: …` for a preamble to exist.
    let mut probe = 0;
    while probe < bytes.len() && (bytes[probe] == b' ' || bytes[probe] == b'\t') {
        probe += 1;
    }
    if probe == bytes.len() || !is_header_key_start(bytes[probe]) {
        return None;
    }
    // Peek the first line's colon.
    if !line_has_header_colon(&bytes[probe..]) {
        return None;
    }

    let mut p = Preamble::default();
    let mut i = 0u32;
    let mut current_key: Option<String> = None;
    let mut current_val = String::new();
    while (i as usize) < bytes.len() {
        let line_start = i as usize;
        let line_end = find_line_end(bytes, line_start);
        let line = &bytes[line_start..line_end];

        if line.is_empty() {
            // Blank line — end of preamble.
            if let Some(k) = current_key.take() {
                p.entries.push((k, std::mem::take(&mut current_val)));
            }
            // Consume the newline if any.
            i = next_line_start(bytes, line_end) as u32;
            p.end = i;
            return Some(p);
        }

        if line[0] == b' ' || line[0] == b'\t' {
            // Continuation of the current key.
            if current_key.is_some() {
                let cont = std::str::from_utf8(line).unwrap_or("").trim();
                if !current_val.is_empty() {
                    current_val.push(' ');
                }
                current_val.push_str(cont);
            } else {
                // Indented line with no prior key — not a preamble line.
                p.end = i;
                return if p.entries.is_empty() { None } else { Some(p) };
            }
        } else if let Some(colon) = line.iter().position(|&b| b == b':') {
            if let Some(k) = current_key.take() {
                p.entries.push((k, std::mem::take(&mut current_val)));
            }
            let key = std::str::from_utf8(&line[..colon])
                .unwrap_or("")
                .trim()
                .to_string();
            let val = std::str::from_utf8(&line[colon + 1..])
                .unwrap_or("")
                .trim()
                .to_string();
            current_key = Some(key);
            current_val = val;
        } else {
            // Line without `:` — preamble ends before this line.
            if let Some(k) = current_key.take() {
                p.entries.push((k, std::mem::take(&mut current_val)));
            }
            p.end = i;
            return if p.entries.is_empty() { None } else { Some(p) };
        }

        i = next_line_start(bytes, line_end) as u32;
    }
    // We exited the loop without seeing a blank line: the entire file
    // is `Key: value` lines with no terminator. Treat as *not* a
    // preamble so a one-line input like `foo: x::y` lexes normally
    // rather than getting eaten whole.
    let _ = (current_key, current_val);
    None
}

fn is_header_key_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

fn line_has_header_colon(line: &[u8]) -> bool {
    // Within the first line (before \n or end), look for ':' preceded by
    // identifier-shaped chars.
    let end = line
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(line.len());
    let line = &line[..end];
    if let Some(colon) = line.iter().position(|&b| b == b':') {
        // The chars before ':' must look like a header key: alpha + '-' + '_'.
        line[..colon]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && colon > 0
    } else {
        false
    }
}

fn find_line_end(bytes: &[u8], start: usize) -> usize {
    let rel = bytes[start..]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(bytes.len() - start);
    start + rel
}

fn next_line_start(bytes: &[u8], line_end: usize) -> usize {
    if line_end >= bytes.len() {
        return bytes.len();
    }
    match bytes[line_end] {
        b'\r' if line_end + 1 < bytes.len() && bytes[line_end + 1] == b'\n' => line_end + 2,
        b'\r' | b'\n' => line_end + 1,
        _ => line_end,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lexer state machine
// ─────────────────────────────────────────────────────────────────────────────

struct Lexer<'a> {
    src: &'a [u8],
    pos: u32,
    file_id: FileId,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, file_id: FileId) -> Self {
        debug_assert!(src.len() <= u32::MAX as usize);
        Self {
            src: src.as_bytes(),
            pos: 0,
            file_id,
        }
    }

    fn skip_preamble(&mut self) {
        if let Some(p) = scan_preamble(std::str::from_utf8(self.src).unwrap_or("")) {
            self.pos = p.end;
        }
    }

    fn span(&self, lo: u32, hi: u32) -> Span {
        Span::new(self.file_id, lo, hi)
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos as usize + off).copied()
    }

    fn done(&self) -> bool {
        self.pos as usize >= self.src.len()
    }

    /// Skip whitespace and comments. Handles **nested** block comments
    /// (spec §3.7).
    fn skip_trivia(&mut self) {
        loop {
            // Whitespace
            while let Some(b) = self.peek(0) {
                if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'\x0C' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Line comment
            if self.peek(0) == Some(b'/') && self.peek(1) == Some(b'/') {
                self.pos += 2;
                while let Some(b) = self.peek(0) {
                    if b == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            // Block comment (nested per spec §3.7)
            if self.peek(0) == Some(b'/') && self.peek(1) == Some(b'*') {
                self.pos += 2;
                let mut depth = 1;
                while depth > 0 && !self.done() {
                    if self.peek(0) == Some(b'/') && self.peek(1) == Some(b'*') {
                        self.pos += 2;
                        depth += 1;
                    } else if self.peek(0) == Some(b'*') && self.peek(1) == Some(b'/') {
                        self.pos += 2;
                        depth -= 1;
                    } else {
                        self.pos += 1;
                    }
                }
                continue;
            }
            break;
        }
    }

    /// Emit the next token. Trivia is consumed *before* this is called.
    fn next_token(&mut self) -> Token {
        let lo = self.pos;
        if self.done() {
            return Token {
                kind: TokenKind::Eof,
                span: self.span(lo, lo),
            };
        }
        let b = self.peek(0).unwrap();
        match b {
            // ── Punctuators ────────────────────────────────────────────
            b'(' => self.single(lo, TokenKind::LParen),
            b')' => self.single(lo, TokenKind::RParen),
            b'[' => self.single(lo, TokenKind::LBracket),
            b']' => self.single(lo, TokenKind::RBracket),
            b'{' => self.single(lo, TokenKind::LBrace),
            b'}' => self.single(lo, TokenKind::RBrace),
            b',' => self.single(lo, TokenKind::Comma),
            b';' => self.single(lo, TokenKind::Semicolon),

            // ── `.`, `..`, `...` (also leading-dot floats) ─────────────
            b'.' => self.lex_dot(lo),

            // ── Operators with multiple shapes ─────────────────────────
            b':' => self.lex_colon(lo),
            b'=' => self.lex_equal(lo),
            b'~' => self.lex_tilde(lo),
            b'<' => self.lex_less_or_ident_start(lo),
            b'>' => self.lex_greater_or_ident_start(lo),
            b'?' => self.lex_query(lo),

            // ── `+`, `-` may be signed-number lead or operator ─────────
            b'+' | b'-' => self.lex_plus_minus(lo, b),

            // ── Single-char graphic operators ──────────────────────────
            // These bytes are also in `is_ident_start` (spec §3.1), so we
            // must peek: a graphic followed by an ident-continue char is
            // the *start* of an identifier (`*global*`, `&foo`), not the
            // operator. Standalone, it's the operator.
            b'*' | b'/' | b'^' | b'&' | b'|' if self.peek(1).is_some_and(is_ident_continue) => {
                self.lex_ident(lo)
            }
            b'*' => self.single(lo, TokenKind::Star),
            b'/' => self.single(lo, TokenKind::Slash),
            b'^' => self.single(lo, TokenKind::Caret),
            b'&' => self.single(lo, TokenKind::Amp),
            b'|' => self.single(lo, TokenKind::Bar),

            // ── Hash-prefixed family ───────────────────────────────────
            b'#' => self.lex_hash(lo),

            // ── Strings & chars ────────────────────────────────────────
            b'"' => self.lex_string(lo),
            b'\'' => self.lex_char(lo),
            b'\\' => self.lex_backslash(lo),

            // ── Digits → numeric (or numeric-alpha → identifier per §3.10)
            b'0'..=b'9' => self.lex_numeric(lo),

            // ── Identifier (alpha or graphic identifier-start) ─────────
            _ if is_ident_start(b) => self.lex_ident(lo),

            // ── Unknown byte ───────────────────────────────────────────
            _ => {
                self.pos += 1;
                Token {
                    kind: TokenKind::Invalid,
                    span: self.span(lo, self.pos),
                }
            }
        }
    }

    fn single(&mut self, lo: u32, kind: TokenKind) -> Token {
        self.pos += 1;
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── . / .. / ... / leading-dot float ──────────────────────────────
    fn lex_dot(&mut self, lo: u32) -> Token {
        // `.` followed by digit → float fraction
        if self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            return self.lex_float_after_dot(lo);
        }
        self.pos += 1;
        if self.peek(0) == Some(b'.') && self.peek(1) == Some(b'.') {
            self.pos += 2;
            return Token {
                kind: TokenKind::Ellipsis,
                span: self.span(lo, self.pos),
            };
        }
        Token {
            kind: TokenKind::Dot,
            span: self.span(lo, self.pos),
        }
    }

    // ─── : :: := plus standalone Colon ─────────────────────────────────
    fn lex_colon(&mut self, lo: u32) -> Token {
        self.pos += 1;
        let kind = match self.peek(0) {
            Some(b':') => {
                self.pos += 1;
                TokenKind::ColonColon
            }
            Some(b'=') => {
                self.pos += 1;
                TokenKind::ColonEqual
            }
            _ => TokenKind::Colon,
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── = == => ───────────────────────────────────────────────────────
    fn lex_equal(&mut self, lo: u32) -> Token {
        self.pos += 1;
        let kind = match self.peek(0) {
            Some(b'=') => {
                self.pos += 1;
                TokenKind::EqualEqual
            }
            Some(b'>') => {
                self.pos += 1;
                TokenKind::Arrow
            }
            _ => TokenKind::Equal,
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── ~ ~= ~== ──────────────────────────────────────────────────────
    fn lex_tilde(&mut self, lo: u32) -> Token {
        self.pos += 1;
        let kind = match (self.peek(0), self.peek(1)) {
            (Some(b'='), Some(b'=')) => {
                self.pos += 2;
                TokenKind::TildeEqualEqual
            }
            (Some(b'='), _) => {
                self.pos += 1;
                TokenKind::TildeEqual
            }
            _ => TokenKind::Tilde,
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── < <= or start of identifier whose first char is < (per §3.1) ──
    fn lex_less_or_ident_start(&mut self, lo: u32) -> Token {
        // Per spec §3.1, `<foo>` is one identifier when the chars after `<`
        // are identifier-continuation chars. From a fresh token-start,
        // however, `<` followed by whitespace/digit/etc. is a *less-than*
        // operator. The disambiguation: if the *next* char is an
        // identifier-continuation (broad), absorb into an identifier.
        // Else it's `<` or `<=`.
        if self.peek(1).is_some_and(is_ident_continue_not_eq) {
            return self.lex_ident(lo);
        }
        self.pos += 1;
        if self.peek(0) == Some(b'=') {
            self.pos += 1;
            return Token {
                kind: TokenKind::LessEqual,
                span: self.span(lo, self.pos),
            };
        }
        Token {
            kind: TokenKind::Less,
            span: self.span(lo, self.pos),
        }
    }

    fn lex_greater_or_ident_start(&mut self, lo: u32) -> Token {
        if self.peek(1).is_some_and(is_ident_continue_not_eq) {
            return self.lex_ident(lo);
        }
        self.pos += 1;
        if self.peek(0) == Some(b'=') {
            self.pos += 1;
            return Token {
                kind: TokenKind::GreaterEqual,
                span: self.span(lo, self.pos),
            };
        }
        Token {
            kind: TokenKind::Greater,
            span: self.span(lo, self.pos),
        }
    }

    // ─── ? ?? ?= ?@ ────────────────────────────────────────────────────
    fn lex_query(&mut self, lo: u32) -> Token {
        self.pos += 1;
        let kind = match self.peek(0) {
            Some(b'?') => {
                self.pos += 1;
                TokenKind::QueryQuery
            }
            Some(b'=') => {
                self.pos += 1;
                TokenKind::QueryEqual
            }
            Some(b'@') => {
                self.pos += 1;
                TokenKind::QueryAt
            }
            _ => TokenKind::Query,
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── + - and signed numbers ────────────────────────────────────────
    fn lex_plus_minus(&mut self, lo: u32, sign: u8) -> Token {
        // `+3` or `-3.14` at token-start: signed numeric.
        if self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            return self.lex_numeric(lo);
        }
        // `.5` is leading-dot-float; `+.5` and `-.5` are not part of the
        // Dylan numeric literal grammar (per spec §2.4, fixed-point lead
        // requires a digit). Emit a bare operator.
        self.pos += 1;
        let kind = if sign == b'+' {
            TokenKind::Plus
        } else {
            TokenKind::Minus
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    // ─── # — large dispatch ────────────────────────────────────────────
    fn lex_hash(&mut self, lo: u32) -> Token {
        self.pos += 1; // consume '#'
        match self.peek(0) {
            None => Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            },
            Some(b'(') => {
                self.pos += 1;
                Token {
                    kind: TokenKind::HashLParen,
                    span: self.span(lo, self.pos),
                }
            }
            Some(b'[') => {
                self.pos += 1;
                Token {
                    kind: TokenKind::HashLBracket,
                    span: self.span(lo, self.pos),
                }
            }
            Some(b'{') => {
                self.pos += 1;
                Token {
                    kind: TokenKind::HashLBrace,
                    span: self.span(lo, self.pos),
                }
            }
            Some(b'#') => {
                self.pos += 1;
                Token {
                    kind: TokenKind::HashHash,
                    span: self.span(lo, self.pos),
                }
            }
            Some(b'"') => self.lex_symbol_after_hash(lo),
            Some(b':') => self.lex_hash_keyword(lo),
            Some(b't' | b'T') => self.match_hash_keyword(lo, b"", TokenKind::HashTrue),
            Some(b'f' | b'F') => self.match_hash_keyword(lo, b"", TokenKind::HashFalse),
            Some(b'b' | b'B') => self.lex_hash_radix(lo, 2),
            Some(b'o' | b'O') => self.lex_hash_radix(lo, 8),
            Some(b'x' | b'X') => self.lex_hash_radix(lo, 16),
            Some(b'n' | b'N') => self.match_hash_keyword(lo, b"ext", TokenKind::HashNext),
            Some(b'k' | b'K') => self.match_hash_keyword(lo, b"ey", TokenKind::HashKey),
            Some(b'a' | b'A') => self.match_hash_keyword(lo, b"ll-keys", TokenKind::HashAllKeys),
            Some(b'r' | b'R') => self.lex_hash_r(lo),
            _ => {
                // Skip the unknown char as well, then Invalid.
                self.pos += 1;
                Token {
                    kind: TokenKind::Invalid,
                    span: self.span(lo, self.pos),
                }
            }
        }
    }

    /// Match a fixed hash-keyword body (case-insensitive). `tail` is the
    /// expected continuation after the first letter (which has already
    /// been peeked but not consumed).
    fn match_hash_keyword(&mut self, lo: u32, tail: &[u8], kind: TokenKind) -> Token {
        // Consume the first letter.
        self.pos += 1;
        for &expected in tail {
            match self.peek(0) {
                Some(b) if b.eq_ignore_ascii_case(&expected) => self.pos += 1,
                _ => {
                    return Token {
                        kind: TokenKind::Invalid,
                        span: self.span(lo, self.pos),
                    };
                }
            }
        }
        // Reject `#trueish` etc. by checking the next char is *not* an
        // identifier continuation. If it is, fall back to Invalid (the
        // user has typed something weird).
        if self.peek(0).is_some_and(is_ident_continue) {
            // Eat the rest as part of the invalid span.
            while self.peek(0).is_some_and(is_ident_continue) {
                self.pos += 1;
            }
            return Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            };
        }
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    /// `#"foo"` symbol — same body grammar as a string literal.
    fn lex_symbol_after_hash(&mut self, lo: u32) -> Token {
        debug_assert_eq!(self.peek(0), Some(b'"'));
        self.pos += 1; // consume opening quote
        let tok = self.scan_string_body(lo);
        // Override the kind: a string-body lexed via `#"` is a Symbol.
        Token {
            kind: if tok.kind == TokenKind::Invalid {
                TokenKind::Invalid
            } else {
                TokenKind::Symbol
            },
            span: tok.span,
        }
    }

    /// `#:foo` hash-keyword literal.
    fn lex_hash_keyword(&mut self, lo: u32) -> Token {
        debug_assert_eq!(self.peek(0), Some(b':'));
        self.pos += 1; // consume ':'
        // Body uses the loose <keyword-syntax-symbol> alphabet (alpha,
        // digits, plus most graphic chars except hard separators).
        if !self.peek(0).is_some_and(is_ident_continue) {
            return Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            };
        }
        while self.peek(0).is_some_and(is_ident_continue) {
            self.pos += 1;
        }
        Token {
            kind: TokenKind::HashKeyword,
            span: self.span(lo, self.pos),
        }
    }

    /// `#b...` `#o...` `#x...` typed integers.
    fn lex_hash_radix(&mut self, lo: u32, radix: u32) -> Token {
        self.pos += 1; // consume the letter
        let mut any_digit = false;
        let mut last_was_underscore = false;
        while let Some(b) = self.peek(0) {
            if b == b'_' {
                if !any_digit || last_was_underscore {
                    break;
                }
                last_was_underscore = true;
                self.pos += 1;
            } else if is_digit_for_radix(b, radix) {
                any_digit = true;
                last_was_underscore = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        let kind = if !any_digit || last_was_underscore {
            TokenKind::Invalid
        } else {
            match radix {
                2 => TokenKind::IntegerBin,
                8 => TokenKind::IntegerOct,
                16 => TokenKind::IntegerHex,
                _ => unreachable!(),
            }
        };
        Token {
            kind,
            span: self.span(lo, self.pos),
        }
    }

    /// `#r` — disambiguate `#rest` from `#r"..."` raw string.
    fn lex_hash_r(&mut self, lo: u32) -> Token {
        // Consume the 'r'.
        self.pos += 1;
        // Look at the next char.
        match self.peek(0) {
            Some(b'"') => {
                // Raw string. Deferred per spec §3 — emit Invalid with a
                // descriptive span so the IDE shows something sensible.
                while let Some(b) = self.peek(0) {
                    self.pos += 1;
                    if b == b'"' {
                        break;
                    }
                }
                Token {
                    kind: TokenKind::StringRaw,
                    span: self.span(lo, self.pos),
                }
            }
            Some(b'e' | b'E') => {
                // `#rest` (case-insensitive).
                self.pos += 1;
                for expected in b"st" {
                    match self.peek(0) {
                        Some(b) if b.eq_ignore_ascii_case(expected) => self.pos += 1,
                        _ => {
                            return Token {
                                kind: TokenKind::Invalid,
                                span: self.span(lo, self.pos),
                            };
                        }
                    }
                }
                if self.peek(0).is_some_and(is_ident_continue) {
                    while self.peek(0).is_some_and(is_ident_continue) {
                        self.pos += 1;
                    }
                    return Token {
                        kind: TokenKind::Invalid,
                        span: self.span(lo, self.pos),
                    };
                }
                Token {
                    kind: TokenKind::HashRest,
                    span: self.span(lo, self.pos),
                }
            }
            _ => Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            },
        }
    }

    // ─── String literals ───────────────────────────────────────────────
    fn lex_string(&mut self, lo: u32) -> Token {
        debug_assert_eq!(self.peek(0), Some(b'"'));
        self.pos += 1;

        // Empty-string carve-out (spec §3.8): `""` is always String("").
        // Check before triple-quote heuristic.
        if self.peek(0) == Some(b'"') && self.peek(1) != Some(b'"') {
            self.pos += 1;
            return Token {
                kind: TokenKind::String,
                span: self.span(lo, self.pos),
            };
        }
        // Triple-quote multi-line. Spec §2.5 — recognised but full
        // body-stripping is deferred. v1 scans naively to the closing
        // `"""` and emits StringMulti.
        if self.peek(0) == Some(b'"') && self.peek(1) == Some(b'"') {
            self.pos += 2; // consume the second and third `"`
            return self.scan_multi_string(lo);
        }
        self.scan_string_body(lo)
    }

    /// Single-line string body. `self.pos` points just past the opening `"`.
    fn scan_string_body(&mut self, lo: u32) -> Token {
        while let Some(b) = self.peek(0) {
            match b {
                b'"' => {
                    self.pos += 1;
                    return Token {
                        kind: TokenKind::String,
                        span: self.span(lo, self.pos),
                    };
                }
                b'\\' => {
                    self.pos += 1;
                    // Accept the next byte (any escape is fine for v1).
                    if self.peek(0).is_some() {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    // Unterminated; emit Invalid up to the newline.
                    return Token {
                        kind: TokenKind::Invalid,
                        span: self.span(lo, self.pos),
                    };
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
        Token {
            kind: TokenKind::Invalid,
            span: self.span(lo, self.pos),
        }
    }

    fn scan_multi_string(&mut self, lo: u32) -> Token {
        // Naive scan for closing `"""`. Spec §2.5 whitespace-prefix
        // stripping is a follow-up.
        while !self.done() {
            if self.peek(0) == Some(b'"')
                && self.peek(1) == Some(b'"')
                && self.peek(2) == Some(b'"')
            {
                self.pos += 3;
                return Token {
                    kind: TokenKind::StringMulti,
                    span: self.span(lo, self.pos),
                };
            }
            self.pos += 1;
        }
        Token {
            kind: TokenKind::Invalid,
            span: self.span(lo, self.pos),
        }
    }

    // ─── Character literal ─────────────────────────────────────────────
    fn lex_char(&mut self, lo: u32) -> Token {
        debug_assert_eq!(self.peek(0), Some(b'\''));
        self.pos += 1;
        match self.peek(0) {
            Some(b'\\') => {
                self.pos += 1;
                // Hex escape `\<HHHH>`: read up to `>`.
                if self.peek(0) == Some(b'<') {
                    self.pos += 1;
                    while let Some(b) = self.peek(0) {
                        if b == b'>' {
                            self.pos += 1;
                            break;
                        }
                        if !b.is_ascii_hexdigit() {
                            break;
                        }
                        self.pos += 1;
                    }
                } else if self.peek(0).is_some() {
                    self.pos += 1; // single-char escape
                }
            }
            Some(b'\'') | None => {
                return Token {
                    kind: TokenKind::Invalid,
                    span: self.span(lo, self.pos),
                };
            }
            Some(_) => {
                self.pos += 1;
            }
        }
        if self.peek(0) == Some(b'\'') {
            self.pos += 1;
            Token {
                kind: TokenKind::Char,
                span: self.span(lo, self.pos),
            }
        } else {
            // Eat until close-quote or end of line so we don't leak garbage.
            while let Some(b) = self.peek(0) {
                if b == b'\n' || b == b'\'' {
                    break;
                }
                self.pos += 1;
            }
            if self.peek(0) == Some(b'\'') {
                self.pos += 1;
            }
            Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            }
        }
    }

    // ─── Backslash-escape identifier (`\+`, `\if`, …) ─────────────────
    fn lex_backslash(&mut self, lo: u32) -> Token {
        self.pos += 1; // consume '\'
        if !self.peek(0).is_some_and(is_ident_continue) {
            return Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            };
        }
        while self.peek(0).is_some_and(is_ident_continue) {
            self.pos += 1;
        }
        Token {
            kind: TokenKind::EscapedIdent,
            span: self.span(lo, self.pos),
        }
    }

    // ─── Numeric literal ───────────────────────────────────────────────
    fn lex_numeric(&mut self, lo: u32) -> Token {
        // Optional sign already at `self.pos` if called from lex_plus_minus.
        if matches!(self.peek(0), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        // Eat integer digit body (with underscores between digits).
        let mut last_was_underscore = false;
        let mut had_digit = false;
        while let Some(b) = self.peek(0) {
            if b.is_ascii_digit() {
                had_digit = true;
                last_was_underscore = false;
                self.pos += 1;
            } else if b == b'_'
                && had_digit
                && !last_was_underscore
                && self.peek(1).is_some_and(|n| n.is_ascii_digit())
            {
                last_was_underscore = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if !had_digit {
            return Token {
                kind: TokenKind::Invalid,
                span: self.span(lo, self.pos),
            };
        }

        // Fraction part `.<digits>` — note `3.` is also a float.
        let mut is_float = false;
        if self.peek(0) == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while self
                .peek(0)
                .is_some_and(|b| b.is_ascii_digit() || b == b'_')
            {
                self.pos += 1;
            }
        }

        // Exponent: e/E/s/S/d/D/x/X
        if let Some(b) = self.peek(0)
            && matches!(b, b'e' | b'E' | b's' | b'S' | b'd' | b'D' | b'x' | b'X')
        {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while self
                .peek(0)
                .is_some_and(|b| b.is_ascii_digit() || b == b'_')
            {
                self.pos += 1;
            }
        }

        // Ratio? `<digits>/<digits>` with no intervening space and no
        // float-fraction taken. Spec §2.4 — deferred recognition kept
        // but only when not already a float.
        if !is_float
            && self.peek(0) == Some(b'/')
            && self.peek(1).is_some_and(|b| b.is_ascii_digit())
        {
            self.pos += 1;
            while self
                .peek(0)
                .is_some_and(|b| b.is_ascii_digit() || b == b'_')
            {
                self.pos += 1;
            }
            return Token {
                kind: TokenKind::Ratio,
                span: self.span(lo, self.pos),
            };
        }

        // §3.10: numeric followed by alpha (other than the exponent
        // markers we already consumed) → identifier.
        if self.peek(0).is_some_and(is_ident_continue) {
            // Fold into an identifier token covering everything from `lo`.
            while self.peek(0).is_some_and(is_ident_continue) {
                self.pos += 1;
            }
            return self.classify_ident_or_keyword(lo);
        }

        Token {
            kind: if is_float {
                TokenKind::Float
            } else {
                TokenKind::Integer
            },
            span: self.span(lo, self.pos),
        }
    }

    // ─── Identifier ────────────────────────────────────────────────────
    fn lex_ident(&mut self, lo: u32) -> Token {
        while self.peek(0).is_some_and(is_ident_continue) {
            self.pos += 1;
        }
        self.classify_ident_or_keyword(lo)
    }

    /// After eating an identifier body starting at `lo`, decide whether
    /// the trailing `:` (if any) folds in as a `KeywordColon`, and whether
    /// the text matches one of the three hard reserveds.
    fn classify_ident_or_keyword(&mut self, lo: u32) -> Token {
        // Peek for trailing `:` that is NOT part of `::` or `:=`.
        if self.peek(0) == Some(b':') && self.peek(1) != Some(b':') && self.peek(1) != Some(b'=') {
            self.pos += 1; // consume the colon
            return Token {
                kind: TokenKind::KeywordColon,
                span: self.span(lo, self.pos),
            };
        }
        let span = self.span(lo, self.pos);
        let text = &self.src[lo as usize..self.pos as usize];
        let kind = match text {
            b"define" => TokenKind::KwDefine,
            b"end" => TokenKind::KwEnd,
            b"otherwise" => TokenKind::KwOtherwise,
            _ => TokenKind::Ident,
        };
        Token { kind, span }
    }

    // ─── Leading-dot float (`.5`) ──────────────────────────────────────
    fn lex_float_after_dot(&mut self, lo: u32) -> Token {
        debug_assert_eq!(self.peek(0), Some(b'.'));
        self.pos += 1;
        while self
            .peek(0)
            .is_some_and(|b| b.is_ascii_digit() || b == b'_')
        {
            self.pos += 1;
        }
        if let Some(b) = self.peek(0)
            && matches!(b, b'e' | b'E' | b's' | b'S' | b'd' | b'D' | b'x' | b'X')
        {
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while self
                .peek(0)
                .is_some_and(|b| b.is_ascii_digit() || b == b'_')
            {
                self.pos += 1;
            }
        }
        Token {
            kind: TokenKind::Float,
            span: self.span(lo, self.pos),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Char-class predicates (spec §3.1)
// ─────────────────────────────────────────────────────────────────────────────

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
        || matches!(
            b,
            b'_' | b'$' | b'*' | b'%' | b'@' | b'!' | b'&'
            | b'^' | b'|' | b'/'
        )
        // 8-bit-ASCII extensions are accepted (spec §1 in scope list).
        || b >= 0x80
}

/// Identifier-continuation alphabet from `lexer-transitions.dylan:339-342`.
fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'&'
                | b'*'
                | b'<'
                | b'='
                | b'>'
                | b'|'
                | b'^'
                | b'$'
                | b'%'
                | b'@'
                | b'_'
                | b'+'
                | b'~'
                | b'?'
                | b'/'
                | b'-'
        )
        || b >= 0x80
}

/// Subset of identifier-continuation that excludes `=`, used by the
/// `<` / `>` dispatch so `<=` and `>=` still lex as operators rather
/// than getting absorbed into a phantom identifier.
fn is_ident_continue_not_eq(b: u8) -> bool {
    is_ident_continue(b) && b != b'='
}

fn is_digit_for_radix(b: u8, radix: u32) -> bool {
    match radix {
        2 => matches!(b, b'0' | b'1'),
        8 => matches!(b, b'0'..=b'7'),
        16 => b.is_ascii_hexdigit(),
        _ => unreachable!(),
    }
}
