//! `nod-macro` — Dylan macro expander (Sprint 17 + Sprint 18).
//!
//! # Macro boundary policy
//!
//! This engine is the **default home for new control-flow surface**
//! in NewOpenDylan. New `case`/`cond`/`while`/`for`/`with-*`-shaped
//! forms land as `define macro` in `stdlib.dylan` and expand through
//! this engine — they do NOT add `Expr::*` / `Statement::*` variants
//! to `nod-reader::ast`. See `docs/MACRO_BOUNDARY.md` for the full
//! rules; the frozen kernel list (`If`, `Begin`, `Let`, `Method`,
//! definitional items, `Block`-with-cleanup) is the complete set of
//! hardcoded forms.
//!
//! When a real-world macro can't be expressed in the current
//! pattern language, the answer is **extend this engine** (per
//! Rule 4 of the policy), not add to the AST. The known deferred
//! extensions (auxiliary `rule` clauses, cleanup-aware expansion,
//! cross-file macro use, definition macros) each unlock a family of
//! subsequent surface forms — pay the engine cost once, get many
//! ports for free.
//!
//! Sprint 17 shipped single-rule macros, three pattern-variable kinds
//! (`expression` / `name` / `body`), and over-conservative hygiene.
//! Sprint 18 widens the engine to:
//!   - Multi-rule definitions with first-match left-to-right rule
//!     selection (no within-rule backtracking; rules themselves are
//!     non-overlapping in practice).
//!   - Pattern variables `?x:variable` (let-binder-shaped, optionally
//!     typed), `?x:macro-arg`, `?x:parameter-list`, `?x:constraint`.
//!     The taxonomy is intentionally Sprint 18-minimal; `variable` is
//!     treated as `name + optional :: type`, `macro-arg` aliases
//!     `expression`, and `constraint` is still expression-shaped.
//!   - Statement-position macro recognition. A macro called at the
//!     statement level (a `Statement::Expr(Expr::Call { … })` whose
//!     callee is registered) gets expanded; if the expansion produces a
//!     statement-shaped form (`begin … end`, `while … end`, `let …`)
//!     the lowering's existing handling already pipes it through.
//!   - Refined hygiene: only Idents in binding positions
//!     (`let <BIND>`, function param names, local-method param names)
//!     get renamed. Idents in expression position keep their original
//!     name so reference resolution sees what the user wrote.
//!
//! Still deferred:
//!   - Full upstream `for` macro (with `from`/`to`/`by`/`above`/`below`/
//!     `then` clauses) — `for-range` in `stdlib-min` covers the common
//!     count-from-N-to-M case.
//!   - `with-*` statement macros (need `cleanup` from Sprint 19).
//!   - Auxiliary `rule` clauses inside `define macro` — Sprint 19+.
//!   - Definition macros (`define table`, `define inline function`) —
//!     parsed but not expanded; Sprint 25 stdlib porting.
//!   - Cross-file macro use — Sprint 19.
//!
//! Pipeline integration: `expand_module` runs on a parsed `Module`,
//! collecting `Item::DefineMacro` definitions and rewriting every macro
//! call shape it recognises. The lowering pass in `nod-sema` invokes us
//! between parsing and DFM lowering.
//!
//! Fragment-vs-AST representation choice (option 1 from SPRINTS.md):
//! pattern matching is fundamentally fragment-level, so when we see a
//! macro-shaped AST node (`Expr::MacroCall { name, span }` for
//! body-shaped Sprint 25 surfaces, or a `Call(Ident, …)` whose name
//! is registered for call-shape sites) we materialise the call-site
//! fragment sequence by re-lexing the AST node's source span. The
//! substituted fragment vector is then flattened back to text,
//! re-lexed, and re-parsed. This sidesteps an AST-shape change for
//! each new macro surface and lets the macro engine evolve toward
//! generic shapes (`define <kw> …`, statement macros, paren-less
//! call sites) without disturbing the parser's output.
//!
//! Sprint 25 retired the hardcoded `Expr::Unless` AST variant in
//! favour of the body-shaped `Expr::MacroCall` recognised by the
//! parser when a name is in its known-macro set; the stdlib's
//! `define macro unless` lives at the call sites' macro table.

use std::collections::HashMap;

use nod_reader::{
    BinOp, Expr, FileId, Fragment, GroupKind, Item, Module, Param, ReturnSig, SourceMap, Span,
    Statement, Token, TokenKind, build_fragments, lex, parse_expr, parse_module,
};

// ─────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────

/// A parsed `define macro` definition. Sprint 18 ships multi-rule
/// definitions with first-match selection: the driver tries `rules[0]`
/// first, falling back to `rules[1]`, etc. — see [`expand_one`].
#[derive(Clone, Debug)]
pub struct MacroDef {
    pub name: String,
    pub rules: Vec<MacroRule>,
    pub source_span: Span,
}

#[derive(Clone, Debug)]
pub struct MacroRule {
    pub pattern: Vec<PatternElem>,
    pub template: Vec<TemplateElem>,
}

#[derive(Clone, Debug)]
pub enum PatternElem {
    /// A literal token the call site must reproduce (matched by
    /// `(kind, text)` equivalence — see [`token_matches_literal`]).
    Literal { kind: TokenKind, text: String, span: Span },
    /// `?x:expression`-style variable.
    Variable { name: String, kind: PatternKind, span: Span },
    /// A grouped pattern: `(...)`, `[...]`, `{...}`, etc.
    Group { kind: GroupKind, body: Vec<PatternElem>, span: Span },
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PatternKind {
    /// Matches a single expression-shaped fragment (one token, or one
    /// grouped fragment). Default if no `:kind` suffix is given.
    Expression,
    /// Matches exactly one `Ident` token.
    Name,
    /// Greedy: matches every remaining fragment in the surrounding
    /// group until the group's close. Sprint 17 ships best-effort.
    Body,
    /// Sprint 18: matches a let-binder-shaped fragment. v1 shape:
    /// either a bare `Ident` (single token) or an `Ident :: <type>`
    /// triple. Bound as `Frags(_)` so substitution emits the raw text.
    Variable,
    /// Sprint 18: matches anything except a comma. For Sprint 18
    /// behaviour-aliases to `Expression` — the practical difference
    /// (no comma-stop) only matters once we have variadic-arg
    /// `?args:macro-arg, …` patterns, which is Sprint 19+ work.
    MacroArg,
    /// Sprint 18: matches a paren-wrapped parameter list, like
    /// `(x, y :: <integer>)`. Bound as `Frags(_)` for verbatim
    /// substitution into a `method`/`function` template position.
    /// Sprint 18 doesn't validate each element's binder shape — we
    /// trust the parser to reject malformed lists at the user-code site.
    ParameterList,
    /// Sprint 18: explicit pattern constraints (`?x:{ <expr> }`).
    /// v1 aliases `Expression`; the constraint check itself is
    /// Sprint 19+. The variant is exposed in the AST so users writing
    /// macros today get a clean signal that we recognise the syntax.
    Constraint,
}

#[derive(Clone, Debug)]
pub enum TemplateElem {
    /// A literal token from the template source. Emitted verbatim
    /// (subject to hygiene-rename if it's an `Ident`).
    Literal { kind: TokenKind, text: String, span: Span },
    /// `?x` — substitute the binding for `x`.
    Substitution { name: String, span: Span },
    /// Grouped template body.
    Group { kind: GroupKind, body: Vec<TemplateElem>, span: Span },
}

/// What a pattern variable binds to at the call site.
#[derive(Clone, Debug)]
pub enum MatchedFragment {
    /// A single token-fragment match (used for `?x:name`).
    Token(Token, String),
    /// A sequence of fragments (used for `?x:expression` and
    /// `?x:body`). `?x:expression` always binds exactly one element;
    /// `?x:body` may bind zero or more.
    Frags(Vec<Fragment>),
}

pub type Bindings = HashMap<String, MatchedFragment>;

/// Registry of `define macro` definitions collected from a `Module`.
#[derive(Default, Clone, Debug)]
pub struct MacroTable {
    pub defs: HashMap<String, MacroDef>,
}

impl MacroTable {
    pub fn get(&self, name: &str) -> Option<&MacroDef> {
        self.defs.get(name)
    }
}

#[derive(Clone, Debug)]
pub enum MacroError {
    /// Sprint 17 carry-over kept for source-compatible call sites. Now
    /// unreachable from the engine itself (Sprint 18 accepts multi-rule
    /// definitions); retained so downstream pattern matches on the
    /// `MacroError` enum continue to compile cleanly.
    MultipleRulesNotSupported { span: Span, name: String },
    /// `define macro` body is malformed (missing `=>`, unclosed group, …).
    MalformedDefinition { span: Span, name: String, detail: String },
    /// A registered macro's pattern didn't match the call site.
    PatternMismatch { call_span: Span, name: String },
    /// Sprint 18: every rule in a multi-rule definition failed to
    /// match the call site. The driver only emits this when at least
    /// one rule exists — empty `rules` is `MalformedDefinition`.
    NoApplicableRule {
        call_span: Span,
        name: String,
        rule_count: usize,
    },
    /// Recursive macro expansion exceeded the depth limit.
    ExpansionDepthExceeded { call_span: Span, depth: usize },
    /// Re-parsing the expansion failed.
    ReparseFailed {
        call_span: Span,
        name: String,
        detail: String,
    },
}

impl std::fmt::Display for MacroError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MultipleRulesNotSupported { name, .. } => write!(
                f,
                "macro `{name}`: legacy multi-rule diagnostic (Sprint 18 accepts multi-rule)"
            ),
            Self::MalformedDefinition { name, detail, .. } => {
                write!(f, "macro `{name}` is malformed: {detail}")
            }
            Self::PatternMismatch { name, .. } => {
                write!(f, "call to macro `{name}` does not match its pattern")
            }
            Self::NoApplicableRule { name, rule_count, .. } => write!(
                f,
                "call to macro `{name}` matched none of its {rule_count} rules"
            ),
            Self::ExpansionDepthExceeded { depth, .. } => write!(
                f,
                "macro expansion exceeded depth limit {depth}; recursive macros without a base case?"
            ),
            Self::ReparseFailed { name, detail, .. } => {
                write!(f, "macro `{name}` expansion failed to re-parse: {detail}")
            }
        }
    }
}

impl std::error::Error for MacroError {}

/// Default recursion depth budget for macro expansion.
pub const DEFAULT_DEPTH_LIMIT: usize = 64;
/// Default per-call-site expansion count budget.
pub const DEFAULT_EXPANSION_BUDGET: usize = 256;

// ─────────────────────────────────────────────────────────────────────────
// Phase A — parse a `define macro` body into a `MacroDef`
// ─────────────────────────────────────────────────────────────────────────

/// Parse the `body_fragments` Sprint 04 captured for a `define macro`
/// into a [`MacroDef`]. Sprint 18 accepts ONE or MORE rules separated
/// by `;`. First-match rule selection happens at expansion time
/// (see [`expand_one`]).
pub fn parse_macro_def(
    name: &str,
    body_fragments: &[Fragment],
    source_span: Span,
    source: &SourceMap,
) -> Result<MacroDef, MacroError> {
    // Grammar: '{' pattern '}' '=>' '{' template '}' (';' …)?
    // We require exactly one rule for Sprint 17.
    let mut i = 0usize;
    let mut rules = Vec::new();
    while i < body_fragments.len() {
        // Skip semicolons between rules / at trailing position.
        if matches!(&body_fragments[i], Fragment::Token(t) if t.kind == TokenKind::Semicolon) {
            i += 1;
            continue;
        }
        let (pat_body, pat_span) = match &body_fragments[i] {
            Fragment::Group { kind: GroupKind::Brace, body, span, .. } => (body, *span),
            other => {
                return Err(MacroError::MalformedDefinition {
                    span: fragment_span(other),
                    name: name.to_string(),
                    detail: "expected `{ pattern }` at start of rule".into(),
                });
            }
        };
        i += 1;
        // Expect `=>`.
        match body_fragments.get(i) {
            Some(Fragment::Token(t)) if t.kind == TokenKind::Arrow => {
                i += 1;
            }
            Some(other) => {
                return Err(MacroError::MalformedDefinition {
                    span: fragment_span(other),
                    name: name.to_string(),
                    detail: "expected `=>` after pattern".into(),
                });
            }
            None => {
                return Err(MacroError::MalformedDefinition {
                    span: pat_span,
                    name: name.to_string(),
                    detail: "expected `=>` after pattern".into(),
                });
            }
        }
        let (tpl_body, _tpl_span) = match body_fragments.get(i) {
            Some(Fragment::Group { kind: GroupKind::Brace, body, span, .. }) => (body, *span),
            Some(other) => {
                return Err(MacroError::MalformedDefinition {
                    span: fragment_span(other),
                    name: name.to_string(),
                    detail: "expected `{ template }` after `=>`".into(),
                });
            }
            None => {
                return Err(MacroError::MalformedDefinition {
                    span: pat_span,
                    name: name.to_string(),
                    detail: "expected `{ template }` after `=>`".into(),
                });
            }
        };
        i += 1;
        let pattern = parse_pattern(pat_body, source)?;
        let template = parse_template(tpl_body, source)?;
        rules.push(MacroRule { pattern, template });
    }
    if rules.is_empty() {
        return Err(MacroError::MalformedDefinition {
            span: source_span,
            name: name.to_string(),
            detail: "macro has no rules".into(),
        });
    }
    Ok(MacroDef {
        name: name.to_string(),
        rules,
        source_span,
    })
}

fn parse_pattern(
    body: &[Fragment],
    source: &SourceMap,
) -> Result<Vec<PatternElem>, MacroError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            Fragment::Token(t) if t.kind == TokenKind::Query => {
                // `?name`, `?name : kind`, or `?name:` (the lexer
                // glues `name:` into one `KeywordColon` token). In
                // all three forms we want (name, kind?, advance).
                let (name, name_span, kind, advance) =
                    parse_pattern_var_head(body, i, t.span, source)?;
                i += advance;
                out.push(PatternElem::Variable {
                    name,
                    kind,
                    span: name_span,
                });
            }
            Fragment::Token(t) => {
                let text = source.slice(t.span).to_string();
                out.push(PatternElem::Literal {
                    kind: t.kind,
                    text,
                    span: t.span,
                });
                i += 1;
            }
            Fragment::Group {
                kind, body: inner, span, ..
            } => {
                let sub = parse_pattern(inner, source)?;
                out.push(PatternElem::Group {
                    kind: *kind,
                    body: sub,
                    span: *span,
                });
                i += 1;
            }
        }
    }
    Ok(out)
}

fn parse_pattern_var_head(
    body: &[Fragment],
    i: usize,
    query_span: Span,
    source: &SourceMap,
) -> Result<(String, Span, PatternKind, usize), MacroError> {
    // body[i] is the `?`. The lexer collapses `name:` into a single
    // `KeywordColon` token, so two physical layouts produce the same
    // pattern variable:
    //   (a) Query, Ident("name")                            (no kind)
    //   (b) Query, Ident("name"), Colon, Ident("kind")      (rare; spec §2)
    //   (c) Query, KeywordColon("name:"), Ident("kind")     (common)
    let after_query = body.get(i + 1).ok_or_else(|| MacroError::MalformedDefinition {
        span: query_span,
        name: "<pattern>".into(),
        detail: "`?` not followed by a pattern variable name".into(),
    })?;
    match after_query {
        Fragment::Token(t) if t.kind == TokenKind::Ident => {
            let name = source.slice(t.span).to_string();
            // Look for `: kind` continuation.
            if let Some(Fragment::Token(c)) = body.get(i + 2)
                && c.kind == TokenKind::Colon
                && let Some(Fragment::Token(k)) = body.get(i + 3)
                && k.kind == TokenKind::Ident
            {
                let kind = parse_kind_word(source.slice(k.span), &name, k.span)?;
                return Ok((name, t.span, kind, 4));
            }
            Ok((name, t.span, PatternKind::Expression, 2))
        }
        Fragment::Token(t) if t.kind == TokenKind::KeywordColon => {
            // `name:` glued — strip trailing colon to get the name.
            let raw = source.slice(t.span);
            let name = raw.trim_end_matches(':').to_string();
            let name_span = Span::new(t.span.file_id, t.span.lo, t.span.hi.saturating_sub(1));
            // Following token (after the `:`) should be the kind ident.
            if let Some(Fragment::Token(k)) = body.get(i + 2)
                && k.kind == TokenKind::Ident
            {
                let kind = parse_kind_word(source.slice(k.span), &name, k.span)?;
                Ok((name, name_span, kind, 3))
            } else {
                Err(MacroError::MalformedDefinition {
                    span: t.span,
                    name,
                    detail: "expected pattern-kind identifier after `:`".into(),
                })
            }
        }
        _ => Err(MacroError::MalformedDefinition {
            span: query_span,
            name: "<pattern>".into(),
            detail: "expected identifier after `?`".into(),
        }),
    }
}

fn parse_kind_word(word: &str, var_name: &str, span: Span) -> Result<PatternKind, MacroError> {
    Ok(match word {
        "expression" => PatternKind::Expression,
        "name" => PatternKind::Name,
        "body" => PatternKind::Body,
        // Sprint 18 additions. See `PatternKind` for the per-kind notes
        // on what's actually enforced today vs deferred.
        "variable" => PatternKind::Variable,
        "macro-arg" => PatternKind::MacroArg,
        "parameter-list" => PatternKind::ParameterList,
        "constraint" => PatternKind::Constraint,
        // Sprint 18+ taxonomy entries we recognise the *name* of but
        // alias to expression for now. Listed explicitly so a malformed
        // ident is still flagged.
        "case-body" | "type" | "case-expression" | "definition" => PatternKind::Expression,
        other => {
            return Err(MacroError::MalformedDefinition {
                span,
                name: var_name.into(),
                detail: format!(
                    "pattern variable kind `{other}` is Sprint 19+ (supported: expression, name, body, variable, macro-arg, parameter-list, constraint)"
                ),
            });
        }
    })
}

fn parse_template(
    body: &[Fragment],
    source: &SourceMap,
) -> Result<Vec<TemplateElem>, MacroError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        match &body[i] {
            Fragment::Token(t) if t.kind == TokenKind::Query => {
                // `?` Ident — no kind annotation in template position.
                let (name, name_span, _kind, advance) =
                    parse_pattern_var_head(body, i, t.span, source)?;
                i += advance;
                out.push(TemplateElem::Substitution {
                    name,
                    span: name_span,
                });
            }
            Fragment::Token(t) => {
                let text = source.slice(t.span).to_string();
                out.push(TemplateElem::Literal {
                    kind: t.kind,
                    text,
                    span: t.span,
                });
                i += 1;
            }
            Fragment::Group {
                kind, body: inner, span, ..
            } => {
                let sub = parse_template(inner, source)?;
                out.push(TemplateElem::Group {
                    kind: *kind,
                    body: sub,
                    span: *span,
                });
                i += 1;
            }
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// Phase B — populate `MacroTable` from a Module
// ─────────────────────────────────────────────────────────────────────────

/// Walk `module.items`, parse every `Item::DefineMacro`, and install it
/// in `table`. `source` is the `SourceMap` covering the original
/// definition file so token text can be recovered from spans.
pub fn collect_macros(
    module: &Module,
    source: &SourceMap,
    table: &mut MacroTable,
) -> Result<(), Vec<MacroError>> {
    let mut errs = Vec::new();
    for it in &module.items {
        if let Item::DefineMacro { span, name, body_fragments } = it {
            match parse_macro_def(name, body_fragments, *span, source) {
                Ok(def) => {
                    table.defs.insert(name.clone(), def);
                }
                Err(e) => errs.push(e),
            }
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

// ─────────────────────────────────────────────────────────────────────────
// Phase C — pattern matching
// ─────────────────────────────────────────────────────────────────────────

/// Match a pattern against a call site's fragment sequence. Greedy,
/// left-to-right, no backtracking (Sprint 18+ adds it).
pub fn match_pattern(pattern: &[PatternElem], call: &[Fragment]) -> Option<Bindings> {
    let mut b: Bindings = Bindings::new();
    let mut ci = 0usize;
    let mut pi = 0usize;
    while pi < pattern.len() {
        match &pattern[pi] {
            PatternElem::Literal { kind, text, .. } => {
                let cf = call.get(ci)?;
                if !token_matches_literal(cf, *kind, text) {
                    return None;
                }
                ci += 1;
                pi += 1;
            }
            PatternElem::Variable { name, kind, .. } => match kind {
                PatternKind::Expression
                | PatternKind::MacroArg
                | PatternKind::Constraint => {
                    // Bind one fragment. Sprint 18: `MacroArg` aliases
                    // `Expression` until variadic patterns land; `Constraint`
                    // bodies are validated at the AST level once Sprint 19
                    // wires the constraint check.
                    let f = call.get(ci)?.clone();
                    b.insert(name.clone(), MatchedFragment::Frags(vec![f]));
                    ci += 1;
                    pi += 1;
                }
                PatternKind::Name => {
                    let cf = call.get(ci)?;
                    match cf {
                        Fragment::Token(t) if t.kind == TokenKind::Ident => {
                            b.insert(
                                name.clone(),
                                MatchedFragment::Token(*t, frag_text(cf)),
                            );
                        }
                        _ => return None,
                    }
                    ci += 1;
                    pi += 1;
                }
                PatternKind::Variable => {
                    // Sprint 18: `?x:variable`. Matches a let-binder-shaped
                    // fragment sequence: either a single Ident or
                    // `Ident :: <type-expression>`. Greedy on the type
                    // annotation if `::` is present. Bound as `Frags(_)`
                    // so substitution emits the raw text verbatim.
                    let head = call.get(ci)?;
                    let Fragment::Token(head_tok) = head else {
                        return None;
                    };
                    if head_tok.kind != TokenKind::Ident {
                        return None;
                    }
                    let mut consumed = 1;
                    // Optional `:: <type>`. Treat `::` as two adjacent
                    // `Colon` tokens — Dylan's lexer doesn't yet have a
                    // dedicated `ColonColon`. Type expression is one
                    // following Ident (or a `<wrapped-name>` ident
                    // — same token kind).
                    if let (Some(Fragment::Token(t1)), Some(Fragment::Token(t2))) =
                        (call.get(ci + 1), call.get(ci + 2))
                        && t1.kind == TokenKind::Colon
                        && t2.kind == TokenKind::Colon
                        && let Some(Fragment::Token(ty)) = call.get(ci + 3)
                        && ty.kind == TokenKind::Ident
                    {
                        consumed = 4;
                    }
                    let slice: Vec<Fragment> = call[ci..ci + consumed].to_vec();
                    b.insert(name.clone(), MatchedFragment::Frags(slice));
                    ci += consumed;
                    pi += 1;
                }
                PatternKind::ParameterList => {
                    // Sprint 18: `?x:parameter-list`. Matches a single
                    // parenthesised group. The internal shape (binders
                    // separated by commas) isn't validated here — the
                    // template re-emits verbatim and the parser at the
                    // expansion site rejects ill-formed lists. Useful
                    // for `define method` macros that synthesise a
                    // generic + method pair.
                    let cf = call.get(ci)?;
                    let Fragment::Group { kind: GroupKind::Paren, .. } = cf else {
                        return None;
                    };
                    b.insert(name.clone(), MatchedFragment::Frags(vec![cf.clone()]));
                    ci += 1;
                    pi += 1;
                }
                PatternKind::Body => {
                    // Sprint 18: Body matches "everything up to the
                    // remaining trailing literals in the pattern".
                    //
                    // Sprint N (delimiter-aware body): if the NEXT
                    // pattern element is a Literal, scan FORWARD in the
                    // call for the first occurrence of that literal and
                    // stop there.  This lets two adjacent `:body`
                    // variables split at an intervening keyword:
                    //
                    //   with-cleanup ?body:body cleanup ?cleanup:body end
                    //
                    // Without the forward scan the old trailing-count
                    // approach only sees `end` as a trailer (it stops at
                    // the `?cleanup:body` variable), so the first body
                    // greedily consumes `cleanup` + its content.
                    //
                    // For `KwEnd` delimiters the scan is DEPTH-AWARE:
                    // body-forming keywords (`if`, `unless`, `while`,
                    // `until`, `for`, `block`, `select`, `case`,
                    // `begin`, `method`, `when`, `with-cleanup`) bump a
                    // nesting depth counter; only a `KwEnd` at depth 0
                    // is accepted as the body terminator.  Without this,
                    // `unless (c) if (y) z end end` would match the
                    // inner `end` (closing the `if`) instead of the
                    // outer one (closing the `unless`), producing a
                    // truncated body and a mis-matched pattern.
                    //
                    // For other delimiters (e.g. `cleanup` in
                    // `with-cleanup`) simple first-occurrence is correct
                    // because those separator words cannot be nested.
                    //
                    // Fallback: if the next element is NOT a Literal (or
                    // there is no next element) use the old
                    // `count_trailing_literals` approach unchanged.
                    let body_end = match pattern.get(pi + 1) {
                        Some(PatternElem::Literal { kind, text, .. }) => {
                            let found = if *kind == TokenKind::KwEnd {
                                // Depth-aware: track end-terminated forms.
                                let mut depth = 0i32;
                                call[ci..].iter().position(|f| match f {
                                    Fragment::Token(t)
                                        if t.kind == TokenKind::Ident =>
                                    {
                                        // Idents that open end-terminated body forms.
                                        if tok_text_eq(t, "if")
                                            || tok_text_eq(t, "unless")
                                            || tok_text_eq(t, "while")
                                            || tok_text_eq(t, "until")
                                            || tok_text_eq(t, "for")
                                            || tok_text_eq(t, "block")
                                            || tok_text_eq(t, "select")
                                            || tok_text_eq(t, "case")
                                            || tok_text_eq(t, "cond")  // Sprint 49b
                                            || tok_text_eq(t, "begin")
                                            || tok_text_eq(t, "method")
                                            || tok_text_eq(t, "when")
                                            || tok_text_eq(t, "with-cleanup")
                                            || tok_text_eq(t, "iterate")
                                            || tok_text_eq(t, "for-each")
                                            || tok_text_eq(t, "dynamic-bind")
                                            || tok_text_eq(t, "repeat")
                                            || tok_text_eq(t, "with-lock")
                                            || tok_text_eq(t, "with-open-file")
                                            || tok_text_eq(t, "with-application-output")
                                            || tok_text_eq(t, "with-pretty-print-to-string")
                                            || tok_text_eq(t, "with-output-to-string")
                                            || tok_text_eq(t, "printing-logical-block")
                                            || tok_text_eq(t, "pprint-logical-block")
                                            || tok_text_eq(t, "printing-object")
                                            || tok_text_eq(t, "collecting")
                                            || tok_text_eq(t, "benchmark-repeat")
                                            || tok_text_eq(t, "timing")
                                            || tok_text_eq(t, "profiling")
                                        {
                                            depth += 1;
                                        }
                                        false
                                    }
                                    Fragment::Token(t)
                                        if t.kind == TokenKind::KwEnd =>
                                    {
                                        if depth == 0 {
                                            true
                                        } else {
                                            depth -= 1;
                                            false
                                        }
                                    }
                                    _ => false,
                                })
                            } else {
                                // Simple first-occurrence for non-`end` delimiters.
                                call[ci..]
                                    .iter()
                                    .position(|f| token_matches_literal(f, *kind, text))
                            };
                            found
                                .map(|pos| ci + pos)
                                .unwrap_or_else(|| {
                                    let trailing_lits =
                                        count_trailing_literals(&pattern[pi + 1..]);
                                    call.len().saturating_sub(trailing_lits)
                                })
                        }
                        _ => {
                            let trailing_lits = count_trailing_literals(&pattern[pi + 1..]);
                            call.len().saturating_sub(trailing_lits)
                        }
                    };
                    if call.len() < ci {
                        return None;
                    }
                    let body_slice: Vec<Fragment> = call[ci..body_end].to_vec();
                    b.insert(name.clone(), MatchedFragment::Frags(body_slice));
                    ci = body_end;
                    pi += 1;
                }
            },
            PatternElem::Group { kind, body: pbody, .. } => {
                let cf = call.get(ci)?;
                let Fragment::Group {
                    kind: ck,
                    body: cbody,
                    ..
                } = cf
                else {
                    return None;
                };
                if ck != kind {
                    return None;
                }
                let sub = match_pattern(pbody, cbody)?;
                for (k, v) in sub {
                    b.insert(k, v);
                }
                ci += 1;
                pi += 1;
            }
        }
    }
    if ci != call.len() {
        return None;
    }
    Some(b)
}

/// Count the trailing run of `PatternElem::Literal` / `PatternElem::Group`
/// entries at the end of a pattern slice. Used by `Body` to leave room
/// for the followers when matching greedily.
fn count_trailing_literals(rest: &[PatternElem]) -> usize {
    let mut n = 0;
    for p in rest.iter().rev() {
        match p {
            PatternElem::Literal { .. } | PatternElem::Group { .. } => n += 1,
            PatternElem::Variable { .. } => break,
        }
    }
    n
}

fn token_matches_literal(f: &Fragment, kind: TokenKind, text: &str) -> bool {
    match f {
        Fragment::Token(t) => t.kind == kind && tok_text_eq(t, text),
        _ => false,
    }
}

/// Compare a token's text to the pattern literal's text. Since `Token`
/// doesn't carry the file it came from, we rely on the surrounding
/// fragment-matcher context: the call site always re-lexes from the
/// `expansion_source` (set by `expand_module`) so the text-extraction
/// is always relative to the *current* call-site source. The matcher
/// is invoked with that source bound via [`with_call_site_source`].
fn tok_text_eq(t: &Token, expected: &str) -> bool {
    CALL_SITE_SOURCE.with(|cs| {
        let cs = cs.borrow();
        if let Some(src) = cs.as_ref() {
            let lo = t.span.lo as usize;
            let hi = t.span.hi as usize;
            src.get(lo..hi).map(|s| s == expected).unwrap_or(false)
        } else {
            false
        }
    })
}

fn frag_text(f: &Fragment) -> String {
    CALL_SITE_SOURCE.with(|cs| {
        let cs = cs.borrow();
        let Some(src) = cs.as_ref() else { return String::new() };
        let sp = fragment_span(f);
        src.get(sp.lo as usize..sp.hi as usize)
            .unwrap_or("")
            .to_string()
    })
}

thread_local! {
    static CALL_SITE_SOURCE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn with_call_site_source<F, R>(src: &str, f: F) -> R
where
    F: FnOnce() -> R,
{
    CALL_SITE_SOURCE.with(|cs| {
        *cs.borrow_mut() = Some(src.to_string());
    });
    let r = f();
    CALL_SITE_SOURCE.with(|cs| {
        *cs.borrow_mut() = None;
    });
    r
}

/// Oracle/test helper: run [`match_pattern`] with the call-site source
/// bound so literal-text comparison (`tok_text_eq`) and `Name`-binding
/// text extraction (`frag_text`) resolve against `call_src`. The bare
/// [`match_pattern`] is only meaningful inside `expand_one`, which sets
/// this binding via the private `with_call_site_source`; this wrapper
/// exposes the same setup so the Sprint 52.3 Dylan-vs-Rust match parity
/// gate can drive the Rust engine directly.
pub fn match_pattern_with_source(
    pattern: &[PatternElem],
    call: &[Fragment],
    call_src: &str,
) -> Option<Bindings> {
    with_call_site_source(call_src, || match_pattern(pattern, call))
}

fn fragment_span(f: &Fragment) -> Span {
    match f {
        Fragment::Token(t) => t.span,
        Fragment::Group { span, .. } => *span,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Phase D — template substitution + hygiene
// ─────────────────────────────────────────────────────────────────────────

/// Set of identifier names that template-introduced occurrences must
/// NOT be hygiene-renamed. Sprint 17 list is intentionally conservative:
/// every Dylan special-form keyword, the core type names already
/// recognised by the parser, and the pattern's literal-token texts.
/// See DEFERRED entry "Hygiene policy refinement".
fn is_template_no_rename(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "elseif"
            | "unless"
            | "begin"
            | "let"
            | "local"
            | "method"
            | "case"
            | "select"
            | "for"
            | "while"
            | "until"
            | "block"
            | "exception"
            | "cleanup"
            | "finally"
            | "afterwards"
            | "from"
            | "to"
            | "by"
            | "below"
            | "above"
            | "in"
            | "values"
            | "next-method"
            | "make"
            | "as"
            | "instance?"
            | "subtype?"
            | "element"
            | "<integer>"
            | "<single-float>"
            | "<double-float>"
            | "<boolean>"
            | "<character>"
            | "<string>"
            | "<byte-string>"
            | "<object>"
            | "<class>"
            | "<symbol>"
            | "<pair>"
            | "<list>"
            | "<empty-list>"
            | "<vector>"
            | "<sequence>"
            | "<collection>"
    )
}

/// Sprint 17 entry point preserved for source compatibility. Sprint 18
/// expansion goes through [`substitute_with_binders`], which threads a
/// "this Ident is in a binding position" set so hygiene only renames
/// binder names. Sprint 17's all-Idents-rename policy is still
/// available by passing an empty `binders` set.
pub fn substitute(
    template: &[TemplateElem],
    bindings: &Bindings,
    hygiene_nonce: u64,
    call_site_source: &str,
    template_source: &SourceMap,
    template_file: FileId,
    pattern_var_names: &std::collections::HashSet<String>,
) -> SubstitutionOutput {
    let binders = collect_template_binders(template);
    substitute_with_binders(
        template,
        bindings,
        hygiene_nonce,
        call_site_source,
        template_source,
        template_file,
        pattern_var_names,
        &binders,
    )
}

/// Sprint 18 substitution. Same as [`substitute`] but only renames an
/// Ident token if it appears in `binders` — the set of Ident texts that
/// occupy a binding position in the template (`let X = …`, `method (X, …)`,
/// `local method (X, …)`, function param positions). Other Idents flow
/// through verbatim, letting the surrounding scope resolve them.
#[allow(clippy::too_many_arguments)]
pub fn substitute_with_binders(
    template: &[TemplateElem],
    bindings: &Bindings,
    hygiene_nonce: u64,
    call_site_source: &str,
    template_source: &SourceMap,
    template_file: FileId,
    pattern_var_names: &std::collections::HashSet<String>,
    binders: &std::collections::HashSet<String>,
) -> SubstitutionOutput {
    let mut buf = String::new();
    let mut origins: Vec<TokenOrigin> = Vec::new();
    emit_template(
        template,
        bindings,
        hygiene_nonce,
        call_site_source,
        template_source,
        template_file,
        pattern_var_names,
        binders,
        &mut buf,
        &mut origins,
    );
    SubstitutionOutput { text: buf, origins }
}

#[derive(Clone, Debug)]
pub struct SubstitutionOutput {
    pub text: String,
    /// Origin annotations: for each "primitive piece" written into
    /// `text`, the offset range in `text` and the original `Span` it
    /// came from. Used by the post-parse span-rewriter.
    pub origins: Vec<TokenOrigin>,
}

#[derive(Copy, Clone, Debug)]
pub struct TokenOrigin {
    pub buf_lo: u32,
    pub buf_hi: u32,
    pub original_span: Span,
    pub from_template: bool,
}

#[allow(clippy::too_many_arguments)]
fn emit_template(
    template: &[TemplateElem],
    bindings: &Bindings,
    hygiene_nonce: u64,
    call_site_source: &str,
    template_source: &SourceMap,
    template_file: FileId,
    pattern_var_names: &std::collections::HashSet<String>,
    binders: &std::collections::HashSet<String>,
    buf: &mut String,
    origins: &mut Vec<TokenOrigin>,
) {
    for elem in template {
        match elem {
            TemplateElem::Literal { kind, text, span } => {
                // Sprint 18 hygiene policy: rename only Idents that
                // appear in *binding position* in the template
                // (`binders`). Reference-position Idents flow through
                // unchanged so they resolve against the surrounding
                // scope. Pattern-variable substitution slots (`?x`)
                // aren't tracked here — they're a separate
                // TemplateElem::Substitution and never reach this arm.
                let emit_text: String = if *kind == TokenKind::Ident
                    && binders.contains(text)
                    && !pattern_var_names.contains(text)
                    && !is_template_no_rename(text)
                {
                    format!("{text}__nod_hyg_{hygiene_nonce}")
                } else {
                    text.clone()
                };
                let lo = buf.len() as u32;
                buf.push_str(&emit_text);
                buf.push(' ');
                let hi = lo + emit_text.len() as u32;
                origins.push(TokenOrigin {
                    buf_lo: lo,
                    buf_hi: hi,
                    original_span: *span,
                    from_template: true,
                });
            }
            TemplateElem::Substitution { name, span } => {
                let Some(m) = bindings.get(name) else {
                    // Unbound substitution — emit a placeholder; reparse
                    // will fail with a clear error. (Sprint 17 trusts
                    // pattern parsing to keep templates honest.)
                    continue;
                };
                match m {
                    MatchedFragment::Token(t, txt) => {
                        let lo = buf.len() as u32;
                        buf.push_str(txt);
                        buf.push(' ');
                        let hi = lo + txt.len() as u32;
                        origins.push(TokenOrigin {
                            buf_lo: lo,
                            buf_hi: hi,
                            original_span: t.span,
                            from_template: false,
                        });
                    }
                    MatchedFragment::Frags(fs) => {
                        for f in fs {
                            emit_fragment_verbatim(f, call_site_source, buf, origins);
                        }
                    }
                }
                // WHY: keep span un-used here; substitution's own span
                // is the template `?x` reference site, not load-bearing.
                let _ = span;
            }
            TemplateElem::Group { kind, body, span } => {
                let (open, close) = group_delims(*kind);
                let lo = buf.len() as u32;
                buf.push_str(open);
                buf.push(' ');
                let hi = lo + open.len() as u32;
                origins.push(TokenOrigin {
                    buf_lo: lo,
                    buf_hi: hi,
                    original_span: *span,
                    from_template: true,
                });
                emit_template(
                    body,
                    bindings,
                    hygiene_nonce,
                    call_site_source,
                    template_source,
                    template_file,
                    pattern_var_names,
                    binders,
                    buf,
                    origins,
                );
                let lo = buf.len() as u32;
                buf.push_str(close);
                buf.push(' ');
                let hi = lo + close.len() as u32;
                origins.push(TokenOrigin {
                    buf_lo: lo,
                    buf_hi: hi,
                    original_span: *span,
                    from_template: true,
                });
            }
        }
    }
    let _ = template_source;
    let _ = template_file;
}

fn emit_fragment_verbatim(
    f: &Fragment,
    call_site_source: &str,
    buf: &mut String,
    origins: &mut Vec<TokenOrigin>,
) {
    match f {
        Fragment::Token(t) => {
            let lo = t.span.lo as usize;
            let hi = t.span.hi as usize;
            let txt = call_site_source.get(lo..hi).unwrap_or("");
            let buf_lo = buf.len() as u32;
            buf.push_str(txt);
            buf.push(' ');
            let buf_hi = buf_lo + txt.len() as u32;
            origins.push(TokenOrigin {
                buf_lo,
                buf_hi,
                original_span: t.span,
                from_template: false,
            });
        }
        Fragment::Group { open, close, body, .. } => {
            // Re-emit open/close + recurse.
            let lo = open.span.lo as usize;
            let hi = open.span.hi as usize;
            let txt = call_site_source.get(lo..hi).unwrap_or("");
            let buf_lo = buf.len() as u32;
            buf.push_str(txt);
            buf.push(' ');
            let buf_hi = buf_lo + txt.len() as u32;
            origins.push(TokenOrigin {
                buf_lo,
                buf_hi,
                original_span: open.span,
                from_template: false,
            });
            for inner in body {
                emit_fragment_verbatim(inner, call_site_source, buf, origins);
            }
            let lo = close.span.lo as usize;
            let hi = close.span.hi as usize;
            let txt = call_site_source.get(lo..hi).unwrap_or("");
            let buf_lo = buf.len() as u32;
            buf.push_str(txt);
            buf.push(' ');
            let buf_hi = buf_lo + txt.len() as u32;
            origins.push(TokenOrigin {
                buf_lo,
                buf_hi,
                original_span: close.span,
                from_template: false,
            });
        }
    }
}

fn group_delims(k: GroupKind) -> (&'static str, &'static str) {
    match k {
        GroupKind::Paren => ("(", ")"),
        GroupKind::Bracket => ("[", "]"),
        GroupKind::Brace => ("{", "}"),
        GroupKind::HashParen => ("#(", ")"),
        GroupKind::HashBracket => ("#[", "]"),
        GroupKind::HashBrace => ("#{", "}"),
    }
}

/// Sprint 18 hygiene refinement: walk a template and return the set of
/// Ident texts that occupy a *binding position*. v1 binding positions:
///
///   - The Ident immediately after `let` and before `=` / `::` / `,`.
///   - Idents inside the paren-arg-list of `method (…) … end` or
///     `local method <name> (…) … end`.
///
/// The walk is conservative — false negatives (a binder we missed) just
/// mean the hygiene rename doesn't fire for that one Ident, which is
/// the Sprint 17 default. False positives (a non-binder we renamed)
/// would break code; we keep the rules narrow to avoid that.
pub fn collect_template_binders(template: &[TemplateElem]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    walk_template_for_binders(template, &mut out);
    out
}

fn walk_template_for_binders(
    template: &[TemplateElem],
    out: &mut std::collections::HashSet<String>,
) {
    let mut i = 0;
    while i < template.len() {
        match &template[i] {
            TemplateElem::Literal { kind, text, .. } if *kind == TokenKind::Ident => {
                if text == "let"
                    && let Some(TemplateElem::Literal { kind: k2, text: t2, .. }) =
                        template.get(i + 1)
                    && *k2 == TokenKind::Ident
                {
                    out.insert(t2.clone());
                    i += 2;
                    continue;
                }
                if (text == "method" || text == "function")
                    && let Some(TemplateElem::Group {
                        kind: GroupKind::Paren,
                        body,
                        ..
                    }) = template.get(i + 1)
                {
                    record_param_idents(body, out);
                    i += 2;
                    continue;
                }
                i += 1;
            }
            TemplateElem::Group { body, .. } => {
                walk_template_for_binders(body, out);
                i += 1;
            }
            _ => i += 1,
        }
    }
}

fn record_param_idents(
    body: &[TemplateElem],
    out: &mut std::collections::HashSet<String>,
) {
    // Param list: `name(, name)*`, with optional `:: <type>` after each.
    // We add every Ident that appears immediately after a `,` or at the
    // head, before a `::` or `,`. v1 approximation: every Ident in the
    // list is treated as a binder.
    let mut expect_name = true;
    for el in body {
        match el {
            TemplateElem::Literal { kind, text, .. } => {
                if *kind == TokenKind::Ident && expect_name {
                    // Skip if the next character is `::` — already a type
                    // name follower; but to keep it simple, just always add.
                    out.insert(text.clone());
                    expect_name = false;
                } else if *kind == TokenKind::Comma {
                    expect_name = true;
                }
            }
            _ => {
                // Anything else (group, sub) resets and let next ident
                // be a parameter.
                expect_name = true;
            }
        }
    }
}

fn collect_pattern_var_names(pattern: &[PatternElem]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    collect_pv(pattern, &mut out);
    out
}

fn collect_pv(pattern: &[PatternElem], out: &mut std::collections::HashSet<String>) {
    for p in pattern {
        match p {
            PatternElem::Variable { name, .. } => {
                out.insert(name.clone());
            }
            PatternElem::Group { body, .. } => collect_pv(body, out),
            PatternElem::Literal { .. } => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Phase F — depth-limited driver: expand a single Expr
// ─────────────────────────────────────────────────────────────────────────

struct ExpansionCtx<'a> {
    table: &'a MacroTable,
    source: &'a SourceMap,
    /// Source map index for the file the macro DEFINITION lives in.
    /// Sprint 17 assumes the call site and the definition share the
    /// same `SourceMap`; cross-file macro use is DEFERRED.
    depth: usize,
    depth_limit: usize,
    nonce_counter: u64,
}

impl<'a> ExpansionCtx<'a> {
    fn fresh_nonce(&mut self) -> u64 {
        let n = self.nonce_counter;
        self.nonce_counter += 1;
        n
    }
}

/// Expand every recognised macro call inside `module`, in-place.
///
/// `source` must be the `SourceMap` registered for the file the
/// module was parsed from. Macros installed in the table by
/// `collect_macros` reference spans into this same map.
pub fn expand_module(
    module: &mut Module,
    table: &MacroTable,
    source: &SourceMap,
) -> Result<(), Vec<MacroError>> {
    let mut errs = Vec::new();
    let mut ctx = ExpansionCtx {
        table,
        source,
        depth: 0,
        depth_limit: DEFAULT_DEPTH_LIMIT,
        nonce_counter: 1,
    };
    // Drop `Item::DefineMacro` entries after collection — they're inert
    // post-expansion and downstream passes (lowering) treat them as
    // `Unsupported`.
    module.items.retain(|it| !matches!(it, Item::DefineMacro { .. }));
    // Rebuild the item list. A top-level `define <word> … end` whose `<word>` is
    // a registered macro is a DEFINITION-MACRO call: expand it to the item it
    // produces, then expand inside that. Everything else expands in place. (The
    // macro table is already fully populated by `collect_macros`.)
    let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len());
    for mut it in std::mem::take(&mut module.items) {
        let def = match &it {
            Item::DefineOther { keyword, .. } => ctx.table.defs.get(keyword).cloned(),
            _ => None,
        };
        if let Some(def) = def {
            match expand_definition_macro(&it, &def, &mut ctx) {
                Ok(mut produced) => {
                    expand_item(&mut produced, &mut ctx, &mut errs);
                    new_items.push(produced);
                }
                Err(e) => {
                    errs.push(e);
                    new_items.push(it);
                }
            }
        } else {
            expand_item(&mut it, &mut ctx, &mut errs);
            new_items.push(it);
        }
    }
    module.items = new_items;
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

fn expand_item(it: &mut Item, ctx: &mut ExpansionCtx<'_>, errs: &mut Vec<MacroError>) {
    match it {
        Item::DefineFunction { body, .. } | Item::DefineMethod { body, .. } => {
            for s in body {
                expand_stmt(s, ctx, errs);
            }
        }
        Item::DefineConstant { value, .. } | Item::DefineVariable { value, .. } => {
            expand_expr(value, ctx, errs);
        }
        Item::Expr(e) => expand_expr(e, ctx, errs),
        // Sprint 17 doesn't expand inside class / generic / library /
        // module headers (no macro use sites known in this scope).
        _ => {}
    }
}

fn expand_stmt(s: &mut Statement, ctx: &mut ExpansionCtx<'_>, errs: &mut Vec<MacroError>) {
    match s {
        Statement::Expr(e) => expand_expr(e, ctx, errs),
        Statement::Let { value, .. } => expand_expr(value, ctx, errs),
        Statement::Local { methods, .. } => {
            for m in methods {
                for s2 in &mut m.body {
                    expand_stmt(s2, ctx, errs);
                }
            }
        }
        Statement::For { body, finally_, .. } => {
            for s2 in body {
                expand_stmt(s2, ctx, errs);
            }
            for s2 in finally_ {
                expand_stmt(s2, ctx, errs);
            }
        }
        Statement::While { cond, body, .. } | Statement::Until { cond, body, .. } => {
            expand_expr(cond, ctx, errs);
            for s2 in body {
                expand_stmt(s2, ctx, errs);
            }
        }
        Statement::Block {
            body, handlers, cleanup, afterwards, ..
        } => {
            for s2 in body {
                expand_stmt(s2, ctx, errs);
            }
            for h in handlers {
                for s2 in &mut h.body {
                    expand_stmt(s2, ctx, errs);
                }
            }
            for s2 in cleanup {
                expand_stmt(s2, ctx, errs);
            }
            for s2 in afterwards {
                expand_stmt(s2, ctx, errs);
            }
        }
    }
}

fn expand_expr(e: &mut Expr, ctx: &mut ExpansionCtx<'_>, errs: &mut Vec<MacroError>) {
    // Bottom-up: expand sub-expressions first.
    walk_subexprs(e, ctx, errs);
    // Then try to recognise this node as a macro call.
    let Some(name) = macro_call_name(e) else { return };
    let Some(def) = ctx.table.get(&name).cloned() else { return };
    if ctx.depth >= ctx.depth_limit {
        errs.push(MacroError::ExpansionDepthExceeded {
            call_span: e.span(),
            depth: ctx.depth_limit,
        });
        return;
    }
    ctx.depth += 1;
    match expand_one(e, &def, ctx) {
        Ok(new) => {
            *e = new;
            // Allow a fresh expansion pass on the result (it may itself
            // be a macro call). Recurse with the same context so the
            // depth counter prevents infinite loops.
            expand_expr(e, ctx, errs);
        }
        Err(err) => errs.push(err),
    }
    ctx.depth -= 1;
    let _ = name;
}

fn walk_subexprs(e: &mut Expr, ctx: &mut ExpansionCtx<'_>, errs: &mut Vec<MacroError>) {
    match e {
        Expr::Integer(..)
        | Expr::Float(..)
        | Expr::String(..)
        | Expr::Char(..)
        | Expr::Bool(..)
        | Expr::Symbol(..)
        | Expr::Ident(..) => {}
        Expr::Call { callee, args, .. } => {
            expand_expr(callee, ctx, errs);
            for a in args {
                expand_expr(a, ctx, errs);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            expand_expr(lhs, ctx, errs);
            expand_expr(rhs, ctx, errs);
        }
        Expr::UnOp { operand, .. } => expand_expr(operand, ctx, errs),
        Expr::Paren { inner, .. } => expand_expr(inner, ctx, errs),
        Expr::If { cond, then_, else_, .. } => {
            expand_expr(cond, ctx, errs);
            expand_expr(then_, ctx, errs);
            if let Some(b) = else_ {
                expand_expr(b, ctx, errs);
            }
        }
        Expr::MacroCall { .. } => {
            // Body-shaped macro call. No sub-expressions in the AST
            // (the body is opaque source text reachable via the span),
            // so the macro engine re-lexes and pattern-matches at the
            // expand_one step below.
        }
        Expr::Case { arms, otherwise, .. } => {
            for a in arms {
                expand_expr(&mut a.cond, ctx, errs);
                for b in &mut a.body {
                    expand_expr(b, ctx, errs);
                }
            }
            if let Some(o) = otherwise {
                expand_expr(o, ctx, errs);
            }
        }
        Expr::Begin { body, .. } => {
            for b in body {
                expand_expr(b, ctx, errs);
            }
        }
        Expr::Let { value, .. } => expand_expr(value, ctx, errs),
        Expr::LocalMethod { body, .. } | Expr::Method { body, .. } => {
            for b in body {
                expand_expr(b, ctx, errs);
            }
        }
        Expr::Stmt(s) => expand_stmt(s, ctx, errs),
    }
}

/// If `e` is a macro-shaped form whose name might appear in the table,
/// return that name. Sprint 25 recognises:
///   - `Expr::MacroCall { name, … }` → that name (body-shaped macro
///     call, captured by the parser when `<name>(…) … end` was seen
///     and `<name>` was in the parser's known-macro set).
///   - `Expr::Call { callee: Ident(name), … }` → that name (call-shape
///     macro call, also handles statement-position macros expanded
///     in place).
fn macro_call_name(e: &Expr) -> Option<String> {
    match e {
        Expr::MacroCall { name, .. } => Some(name.clone()),
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Ident(_, n) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Run one expansion step: materialise the call site as fragments,
/// try each rule in order, substitute on first match, re-parse the
/// result. Sprint 18: first-match rule selection.
fn expand_one(
    e: &Expr,
    def: &MacroDef,
    ctx: &mut ExpansionCtx<'_>,
) -> Result<Expr, MacroError> {
    if def.rules.is_empty() {
        return Err(MacroError::MalformedDefinition {
            span: def.source_span,
            name: def.name.clone(),
            detail: "no rules".into(),
        });
    }
    // 1. Synthesise the call-site fragment sequence.
    let (call_text, call_frags) = call_site_fragments(e, ctx.source, &def.name)?;
    // 2. Try rules left-to-right; first match wins.
    let mut chosen: Option<(usize, Bindings)> = None;
    with_call_site_source(&call_text, || {
        for (idx, rule) in def.rules.iter().enumerate() {
            if let Some(b) = match_pattern(&rule.pattern, &call_frags) {
                chosen = Some((idx, b));
                return;
            }
        }
    });
    let (rule_idx, bindings) = chosen.ok_or_else(|| MacroError::NoApplicableRule {
        call_span: e.span(),
        name: def.name.clone(),
        rule_count: def.rules.len(),
    })?;
    let rule = &def.rules[rule_idx];
    // 3. Substitute → string + origins.
    let mut pv_names = collect_pattern_var_names(&rule.pattern);
    // WHY: a macro can refer to itself (and to other registered macros)
    // in its template — renaming those would break self-recursion and
    // macro-to-macro composition. Add every known macro name to the
    // no-rename set.
    for n in ctx.table.defs.keys() {
        pv_names.insert(n.clone());
    }
    // Sprint 18 hygiene: only Idents in binding positions
    // (`let <BIND>`, method/local-method param names) get renamed.
    // Compute that set by walking the template tree.
    let binders = collect_template_binders(&rule.template);
    let nonce = ctx.fresh_nonce();
    let out = substitute_with_binders(
        &rule.template,
        &bindings,
        nonce,
        &call_text,
        ctx.source,
        // Sprint 17: definition and use share a SourceMap (single file).
        // Cross-file macro use is DEFERRED to Sprint 19.
        e.span().file_id,
        &pv_names,
        &binders,
    );
    // 4. Re-lex + re-parse as an expression.
    let mut scratch = SourceMap::new();
    let scratch_id = scratch
        .add(
            std::path::PathBuf::from(format!("<macro-expand:{}>", def.name)),
            out.text.clone(),
        )
        .map_err(|e_| MacroError::ReparseFailed {
            call_span: e.span(),
            name: def.name.clone(),
            detail: format!("source map: {e_}"),
        })?;
    let toks = lex(&out.text, scratch_id);
    let mut parsed = parse_expr(&out.text, &toks).map_err(|d| MacroError::ReparseFailed {
        call_span: e.span(),
        name: def.name.clone(),
        detail: d.message,
    })?;
    // 5. Rewrite spans: replace every scratch-file span with the
    //    original-span recorded in `out.origins`. Spans we can't map
    //    fall back to the call-site span.
    rewrite_spans_expr(&mut parsed, &out.origins, scratch_id, e.span());
    // 6. Anchor the top-level node's span to the call site. Sub-trees
    //    keep their fine-grained provenance (template vs call) but the
    //    outer expression's span is uniform, which keeps recursive
    //    expansion (lexing the new Expr's span) consistent — otherwise
    //    a mixed template-vs-call span re-lexes garbage.
    set_top_span(&mut parsed, e.span());
    Ok(parsed)
}

/// Expand a definition macro: a top-level `define <word> … end` whose `<word>`
/// is a registered macro. Mirrors [`expand_one`] but re-parses the substituted
/// expansion as a top-level module **item** (a definition), not an expression.
fn expand_definition_macro(
    it: &Item,
    def: &MacroDef,
    ctx: &mut ExpansionCtx<'_>,
) -> Result<Item, MacroError> {
    if def.rules.is_empty() {
        return Err(MacroError::MalformedDefinition {
            span: def.source_span,
            name: def.name.clone(),
            detail: "no rules".into(),
        });
    }
    let sp = it.span();
    let (call_text, call_frags) = call_site_fragments_span(sp, ctx.source, &def.name)?;
    let mut chosen: Option<(usize, Bindings)> = None;
    with_call_site_source(&call_text, || {
        for (idx, rule) in def.rules.iter().enumerate() {
            if let Some(b) = match_pattern(&rule.pattern, &call_frags) {
                chosen = Some((idx, b));
                return;
            }
        }
    });
    let (rule_idx, bindings) = chosen.ok_or_else(|| MacroError::NoApplicableRule {
        call_span: sp,
        name: def.name.clone(),
        rule_count: def.rules.len(),
    })?;
    let rule = &def.rules[rule_idx];
    let mut pv_names = collect_pattern_var_names(&rule.pattern);
    for n in ctx.table.defs.keys() {
        pv_names.insert(n.clone());
    }
    let binders = collect_template_binders(&rule.template);
    let nonce = ctx.fresh_nonce();
    let out = substitute_with_binders(
        &rule.template,
        &bindings,
        nonce,
        &call_text,
        ctx.source,
        sp.file_id,
        &pv_names,
        &binders,
    );
    // Re-lex + re-parse the expansion as a top-level module item (a definition,
    // not an expression). Lex against the call-site file id so produced spans
    // reference a valid file (offsets are into the expansion buffer — fine for
    // lowering, which reads names/values from the AST, not source by span). Use
    // the canonical Rust parser directly so an installed `--parse-with-dylan`
    // override never routes this internal expansion through the partial Dylan
    // parser; seed the known macro names so body-shaped macro calls in the body
    // still parse.
    let toks = lex(&out.text, sp.file_id);
    let known: std::collections::HashSet<String> = ctx.table.defs.keys().cloned().collect();
    let module = nod_reader::parse_module_with_macros_rust(&out.text, &toks, None, &known).map_err(
        |ds| MacroError::ReparseFailed {
            call_span: sp,
            name: def.name.clone(),
            detail: ds
                .into_iter()
                .next()
                .map(|d| d.message)
                .unwrap_or_else(|| "definition-macro expansion did not parse".into()),
        },
    )?;
    module
        .items
        .into_iter()
        .next()
        .ok_or_else(|| MacroError::ReparseFailed {
            call_span: sp,
            name: def.name.clone(),
            detail: "definition-macro expansion produced no item".into(),
        })
}

fn set_top_span(e: &mut Expr, sp: Span) {
    match e {
        Expr::Integer(s, _)
        | Expr::Float(s, _)
        | Expr::String(s, _)
        | Expr::Char(s, _)
        | Expr::Bool(s, _)
        | Expr::Symbol(s, _)
        | Expr::Ident(s, _) => *s = sp,
        Expr::Call { span, .. }
        | Expr::BinOp { span, .. }
        | Expr::UnOp { span, .. }
        | Expr::Paren { span, .. }
        | Expr::If { span, .. }
        | Expr::Case { span, .. }
        | Expr::MacroCall { span, .. }
        | Expr::Begin { span, .. }
        | Expr::Let { span, .. }
        | Expr::LocalMethod { span, .. }
        | Expr::Method { span, .. } => *span = sp,
        Expr::Stmt(s) => match s.as_mut() {
            Statement::Expr(e2) => set_top_span(e2, sp),
            Statement::Let { span, .. }
            | Statement::Local { span, .. }
            | Statement::For { span, .. }
            | Statement::While { span, .. }
            | Statement::Until { span, .. }
            | Statement::Block { span, .. } => *span = sp,
        },
    }
}

/// Build a call-site fragment list for the AST node `e`. Tokens carry
/// their *original* user-space spans so pattern-bound subtrees retain
/// call-site provenance after substitution and re-parse.
///
/// Strategy: re-lex the user's source — but only the slice the AST node
/// covers. The lexer is called against the FULL source with a starting
/// offset of `e.span().lo`; tokens produced therefore have spans
/// relative to the call site's file, not relative to a slice. Then we
/// filter to tokens within the AST node's span and build fragments.
fn call_site_fragments(
    e: &Expr,
    source: &SourceMap,
    macro_name: &str,
) -> Result<(String, Vec<Fragment>), MacroError> {
    call_site_fragments_span(e.span(), source, macro_name)
}

/// Span-based variant of [`call_site_fragments`]. Definition macros expand an
/// `Item` (`define <word> … end`) whose call site is a span, not an `Expr`.
fn call_site_fragments_span(
    sp: Span,
    source: &SourceMap,
    macro_name: &str,
) -> Result<(String, Vec<Fragment>), MacroError> {
    let file_src = source.source(sp.file_id);
    // Re-lex the entire file, then keep tokens within `sp`. (Cheap; we
    // could partition once per file via a future cache.)
    let full_toks = lex(file_src, sp.file_id);
    let mut slice: Vec<Token> = full_toks
        .iter()
        .copied()
        .filter(|t| t.kind != TokenKind::Eof && t.span.lo >= sp.lo && t.span.hi <= sp.hi)
        .collect();
    // Append a synthetic EOF so `build_fragments` terminates.
    if let Some(last) = slice.last().copied() {
        slice.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(sp.file_id, last.span.hi, last.span.hi),
        });
    } else {
        return Err(MacroError::PatternMismatch {
            call_span: sp,
            name: macro_name.into(),
        });
    }
    let frags = build_fragments(&slice).map_err(|err| MacroError::ReparseFailed {
        call_span: sp,
        name: macro_name.into(),
        detail: format!("fragment build: {err}"),
    })?;
    Ok((file_src.to_string(), frags))
}

/// Walk the parsed expansion AST and rewrite every span that points
/// into `scratch_id` to its origin span (looked up by buffer offset
/// in `origins`). Spans we can't resolve fall back to `fallback`.
fn rewrite_spans_expr(
    e: &mut Expr,
    origins: &[TokenOrigin],
    scratch_id: FileId,
    fallback: Span,
) {
    let map = |sp: &mut Span| {
        if sp.file_id != scratch_id {
            return;
        }
        // Single-token case: the AST node's span fits inside ONE origin.
        if let Some(o) = origins
            .iter()
            .find(|o| o.buf_lo <= sp.lo && sp.hi <= o.buf_hi)
        {
            *sp = o.original_span;
            return;
        }
        // Multi-token case: synthesize a span using the origin that
        // covers buf[sp.lo] for the lo end, and the one that covers
        // buf[sp.hi-1] for the hi end. Both must come from the same
        // file (template-introduced span vs pattern-bound span). If
        // they straddle files we fall back to the lo origin's span.
        let lo_origin = origins.iter().find(|o| o.buf_lo <= sp.lo && sp.lo < o.buf_hi);
        let hi_origin = origins
            .iter()
            .find(|o| sp.hi > o.buf_lo && sp.hi <= o.buf_hi)
            .or_else(|| origins.iter().filter(|o| o.buf_hi <= sp.hi).max_by_key(|o| o.buf_hi));
        match (lo_origin, hi_origin) {
            (Some(lo), Some(hi))
                if lo.original_span.file_id == hi.original_span.file_id
                    && lo.original_span.lo <= hi.original_span.hi =>
            {
                *sp = Span::new(
                    lo.original_span.file_id,
                    lo.original_span.lo,
                    hi.original_span.hi,
                );
            }
            (Some(lo), Some(_)) => {
                // Sprint 18: the lo and hi origins span both call-site
                // and template positions where call-site lo > template
                // hi (e.g. a BinOp joining a call-bound operand with a
                // template-introduced operator). Fall back to the lo
                // origin's whole span; the more granular split is
                // available on the children individually.
                *sp = lo.original_span;
            }
            (Some(lo), _) => {
                *sp = lo.original_span;
            }
            _ => {
                *sp = fallback;
            }
        }
    };
    walk_expr_spans(e, &mut |sp| map(sp));
}

fn walk_expr_spans(e: &mut Expr, f: &mut dyn FnMut(&mut Span)) {
    match e {
        Expr::Integer(sp, _)
        | Expr::Float(sp, _)
        | Expr::String(sp, _)
        | Expr::Char(sp, _)
        | Expr::Bool(sp, _)
        | Expr::Symbol(sp, _)
        | Expr::Ident(sp, _) => f(sp),
        Expr::Call { span, callee, args } => {
            f(span);
            walk_expr_spans(callee, f);
            for a in args {
                walk_expr_spans(a, f);
            }
        }
        Expr::BinOp { span, lhs, rhs, .. } => {
            f(span);
            walk_expr_spans(lhs, f);
            walk_expr_spans(rhs, f);
        }
        Expr::UnOp { span, operand, .. } => {
            f(span);
            walk_expr_spans(operand, f);
        }
        Expr::Paren { span, inner } => {
            f(span);
            walk_expr_spans(inner, f);
        }
        Expr::If { span, cond, then_, else_ } => {
            f(span);
            walk_expr_spans(cond, f);
            walk_expr_spans(then_, f);
            if let Some(b) = else_ {
                walk_expr_spans(b, f);
            }
        }
        Expr::MacroCall { span, .. } => f(span),
        Expr::Case { span, arms, otherwise } => {
            f(span);
            for a in arms {
                walk_expr_spans(&mut a.cond, f);
                for b in &mut a.body {
                    walk_expr_spans(b, f);
                }
            }
            if let Some(o) = otherwise {
                walk_expr_spans(o, f);
            }
        }
        Expr::Begin { span, body } => {
            f(span);
            for b in body {
                walk_expr_spans(b, f);
            }
        }
        Expr::Let { span, value, .. } => {
            f(span);
            walk_expr_spans(value, f);
        }
        Expr::LocalMethod { span, params, body, .. }
        | Expr::Method { span, params, body, .. } => {
            f(span);
            for p in params {
                walk_param_spans(p, f);
            }
            for b in body {
                walk_expr_spans(b, f);
            }
        }
        Expr::Stmt(s) => walk_stmt_spans(s, f),
    }
}

fn walk_param_spans(p: &mut Param, f: &mut dyn FnMut(&mut Span)) {
    f(&mut p.span);
    if let Some(t) = &mut p.type_ {
        walk_expr_spans(t, f);
    }
}

fn walk_stmt_spans(s: &mut Statement, f: &mut dyn FnMut(&mut Span)) {
    match s {
        Statement::Expr(e) => walk_expr_spans(e, f),
        Statement::Let { span, value, .. } => {
            f(span);
            walk_expr_spans(value, f);
        }
        Statement::Local { span, methods } => {
            f(span);
            for m in methods {
                f(&mut m.span);
                for s2 in &mut m.body {
                    walk_stmt_spans(s2, f);
                }
            }
        }
        Statement::For { span, body, finally_, .. } => {
            f(span);
            for s2 in body {
                walk_stmt_spans(s2, f);
            }
            for s2 in finally_ {
                walk_stmt_spans(s2, f);
            }
        }
        Statement::While { span, cond, body } | Statement::Until { span, cond, body } => {
            f(span);
            walk_expr_spans(cond, f);
            for s2 in body {
                walk_stmt_spans(s2, f);
            }
        }
        Statement::Block {
            span, body, handlers, cleanup, afterwards, ..
        } => {
            f(span);
            for s2 in body {
                walk_stmt_spans(s2, f);
            }
            for h in handlers {
                f(&mut h.span);
                for s2 in &mut h.body {
                    walk_stmt_spans(s2, f);
                }
            }
            for s2 in cleanup {
                walk_stmt_spans(s2, f);
            }
            for s2 in afterwards {
                walk_stmt_spans(s2, f);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Phase G — top-level convenience: parse + collect + expand in one shot
// ─────────────────────────────────────────────────────────────────────────

/// Convenience driver: take the source text + parsed Module, collect
/// macros from the module, expand call sites, and return the mutated
/// Module. Used by the `dump-expanded` driver helper in `nod-sema`.
pub fn collect_and_expand(
    module: &mut Module,
    source: &SourceMap,
) -> Result<MacroTable, Vec<MacroError>> {
    let mut table = MacroTable::default();
    collect_macros(module, source, &mut table)?;
    expand_module(module, &table, source)?;
    Ok(table)
}

// ─────────────────────────────────────────────────────────────────────────
// Re-exports & unused-warning shims
// ─────────────────────────────────────────────────────────────────────────

// WHY: silence "unused" on BinOp / parse_module / ReturnSig — we re-export the
// nod-reader types these refer to elsewhere via paths, and the explicit
// import is here to keep them in the public type surface without
// trapping rustc.
#[doc(hidden)]
#[allow(dead_code)]
fn _surface_keepalive() -> (Option<BinOp>, Option<ReturnSig>) {
    let _f: fn(&str, &[Token], Option<&nod_reader::Preamble>) -> _ = parse_module;
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nod_reader::{lex, scan_preamble};

    #[test]
    fn parse_macro_def_smoke() {
        let src = "\
define macro unless
  { unless ?cond:expression ?body:expression end } => { if (~ ?cond) ?body end }
end macro;
";
        let mut sm = SourceMap::new();
        let id = sm.add("<t>", src.to_string()).unwrap();
        let toks = lex(src, id);
        let pre = scan_preamble(src);
        let m = parse_module(src, &toks, pre.as_ref()).expect("parse");
        let Item::DefineMacro { name, body_fragments, span } = &m.items[0] else {
            panic!("expected DefineMacro");
        };
        let def = parse_macro_def(name, body_fragments, *span, &sm).expect("parse_macro_def");
        assert_eq!(def.name, "unless");
        assert_eq!(def.rules.len(), 1);
        // Pattern: [Lit(unless), Var(cond,Expression), Var(body,Expression), Lit(end)]
        let p = &def.rules[0].pattern;
        assert_eq!(p.len(), 4);
        assert!(matches!(p[1], PatternElem::Variable { kind: PatternKind::Expression, .. }));
    }
}
