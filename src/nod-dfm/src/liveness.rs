//! Sprint 11b — across-call liveness for precise GC roots.
//!
//! The precise-roots story: at every potentially-allocating call site
//! (`Computation::DirectCall`, `Call`, `Dispatch`), the codegen layer
//! brackets the call with `nod_register_root` / `nod_unregister_root`
//! pairs around an alloca slot holding each live pointer-shaped temp.
//! The GC walks the registered slots, evacuates any reachable young
//! objects, and rewrites the slots; codegen reloads from the slot
//! after the call so downstream uses see the relocated address.
//!
//! This module computes the "which temps need protecting at which
//! call?" set via a real global **backward liveness dataflow fixpoint**
//! (GAP-011). An earlier version used a per-block approximation; it was
//! unsound for *live-through* temps — a temp defined in block A, used in
//! block C, but neither mentioned nor threaded through an intermediate
//! block B was invisible to B's analysis, so allocating calls in B got
//! no protecting root for it. A collection in that window reclaimed the
//! object without forwarding the (never-registered) root, leaving a
//! dangling pointer for the next use. Recursive-descent code (allocate,
//! then branch deeper, with an accumulator live throughout) hit this
//! constantly; straight-line / single-branch code did not.
//!
//! The algorithm now is the textbook one:
//!
//!   gen[B]  = upward-exposed uses (operands used in B before any def in B)
//!   kill[B] = temps defined in B (block params + each computation's dst)
//!   live_out[B] = U over successors S of live_in[S]
//!   live_in[B]  = gen[B] U (live_out[B] \ kill[B])
//!     iterated to a fixpoint.
//!
//! Then within each block, a backward sweep seeded with `live_out[B]`
//! (plus the terminator's operands) yields `live_after(c)` = the temps
//! live immediately after computation `c`. A temp is "live across" an
//! allocating call at `c` iff it is in `live_after(c)` and is not the
//! call's own result (the result is produced BY the call, not a
//! pre-existing value flowing across it).
//!
//!   safepoint_roots(c) = { t in live_after(c)
//!                            : t != dst(c) and t.type.needs_gc_protection() }
//!
//! Function parameters need no special-casing: they are never killed and
//! propagate as ordinary live-in/uses, so they are protected exactly
//! where they are actually live across a call.
//!
//! The output is written back into each call's `safepoint_roots` field,
//! sorted for deterministic codegen output and test snapshots.

use std::collections::{HashMap, HashSet};

use crate::ir::{Block, BlockId, Computation, Function, TempId, Terminator};

/// Run the per-block live-across-call analysis and populate
/// `safepoint_roots` on every call-shaped Computation in `f`.
///
/// Idempotent: calling twice produces the same result. Tests rely on
/// this.
pub fn populate_safepoint_roots(f: &mut Function) {
    let temp_types: HashMap<TempId, crate::ir::TypeEstimate> =
        f.temps.iter().map(|t| (t.id, t.type_estimate)).collect();
    let live_out = compute_global_live_out(f);

    for block_idx in 0..f.blocks.len() {
        // `live_after` owns its sets, so the immutable borrow of the block
        // ends here — we can mutate computations below.
        let live_after = live_after_per_computation(&f.blocks[block_idx], &live_out[block_idx]);
        let computations_len = f.blocks[block_idx].computations.len();
        for c_idx in 0..computations_len {
            if !f.blocks[block_idx].computations[c_idx].is_potentially_allocating_call() {
                continue;
            }
            let call_dst = f.blocks[block_idx].computations[c_idx].dst();
            // A temp is "live across" the call iff it is live immediately
            // after the call — excluding the call's own result (produced BY
            // the call) — and is GC-managed.
            //
            // GAP-011 hypothesis-test (2026-05-30): we briefly added
            // every heap-typed CALL ARG here regardless of post-call
            // liveness, per the agent-review "register-arg becomes
            // stale during callee" theory. The change closed 100% of
            // the `NOD_DIAG_ARG_ROOT_COVERAGE` gaps but did NOT fix the
            // GAP-011 crash — identical signature in
            // `stretchy_vector_push: not a <stretchy-vector>` with
            // `sv=0x...771 ptr=0x...770`. Hypothesis refuted. The
            // callee already spills incoming args to local slots at
            // -O0 and re-spills them to its own slab at every internal
            // safepoint where the arg is live across, so the caller's
            // slab adding the arg is redundant. The change was
            // reverted. The probe stays as a permanent diagnostic.
            let mut roots: Vec<TempId> = live_after[c_idx]
                .iter()
                .copied()
                .filter(|&t| t != call_dst)
                .filter(|t| {
                    temp_types
                        .get(t)
                        .map(|ty| ty.needs_gc_protection())
                        .unwrap_or(false)
                })
                .collect();
            roots.sort_by_key(|t| t.0);
            roots.dedup();
            if let Some(slot) = f.blocks[block_idx].computations[c_idx].safepoint_roots_mut() {
                *slot = roots;
            }
        }
    }
}

/// Successor blocks of a terminator.
fn terminator_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Return { .. } => Vec::new(),
        Terminator::If { then_block, else_block, .. } => vec![*then_block, *else_block],
        Terminator::Jump { target, .. } => vec![*target],
    }
}

/// Global backward liveness fixpoint. Returns `live_out`, indexed by block
/// *position* in `f.blocks`: `live_out[i]` is the set of temps live on exit
/// from `f.blocks[i]` (= the union of the live-in sets of its successors).
///
/// This is the real dataflow the old per-block approximation lacked. Without
/// it, a temp live THROUGH a block — defined upstream, used downstream,
/// neither referenced nor threaded through the block — was invisible, so
/// allocating calls in that block got no protecting root for it (GAP-011).
fn compute_global_live_out(f: &Function) -> Vec<HashSet<TempId>> {
    let n = f.blocks.len();
    let id_to_idx: HashMap<BlockId, usize> =
        f.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();

    // Per-block gen (upward-exposed uses: used before any def in the block)
    // and kill (defs: block params + each computation's dst). Function params
    // are never killed and propagate naturally as live-in/uses.
    let mut gen_set: Vec<HashSet<TempId>> = vec![HashSet::new(); n];
    let mut kill: Vec<HashSet<TempId>> = vec![HashSet::new(); n];
    for (i, b) in f.blocks.iter().enumerate() {
        let mut defined: HashSet<TempId> = HashSet::new();
        for &bp in &b.params {
            defined.insert(bp);
            kill[i].insert(bp);
        }
        for c in &b.computations {
            for op in computation_operands(c) {
                if !defined.contains(&op) {
                    gen_set[i].insert(op);
                }
            }
            let d = c.dst();
            defined.insert(d);
            kill[i].insert(d);
        }
        for op in terminator_uses(&b.terminator) {
            if !defined.contains(&op) {
                gen_set[i].insert(op);
            }
        }
    }

    let succ_idx: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|b| {
            terminator_successors(&b.terminator)
                .into_iter()
                .filter_map(|s| id_to_idx.get(&s).copied())
                .collect()
        })
        .collect();

    let mut live_in: Vec<HashSet<TempId>> = vec![HashSet::new(); n];
    let mut live_out: Vec<HashSet<TempId>> = vec![HashSet::new(); n];

    // Backward dataflow to a fixpoint. Reverse block order converges fast for
    // the forward-emitted blocks Dylan lowering produces; correctness is
    // order-independent.
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev() {
            let mut new_out: HashSet<TempId> = HashSet::new();
            for &s in &succ_idx[i] {
                new_out.extend(live_in[s].iter().copied());
            }
            let mut new_in = gen_set[i].clone();
            for &t in &new_out {
                if !kill[i].contains(&t) {
                    new_in.insert(t);
                }
            }
            if new_out != live_out[i] {
                live_out[i] = new_out;
                changed = true;
            }
            if new_in != live_in[i] {
                live_in[i] = new_in;
                changed = true;
            }
        }
    }

    live_out
}

/// For one block (given its `live_out`), compute `live_after[i]` = the set of
/// temps live immediately AFTER computation `i`. A temp is "live across" an
/// allocating call at index `i` iff it is in `live_after[i]` (and is not the
/// call's own result).
fn live_after_per_computation(block: &Block, live_out: &HashSet<TempId>) -> Vec<HashSet<TempId>> {
    let n = block.computations.len();
    // Seed with live-out, plus the terminator's operands (jump args / branch
    // cond / return value are live entering the terminator but may not be in
    // live-out, which holds successor block-PARAMS, not the args bound to them).
    let mut live = live_out.clone();
    for op in terminator_uses(&block.terminator) {
        live.insert(op);
    }
    let mut live_after: Vec<HashSet<TempId>> = vec![HashSet::new(); n];
    for i in (0..n).rev() {
        live_after[i] = live.clone();
        let c = &block.computations[i];
        live.remove(&c.dst());
        for op in computation_operands(c) {
            live.insert(op);
        }
    }
    live_after
}

fn computation_operands(c: &Computation) -> Vec<TempId> {
    match c {
        Computation::Const { .. } => Vec::new(),
        Computation::PrimOp { args, .. } => args.clone(),
        Computation::DirectCall { args, .. } => args.clone(),
        Computation::Call { callee, args, .. } => {
            let mut v = Vec::with_capacity(args.len() + 1);
            v.push(*callee);
            v.extend_from_slice(args);
            v
        }
        Computation::TypeCheck { value, .. } => vec![*value],
        Computation::WriteBarrier { slot, value, .. } => vec![*slot, *value],
        Computation::LoadSlot { instance, .. } => vec![*instance],
        Computation::StoreSlot { instance, value, .. } => vec![*instance, *value],
        Computation::Dispatch { args, .. } => args.clone(),
        Computation::SealedDirectCall { args, .. } => args.clone(),
    }
}

fn terminator_uses(t: &Terminator) -> Vec<TempId> {
    match t {
        Terminator::Return { value: Some(v) } => vec![*v],
        Terminator::Return { value: None } => Vec::new(),
        Terminator::If { cond, .. } => vec![*cond],
        Terminator::Jump { args, .. } => args.clone(),
    }
}

/// Validate that every call-shaped Computation's `safepoint_roots` is
/// a subset of the temps live across that call. Used by the Sprint
/// 11b verifier extension to catch programming errors in liveness
/// passes; if you populate `safepoint_roots` with a temp that isn't
/// actually live, the runtime registers a slot whose value the
/// post-call reload trashes the original temp with — silent
/// miscompilation.
pub fn verify_safepoint_roots(f: &Function) -> Result<(), Vec<SafepointError>> {
    let mut errs = Vec::new();
    let temp_types: HashMap<TempId, crate::ir::TypeEstimate> =
        f.temps.iter().map(|t| (t.id, t.type_estimate)).collect();
    let live_out = compute_global_live_out(f);

    for (block_idx, block) in f.blocks.iter().enumerate() {
        let live_after = live_after_per_computation(block, &live_out[block_idx]);
        for (c_idx, c) in block.computations.iter().enumerate() {
            let Some(roots) = c.safepoint_roots() else {
                continue;
            };
            let call_dst = c.dst();
            for r in roots {
                // Each registered root must be live across the call (live
                // immediately after it, excluding the call's own result) ...
                if !live_after[c_idx].contains(r) || *r == call_dst {
                    errs.push(SafepointError::TempNotLiveAcrossCall {
                        call_dst,
                        temp: *r,
                    });
                }
                // ... and must actually be a GC-managed type.
                let ty = temp_types.get(r).copied().unwrap_or(crate::ir::TypeEstimate::Top);
                if !ty.needs_gc_protection() {
                    errs.push(SafepointError::TempDoesNotNeedProtection {
                        call_dst,
                        temp: *r,
                    });
                }
            }
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SafepointError {
    TempNotLiveAcrossCall { call_dst: TempId, temp: TempId },
    TempDoesNotNeedProtection { call_dst: TempId, temp: TempId },
}

/// One per (function, block, computation-index, missing-arg) record.
/// The `NOD_DIAG_ARG_ROOT_COVERAGE` probe (GAP-011 hypothesis) yields
/// these for every call where a GC-typed argument is not in
/// `safepoint_roots`. If non-empty after `populate_safepoint_roots`,
/// the agent's hypothesis holds for those sites: the arg flows as a
/// register/SSA value, the callee receives it before its own first
/// safepoint, and if the callee allocates and triggers a moving GC,
/// the arg becomes stale.
#[derive(Clone, Debug)]
pub struct ArgRootCoverageGap {
    pub function: String,
    pub block: BlockId,
    pub computation_index: usize,
    pub callee_label: String,
    pub call_dst: TempId,
    pub gc_typed_arg: TempId,
    pub arg_type: crate::ir::TypeEstimate,
    pub arg_position: usize,
}

/// Scan every potentially-allocating call in `f` and report each
/// GC-typed argument that is NOT present in the call's
/// `safepoint_roots`. Returns the gap list — empty means coverage is
/// complete. Call AFTER `populate_safepoint_roots` so the roots
/// reflect the final dataflow.
///
/// GAP-011 hypothesis (agent review, 2026-05-29): liveness correctly
/// excludes args that are dead-after-call from `safepoint_roots` —
/// the value is needed only as an operand TO the call and isn't used
/// after the call returns. But if the callee is allocating and the
/// arg is a GC pointer, the caller's slab doesn't track it; the
/// value flows in a register; if a moving GC fires inside the
/// callee before its first safepoint, the register-held arg goes
/// stale. This probe enumerates those sites so we can decide whether
/// the fix is (a) extend `populate_safepoint_roots` to always
/// include GC-typed args, or (b) something callee-side.
pub fn diagnose_arg_root_coverage(f: &Function) -> Vec<ArgRootCoverageGap> {
    let temp_types: HashMap<TempId, crate::ir::TypeEstimate> =
        f.temps.iter().map(|t| (t.id, t.type_estimate)).collect();
    let mut out = Vec::new();
    for block in &f.blocks {
        for (c_idx, c) in block.computations.iter().enumerate() {
            if !c.is_potentially_allocating_call() {
                continue;
            }
            let Some(args) = c.call_args() else { continue };
            let Some(roots) = c.safepoint_roots() else { continue };
            let root_set: HashSet<TempId> = roots.iter().copied().collect();
            let callee_label = match c {
                Computation::DirectCall { callee, .. } => callee.clone(),
                Computation::Call { callee, .. } => format!("<indirect t{}>", callee.0),
                Computation::Dispatch { generic_name, .. } => format!("dispatch {generic_name}"),
                Computation::SealedDirectCall { method, generic_name, .. } => {
                    format!("sealed {method} (gf {generic_name})")
                }
                _ => "?".into(),
            };
            for (i, &arg) in args.iter().enumerate() {
                let ty = temp_types
                    .get(&arg)
                    .copied()
                    .unwrap_or(crate::ir::TypeEstimate::Top);
                if !ty.needs_gc_protection() {
                    continue;
                }
                if root_set.contains(&arg) {
                    continue;
                }
                out.push(ArgRootCoverageGap {
                    function: f.name.clone(),
                    block: block.id,
                    computation_index: c_idx,
                    callee_label: callee_label.clone(),
                    call_dst: c.dst(),
                    gc_typed_arg: arg,
                    arg_type: ty,
                    arg_position: i,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        Block, BlockId, ConstValue, Function, FunctionId, Temporary, Terminator,
        TypeEstimate,
    };
    use nod_reader::{FileId, Span};

    fn fake_span() -> Span {
        Span::new(FileId(0), 0, 0)
    }

    fn mk_temp(id: u32, ty: TypeEstimate) -> Temporary {
        Temporary {
            id: TempId(id),
            type_estimate: ty,
        }
    }

    #[test]
    fn fixnum_args_do_not_register_as_roots() {
        // fn f() -> Integer:
        //   t0 = Const 1
        //   t1 = DirectCall foo(t0)     ; safepoint_roots empty (t0 is dead)
        //   Return t1
        let f = Function {
            id: FunctionId(0),
            name: "f".into(),
            params: vec![],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".into(),
                params: vec![],
                computations: vec![
                    Computation::Const {
                        dst: TempId(0),
                        value: ConstValue::Integer(1),
                    },
                    Computation::DirectCall {
                        dst: TempId(1),
                        callee: "foo".into(),
                        args: vec![TempId(0)],
                        safepoint_roots: vec![],
                        is_no_alloc: false,
                    },
                ],
                terminator: Terminator::Return { value: Some(TempId(1)) },
            }],
            temps: vec![
                mk_temp(0, TypeEstimate::Integer),
                mk_temp(1, TypeEstimate::Integer),
            ],
            return_type: TypeEstimate::Integer,
            span: fake_span(),
        };
        let mut f = f;
        populate_safepoint_roots(&mut f);
        let c = &f.blocks[0].computations[1];
        assert_eq!(c.safepoint_roots(), Some(&[][..]));
    }

    #[test]
    fn live_pointer_across_call_gets_protected() {
        // fn f() -> Top:
        //   t0 = Const string "hello"           ; pointer-shaped
        //   t1 = DirectCall foo()               ; t0 live across
        //   t2 = DirectCall bar(t0, t1)         ; uses t0 + t1
        //   Return t2
        let mut f = Function {
            id: FunctionId(0),
            name: "f".into(),
            params: vec![],
            entry: BlockId(0),
            blocks: vec![Block {
                id: BlockId(0),
                label: "entry".into(),
                params: vec![],
                computations: vec![
                    Computation::Const {
                        dst: TempId(0),
                        value: ConstValue::String("hello".into()),
                    },
                    Computation::DirectCall {
                        dst: TempId(1),
                        callee: "foo".into(),
                        args: vec![],
                        safepoint_roots: vec![],
                        is_no_alloc: false,
                    },
                    Computation::DirectCall {
                        dst: TempId(2),
                        callee: "bar".into(),
                        args: vec![TempId(0), TempId(1)],
                        safepoint_roots: vec![],
                        is_no_alloc: false,
                    },
                ],
                terminator: Terminator::Return { value: Some(TempId(2)) },
            }],
            temps: vec![
                mk_temp(0, TypeEstimate::String),
                mk_temp(1, TypeEstimate::Top),
                mk_temp(2, TypeEstimate::Top),
            ],
            return_type: TypeEstimate::Top,
            span: fake_span(),
        };
        populate_safepoint_roots(&mut f);
        // First call: t0 alive across.
        assert_eq!(f.blocks[0].computations[1].safepoint_roots(), Some(&[TempId(0)][..]));
        // Second call: t0 and t1 alive across (t1 used in Return).
        let second = f.blocks[0].computations[2].safepoint_roots().unwrap();
        // Args of the call don't need protection AT this call (they're
        // operands), but if also used later (Return reads t2 only, so
        // t0 and t1 dead AFTER bar), they aren't live AFTER the call.
        // The Return only references t2 — so t0 and t1 are dead at end
        // of block — the second call has no roots to protect.
        assert!(
            second.is_empty(),
            "bar's args are dead after the call; no roots: {second:?}"
        );
    }
}
