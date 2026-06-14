Module: kernel-arith

// Sprint 55a — short-circuit `|` / `&` coverage for the lowering gate,
// including the env-merge (RHS-assignment threading + nested short-circuit).

define function sc-or (a :: <integer>, b :: <integer>) => (r :: <integer>)
  a | b
end function;

define function sc-and (a :: <integer>, b :: <integer>) => (r :: <integer>)
  a & b
end function;

define function sc-in-if (a :: <integer>, b :: <integer>, c :: <integer>) => (r :: <integer>)
  if (a | b) c + 1 else c - 1 end
end function;

define function sc-rhs-assign (a :: <integer>) => (r :: <integer>)
  let x = 0;
  let q = a | (x := 5);
  q + x
end function;

define function sc-nested (a :: <integer>, b :: <integer>, c :: <integer>) => (r :: <integer>)
  a | (b & c)
end function;
