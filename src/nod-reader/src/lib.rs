//! `nod-reader` — Dylan lexer, AST, and parser.
//!
//! Sprint 02: lexer + source map.
//! Sprint 03: fragments + expression parser.
//! Sprint 04: top-level forms + statement parser + pretty-printer.
//!
//! See `specs/01-lexer.md` for the lexer contract; `SPRINTS.md` §117–178
//! for the parser sketch.

pub mod ast;
pub mod format;
pub mod format_dylan;
pub mod fragments;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;

pub use ast::{
    BinOp, Binder, CaseArm, ExceptionClause, Expr, ForClause, ImportSet, ImportSpec, Item,
    LibraryUseClause, LocalMethodDecl, Modifier, Module, ModuleUseClause, Param, ReturnRest,
    ReturnSig, ReturnValue, SlotAllocation, SlotDef, Statement, UnOp, format_ast,
    format_ast_module,
};
pub use format::format_tokens;
pub use format_dylan::format_dylan;
pub use fragments::{Fragment, FragmentError, GroupKind, build_fragments};
pub use lexer::{LexFn, Preamble, has_lex_override, lex, lex_rust, scan_preamble, set_lex_override};
pub use parser::{
    Diagnostic, ParseFn, has_parse_override, parse_expr, parse_expr_with_macros, parse_module,
    parse_module_with_macros, parse_module_with_macros_rust, parse_top_level_exprs,
    set_parse_override,
};
pub use span::{FileId, SourceMap, SourceMapError, Span};
pub use token::{Token, TokenKind};
