Module: gc-accum

// Reproducers for GAP-011 #2 — GC staleness of heap LOCALS across block
// boundaries that lowering did not thread through a block param.
//
//   build-simple / build-nested : loop-carried accumulators. Lowering
//       threads these through the loop-header phi (DFM-confirmed), so they
//       WORK — included as the negative controls.
//
//   pick : a heap local (`tag`) live ACROSS an if/elseif join whose arms
//       allocate. This matches defn-return-estimate's shape, where the
//       merge-divergence detector flags real non-param-local sites. If
//       `tag` is not a join block param, a GC in an arm leaves it stale.

define function build-simple (n :: <integer>) => (s :: <byte-string>)
  let out = "";
  let i = 0;
  until (i = n)
    out := concatenate(out, "x");
    i := i + 1;
  end;
  out
end function;

define function build-nested (n :: <integer>) => (s :: <byte-string>)
  let out = "";
  let i = 0;
  until (i = n)
    out := concatenate(out, concatenate("item", "\n"));
    i := i + 1;
  end;
  out
end function;

define function pick (base :: <byte-string>) => (s :: <byte-string>)
  let tag = concatenate(base, "-");          // heap local, allocated
  let body = if (size(base) = 0)
               concatenate("empty", "!")     // arm allocates  -> may GC
             elseif (size(base) = 1)
               concatenate("one", "!")
             else
               concatenate("many", "!")
             end;
  // `tag` is live across the if-join here:
  concatenate(tag, body)
end function;

define function drive (n :: <integer>) => (k :: <integer>)
  let total = 0;
  let i = 0;
  until (i = n)
    let r = pick("xy");
    total := total + size(r);
    i := i + 1;
  end;
  total
end function;

define function main () => (<integer>)
  let a = build-simple(4000);
  format-out("simple size=%d\n", size(a));
  let b = build-nested(4000);
  format-out("nested size=%d\n", size(b));
  let t = drive(40000);
  format-out("drive total=%d\n", t);
  size(a) + size(b) + t
end function;
