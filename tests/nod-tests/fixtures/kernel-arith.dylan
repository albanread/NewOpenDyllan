Module: kernel-arith

define constant *answer* = 42;

define function sq (x :: <integer>) => (<integer>)
  x * x
end function sq;

define function abs (x :: <integer>) => (<integer>)
  if (x < 0) -x else x end
end function abs;

define function hypot-sq (x :: <integer>, y :: <integer>) => (<integer>)
  sq(x) + sq(y)
end function hypot-sq;
