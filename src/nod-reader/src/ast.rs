//! Dylan AST — Sprint 03 expression grammar, Sprint 04 top-level + statements.
//!
//! # Macro boundary policy
//!
//! **Before adding a new `Expr::*` or `Statement::*` variant for a
//! control-flow keyword, iteration form, or "sugar" shape, read
//! `docs/MACRO_BOUNDARY.md`.** The frozen kernel forms (`If`, `Begin`,
//! `Let`, `Method`, definitional items, `Block`-with-cleanup) are the
//! complete list of legitimate hardcoded variants. Everything else
//! belongs in `src/nod-dylan/dylan-sources/stdlib.dylan` as a
//! `define macro` and gets surfaced via `Expr::MacroCall`. Sprint 25
//! retired `Expr::Unless` to that pattern; future additions follow
//! the same path. Rule 3 (pre-flight): try `define macro` first.
//!
//! Sprint 47 — `Statement::Let { binders: Vec<Binder>, … }` carries the
//! multi-binder `let (a, b, c) = expr;` shape (GAP-003 fix). Although
//! it shares the `Let` variant with the single-binder form, the
//! multi-binder shape is **also a frozen kernel binding form** — it
//! can't desugar to nested single-binder `Let`s because the RHS must
//! be evaluated exactly once and the binders see distinct return values
//! from one call. The SBCL-style secondary-values lowering happens in
//! `nod-sema::lower` against the same AST shape; no separate
//! `LetMulti` variant is needed.

use crate::fragments::Fragment;
use crate::span::Span;

#[derive(Clone, Debug)]
pub enum Expr {
    Integer(Span, i128),
    Float(Span, f64),
    String(Span, String),
    Char(Span, char),
    Bool(Span, bool),
    Symbol(Span, String),
    Ident(Span, String),
    Call {
        span: Span,
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    BinOp {
        span: Span,
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    UnOp {
        span: Span,
        op: UnOp,
        operand: Box<Expr>,
    },
    Paren {
        span: Span,
        inner: Box<Expr>,
    },
    If {
        span: Span,
        cond: Box<Expr>,
        then_: Box<Expr>,
        else_: Option<Box<Expr>>,
    },
    Case {
        span: Span,
        arms: Vec<CaseArm>,
        otherwise: Option<Box<Expr>>,
    },
    /// Sprint 25: body-shaped macro call.
    ///
    /// Recognises the surface
    /// ```text
    ///   <name> (head…) body… end
    /// ```
    /// at parse time when `<name>` is in the parser's known-macro set
    /// (passed in via [`crate::parser::parse_module_with_macros`]).
    /// The variant carries only the name and the full source span;
    /// the macro engine re-lexes the span (via the existing
    /// `call_site_fragments` path in `nod-macro`) and runs its
    /// fragment-level pattern matcher on the result, so the head's
    /// internal structure doesn't need to be modelled in the AST.
    /// This is what lets a macro head contain macro-specific syntax
    /// like `(?var:name in ?coll:expression)` — the parser doesn't
    /// have to parse `x in c` as a Dylan expression.
    MacroCall {
        span: Span,
        name: String,
    },
    Begin {
        span: Span,
        body: Vec<Expr>,
    },
    Let {
        span: Span,
        binder: String,
        value: Box<Expr>,
    },
    LocalMethod {
        span: Span,
        name: String,
        params: Vec<Param>,
        body: Vec<Expr>,
    },
    Method {
        span: Span,
        params: Vec<Param>,
        body: Vec<Expr>,
    },
    /// Wraps a Sprint 04 statement form (`block`, `for`, `while`, `until`)
    /// when it appears at expression position. The value is the body's
    /// final expression; this exists so the precedence chain can still
    /// chain `.something` / function-call postfix onto a `block (...) ... end`.
    Stmt(Box<crate::ast::Statement>),
}

#[derive(Clone, Debug)]
pub struct CaseArm {
    pub span: Span,
    pub cond: Expr,
    pub body: Vec<Expr>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub span: Span,
    pub name: String,
    pub type_: Option<Expr>,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Rem,
    Pow,
    Eq,
    EqEq,
    Ne,
    NeEq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Assign,
}

impl BinOp {
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "mod",
            BinOp::Rem => "rem",
            BinOp::Pow => "^",
            BinOp::Eq => "=",
            BinOp::EqEq => "==",
            BinOp::Ne => "~=",
            BinOp::NeEq => "~==",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::And => "&",
            BinOp::Or => "|",
            BinOp::Assign => ":=",
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UnOp {
    Neg,
    Not,
}

impl UnOp {
    pub fn name(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "~",
        }
    }
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Integer(s, _)
            | Expr::Float(s, _)
            | Expr::String(s, _)
            | Expr::Char(s, _)
            | Expr::Bool(s, _)
            | Expr::Symbol(s, _)
            | Expr::Ident(s, _) => *s,
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
            | Expr::Method { span, .. } => *span,
            Expr::Stmt(s) => s.span(),
        }
    }
}

pub fn format_ast(expr: &Expr) -> String {
    let mut out = String::new();
    fmt_expr(expr, 0, &mut out);
    out
}

fn indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

fn fmt_expr(e: &Expr, depth: usize, out: &mut String) {
    // Peel transparent `Expr::Paren` wrappers BEFORE indenting, so the inner
    // node prints in this slot with no wrapper line and correct depth. Under
    // Dylan's flat precedence the tree *shape* already encodes grouping
    // losslessly (`a + (b + c)` is right-nested, `a + b + c` left-nested —
    // distinct trees with or without a Paren marker), so `Expr::Paren` is
    // syntactic provenance, not structure — like a span, which this dump
    // also omits. This dump is the oracle the `--parse-with-dylan`
    // translation gate diffs against; the Dylan-in-Dylan parser drops single
    // grouping parens transparently (parse-paren-fragment), so skipping the
    // wrapper here lets the two parsers agree on semantic structure without
    // bolting fragile paren-recovery onto the translator. A genuine misgroup
    // still shows as a different tree shape, so the gate is not weakened.
    let mut e = e;
    while let Expr::Paren { inner, .. } = e {
        e = inner;
    }
    indent(depth, out);
    match e {
        Expr::Integer(_, v) => {
            out.push_str(&format!("(Integer {v})\n"));
        }
        Expr::Float(_, v) => {
            out.push_str(&format!("(Float {v})\n"));
        }
        Expr::String(_, s) => {
            out.push_str(&format!("(String {s:?})\n"));
        }
        Expr::Char(_, c) => {
            out.push_str(&format!("(Char {c:?})\n"));
        }
        Expr::Bool(_, b) => {
            out.push_str(&format!("(Bool {b})\n"));
        }
        Expr::Symbol(_, s) => {
            out.push_str(&format!("(Symbol {s:?})\n"));
        }
        Expr::Ident(_, name) => {
            out.push_str(&format!("(Ident {name:?})\n"));
        }
        Expr::Call { callee, args, .. } => {
            out.push_str("(Call\n");
            fmt_expr(callee, depth + 1, out);
            indent(depth + 1, out);
            out.push_str("(Args\n");
            for a in args {
                fmt_expr(a, depth + 2, out);
            }
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::BinOp { op, lhs, rhs, .. } => {
            out.push_str(&format!("(BinOp {}\n", op.name()));
            fmt_expr(lhs, depth + 1, out);
            fmt_expr(rhs, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::UnOp { op, operand, .. } => {
            out.push_str(&format!("(UnOp {}\n", op.name()));
            fmt_expr(operand, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        // Peeled above before indenting — never reached.
        Expr::Paren { .. } => unreachable!("Expr::Paren is peeled before the match"),
        Expr::If { cond, then_, else_, .. } => {
            out.push_str("(If\n");
            fmt_expr(cond, depth + 1, out);
            fmt_expr(then_, depth + 1, out);
            if let Some(e) = else_ {
                fmt_expr(e, depth + 1, out);
            } else {
                indent(depth + 1, out);
                out.push_str("(NoElse)\n");
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::MacroCall { name, .. } => {
            out.push_str(&format!("(MacroCall {name:?})\n"));
        }
        Expr::Case { arms, otherwise, .. } => {
            out.push_str("(Case\n");
            for a in arms {
                indent(depth + 1, out);
                out.push_str("(Arm\n");
                fmt_expr(&a.cond, depth + 2, out);
                indent(depth + 2, out);
                out.push_str("(Body\n");
                for b in &a.body {
                    fmt_expr(b, depth + 3, out);
                }
                indent(depth + 2, out);
                out.push_str(")\n");
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            if let Some(o) = otherwise {
                indent(depth + 1, out);
                out.push_str("(Otherwise\n");
                fmt_expr(o, depth + 2, out);
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::Begin { body, .. } => {
            out.push_str("(Begin\n");
            for b in body {
                fmt_expr(b, depth + 1, out);
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::Let { binder, value, .. } => {
            out.push_str(&format!("(Let {binder:?}\n"));
            fmt_expr(value, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::LocalMethod { name, params, body, .. } => {
            out.push_str(&format!("(LocalMethod {name:?}\n"));
            fmt_params(params, depth + 1, out);
            indent(depth + 1, out);
            out.push_str("(Body\n");
            for b in body {
                fmt_expr(b, depth + 2, out);
            }
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::Method { params, body, .. } => {
            out.push_str("(Method\n");
            fmt_params(params, depth + 1, out);
            indent(depth + 1, out);
            out.push_str("(Body\n");
            for b in body {
                fmt_expr(b, depth + 2, out);
            }
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Expr::Stmt(s) => {
            out.push_str("(StmtExpr\n");
            fmt_stmt(s, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
    }
}

fn fmt_params(params: &[Param], depth: usize, out: &mut String) {
    indent(depth, out);
    out.push_str("(Params\n");
    for p in params {
        indent(depth + 1, out);
        out.push_str(&format!("(Param {:?}", p.name));
        if let Some(t) = &p.type_ {
            out.push('\n');
            fmt_expr(t, depth + 2, out);
            indent(depth + 1, out);
            out.push_str(")\n");
        } else {
            out.push_str(")\n");
        }
    }
    indent(depth, out);
    out.push_str(")\n");
}

// ─────────────────────────────────────────────────────────────────────────
// Sprint 04 — top-level forms, statements, library/module clauses
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Module {
    pub span: Span,
    pub header: Vec<(String, String)>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub struct ReturnSig {
    pub span: Span,
    pub values: Vec<ReturnValue>,
    pub rest: Option<ReturnRest>,
}

#[derive(Clone, Debug)]
pub struct ReturnValue {
    pub span: Span,
    pub name: Option<String>,
    pub type_: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct ReturnRest {
    pub span: Span,
    pub name: Option<String>,
    pub type_: Option<Expr>,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Modifier {
    Open,
    Sealed,
    Abstract,
    Concrete,
    Primary,
    Free,
    Inline,
    NotInline,
    Sideways,
    Domain,
}

impl Modifier {
    pub fn name(self) -> &'static str {
        match self {
            Modifier::Open => "open",
            Modifier::Sealed => "sealed",
            Modifier::Abstract => "abstract",
            Modifier::Concrete => "concrete",
            Modifier::Primary => "primary",
            Modifier::Free => "free",
            Modifier::Inline => "inline",
            Modifier::NotInline => "not-inline",
            Modifier::Sideways => "sideways",
            Modifier::Domain => "domain",
        }
    }
    pub fn from_word(w: &str) -> Option<Modifier> {
        Some(match w {
            "open" => Modifier::Open,
            "sealed" => Modifier::Sealed,
            "abstract" => Modifier::Abstract,
            "concrete" => Modifier::Concrete,
            "primary" => Modifier::Primary,
            "free" => Modifier::Free,
            "inline" => Modifier::Inline,
            "not-inline" => Modifier::NotInline,
            "sideways" => Modifier::Sideways,
            "domain" => Modifier::Domain,
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum SlotAllocation {
    Instance,
    Class,
    EachSubclass,
    Virtual,
    Constant,
}

#[derive(Clone, Debug)]
pub struct SlotDef {
    pub span: Span,
    pub name: String,
    pub type_: Option<Expr>,
    pub init_value: Option<Expr>,
    pub init_keyword: Option<String>,
    pub required_init_keyword: bool,
    pub setter: Option<bool>,
    pub allocation: SlotAllocation,
}

#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub span: Span,
    pub name: String,
    pub rename: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ImportSet {
    All,
    Items(Vec<ImportSpec>),
}

#[derive(Clone, Debug)]
pub struct LibraryUseClause {
    pub span: Span,
    pub name: String,
    pub import: Option<ImportSet>,
    pub exclude: Vec<String>,
    pub rename: Vec<(String, String)>,
    pub prefix: Option<String>,
    pub export: Option<ImportSet>,
}

#[derive(Clone, Debug)]
pub struct ModuleUseClause {
    pub span: Span,
    pub name: String,
    pub import: Option<ImportSet>,
    pub exclude: Vec<String>,
    pub rename: Vec<(String, String)>,
    pub prefix: Option<String>,
    pub export: Option<ImportSet>,
}

#[derive(Clone, Debug)]
pub enum Item {
    DefineConstant {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        type_: Option<Expr>,
        value: Expr,
    },
    DefineVariable {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        type_: Option<Expr>,
        value: Expr,
    },
    DefineFunction {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        params: Vec<Param>,
        return_: Option<ReturnSig>,
        body: Vec<Statement>,
    },
    DefineMethod {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        params: Vec<Param>,
        return_: Option<ReturnSig>,
        body: Vec<Statement>,
    },
    DefineGeneric {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        params: Vec<Param>,
        return_: Option<ReturnSig>,
    },
    DefineClass {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        supers: Vec<Expr>,
        slots: Vec<SlotDef>,
    },
    /// Sprint 27: `define c-function NAME (PARAMS) => (RET);
    /// [c-name: "STR";] library: "STR"; end;`. The header is shaped
    /// like `define function` / `define generic`. The body is just
    /// attribute clauses — no Dylan code; the binding's call site
    /// is later generated by the FFI lowering.
    ///
    /// In Sprint 27 the FFI lowering itself doesn't exist yet —
    /// `nod-sema` recognises the variant, records the DLL
    /// provenance in the namespace, and errors at any actual call
    /// site with "Sprint 28" diagnostic.
    DefineCFunction {
        span: Span,
        modifiers: Vec<Modifier>,
        name: String,
        params: Vec<Param>,
        return_: Option<ReturnSig>,
        /// `c-name: "STR"`. `None` defaults to `name` at lowering time.
        c_name: Option<String>,
        /// `library: "STR"`. Required — defaults to `""` if the
        /// parser allowed the clause to be omitted (sema errors in
        /// that case).
        library: String,
    },
    DefineLibrary {
        span: Span,
        name: String,
        uses: Vec<LibraryUseClause>,
        exports: Vec<String>,
        creates: Vec<String>,
    },
    DefineModule {
        span: Span,
        name: String,
        uses: Vec<ModuleUseClause>,
        exports: Vec<String>,
        creates: Vec<String>,
    },
    DefineMacro {
        span: Span,
        name: String,
        body_fragments: Vec<Fragment>,
    },
    /// Catch-all for `define <kw>` forms whose body shape isn't yet modelled
    /// (e.g. `define test`, `define suite`, `define table`). We still record
    /// the keyword and capture the body as fragments so downstream sprints
    /// can lift them when ready.
    DefineOther {
        span: Span,
        modifiers: Vec<Modifier>,
        keyword: String,
        name: Option<String>,
        body_fragments: Vec<Fragment>,
    },
    Expr(Expr),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::DefineConstant { span, .. }
            | Item::DefineVariable { span, .. }
            | Item::DefineFunction { span, .. }
            | Item::DefineMethod { span, .. }
            | Item::DefineGeneric { span, .. }
            | Item::DefineClass { span, .. }
            | Item::DefineCFunction { span, .. }
            | Item::DefineLibrary { span, .. }
            | Item::DefineModule { span, .. }
            | Item::DefineMacro { span, .. }
            | Item::DefineOther { span, .. } => *span,
            Item::Expr(e) => e.span(),
        }
    }
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Item::DefineConstant { .. } => "define-constant",
            Item::DefineVariable { .. } => "define-variable",
            Item::DefineFunction { .. } => "define-function",
            Item::DefineMethod { .. } => "define-method",
            Item::DefineGeneric { .. } => "define-generic",
            Item::DefineClass { .. } => "define-class",
            Item::DefineCFunction { .. } => "define-c-function",
            Item::DefineLibrary { .. } => "define-library",
            Item::DefineModule { .. } => "define-module",
            Item::DefineMacro { .. } => "define-macro",
            Item::DefineOther { .. } => "define-other",
            Item::Expr(_) => "expr",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Binder {
    pub span: Span,
    pub name: String,
    pub type_: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct LocalMethodDecl {
    pub span: Span,
    pub name: String,
    pub params: Vec<Param>,
    pub return_: Option<ReturnSig>,
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug)]
pub enum ForClause {
    Numeric(Box<NumericForClause>),
    In {
        span: Span,
        var: String,
        coll: Expr,
    },
    From(Box<FromForClause>),
    Until {
        span: Span,
        cond: Expr,
    },
    While {
        span: Span,
        cond: Expr,
    },
}

#[derive(Clone, Debug)]
pub struct NumericForClause {
    pub span: Span,
    pub var: String,
    pub from: Expr,
    pub to: Option<Expr>,
    pub below: Option<Expr>,
    pub above: Option<Expr>,
    pub by: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct FromForClause {
    pub span: Span,
    pub var: String,
    pub from: Expr,
    pub by: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct ExceptionClause {
    pub span: Span,
    pub var: Option<String>,
    pub class: Expr,
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Expr(Expr),
    Let {
        span: Span,
        binders: Vec<Binder>,
        rest: Option<Binder>,
        value: Expr,
    },
    Local {
        span: Span,
        methods: Vec<LocalMethodDecl>,
    },
    For {
        span: Span,
        clauses: Vec<ForClause>,
        body: Vec<Statement>,
        finally_: Vec<Statement>,
    },
    While {
        span: Span,
        cond: Expr,
        body: Vec<Statement>,
    },
    Until {
        span: Span,
        cond: Expr,
        body: Vec<Statement>,
    },
    Block {
        span: Span,
        exit_var: Option<String>,
        body: Vec<Statement>,
        handlers: Vec<ExceptionClause>,
        cleanup: Vec<Statement>,
        afterwards: Vec<Statement>,
    },
}

impl Statement {
    pub fn span(&self) -> Span {
        match self {
            Statement::Expr(e) => e.span(),
            Statement::Let { span, .. }
            | Statement::Local { span, .. }
            | Statement::For { span, .. }
            | Statement::While { span, .. }
            | Statement::Until { span, .. }
            | Statement::Block { span, .. } => *span,
        }
    }
}

// ─── Debug AST dump for the top-level layer ──────────────────────────────

pub fn format_ast_module(module: &Module) -> String {
    let mut out = String::new();
    out.push_str("(Module\n");
    if !module.header.is_empty() {
        indent(1, &mut out);
        out.push_str("(Header\n");
        for (k, v) in &module.header {
            indent(2, &mut out);
            out.push_str(&format!("({k:?} {v:?})\n"));
        }
        indent(1, &mut out);
        out.push_str(")\n");
    }
    for it in &module.items {
        fmt_item(it, 1, &mut out);
    }
    out.push_str(")\n");
    out
}

fn fmt_modifiers(mods: &[Modifier], depth: usize, out: &mut String) {
    if mods.is_empty() {
        return;
    }
    indent(depth, out);
    out.push_str("(Modifiers");
    for m in mods {
        out.push(' ');
        out.push_str(m.name());
    }
    out.push_str(")\n");
}

fn fmt_return(r: &Option<ReturnSig>, depth: usize, out: &mut String) {
    let Some(r) = r else { return };
    indent(depth, out);
    out.push_str("(Return\n");
    for v in &r.values {
        indent(depth + 1, out);
        match &v.name {
            Some(n) => out.push_str(&format!("(Value {n:?}")),
            None => out.push_str("(Value _"),
        }
        if let Some(t) = &v.type_ {
            out.push('\n');
            fmt_expr(t, depth + 2, out);
            indent(depth + 1, out);
            out.push_str(")\n");
        } else {
            out.push_str(")\n");
        }
    }
    if let Some(rest) = &r.rest {
        indent(depth + 1, out);
        out.push_str(&format!("(Rest {:?})\n", rest.name.as_deref().unwrap_or("_")));
    }
    indent(depth, out);
    out.push_str(")\n");
}

fn fmt_body(body: &[Statement], depth: usize, out: &mut String) {
    indent(depth, out);
    out.push_str("(Body\n");
    for s in body {
        fmt_stmt(s, depth + 1, out);
    }
    indent(depth, out);
    out.push_str(")\n");
}

fn fmt_item(it: &Item, depth: usize, out: &mut String) {
    indent(depth, out);
    match it {
        Item::DefineConstant {
            name,
            type_,
            value,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineConstant {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            if let Some(t) = type_ {
                indent(depth + 1, out);
                out.push_str("(Type\n");
                fmt_expr(t, depth + 2, out);
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            indent(depth + 1, out);
            out.push_str("(Value\n");
            fmt_expr(value, depth + 2, out);
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineVariable {
            name,
            type_,
            value,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineVariable {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            if let Some(t) = type_ {
                indent(depth + 1, out);
                out.push_str("(Type\n");
                fmt_expr(t, depth + 2, out);
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            indent(depth + 1, out);
            out.push_str("(Value\n");
            fmt_expr(value, depth + 2, out);
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineFunction {
            name,
            params,
            return_,
            body,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineFunction {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            fmt_params(params, depth + 1, out);
            fmt_return(return_, depth + 1, out);
            fmt_body(body, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineMethod {
            name,
            params,
            return_,
            body,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineMethod {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            fmt_params(params, depth + 1, out);
            fmt_return(return_, depth + 1, out);
            fmt_body(body, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineGeneric {
            name,
            params,
            return_,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineGeneric {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            fmt_params(params, depth + 1, out);
            fmt_return(return_, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineClass {
            name,
            supers,
            slots,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineClass {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            indent(depth + 1, out);
            out.push_str("(Supers\n");
            for s in supers {
                fmt_expr(s, depth + 2, out);
            }
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth + 1, out);
            out.push_str("(Slots\n");
            for sl in slots {
                indent(depth + 2, out);
                out.push_str(&format!(
                    "(Slot {:?} alloc={:?} req-init={} ",
                    sl.name, sl.allocation, sl.required_init_keyword
                ));
                if let Some(k) = &sl.init_keyword {
                    out.push_str(&format!("init-key={k:?} "));
                }
                out.push_str(")\n");
            }
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineLibrary {
            name,
            uses,
            exports,
            creates,
            ..
        } => {
            out.push_str(&format!("(DefineLibrary {name:?}\n"));
            for u in uses {
                indent(depth + 1, out);
                out.push_str(&format!("(Use {:?})\n", u.name));
            }
            for e in exports {
                indent(depth + 1, out);
                out.push_str(&format!("(Export {e:?})\n"));
            }
            for c in creates {
                indent(depth + 1, out);
                out.push_str(&format!("(Create {c:?})\n"));
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineModule {
            name,
            uses,
            exports,
            creates,
            ..
        } => {
            out.push_str(&format!("(DefineModule {name:?}\n"));
            for u in uses {
                indent(depth + 1, out);
                out.push_str(&format!("(Use {:?})\n", u.name));
            }
            for e in exports {
                indent(depth + 1, out);
                out.push_str(&format!("(Export {e:?})\n"));
            }
            for c in creates {
                indent(depth + 1, out);
                out.push_str(&format!("(Create {c:?})\n"));
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineCFunction {
            name,
            params,
            return_,
            c_name,
            library,
            modifiers,
            ..
        } => {
            out.push_str(&format!("(DefineCFunction {name:?}\n"));
            fmt_modifiers(modifiers, depth + 1, out);
            fmt_params(params, depth + 1, out);
            fmt_return(return_, depth + 1, out);
            indent(depth + 1, out);
            out.push_str(&format!("(library {library:?})\n"));
            if let Some(cn) = c_name {
                indent(depth + 1, out);
                out.push_str(&format!("(c-name {cn:?})\n"));
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Item::DefineMacro { name, body_fragments, .. } => {
            out.push_str(&format!(
                "(DefineMacro {name:?} fragments={})\n",
                body_fragments.len()
            ));
        }
        Item::DefineOther {
            modifiers,
            keyword,
            name,
            body_fragments,
            ..
        } => {
            out.push_str(&format!(
                "(DefineOther {keyword:?} name={:?} fragments={})\n",
                name.as_deref().unwrap_or(""),
                body_fragments.len()
            ));
            fmt_modifiers(modifiers, depth + 1, out);
        }
        Item::Expr(e) => {
            out.push_str("(TopExpr\n");
            fmt_expr(e, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
    }
}

fn fmt_stmt(s: &Statement, depth: usize, out: &mut String) {
    indent(depth, out);
    match s {
        Statement::Expr(e) => {
            out.push_str("(Stmt\n");
            fmt_expr(e, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::Let { binders, rest, value, .. } => {
            out.push_str("(Let\n");
            indent(depth + 1, out);
            out.push_str("(Binders");
            for b in binders {
                out.push_str(&format!(" {:?}", b.name));
            }
            if let Some(r) = rest {
                out.push_str(&format!(" #rest {:?}", r.name));
            }
            out.push_str(")\n");
            indent(depth + 1, out);
            out.push_str("(Value\n");
            fmt_expr(value, depth + 2, out);
            indent(depth + 1, out);
            out.push_str(")\n");
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::Local { methods, .. } => {
            out.push_str("(Local\n");
            for m in methods {
                indent(depth + 1, out);
                out.push_str(&format!("(Method {:?})\n", m.name));
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::For { clauses, body, finally_, .. } => {
            out.push_str(&format!("(For clauses={}\n", clauses.len()));
            fmt_body(body, depth + 1, out);
            if !finally_.is_empty() {
                indent(depth + 1, out);
                out.push_str("(Finally\n");
                for f in finally_ {
                    fmt_stmt(f, depth + 2, out);
                }
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::While { cond, body, .. } => {
            out.push_str("(While\n");
            fmt_expr(cond, depth + 1, out);
            fmt_body(body, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::Until { cond, body, .. } => {
            out.push_str("(Until\n");
            fmt_expr(cond, depth + 1, out);
            fmt_body(body, depth + 1, out);
            indent(depth, out);
            out.push_str(")\n");
        }
        Statement::Block {
            exit_var,
            body,
            handlers,
            cleanup,
            afterwards,
            ..
        } => {
            out.push_str(&format!(
                "(Block exit={:?}\n",
                exit_var.as_deref().unwrap_or("")
            ));
            fmt_body(body, depth + 1, out);
            for h in handlers {
                indent(depth + 1, out);
                out.push_str(&format!(
                    "(Exception var={:?})\n",
                    h.var.as_deref().unwrap_or("")
                ));
            }
            if !cleanup.is_empty() {
                indent(depth + 1, out);
                out.push_str("(Cleanup\n");
                for c in cleanup {
                    fmt_stmt(c, depth + 2, out);
                }
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            if !afterwards.is_empty() {
                indent(depth + 1, out);
                out.push_str("(Afterwards\n");
                for c in afterwards {
                    fmt_stmt(c, depth + 2, out);
                }
                indent(depth + 1, out);
                out.push_str(")\n");
            }
            indent(depth, out);
            out.push_str(")\n");
        }
    }
}
