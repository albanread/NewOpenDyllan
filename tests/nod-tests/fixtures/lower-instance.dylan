Module: kernel-arith

// Sprint 55b — `instance?(value, <class>)` -> `TypeCheck value <label>` (dst
// <boolean>). Like slot accessors, this is id-free and pass-free: a TypeCheck
// is not a call (no safepoint) and not a Dispatch (no resolver), and the class
// label is ClassCheck::name() — the source name verbatim for most classes, with
// two builtins normalized (<string> -> <byte-string>, <vector> ->
// <simple-object-vector>). Covers verbatim builtins, both normalizations, and a
// user class (by name). The <widget> class also exercises accessor emission
// alongside (Phase 3 + Phase 4 in one module).

define class <widget> (<object>)
  slot wx :: <integer>, init-keyword: wx:;
end class;

define function is-int (v :: <object>) => (<boolean>) instance?(v, <integer>) end function;

define function is-str (v :: <object>) => (<boolean>) instance?(v, <string>) end function;

define function is-vec (v :: <object>) => (<boolean>) instance?(v, <vector>) end function;

define function is-ch (v :: <object>) => (<boolean>) instance?(v, <character>) end function;

define function is-widget (v :: <object>) => (<boolean>) instance?(v, <widget>) end function;
