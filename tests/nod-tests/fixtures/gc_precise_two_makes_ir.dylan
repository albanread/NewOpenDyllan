Module: gc-precise-two-makes-ir

define class <gcp-pt-ir> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;

define function main () => (<integer>)
  let a = make(<gcp-pt-ir>, x: 1, y: 2);
  let b = make(<gcp-pt-ir>, x: 3, y: 4);
  x(a) + x(b)
end function main;
