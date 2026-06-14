//! SSA invariant verifier for `Function`.

use std::collections::HashSet;

use crate::ir::{BlockId, Computation, Function, TempId, Terminator, TypeEstimate};

#[derive(Clone, Debug, PartialEq)]
pub enum VerifyError {
    /// A `TempId` was used as an operand before any computation defined it
    /// (and it isn't a function or block parameter).
    UseBeforeDef {
        block: BlockId,
        temp: TempId,
    },
    /// A `TempId` was defined by two different computations.
    DoubleDefine {
        temp: TempId,
    },
    /// Terminator references a `BlockId` not present in `Function::blocks`.
    DanglingBlockRef {
        from: BlockId,
        to: BlockId,
    },
    /// `Function::entry` references a missing block.
    MissingEntry {
        entry: BlockId,
    },
    /// `Block` count and unique-id count mismatch — duplicate block ids.
    DuplicateBlockId {
        block: BlockId,
    },
    /// `Return { value: Some }` in a function declared `Unit`-returning.
    ReturnValueInUnitFn,
    /// `Return { value: None }` in a function declared non-`Unit`-returning.
    MissingReturnValue,
    /// Jump arg count doesn't match the target block's parameter count.
    JumpArityMismatch {
        from: BlockId,
        to: BlockId,
        expected: usize,
        got: usize,
    },
}

pub fn verify(f: &Function) -> Result<(), Vec<VerifyError>> {
    let mut errs = Vec::new();

    let block_ids: HashSet<BlockId> = f.blocks.iter().map(|b| b.id).collect();
    if block_ids.len() != f.blocks.len() {
        let mut seen: HashSet<BlockId> = HashSet::new();
        for b in &f.blocks {
            if !seen.insert(b.id) {
                errs.push(VerifyError::DuplicateBlockId { block: b.id });
            }
        }
    }
    if !block_ids.contains(&f.entry) {
        errs.push(VerifyError::MissingEntry { entry: f.entry });
    }

    let mut defined: HashSet<TempId> = HashSet::new();
    for p in &f.params {
        if !defined.insert(*p) {
            errs.push(VerifyError::DoubleDefine { temp: *p });
        }
    }
    // Walk blocks in declaration order. We treat the entry block as
    // visited first; non-entry blocks must come later. SSA dominance is
    // not yet enforced — Sprint 06's verifier checks the weaker
    // "each temp defined exactly once before any use within the same
    // textual order" rule, which is sufficient for the kernel-subset
    // lowering's straight-line + structured-if forms.
    for b in &f.blocks {
        for bp in &b.params {
            if !defined.insert(*bp) {
                errs.push(VerifyError::DoubleDefine { temp: *bp });
            }
        }
        for c in &b.computations {
            for operand in computation_operands(c) {
                if !defined.contains(&operand) {
                    errs.push(VerifyError::UseBeforeDef {
                        block: b.id,
                        temp: operand,
                    });
                }
            }
            let dst = c.dst();
            if !defined.insert(dst) {
                errs.push(VerifyError::DoubleDefine { temp: dst });
            }
        }
        match &b.terminator {
            Terminator::Return { value } => {
                if let Some(t) = value {
                    if !defined.contains(t) {
                        errs.push(VerifyError::UseBeforeDef {
                            block: b.id,
                            temp: *t,
                        });
                    }
                    if f.return_type == TypeEstimate::Unit {
                        errs.push(VerifyError::ReturnValueInUnitFn);
                    }
                } else if f.return_type != TypeEstimate::Unit {
                    errs.push(VerifyError::MissingReturnValue);
                }
            }
            Terminator::If {
                cond,
                then_block,
                else_block,
            } => {
                if !defined.contains(cond) {
                    errs.push(VerifyError::UseBeforeDef {
                        block: b.id,
                        temp: *cond,
                    });
                }
                if !block_ids.contains(then_block) {
                    errs.push(VerifyError::DanglingBlockRef {
                        from: b.id,
                        to: *then_block,
                    });
                }
                if !block_ids.contains(else_block) {
                    errs.push(VerifyError::DanglingBlockRef {
                        from: b.id,
                        to: *else_block,
                    });
                }
            }
            Terminator::Jump { target, args } => {
                if !block_ids.contains(target) {
                    errs.push(VerifyError::DanglingBlockRef {
                        from: b.id,
                        to: *target,
                    });
                }
                for a in args {
                    if !defined.contains(a) {
                        errs.push(VerifyError::UseBeforeDef {
                            block: b.id,
                            temp: *a,
                        });
                    }
                }
                if let Some(tb) = f.block(*target)
                    && tb.params.len() != args.len()
                {
                    errs.push(VerifyError::JumpArityMismatch {
                        from: b.id,
                        to: *target,
                        expected: tb.params.len(),
                        got: args.len(),
                    });
                }
            }
        }
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
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
