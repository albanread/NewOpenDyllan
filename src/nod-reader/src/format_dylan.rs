//! Sprint 04 pretty-printer.
//!
//! Produces parseable Dylan from a [`Module`]. The acceptance criterion is
//! AST → pretty-print → re-parse → identical shape. The output here is
//! NOT byte-for-byte faithful to the input — it's a canonical form.

use crate::ast::{
    BinOp, Binder, ExceptionClause, Expr, ForClause, ImportSet, Item, LocalMethodDecl,
    Module, Param, ReturnSig, SlotAllocation, SlotDef, Statement, UnOp,
};

pub fn format_dylan(module: &Module) -> String {
    let mut out = String::new();
    for (k, v) in &module.header {
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
        out.push('\n');
    }
    if !module.header.is_empty() {
        out.push('\n');
    }
    for (i, it) in module.items.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        fmt_item(it, 0, &mut out);
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn fmt_modifiers(mods: &[crate::ast::Modifier], out: &mut String) {
    for m in mods {
        out.push_str(m.name());
        out.push(' ');
    }
}

fn fmt_item(it: &Item, depth: usize, out: &mut String) {
    indent(out, depth);
    match it {
        Item::DefineConstant { modifiers, name, type_, value, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("constant ");
            out.push_str(name);
            if let Some(t) = type_ {
                out.push_str(" :: ");
                fmt_expr(t, out);
            }
            out.push_str(" = ");
            fmt_expr(value, out);
            out.push_str(";\n");
        }
        Item::DefineVariable { modifiers, name, type_, value, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("variable ");
            out.push_str(name);
            if let Some(t) = type_ {
                out.push_str(" :: ");
                fmt_expr(t, out);
            }
            out.push_str(" = ");
            fmt_expr(value, out);
            out.push_str(";\n");
        }
        Item::DefineFunction { modifiers, name, params, return_, body, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("function ");
            out.push_str(name);
            out.push(' ');
            fmt_params(params, out);
            fmt_return(return_, out);
            out.push('\n');
            fmt_body(body, depth + 1, out);
            indent(out, depth);
            out.push_str("end function ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineMethod { modifiers, name, params, return_, body, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("method ");
            out.push_str(name);
            out.push(' ');
            fmt_params(params, out);
            fmt_return(return_, out);
            out.push('\n');
            fmt_body(body, depth + 1, out);
            indent(out, depth);
            out.push_str("end method ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineGeneric { modifiers, name, params, return_, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("generic ");
            out.push_str(name);
            out.push(' ');
            fmt_params(params, out);
            fmt_return(return_, out);
            out.push_str(";\n");
        }
        Item::DefineClass { modifiers, name, supers, slots, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("class ");
            out.push_str(name);
            out.push_str(" (");
            for (i, s) in supers.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                fmt_expr(s, out);
            }
            out.push_str(")\n");
            for sl in slots {
                fmt_slot(sl, depth + 1, out);
            }
            indent(out, depth);
            out.push_str("end class ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineLibrary { name, uses, exports, creates, .. } => {
            out.push_str("define library ");
            out.push_str(name);
            out.push('\n');
            for u in uses {
                indent(out, depth + 1);
                out.push_str("use ");
                out.push_str(&u.name);
                fmt_use_options(
                    &u.import,
                    &u.exclude,
                    &u.rename,
                    &u.prefix,
                    &u.export,
                    out,
                );
                out.push_str(";\n");
            }
            if !exports.is_empty() {
                indent(out, depth + 1);
                out.push_str("export ");
                for (i, e) in exports.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(e);
                }
                out.push_str(";\n");
            }
            if !creates.is_empty() {
                indent(out, depth + 1);
                out.push_str("create ");
                for (i, c) in creates.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(c);
                }
                out.push_str(";\n");
            }
            indent(out, depth);
            out.push_str("end library ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineModule { name, uses, exports, creates, .. } => {
            out.push_str("define module ");
            out.push_str(name);
            out.push('\n');
            for u in uses {
                indent(out, depth + 1);
                out.push_str("use ");
                out.push_str(&u.name);
                fmt_use_options(
                    &u.import,
                    &u.exclude,
                    &u.rename,
                    &u.prefix,
                    &u.export,
                    out,
                );
                out.push_str(";\n");
            }
            if !exports.is_empty() {
                indent(out, depth + 1);
                out.push_str("export ");
                for (i, e) in exports.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(e);
                }
                out.push_str(";\n");
            }
            if !creates.is_empty() {
                indent(out, depth + 1);
                out.push_str("create ");
                for (i, c) in creates.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(c);
                }
                out.push_str(";\n");
            }
            indent(out, depth);
            out.push_str("end module ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineCFunction { modifiers, name, params, return_, c_name, library, .. } => {
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str("c-function ");
            out.push_str(name);
            out.push(' ');
            fmt_params(params, out);
            fmt_return(return_, out);
            out.push_str(";\n");
            if let Some(cn) = c_name {
                indent(out, depth + 1);
                out.push_str("c-name: \"");
                out.push_str(cn);
                out.push_str("\";\n");
            }
            indent(out, depth + 1);
            out.push_str("library: \"");
            out.push_str(library);
            out.push_str("\";\n");
            indent(out, depth);
            out.push_str("end c-function ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineMacro { name, body_fragments, .. } => {
            // Emit a stub body that is bracket-balanced and contains no
            // top-level `end` token. The original body fragments cannot be
            // round-tripped without the source map; the shape-level
            // round-trip just needs the item kind and name to survive.
            let _ = body_fragments;
            out.push_str("define macro ");
            out.push_str(name);
            out.push_str("\n  { }\n");
            indent(out, depth);
            out.push_str("end macro ");
            out.push_str(name);
            out.push_str(";\n");
        }
        Item::DefineOther { modifiers, keyword, name, body_fragments, .. } => {
            let _ = body_fragments;
            out.push_str("define ");
            fmt_modifiers(modifiers, out);
            out.push_str(keyword);
            if let Some(n) = name {
                out.push(' ');
                out.push_str(n);
            }
            out.push_str(" ()\n");
            indent(out, depth);
            out.push_str("end ");
            out.push_str(keyword);
            if let Some(n) = name {
                out.push(' ');
                out.push_str(n);
            }
            out.push_str(";\n");
        }
        Item::Expr(e) => {
            fmt_expr(e, out);
            out.push_str(";\n");
        }
    }
}

fn fmt_use_options(
    import: &Option<ImportSet>,
    exclude: &[String],
    rename: &[(String, String)],
    prefix: &Option<String>,
    export: &Option<ImportSet>,
    out: &mut String,
) {
    if let Some(is) = import {
        out.push_str(", import: ");
        fmt_import_set(is, out);
    }
    if !exclude.is_empty() {
        out.push_str(", exclude: { ");
        for (i, e) in exclude.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(e);
        }
        out.push_str(" }");
    }
    if !rename.is_empty() {
        out.push_str(", rename: { ");
        for (i, (a, b)) in rename.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(a);
            out.push_str(" => ");
            out.push_str(b);
        }
        out.push_str(" }");
    }
    if let Some(p) = prefix {
        out.push_str(", prefix: \"");
        out.push_str(p);
        out.push('"');
    }
    if let Some(is) = export {
        out.push_str(", export: ");
        fmt_import_set(is, out);
    }
}

fn fmt_import_set(is: &ImportSet, out: &mut String) {
    match is {
        ImportSet::All => out.push_str("all"),
        ImportSet::Items(v) => {
            out.push_str("{ ");
            for (i, s) in v.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&s.name);
                if let Some(r) = &s.rename {
                    out.push_str(" => ");
                    out.push_str(r);
                }
            }
            out.push_str(" }");
        }
    }
}

fn fmt_slot(sl: &SlotDef, depth: usize, out: &mut String) {
    indent(out, depth);
    match sl.allocation {
        SlotAllocation::Class => out.push_str("class slot "),
        SlotAllocation::EachSubclass => out.push_str("each-subclass slot "),
        SlotAllocation::Virtual => out.push_str("virtual slot "),
        SlotAllocation::Constant => out.push_str("constant slot "),
        SlotAllocation::Instance => out.push_str("slot "),
    }
    out.push_str(&sl.name);
    if let Some(t) = &sl.type_ {
        out.push_str(" :: ");
        fmt_expr(t, out);
    }
    if let Some(iv) = &sl.init_value {
        out.push_str(", init-value: ");
        fmt_expr(iv, out);
    }
    if let Some(k) = &sl.init_keyword {
        if sl.required_init_keyword {
            out.push_str(", required-init-keyword: ");
        } else {
            out.push_str(", init-keyword: ");
        }
        out.push_str(k);
        out.push(':');
    }
    if let Some(s) = sl.setter {
        out.push_str(", setter: ");
        out.push_str(if s { "#t" } else { "#f" });
    }
    out.push_str(";\n");
}

fn fmt_params(params: &[Param], out: &mut String) {
    out.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&p.name);
        if let Some(t) = &p.type_ {
            out.push_str(" :: ");
            fmt_expr(t, out);
        }
    }
    out.push(')');
}

fn fmt_return(r: &Option<ReturnSig>, out: &mut String) {
    let Some(r) = r else { return };
    out.push_str(" => (");
    let mut first = true;
    for v in &r.values {
        if !first {
            out.push_str(", ");
        }
        first = false;
        if let Some(n) = &v.name {
            out.push_str(n);
            if let Some(t) = &v.type_ {
                out.push_str(" :: ");
                fmt_expr(t, out);
            }
        } else if let Some(t) = &v.type_ {
            fmt_expr(t, out);
        }
    }
    if let Some(rest) = &r.rest {
        if !first {
            out.push_str(", ");
        }
        out.push_str("#rest ");
        out.push_str(rest.name.as_deref().unwrap_or("_"));
    }
    out.push(')');
}

fn fmt_body(body: &[Statement], depth: usize, out: &mut String) {
    for (i, s) in body.iter().enumerate() {
        indent(out, depth);
        fmt_stmt(s, depth, out);
        if i + 1 < body.len() {
            out.push_str(";\n");
        } else {
            out.push('\n');
        }
    }
}

fn fmt_stmt(s: &Statement, depth: usize, out: &mut String) {
    match s {
        Statement::Expr(e) => fmt_expr(e, out),
        Statement::Let { binders, rest, value, .. } => {
            out.push_str("let ");
            if binders.len() == 1 && rest.is_none() {
                fmt_binder(&binders[0], out);
            } else {
                out.push('(');
                for (i, b) in binders.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    fmt_binder(b, out);
                }
                if let Some(r) = rest {
                    if !binders.is_empty() {
                        out.push_str(", ");
                    }
                    out.push_str("#rest ");
                    out.push_str(&r.name);
                }
                out.push(')');
            }
            out.push_str(" = ");
            fmt_expr(value, out);
        }
        Statement::Local { methods, .. } => {
            out.push_str("local\n");
            for (i, m) in methods.iter().enumerate() {
                indent(out, depth + 1);
                fmt_local_method(m, depth + 1, out);
                if i + 1 < methods.len() {
                    out.push_str(",\n");
                } else {
                    out.push('\n');
                }
            }
        }
        Statement::For { clauses, body, finally_, .. } => {
            out.push_str("for (");
            for (i, c) in clauses.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                fmt_for_clause(c, out);
            }
            out.push_str(")\n");
            fmt_body(body, depth + 1, out);
            if !finally_.is_empty() {
                indent(out, depth);
                out.push_str("finally\n");
                fmt_body(finally_, depth + 1, out);
            }
            indent(out, depth);
            out.push_str("end for");
        }
        Statement::While { cond, body, .. } => {
            out.push_str("while (");
            fmt_expr(cond, out);
            out.push_str(")\n");
            fmt_body(body, depth + 1, out);
            indent(out, depth);
            out.push_str("end while");
        }
        Statement::Until { cond, body, .. } => {
            out.push_str("until (");
            fmt_expr(cond, out);
            out.push_str(")\n");
            fmt_body(body, depth + 1, out);
            indent(out, depth);
            out.push_str("end until");
        }
        Statement::Block {
            exit_var,
            body,
            handlers,
            cleanup,
            afterwards,
            ..
        } => {
            out.push_str("block (");
            if let Some(v) = exit_var {
                out.push_str(v);
            }
            out.push_str(")\n");
            fmt_body(body, depth + 1, out);
            for h in handlers {
                indent(out, depth);
                fmt_exception(h, depth, out);
            }
            if !cleanup.is_empty() {
                indent(out, depth);
                out.push_str("cleanup\n");
                fmt_body(cleanup, depth + 1, out);
            }
            if !afterwards.is_empty() {
                indent(out, depth);
                out.push_str("afterwards\n");
                fmt_body(afterwards, depth + 1, out);
            }
            indent(out, depth);
            out.push_str("end block");
        }
    }
}

fn fmt_exception(h: &ExceptionClause, depth: usize, out: &mut String) {
    out.push_str("exception (");
    if let Some(v) = &h.var {
        out.push_str(v);
        out.push_str(" :: ");
    }
    fmt_expr(&h.class, out);
    out.push_str(")\n");
    fmt_body(&h.body, depth + 1, out);
}

fn fmt_local_method(m: &LocalMethodDecl, depth: usize, out: &mut String) {
    out.push_str("method ");
    out.push_str(&m.name);
    out.push(' ');
    fmt_params(&m.params, out);
    fmt_return(&m.return_, out);
    out.push('\n');
    fmt_body(&m.body, depth + 1, out);
    indent(out, depth);
    out.push_str("end method");
}

fn fmt_binder(b: &Binder, out: &mut String) {
    out.push_str(&b.name);
    if let Some(t) = &b.type_ {
        out.push_str(" :: ");
        fmt_expr(t, out);
    }
}

fn fmt_for_clause(c: &ForClause, out: &mut String) {
    match c {
        ForClause::Numeric(n) => {
            out.push_str(&n.var);
            out.push_str(" from ");
            fmt_expr(&n.from, out);
            if let Some(e) = &n.to {
                out.push_str(" to ");
                fmt_expr(e, out);
            }
            if let Some(e) = &n.below {
                out.push_str(" below ");
                fmt_expr(e, out);
            }
            if let Some(e) = &n.above {
                out.push_str(" above ");
                fmt_expr(e, out);
            }
            if let Some(e) = &n.by {
                out.push_str(" by ");
                fmt_expr(e, out);
            }
        }
        ForClause::In { var, coll, .. } => {
            out.push_str(var);
            out.push_str(" in ");
            fmt_expr(coll, out);
        }
        ForClause::From(f) => {
            out.push_str(&f.var);
            out.push_str(" from ");
            fmt_expr(&f.from, out);
            if let Some(e) = &f.by {
                out.push_str(" by ");
                fmt_expr(e, out);
            }
        }
        ForClause::Until { cond, .. } => {
            out.push_str("until ");
            fmt_expr(cond, out);
        }
        ForClause::While { cond, .. } => {
            out.push_str("while ");
            fmt_expr(cond, out);
        }
        ForClause::Step(s) => {
            out.push_str(&s.var);
            out.push_str(" = ");
            fmt_expr(&s.init, out);
            if let Some(n) = &s.next {
                out.push_str(" then ");
                fmt_expr(n, out);
            }
        }
        ForClause::Keyed { var, key, coll, .. } => {
            out.push_str(var);
            out.push_str(" keyed-by ");
            out.push_str(key);
            out.push_str(" in ");
            fmt_expr(coll, out);
        }
    }
}

fn fmt_expr(e: &Expr, out: &mut String) {
    match e {
        Expr::Integer(_, v) => out.push_str(&v.to_string()),
        Expr::Float(_, v) => out.push_str(&format!("{v}")),
        Expr::String(_, s) => {
            // Already-quoted in `s`.
            out.push_str(s);
        }
        Expr::Char(_, c) => {
            out.push('\'');
            out.push(*c);
            out.push('\'');
        }
        Expr::Bool(_, b) => out.push_str(if *b { "#t" } else { "#f" }),
        Expr::Symbol(_, s) => out.push_str(s),
        Expr::Ident(_, n) => out.push_str(n),
        Expr::Call { callee, args, .. } => {
            // Hash-literal back-conversion.
            if let Expr::Ident(_, name) = callee.as_ref() {
                if let Some(open) = match name.as_str() {
                    "#list" => Some(("#(", ")")),
                    "#vector" => Some(("#[", "]")),
                    "#set" => Some(("#{", "}")),
                    _ => None,
                } {
                    out.push_str(open.0);
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        fmt_expr(a, out);
                    }
                    out.push_str(open.1);
                    return;
                }
                // Synthesised keyword-argument: emit `key: value` directly so
                // the round-trip lexes back into the same KeywordColon arg.
                if name == "%kw-arg"
                    && args.len() == 2
                    && let Expr::Symbol(_, k) = &args[0]
                {
                    out.push_str(k);
                    out.push(' ');
                    fmt_expr(&args[1], out);
                    return;
                }
            }
            fmt_expr(callee, out);
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                fmt_expr(a, out);
            }
            out.push(')');
        }
        Expr::BinOp { op, lhs, rhs, .. } => {
            fmt_expr(lhs, out);
            out.push(' ');
            out.push_str(op_text(*op));
            out.push(' ');
            fmt_expr(rhs, out);
        }
        Expr::UnOp { op, operand, .. } => {
            out.push_str(match op {
                UnOp::Neg => "-",
                UnOp::Not => "~",
            });
            fmt_expr(operand, out);
        }
        Expr::Paren { inner, .. } => {
            out.push('(');
            fmt_expr(inner, out);
            out.push(')');
        }
        Expr::If { cond, then_, else_, .. } => {
            out.push_str("if (");
            fmt_expr(cond, out);
            out.push_str(") ");
            fmt_expr(then_, out);
            if let Some(e) = else_ {
                out.push_str(" else ");
                fmt_expr(e, out);
            }
            out.push_str(" end");
        }
        Expr::MacroCall { name, .. } => {
            // Sprint 25: pretty-print body-shaped macro calls as a
            // pseudo-call followed by `end`. We don't have the head /
            // body in the AST (only the source span), so the output
            // is intentionally schematic — round-trip fidelity for
            // macro call sites isn't a property the formatter
            // currently promises.
            out.push_str(name);
            out.push_str("() end");
        }
        Expr::Case { arms, otherwise, .. } => {
            out.push_str("case ");
            for a in arms {
                fmt_expr(&a.cond, out);
                out.push_str(" => ");
                for b in &a.body {
                    fmt_expr(b, out);
                    out.push_str("; ");
                }
            }
            if let Some(o) = otherwise {
                out.push_str("otherwise => ");
                fmt_expr(o, out);
            }
            out.push_str(" end");
        }
        Expr::Begin { body, .. } => {
            out.push_str("begin ");
            for (i, b) in body.iter().enumerate() {
                if i > 0 {
                    out.push_str("; ");
                }
                fmt_expr(b, out);
            }
            out.push_str(" end");
        }
        Expr::Let { binder, value, .. } => {
            out.push_str("let ");
            out.push_str(binder);
            out.push_str(" = ");
            fmt_expr(value, out);
        }
        Expr::LocalMethod { name, params, body, .. } => {
            out.push_str("local method ");
            out.push_str(name);
            fmt_params(params, out);
            out.push(' ');
            for (i, b) in body.iter().enumerate() {
                if i > 0 {
                    out.push_str("; ");
                }
                fmt_expr(b, out);
            }
            out.push_str(" end");
        }
        Expr::Method { params, body, .. } => {
            out.push_str("method ");
            fmt_params(params, out);
            out.push(' ');
            for (i, b) in body.iter().enumerate() {
                if i > 0 {
                    out.push_str("; ");
                }
                fmt_expr(b, out);
            }
            out.push_str(" end");
        }
        Expr::Stmt(s) => {
            fmt_stmt(s, 0, out);
        }
    }
}

fn op_text(op: BinOp) -> &'static str {
    op.name()
}
