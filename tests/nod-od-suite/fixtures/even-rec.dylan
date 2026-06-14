Module: even-rec

// Mutually-recursive predicates returning 1/0 for true/false. Exercises
// cross-function calls within one module and the deeper call-stack
// shape Sprint 11b's safepoints need to cope with. is-even(8) = 1.

define function is-even (n :: <integer>) => (<integer>)
  if (n = 0)
    1
  else
    is-odd(n - 1)
  end
end function is-even;

define function is-odd (n :: <integer>) => (<integer>)
  if (n = 0)
    0
  else
    is-even(n - 1)
  end
end function is-odd;

define function main () => (<integer>)
  is-even(8)
end function main;
