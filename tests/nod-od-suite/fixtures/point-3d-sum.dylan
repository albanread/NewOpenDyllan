Module: point-3d-sum

// Class hierarchy with inherited slots. A <point-3d> instance reads
// its own `z` slot and the `x`/`y` slots inherited from <point-2d>.
// Exercises CPL walks, slot offsets across inheritance, and the
// auto-generated getters. (1 + 2) + 3 = 6.

define class <point-2d> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;

define class <point-3d> (<point-2d>)
  slot z :: <integer>, init-keyword: z:;
end class;

define function sum-coords (p :: <point-3d>) => (<integer>)
  x(p) + y(p) + z(p)
end function sum-coords;

define function main () => (<integer>)
  sum-coords(make(<point-3d>, x: 1, y: 2, z: 3))
end function main;
