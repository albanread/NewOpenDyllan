Module: translate-class

// Sprint 51e — a minimal `define class` + accessor designed to FULLY
// translate via `--parse-with-dylan`, so the translation gate actually
// validates the DefineClass / SlotSpec reconstruction (point.dylan
// can't: its body has nested binary ops that hit the precedence fork).
// Exercises: a superclass, an `init-keyword:` slot, a
// `required-init-keyword:` slot, a typed param, a bare-type return, and
// a single-call body (no nested binops, no macros, no loops).

define class <pt> (<object>)
  slot px :: <integer>, init-keyword: px:;
  slot py :: <integer>, required-init-keyword: py:;
end class;

define function get-x (p :: <pt>) => (<integer>)
  px(p)
end function;
