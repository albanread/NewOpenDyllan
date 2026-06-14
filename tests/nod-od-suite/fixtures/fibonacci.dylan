Module: fibonacci

// Classic recursive Fibonacci. Exercises straight recursion, branching,
// and i64 arithmetic. fib(10) = 55.

define function fib (n :: <integer>) => (<integer>)
  if (n < 2)
    n
  else
    fib(n - 1) + fib(n - 2)
  end
end function fib;

define function main () => (<integer>)
  fib(10)
end function main;
