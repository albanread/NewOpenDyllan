Module: mutual

define function g (n :: <integer>) => (<integer>)
  n * 2
end function g;

define function f (n :: <integer>) => (<integer>)
  g(n) + 1
end function f;

define function main () => (<integer>)
  f(5)
end function main;
