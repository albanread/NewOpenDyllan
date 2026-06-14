Module: kernel-arith

define function classify (x :: <integer>) => (r :: <integer>)
  let base = x * 10;
  if (x > 0)
    base + 1
  else
    base - 1
  end
end function;
