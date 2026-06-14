//! Token kinds and the `Token` type. Surface defined in `specs/01-lexer.md` §2.
//!
//! `Token` carries kind + span. The token text is derivable from the span
//! via `SourceMap::slice` — keeping the `Token` lifetime-free simplifies
//! the parser and the IDE.

use crate::span::Span;

/// One lexical token.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// The full Dylan token taxonomy from `specs/01-lexer.md` §2.
///
/// IMPORTANT (§2.1): the only hard-reserved words at the lexer level are
/// `define`, `end`, `otherwise`. Everything else — `if`, `let`, `class`,
/// `method`, `library`, `module`, `for`, `while`, `sealed`, `open`, … —
/// is a regular [`TokenKind::Ident`]. The parser classifies them per module.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum TokenKind {
    // — Identifiers and the three hard reserveds (§2.1)
    Ident,
    KwDefine,
    KwEnd,
    KwOtherwise,
    /// `\+`, `\=`, `\if`, etc. — operator-or-keyword used as a value name.
    /// The leading `\` is in `span.text()` but the parser strips it.
    EscapedIdent,

    // — Hash-prefixed (§2.2)
    HashTrue,
    HashFalse,
    HashLParen,
    HashLBracket,
    HashLBrace,
    HashHash,
    HashRest,
    HashKey,
    HashAllKeys,
    HashNext,
    HashIncludeMarker, // `#include`-style — currently invalid but recognised
    /// `#"..."` — symbol literal.
    Symbol,
    /// `#:foo`, `#:!bang` — hash-keyword literal (§2.2).
    HashKeyword,

    IntegerBin,
    IntegerOct,
    IntegerHex,

    // — Trailing-colon keywords (§2.3)
    KeywordColon,

    // — Numerics (§2.4)
    Integer,
    Float,
    Ratio,

    // — Strings & chars (§2.5, §2.6)
    String,
    StringMulti,
    StringRaw,
    Char,

    // — Operators & punctuators (§2.7)
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Semicolon,
    Dot,
    Ellipsis,

    Colon,
    ColonColon,
    ColonEqual,
    Equal,
    EqualEqual,
    Arrow, // `=>`
    Tilde,
    TildeEqual,
    TildeEqualEqual,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Amp,
    Bar,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Query,
    QueryQuery,
    QueryEqual,
    QueryAt,

    // — End and errors
    Eof,
    /// Recoverable error. The span covers the offending bytes; the lexer
    /// resynchronises and keeps emitting tokens.
    Invalid,
}

impl TokenKind {
    /// Screaming-snake-case name, used in the `format_tokens` dump.
    pub fn name(self) -> &'static str {
        use TokenKind::*;
        match self {
            Ident => "IDENT",
            KwDefine => "KW_DEFINE",
            KwEnd => "KW_END",
            KwOtherwise => "KW_OTHERWISE",
            EscapedIdent => "ESCAPED_IDENT",
            HashTrue => "HASH_TRUE",
            HashFalse => "HASH_FALSE",
            HashLParen => "HASH_LPAREN",
            HashLBracket => "HASH_LBRACKET",
            HashLBrace => "HASH_LBRACE",
            HashHash => "HASH_HASH",
            HashRest => "HASH_REST",
            HashKey => "HASH_KEY",
            HashAllKeys => "HASH_ALL_KEYS",
            HashNext => "HASH_NEXT",
            HashIncludeMarker => "HASH_INCLUDE_MARKER",
            Symbol => "SYMBOL",
            HashKeyword => "HASH_KEYWORD",
            IntegerBin => "INTEGER_BIN",
            IntegerOct => "INTEGER_OCT",
            IntegerHex => "INTEGER_HEX",
            KeywordColon => "KEYWORD_COLON",
            Integer => "INTEGER",
            Float => "FLOAT",
            Ratio => "RATIO",
            String => "STRING",
            StringMulti => "STRING_MULTI",
            StringRaw => "STRING_RAW",
            Char => "CHAR",
            LParen => "LPAREN",
            RParen => "RPAREN",
            LBracket => "LBRACKET",
            RBracket => "RBRACKET",
            LBrace => "LBRACE",
            RBrace => "RBRACE",
            Comma => "COMMA",
            Semicolon => "SEMICOLON",
            Dot => "DOT",
            Ellipsis => "ELLIPSIS",
            Colon => "COLON",
            ColonColon => "COLON_COLON",
            ColonEqual => "COLON_EQUAL",
            Equal => "EQUAL",
            EqualEqual => "EQUAL_EQUAL",
            Arrow => "ARROW",
            Tilde => "TILDE",
            TildeEqual => "TILDE_EQUAL",
            TildeEqualEqual => "TILDE_EQUAL_EQUAL",
            Plus => "PLUS",
            Minus => "MINUS",
            Star => "STAR",
            Slash => "SLASH",
            Caret => "CARET",
            Amp => "AMP",
            Bar => "BAR",
            Less => "LESS",
            Greater => "GREATER",
            LessEqual => "LESS_EQUAL",
            GreaterEqual => "GREATER_EQUAL",
            Query => "QUERY",
            QueryQuery => "QUERY_QUERY",
            QueryEqual => "QUERY_EQUAL",
            QueryAt => "QUERY_AT",
            Eof => "EOF",
            Invalid => "INVALID",
        }
    }
}
