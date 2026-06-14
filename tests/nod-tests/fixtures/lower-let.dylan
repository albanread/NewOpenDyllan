Module: kernel-arith

// Sprint 55a — straight-line `let` coverage for the Dylan AST->DFM lowering
// gate (dylan_lower_phase0_dump_dfm_byte_match). No corpus fixture exercises
// `let` without also using control flow / classes / primitives, so this is a
// dedicated minimal case: chained `let` bindings + arithmetic, lowering to a
// single straight-line block. A non-captured `let` is just a name->value-temp
// binding (no extra computation), so y/z reuse the binop/const temps directly.

define function lower-let-chain (x :: <integer>) => (r :: <integer>)
  let y = x * 2;
  let z = y + 1;
  z + x
end function;
