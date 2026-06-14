//! Sprint 51e — `DylanAst` (wire tree) → `nod_reader::ast::Module`.
//!
//! This is the payoff of the AST wire format: turn the Dylan-side
//! parser's output into the *canonical* Rust AST, so the Dylan parser
//! can **replace** `parse_module` for the files it fully understands.
//! Everything it can't yet reconstruct returns [`Unsupported`], and the
//! `--parse-with-dylan` driver path falls back to the Rust parser for
//! that whole file. The bar is **byte-identical** `format_ast_module`
//! output vs the Rust parser — so "translated" genuinely means "the two
//! parsers agree on the AST," not merely "didn't crash."
//!
//! ## What v1 translates
//!
//! - The module header (`Module: foo`) — re-scanned host-side with
//!   [`nod_reader::scan_preamble`], because the Dylan parser treats the
//!   header as ordinary body forms (a `SymbolLit`/`VariableRef` pair).
//!   Those leading forms are skipped by source offset.
//! - `define function` / `define method` with required params, a return
//!   spec, and a body of expression statements.
//! - Expressions: identifiers, string literals, integer/float/boolean
//!   literals, and calls.
//!
//! Anything else — modifiers on a definition, `#rest`/`#key` params,
//! `let`/`if`/`while`/… statement bodies, binary operators, classes,
//! generics, macros — is [`Unsupported`] and triggers fallback. Each
//! increment grows this set; the translation-coverage harness measures
//! how many corpus files take the Dylan path.
//!
//! Spans don't matter to the comparison: `format_ast_module` prints no
//! spans, only names / structure / values / operators / modifiers. We
//! still thread real spans through (recovered from the wire) so the
//! resulting `Module` is usable downstream, not just dump-equal.

use crate::dylan_parse_wire::{DylanAst, Kind};
use nod_reader::ast::{
    Binder, Expr, Item, Modifier, Module, Param, ReturnRest, ReturnSig, ReturnValue,
    SlotAllocation, SlotDef, Statement,
};
use nod_reader::span::{FileId, Span};

/// A construct the v1 translator doesn't reconstruct yet. Carries a
/// short reason for the `--parse-with-dylan` fallback log.
#[derive(Debug, Clone)]
pub struct Unsupported(pub String);

fn unsupported<T>(msg: impl Into<String>) -> Result<T, Unsupported> {
    Err(Unsupported(msg.into()))
}

fn span_of(node: &DylanAst) -> Span {
    Span::new(FileId(0), node.span_lo, node.span_hi)
}

/// `&src[lo..hi]`, bounds-checked. Returns `Unsupported` on a bad span
/// rather than panicking — a malformed wire record shouldn't crash the
/// driver, just decline the Dylan path.
fn slice<'a>(src: &'a str, node: &DylanAst) -> Result<&'a str, Unsupported> {
    let lo = node.span_lo as usize;
    let hi = node.span_hi as usize;
    src.get(lo..hi)
        .ok_or_else(|| Unsupported(format!("span {lo}..{hi} out of bounds / not a char boundary")))
}

/// Translate the whole wire tree into a [`Module`]. `src` is the exact
/// source the Dylan parser was handed (the host re-reads it for every
/// leaf payload). Returns `Unsupported` if any item isn't reconstructible.
pub fn to_ast_module(tree: &DylanAst, src: &str) -> Result<Module, Unsupported> {
    if tree.kind != Kind::Body {
        return unsupported(format!("top node is {:?}, expected Body", tree.kind));
    }

    // The Dylan parser doesn't model the `Key: value` header — it lexes
    // those lines as ordinary forms. Re-scan the header host-side and
    // skip every top-level form that starts inside the preamble.
    let preamble = nod_reader::scan_preamble(src);
    let header = preamble
        .as_ref()
        .map(|p| p.entries.clone())
        .unwrap_or_default();
    let body_start = preamble.as_ref().map(|p| p.end).unwrap_or(0);

    // Sprint 51e — the Dylan-in-Dylan parser now honours the `Precedence: c`
    // module header itself (see dylan-lex-shim.dylan `precedence-c-header?`
    // + dylan-parser.dylan's C-style ladder), so a C-precedence file emits a
    // wire tree whose operator nesting already matches nod-reader's
    // C-precedence path. We no longer decline these files here; the
    // `BinaryOp` arm below reconstructs the nesting faithfully (the dump
    // diffs tree shape, and both parsers now build the same shape). The
    // prior wholesale `Precedence: c` reject is gone with the pragma gap.

    let mut items = Vec::new();
    for child in &tree.children {
        // Skip the header forms the Dylan parser lexed as ordinary
        // constituents (`Module: foo` → a SymbolLit/VariableRef pair):
        // they are spanned and lie entirely within the preamble. An
        // UNSPANNED node (span_hi == 0) is NEVER a header form — it's an
        // `Error` or some unspanned construct — and must not be silently
        // dropped, or we'd emit a too-empty Module instead of an honest
        // fallback. (This bit us on stdlib-min/ide_win_calls, whose
        // `define macro`/`define c-function` forms emit as `Error 0..0`.)
        if child.span_hi != 0 && child.span_hi <= body_start {
            continue;
        }
        if child.kind == Kind::Error {
            return unsupported("Dylan parser emitted an Error node");
        }
        items.push(translate_item(child, src)?);
    }

    Ok(Module {
        span: span_of(tree),
        header,
        items,
    })
}

/// Collect the leading `Modifier` children of a definition node into a
/// `Vec<Modifier>` in source order. Each is a leaf whose span is the
/// adjective word (`sealed`/`open`/…); `&src[span]` maps to `ast::Modifier`
/// via `Modifier::from_word`. An unknown adjective is `Unsupported` (the
/// Rust parser would reject it too). The order matches the Rust parser's
/// (both collect in source order), so the `(Modifiers …)` dump agrees.
fn collect_modifiers(node: &DylanAst, src: &str) -> Result<Vec<Modifier>, Unsupported> {
    let mut mods = Vec::new();
    for child in &node.children {
        if child.kind == Kind::Modifier {
            let word = slice(src, child)?;
            let m = Modifier::from_word(word)
                .ok_or_else(|| Unsupported(format!("unknown modifier {word:?}")))?;
            mods.push(m);
        }
    }
    Ok(mods)
}

fn translate_item(node: &DylanAst, src: &str) -> Result<Item, Unsupported> {
    match node.kind {
        Kind::DefineFunction | Kind::DefineMethod => translate_def(node, src),
        Kind::DefineGeneric => translate_generic(node, src),
        Kind::DefineClass => translate_class(node, src),
        Kind::DefineBinding => translate_binding(node, src),
        // A free-standing top-level expression (e.g. `s00(1) + t00(0)`)
        // → `Item::Expr`, exactly as the Rust parser's `parse_top_item`
        // wraps a non-`define` form. `translate_expr` reconstructs the
        // BinaryOp / Call / literal; the header forms the Dylan parser
        // lexed as constituents were already skipped (preamble offset)
        // and an `Error` node was already rejected, both up in
        // `to_ast_module`, so reaching here means a genuine expression.
        _ => Ok(Item::Expr(translate_expr(node, src)?)),
    }
}

/// `define class NAME (supers) slot… end` → `Item::DefineClass`. Wire
/// children: `DefName` (class name), then super exprs and `SlotSpec`s
/// (dispatched by kind).
fn translate_class(node: &DylanAst, src: &str) -> Result<Item, Unsupported> {
    // Sprint 51e.4 — modifiers (sealed/open/abstract/…) now arrive as
    // leading `Modifier` children on the wire; collect them in source
    // order. The `Kind::Modifier => {}` arm below skips them in the
    // child walk so they aren't mistaken for superclass exprs.
    let modifiers = collect_modifiers(node, src)?;
    let mut name: Option<String> = None;
    let mut supers: Vec<Expr> = Vec::new();
    let mut slots: Vec<SlotDef> = Vec::new();
    for child in &node.children {
        match child.kind {
            Kind::Modifier => {}
            Kind::DefName => name = Some(slice(src, child)?.to_string()),
            Kind::SlotSpec => slots.push(translate_slot(child, src)?),
            _ => supers.push(translate_expr(child, src)?),
        }
    }
    let name = name.ok_or_else(|| Unsupported("class has no DefName".into()))?;
    Ok(Item::DefineClass {
        span: span_of(node),
        modifiers,
        name,
        supers,
        slots,
    })
}

/// `define [modifiers] generic NAME (params) => (returns);` →
/// `Item::DefineGeneric`. Wire children (dispatched by kind): `Modifier`*,
/// `DefName`, `ParamList`, optional `ReturnSpec`. No body (a generic has
/// no `end`). Mirrors `translate_def` minus the Body.
fn translate_generic(node: &DylanAst, src: &str) -> Result<Item, Unsupported> {
    let modifiers = collect_modifiers(node, src)?;
    let mut name: Option<String> = None;
    let mut params: Vec<Param> = Vec::new();
    let mut return_: Option<ReturnSig> = None;
    for child in &node.children {
        match child.kind {
            Kind::Modifier => {}
            Kind::DefName => name = Some(slice(src, child)?.to_string()),
            Kind::ParamList => params = translate_param_list(child, src)?,
            Kind::ReturnSpec => return_ = Some(translate_return_spec(child, src)?),
            other => return unsupported(format!("unexpected generic child {other:?}")),
        }
    }
    let name = name.ok_or_else(|| Unsupported("generic has no DefName".into()))?;
    Ok(Item::DefineGeneric {
        span: span_of(node),
        modifiers,
        name,
        params,
        return_,
    })
}

/// `define constant`/`variable NAME [:: TYPE] = INIT` →
/// `Item::DefineConstant`/`DefineVariable`. The wire node's span is the
/// `constant`/`variable` keyword (selecting which Item); its single
/// child is the binding-list `Body` holding a `BinaryOp(binder = init)`,
/// decoded by the shared [`local_decl_parts`]. Modifiers aren't on the
/// wire yet (51e.4) → fall back if the source shows one before the
/// keyword. Only a single, simple/typed binder is reconstructed; a
/// destructuring `define constant (a, b) = …` falls back.
fn translate_binding(node: &DylanAst, src: &str) -> Result<Item, Unsupported> {
    // Sprint 51e.4 — `define [modifiers] constant`/`variable`; modifiers
    // arrive as leading `Modifier` children (collected here), and
    // `local_decl_parts` finds the binding Body by kind, past them.
    let modifiers = collect_modifiers(node, src)?;
    let is_constant = match slice(src, node)? {
        "constant" => true,
        "variable" => false,
        other => return unsupported(format!("define-binding word {other:?}")),
    };
    let parts = local_decl_parts(node, src)?;
    if parts.binders.len() != 1 {
        return unsupported("define binding: destructuring binder");
    }
    let binder = parts.binders.into_iter().next().unwrap();
    let span = span_of(node);
    Ok(if is_constant {
        Item::DefineConstant {
            span,
            modifiers,
            name: binder.name,
            type_: binder.type_,
            value: parts.value,
        }
    } else {
        Item::DefineVariable {
            span,
            modifiers,
            name: binder.name,
            type_: binder.type_,
            value: parts.value,
        }
    })
}

/// One `SlotSpec` → `SlotDef`. Children are kind-tagged: `DefName`
/// (name), `SlotAlloc` (allocation adjective), `SlotInitKw`
/// (init-keyword, host strips the trailing `:`), `SlotRequired`
/// (required-init-keyword marker), `SlotType`/`SlotInit` (wrapped exprs).
fn translate_slot(node: &DylanAst, src: &str) -> Result<SlotDef, Unsupported> {
    let mut name: Option<String> = None;
    let mut allocation = SlotAllocation::Instance;
    let mut init_keyword: Option<String> = None;
    let mut required_init_keyword = false;
    let mut type_: Option<Expr> = None;
    let mut init_value: Option<Expr> = None;
    for child in &node.children {
        match child.kind {
            Kind::DefName => name = Some(slice(src, child)?.to_string()),
            Kind::SlotAlloc => {
                allocation = match slice(src, child)? {
                    "class" => SlotAllocation::Class,
                    "each-subclass" => SlotAllocation::EachSubclass,
                    "virtual" => SlotAllocation::Virtual,
                    "constant" => SlotAllocation::Constant,
                    other => return unsupported(format!("slot allocation {other:?}")),
                };
            }
            Kind::SlotInitKw => {
                init_keyword = Some(slice(src, child)?.trim_end_matches(':').to_string());
            }
            Kind::SlotRequired => required_init_keyword = true,
            Kind::SlotType => {
                let t = child
                    .children
                    .first()
                    .ok_or_else(|| Unsupported("SlotType has no child".into()))?;
                type_ = Some(translate_expr(t, src)?);
            }
            Kind::SlotInit => {
                let v = child
                    .children
                    .first()
                    .ok_or_else(|| Unsupported("SlotInit has no child".into()))?;
                init_value = Some(translate_expr(v, src)?);
            }
            other => return unsupported(format!("slot child {other:?}")),
        }
    }
    let name = name.ok_or_else(|| Unsupported("slot has no name".into()))?;
    Ok(SlotDef {
        span: span_of(node),
        name,
        type_,
        init_value,
        init_keyword,
        required_init_keyword,
        setter: None,
        allocation,
    })
}

/// Shared translation for `DefineFunction` / `DefineMethod`, whose wire
/// children are (in any order, dispatched by kind): `DefName`,
/// `ParamList`, optional `ReturnSpec`, `Body`.
fn translate_def(node: &DylanAst, src: &str) -> Result<Item, Unsupported> {
    // Sprint 51e.4 — modifiers (sealed/open/inline/…) arrive as leading
    // `Modifier` children; collect them in source order. The
    // `Kind::Modifier => {}` arm skips them in the dispatch walk.
    let modifiers = collect_modifiers(node, src)?;

    let mut name: Option<String> = None;
    let mut params: Vec<Param> = Vec::new();
    let mut return_: Option<ReturnSig> = None;
    let mut body: Option<Vec<Statement>> = None;

    for child in &node.children {
        match child.kind {
            Kind::Modifier => {}
            Kind::DefName => name = Some(slice(src, child)?.to_string()),
            Kind::ParamList => params = translate_param_list(child, src)?,
            Kind::ReturnSpec => return_ = Some(translate_return_spec(child, src)?),
            Kind::Body => body = Some(translate_body(child, src)?),
            other => return unsupported(format!("unexpected definition child {other:?}")),
        }
    }

    let name = name.ok_or_else(|| Unsupported("definition has no DefName".into()))?;
    let body = body.ok_or_else(|| Unsupported("definition has no Body".into()))?;
    let span = span_of(node);

    Ok(match node.kind {
        Kind::DefineFunction => Item::DefineFunction {
            span,
            modifiers,
            name,
            params,
            return_,
            body,
        },
        Kind::DefineMethod => Item::DefineMethod {
            span,
            modifiers,
            name,
            params,
            return_,
            body,
        },
        _ => unreachable!("translate_def only called for function/method"),
    })
}

fn translate_param_list(node: &DylanAst, src: &str) -> Result<Vec<Param>, Unsupported> {
    let mut params = Vec::new();
    for child in &node.children {
        match child.kind {
            Kind::Param => {
                let span = span_of(child);
                let name = slice(src, child)?.to_string();
                let type_ = match child.children.first() {
                    Some(t) => Some(translate_expr(t, src)?),
                    None => None,
                };
                params.push(Param { span, name, type_ });
            }
            Kind::VarMarker => {
                return unsupported("param list has #rest/#key/#all-keys/#next");
            }
            other => return unsupported(format!("unexpected param-list child {other:?}")),
        }
    }
    Ok(params)
}

fn translate_return_spec(node: &DylanAst, src: &str) -> Result<ReturnSig, Unsupported> {
    let mut values = Vec::new();
    // v1 always declines `#rest` returns (the VarMarker arm below bails),
    // so the reconstructed rest is always absent.
    let rest: Option<ReturnRest> = None;
    for child in &node.children {
        match child.kind {
            Kind::ReturnValue => {
                let span = span_of(child);
                // A type child present → `name :: type` (name = span).
                // No child → a bare type like `<integer>` → the Dylan
                // parser stored the type AS the token, so name = None
                // and type = Ident(span). See DYLAN_AST_WIRE.md row 30.
                match child.children.first() {
                    Some(t) => values.push(ReturnValue {
                        span,
                        name: Some(slice(src, child)?.to_string()),
                        type_: Some(translate_expr(t, src)?),
                    }),
                    None => {
                        let ident = Expr::Ident(span, slice(src, child)?.to_string());
                        values.push(ReturnValue {
                            span,
                            name: None,
                            type_: Some(ident),
                        });
                    }
                }
            }
            Kind::VarMarker => return unsupported("return spec has #rest"),
            other => return unsupported(format!("unexpected return-spec child {other:?}")),
        }
    }
    Ok(ReturnSig {
        span: span_of(node),
        values,
        rest,
    })
}

/// A function/method body Body → a `Vec<Statement>`. A `LocalDecl`
/// constituent is a `Statement::Let`; everything else is a translatable
/// expression wrapped in `Statement::Expr` (an `if` at statement
/// position becomes `Statement::Expr(Expr::If)`, matching the Rust
/// parser).
fn translate_body(node: &DylanAst, src: &str) -> Result<Vec<Statement>, Unsupported> {
    translate_stmts(&node.children, src)
}

/// A sequence of body constituents → `Vec<Statement>`. `LocalDecl` →
/// `Statement::Let`; a `Statement` node → the matching statement form
/// (`while`/`until` → loops, `if` → `Stmt(Expr::If)`); everything else
/// is a `Statement::Expr`.
fn translate_stmts(children: &[DylanAst], src: &str) -> Result<Vec<Statement>, Unsupported> {
    let mut stmts = Vec::new();
    for child in children {
        match child.kind {
            Kind::LocalDecl => stmts.push(translate_local_decl(child, src)?),
            Kind::Statement => stmts.push(translate_statement(child, src)?),
            _ => stmts.push(Statement::Expr(translate_expr(child, src)?)),
        }
    }
    Ok(stmts)
}

/// The decoded LHS/RHS of a `let` binding: the binder list (one for a
/// simple `let x = …`, several for a destructuring `let (a, b) = …`)
/// and the init expression. Shared by the statement-position
/// [`translate_local_decl`] and the expression-position
/// [`translate_local_decl_as_expr`].
struct LetParts {
    binders: Vec<Binder>,
    value: Expr,
}

/// Decode the `binder = init` core a `LocalDecl` carries. The Dylan
/// parser models the whole binding as a single `=`-`BinaryOp` inside
/// the LocalDecl's body. Handles a single binder (`let x = e`, typed
/// `let x :: T = e` — the type is recovered onto the `Binder` but the
/// dump ignores it, matching the Rust parser), and a destructuring
/// binder (`let (a, b) = e` → a `ParenList` LHS). A `#rest` in the
/// destructuring or a missing init falls back.
fn local_decl_parts(node: &DylanAst, src: &str) -> Result<LetParts, Unsupported> {
    // Find the binding Body by KIND, not position: a statement `let` has
    // it as child[0], but a `define [modifiers] constant/variable` (which
    // shares this decoder) emits leading `Modifier` children before it.
    let body = node
        .children
        .iter()
        .find(|c| c.kind == Kind::Body)
        .ok_or_else(|| Unsupported("let: no body".into()))?;
    if body.children.len() != 1 {
        return unsupported(format!("let: body has {} forms", body.children.len()));
    }
    let binop = &body.children[0];
    if binop.kind != Kind::BinaryOp || binop.children.len() != 2 {
        return unsupported("let: body is not a `binder = init` binding");
    }
    let lhs = &binop.children[0];
    let rhs = &binop.children[1];
    // Confirm the join operator is `=` (the let binder), not something else.
    let lhs_ext = subtree_extent(lhs)
        .ok_or_else(|| Unsupported("let: binder has no span".into()))?;
    let rhs_ext = subtree_extent(rhs)
        .ok_or_else(|| Unsupported("let: init has no span".into()))?;
    let gap = src
        .get(lhs_ext.1 as usize..rhs_ext.0 as usize)
        .ok_or_else(|| Unsupported("let: binder gap out of bounds".into()))?;
    let op_str = operator_in_gap(gap);
    if op_str != "=" {
        return unsupported(format!("let binder operator {op_str:?}"));
    }
    let binders = match lhs.kind {
        // `let x = e` (untyped) or `let x :: T = e` (typed). A simple
        // untyped binder is a `VariableRef`; a typed binder is a `Param`
        // (the Dylan parser wraps `x :: T` in an <ast-typed-name>, emitted
        // as a Param: name-span + type child).
        Kind::VariableRef | Kind::Param => vec![binder_from_node(lhs, src)?],
        // `let (a, b, …) = e` — a destructuring binder. The Dylan parser
        // emits the binder tuple as a `ParenList` whose items are the
        // binders (each a `VariableRef`, or a `Param` for `(a :: T, b)`);
        // the Rust parser produces `Statement::Let` with one `Binder` per
        // name (and the dump prints `(Binders "a" "b")`). A `#rest`
        // marker inside the tuple isn't reconstructed yet → fall back.
        Kind::ParenList => {
            let mut binders = Vec::new();
            for b in &lhs.children {
                binders.push(binder_from_node(b, src)?);
            }
            binders
        }
        _ => return unsupported("let: non-simple binder (typed or destructuring)"),
    };
    let value = translate_expr(rhs, src)?;
    Ok(LetParts { binders, value })
}

/// One binder of a `let` binding → `Binder`. A `VariableRef` is an
/// untyped binder (`x`); a `Param` is a typed binder (`x :: T`, span =
/// the name token, child = the type expr). The Rust `Binder` keeps the
/// type, but the AST dump prints only the name, so recovering the type
/// is best-effort (and harmless if present).
fn binder_from_node(node: &DylanAst, src: &str) -> Result<Binder, Unsupported> {
    match node.kind {
        Kind::VariableRef => Ok(Binder {
            span: span_of(node),
            name: slice(src, node)?.to_string(),
            type_: None,
        }),
        Kind::Param => {
            let type_ = match node.children.first() {
                Some(t) => Some(translate_expr(t, src)?),
                None => None,
            };
            Ok(Binder {
                span: span_of(node),
                name: slice(src, node)?.to_string(),
                type_,
            })
        }
        _ => unsupported("let: binder is not a simple/typed name"),
    }
}

/// `let <binder> = <init>` → `Statement::Let` (statement position).
fn translate_local_decl(node: &DylanAst, src: &str) -> Result<Statement, Unsupported> {
    let parts = local_decl_parts(node, src)?;
    Ok(Statement::Let {
        span: span_of(node),
        binders: parts.binders,
        rest: None,
        value: parts.value,
    })
}

/// `let <binder> = <init>` at EXPRESSION position → `Expr::Let`. The
/// Rust parser parses an in-body `let` (e.g. inside an `if` then/else
/// branch) with `parse_let_expr_compat`, which yields the
/// single-binder `Expr::Let { binder, value }` and wraps a
/// destructuring `let (a, b) = …` in `Expr::Stmt(Statement::Let)`. We
/// mirror both: one binder → `Expr::Let`; many → `Expr::Stmt`.
fn translate_local_decl_as_expr(node: &DylanAst, src: &str) -> Result<Expr, Unsupported> {
    let span = span_of(node);
    let parts = local_decl_parts(node, src)?;
    if parts.binders.len() == 1 {
        Ok(Expr::Let {
            span,
            binder: parts.binders.into_iter().next().unwrap().name,
            value: Box::new(parts.value),
        })
    } else {
        Ok(Expr::Stmt(Box::new(Statement::Let {
            span,
            binders: parts.binders,
            rest: None,
            value: parts.value,
        })))
    }
}

fn translate_expr(node: &DylanAst, src: &str) -> Result<Expr, Unsupported> {
    let span = span_of(node);
    match node.kind {
        Kind::VariableRef => Ok(Expr::Ident(span, slice(src, node)?.to_string())),
        // ast::Expr::String stores the RAW quoted source slice (the Rust
        // parser does NOT decode escapes here) — so the verbatim span
        // text is exactly right.
        Kind::StringLit => Ok(Expr::String(span, slice(src, node)?.to_string())),
        Kind::IntegerLit => {
            let text = slice(src, node)?;
            let v = parse_integer(text)
                .ok_or_else(|| Unsupported(format!("integer literal {text:?}")))?;
            Ok(Expr::Integer(span, v))
        }
        Kind::FloatLit => {
            let text = slice(src, node)?;
            let v: f64 = text
                .parse()
                .map_err(|_| Unsupported(format!("float literal {text:?}")))?;
            Ok(Expr::Float(span, v))
        }
        Kind::BoolLit => {
            let text = slice(src, node)?;
            match text {
                "#t" => Ok(Expr::Bool(span, true)),
                "#f" => Ok(Expr::Bool(span, false)),
                other => unsupported(format!("boolean literal {other:?}")),
            }
        }
        Kind::Call => {
            let mut it = node.children.iter();
            let callee_node = it
                .next()
                .ok_or_else(|| Unsupported("Call with no callee".into()))?;
            // The Dylan parser has no body-macro knowledge: it parses
            // `when (cond) body end` as a plain call `when(cond)` with a
            // dangling body, whereas the Rust parser (seeded with the
            // stdlib macro names) folds the whole form into one
            // `Expr::MacroCall`. The two ASTs genuinely disagree, so we
            // can't authoritatively translate a call to a known macro —
            // fall back to the Rust parser for the whole file. (Until the
            // Dylan parser itself learns macro-call parsing + seeding.)
            if callee_node.kind == Kind::VariableRef && is_body_macro(slice(src, callee_node)?) {
                return unsupported(format!(
                    "call to body-macro {:?} (Dylan parser lacks macro seeding)",
                    slice(src, callee_node)?
                ));
            }
            let callee = Box::new(translate_expr(callee_node, src)?);
            let mut args = Vec::new();
            for a in it {
                args.push(translate_expr(a, src)?);
            }
            Ok(Expr::Call { span, callee, args })
        }
        Kind::BinaryOp => {
            if node.children.len() != 2 {
                return unsupported(format!("BinaryOp arity {}", node.children.len()));
            }
            let lhs = &node.children[0];
            let rhs = &node.children[1];
            // Nested binary operators now translate safely. Both parsers are
            // flat left-associative for non-pragma files, so `a + b * c`
            // builds the same `(* (+ a b) c)` tree on both sides; and the
            // Rust dump prints `Expr::Paren` transparently (see `fmt_expr`),
            // so a grouped operand like `(a * b) + c` no longer needs the
            // translator to reproduce a `Paren` wrapper the Dylan wire
            // dropped. (`Precedence: c` files — where Rust climbs C-style but
            // the Dylan parser stays flat — are still declined wholesale up
            // in `to_ast_module`.)
            // The operator token isn't a node — it lives in the source
            // gap between the operands. A node's own span may not cover
            // its children (a `Call`'s span is just its paren), so we
            // bound the gap by the TRUE subtree extents.
            let lhs_ext = subtree_extent(lhs)
                .ok_or_else(|| Unsupported("BinaryOp lhs has no span".into()))?;
            let rhs_ext = subtree_extent(rhs)
                .ok_or_else(|| Unsupported("BinaryOp rhs has no span".into()))?;
            let gap = src
                .get(lhs_ext.1 as usize..rhs_ext.0 as usize)
                .ok_or_else(|| Unsupported("BinaryOp operator gap out of bounds".into()))?;
            let op_str = operator_in_gap(gap);
            let op = parse_binop(&op_str)
                .ok_or_else(|| Unsupported(format!("binary operator {op_str:?}")))?;
            let lhs = Box::new(translate_expr(lhs, src)?);
            let rhs = Box::new(translate_expr(rhs, src)?);
            Ok(Expr::BinOp { span, op, lhs, rhs })
        }
        // `#"sym"` / `foo:` symbol literal. The Rust parser stores the
        // RAW token text (`#"foo"` keeps its `#"…"`, a `foo:` keyword
        // keeps its trailing `:`) in `Expr::Symbol`, so the verbatim
        // span slice is exactly what `fmt_expr` prints as `(Symbol …)`.
        Kind::SymbolLit => Ok(Expr::Symbol(span, slice(src, node)?.to_string())),
        // `~e` / `-e` prefix operator. The Dylan parser stores the
        // operator token AS the `UnaryOp` node's own span and the
        // operand as child[0] (see DYLAN_AST_WIRE.md row 17). The Rust
        // parser builds `Expr::UnOp { op, operand }` — `-`→Neg, `~`→Not.
        Kind::UnaryOp => {
            let op = match slice(src, node)? {
                "-" => nod_reader::ast::UnOp::Neg,
                "~" => nod_reader::ast::UnOp::Not,
                other => return unsupported(format!("unary operator {other:?}")),
            };
            let operand_node = node
                .children
                .first()
                .ok_or_else(|| Unsupported("UnaryOp has no operand".into()))?;
            // `-1` / `-1.5` — a `-` IMMEDIATELY followed by a numeric
            // literal (no source gap) is fused into a SIGNED literal, not
            // a `UnOp`. This mirrors the Rust *lexer* (lexer.rs
            // `lex_plus_minus`: `-`/`+` + digit at token-start is one
            // signed-numeric token), which the Dylan lexer splits into
            // `-` + `1`. We refuse the operator otherwise (`- 1` with a
            // gap stays a `UnOp`, matching Rust). Only `-` fuses — the
            // Dylan parser never treats `+` as unary, and `~` is logical
            // not.
            if op == nod_reader::ast::UnOp::Neg
                && node.span_hi == operand_node.span_lo
            {
                match operand_node.kind {
                    Kind::IntegerLit => {
                        let text = slice(src, operand_node)?;
                        let v = parse_integer(text)
                            .ok_or_else(|| Unsupported(format!("integer literal {text:?}")))?;
                        return Ok(Expr::Integer(span, -v));
                    }
                    Kind::FloatLit => {
                        let text = slice(src, operand_node)?;
                        let v: f64 = text
                            .parse()
                            .map_err(|_| Unsupported(format!("float literal {text:?}")))?;
                        return Ok(Expr::Float(span, -v));
                    }
                    _ => {}
                }
            }
            let operand = Box::new(translate_expr(operand_node, src)?);
            Ok(Expr::UnOp { span, op, operand })
        }
        // `a[i, j]` indexing. The Rust parser lowers it to a single call
        // `element(a, i, j)` (parser.rs `parse_postfix`'s LBracket arm),
        // so the AST has one call shape. The wire `Subscript` node's
        // child[0] is the base and the remaining children are the
        // indices.
        Kind::Subscript => {
            let mut it = node.children.iter();
            let base_node = it
                .next()
                .ok_or_else(|| Unsupported("Subscript has no base".into()))?;
            let mut args = vec![translate_expr(base_node, src)?];
            for idx in it {
                args.push(translate_expr(idx, src)?);
            }
            Ok(Expr::Call {
                span,
                callee: Box::new(Expr::Ident(span, "element".to_string())),
                args,
            })
        }
        // `#(…)` / `#[…]` / `#{…}` literal. The Rust parser lowers each to
        // a `Call` to a synthetic constructor (`parse_hash_literal`):
        // `#(` → `#list`, `#[` → `#vector`, `#{` → `#set`. The wire node's
        // span is the open token, so the byte right after `#` selects the
        // form. Children are the element exprs → the call args.
        Kind::HashLit => {
            let open = slice(src, node)?;
            let name = match open.as_bytes().get(1) {
                Some(b'(') => "#list",
                Some(b'[') => "#vector",
                Some(b'{') => "#set",
                _ => return unsupported(format!("hash literal opener {open:?}")),
            };
            let mut args = Vec::new();
            for el in &node.children {
                args.push(translate_expr(el, src)?);
            }
            Ok(Expr::Call {
                span,
                callee: Box::new(Expr::Ident(span, name.to_string())),
                args,
            })
        }
        // A `let`/`local` at EXPRESSION position (e.g. inside an `if`
        // then/else body, which the Rust parser parses with
        // `parse_expr_full` → `parse_let_expr_compat`). Reconstruct the
        // value-producing `Expr::Let` form here; `translate_local_decl`
        // handles the STATEMENT-position `let`.
        Kind::LocalDecl => translate_local_decl_as_expr(node, src),
        // A statement at expression position — Dylan's `if`/`while`/… are
        // value-producing. v1 reconstructs `if` (→ Expr::If with
        // Begin-wrapped arms); other statement keywords fall back.
        Kind::Statement => translate_statement_as_expr(node, src),
        // `key: value` keyword argument → the Rust parser's synthetic
        // `%kw-arg(Symbol("key:"), value)` call. The `key:` symbol keeps
        // its trailing colon (matches `(Symbol "x:")`).
        Kind::KwArg => {
            let key = slice(src, node)?.to_string();
            let value_node = node
                .children
                .first()
                .ok_or_else(|| Unsupported("KwArg has no value".into()))?;
            let value = translate_expr(value_node, src)?;
            Ok(Expr::Call {
                span,
                callee: Box::new(Expr::Ident(span, "%kw-arg".to_string())),
                args: vec![Expr::Symbol(span, key), value],
            })
        }
        other => unsupported(format!("expression {other:?}")),
    }
}

/// The true byte extent of a subtree: min `span_lo` / max `span_hi`
/// over the node and all descendants that carry a real span (`hi >
/// lo`). Unspanned nodes (`0..0`, e.g. a backfill-less `Call` whose own
/// record is just the paren) contribute only through their children.
fn subtree_extent(node: &DylanAst) -> Option<(u32, u32)> {
    let mut acc: Option<(u32, u32)> = if node.span_hi > node.span_lo {
        Some((node.span_lo, node.span_hi))
    } else {
        None
    };
    for c in &node.children {
        if let Some((clo, chi)) = subtree_extent(c) {
            acc = Some(match acc {
                Some((lo, hi)) => (lo.min(clo), hi.max(chi)),
                None => (clo, chi),
            });
        }
    }
    acc
}

/// The stdlib body-shaped macro names the Rust `dump-ast` path seeds
/// the parser with. A call to one of these is a `MacroCall` to the Rust
/// parser but a plain function call to the (macro-unaware) Dylan parser
/// — so the translator declines it. Keep in sync with the seed list in
/// `main.rs::run_dump_ast`.
fn is_body_macro(name: &str) -> bool {
    matches!(
        name,
        "case" | "cond" | "for-each" | "iterate" | "select" | "unless" | "when" | "while"
    )
}

/// Extract the operator token from the source gap between two operands.
/// The gap can carry a closing delimiter from the left operand
/// (`f(x) + y` → `") + "`, `a[i] := v` → `"] := "`, a `#{…}` → `}`), an
/// opening delimiter from the right operand, and/or **comments** — a
/// multi-line `// …` block or a `/* … */` block can sit between two
/// `|`-chained operands (e.g. dylan-parser.dylan's `f = #"dot-dot"` …
/// 6-line comment … `| f = #"arrow"`). Strip comments first (Dylan `//`
/// to end-of-line, `/* … */` non-nesting), then ALL bracket/brace/paren
/// delimiters and whitespace; what remains is the operator (`+`, `<=`,
/// `:=`, `mod`, `|`, …). A Dylan infix operator never contains a comment,
/// delimiter, or whitespace, so this is lossless. (A lone `/` is the
/// divide operator and is preserved — only `//` and `/*` start comments.)
fn operator_in_gap(gap: &str) -> String {
    let mut cleaned = String::with_capacity(gap.len());
    let mut chars = gap.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            match chars.peek() {
                // `// …` line comment → skip through the newline.
                Some('/') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                // `/* … */` block comment → skip to the closing `*/`
                // (Dylan block comments do not nest).
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for nc in chars.by_ref() {
                        if prev == '*' && nc == '/' {
                            break;
                        }
                        prev = nc;
                    }
                    continue;
                }
                // A lone `/` is the divide operator — keep it.
                _ => {}
            }
        }
        cleaned.push(c);
    }
    cleaned
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))
        .collect()
}

/// Map a Dylan infix-operator token to `ast::BinOp`. The operator is
/// matched exactly, so `=`/`==`/`:=` disambiguate cleanly.
fn parse_binop(op: &str) -> Option<nod_reader::ast::BinOp> {
    use nod_reader::ast::BinOp;
    Some(match op {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "mod" => BinOp::Mod,
        "rem" => BinOp::Rem,
        "^" => BinOp::Pow,
        "=" => BinOp::Eq,
        "==" => BinOp::EqEq,
        "~=" => BinOp::Ne,
        "~==" => BinOp::NeEq,
        "<" => BinOp::Lt,
        ">" => BinOp::Gt,
        "<=" => BinOp::Le,
        ">=" => BinOp::Ge,
        "&" => BinOp::And,
        "|" => BinOp::Or,
        ":=" => BinOp::Assign,
        _ => return None,
    })
}

/// A `Statement` wire node at EXPRESSION position. `if` → `Expr::If`;
/// `while`/`until` → `Expr::Stmt(Statement::While|Until)` (the Rust
/// parser wraps a statement form in `Expr::Stmt` when it appears where
/// a value is expected). Other keywords fall back.
fn translate_statement_as_expr(node: &DylanAst, src: &str) -> Result<Expr, Unsupported> {
    match slice(src, node)? {
        "if" => build_if(node, src),
        "while" | "until" => Ok(Expr::Stmt(Box::new(translate_statement(node, src)?))),
        // An anonymous `method (params) body end` literal → `Expr::Method`.
        // (The Dylan parser emits these as a `Statement` whose word is
        // `method`/`function`, carrying a `ParamList` + optional
        // `ReturnSpec` before the `Body` — see the `<ast-statement>`
        // emitter.) The Rust parser builds `Expr::Method { params, body }`
        // for `method …`; `function …` literals are rare in the corpus
        // but share the shape.
        "method" => build_method_literal(node, src),
        other => unsupported(format!("statement {other:?}")),
    }
}

/// An anonymous `method (params) => (ret) body end` literal →
/// `Expr::Method`. Wire children (in order): optional `ParamList`,
/// optional `ReturnSpec`, then the `Body`. The Rust parser discards the
/// return spec of a method literal (`Expr::Method` carries only params
/// + body), so we parse it for validity but don't attach it. The body
/// forms are translated as expressions (a `let` inside becomes
/// `Expr::Let`), matching `parse_method`'s `Vec<Expr>` body.
fn build_method_literal(node: &DylanAst, src: &str) -> Result<Expr, Unsupported> {
    let mut params: Vec<Param> = Vec::new();
    let mut body: Option<Vec<Expr>> = None;
    for child in &node.children {
        match child.kind {
            Kind::ParamList => params = translate_param_list(child, src)?,
            // The return spec is parsed by the Rust `parse_method` but not
            // represented in `Expr::Method`; accept and ignore it.
            Kind::ReturnSpec => {
                let _ = translate_return_spec(child, src)?;
            }
            Kind::Body => {
                body = Some(
                    child
                        .children
                        .iter()
                        .map(|c| translate_expr(c, src))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            other => return unsupported(format!("method literal child {other:?}")),
        }
    }
    let body = body.ok_or_else(|| Unsupported("method literal has no Body".into()))?;
    Ok(Expr::Method {
        span: span_of(node),
        params,
        body,
    })
}

/// A `Statement` wire node at STATEMENT position → `ast::Statement`.
/// `if` → `Statement::Expr(Expr::If)`; `while`/`until` → the loop forms.
fn translate_statement(node: &DylanAst, src: &str) -> Result<Statement, Unsupported> {
    match slice(src, node)? {
        "if" => Ok(Statement::Expr(build_if(node, src)?)),
        "while" => build_loop(node, src, /* is_while */ true),
        "until" => build_loop(node, src, /* is_while */ false),
        other => unsupported(format!("statement {other:?}")),
    }
}

/// `if` → `Expr::If`. Wire shape: child[0] = leading `Body` holding
/// `[cond, then-forms…]`, then zero-or-more `StatementClause` children
/// — any number of `elseif` clauses (each carrying its own
/// `[cond, then-forms…]` Body) optionally followed by one trailing
/// `else` clause (carrying just `[forms…]`).
///
/// `elseif` desugars to a **nested `Expr::If`** exactly as the Rust
/// parser does (parser.rs `parse_if_branches`): the chain
/// `if (c1) t… elseif (c2) u… else e… end` becomes
/// `If(c1, Begin[t…], Begin[ If(c2, Begin[u…], Begin[e…]) ])`. The
/// outer `else_` arm of each `If` wraps the nested `If` in a `Begin`
/// (the Rust parser returns `Some(vec![nested])` and then `Begin`-wraps
/// it), so the dump nests `(If …)` inside a `(Begin (If …))`.
fn build_if(node: &DylanAst, src: &str) -> Result<Expr, Unsupported> {
    let mut children = node.children.iter();
    let head = children
        .next()
        .ok_or_else(|| Unsupported("if: no head body".into()))?;
    let (cond, then_) = if_branch_from_body(head, src)?;

    // Collect the trailing clauses, then fold them right-to-left into
    // the else chain so each `elseif` nests inside the previous one's
    // else arm (matching the Rust parser's recursive descent).
    let clauses: Vec<&DylanAst> = children.collect();
    let mut else_: Option<Box<Expr>> = None;
    for clause in clauses.iter().rev() {
        if clause.kind != Kind::StatementClause {
            return unsupported(format!("if: unexpected child {:?}", clause.kind));
        }
        let cbody = clause
            .children
            .first()
            .ok_or_else(|| Unsupported("if-clause: no body".into()))?;
        if cbody.kind != Kind::Body {
            return unsupported("if-clause: child is not a Body");
        }
        match slice(src, clause)? {
            "else" => {
                if else_.is_some() {
                    return unsupported("if: `else` not the final clause");
                }
                let else_body = cbody
                    .children
                    .iter()
                    .map(|c| translate_expr(c, src))
                    .collect::<Result<Vec<_>, _>>()?;
                else_ = Some(Box::new(Expr::Begin {
                    span: span_of(cbody),
                    body: else_body,
                }));
            }
            "elseif" => {
                // The clause body is `[cond, then-forms…]`, like the head.
                let (ei_cond, ei_then) = if_branch_from_body(cbody, src)?;
                let nested = Expr::If {
                    span: span_of(clause),
                    cond: Box::new(ei_cond),
                    then_: Box::new(ei_then),
                    else_: else_.take(),
                };
                // The Rust parser threads the nested `If` through the
                // outer else arm wrapped in a `Begin` (the recursive
                // `parse_if_branches` returns `Some(vec![nested])`).
                else_ = Some(Box::new(Expr::Begin {
                    span: span_of(clause),
                    body: vec![nested],
                }));
            }
            other => return unsupported(format!("if clause {other:?}")),
        }
    }

    Ok(Expr::If {
        span: span_of(node),
        cond: Box::new(cond),
        then_: Box::new(then_),
        else_,
    })
}

/// A leading `Body` of the form `[cond, then-forms…]` (the `if` head or
/// an `elseif` clause body) → `(cond, Begin[then-forms…])`. The
/// then-forms can themselves be `let`/`if`/… (translated via
/// `translate_expr`, which handles a value-position `LocalDecl`/`if`).
fn if_branch_from_body(body: &DylanAst, src: &str) -> Result<(Expr, Expr), Unsupported> {
    if body.kind != Kind::Body {
        return unsupported("if: branch head is not a Body");
    }
    if body.children.is_empty() {
        return unsupported("if: empty branch body (no condition)");
    }
    let cond = translate_expr(&body.children[0], src)?;
    let then_body = body.children[1..]
        .iter()
        .map(|c| translate_expr(c, src))
        .collect::<Result<Vec<_>, _>>()?;
    let then_ = Expr::Begin {
        span: span_of(body),
        body: then_body,
    };
    Ok((cond, then_))
}

/// `while`/`until` → `Statement::While`/`Until`. Wire shape: child[0] =
/// leading `Body` holding `[cond, body-forms…]`. The body forms are
/// translated as statements (so a nested `let`/loop is handled too).
fn build_loop(node: &DylanAst, src: &str, is_while: bool) -> Result<Statement, Unsupported> {
    let head = node
        .children
        .first()
        .ok_or_else(|| Unsupported("loop: no head body".into()))?;
    if head.kind != Kind::Body {
        return unsupported("loop: head is not a Body");
    }
    if head.children.is_empty() {
        return unsupported("loop: empty head body (no condition)");
    }
    if node.children.len() != 1 {
        // A loop has no trailing clauses; extra children mean something
        // we don't model (e.g. a `for`/`finally` shape).
        return unsupported("loop: unexpected trailing clause");
    }
    let cond = translate_expr(&head.children[0], src)?;
    let body = translate_stmts(&head.children[1..], src)?;
    let span = span_of(node);
    Ok(if is_while {
        Statement::While { span, cond, body }
    } else {
        Statement::Until { span, cond, body }
    })
}

/// Parse a Dylan integer literal text into `i128`. Handles decimal and
/// the `#x`/`#o`/`#b`/`#d` radix prefixes. Returns `None` on anything
/// else so the caller can fall back.
fn parse_integer(text: &str) -> Option<i128> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix("#x").or_else(|| t.strip_prefix("#X")) {
        return i128::from_str_radix(&hex.replace('_', ""), 16).ok();
    }
    if let Some(oct) = t.strip_prefix("#o").or_else(|| t.strip_prefix("#O")) {
        return i128::from_str_radix(&oct.replace('_', ""), 8).ok();
    }
    if let Some(bin) = t.strip_prefix("#b").or_else(|| t.strip_prefix("#B")) {
        return i128::from_str_radix(&bin.replace('_', ""), 2).ok();
    }
    if let Some(dec) = t.strip_prefix("#d").or_else(|| t.strip_prefix("#D")) {
        return dec.replace('_', "").parse().ok();
    }
    t.replace('_', "").parse().ok()
}
