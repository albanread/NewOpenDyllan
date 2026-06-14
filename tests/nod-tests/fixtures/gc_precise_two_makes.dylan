Module: gc-precise-two-makes

define class <gcp-pt> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;

define function main () => (<integer>)
  let a = make(<gcp-pt>, x: 1, y: 2);
  let b = make(<gcp-pt>, x: 3, y: 4);
  x(a) + x(b)
end function main;
