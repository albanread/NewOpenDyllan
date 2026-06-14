//! `Fragment` — paren/bracket/brace-grouped token tree.
//!
//! Sprint 03: anchors token nesting for the infix parser. Mirrors the role
//! of `dfmc/reader/fragments.dylan` upstream.

use crate::span::Span;
use crate::token::{Token, TokenKind};

#[derive(Clone, Debug)]
pub enum Fragment {
    Token(Token),
    Group {
        open: Token,
        close: Token,
        kind: GroupKind,
        body: Vec<Fragment>,
        span: Span,
    },
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum GroupKind {
    Paren,
    Bracket,
    Brace,
    HashParen,
    HashBracket,
    HashBrace,
}

impl GroupKind {
    pub fn name(self) -> &'static str {
        match self {
            GroupKind::Paren => "PAREN",
            GroupKind::Bracket => "BRACKET",
            GroupKind::Brace => "BRACE",
            GroupKind::HashParen => "HASH_PAREN",
            GroupKind::HashBracket => "HASH_BRACKET",
            GroupKind::HashBrace => "HASH_BRACE",
        }
    }

    fn from_open(kind: TokenKind) -> Option<(GroupKind, TokenKind)> {
        match kind {
            TokenKind::LParen => Some((GroupKind::Paren, TokenKind::RParen)),
            TokenKind::LBracket => Some((GroupKind::Bracket, TokenKind::RBracket)),
            TokenKind::LBrace => Some((GroupKind::Brace, TokenKind::RBrace)),
            TokenKind::HashLParen => Some((GroupKind::HashParen, TokenKind::RParen)),
            TokenKind::HashLBracket => Some((GroupKind::HashBracket, TokenKind::RBracket)),
            TokenKind::HashLBrace => Some((GroupKind::HashBrace, TokenKind::RBrace)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum FragmentError {
    Unclosed { open: Token, expected: TokenKind },
    Mismatched { open: Token, found: Token, expected: TokenKind },
    StrayClose { close: Token },
}

impl std::fmt::Display for FragmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FragmentError::Unclosed { open, expected } => write!(
                f,
                "unclosed {:?} group; expected matching {:?}",
                open.kind, expected
            ),
            FragmentError::Mismatched { open, found, expected } => write!(
                f,
                "mismatched group: {:?} closed by {:?}, expected {:?}",
                open.kind, found.kind, expected
            ),
            FragmentError::StrayClose { close } => {
                write!(f, "stray closing delimiter {:?}", close.kind)
            }
        }
    }
}

impl std::error::Error for FragmentError {}

pub fn build_fragments(tokens: &[Token]) -> Result<Vec<Fragment>, FragmentError> {
    let mut cursor = 0usize;
    build_until(tokens, &mut cursor, None)
}

fn build_until(
    tokens: &[Token],
    cursor: &mut usize,
    closer: Option<(Token, TokenKind, GroupKind)>,
) -> Result<Vec<Fragment>, FragmentError> {
    let mut out = Vec::new();
    while let Some(tok) = tokens.get(*cursor).copied() {
        if tok.kind == TokenKind::Eof {
            if let Some((open, expected, _)) = closer {
                return Err(FragmentError::Unclosed { open, expected });
            }
            *cursor += 1;
            return Ok(out);
        }
        if let Some((_, expected, _)) = closer
            && tok.kind == expected
        {
            *cursor += 1;
            return Ok(out);
        }
        if let Some((_, expected, _)) = closer
            && is_closer(tok.kind)
        {
            let open = closer.map(|(o, _, _)| o).expect("closer set");
            return Err(FragmentError::Mismatched {
                open,
                found: tok,
                expected,
            });
        }
        if is_closer(tok.kind) {
            return Err(FragmentError::StrayClose { close: tok });
        }
        if let Some((gk, expected)) = GroupKind::from_open(tok.kind) {
            *cursor += 1;
            let open = tok;
            let body = build_until(tokens, cursor, Some((open, expected, gk)))?;
            let close = tokens
                .get(cursor.checked_sub(1).unwrap_or(0))
                .copied()
                .expect("closer consumed");
            let span = Span::new(open.span.file_id, open.span.lo, close.span.hi);
            out.push(Fragment::Group {
                open,
                close,
                kind: gk,
                body,
                span,
            });
        } else {
            out.push(Fragment::Token(tok));
            *cursor += 1;
        }
    }
    if let Some((open, expected, _)) = closer {
        return Err(FragmentError::Unclosed { open, expected });
    }
    Ok(out)
}

fn is_closer(k: TokenKind) -> bool {
    matches!(k, TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace)
}
