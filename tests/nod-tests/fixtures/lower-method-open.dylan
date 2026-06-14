Module: lower-method-open

// Sprint 55b — `define method` + `define generic` lowering (OPEN/UNSEALED).
// A generic with two methods specialised on distinct user classes, plus a
// function that dispatches to it. The Dylan lowering names each method body
// `run-task$<class>_<integer>` (specialisers by class name); the host
// reconstruction resolves the `$<class>` suffix to the numeric ClassId scheme.
// Flip-only (dispatch resolution + safepoints are post-pass effects).

define open class <idler> (<object>)
  slot id-state :: <integer>, init-keyword: id-state:;
end class;

define open class <worker> (<object>)
  slot wk-state :: <integer>, init-keyword: wk-state:;
end class;

define open generic run-task (t :: <idler>, packet :: <integer>) => (<integer>);

define method run-task (t :: <idler>, packet :: <integer>) => (<integer>)
  id-state(t) + packet
end method;

define method run-task (t :: <worker>, packet :: <integer>) => (<integer>)
  wk-state(t) * 2 + packet
end method;

define function drive (a :: <idler>, b :: <worker>, p :: <integer>) => (<integer>)
  run-task(a, p) + run-task(b, p)
end function;
