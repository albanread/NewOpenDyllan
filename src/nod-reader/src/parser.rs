//! Dylan expression parser — Sprint 03 Pratt parser, Sprint 04 statement
//! and top-level layers.
//!
//! Sketch in `SPRINTS.md` §117–149 and §151–178. Operates on the raw
//! token stream; source text is required to read identifier/literal
//! lexemes.

use std::collections::HashSet;

use crate::ast::{
    BinOp, Binder, ExceptionClause, Expr, ForClause, FromForClause, ImportSet,
    ImportSpec, Item, LibraryUseClause, LocalMethodDecl, Modifier, Module, ModuleUseClause,
    NumericForClause, Param, ReturnRest, ReturnSig, ReturnValue, SlotAllocation, SlotDef,
    Statement, StepForClause, UnOp,
};
use crate::fragments::Fragment;
use crate::lexer::Preamble;
use crate::span::Span;
use crate::token::{Token, TokenKind};

type IfBranches = (Vec<Expr>, Option<Vec<Expr>>, Token);
type ClassBody = (Vec<Expr>, Vec<SlotDef>);
type LibraryHead = (Vec<LibraryUseClause>, Vec<String>, Vec<String>);
type ModuleHead = (Vec<ModuleUseClause>, Vec<String>, Vec<String>);
type UseClauseOptions = (
    Option<ImportSet>,
    Vec<String>,
    Vec<(String, String)>,
    Option<String>,
    Option<ImportSet>,
);

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Diagnostic {}

pub fn parse_expr(src: &str, tokens: &[Token]) -> Result<Expr, Diagnostic> {
    parse_expr_with_macros(src, tokens, &HashSet::new())
}

/// Sprint 25: expression parser variant that recognises body-shaped
/// macro calls. `known_macros` is the set of names (typically populated
/// from the stdlib + the surrounding module's `define macro` items) the
/// parser should treat as macro call sites when it sees the shape
/// `<name>(head…) body… end`. Callers that don't need this (most
/// tests, the `dump-tokens`/`dump-ast` paths) pass an empty set.
pub fn parse_expr_with_macros(
    src: &str,
    tokens: &[Token],
    known_macros: &HashSet<String>,
) -> Result<Expr, Diagnostic> {
    let mut p = Parser::new(src, tokens, known_macros);
    let e = p.parse_expr_full()?;
    p.skip_trailing_semis();
    if !p.at_end() {
        let t = p.peek();
        return Err(p.diag(t.span, format!("unexpected token {:?} after expression", t.kind)));
    }
    Ok(e)
}

pub fn parse_top_level_exprs(src: &str, tokens: &[Token]) -> Result<Vec<Expr>, Vec<Diagnostic>> {
    let empty = HashSet::new();
    let mut p = Parser::new(src, tokens, &empty);
    let mut out = Vec::new();
    let mut diags = Vec::new();
    p.skip_trailing_semis();
    while !p.at_end() {
        match p.parse_expr_full() {
            Ok(e) => out.push(e),
            Err(d) => {
                diags.push(d);
                p.recover_to_semi();
            }
        }
        p.skip_trailing_semis();
    }
    if diags.is_empty() {
        Ok(out)
    } else {
        Err(diags)
    }
}

/// Sprint 04 entrypoint — parse a full Dylan source file (header + items).
///
/// The lexer skips the preamble; pass it in here so it lands on the AST.
/// If `preamble` is `None`, [`crate::scan_preamble`] is called on `src`.
pub fn parse_module(
    src: &str,
    tokens: &[Token],
    preamble: Option<&Preamble>,
) -> Result<Module, Vec<Diagnostic>> {
    parse_module_with_macros(src, tokens, preamble, &HashSet::new())
}

/// Sprint 25: top-level entry that takes a pre-seeded set of macro
/// names the parser should recognise as body-shaped call sites
/// (`<name>(…) … end`). Used by `nod-sema::expand_and_lower_module`
/// to seed in the stdlib's macros (so user code can write
/// `for-each (x in c) … end` without referencing the macro by
/// `define macro` in its own file).
///
/// As the parser encounters in-source `define macro <name>` items,
/// it extends its OWN copy of the set so later items in the same
/// module can call the macro.
///
/// Sprint 51e.5 — this is the **dispatcher**. If a parse override has
/// been installed via [`set_parse_override`] (the Dylan-side parser,
/// JIT-strapped in by `nod-driver`'s `--parse-with-dylan`), dispatch to
/// it; otherwise call the canonical [`parse_module_with_macros_rust`].
/// Mirrors how [`crate::lexer::lex`] dispatches to its `LEX_OVERRIDE` or
/// `lex_rust`. With no override installed, the path is identical to the
/// pre-51e.5 behaviour.
pub fn parse_module_with_macros(
    src: &str,
    tokens: &[Token],
    preamble: Option<&Preamble>,
    seed_macros: &HashSet<String>,
) -> Result<Module, Vec<Diagnostic>> {
    if let Some(&f) = PARSE_OVERRIDE.get() {
        return f(src, tokens, preamble, seed_macros);
    }
    parse_module_with_macros_rust(src, tokens, preamble, seed_macros)
}

/// Signature of an alternate `parse_module_with_macros` implementation
/// that can be installed at runtime via [`set_parse_override`]. Must
/// match [`parse_module_with_macros_rust`] semantically: it receives the
/// same `(src, tokens, preamble, seed_macros)` and returns the same
/// `Result<Module, Vec<Diagnostic>>`. The Dylan-side implementation
/// (`nod-driver::install_dylan_parse_override`) ignores `tokens`,
/// `preamble`, and `seed_macros` (it re-lexes `src` inside the shim) and
/// falls back to [`parse_module_with_macros_rust`] for any file it can't
/// translate, so the result is never wrong — only "Dylan-translated" or
/// "Rust-fallback".
pub type ParseFn =
    fn(&str, &[Token], Option<&Preamble>, &HashSet<String>) -> Result<Module, Vec<Diagnostic>>;

static PARSE_OVERRIDE: std::sync::OnceLock<ParseFn> = std::sync::OnceLock::new();

/// Sprint 51e.5 — install an alternate `parse_module_with_macros`
/// implementation. Subsequent calls to [`parse_module_with_macros`]
/// dispatch through it; calls to [`parse_module_with_macros_rust`]
/// remain unaffected (the verify oracle and the Dylan path's own
/// fall-back use that as the reference path).
///
/// **Install-once.** A `OnceLock` backs the slot, so the first caller
/// wins. Re-installing a different function returns `Err(existing)` per
/// the standard `OnceLock` semantics. The driver installs at startup
/// (after parsing `--parse-with-dylan`) and never replaces — mirrors the
/// "load once, redirect from Rust parser" model of [`set_lex_override`].
pub fn set_parse_override(f: ParseFn) -> Result<(), ParseFn> {
    PARSE_OVERRIDE.set(f)
}

/// Returns `true` if an alternate `parse_module_with_macros`
/// implementation is currently installed. Used by the driver's
/// `--parse-with-dylan` status line.
pub fn has_parse_override() -> bool {
    PARSE_OVERRIDE.get().is_some()
}

/// The canonical (Rust) `parse_module_with_macros`. Always available;
/// [`parse_module_with_macros`] may dispatch to a Dylan-side override
/// but `parse_module_with_macros_rust` is the canonical fall-back and
/// the verify oracle's reference path.
///
/// The Dylan-side override (`nod-driver`) calls THIS directly for its
/// whole-file fall-back, deliberately NOT the dispatcher, so a fall-back
/// can't recurse back into the override.
pub fn parse_module_with_macros_rust(
    src: &str,
    tokens: &[Token],
    preamble: Option<&Preamble>,
    seed_macros: &HashSet<String>,
) -> Result<Module, Vec<Diagnostic>> {
    let header: Vec<(String, String)> = match preamble {
        Some(p) => p.entries.clone(),
        None => crate::lexer::scan_preamble(src)
            .map(|p| p.entries)
            .unwrap_or_default(),
    };

    let mut p = Parser::new(src, tokens, seed_macros);
    // Sprint 51e — `Precedence:` header pragma. Default is the DRM flat
    // rule (no precedence, left-associative). A legacy file written
    // against C-style precedence can opt in with `Precedence: c` in its
    // module header rather than being rewritten with explicit parens.
    // (A migration bridge — see docs/journal; the long-term goal is
    // flat everywhere, dropping the pragma.)
    if header
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("precedence") && v.trim().eq_ignore_ascii_case("c"))
    {
        p.precedence_c = true;
    }
    let mut items: Vec<Item> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();
    p.skip_trailing_semis();
    let module_lo = tokens.first().map(|t| t.span.lo).unwrap_or(0);
    let module_hi = tokens
        .iter()
        .rfind(|t| t.kind != TokenKind::Eof)
        .map(|t| t.span.hi)
        .unwrap_or(module_lo);
    let module_span = Span::new(
        tokens
            .first()
            .map(|t| t.span.file_id)
            .unwrap_or(crate::span::FileId(0)),
        module_lo,
        module_hi,
    );
    while !p.at_end() {
        match p.parse_top_item() {
            Ok(it) => items.push(it),
            Err(d) => {
                diags.push(d);
                p.recover_to_top_level();
            }
        }
        p.skip_trailing_semis();
    }
    if diags.is_empty() {
        Ok(Module {
            span: module_span,
            header,
            items,
        })
    } else {
        Err(diags)
    }
}

struct Parser<'a> {
    src: &'a str,
    tokens: &'a [Token],
    pos: usize,
    /// Sprint 25: names the parser should recognise as body-shaped
    /// macro call sites (`<name>(…) … end`). Seeded by the caller
    /// (typically `nod-sema` with the stdlib's macros), then extended
    /// in-place as `define macro <name>` items are parsed in the
    /// surrounding module so later items can use the macro.
    known_macros: HashSet<String>,
    /// Sprint 51e — operator-precedence mode. `false` (the default) is
    /// the DRM rule: all binary operators are one flat, left-associative
    /// level. `true` opts a file into legacy C-style precedence
    /// climbing, set from a `Precedence: c` module header. See
    /// [`Self::parse_binary`] / [`Self::parse_or`].
    precedence_c: bool,
    /// Monotone counter for synthetic `%select-key-N` binders introduced
    /// when desugaring `select (key …) …` into an `if`-tree. The key is
    /// evaluated ONCE into this binder; a fresh suffix per `select` keeps
    /// nested selects from colliding. See [`Self::parse_select`].
    select_counter: u32,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, tokens: &'a [Token], seed_macros: &HashSet<String>) -> Self {
        Self {
            src,
            tokens,
            pos: 0,
            known_macros: seed_macros.clone(),
            precedence_c: false,
            select_counter: 0,
        }
    }

    fn peek(&self) -> Token {
        self.tokens
            .get(self.pos)
            .copied()
            .unwrap_or_else(|| self.eof_token())
    }

    fn peek_kind(&self) -> TokenKind {
        self.peek().kind
    }

    fn eof_token(&self) -> Token {
        if let Some(last) = self.tokens.last() {
            *last
        } else {
            Token {
                kind: TokenKind::Eof,
                span: Span::new(crate::span::FileId(0), 0, 0),
            }
        }
    }

    fn bump(&mut self) -> Token {
        let t = self.peek();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn at_end(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }

    fn text(&self, span: Span) -> &str {
        &self.src[span.lo as usize..span.hi as usize]
    }

    fn token_text(&self, t: Token) -> &str {
        self.text(t.span)
    }

    fn diag(&self, span: Span, message: String) -> Diagnostic {
        Diagnostic { span, message }
    }

    fn skip_trailing_semis(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }
    }

    fn recover_to_semi(&mut self) {
        while !self.at_end() && !matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }
        if matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token, Diagnostic> {
        if self.peek_kind() == kind {
            Ok(self.bump())
        } else {
            let t = self.peek();
            Err(self.diag(
                t.span,
                format!("expected {what} ({:?}), got {:?}", kind, t.kind),
            ))
        }
    }

    fn expect_ident_keyword(&mut self, word: &str) -> Result<Token, Diagnostic> {
        let t = self.peek();
        if t.kind == TokenKind::Ident && self.token_text(t) == word {
            Ok(self.bump())
        } else {
            Err(self.diag(t.span, format!("expected `{word}`, got {:?}", t.kind)))
        }
    }

    fn ident_text_is(&self, t: Token, word: &str) -> bool {
        t.kind == TokenKind::Ident && self.token_text(t) == word
    }

    // ─── Expression entry ────────────────────────────────────────────────

    fn parse_expr_full(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_assign()
    }

    /// Assignment (`:=`) — right-assoc, lowest precedence. This is the
    /// only operator that climbs precedence in Dylan; everything below
    /// is one flat left-associative level (the DRM rule). See
    /// [`Self::parse_binary`].
    fn parse_assign(&mut self) -> Result<Expr, Diagnostic> {
        // Flat (DRM) by default; legacy `Precedence: c` files climb the
        // C-style precedence ladder (`parse_or` → … → `parse_pow`).
        let lhs = if self.precedence_c {
            self.parse_or()?
        } else {
            self.parse_binary()?
        };
        if matches!(self.peek_kind(), TokenKind::ColonEqual) {
            self.bump();
            let rhs = self.parse_assign()?;
            let span = join(lhs.span(), rhs.span());
            return Ok(Expr::BinOp {
                span,
                op: BinOp::Assign,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            });
        }
        Ok(lhs)
    }

    /// All binary operators — ONE flat, left-associative precedence
    /// level, per the Dylan Reference Manual. Dylan deliberately has no
    /// precedence among binary operators: `3 + 4 * 5` is `(3 + 4) * 5`
    /// (= 35), not `3 + (4 * 5)`. So `+ - * / ^ = == ~= ~== < > <= >= &
    /// | mod rem` all bind equally and group left to right. (`:=` is the
    /// one exception — right-assoc and looser — handled in
    /// [`Self::parse_assign`]; unary `-`/`~` bind tighter, in
    /// [`Self::parse_unary`].)
    ///
    /// This matches the Dylan-in-Dylan parser's `is-binary-op?` +
    /// flat `parse-expression` loop, so `--parse-with-dylan` agrees
    /// byte-for-byte. (Before Sprint 51e this climbed C-style
    /// precedence — a real bug that mis-parsed every mixed-operator
    /// expression.)
    fn parse_binary(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Bar => BinOp::Or,
                TokenKind::Amp => BinOp::And,
                TokenKind::Equal => BinOp::Eq,
                TokenKind::EqualEqual => BinOp::EqEq,
                TokenKind::TildeEqual => BinOp::Ne,
                TokenKind::TildeEqualEqual => BinOp::NeEq,
                TokenKind::Less => BinOp::Lt,
                TokenKind::Greater => BinOp::Gt,
                TokenKind::LessEqual => BinOp::Le,
                TokenKind::GreaterEqual => BinOp::Ge,
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Caret => BinOp::Pow,
                TokenKind::Ident => {
                    let t = self.peek();
                    match self.token_text(t) {
                        "mod" => BinOp::Mod,
                        "rem" => BinOp::Rem,
                        _ => break,
                    }
                }
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    // ── Legacy C-style precedence ladder (`Precedence: c` files only) ──
    // Retained so files written before the DRM-flat fix keep parsing
    // with their original grouping. New code uses `parse_binary` (flat).

    fn parse_or(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek_kind(), TokenKind::Bar) {
            self.bump();
            let rhs = self.parse_and()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_cmp()?;
        while matches!(self.peek_kind(), TokenKind::Amp) {
            self.bump();
            let rhs = self.parse_cmp()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_add()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Equal => BinOp::Eq,
                TokenKind::EqualEqual => BinOp::EqEq,
                TokenKind::TildeEqual => BinOp::Ne,
                TokenKind::TildeEqualEqual => BinOp::NeEq,
                TokenKind::Less => BinOp::Lt,
                TokenKind::Greater => BinOp::Gt,
                TokenKind::LessEqual => BinOp::Le,
                TokenKind::GreaterEqual => BinOp::Ge,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_add()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_add(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_mul()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_mul()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, Diagnostic> {
        let mut lhs = self.parse_pow()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Ident => {
                    let t = self.peek();
                    match self.token_text(t) {
                        "mod" => BinOp::Mod,
                        "rem" => BinOp::Rem,
                        _ => break,
                    }
                }
                _ => break,
            };
            self.bump();
            let rhs = self.parse_pow()?;
            let span = join(lhs.span(), rhs.span());
            lhs = Expr::BinOp {
                span,
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// Exponentiation — right-assoc (legacy C-mode only).
    fn parse_pow(&mut self) -> Result<Expr, Diagnostic> {
        let lhs = self.parse_unary()?;
        if matches!(self.peek_kind(), TokenKind::Caret) {
            self.bump();
            let rhs = self.parse_pow()?;
            let span = join(lhs.span(), rhs.span());
            return Ok(Expr::BinOp {
                span,
                op: BinOp::Pow,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            });
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        match self.peek_kind() {
            TokenKind::Minus => {
                let op_tok = self.bump();
                let inner = self.parse_unary()?;
                let span = join(op_tok.span, inner.span());
                Ok(Expr::UnOp {
                    span,
                    op: UnOp::Neg,
                    operand: Box::new(inner),
                })
            }
            TokenKind::Tilde => {
                let op_tok = self.bump();
                let inner = self.parse_unary()?;
                let span = join(op_tok.span, inner.span());
                Ok(Expr::UnOp {
                    span,
                    op: UnOp::Not,
                    operand: Box::new(inner),
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut e = self.parse_atom()?;
        loop {
            match self.peek_kind() {
                TokenKind::LParen => {
                    self.bump();
                    let args = self.parse_arg_list()?;
                    let close = self.expect(TokenKind::RParen, "`)`")?;
                    let span = join(e.span(), close.span);
                    e = Expr::Call {
                        span,
                        callee: Box::new(e),
                        args,
                    };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let mut args = Vec::new();
                    if !matches!(self.peek_kind(), TokenKind::RBracket) {
                        loop {
                            args.push(self.parse_expr_full()?);
                            if matches!(self.peek_kind(), TokenKind::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    let close = self.expect(TokenKind::RBracket, "`]`")?;
                    let span = join(e.span(), close.span);
                    // Lower `a[i,j]` to `element(a, i, j)` so the AST has one
                    // call shape. Pretty-printer prints it back as a call.
                    let mut all = vec![e];
                    all.extend(args);
                    e = Expr::Call {
                        span,
                        callee: Box::new(Expr::Ident(span, "element".into())),
                        args: all,
                    };
                }
                TokenKind::Dot => {
                    // `x.slot` — slot/method access. Lower to `slot(x)`.
                    self.bump();
                    let name_tok = match self.peek_kind() {
                        TokenKind::Ident => self.bump(),
                        _ => {
                            return Err(self.diag(
                                self.peek().span,
                                format!("expected identifier after `.`, got {:?}", self.peek_kind()),
                            ));
                        }
                    };
                    let span = join(e.span(), name_tok.span);
                    let name = self.token_text(name_tok).to_string();
                    e = Expr::Call {
                        span,
                        callee: Box::new(Expr::Ident(name_tok.span, name)),
                        args: vec![e],
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut args = Vec::new();
        if matches!(self.peek_kind(), TokenKind::RParen) {
            return Ok(args);
        }
        loop {
            // Keyword argument: `foo: <expr>` — eat the keyword and the value
            // as one expression. We synthesise a call to a `%kw-arg` helper
            // so it round-trips. (The `%`-prefix is a normal-identifier
            // start char per spec §3.1, so the pretty-printer's output
            // re-lexes cleanly.)
            if matches!(self.peek_kind(), TokenKind::KeywordColon) {
                let kw_tok = self.bump();
                let raw = self.token_text(kw_tok).to_string();
                let value = self.parse_expr_full()?;
                let span = join(kw_tok.span, value.span());
                args.push(Expr::Call {
                    span,
                    callee: Box::new(Expr::Ident(kw_tok.span, "%kw-arg".into())),
                    args: vec![Expr::Symbol(kw_tok.span, raw), value],
                });
            } else {
                args.push(self.parse_expr_full()?);
            }
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(args)
    }

    fn parse_atom(&mut self) -> Result<Expr, Diagnostic> {
        let t = self.peek();
        match t.kind {
            TokenKind::Integer => {
                self.bump();
                let raw = self.token_text(t);
                let val = parse_int_lit(raw, 10)
                    .ok_or_else(|| self.diag(t.span, format!("malformed integer `{raw}`")))?;
                Ok(Expr::Integer(t.span, val))
            }
            TokenKind::IntegerBin => {
                self.bump();
                let raw = self.token_text(t);
                let val = parse_int_lit(&raw[2..], 2)
                    .ok_or_else(|| self.diag(t.span, format!("malformed binary `{raw}`")))?;
                Ok(Expr::Integer(t.span, val))
            }
            TokenKind::IntegerOct => {
                self.bump();
                let raw = self.token_text(t);
                let val = parse_int_lit(&raw[2..], 8)
                    .ok_or_else(|| self.diag(t.span, format!("malformed octal `{raw}`")))?;
                Ok(Expr::Integer(t.span, val))
            }
            TokenKind::IntegerHex => {
                self.bump();
                let raw = self.token_text(t);
                let val = parse_int_lit(&raw[2..], 16)
                    .ok_or_else(|| self.diag(t.span, format!("malformed hex `{raw}`")))?;
                Ok(Expr::Integer(t.span, val))
            }
            TokenKind::Float => {
                self.bump();
                let raw = self.token_text(t);
                let cleaned = strip_float_suffix(raw);
                let v: f64 = cleaned
                    .parse()
                    .map_err(|_| self.diag(t.span, format!("malformed float `{raw}`")))?;
                Ok(Expr::Float(t.span, v))
            }
            TokenKind::String | TokenKind::StringRaw | TokenKind::StringMulti => {
                self.bump();
                let mut raw = self.token_text(t).to_string();
                let mut span = t.span;
                // Adjacent string-literal folding (DRM): consecutive plain
                // `"..."` literals concatenate into one constant, e.g.
                //   format-out("part one\n"
                //              "part two\n", ...)
                // Splice by dropping our trailing quote and the next leading
                // quote so escapes stay raw for decode_dylan_string_literal.
                if matches!(t.kind, TokenKind::String) {
                    while matches!(self.peek_kind(), TokenKind::String) && raw.ends_with('"') {
                        let nt = self.peek();
                        let next_raw = self.token_text(nt).to_string();
                        if !next_raw.starts_with('"') {
                            break;
                        }
                        self.bump();
                        raw.truncate(raw.len() - 1);
                        raw.push_str(&next_raw[1..]);
                        span = join(span, nt.span);
                    }
                }
                Ok(Expr::String(span, raw))
            }
            TokenKind::Char => {
                self.bump();
                let raw = self.token_text(t);
                let c = parse_char_lit(raw).unwrap_or('\u{FFFD}');
                Ok(Expr::Char(t.span, c))
            }
            TokenKind::HashTrue => {
                self.bump();
                Ok(Expr::Bool(t.span, true))
            }
            TokenKind::HashFalse => {
                self.bump();
                Ok(Expr::Bool(t.span, false))
            }
            TokenKind::Symbol => {
                self.bump();
                let raw = self.token_text(t);
                Ok(Expr::Symbol(t.span, raw.to_string()))
            }
            TokenKind::HashKeyword => {
                self.bump();
                let raw = self.token_text(t);
                Ok(Expr::Symbol(t.span, raw.to_string()))
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr_full()?;
                let close = self.expect(TokenKind::RParen, "`)`")?;
                let span = join(t.span, close.span);
                Ok(Expr::Paren {
                    span,
                    inner: Box::new(inner),
                })
            }
            TokenKind::Ident => {
                let word = self.token_text(t).to_string();
                match word.as_str() {
                    "if" => self.parse_if(),
                    "begin" => self.parse_begin(),
                    "let" => self.parse_let_expr_compat(),
                    "local" => self.parse_local_expr_compat(),
                    "method" => self.parse_method(),
                    "case" => self.parse_case(),
                    "select" => self.parse_select(),
                    "for" => {
                        let s = self.parse_for()?;
                        Ok(Expr::Stmt(Box::new(s)))
                    }
                    "while" => {
                        let s = self.parse_while()?;
                        Ok(Expr::Stmt(Box::new(s)))
                    }
                    "until" => {
                        let s = self.parse_until()?;
                        Ok(Expr::Stmt(Box::new(s)))
                    }
                    "block" => {
                        let s = self.parse_block()?;
                        Ok(Expr::Stmt(Box::new(s)))
                    }
                    _ if (self.known_macros.contains(&word)
                        || Self::is_block_opener_kw(&word))
                        && self.peek_after_ident_is_macro_call_shape() =>
                    {
                        self.parse_body_shaped_macro_call(t, word)
                    }
                    _ => {
                        self.bump();
                        Ok(Expr::Ident(t.span, word))
                    }
                }
            }
            TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                self.parse_hash_literal()
            }
            TokenKind::EscapedIdent => {
                self.bump();
                let raw = self.token_text(t);
                let name = raw.strip_prefix('\\').unwrap_or(raw).to_string();
                Ok(Expr::Ident(t.span, name))
            }
            _ => Err(self.diag(t.span, format!("unexpected token {:?}", t.kind))),
        }
    }

    // ─── Forms ──────────────────────────────────────────────────────────

    fn parse_if(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        self.expect(TokenKind::LParen, "`(` after `if`")?;
        let cond = self.parse_expr_full()?;
        self.expect(TokenKind::RParen, "`)` after `if` condition")?;
        let (then_body, else_body, end_tok) = self.parse_if_branches()?;
        let then_span = body_span(&then_body, kw.span);
        let then_ = Expr::Begin {
            span: then_span,
            body: then_body,
        };
        let else_ = else_body.map(|b| {
            let span = body_span(&b, kw.span);
            Box::new(Expr::Begin { span, body: b })
        });
        Ok(Expr::If {
            span: join(kw.span, end_tok.span),
            cond: Box::new(cond),
            then_: Box::new(then_),
            else_,
        })
    }

    fn parse_if_branches(&mut self) -> Result<IfBranches, Diagnostic> {
        let mut then_body = Vec::new();
        let mut else_body: Option<Vec<Expr>> = None;
        loop {
            let t = self.peek();
            if t.kind == TokenKind::KwEnd {
                let end_tok = self.bump();
                self.consume_optional_kw("if");
                return Ok((then_body, else_body, end_tok));
            }
            if self.ident_text_is(t, "else") {
                self.bump();
                else_body = Some(self.parse_else_body()?);
                continue;
            }
            if self.ident_text_is(t, "elseif") {
                self.bump();
                self.expect(TokenKind::LParen, "`(` after `elseif`")?;
                let cond = self.parse_expr_full()?;
                self.expect(TokenKind::RParen, "`)` after `elseif` condition")?;
                let (inner_then, inner_else, end_tok) = self.parse_if_branches()?;
                let then_span = body_span(&inner_then, end_tok.span);
                let then_ = Expr::Begin {
                    span: then_span,
                    body: inner_then,
                };
                let else_ = inner_else.map(|b| {
                    let span = body_span(&b, end_tok.span);
                    Box::new(Expr::Begin { span, body: b })
                });
                let nested = Expr::If {
                    span: join(t.span, end_tok.span),
                    cond: Box::new(cond),
                    then_: Box::new(then_),
                    else_,
                };
                return Ok((then_body, Some(vec![nested]), end_tok));
            }
            then_body.push(self.parse_expr_full()?);
            self.skip_trailing_semis();
        }
    }

    fn parse_else_body(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut body = Vec::new();
        loop {
            let t = self.peek();
            if t.kind == TokenKind::KwEnd {
                return Ok(body);
            }
            if self.ident_text_is(t, "else") || self.ident_text_is(t, "elseif") {
                return Ok(body);
            }
            body.push(self.parse_expr_full()?);
            self.skip_trailing_semis();
        }
    }

    /// Sprint 25: lookahead from the *current* ident token. Returns
    /// `true` iff what follows looks like a body-shaped macro call
    /// — `(head…) <body-content> end` with at least one non-trivial
    /// body token between `)` and `end`.
    ///
    /// Disambiguation from call-shape macros: a macro defined as
    /// `{ forever ?x:expression }` (no body) is called as
    /// `forever(1)` and the next token is whatever follows. If
    /// that "next" is a continuation token (binop, comma,
    /// semicolon, closer, dot, arrow, assign) we treat the form
    /// as a normal `Expr::Call` — the macro engine still picks it
    /// up via the `Call(Ident, args)` recognition path. The empty
    /// body case (`<name>(head) end`) currently isn't recognised
    /// by this lookahead; it's not a shape any in-tree macro uses.
    fn peek_after_ident_is_macro_call_shape(&self) -> bool {
        // Position immediately past the macro-name ident.
        let mut i = self.pos + 1;
        // Sprint 25 v1: only recognise the parenthesised-head shape.
        // `<name> body end` without a head paren ambiguates badly
        // with `<name>` as an expression followed by a statement,
        // so the head paren is required.
        //
        // Sprint N extension: also accept the no-head-paren shape when
        // the tokens directly contain a closing `end` at depth 0.
        // `end` at depth 0 can ONLY appear as a closing form delimiter,
        // never as a standalone statement, so the presence of a reachable
        // `end` unambiguously identifies a body-shaped call.  This unlocks
        // macros like `with-cleanup body cleanup cleanup-body end` that
        // have no parenthesised condition before the body.
        if self.tokens.get(i).map(|t| t.kind) != Some(TokenKind::LParen) {
            // No-paren path: scan forward for a balancing `end`.
            let mut depth = 0i32;
            let mut saw_body_content = false;
            while let Some(t) = self.tokens.get(i) {
                match t.kind {
                    TokenKind::LParen
                    | TokenKind::LBracket
                    | TokenKind::LBrace
                    | TokenKind::HashLParen
                    | TokenKind::HashLBracket
                    | TokenKind::HashLBrace => {
                        depth += 1;
                        saw_body_content = true;
                    }
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                        if depth == 0 {
                            return false;
                        }
                        depth -= 1;
                    }
                    TokenKind::KwEnd if depth == 0 => return saw_body_content,
                    TokenKind::Semicolon if depth == 0 => {
                        // Semicolons separate body statements — stay.
                    }
                    TokenKind::Eof => return false,
                    _ => {
                        saw_body_content = true;
                    }
                }
                i += 1;
            }
            return false;
        }
        i += 1;
        let mut depth = 1i32;
        while let Some(t) = self.tokens.get(i) {
            match t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                | TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                    depth += 1;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        // `i` now points to the first token AFTER `)`. If it's a
        // continuation token, this is a normal call.
        let next = match self.tokens.get(i).map(|t| t.kind) {
            Some(k) => k,
            None => return false,
        };
        if is_call_continuation(next) {
            return false;
        }
        // Now scan body content: require at least one significant
        // token before the matching `end` at depth 0.
        let mut depth = 0i32;
        let mut saw_body_content = false;
        while let Some(t) = self.tokens.get(i) {
            match t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                | TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                    depth += 1;
                    saw_body_content = true;
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                TokenKind::KwEnd if depth == 0 => return saw_body_content,
                TokenKind::Semicolon if depth == 0 => {
                    // Stay; semicolons separate body statements.
                }
                TokenKind::Eof => return false,
                _ => {
                    saw_body_content = true;
                }
            }
            i += 1;
        }
        false
    }

    /// Sprint 25: parse `<name>(head…) body… end` (or `<name> body… end`)
    /// into an `Expr::MacroCall`. The macro engine re-lexes the span
    /// to do fragment-level pattern matching, so we only need to
    /// capture the source extent here — not the head's internal
    /// structure (which is macro-pattern-specific and can't be
    /// AST'd at the parser layer).
    fn parse_body_shaped_macro_call(
        &mut self,
        name_tok: Token,
        name: String,
    ) -> Result<Expr, Diagnostic> {
        // Consume the macro name.
        self.bump();
        // Skip the head group (paren / bracket / brace) if present.
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.bump();
            let mut depth = 1i32;
            while !self.at_end() {
                match self.peek_kind() {
                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                    | TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                        depth += 1;
                        self.bump();
                    }
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                        depth -= 1;
                        self.bump();
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        }
        // Skip body tokens up to the matching `end` that closes THIS macro.
        // The body is opaque to the parser — macro pattern matching sees it as
        // raw fragments via `call_site_fragments`. Track BOTH grouping depth
        // (`()[]{}`) and nested end-terminated block depth, so a nested
        // `if`/`for`/`begin`/body-macro (each closing with its own `end`, often
        // a bare `end;`) is not mistaken for this macro's terminator.
        let mut depth = 0i32; // () [] {} grouping
        let mut bdepth = 0i32; // nested end-terminated blocks
        let end_tok = loop {
            if self.at_end() {
                return Err(self.diag(
                    name_tok.span,
                    format!(
                        "macro call `{name}` is missing its closing `end` keyword"
                    ),
                ));
            }
            match self.peek_kind() {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                | TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        // Shouldn't happen given peek_after_ident_is_macro_call_shape
                        // succeeded, but guard for robustness.
                        return Err(self.diag(
                            self.peek().span,
                            format!(
                                "macro call `{name}`: unbalanced closing delimiter"
                            ),
                        ));
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::KwEnd => {
                    if depth > 0 {
                        // literal `end` inside a grouping (e.g. a macro rule)
                        self.bump();
                    } else if bdepth > 0 {
                        // closes a nested block
                        bdepth -= 1;
                        self.bump();
                        // skip an optional `end <keyword>` echo
                        if matches!(self.peek_kind(), TokenKind::Ident)
                            && Self::is_block_opener_kw(self.token_text(self.peek()))
                        {
                            self.bump();
                        }
                    } else {
                        // closes THIS macro
                        break self.bump();
                    }
                }
                TokenKind::Ident if depth == 0 => {
                    if Self::is_block_opener_kw(self.token_text(self.peek())) {
                        bdepth += 1;
                    }
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        };
        // Optional trailing form-name `end <name>;` — consume it.
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        Ok(Expr::MacroCall {
            span: join(name_tok.span, end_tok.span),
            name,
        })
    }

    fn parse_begin(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        let body = self.parse_body_until_end()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end`")?;
        self.consume_optional_kw("begin");
        Ok(Expr::Begin {
            span: join(kw.span, end_tok.span),
            body,
        })
    }

    /// Expression-position `let` retains the Sprint 03 surface (single binder)
    /// for back-compat with `parse_expr` callers. Statement position uses
    /// `parse_let_stmt` for multi-binders.
    fn parse_let_expr_compat(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        if matches!(self.peek_kind(), TokenKind::LParen) {
            // Multi-binder: wrap as Stmt expr.
            let s = self.finish_let_stmt(kw, true)?;
            return Ok(Expr::Stmt(Box::new(s)));
        }
        let name_tok = self.expect(TokenKind::Ident, "binder name")?;
        let binder = self.token_text(name_tok).to_string();
        if matches!(self.peek_kind(), TokenKind::ColonColon) {
            self.bump();
            let _ = self.parse_postfix()?;
        }
        self.expect(TokenKind::Equal, "`=` after let binder")?;
        let value = self.parse_expr_full()?;
        let span = join(kw.span, value.span());
        Ok(Expr::Let {
            span,
            binder,
            value: Box::new(value),
        })
    }

    fn parse_local_expr_compat(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        // `local method f … end method, method g … end method` — multi.
        // We always parse as multi to handle either single or sequence.
        let methods = self.parse_local_methods()?;
        let span = match methods.last() {
            Some(m) => join(kw.span, m.span),
            None => kw.span,
        };
        // Always emit `Expr::Stmt(Statement::Local)` — including the
        // single-method case. `Statement::Local` is the ONLY local-method
        // shape the lowering's lift pre-pass recognises (`lift_statement`
        // hoists it to a top-level function and wires up the self/mutual
        // recursion cells); a bare `Expr::LocalMethod` in expression
        // position is rejected by lowering ("expression form `local-method`
        // not lowered"). Wrapping the single case the same way the
        // multi-method case already is lets a `begin`-bodied macro
        // expansion — e.g. `iterate NAME (…) … end` lowering to
        // `begin local method NAME (…) … end; NAME(…) end` — lower
        // correctly. (Previously the single case produced `Expr::LocalMethod`
        // for a now-defunct "Sprint 03 callers expect that" contract; no
        // live lowering path consumes that variant successfully.)
        Ok(Expr::Stmt(Box::new(Statement::Local {
            span,
            methods,
        })))
    }

    /// `select (key [by test]) v1, v2 => body; … otherwise => body; end select`
    ///
    /// Desugared HERE into an `if`/`elseif`-tree — the frozen kernel
    /// primitive — so every downstream pass (lift, free-var capture,
    /// lowering, AOT codegen) sees only `Expr::If` and needs no
    /// `Expr::Case` support. This is the same expansion a `define macro`
    /// would produce; the macro engine can't yet express the surface
    /// (`*`-repetition of `;`-separated, comma-value, multi-statement
    /// arms + an `end select` keyword), so the desugaring lives where the
    /// structured arm data is already in hand.
    ///
    /// Shape:
    ///   `begin let %select-key-N = <key>;
    ///      if (TEST(%select-key-N, v1) | TEST(%select-key-N, v2)) body1
    ///      elseif (…) body2 …
    ///      else <otherwise-or-#f> end
    ///    end`
    /// where TEST is `=` by default, or `f(key, v)` for `select (k by f)`.
    /// The key is evaluated EXACTLY ONCE (bound to the synthetic binder).
    fn parse_select(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        self.expect(TokenKind::LParen, "`(` after `select`")?;
        let key = self.parse_expr_full()?;
        // Optional `by <test>` — the membership test function. Absent ⇒ `=`.
        let mut test: Option<Expr> = None;
        if let t = self.peek()
            && self.ident_text_is(t, "by")
        {
            self.bump();
            test = Some(self.parse_expr_full()?);
        }
        self.expect(TokenKind::RParen, "`)` in select head")?;

        // Fresh, unique binder for the once-evaluated key.
        let n = self.select_counter;
        self.select_counter += 1;
        let key_name = format!("%select-key-{n}");
        let key_span = key.span();

        // Collect arms as (value-list, body) and an optional otherwise.
        let mut arms: Vec<(Vec<Expr>, Vec<Expr>)> = Vec::new();
        let mut otherwise: Option<Vec<Expr>> = None;
        let end_tok;
        loop {
            let t = self.peek();
            if t.kind == TokenKind::KwEnd {
                end_tok = self.bump();
                self.consume_optional_kw("select");
                break;
            }
            if t.kind == TokenKind::KwOtherwise {
                self.bump();
                self.expect(TokenKind::Arrow, "`=>` after `otherwise`")?;
                otherwise = Some(self.parse_case_arm_body()?);
                continue;
            }
            // value [, value]* => body
            let mut values = vec![self.parse_expr_full()?];
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
                values.push(self.parse_expr_full()?);
            }
            self.expect(TokenKind::Arrow, "`=>` after select arm value(s)")?;
            let body = self.parse_case_arm_body()?;
            arms.push((values, body));
        }

        let whole = join(kw.span, end_tok.span);
        // Build the membership predicate for one arm: OR over each value of
        // `TEST(key, value)`.
        let make_pred = |values: &[Expr]| -> Expr {
            let mut it = values.iter();
            let first = it.next().expect("select arm has >=1 value");
            let mut acc = select_test_expr(&key_name, key_span, test.as_ref(), first);
            for v in it {
                let rhs = select_test_expr(&key_name, key_span, test.as_ref(), v);
                acc = Expr::BinOp {
                    span: whole,
                    op: BinOp::Or,
                    lhs: Box::new(acc),
                    rhs: Box::new(rhs),
                };
            }
            acc
        };
        let preds: Vec<(Expr, Vec<Expr>)> = arms
            .into_iter()
            .map(|(vals, body)| (make_pred(&vals), body))
            .collect();
        let if_tree = build_if_tree(preds, otherwise, whole);

        // Wrap in `begin let %select-key-N = <key>; <if-tree> end` so the key
        // is evaluated once.
        let let_stmt = Expr::Let {
            span: key_span,
            binder: key_name,
            value: Box::new(key),
        };
        Ok(Expr::Begin {
            span: whole,
            body: vec![let_stmt, if_tree],
        })
    }

    fn parse_hash_literal(&mut self) -> Result<Expr, Diagnostic> {
        // `#(...)`, `#[...]`, `#{...}` — literal lists/vectors/sets. We
        // accept them as a Call to a synthetic name so the round-trip
        // tests work; full literal modelling is a Sprint 06 concern.
        let open = self.bump();
        let (close_kind, name) = match open.kind {
            TokenKind::HashLParen => (TokenKind::RParen, "#list"),
            TokenKind::HashLBracket => (TokenKind::RBracket, "#vector"),
            TokenKind::HashLBrace => (TokenKind::RBrace, "#set"),
            _ => unreachable!(),
        };
        let mut args = Vec::new();
        if self.peek_kind() != close_kind {
            loop {
                args.push(self.parse_expr_full()?);
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.bump();
                    if self.peek_kind() == close_kind {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let close = self.expect(close_kind, "closing hash-literal")?;
        Ok(Expr::Call {
            span: join(open.span, close.span),
            callee: Box::new(Expr::Ident(open.span, name.to_string())),
            args,
        })
    }

    fn parse_method(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        let params = self.parse_param_list_loose()?;
        // Optional `=> (return-sig)` — for anonymous methods. We parse and
        // discard the type info at expression level.
        let _ret = self.maybe_return_sig()?;
        if matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }
        let body = self.parse_body_until_end()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end`")?;
        self.consume_optional_kw("method");
        Ok(Expr::Method {
            span: join(kw.span, end_tok.span),
            params,
            body,
        })
    }

    /// `case test1 => body1; test2 => body2; … otherwise => bodyN; end case`
    ///
    /// Desugared into an `if`/`elseif`-tree (see [`Self::parse_select`] for
    /// the rationale). Each arm's `test` is a boolean expression used
    /// verbatim as the `if` condition — no key, no membership test.
    fn parse_case(&mut self) -> Result<Expr, Diagnostic> {
        let kw = self.bump();
        let mut arms: Vec<(Expr, Vec<Expr>)> = Vec::new();
        let mut otherwise: Option<Vec<Expr>> = None;
        let end_tok;
        loop {
            let t = self.peek();
            if t.kind == TokenKind::KwEnd {
                end_tok = self.bump();
                self.consume_optional_kw("case");
                break;
            }
            if t.kind == TokenKind::KwOtherwise {
                self.bump();
                self.expect(TokenKind::Arrow, "`=>` after `otherwise`")?;
                otherwise = Some(self.parse_case_arm_body()?);
                continue;
            }
            let cond = self.parse_expr_full()?;
            self.expect(TokenKind::Arrow, "`=>` after case condition")?;
            let body = self.parse_case_arm_body()?;
            arms.push((cond, body));
        }
        let whole = join(kw.span, end_tok.span);
        Ok(build_if_tree(arms, otherwise, whole))
    }

    fn parse_case_arm_body(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut body = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd | TokenKind::KwOtherwise) {
                return Ok(body);
            }
            // Empty consequent: `value => ;` or `value =>` immediately before
            // the next arm / `end`. A leading `;` with nothing before it
            // means this arm has no body (Dylan: the arm yields `#f`).
            if matches!(t.kind, TokenKind::Semicolon) {
                self.bump();
                return Ok(body);
            }
            body.push(self.parse_expr_full()?);
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
                let t = self.peek();
                if matches!(t.kind, TokenKind::KwEnd | TokenKind::KwOtherwise) {
                    return Ok(body);
                }
                // Look ahead: an arm ends when we see `<expr> =>` before `;`.
                if self.next_is_arm_head() {
                    return Ok(body);
                }
                continue;
            }
            return Ok(body);
        }
    }

    fn next_is_arm_head(&self) -> bool {
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.tokens.len() {
            let k = self.tokens[i].kind;
            match k {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                TokenKind::Semicolon if depth == 0 => return false,
                TokenKind::KwEnd | TokenKind::KwOtherwise if depth == 0 => return false,
                TokenKind::Arrow if depth == 0 => return true,
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn parse_body_until_end(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut body = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd) {
                return Ok(body);
            }
            if self.ident_text_is(t, "else") || self.ident_text_is(t, "elseif") {
                return Ok(body);
            }
            if matches!(t.kind, TokenKind::Eof) {
                return Err(self.diag(t.span, "unexpected end-of-file in body".to_string()));
            }
            body.push(self.parse_expr_full()?);
            self.skip_trailing_semis();
        }
    }

    fn consume_optional_kw(&mut self, word: &str) {
        let t = self.peek();
        if self.ident_text_is(t, word) {
            self.bump();
        }
    }

    // ─── Sprint 04 — statements ────────────────────────────────────────

    /// Parse one statement. Statements are: `let`/`local`/`for`/`while`/
    /// `until`/`block` (each with a body) or a bare expression.
    fn parse_statement(&mut self) -> Result<Statement, Diagnostic> {
        let t = self.peek();
        if t.kind == TokenKind::Ident {
            let word = self.token_text(t);
            match word {
                "let" => {
                    let kw = self.bump();
                    return self.finish_let_stmt(kw, false);
                }
                "local" => {
                    let kw = self.bump();
                    let methods = self.parse_local_methods()?;
                    let span = match methods.last() {
                        Some(m) => join(kw.span, m.span),
                        None => kw.span,
                    };
                    return Ok(Statement::Local { span, methods });
                }
                "for" => return self.parse_for(),
                "while" => return self.parse_while(),
                "until" => return self.parse_until(),
                "block" => return self.parse_block(),
                _ => {}
            }
        }
        let e = self.parse_expr_full()?;
        Ok(Statement::Expr(e))
    }

    /// Parse a sequence of statements up to `end`/`else`/`elseif`/`finally`/
    /// `cleanup`/`exception`/`afterwards`/EOF. Semicolons separate statements
    /// but are tolerated as omitted before a body terminator.
    fn parse_stmt_body(&mut self) -> Result<Vec<Statement>, Diagnostic> {
        let mut body = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd | TokenKind::Eof) {
                return Ok(body);
            }
            if self.is_body_terminator(t) {
                return Ok(body);
            }
            // If a bare Ident-call-with-body (statement macro) leaves us
            // looking at a new statement-start without a `;`, we still
            // continue — Dylan's reader treats undelimited statement
            // macros' bodies as separate from the surrounding sequence.
            let before = self.pos;
            body.push(self.parse_statement()?);
            // If a non-`;` non-terminator follows, the previous statement
            // was likely the head of a macro form whose body was already
            // consumed as nested expressions; keep parsing.
            match self.peek_kind() {
                TokenKind::Semicolon => {
                    self.bump();
                    continue;
                }
                TokenKind::KwEnd | TokenKind::Eof => return Ok(body),
                _ => {
                    if self.is_body_terminator(self.peek()) {
                        return Ok(body);
                    }
                    // Guard against infinite loop: if parse_statement did not
                    // advance, force-advance.
                    if self.pos == before {
                        self.bump();
                    }
                    continue;
                }
            }
        }
    }

    fn is_body_terminator(&self, t: Token) -> bool {
        if t.kind != TokenKind::Ident {
            return false;
        }
        matches!(
            self.token_text(t),
            "else"
                | "elseif"
                | "finally"
                | "cleanup"
                | "exception"
                | "afterwards"
        )
    }

    fn finish_let_stmt(&mut self, kw: Token, want_value: bool) -> Result<Statement, Diagnostic> {
        let _ = want_value; // future use
        // multi-binder: `let (a, b) = …`
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.bump();
            let mut binders: Vec<Binder> = Vec::new();
            let mut rest: Option<Binder> = None;
            if !matches!(self.peek_kind(), TokenKind::RParen) {
                loop {
                    if matches!(self.peek_kind(), TokenKind::HashRest) {
                        self.bump();
                        let name_tok = self.expect(TokenKind::Ident, "#rest binder")?;
                        let name = self.token_text(name_tok).to_string();
                        rest = Some(Binder {
                            span: name_tok.span,
                            name,
                            type_: None,
                        });
                        break;
                    }
                    let name_tok = self.expect(TokenKind::Ident, "let binder name")?;
                    let name = self.token_text(name_tok).to_string();
                    let mut type_: Option<Expr> = None;
                    if matches!(self.peek_kind(), TokenKind::ColonColon) {
                        self.bump();
                        type_ = Some(self.parse_postfix()?);
                    }
                    let span = type_
                        .as_ref()
                        .map(|t| join(name_tok.span, t.span()))
                        .unwrap_or(name_tok.span);
                    binders.push(Binder { span, name, type_ });
                    if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, "`)` after let binders")?;
            self.expect(TokenKind::Equal, "`=` after let binders")?;
            let value = self.parse_expr_full()?;
            let span = join(kw.span, value.span());
            return Ok(Statement::Let {
                span,
                binders,
                rest,
                value,
            });
        }
        // single binder: `let x [:: T] = expr`
        let name_tok = self.expect(TokenKind::Ident, "let binder name")?;
        let name = self.token_text(name_tok).to_string();
        let mut type_: Option<Expr> = None;
        if matches!(self.peek_kind(), TokenKind::ColonColon) {
            self.bump();
            type_ = Some(self.parse_postfix()?);
        }
        // Dylan also has the variant `let handler <error> = handler-fn;` which
        // we accept by virtue of `let <error>` lexing as one identifier.
        self.expect(TokenKind::Equal, "`=` after let binder")?;
        let value = self.parse_expr_full()?;
        let span = join(kw.span, value.span());
        let binder_span = match &type_ {
            Some(t) => join(name_tok.span, t.span()),
            None => name_tok.span,
        };
        Ok(Statement::Let {
            span,
            binders: vec![Binder {
                span: binder_span,
                name,
                type_,
            }],
            rest: None,
            value,
        })
    }

    fn parse_local_methods(&mut self) -> Result<Vec<LocalMethodDecl>, Diagnostic> {
        // After `local` we may see `method NAME (...) ... end method[,]`
        // possibly followed by `method NAME ...` etc.
        let mut out = Vec::new();
        loop {
            self.expect_ident_keyword("method")?;
            let name_tok = self.expect(TokenKind::Ident, "local method name")?;
            let name = self.token_text(name_tok).to_string();
            let params = self.parse_param_list_loose()?;
            let return_ = self.maybe_return_sig()?;
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
            }
            let body = self.parse_stmt_body()?;
            let end_tok = self.expect(TokenKind::KwEnd, "`end` for local method")?;
            self.consume_optional_kw("method");
            // Optional method-name echo: `end method go-c`. Without consuming
            // it, a comma-separated `local method go-c () … end method go-c,
            // method go-d () … end` sequence would leave `go-c` (then `,`)
            // unconsumed and mis-parse the second method.
            if matches!(self.peek_kind(), TokenKind::Ident)
                && self.token_text(self.peek()) == name.as_str()
            {
                self.bump();
            }
            let span = join(name_tok.span, end_tok.span);
            out.push(LocalMethodDecl {
                span,
                name,
                params,
                return_,
                body,
            });
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
                continue;
            }
            break;
        }
        Ok(out)
    }

    fn parse_for(&mut self) -> Result<Statement, Diagnostic> {
        let kw = self.bump();
        self.expect(TokenKind::LParen, "`(` after `for`")?;
        let mut clauses: Vec<ForClause> = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                let c = self.parse_for_clause()?;
                clauses.push(c);
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.bump();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)` after for-clauses")?;
        let body = self.parse_stmt_body()?;
        let mut finally_: Vec<Statement> = Vec::new();
        if self.peek_ident_is("finally") {
            self.bump();
            finally_ = self.parse_stmt_body()?;
        }
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of `for`")?;
        self.consume_optional_kw("for");
        Ok(Statement::For {
            span: join(kw.span, end_tok.span),
            clauses,
            body,
            finally_,
        })
    }

    fn parse_for_clause(&mut self) -> Result<ForClause, Diagnostic> {
        // `var from EXPR [to|below|above EXPR] [by EXPR]`
        // `var in EXPR`
        // `var = INIT [then NEXT]`            (explicit-step)
        // `var keyed-by KEY in EXPR`          (keyed iteration)
        // `until: COND` / `while: COND`       (keyword terminator clauses)

        // Leading keyword clause with no variable: `until: cond` / `while: cond`.
        if matches!(self.peek_kind(), TokenKind::KeywordColon) {
            let kt = self.peek();
            let kw = self.token_text(kt).trim_end_matches(':').to_string();
            if kw == "until" || kw == "while" {
                self.bump();
                let cond = self.parse_expr_until_for_terminator()?;
                let span = join(kt.span, cond.span());
                return Ok(if kw == "while" {
                    ForClause::While { span, cond }
                } else {
                    ForClause::Until { span, cond }
                });
            }
        }

        let var_tok = self.expect(TokenKind::Ident, "for-clause variable")?;
        let var = self.token_text(var_tok).to_string();
        // Optional :: TYPE
        if matches!(self.peek_kind(), TokenKind::ColonColon) {
            self.bump();
            let _ = self.parse_postfix()?;
        }
        // Explicit-step clause: `var = init [then next]`.
        if matches!(self.peek_kind(), TokenKind::Equal) {
            self.bump();
            let init = self.parse_expr_until_for_terminator()?;
            let next = if self.peek_ident_is("then") {
                self.bump();
                Some(self.parse_expr_until_for_terminator()?)
            } else {
                None
            };
            let last = next
                .as_ref()
                .map(|e| e.span())
                .unwrap_or_else(|| init.span());
            let span = join(var_tok.span, last);
            return Ok(ForClause::Step(Box::new(StepForClause {
                span,
                var,
                init,
                next,
            })));
        }
        // Keyed iteration: `var keyed-by KEY [:: TYPE] in coll`.
        if self.peek_ident_is("keyed-by") {
            self.bump();
            let key_tok = self.expect(TokenKind::Ident, "key variable after `keyed-by`")?;
            let key = self.token_text(key_tok).to_string();
            if matches!(self.peek_kind(), TokenKind::ColonColon) {
                self.bump();
                let _ = self.parse_postfix()?;
            }
            self.expect_ident_keyword("in")?;
            let coll = self.parse_expr_until_for_terminator()?;
            let span = join(var_tok.span, coll.span());
            return Ok(ForClause::Keyed { span, var, key, coll });
        }
        if self.peek_ident_is("in") {
            self.bump();
            let coll = self.parse_expr_until_for_terminator()?;
            let span = join(var_tok.span, coll.span());
            return Ok(ForClause::In { span, var, coll });
        }
        if self.peek_ident_is("from") {
            self.bump();
            let from = self.parse_expr_until_for_terminator()?;
            let mut to: Option<Expr> = None;
            let mut below: Option<Expr> = None;
            let mut above: Option<Expr> = None;
            let mut by: Option<Expr> = None;
            loop {
                if self.peek_ident_is("to") {
                    self.bump();
                    to = Some(self.parse_expr_until_for_terminator()?);
                } else if self.peek_ident_is("below") {
                    self.bump();
                    below = Some(self.parse_expr_until_for_terminator()?);
                } else if self.peek_ident_is("above") {
                    self.bump();
                    above = Some(self.parse_expr_until_for_terminator()?);
                } else if self.peek_ident_is("by") {
                    self.bump();
                    by = Some(self.parse_expr_until_for_terminator()?);
                } else {
                    break;
                }
            }
            let last = above
                .as_ref()
                .or(below.as_ref())
                .or(to.as_ref())
                .or(by.as_ref())
                .map(|e| e.span())
                .unwrap_or(from.span());
            let span = join(var_tok.span, last);
            return Ok(ForClause::Numeric(Box::new(NumericForClause {
                span,
                var,
                from,
                to,
                below,
                above,
                by,
            })));
        }
        if self.peek_ident_is("until") {
            self.bump();
            let cond = self.parse_expr_until_for_terminator()?;
            let span = join(var_tok.span, cond.span());
            return Ok(ForClause::Until { span, cond });
        }
        if self.peek_ident_is("while") {
            self.bump();
            let cond = self.parse_expr_until_for_terminator()?;
            let span = join(var_tok.span, cond.span());
            return Ok(ForClause::While { span, cond });
        }
        // Bare `from` shorthand we don't model — accept as Numeric { from }.
        let span = var_tok.span;
        Ok(ForClause::From(Box::new(FromForClause {
            span,
            var,
            from: Expr::Ident(var_tok.span, "?".to_string()),
            by: None,
        })))
    }

    /// In a for-clause, an expression ends at `,` / `)` / `to` / `below` / etc.
    fn parse_expr_until_for_terminator(&mut self) -> Result<Expr, Diagnostic> {
        // The expression parser will stop at `,` / `)` naturally because they
        // are not infix operators. The Dylan keywords (`to`, `below`, …)
        // appear as Ident tokens that the parser will treat as the *start* of
        // a new identifier — but since we are mid-expression after an
        // operator/atom, a stray Ident terminates the expression at the
        // `parse_postfix` level. The simplest implementation: parse_expr_full
        // already stops at those points because no operator follows.
        self.parse_expr_full()
    }

    fn parse_while(&mut self) -> Result<Statement, Diagnostic> {
        let kw = self.bump();
        self.expect(TokenKind::LParen, "`(` after `while`")?;
        let cond = self.parse_expr_full()?;
        self.expect(TokenKind::RParen, "`)` after `while` condition")?;
        let body = self.parse_stmt_body()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of `while`")?;
        self.consume_optional_kw("while");
        Ok(Statement::While {
            span: join(kw.span, end_tok.span),
            cond,
            body,
        })
    }

    fn parse_until(&mut self) -> Result<Statement, Diagnostic> {
        let kw = self.bump();
        self.expect(TokenKind::LParen, "`(` after `until`")?;
        let cond = self.parse_expr_full()?;
        self.expect(TokenKind::RParen, "`)` after `until` condition")?;
        let body = self.parse_stmt_body()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of `until`")?;
        self.consume_optional_kw("until");
        Ok(Statement::Until {
            span: join(kw.span, end_tok.span),
            cond,
            body,
        })
    }

    fn parse_block(&mut self) -> Result<Statement, Diagnostic> {
        let kw = self.bump();
        let mut exit_var: Option<String> = None;
        if matches!(self.peek_kind(), TokenKind::LParen) {
            self.bump();
            if !matches!(self.peek_kind(), TokenKind::RParen) {
                let name_tok = self.expect(TokenKind::Ident, "exit-procedure name")?;
                exit_var = Some(self.token_text(name_tok).to_string());
            }
            self.expect(TokenKind::RParen, "`)` after block exit-var")?;
        }
        let body = self.parse_stmt_body()?;
        let mut handlers: Vec<ExceptionClause> = Vec::new();
        let mut cleanup: Vec<Statement> = Vec::new();
        let mut afterwards: Vec<Statement> = Vec::new();
        loop {
            if self.peek_ident_is("exception") {
                let kwx = self.bump();
                self.expect(TokenKind::LParen, "`(` after `exception`")?;
                let mut var: Option<String> = None;
                let class: Expr;
                // `(var :: TYPE)` or `(TYPE)`.
                if matches!(self.peek_kind(), TokenKind::Ident) {
                    let first_tok = self.peek();
                    // Could be a var name followed by `::`, or directly a type.
                    let save = self.pos;
                    self.bump();
                    if matches!(self.peek_kind(), TokenKind::ColonColon) {
                        self.bump();
                        var = Some(self.token_text(first_tok).to_string());
                        class = self.parse_postfix()?;
                    } else {
                        self.pos = save;
                        class = self.parse_expr_full()?;
                    }
                } else {
                    class = self.parse_expr_full()?;
                }
                // Optional `, condition: …, init-arguments: …` — skip rest until `)`.
                while !matches!(self.peek_kind(), TokenKind::RParen | TokenKind::Eof) {
                    self.bump();
                }
                self.expect(TokenKind::RParen, "`)` after exception clause head")?;
                let hbody = self.parse_stmt_body()?;
                let span = hbody
                    .last()
                    .map(|s| join(kwx.span, s.span()))
                    .unwrap_or(kwx.span);
                handlers.push(ExceptionClause {
                    span,
                    var,
                    class,
                    body: hbody,
                });
                continue;
            }
            if self.peek_ident_is("cleanup") {
                self.bump();
                cleanup = self.parse_stmt_body()?;
                continue;
            }
            if self.peek_ident_is("afterwards") {
                self.bump();
                afterwards = self.parse_stmt_body()?;
                continue;
            }
            break;
        }
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of `block`")?;
        self.consume_optional_kw("block");
        Ok(Statement::Block {
            span: join(kw.span, end_tok.span),
            exit_var,
            body,
            handlers,
            cleanup,
            afterwards,
        })
    }

    fn peek_ident_is(&self, word: &str) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Ident && self.token_text(t) == word
    }

    // ─── Sprint 04 — top-level items ───────────────────────────────────

    fn recover_to_top_level(&mut self) {
        // Skip to next `define` at depth 0, or EOF.
        let mut depth: i32 = 0;
        while !self.at_end() {
            match self.peek_kind() {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace if depth > 0 => {
                    depth -= 1;
                }
                TokenKind::KwDefine if depth == 0 => return,
                TokenKind::Semicolon if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn parse_top_item(&mut self) -> Result<Item, Diagnostic> {
        if !matches!(self.peek_kind(), TokenKind::KwDefine) {
            // Free-standing expression at top level.
            let e = self.parse_expr_full()?;
            return Ok(Item::Expr(e));
        }
        let define_tok = self.bump();
        let mut modifiers: Vec<Modifier> = Vec::new();
        // Collect adjective-modifiers: `define open sealed primary class …`.
        loop {
            let t = self.peek();
            if t.kind != TokenKind::Ident {
                break;
            }
            let w = self.token_text(t);
            match w {
                "open" | "sealed" | "abstract" | "concrete" | "primary" | "free" | "inline"
                | "not-inline" | "sideways" => {
                    let m = Modifier::from_word(w).unwrap();
                    self.bump();
                    modifiers.push(m);
                }
                _ => {
                    // Unknown leading adjective (e.g. `made-inline`): if a
                    // known modifier or a hard definer keyword follows, this
                    // word is an (ignored) adjective — drop it and keep
                    // scanning. Otherwise it is the definer keyword itself
                    // (a macro definer like `test`/`suite`), so stop.
                    let next_is_adj_or_definer =
                        if let Some(nt) = self.tokens.get(self.pos + 1).copied() {
                            nt.kind == TokenKind::Ident && {
                                let nw = self.token_text(nt);
                                Modifier::from_word(nw).is_some()
                                    || is_hard_definer_keyword(nw)
                            }
                        } else {
                            false
                        };
                    if next_is_adj_or_definer {
                        self.bump();
                        continue;
                    }
                    break;
                }
            }
        }
        // `define ... domain` is a sealing form; Sprint 15. Recognise the
        // keyword and stash the body as fragments.
        let kw_tok = self.peek();
        if kw_tok.kind != TokenKind::Ident {
            return Err(self.diag(kw_tok.span, format!(
                "expected define-keyword (constant, function, …) got {:?}",
                kw_tok.kind
            )));
        }
        let kw = self.token_text(kw_tok).to_string();
        match kw.as_str() {
            "constant" => {
                self.bump();
                self.parse_define_value(define_tok, modifiers, /*is_const=*/ true)
            }
            "variable" => {
                self.bump();
                self.parse_define_value(define_tok, modifiers, /*is_const=*/ false)
            }
            "function" => {
                self.bump();
                self.parse_define_function_like(define_tok, modifiers, /*kind=*/ "function")
            }
            "c-function" => {
                // Sprint 27: FFI Phase A.
                self.bump();
                self.parse_define_c_function(define_tok, modifiers)
            }
            "method" => {
                self.bump();
                self.parse_define_function_like(define_tok, modifiers, "method")
            }
            "generic" => {
                self.bump();
                self.parse_define_generic(define_tok, modifiers)
            }
            "class" => {
                self.bump();
                self.parse_define_class(define_tok, modifiers)
            }
            "library" => {
                self.bump();
                self.parse_define_library(define_tok)
            }
            "module" => {
                self.bump();
                self.parse_define_module(define_tok)
            }
            "macro" => {
                self.bump();
                self.parse_define_macro(define_tok)
            }
            _ => {
                // Unknown define form (e.g. `define test`, `define suite`,
                // `define sealed domain`). Capture body raw.
                self.bump();
                self.parse_define_other(define_tok, modifiers, kw)
            }
        }
    }

    fn parse_define_value(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
        is_const: bool,
    ) -> Result<Item, Diagnostic> {
        // [name] [:: type] = expr ;
        // Also accept multi-binder form `define constant (a, b) = …`.
        if matches!(self.peek_kind(), TokenKind::LParen) {
            // Multi-value form — model only the first binder name; rest is
            // stashed in `DEFERRED`. We still parse it.
            self.bump();
            let mut first: Option<String> = None;
            while !matches!(self.peek_kind(), TokenKind::RParen | TokenKind::Eof) {
                let t = self.peek();
                if t.kind == TokenKind::Ident && first.is_none() {
                    first = Some(self.token_text(t).to_string());
                }
                self.bump();
            }
            self.expect(TokenKind::RParen, "`)`")?;
            self.expect(TokenKind::Equal, "`=`")?;
            let value = self.parse_expr_full()?;
            let span = join(define_tok.span, value.span());
            let name = first.unwrap_or_else(|| "_anon".into());
            return Ok(if is_const {
                Item::DefineConstant {
                    span,
                    modifiers,
                    name,
                    type_: None,
                    value,
                }
            } else {
                Item::DefineVariable {
                    span,
                    modifiers,
                    name,
                    type_: None,
                    value,
                }
            });
        }
        let name_tok = self.expect(TokenKind::Ident, "name in define-value form")?;
        let name = self.token_text(name_tok).to_string();
        let mut type_: Option<Expr> = None;
        if matches!(self.peek_kind(), TokenKind::ColonColon) {
            self.bump();
            type_ = Some(self.parse_postfix()?);
        }
        self.expect(TokenKind::Equal, "`=` in define-value form")?;
        let value = self.parse_expr_full()?;
        let span = join(define_tok.span, value.span());
        Ok(if is_const {
            Item::DefineConstant {
                span,
                modifiers,
                name,
                type_,
                value,
            }
        } else {
            Item::DefineVariable {
                span,
                modifiers,
                name,
                type_,
                value,
            }
        })
    }

    fn parse_define_function_like(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
        kind: &str,
    ) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "name in define-function form")?;
        let name = self.token_text(name_tok).to_string();
        let params = self.parse_param_list_loose()?;
        let return_ = self.maybe_return_sig()?;
        // A `;` may terminate the signature before the body
        // (`define method f (…) => (…) ; body end`), as in c-function.
        if matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }
        let body = self.parse_stmt_body()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of define-function")?;
        // Optional `kind` echo then optional name echo.
        self.consume_optional_kw(kind);
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        let span = join(define_tok.span, end_tok.span);
        Ok(if kind == "function" {
            Item::DefineFunction {
                span,
                modifiers,
                name,
                params,
                return_,
                body,
            }
        } else {
            Item::DefineMethod {
                span,
                modifiers,
                name,
                params,
                return_,
                body,
            }
        })
    }

    /// Sprint 27 FFI Phase A — `define c-function`.
    ///
    /// Grammar:
    /// ```text
    /// define c-function NAME (PARAMS) [=> (RET)] ;
    ///     [c-name:  "STR" ;]
    ///     [library: "STR" ;]
    /// end [c-function] [NAME] ;
    /// ```
    ///
    /// Header attribute clauses appear AFTER the signature `;` and
    /// BEFORE `end`. Order is not significant. `library:` is the
    /// only mandatory attribute (Sprint 27 sema enforces); `c-name:`
    /// defaults to the Dylan-side `NAME` when omitted.
    ///
    /// Body shape mirrors `define function`'s tail (`end c-function
    /// Beep;`); the optional `kind` echo and optional name echo are
    /// both accepted.
    fn parse_define_c_function(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
    ) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "name in define-c-function form")?;
        let name = self.token_text(name_tok).to_string();
        let params = self.parse_param_list_loose()?;
        let return_ = self.maybe_return_sig()?;
        // Consume the signature-terminator `;` if present.
        if matches!(self.peek_kind(), TokenKind::Semicolon) {
            self.bump();
        }

        // Attribute clauses: `c-name:` / `library:`. Loop until we
        // see `end`. Each attribute is `IDENT : STRING ;`.
        let mut c_name: Option<String> = None;
        let mut library: Option<String> = None;
        loop {
            if matches!(self.peek_kind(), TokenKind::KwEnd | TokenKind::Eof) {
                break;
            }
            // `KeywordColon` lexes `foo:` as a single token whose
            // text includes the trailing `:`. The text matches by
            // stripping the colon.
            let tk = self.peek();
            if tk.kind != TokenKind::KeywordColon {
                return Err(self.diag(
                    tk.span,
                    format!(
                        "expected `c-name:` / `library:` attribute or `end` in define-c-function, got {:?}",
                        tk.kind
                    ),
                ));
            }
            let attr_text = self.token_text(tk);
            let attr = attr_text.trim_end_matches(':').to_string();
            self.bump();
            // String literal value.
            let val_tok = self.peek();
            let value = match val_tok.kind {
                TokenKind::String | TokenKind::StringRaw | TokenKind::StringMulti => {
                    self.bump();
                    let raw = self.token_text(val_tok);
                    // Strip enclosing quotes for plain `String`. Raw /
                    // Multi forms keep their delimiters; we strip the
                    // first/last char which is the simple case for
                    // Sprint 27 (DLL names don't need escapes).
                    raw.trim_start_matches('"').trim_end_matches('"').to_string()
                }
                _ => {
                    return Err(self.diag(
                        val_tok.span,
                        format!(
                            "expected string literal for `{attr}:` value, got {:?}",
                            val_tok.kind
                        ),
                    ));
                }
            };
            // Consume terminator `;`.
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
            }
            match attr.as_str() {
                "c-name" => c_name = Some(value),
                "library" => library = Some(value),
                _ => {
                    // Unknown attribute — accept for forward compat
                    // but ignore. Sprint 28+ will widen the set.
                }
            }
        }

        let end_tok = self.expect(TokenKind::KwEnd, "`end` of define-c-function")?;
        self.consume_optional_kw("c-function");
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        let span = join(define_tok.span, end_tok.span);

        Ok(Item::DefineCFunction {
            span,
            modifiers,
            name,
            params,
            return_,
            c_name,
            // Sema enforces non-empty.
            library: library.unwrap_or_default(),
        })
    }

    fn parse_define_generic(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
    ) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "name in define generic")?;
        let name = self.token_text(name_tok).to_string();
        let params = self.parse_param_list_loose()?;
        let return_ = self.maybe_return_sig()?;
        // Optional adjective trailer `, sealed` etc. Skip until `;`/EOF.
        let span_hi = match &return_ {
            Some(r) => r.span,
            None => name_tok.span,
        };
        // Skip stragglers up to `;` so the trailing options don't trip us.
        let mut last = span_hi;
        while !matches!(
            self.peek_kind(),
            TokenKind::Semicolon | TokenKind::Eof | TokenKind::KwDefine
        ) {
            last = self.peek().span;
            self.bump();
        }
        Ok(Item::DefineGeneric {
            span: join(define_tok.span, last),
            modifiers,
            name,
            params,
            return_,
        })
    }

    fn parse_define_class(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
    ) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "name in define class")?;
        let name = self.token_text(name_tok).to_string();
        // Superclass list `(sup, sup, …)`.
        let supers = self.parse_super_list()?;
        let (_extras, slots) = self.parse_class_body()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of class")?;
        self.consume_optional_kw("class");
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        Ok(Item::DefineClass {
            span: join(define_tok.span, end_tok.span),
            modifiers,
            name,
            supers,
            slots,
        })
    }

    fn parse_super_list(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut out = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::LParen) {
            return Ok(out);
        }
        self.bump();
        if matches!(self.peek_kind(), TokenKind::RParen) {
            self.bump();
            return Ok(out);
        }
        loop {
            out.push(self.parse_expr_full()?);
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)` of superclass list")?;
        Ok(out)
    }

    fn parse_class_body(&mut self) -> Result<ClassBody, Diagnostic> {
        let mut slots: Vec<SlotDef> = Vec::new();
        let extras: Vec<Expr> = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd | TokenKind::Eof) {
                break;
            }
            // Slot starts with optional adjectives + optional allocation +
            // `slot` keyword.
            let save = self.pos;
            let mut allocation = SlotAllocation::Instance;
            // adjectives we ignore for v1: open, sealed, abstract, concrete, …
            loop {
                let tk = self.peek();
                if tk.kind != TokenKind::Ident {
                    break;
                }
                let w = self.token_text(tk);
                if matches!(w, "open" | "sealed" | "abstract" | "concrete") {
                    self.bump();
                    continue;
                }
                if w == "class" {
                    self.bump();
                    allocation = SlotAllocation::Class;
                    continue;
                }
                if w == "each-subclass" {
                    self.bump();
                    allocation = SlotAllocation::EachSubclass;
                    continue;
                }
                if w == "virtual" {
                    self.bump();
                    allocation = SlotAllocation::Virtual;
                    continue;
                }
                if w == "constant" {
                    self.bump();
                    allocation = SlotAllocation::Constant;
                    continue;
                }
                break;
            }
            if !self.peek_ident_is("slot")
                && !self.peek_ident_is("inherited")
                && !self.peek_ident_is("keyword")
            {
                // Not a slot — restore and bail. Could be a member-clause
                // we don't model (`required keyword foo:;`, an init-form, …).
                // Skip to next `;` and continue.
                self.pos = save;
                while !matches!(
                    self.peek_kind(),
                    TokenKind::Semicolon | TokenKind::KwEnd | TokenKind::Eof
                ) {
                    self.bump();
                }
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            // Consume slot keyword token (`slot`, `inherited`, `keyword`).
            self.bump();
            // `inherited slot NAME` — skip the value side; we just record name.
            // `keyword foo:;` — required keyword clause.
            if matches!(self.peek_kind(), TokenKind::KeywordColon) {
                // `keyword foo:` — required init keyword without a slot. Skip.
                self.bump();
                while !matches!(
                    self.peek_kind(),
                    TokenKind::Semicolon | TokenKind::KwEnd | TokenKind::Eof
                ) {
                    self.bump();
                }
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            let name_tok = self.expect(TokenKind::Ident, "slot name")?;
            let name = self.token_text(name_tok).to_string();
            let mut type_: Option<Expr> = None;
            if matches!(self.peek_kind(), TokenKind::ColonColon) {
                self.bump();
                type_ = Some(self.parse_postfix()?);
            }
            // `= expr` shorthand for init-value (standard Dylan syntax).
            let mut init_value: Option<Expr> = None;
            if matches!(self.peek_kind(), TokenKind::Equal) {
                self.bump();
                init_value = Some(self.parse_expr_full()?);
            }
            // Trailing `, key: value` options.
            let mut init_keyword: Option<String> = None;
            let mut required_init_keyword = false;
            let mut setter: Option<bool> = None;
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
                let opt_tok = self.peek();
                if opt_tok.kind != TokenKind::KeywordColon {
                    // Not a keyword option — skip token and continue.
                    self.bump();
                    continue;
                }
                self.bump();
                let raw = self.token_text(opt_tok);
                let key = raw.trim_end_matches(':').to_string();
                match key.as_str() {
                    "init-value" => {
                        init_value = Some(self.parse_expr_full()?);
                    }
                    "init-function" => {
                        let _ = self.parse_expr_full()?;
                    }
                    "init-keyword" => {
                        let v = self.peek();
                        if v.kind == TokenKind::KeywordColon {
                            self.bump();
                            let raw = self.token_text(v);
                            init_keyword = Some(raw.trim_end_matches(':').to_string());
                        } else {
                            let e = self.parse_expr_full()?;
                            if let Expr::Symbol(_, s) = &e {
                                init_keyword =
                                    Some(s.trim_start_matches("#\"").trim_end_matches('"').into());
                            }
                        }
                    }
                    "required-init-keyword" => {
                        required_init_keyword = true;
                        let v = self.peek();
                        if v.kind == TokenKind::KeywordColon {
                            self.bump();
                            let raw = self.token_text(v);
                            init_keyword = Some(raw.trim_end_matches(':').to_string());
                        } else {
                            let _ = self.parse_expr_full()?;
                        }
                    }
                    "setter" => {
                        let v = self.parse_expr_full()?;
                        if let Expr::Bool(_, b) = v {
                            setter = Some(b);
                        }
                    }
                    "type" => {
                        type_ = Some(self.parse_postfix()?);
                    }
                    _ => {
                        // Unknown keyword option — eat the value.
                        let _ = self.parse_expr_full()?;
                    }
                }
            }
            let slot_hi = init_value
                .as_ref()
                .map(|e| e.span().hi)
                .or(type_.as_ref().map(|e| e.span().hi))
                .unwrap_or(name_tok.span.hi);
            let span = Span::new(name_tok.span.file_id, name_tok.span.lo, slot_hi);
            slots.push(SlotDef {
                span,
                name,
                type_,
                init_value,
                init_keyword,
                required_init_keyword,
                setter,
                allocation,
            });
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
            } else {
                break;
            }
        }
        Ok((extras, slots))
    }

    fn parse_define_library(&mut self, define_tok: Token) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "library name")?;
        let name = self.token_text(name_tok).to_string();
        let (uses, exports, creates) = self.parse_library_clauses_libform()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of library")?;
        self.consume_optional_kw("library");
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        Ok(Item::DefineLibrary {
            span: join(define_tok.span, end_tok.span),
            name,
            uses,
            exports,
            creates,
        })
    }

    fn parse_define_module(&mut self, define_tok: Token) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "module name")?;
        let name = self.token_text(name_tok).to_string();
        let (uses, exports, creates) = self.parse_module_clauses_modform()?;
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of module")?;
        self.consume_optional_kw("module");
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        Ok(Item::DefineModule {
            span: join(define_tok.span, end_tok.span),
            name,
            uses,
            exports,
            creates,
        })
    }

    fn parse_library_clauses_libform(&mut self) -> Result<LibraryHead, Diagnostic> {
        let mut uses: Vec<LibraryUseClause> = Vec::new();
        let mut exports: Vec<String> = Vec::new();
        let mut creates: Vec<String> = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd | TokenKind::Eof) {
                break;
            }
            if self.peek_ident_is("use") {
                let kw = self.bump();
                let name_tok = self.expect(TokenKind::Ident, "library use name")?;
                let lib_name = self.token_text(name_tok).to_string();
                let (import, exclude, rename, prefix, export) = self.parse_use_clause_options()?;
                let span_hi = self.tokens.get(self.pos.saturating_sub(1)).map(|t| t.span.hi).unwrap_or(name_tok.span.hi);
                uses.push(LibraryUseClause {
                    span: Span::new(kw.span.file_id, kw.span.lo, span_hi),
                    name: lib_name,
                    import,
                    exclude,
                    rename,
                    prefix,
                    export,
                });
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            if self.peek_ident_is("export") {
                self.bump();
                exports.extend(self.parse_name_list()?);
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            if self.peek_ident_is("create") {
                self.bump();
                creates.extend(self.parse_name_list()?);
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            // Unknown clause — skip to next `;`.
            while !matches!(
                self.peek_kind(),
                TokenKind::Semicolon | TokenKind::KwEnd | TokenKind::Eof
            ) {
                self.bump();
            }
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
            }
        }
        Ok((uses, exports, creates))
    }

    fn parse_module_clauses_modform(&mut self) -> Result<ModuleHead, Diagnostic> {
        let mut uses: Vec<ModuleUseClause> = Vec::new();
        let mut exports: Vec<String> = Vec::new();
        let mut creates: Vec<String> = Vec::new();
        loop {
            let t = self.peek();
            if matches!(t.kind, TokenKind::KwEnd | TokenKind::Eof) {
                break;
            }
            if self.peek_ident_is("use") {
                let kw = self.bump();
                let name_tok = self.expect(TokenKind::Ident, "module use name")?;
                let mod_name = self.token_text(name_tok).to_string();
                let (import, exclude, rename, prefix, export) = self.parse_use_clause_options()?;
                let span_hi = self.tokens.get(self.pos.saturating_sub(1)).map(|t| t.span.hi).unwrap_or(name_tok.span.hi);
                uses.push(ModuleUseClause {
                    span: Span::new(kw.span.file_id, kw.span.lo, span_hi),
                    name: mod_name,
                    import,
                    exclude,
                    rename,
                    prefix,
                    export,
                });
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            if self.peek_ident_is("export") {
                self.bump();
                exports.extend(self.parse_name_list()?);
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            if self.peek_ident_is("create") {
                self.bump();
                creates.extend(self.parse_name_list()?);
                if matches!(self.peek_kind(), TokenKind::Semicolon) {
                    self.bump();
                }
                continue;
            }
            while !matches!(
                self.peek_kind(),
                TokenKind::Semicolon | TokenKind::KwEnd | TokenKind::Eof
            ) {
                self.bump();
            }
            if matches!(self.peek_kind(), TokenKind::Semicolon) {
                self.bump();
            }
        }
        Ok((uses, exports, creates))
    }

    fn parse_use_clause_options(&mut self) -> Result<UseClauseOptions, Diagnostic> {
        let mut import: Option<ImportSet> = None;
        let mut exclude: Vec<String> = Vec::new();
        let mut rename: Vec<(String, String)> = Vec::new();
        let mut prefix: Option<String> = None;
        let mut export: Option<ImportSet> = None;
        while matches!(self.peek_kind(), TokenKind::Comma) {
            self.bump();
            let opt_tok = self.peek();
            if opt_tok.kind != TokenKind::KeywordColon {
                break;
            }
            self.bump();
            let raw = self.token_text(opt_tok);
            let key = raw.trim_end_matches(':').to_string();
            match key.as_str() {
                "import" => {
                    import = Some(self.parse_import_set()?);
                }
                "exclude" => {
                    exclude.extend(self.parse_brace_name_list()?);
                }
                "rename" => {
                    rename.extend(self.parse_rename_list()?);
                }
                "prefix" => {
                    // `"prefix"` is a string literal.
                    let t = self.peek();
                    if matches!(t.kind, TokenKind::String) {
                        self.bump();
                        let raw = self.token_text(t);
                        prefix = Some(
                            raw.trim_start_matches('"').trim_end_matches('"').to_string(),
                        );
                    } else {
                        let _ = self.parse_expr_full()?;
                    }
                }
                "export" => {
                    export = Some(self.parse_import_set()?);
                }
                _ => {
                    let _ = self.parse_expr_full()?;
                }
            }
        }
        Ok((import, exclude, rename, prefix, export))
    }

    /// Accept a binding name in an import/export/rename spec: a plain `Ident`
    /// or an `EscapedIdent` (`\name`, used to name operator-shaped bindings such
    /// as `\without-bounds-checks`). Returns the token and the name with any
    /// leading backslash stripped.
    fn expect_binding_name(&mut self, what: &str) -> Result<(Token, String), Diagnostic> {
        let t = self.peek();
        if matches!(t.kind, TokenKind::Ident | TokenKind::EscapedIdent) {
            self.bump();
            let raw = self.token_text(t);
            let name = raw.strip_prefix('\\').unwrap_or(raw).to_string();
            Ok((t, name))
        } else {
            Err(self.diag(t.span, format!("expected {what} (Ident)")))
        }
    }

    fn parse_import_set(&mut self) -> Result<ImportSet, Diagnostic> {
        // `all` keyword (as #all) or a `{ … }` set.
        if matches!(self.peek_kind(), TokenKind::HashAllKeys) {
            self.bump();
            return Ok(ImportSet::All);
        }
        let t = self.peek();
        if t.kind == TokenKind::Ident && self.token_text(t) == "all" {
            self.bump();
            return Ok(ImportSet::All);
        }
        if matches!(self.peek_kind(), TokenKind::LBrace) {
            self.bump();
            let mut specs = Vec::new();
            if !matches!(self.peek_kind(), TokenKind::RBrace) {
                loop {
                    let (name_tok, name) = self.expect_binding_name("imported name")?;
                    let mut rename: Option<String> = None;
                    if matches!(self.peek_kind(), TokenKind::Arrow) {
                        self.bump();
                        let (_r_tok, r_name) = self.expect_binding_name("rename target")?;
                        rename = Some(r_name);
                    }
                    specs.push(ImportSpec {
                        span: name_tok.span,
                        name,
                        rename,
                    });
                    if matches!(self.peek_kind(), TokenKind::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RBrace, "`}` of import set")?;
            return Ok(ImportSet::Items(specs));
        }
        Err(self.diag(t.span, "expected `{ … }` or `all` in import option".into()))
    }

    fn parse_brace_name_list(&mut self) -> Result<Vec<String>, Diagnostic> {
        let mut out = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::LBrace) {
            return Ok(out);
        }
        self.bump();
        while !matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof) {
            let t = self.peek();
            if t.kind == TokenKind::Ident {
                out.push(self.token_text(t).to_string());
                self.bump();
            } else {
                self.bump();
            }
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "`}`")?;
        Ok(out)
    }

    fn parse_rename_list(&mut self) -> Result<Vec<(String, String)>, Diagnostic> {
        let mut out = Vec::new();
        if !matches!(self.peek_kind(), TokenKind::LBrace) {
            return Ok(out);
        }
        self.bump();
        while !matches!(self.peek_kind(), TokenKind::RBrace | TokenKind::Eof) {
            let from_tok = self.expect(TokenKind::Ident, "rename source")?;
            let from = self.token_text(from_tok).to_string();
            self.expect(TokenKind::Arrow, "`=>` in rename")?;
            let to_tok = self.expect(TokenKind::Ident, "rename target")?;
            let to = self.token_text(to_tok).to_string();
            out.push((from, to));
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace, "`}` of rename")?;
        Ok(out)
    }

    fn parse_name_list(&mut self) -> Result<Vec<String>, Diagnostic> {
        let mut out = Vec::new();
        // Either `{ a, b, c }` or `a, b, c`.
        if matches!(self.peek_kind(), TokenKind::LBrace) {
            return self.parse_brace_name_list();
        }
        loop {
            let t = self.peek();
            if t.kind != TokenKind::Ident {
                break;
            }
            out.push(self.token_text(t).to_string());
            self.bump();
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(out)
    }

    fn parse_define_macro(&mut self, define_tok: Token) -> Result<Item, Diagnostic> {
        let name_tok = self.expect(TokenKind::Ident, "macro name")?;
        let name = self.token_text(name_tok).to_string();
        // Sprint 25: extend the known-macro set so later items in
        // this module can call the macro as a body-shaped form.
        self.known_macros.insert(name.clone());
        // Capture everything up to the matching `end macro [name]` (i.e. the
        // `end` for *this* form). Nested body forms like `end for` are not
        // the macro-end; skip past them. See `parse_define_other` for the
        // same logic.
        let body_start = self.pos;
        self.skip_body_to_matching_end("macro");
        let body_end = self.pos;
        let body_tokens = &self.tokens[body_start..body_end];
        let body_fragments = crate::fragments::build_fragments(body_tokens)
            .unwrap_or_else(|_| body_tokens.iter().copied().map(Fragment::Token).collect());
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of macro")?;
        self.consume_optional_kw("macro");
        if matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == name
        {
            self.bump();
        }
        Ok(Item::DefineMacro {
            span: join(define_tok.span, end_tok.span),
            name,
            body_fragments,
        })
    }

    /// Keywords that open an `end`-terminated block in a statement/expression
    /// body — kernel control-flow plus the stdlib's body-shaped macros. Used by
    /// [`Self::skip_body_to_matching_end`] to balance nested `end`s.
    fn is_block_opener_kw(w: &str) -> bool {
        matches!(
            w,
            // kernel control-flow
            "if" | "for"
                | "while"
                | "until"
                | "case"
                | "select"
                | "begin"
                | "block"
                | "unless"
                | "when"
                | "cond"
                | "iterate"
                | "method"
                // stdlib body-shaped macros
                | "when-let"
                | "if-let"
                | "for-each"
                | "with-cleanup"
                | "dynamic-bind"
                | "repeat"
                // common library / test-harness body-shaped macros (end-terminated)
                | "with-lock"
                | "with-open-file"
                | "with-application-output"
                | "with-pretty-print-to-string"
                | "with-output-to-string"
                | "printing-logical-block"
                | "pprint-logical-block"
                | "printing-object"
                | "collecting"
                | "benchmark-repeat"
                | "timing"
                | "profiling"
        )
    }

    /// Advance to the `KwEnd` that closes the current define-form.
    ///
    /// The body of an unknown `define`-macro (e.g. testworks `define test` /
    /// `define suite`) can contain arbitrarily nested end-terminated blocks
    /// (`if`/`for`/`while`/`begin`/`block`/`method`/…), each of which closes
    /// with `end` — frequently a *bare* `end;` with no keyword echo. We track
    /// block-nesting depth: a block-opening keyword at grouping-depth 0 pushes a
    /// level and each `end` pops one; the `end` seen at depth 0 is the one that
    /// terminates the define-form. `end` tokens inside `( )` / `[ ]` / `{ }`
    /// (e.g. literal `end`s in a macro rule) are grouping content, never block
    /// terminators.
    fn skip_body_to_matching_end(&mut self, _form_kw: &str) {
        let mut pdepth: i32 = 0; // () [] {} grouping depth
        let mut bdepth: i32 = 0; // nested end-terminated block depth
        while !self.at_end() {
            match self.peek_kind() {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
                | TokenKind::HashLParen | TokenKind::HashLBracket | TokenKind::HashLBrace => {
                    pdepth += 1;
                    self.bump();
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if pdepth > 0 {
                        pdepth -= 1;
                    }
                    self.bump();
                }
                TokenKind::KwEnd => {
                    if pdepth > 0 {
                        // literal `end` inside a grouping (e.g. a macro rule)
                        self.bump();
                    } else if bdepth > 0 {
                        // closes a nested block
                        bdepth -= 1;
                        self.bump();
                        // skip an optional `end <keyword>` echo so the echoed
                        // keyword isn't re-counted as a fresh block opener
                        if matches!(self.peek_kind(), TokenKind::Ident)
                            && Self::is_block_opener_kw(self.token_text(self.peek()))
                        {
                            self.bump();
                        }
                    } else {
                        // depth 0: this `end` terminates the define-form
                        return;
                    }
                }
                TokenKind::Ident if pdepth == 0 => {
                    if Self::is_block_opener_kw(self.token_text(self.peek())) {
                        bdepth += 1;
                    }
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_define_other(
        &mut self,
        define_tok: Token,
        modifiers: Vec<Modifier>,
        keyword: String,
    ) -> Result<Item, Diagnostic> {
        // Optional name.
        let mut name: Option<String> = None;
        if matches!(self.peek_kind(), TokenKind::Ident) {
            let t = self.peek();
            name = Some(self.token_text(t).to_string());
            self.bump();
        }
        // Skip any `( … )` head.
        if matches!(self.peek_kind(), TokenKind::LParen) {
            let mut depth: i32 = 1;
            self.bump();
            while !self.at_end() && depth > 0 {
                match self.peek_kind() {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => depth -= 1,
                    _ => {}
                }
                self.bump();
            }
        }
        // No-`end` declaration form terminated by `;`, e.g.
        // `define sealed domain make (singleton(<node>));` — a sealing
        // declaration with an optional head but no body and no `end`. Without
        // this, the body-to-`end` capture below would devour later forms
        // hunting for a non-existent `end`.
        if matches!(self.peek_kind(), TokenKind::Semicolon) {
            let end_span = self
                .tokens
                .get(self.pos.saturating_sub(1))
                .map(|t| t.span)
                .unwrap_or(define_tok.span);
            return Ok(Item::DefineOther {
                span: join(define_tok.span, end_span),
                modifiers,
                keyword,
                name,
                body_fragments: Vec::new(),
            });
        }
        // No-`end` assignment shorthand: `define <word> NAME = EXPR ;`
        // (e.g. `define benchmark takr = testtakr;`). There is no `end` for
        // this form — capture `= EXPR` as the body fragments (a macro for the
        // keyword rewrites it) and finish at the `;`/`define`/EOF terminator.
        if matches!(self.peek_kind(), TokenKind::Equal) {
            let body_start = self.pos; // include the `=`
            while !self.at_end()
                && !matches!(
                    self.peek_kind(),
                    TokenKind::Semicolon | TokenKind::KwDefine
                )
            {
                self.bump();
            }
            let body_end = self.pos;
            let body_tokens = &self.tokens[body_start..body_end];
            let body_fragments = crate::fragments::build_fragments(body_tokens)
                .unwrap_or_else(|_| body_tokens.iter().copied().map(Fragment::Token).collect());
            let end_span = self
                .tokens
                .get(body_end.saturating_sub(1))
                .map(|t| t.span)
                .unwrap_or(define_tok.span);
            return Ok(Item::DefineOther {
                span: join(define_tok.span, end_span),
                modifiers,
                keyword,
                name,
                body_fragments,
            });
        }
        // Capture body up to matching `end [keyword] [name]`.
        let body_start = self.pos;
        self.skip_body_to_matching_end(&keyword);
        let body_end = self.pos;
        let body_tokens = &self.tokens[body_start..body_end];
        let body_fragments = crate::fragments::build_fragments(body_tokens)
            .unwrap_or_else(|_| body_tokens.iter().copied().map(Fragment::Token).collect());
        let end_tok = self.expect(TokenKind::KwEnd, "`end` of define-form")?;
        // Optional keyword echo and optional name echo.
        let kw_str = keyword.clone();
        self.consume_optional_kw(&kw_str);
        if let Some(n) = &name
            && matches!(self.peek_kind(), TokenKind::Ident)
            && self.token_text(self.peek()) == n
        {
            self.bump();
        }
        Ok(Item::DefineOther {
            span: join(define_tok.span, end_tok.span),
            modifiers,
            keyword,
            name,
            body_fragments,
        })
    }

    // ─── Loose parameter list + return-sig helpers ─────────────────────

    /// Like `parse_param_list` but tolerant of `#key`, `#rest`, `#next`,
    /// `#all-keys`, `==` specialisers, and singleton/typed specialisers.
    fn parse_param_list_loose(&mut self) -> Result<Vec<Param>, Diagnostic> {
        self.expect(TokenKind::LParen, "`(` for parameter list")?;
        let mut params: Vec<Param> = Vec::new();
        if matches!(self.peek_kind(), TokenKind::RParen) {
            self.bump();
            return Ok(params);
        }
        loop {
            // Eat any leading commas that fell through from a previous
            // sub-parser (e.g. after `#key`).
            while matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            }
            if matches!(self.peek_kind(), TokenKind::RParen) {
                break;
            }
            let t = self.peek();
            match t.kind {
                TokenKind::HashRest => {
                    self.bump();
                    if matches!(self.peek_kind(), TokenKind::Ident) {
                        let n = self.bump();
                        params.push(Param {
                            span: n.span,
                            name: format!("#rest {}", self.token_text(n)),
                            type_: None,
                        });
                    }
                }
                TokenKind::HashKey => {
                    self.bump();
                    // Followed by key params until `)` / `#rest` / `#all-keys`.
                    while !matches!(
                        self.peek_kind(),
                        TokenKind::RParen
                            | TokenKind::HashRest
                            | TokenKind::HashAllKeys
                            | TokenKind::Eof
                    ) {
                        if matches!(self.peek_kind(), TokenKind::Comma) {
                            self.bump();
                            continue;
                        }
                        if matches!(self.peek_kind(), TokenKind::Ident) {
                            let n = self.bump();
                            let name = format!("#key {}", self.token_text(n));
                            let mut type_: Option<Expr> = None;
                            if matches!(self.peek_kind(), TokenKind::ColonColon) {
                                self.bump();
                                type_ = Some(self.parse_postfix()?);
                            }
                            if matches!(self.peek_kind(), TokenKind::Equal) {
                                self.bump();
                                let _ = self.parse_expr_full()?;
                            }
                            params.push(Param {
                                span: n.span,
                                name,
                                type_,
                            });
                        } else {
                            self.bump();
                        }
                    }
                }
                TokenKind::HashAllKeys => {
                    self.bump();
                    params.push(Param {
                        span: t.span,
                        name: "#all-keys".into(),
                        type_: None,
                    });
                }
                TokenKind::HashNext => {
                    self.bump();
                    if matches!(self.peek_kind(), TokenKind::Ident) {
                        let n = self.bump();
                        params.push(Param {
                            span: n.span,
                            name: format!("#next {}", self.token_text(n)),
                            type_: None,
                        });
                    }
                }
                TokenKind::Ident => {
                    let name_tok = self.bump();
                    let name = self.token_text(name_tok).to_string();
                    let mut type_: Option<Expr> = None;
                    if matches!(self.peek_kind(), TokenKind::EqualEqual) {
                        self.bump();
                        let e = self.parse_postfix()?;
                        type_ = Some(Expr::Call {
                            span: e.span(),
                            callee: Box::new(Expr::Ident(e.span(), "singleton".into())),
                            args: vec![e],
                        });
                    } else if matches!(self.peek_kind(), TokenKind::ColonColon) {
                        self.bump();
                        if matches!(self.peek_kind(), TokenKind::EqualEqual) {
                            self.bump();
                            let e = self.parse_postfix()?;
                            type_ = Some(Expr::Call {
                                span: e.span(),
                                callee: Box::new(Expr::Ident(e.span(), "singleton".into())),
                                args: vec![e],
                            });
                        } else {
                            type_ = Some(self.parse_postfix()?);
                        }
                    }
                    let span = type_
                        .as_ref()
                        .map(|t| join(name_tok.span, t.span()))
                        .unwrap_or(name_tok.span);
                    params.push(Param { span, name, type_ });
                }
                TokenKind::RParen | TokenKind::Eof => break,
                _ => {
                    return Err(self.diag(
                        t.span,
                        format!("unexpected token in parameter list: {:?}", t.kind),
                    ));
                }
            }
            if matches!(self.peek_kind(), TokenKind::Comma) {
                self.bump();
            } else if !matches!(self.peek_kind(), TokenKind::RParen) {
                // Sub-parser may have ended on a non-comma terminator we
                // should accept (e.g. `#all-keys` directly after `#key`'s
                // sub-loop). Loop back and let the outer match handle it.
                continue;
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)` after parameter list")?;
        Ok(params)
    }

    fn maybe_return_sig(&mut self) -> Result<Option<ReturnSig>, Diagnostic> {
        if !matches!(self.peek_kind(), TokenKind::Arrow) {
            return Ok(None);
        }
        let arrow = self.bump();
        // `=> NAME` (single ident) or `=> ( … )` group.
        if !matches!(self.peek_kind(), TokenKind::LParen) {
            // Single value, possibly typed.
            let e = self.parse_postfix()?;
            return Ok(Some(ReturnSig {
                span: join(arrow.span, e.span()),
                values: vec![ReturnValue {
                    span: e.span(),
                    name: None,
                    type_: Some(e),
                }],
                rest: None,
            }));
        }
        self.bump(); // `(`
        let mut values: Vec<ReturnValue> = Vec::new();
        let mut rest: Option<ReturnRest> = None;
        if !matches!(self.peek_kind(), TokenKind::RParen) {
            loop {
                if matches!(self.peek_kind(), TokenKind::HashRest) {
                    self.bump();
                    if matches!(self.peek_kind(), TokenKind::Ident) {
                        let n = self.bump();
                        let name = self.token_text(n).to_string();
                        let mut type_: Option<Expr> = None;
                        if matches!(self.peek_kind(), TokenKind::ColonColon) {
                            self.bump();
                            type_ = Some(self.parse_postfix()?);
                        }
                        rest = Some(ReturnRest {
                            span: n.span,
                            name: Some(name),
                            type_,
                        });
                    } else {
                        rest = Some(ReturnRest {
                            span: arrow.span,
                            name: None,
                            type_: None,
                        });
                    }
                    break;
                }
                // `name :: type` or bare `type`.
                let first = self.parse_postfix()?;
                if matches!(self.peek_kind(), TokenKind::ColonColon) {
                    self.bump();
                    let ty = self.parse_postfix()?;
                    let name = match &first {
                        Expr::Ident(_, n) => Some(n.clone()),
                        _ => None,
                    };
                    values.push(ReturnValue {
                        span: join(first.span(), ty.span()),
                        name,
                        type_: Some(ty),
                    });
                } else {
                    let span = first.span();
                    values.push(ReturnValue {
                        span,
                        name: None,
                        type_: Some(first),
                    });
                }
                if matches!(self.peek_kind(), TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        let close = self.expect(TokenKind::RParen, "`)` after return signature")?;
        Ok(Some(ReturnSig {
            span: join(arrow.span, close.span),
            values,
            rest,
        }))
    }
}

fn join(a: Span, b: Span) -> Span {
    Span::new(a.file_id, a.lo.min(b.lo), a.hi.max(b.hi))
}

/// The built-in `define <word>` definer keywords (everything else after
/// `define` + adjectives is a macro definer, e.g. `test`/`suite`/`benchmark`).
/// Used to recognise unknown adjectives: in `define made-inline sealed class …`
/// the unrecognised `made-inline` is an adjective because a hard definer
/// (`class`) follows it.
fn is_hard_definer_keyword(w: &str) -> bool {
    matches!(
        w,
        "constant"
            | "variable"
            | "function"
            | "c-function"
            | "method"
            | "generic"
            | "class"
            | "library"
            | "module"
            | "macro"
    )
}

/// Sprint 25: tokens that, when seen immediately after `<ident>(args)`,
/// indicate the form is part of a larger expression (a function call
/// being used as an operand, an operator continuation, or a list
/// element) rather than a body-shaped macro call's head terminator.
/// The body-shaped macro recognition path uses this to bail out
/// before consuming source it doesn't own.
fn is_call_continuation(k: TokenKind) -> bool {
    matches!(
        k,
        // Arithmetic / comparison / logical operators
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Caret
            | TokenKind::Amp
            | TokenKind::Bar
            | TokenKind::Equal
            | TokenKind::ColonEqual
            | TokenKind::EqualEqual
            | TokenKind::TildeEqual
            | TokenKind::TildeEqualEqual
            | TokenKind::Less
            | TokenKind::Greater
            | TokenKind::LessEqual
            | TokenKind::GreaterEqual
            | TokenKind::Tilde
            // Separators / closers / postfix
            | TokenKind::Comma
            | TokenKind::Semicolon
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Dot
            | TokenKind::Ellipsis
            | TokenKind::Colon
            | TokenKind::ColonColon
            | TokenKind::Arrow
            | TokenKind::Eof
    )
}

fn body_span(body: &[Expr], fallback: Span) -> Span {
    if body.is_empty() {
        return fallback;
    }
    let first = body[0].span();
    let last = body[body.len() - 1].span();
    join(first, last)
}

/// Wrap an arm body (a `Vec<Expr>`) as a single `Expr` for an `if`
/// branch. An empty body (a `select`/`case` arm with no consequent,
/// e.g. `5 => ;`) yields `#f` — the Dylan value of an empty consequent.
/// A single-element body unwraps to that element; otherwise a `begin …
/// end` sequences the statements.
fn body_to_expr(mut body: Vec<Expr>, span: Span) -> Expr {
    match body.len() {
        0 => Expr::Bool(span, false),
        1 => body.pop().unwrap(),
        _ => {
            let bspan = body_span(&body, span);
            Expr::Begin { span: bspan, body }
        }
    }
}

/// Build one `select`-arm membership test: `key = value` by default, or
/// `test(key, value)` when the `select` head used `by <test>`. `key_name`
/// is the synthetic once-evaluated binder introduced by `parse_select`.
fn select_test_expr(key_name: &str, key_span: Span, test: Option<&Expr>, value: &Expr) -> Expr {
    let key_ref = Expr::Ident(key_span, key_name.to_string());
    match test {
        None => Expr::BinOp {
            span: value.span(),
            op: BinOp::Eq,
            lhs: Box::new(key_ref),
            rhs: Box::new(value.clone()),
        },
        Some(t) => Expr::Call {
            span: value.span(),
            callee: Box::new(t.clone()),
            args: vec![key_ref, value.clone()],
        },
    }
}

/// Fold `(cond, body)` arms + an optional `otherwise` body into a nested
/// `if (cond1) body1 elseif (cond2) body2 … else otherwise end` tree. The
/// shared desugaring target for both `select` and `case`. With no arms it
/// is just the `otherwise` body (or `#f`).
fn build_if_tree(
    arms: Vec<(Expr, Vec<Expr>)>,
    otherwise: Option<Vec<Expr>>,
    span: Span,
) -> Expr {
    // Innermost else-branch: the otherwise body, or `#f` if absent.
    let mut else_: Option<Box<Expr>> = otherwise.map(|b| Box::new(body_to_expr(b, span)));
    // Fold from the last arm backward so the first arm ends up outermost.
    for (cond, body) in arms.into_iter().rev() {
        let then_ = body_to_expr(body, span);
        else_ = Some(Box::new(Expr::If {
            span,
            cond: Box::new(cond),
            then_: Box::new(then_),
            else_,
        }));
    }
    match else_ {
        Some(e) => *e,
        // No arms and no otherwise: empty select/case ⇒ `#f`.
        None => Expr::Bool(span, false),
    }
}

fn parse_int_lit(s: &str, radix: u32) -> Option<i128> {
    let mut t = String::with_capacity(s.len());
    let mut sign: i128 = 1;
    let mut it = s.chars().peekable();
    if let Some(&c) = it.peek() {
        if c == '+' {
            it.next();
        } else if c == '-' {
            sign = -1;
            it.next();
        }
    }
    for c in it {
        if c == '_' {
            continue;
        }
        t.push(c);
    }
    let magnitude = i128::from_str_radix(&t, radix).ok()?;
    Some(sign * magnitude)
}

fn strip_float_suffix(raw: &str) -> String {
    // Dylan tags precision with `s`/`d`/`x`; normalise to `e` for parsing.
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c == '_' {
            continue;
        }
        match c {
            's' | 'S' | 'd' | 'D' | 'x' | 'X' => out.push('e'),
            _ => out.push(c),
        }
    }
    out
}

fn parse_char_lit(raw: &str) -> Option<char> {
    let inner = raw.strip_prefix('\'')?.strip_suffix('\'')?;
    if let Some(esc) = inner.strip_prefix('\\') {
        return match esc {
            "n" => Some('\n'),
            "r" => Some('\r'),
            "t" => Some('\t'),
            "0" => Some('\0'),
            "\\" => Some('\\'),
            "'" => Some('\''),
            "\"" => Some('"'),
            s if s.starts_with('<') && s.ends_with('>') => {
                let hex = &s[1..s.len() - 1];
                let code = u32::from_str_radix(hex, 16).ok()?;
                char::from_u32(code)
            }
            _ => inner.chars().next(),
        };
    }
    inner.chars().next()
}
