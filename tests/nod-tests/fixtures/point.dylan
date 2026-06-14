Module: point

// Sprint 34: renamed from <point> to <user-point> because `<point>` is
// now a seed `<c-struct>` registered at process boot (see
// `nod-runtime/src/structs.rs`). This fixture exercises user-class
// `define class` lowering — no relation to the Sprint 34 struct.
define class <user-point> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;

define function distance-squared (p :: <user-point>) => (<integer>)
  let xx = x(p);
  let yy = y(p);
  // Dylan has no operator precedence — all binary operators are flat and
  // left-associative — so the grouping MUST be explicit. Without the
  // parens, `xx * xx + yy * yy` is `(((xx * xx) + yy) * yy)`, not the sum
  // of squares. (This fixture silently relied on C-style precedence until
  // Sprint 51e fixed the parser; the parens make it correct Dylan.)
  (xx * xx) + (yy * yy)
end function distance-squared;

define function main () => (<integer>)
  distance-squared(make(<user-point>, x: 3, y: 4))
end function main;
