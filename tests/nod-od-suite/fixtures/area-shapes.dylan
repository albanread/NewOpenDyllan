Module: area-shapes

// Sprint 12 / Sprint 13 single-dispatch demo. A generic `area` with
// two methods specialised on subclasses of <shape>. The result picks
// the most-specific method per receiver. area(circle) + area(square)
// = 12 (circle area approximated as r * r * 3 for integer arithmetic)
// + 25 = 37.

define class <shape> (<object>)
end class;

define class <circle> (<shape>)
  slot radius :: <integer>, init-keyword: radius:;
end class;

define class <square> (<shape>)
  slot side :: <integer>, init-keyword: side:;
end class;

define generic area (s :: <shape>) => (<integer>);

define method area (c :: <circle>) => (<integer>)
  // Integer approximation of pi * r^2 with pi ~= 3.
  radius(c) * radius(c) * 3
end method area;

define method area (s :: <square>) => (<integer>)
  side(s) * side(s)
end method area;

define function main () => (<integer>)
  let c = make(<circle>, radius: 2);
  let s = make(<square>, side: 5);
  area(c) + area(s)
end function main;
