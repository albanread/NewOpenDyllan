Module: gc-alloc-loop

// GC allocation stress test.  Allocates 1000 <box> objects inside a
// while loop, reading a slot from each before letting it go.  The
// accumulated sum must equal 1+2+...+1000 = 500500.  Any GC
// corruption (stale pointer, wrong slot value after collection) will
// show up as a wrong return value.

define class <box> (<object>)
  slot value :: <integer>, init-keyword: value:;
end class;

define function main () => (<integer>)
  let i   = 1;
  let sum = 0;
  while (i <= 1000)
    let b = make(<box>, value: i);
    sum := sum + value(b);
    i := i + 1
  end;
  sum
end function main;
