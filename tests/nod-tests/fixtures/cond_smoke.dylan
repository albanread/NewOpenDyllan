Module: cond-smoke

// Sprint 49b — `cond` macro smoke fixture.
//
// Exercises the stdlib `cond` macro at every arity it supports
// today (1 to 4 test/body pairs + `otherwise`), and at each arity
// fires BOTH a matching arm AND the `otherwise` default. The
// driver harness builds + runs this and asserts the stdout exactly,
// so regressions in the macro expansion, the parser's nested-end
// handling, or the `if/elseif/else` lowering would fail the diff.
//
// Expected stdout (each line followed by '\n'):
//
//   a1[0] -> 10
//   a1[5] -> 11
//   a2[-1] -> 20
//   a2[0] -> 21
//   a2[1] -> 22
//   a3[1] -> 32
//   a3[9] -> 33
//   a4[2] -> 43
//   a4[9] -> 44

define function arity1 (x :: <integer>) => (r :: <integer>)
  cond
    (x = 0) (10)
    otherwise (11)
  end
end function;

define function arity2 (x :: <integer>) => (r :: <integer>)
  cond
    (x < 0) (20)
    (x = 0) (21)
    otherwise (22)
  end
end function;

define function arity3 (x :: <integer>) => (r :: <integer>)
  cond
    (x < 0) (30)
    (x = 0) (31)
    (x = 1) (32)
    otherwise (33)
  end
end function;

define function arity4 (x :: <integer>) => (r :: <integer>)
  cond
    (x < 0) (40)
    (x = 0) (41)
    (x = 1) (42)
    (x = 2) (43)
    otherwise (44)
  end
end function;

define function main () => ()
  format-out("a1[0] -> %d\n",  arity1(0));
  format-out("a1[5] -> %d\n",  arity1(5));
  format-out("a2[-1] -> %d\n", arity2(-1));
  format-out("a2[0] -> %d\n",  arity2(0));
  format-out("a2[1] -> %d\n",  arity2(1));
  format-out("a3[1] -> %d\n",  arity3(1));
  format-out("a3[9] -> %d\n",  arity3(9));
  format-out("a4[2] -> %d\n",  arity4(2));
  format-out("a4[9] -> %d\n",  arity4(9));
end function;
