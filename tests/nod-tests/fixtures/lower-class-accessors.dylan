Module: kernel-arith

// Sprint 55b — slot-accessor emission (Phase 3) for the Dylan AST->DFM
// lowering gate. Each `define class` synthesizes a getter (and, since none of
// these slots are `constant`, a setter) per own slot, emitted BEFORE all user
// functions and byte-identical to the Rust oracle (LoadSlot/StoreSlot @offset
// [SlotTypeKind]). Covers:
//   * offset progression (own slot i -> @ 8 + 8i),
//   * every SlotTypeKind label: <integer>/<character> -> [Integer], others ->
//     [Object],
//   * getter return types (slot_type_to_estimate): <integer>, <string> (from
//     <byte-string>), <boolean>, <character>, <top> (from <object>),
//   * multiple classes (accessors in class source order), and
//   * a trailing straight-line function (Phase 4) that does NOT touch the
//     classes — proving all accessors precede all user functions in the dump.

define class <coord> (<object>)
  slot px :: <integer>, init-keyword: px:;
  slot py :: <integer>, init-keyword: py:;
end class;

define class <tag> (<object>)
  slot tg-text :: <byte-string>, init-keyword: text:;
  slot tg-flag :: <boolean>, init-keyword: flag:;
  slot tg-glyph :: <character>, init-keyword: glyph:;
  slot tg-misc :: <object>, init-keyword: misc:;
end class;

define function bump (a :: <integer>) => (<integer>)
  a + 1
end function;
