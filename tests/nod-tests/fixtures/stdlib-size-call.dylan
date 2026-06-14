Module: __test__

// Sprint 20b spot-check fixture — calls into the stdlib's `size`
// generic. The `dump-llvm` output should show a call through the
// dispatch path (or a resolved direct call to the stdlib method's
// body symbol — both are evidence the stdlib path is active).

define function test-main () => (n :: <integer>)
  size(#(10, 20, 30))
end function;
