Module: gap-007-repro-ir

// GAP-007 IR-shape fixture. Mirror of `gap-007-repro.dylan` with a
// distinct class name (`<tok-ir>`). The runtime class registry is
// process-global and refuses redefinition, so the IR-dump and JIT-run
// tests use separate fixtures (same pattern as
// `gc_precise_two_makes.dylan` vs `gc_precise_two_makes_ir.dylan`).
//
// We only need the loop-carried phi shape here — exact iteration
// count doesn't matter for the IR assertions, so this one stays
// short.

define class <tok-ir> (<object>)
  slot tok-ir-tag :: <integer>, init-keyword: tag:;
end class;

define function dump-ir (vec :: <stretchy-vector>) => (m :: <integer>)
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
  let v = %make-stretchy-vector(4);
  let i = 0;
  until (i = 4)
    %stretchy-vector-push(v, make(<tok-ir>, tag: i));
    i := i + 1;
  end;
  dump-ir(v)
end function;
