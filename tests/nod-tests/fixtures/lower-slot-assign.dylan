Module: kernel-arith

// Sprint 55b — slot assignment: `slot(obj) := v` lowers to a `<slot>-setter`
// Dispatch (NOT a StoreSlot; try_resolve_slot_offset always returns None).
// Flip-only: the setter Dispatch carries a populated safepoint.

define class <counter> (<object>)
  slot counter-val :: <integer>, init-keyword: val:;
end class;

define function set-counter (c :: <counter>, n :: <integer>) => ()
  counter-val(c) := n;
end function;
