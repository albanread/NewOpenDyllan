Module: richards-shape

define sealed class <task> (<object>) end class;

define sealed class <idler> (<task>)
  slot id-state :: <integer>, init-keyword: id-state:;
end class;

define sealed class <worker> (<task>)
  slot wk-state :: <integer>, init-keyword: wk-state:;
end class;

define sealed class <handler> (<task>)
  slot h-state :: <integer>, init-keyword: h-state:;
end class;

define sealed class <device> (<task>)
  slot d-state :: <integer>, init-keyword: d-state:;
end class;

define sealed generic run-task (t :: <task>, packet :: <integer>) => (<integer>);

define method run-task (t :: <idler>, packet :: <integer>) => (<integer>)
  id-state(t) + packet
end method;

define method run-task (t :: <worker>, packet :: <integer>) => (<integer>)
  wk-state(t) * 2 + packet
end method;

define method run-task (t :: <handler>, packet :: <integer>) => (<integer>)
  // Parens required: Dylan binary operators are flat / left-associative
  // (no precedence), so `h-state(t) + packet * 3` would be
  // `(h-state(t) + packet) * 3`. The intended value is `h-state + 3*packet`.
  h-state(t) + (packet * 3)
end method;

define method run-task (t :: <device>, packet :: <integer>) => (<integer>)
  d-state(t) + packet - 1
end method;

define function visit-list (tasks :: <pair>, packet :: <integer>) => (<integer>)
  let head-task = head(tasks);
  let acc = run-task(head-task, packet);
  let rest = tail(tasks);
  if (empty?(rest))
    acc
  else
    acc + visit-list(rest, packet + 1)
  end
end function;

define function step (idler :: <idler>, worker :: <worker>, handler :: <handler>, device :: <device>, p :: <integer>) => (<integer>)
  run-task(idler, p) +
  run-task(worker, p + 1) +
  run-task(handler, p + 2) +
  run-task(device, p + 3) +
  run-task(idler, p + 4) +
  run-task(worker, p + 5) +
  run-task(handler, p + 6) +
  run-task(device, p + 7) +
  run-task(idler, p + 8) +
  run-task(worker, p + 9) +
  run-task(handler, p + 10) +
  run-task(device, p + 11) +
  run-task(idler, p + 12) +
  run-task(worker, p + 13) +
  run-task(handler, p + 14) +
  run-task(device, p + 15)
end function;

define function inner-loop (k :: <integer>, idler :: <idler>, worker :: <worker>, handler :: <handler>, device :: <device>, p :: <integer>, acc :: <integer>) => (<integer>)
  if (k = 0)
    acc
  else
    inner-loop(k - 1, idler, worker, handler, device, p, acc + step(idler, worker, handler, device, p))
  end
end function;

define function outer-loop (n :: <integer>, idler :: <idler>, worker :: <worker>, handler :: <handler>, device :: <device>, acc :: <integer>) => (<integer>)
  if (n = 0)
    acc
  else
    outer-loop(n - 1, idler, worker, handler, device, inner-loop(2000, idler, worker, handler, device, n, acc))
  end
end function;

define function main () => (<integer>)
  let idler-task = make(<idler>, id-state: 1);
  let worker-task = make(<worker>, wk-state: 2);
  let handler-task = make(<handler>, h-state: 3);
  let device-task = make(<device>, d-state: 4);
  let task-list =
    pair(idler-task,
         pair(worker-task,
              pair(handler-task,
                   pair(device-task,
                        nil()))));
  let list-check = visit-list(task-list, 0);
  let main-result = outer-loop(500, idler-task, worker-task, handler-task, device-task, 0);
  main-result + list-check
end function;
