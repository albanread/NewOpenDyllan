Module: kernel-arith

// Sprint 55a — while/until loops + `:=` coverage for the lowering gate.
// (No corpus fixture uses loops without also touching %-primitives / make.)

define function fac (n :: <integer>) => (r :: <integer>)
  let result = 1;
  let i = 1;
  until (i > n)
    result := result * i;
    i := i + 1;
  end;
  result
end function;

define function sumto (n :: <integer>) => (s :: <integer>)
  let total = 0;
  let i = 1;
  while (i <= n)
    total := total + i;
    i := i + 1;
  end;
  total
end function;

define function bump (n :: <integer>) => (r :: <integer>)
  let x = n;
  x := x + 1;
  x
end function;
