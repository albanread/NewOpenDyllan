Module: euclid-gcd

// Euclid's algorithm for GCD. Exercises `mod`, recursion, and the
// non-trivial-base-case branch. gcd(48, 18) = 6.

define function gcd (a :: <integer>, b :: <integer>) => (<integer>)
  if (b = 0)
    a
  else
    gcd(b, a mod b)
  end
end function gcd;

define function main () => (<integer>)
  gcd(48, 18)
end function main;
