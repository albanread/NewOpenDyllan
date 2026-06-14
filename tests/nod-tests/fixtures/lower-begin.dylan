Module: kernel-arith

// Sprint 56 (axis-1) — `begin … end` transparent body-sequence lowering.

// (1) Value body: last statement is a value -> no extra const, returns x+1.
define function bv (n :: <integer>) => (r :: <integer>)
  begin let x = 5; x + 1 end
end function;

// (2) Non-tail begin whose last statement is a VOID while loop (a `sum`
// follows): the begin materialises the void value as a `<unit>` const at
// loop_exit, then the function continues.
define function bvoid (n :: <integer>) => (s :: <integer>)
  let sum = 0;
  begin let i = 1; while (i <= n) sum := sum + i; i := i + 1 end end;
  sum
end function;

// (3) Interior void loop followed by a value: the interior loop also emits a
// `<unit>` const (unit_temp per loop), then the value continues.
define function binterior (n :: <integer>) => (r :: <integer>)
  let acc = 0;
  begin
    let i = 1;
    while (i <= n) acc := acc + i; i := i + 1 end;
    acc + 100
  end
end function;
