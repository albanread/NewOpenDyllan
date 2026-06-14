Module: gap-007-repro

// GAP-007 minimal reproducer (see docs/COMPILER_GAPS.md).
//
// Holds two heap-shaped `let`-locals across a heavy allocation loop:
//
//   * `vec` — a `<stretchy-vector>` accumulator, allocated once at
//     the top of `dump`.
//   * `acc` — a `<stretchy-vector>` mirror that the loop body grows
//     alongside `vec`, so every iteration both reads `vec` AND writes
//     `acc`. That's the loop-carried-phi shape the bug bites.
//
// Before the codegen fix landed, both locals turned to garbage after
// ~92 iterations because `pending_incoming` recorded TempIds and the
// end-of-function phi-wiring re-resolved them through `state.temps`
// AFTER `end_safepoint` had clobbered each entry, leaving the loop
// header's phi referencing a body-block-local `gc.reload` SSA on
// both edges.
//
// `main` allocates 200 `<tok>`s into `vec`, then calls `dump(vec)`
// which walks them while growing `acc`. Returns 42 on success.

define class <tok> (<object>)
  slot tok-tag :: <integer>, init-keyword: tag:;
end class;

// Walks `vec` while growing a second `<stretchy-vector>` (`acc`).
// Both `vec` and `acc` are loop-carried `let`-locals — exactly the
// shape that breaks pre-fix. Returns the size of `acc` at the end.
define function dump (vec :: <stretchy-vector>) => (m :: <integer>)
  let acc = %make-stretchy-vector(4);
  let n = %stretchy-vector-size(vec);
  let i = 0;
  until (i = n)
    let t = %stretchy-vector-element(vec, i);
    %stretchy-vector-push(acc, t);
    i := i + 1;
  end;
  %stretchy-vector-size(acc)
end function;

define function main () => (n :: <integer>)
  // 1000 iterations comfortably exceeds the ~92-iter trip point from
  // the GAP-entry's narrative AND the ~650-line lexer envelope. Stays
  // well under any test-timeout budget.
  let v = %make-stretchy-vector(4);
  let i = 0;
  until (i = 1000)
    %stretchy-vector-push(v, make(<tok>, tag: i));
    i := i + 1;
  end;
  let m = dump(v);
  if (m = 1000)
    42
  else
    m
  end if
end function;
