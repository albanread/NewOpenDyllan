Module: gap011-repro

// GAP-011 minimal repro. Mimics dump-node/acc-string: recursive
// function whose accumulator is a stretchy-vector, each level
// pushes many bytes. If the stale-root bug is in the spill/reload
// around `add!`, this will crash with
// `stretchy_vector_push: not a <stretchy-vector>`.

define function flood (v :: <stretchy-vector>) => ()
  let i = 0;
  while (i < 64)
    add!(v, i);
    i := i + 1;
  end while;
end function flood;

define function recurse (v :: <stretchy-vector>, depth :: <integer>) => ()
  if (depth > 0)
    flood(v);
    recurse(v, depth - 1);
    flood(v);
  end if;
end function recurse;

define function main () => (<integer>)
  let v = make(<stretchy-vector>);
  recurse(v, 1000);
  size(v)
end function main;
