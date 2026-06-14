//! Indented DFM dump — stable line-oriented format used by `dump-dfm`,
//! snapshot tests, and (Sprint 26b) the IDE inspector panel.

use std::fmt::Write;

use crate::ir::{Computation, ConstValue, Function, Terminator, TypeEstimate};

fn type_label(ty: TypeEstimate) -> String {
    match ty {
        TypeEstimate::Class(id) => format!("<class:{id}>"),
        TypeEstimate::Singleton(bits) => format!("<singleton:{bits:#x}>"),
        other => other.name().to_string(),
    }
}

pub fn format_dfm(f: &Function) -> String {
    let mut out = String::new();
    fmt_function(f, &mut out);
    out
}

pub fn format_dfm_module(fns: &[Function]) -> String {
    let mut out = String::new();
    for (i, f) in fns.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        fmt_function(f, &mut out);
    }
    out
}

/// Sprint 37: produce a fully-canonical text representation of a DFM
/// module suitable for use as input to a JIT-cache hash.
///
/// The format is identical to [`format_dfm_module`] today — the existing
/// dump is already line-oriented and emits functions / blocks /
/// computations in deterministic source order, with no environment-
/// dependent values (no addresses, no timestamps, no `Debug`-derived
/// `HashMap` iteration).
///
/// The function exists as a separate API so future format changes can
/// be made for cache-key needs without disturbing the IDE-inspector /
/// snapshot-test consumers of [`format_dfm_module`]. Bumping the cache
/// key's version constant (see `nod-llvm::cache::CACHE_KEY_VERSION`) is
/// the mechanism for invalidating stale cached objects after a format
/// change.
pub fn format_for_cache_key(fns: &[Function]) -> String {
    format_dfm_module(fns)
}

fn fmt_function(f: &Function, out: &mut String) {
    let _ = write!(out, "fn {} (", f.name);
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "t{}: {}", p.0, type_label(f.temp_type(*p)));
    }
    let _ = writeln!(out, ") -> {}:", type_label(f.return_type));
    for block in &f.blocks {
        out.push_str("  ");
        let _ = write!(out, "{}", block.label);
        if !block.params.is_empty() {
            out.push('(');
            for (i, bp) in block.params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "t{}: {}", bp.0, type_label(f.temp_type(*bp)));
            }
            out.push(')');
        }
        out.push_str(":\n");
        for c in &block.computations {
            fmt_computation(c, f, out);
        }
        fmt_terminator(&block.terminator, f, out);
    }
}

fn fmt_computation(c: &Computation, f: &Function, out: &mut String) {
    out.push_str("    ");
    match c {
        Computation::Const { dst, value } => {
            let _ = write!(out, "t{}: {} = Const ", dst.0, f.temp_type(*dst).name());
            fmt_const(value, out);
            out.push('\n');
        }
        Computation::PrimOp { dst, op, args } => {
            let _ = write!(
                out,
                "t{}: {} = PrimOp {}",
                dst.0,
                f.temp_type(*dst).name(),
                op.name()
            );
            for a in args {
                let _ = write!(out, " t{}", a.0);
            }
            out.push('\n');
        }
        Computation::DirectCall { dst, callee, args, safepoint_roots, is_no_alloc } => {
            let _ = write!(
                out,
                "t{}: {} = DirectCall {}(",
                dst.0,
                f.temp_type(*dst).name(),
                callee
            );
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "t{}", a.0);
            }
            out.push(')');
            fmt_safepoint(safepoint_roots, out);
            // Sprint 48: emit [no_alloc] annotation when set. Folded
            // into the dump output (and therefore into the cache key
            // produced by `format_for_cache_key`) so that flipping a
            // primitive's annotation invalidates stale cached bitcode.
            if *is_no_alloc {
                out.push_str(" [no_alloc]");
            }
            out.push('\n');
        }
        Computation::Call { dst, callee, args, safepoint_roots } => {
            let _ = write!(
                out,
                "t{}: {} = Call t{}(",
                dst.0,
                f.temp_type(*dst).name(),
                callee.0
            );
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "t{}", a.0);
            }
            out.push(')');
            fmt_safepoint(safepoint_roots, out);
            out.push('\n');
        }
        Computation::TypeCheck { dst, value, class } => {
            let _ = writeln!(
                out,
                "t{}: {} = TypeCheck t{} {}",
                dst.0,
                f.temp_type(*dst).name(),
                value.0,
                class.name()
            );
        }
        Computation::WriteBarrier { dst, slot, value } => {
            let _ = writeln!(
                out,
                "t{}: {} = WriteBarrier t{} := t{}",
                dst.0,
                f.temp_type(*dst).name(),
                slot.0,
                value.0
            );
        }
        Computation::LoadSlot { dst, instance, offset, slot_type } => {
            let _ = writeln!(
                out,
                "t{}: {} = LoadSlot t{} @{} [{:?}]",
                dst.0,
                f.temp_type(*dst).name(),
                instance.0,
                offset,
                slot_type
            );
        }
        Computation::StoreSlot { dst, instance, offset, value, slot_type } => {
            let _ = writeln!(
                out,
                "t{}: {} = StoreSlot t{} @{} := t{} [{:?}]",
                dst.0,
                f.temp_type(*dst).name(),
                instance.0,
                offset,
                value.0,
                slot_type
            );
        }
        Computation::Dispatch { dst, generic_name, args, safepoint_roots } => {
            let _ = write!(
                out,
                "t{}: {} = Dispatch {}(",
                dst.0,
                type_label(f.temp_type(*dst)),
                generic_name
            );
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "t{}", a.0);
            }
            out.push(')');
            fmt_safepoint(safepoint_roots, out);
            out.push('\n');
        }
        Computation::SealedDirectCall {
            dst,
            method,
            fallback_chain,
            generic_name,
            args,
            safepoint_roots,
            is_no_alloc,
        } => {
            let _ = write!(
                out,
                "t{}: {} = SealedDirectCall {}(",
                dst.0,
                type_label(f.temp_type(*dst)),
                method
            );
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "t{}", a.0);
            }
            out.push(')');
            fmt_safepoint(safepoint_roots, out);
            // Sprint 48: see DirectCall above.
            if *is_no_alloc {
                out.push_str(" [no_alloc]");
            }
            // Trailing comment annotation per spec §7.3 — `; sealed-direct`
            // marker plus the generic name and chain depth, so the dump is
            // self-explanatory.
            let _ = write!(
                out,
                "  ; sealed-direct on `{generic_name}` (chain={})",
                fallback_chain.len()
            );
            out.push('\n');
        }
    }
}

fn fmt_safepoint(roots: &[crate::ir::TempId], out: &mut String) {
    if roots.is_empty() {
        return;
    }
    out.push_str("  safepoint=[");
    for (i, r) in roots.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "t{}", r.0);
    }
    out.push(']');
}

fn fmt_const(v: &ConstValue, out: &mut String) {
    match v {
        ConstValue::Integer(i) => {
            let _ = write!(out, "Integer({i})");
        }
        ConstValue::Float(f) => {
            let _ = write!(out, "Float({f:?})");
        }
        ConstValue::Bool(b) => {
            let _ = write!(out, "Bool({b})");
        }
        ConstValue::String(s) => {
            let _ = write!(out, "String({s:?})");
        }
        ConstValue::Char(c) => {
            let _ = write!(out, "Char({c:?})");
        }
        ConstValue::Unit => {
            out.push_str("Unit");
        }
        ConstValue::WordBits(bits) => {
            let _ = write!(out, "WordBits({bits:#x})");
        }
        ConstValue::ClassMetadataPtr { class_id, tagged } => {
            let _ = write!(out, "ClassMetadataPtr({class_id}, tagged={tagged})");
        }
        ConstValue::StringLiteralRef(s) => {
            let _ = write!(out, "StringLiteralRef({s:?})");
        }
        ConstValue::SymbolLiteralRef(name) => {
            let _ = write!(out, "SymbolLiteralRef({name:?})");
        }
        ConstValue::StubEntryRef {
            dll,
            symbol,
            signature_bytes,
        } => {
            // Render the signature as a stable hex string so two
            // sites with the same (dll, symbol) but different
            // marshaling shapes hash distinctly.
            out.push_str("StubEntryRef(");
            let _ = write!(out, "{dll:?}, {symbol:?}, sig=");
            for b in signature_bytes {
                let _ = write!(out, "{b:02x}");
            }
            out.push(')');
        }
    }
}

fn fmt_terminator(t: &Terminator, f: &Function, out: &mut String) {
    out.push_str("    ");
    match t {
        Terminator::Return { value: Some(v) } => {
            let _ = writeln!(out, "Return t{}", v.0);
        }
        Terminator::Return { value: None } => {
            out.push_str("Return\n");
        }
        Terminator::If {
            cond,
            then_block,
            else_block,
        } => {
            let _ = writeln!(
                out,
                "If t{} {} {}",
                cond.0,
                block_label(f, *then_block),
                block_label(f, *else_block)
            );
        }
        Terminator::Jump { target, args } => {
            let _ = write!(out, "Jump {}", block_label(f, *target));
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "t{}", a.0);
                }
                out.push(')');
            }
            out.push('\n');
        }
    }
}

fn block_label(f: &Function, id: crate::ir::BlockId) -> String {
    match f.block(id) {
        Some(b) => b.label.clone(),
        None => format!("<missing b{}>", id.0),
    }
}
