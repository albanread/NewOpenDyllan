Module: factorial

define function factorial (n :: <integer>) => (<integer>)
  if (n = 0) 1 else n * factorial(n - 1) end
end function factorial;

define function main () => (<integer>)
  factorial(10)
end function main;
