Module: kernel-arith

// Sprint 55a — if env-merge: assigned-var threading through the join, and the
// join block created AFTER both arms (so nested-control-flow arms order right).

define function pick-asg (x :: <integer>) => (r :: <integer>)
  let y = 0;
  if (x > 0) y := 1 else y := 2 end;
  y
end function;

define function pick-nest (a :: <integer>, b :: <integer>) => (r :: <integer>)
  if (a > 0)
    if (b > 0) 1 else 2 end
  else
    3
  end
end function;
