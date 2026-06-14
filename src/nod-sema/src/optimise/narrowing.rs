//! Type-estimate narrowing — Sprint 15 forward dataflow that
//! strengthens `TypeEstimate`s for the dispatch resolver to consult.
//!
//! See spec 15 §4 for the rules. Brief summary:
//!
//!   - Function/block parameters keep the estimate the lowering pass
//!     already gave them (driven by method specialisers / declared
//!     parameter types via Sprint 15's extended `type_from_expr`).
//!   - `make(<C>, ...)` → `Class(C)`.
//!   - `LoadSlot` whose slot type is `Class(C)` → `Class(C)`; integer
//!     slot → `Integer`; etc.
//!   - `Computation::TypeCheck` followed by `Terminator::If` widens
//!     the checked temp to `meet(prev_estimate, Class(target))` ON
//!     the then-branch only. The else-branch keeps the prior estimate
//!     (spec 15 §9.2 — over-conservative, sound).
//!   - `Computation::DirectCall` to a known top-level function inherits
//!     its declared return type (the temp's existing `type_estimate`
//!     already carries this).
//!
//! The pass is function-local. It runs as a single forward sweep over
//! the blocks in declaration order, treating block parameters as
//! join-points. Sprint 18 will promote this to whole-program propagation.

use std::collections::HashMap;

use nod_dfm::{BlockId, Computation, ConstValue, Function, TempId, Terminator, TypeEstimate};
use nod_runtime::{ClassId, class_metadata_ptr, is_subclass};

/// Per-temp narrowed type estimate. Indexed by `TempId`. Missing
/// entries default to the temp's existing `type_estimate` from the IR.
pub type NarrowedEstimates = HashMap<TempId, TypeEstimate>;

/// Run the narrowing pass on a single function.
pub fn narrow_function(f: &Function) -> NarrowedEstimates {
    let mut out: NarrowedEstimates = HashMap::new();
    for temp in &f.temps {
        out.insert(temp.id, temp.type_estimate);
    }

    // Pre-compute: for each `If` terminator that branches on a
    // TypeCheck dst, record `(then_block, value_temp, narrow_class)`.
    // We then narrow the value_temp on entry to that then_block.
    let mut typecheck_dst_to_check: HashMap<TempId, (TempId, ClassId)> = HashMap::new();
    for block in &f.blocks {
        for c in &block.computations {
            if let Computation::TypeCheck { dst, value, class } = c {
                // Only `UserClass` and the immediate-classed checks
                // strengthen the estimate. The unsupported / stub
                // checks contribute nothing.
                let cid = match class {
                    nod_dfm::ClassCheck::UserClass { id, .. } => Some(ClassId(*id)),
                    nod_dfm::ClassCheck::Integer => Some(ClassId::INTEGER),
                    nod_dfm::ClassCheck::Boolean => Some(ClassId::BOOLEAN),
                    nod_dfm::ClassCheck::String => Some(ClassId::BYTE_STRING),
                    nod_dfm::ClassCheck::Symbol => Some(ClassId::SYMBOL),
                    nod_dfm::ClassCheck::Vector => Some(ClassId::SIMPLE_OBJECT_VECTOR),
                    nod_dfm::ClassCheck::Character => Some(ClassId::CHARACTER),
                    nod_dfm::ClassCheck::EmptyList => Some(ClassId::EMPTY_LIST),
                    nod_dfm::ClassCheck::Unsupported { .. } => None,
                };
                if let Some(cid) = cid {
                    typecheck_dst_to_check.insert(*dst, (*value, cid));
                }
            }
        }
    }

    // Map each block id to a per-entry override map. The then-branch
    // of an `If t cond then else` where `t = TypeCheck v <C>` gets
    // an override `v -> Class(<C>) meet existing`.
    let mut block_entry_overrides: HashMap<BlockId, HashMap<TempId, TypeEstimate>> = HashMap::new();
    for block in &f.blocks {
        if let Terminator::If {
            cond,
            then_block,
            else_block: _,
        } = &block.terminator
            && let Some(&(value, cid)) = typecheck_dst_to_check.get(cond)
        {
            let entry = block_entry_overrides.entry(*then_block).or_default();
            // Apply meet with the current estimate of `value`.
            let prev = out.get(&value).copied().unwrap_or(TypeEstimate::Top);
            let new = prev.meet(TypeEstimate::Class(cid.0), &class_subclass_check);
            entry.insert(value, new);
        }
    }

    // First, find every class-ref Const and record the class id behind
    // each temp. These are the class-ref temps emitted by
    // `emit_class_metadata_ptr_const` and by `make`'s first-arg
    // lowering. We use this map to narrow `%make` call results.
    //
    // Sprint 38c — the class-ref bake site now uses
    // `ConstValue::ClassMetadataPtr { class_id, .. }` which carries
    // the id directly (no address-lookup needed). Pre-Sprint-38c
    // `ConstValue::WordBits(addr)` still flows here from legacy lowering
    // paths (`emit_string_literal`, `emit_symbol_literal`, the
    // %make/$NULL exit-procedure scaffolding); we keep that lookup arm
    // for those — only true class-metadata-pointer bits will resolve.
    let mut class_ref_temps: HashMap<TempId, ClassId> = HashMap::new();
    for block in &f.blocks {
        for c in &block.computations {
            match c {
                Computation::Const {
                    dst,
                    value: ConstValue::ClassMetadataPtr { class_id, .. },
                } => {
                    class_ref_temps.insert(*dst, ClassId(*class_id));
                }
                Computation::Const {
                    dst,
                    value: ConstValue::WordBits(bits),
                } => {
                    let raw = *bits;
                    let candidates = [raw, raw & !1u64];
                    for cand in candidates {
                        if cand == 0 {
                            continue;
                        }
                        if let Some(cid) = class_id_for_metadata_addr(cand) {
                            class_ref_temps.insert(*dst, cid);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Forward sweep — walk every block, then every computation in
    // order, refining the estimate map. Block-entry overrides flow in
    // at the start of each block; estimates set inside a block apply
    // to subsequent computations in the same block.
    //
    // We DON'T propagate across block boundaries beyond the then-
    // branch narrowing — multi-block analysis with phi nodes lands in
    // Sprint 18. The Sprint 15 pass is intentionally simple.
    for block in &f.blocks {
        if let Some(overrides) = block_entry_overrides.get(&block.id) {
            for (t, ty) in overrides {
                out.insert(*t, *ty);
            }
        }
        for c in &block.computations {
            if let Computation::DirectCall { dst, callee, args, .. } = c
                && callee == "%make"
                && let Some(first_arg) = args.first()
                && let Some(&cid) = class_ref_temps.get(first_arg)
            {
                // `make(<C>, ...)` → exactly `Class(<C>)`.
                out.insert(*dst, TypeEstimate::Class(cid.0));
                continue;
            }
            // Fall through — leave the temp's existing estimate alone.
            let dst = c.dst();
            let existing = out.get(&dst).copied().unwrap_or(TypeEstimate::Top);
            out.insert(dst, existing);
        }
    }

    out
}

/// If `addr` is the raw address of a registered `ClassMetadata`,
/// return its `ClassId`. Otherwise `None`. Walks the registry by
/// inspecting `class_metadata_ptr` for every seed + user class — a
/// linear scan is fine because the registry stays small in v1.
fn class_id_for_metadata_addr(addr: u64) -> Option<ClassId> {
    let target = addr as *const nod_runtime::ClassMetadata;
    let mut hit: Option<ClassId> = None;
    nod_runtime::for_each_class(|md| {
        if hit.is_some() {
            return;
        }
        let p = class_metadata_ptr(md.id);
        if p == target {
            hit = Some(md.id);
        }
    });
    hit
}

/// Subclass-check trampoline. The lattice ops in `nod-dfm` accept a
/// closure of this shape so the IR crate stays runtime-free.
fn class_subclass_check(sub: u32, sup: u32) -> bool {
    is_subclass(ClassId(sub), ClassId(sup))
}
