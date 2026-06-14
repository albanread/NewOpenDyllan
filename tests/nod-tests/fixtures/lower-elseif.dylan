Module: lower-elseif

// Sprint 56 — `if / elseif / else` desugars to NESTED ifs (mirrors the Rust
// lowering). Two shapes: a plain multi-arm chain (value-producing arms), and
// an arm-assigned var threaded through BOTH nested joins (the merge-set
// nesting test).

define function classify (x :: <integer>) => (r :: <integer>)
  if (x = 0)
    10
  elseif (x = 1)
    20
  elseif (x = 2)
    30
  else
    40
  end
end function;

define function pick (x :: <integer>) => (r :: <integer>)
  let acc = 0;
  if (x = 0)
    acc := 11
  elseif (x = 1)
    acc := 22
  else
    acc := 33
  end;
  acc
end function;
