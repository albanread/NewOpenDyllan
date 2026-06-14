Module: dylan-lexer
Precedence: c

// Sprint 53.3 — reusable C3 linearisation, bundled into the sema project.
//
// This is the pure-algorithm core lifted verbatim from
// `dylan-c3-smoke.dylan` (the Sprint 51a Dylan port of
// `src/nod-sema/src/c3.rs`): `<c3-result>` + its constructors, the
// `<queue>` helper, and `c3-linearise` / `c3-merge`. The smoke file's
// print / builder / `c3-main` helpers (which pull in `format-out`) are
// NOT copied — the sema walk only needs the linearisation.
//
// `Module: dylan-lexer` (same header as every other file in
// `dylan-sema.prj`) so the project's AST-concatenation places these
// definitions in the one shared module that `dylan-sema.dylan` sees.
//
// The Sprint 51a smoke file (`dylan-c3-smoke.dylan`) is its own separate
// project (`dylan-c3-smoke.prj` → `dylan-c3-smoke.exe`, gated by
// `c3_oracle.rs`); it is untouched. Bundling a copy here cannot clash —
// separate EXEs, separate module instances.

// ─── result discrimination ────────────────────────────────────────────────
//
// Dylan-side equivalent of Rust's `Result<Vec<String>, C3Error>`. A
// successful linearisation goes in `c3-result-cpl`; on failure the
// `c3-result-kind` is one of `#"inconsistent-merge"` or
// `#"unresolved-parent"` and `c3-result-failing-class` names the
// offender. Single class keeps the dispatch surface small.

define class <c3-result> (<object>)
  slot c3-result-kind :: <symbol>,         init-keyword: kind:;
    // #"ok" | #"inconsistent-merge" | #"unresolved-parent"
  slot c3-result-cpl  :: <stretchy-vector>, init-keyword: cpl:;
    // populated on #"ok"; empty on errors
  slot c3-result-failing-class :: <byte-string>,
                                            init-keyword: failing-class:;
    // populated on errors; "" on success
end class;

define function make-ok-result (cpl :: <stretchy-vector>) => (r :: <c3-result>)
  make(<c3-result>, kind: #"ok", cpl: cpl, failing-class: "")
end function;

define function make-inconsistent-result (name :: <byte-string>)
 => (r :: <c3-result>)
  make(<c3-result>, kind: #"inconsistent-merge",
       cpl: make(<stretchy-vector>), failing-class: name)
end function;

// ─── queue helpers ────────────────────────────────────────────────────────
//
// The stdlib's `<stretchy-vector>` exposes push but no pop or size
// shrink. So a "queue" is a small class that wraps the vector plus a
// `head` index; `pop-front!` just advances `head`. The queue is
// "empty" when `head` reaches the vector's size. Operations are O(1)
// instead of O(n) — bonus over the Rust `VecDeque` shape, which has
// the same conceptual front-pointer model.

define class <queue> (<object>)
  slot queue-items :: <stretchy-vector>, init-keyword: items:;
  slot queue-head  :: <integer>,         init-keyword: head:, init-value: 0;
end class;

define function make-queue-from (items :: <stretchy-vector>) => (q :: <queue>)
  // Fresh copy so the caller's vector isn't mutated. Tiny vectors —
  // typical CPL depths are < 10.
  let copy = make(<stretchy-vector>);
  let n = size(items);
  let i = 0;
  until (i = n)
    add!(copy, items[i]);
    i := i + 1;
  end;
  make(<queue>, items: copy, head: 0)
end function;

define function queue-empty? (q :: <queue>) => (yes? :: <boolean>)
  queue-head(q) >= size(queue-items(q))
end function;

define function queue-front (q :: <queue>) => (x :: <byte-string>)
  queue-items(q)[queue-head(q)]
end function;

define function queue-pop-front! (q :: <queue>) => ()
  queue-head(q) := queue-head(q) + 1;
end function;

// Does the queue contain `x` strictly AFTER the current head?
// (Equivalent to "in the tail, with the head being index 0 of the
// logical queue".)
//
// Guard: when `head` is already at or past `size` (queue is empty or
// just emptied), `head + 1` may be > `size`, and `i = n` would never
// fire because the loop counter overshot. Use `>=` not `=`.
define function queue-has-in-tail? (q :: <queue>, x :: <byte-string>)
 => (yes? :: <boolean>)
  let items = queue-items(q);
  let n = size(items);
  let i = queue-head(q) + 1;
  let found? = #f;
  until (i >= n | found?)
    if (items[i] = x) found? := #t; end;
    i := i + 1;
  end;
  found?
end function;

// ─── the algorithm ────────────────────────────────────────────────────────
//
// `c3-linearise(name, parents, parent-cpls)`:
//   `name` — class name (for diagnostics + as the first element of
//             the resulting CPL).
//   `parents` — direct supers in declaration order, as a
//               <stretchy-vector> of <byte-string>.
//   `parent-cpls` — for each parent, that parent's CPL (assumed
//               precomputed), as a <stretchy-vector> of
//               <stretchy-vector> of <byte-string>.
// Returns a <c3-result>.

define function c3-linearise (name        :: <byte-string>,
                              parents     :: <stretchy-vector>,
                              parent-cpls :: <stretchy-vector>)
 => (r :: <c3-result>)
  if (size(parents) ~= size(parent-cpls))
    make-inconsistent-result(name)
  elseif (size(parents) = 0)
    // Bottom of the hierarchy: CPL is [self].
    let cpl = make(<stretchy-vector>);
    add!(cpl, name);
    make-ok-result(cpl)
  elseif (size(parents) = 1)
    // Single-inheritance fast path: [self, parent.cpl...].
    let cpl = make(<stretchy-vector>);
    add!(cpl, name);
    let pcpl = parent-cpls[0];
    let pn = size(pcpl);
    let i = 0;
    until (i = pn)
      add!(cpl, pcpl[i]);
      i := i + 1;
    end;
    make-ok-result(cpl)
  else
    c3-merge(name, parents, parent-cpls)
  end
end function;

// Inner merge — called only when size(parents) >= 2. Builds the input
// queues (one per parent's CPL, plus a final "parents in declaration
// order" queue) and runs the good-head-picking loop.
define function c3-merge (name        :: <byte-string>,
                          parents     :: <stretchy-vector>,
                          parent-cpls :: <stretchy-vector>)
 => (r :: <c3-result>)
  // Build the queues — fresh copies (`make-queue-from`) so we can
  // advance the head without disturbing the caller's data.
  let inputs = make(<stretchy-vector>);
  let pn = size(parent-cpls);
  let i = 0;
  until (i = pn)
    add!(inputs, make-queue-from(parent-cpls[i]));
    i := i + 1;
  end;
  // Final queue: parents themselves, in declaration order.
  add!(inputs, make-queue-from(parents));

  // Result CPL starts with the class itself.
  let result = make(<stretchy-vector>);
  add!(result, name);

  // Pick-and-pop loop. Bail on inconsistency.
  let failed? = #f;
  let any-nonempty? = #t;
  let in-n = size(inputs);
  until (~ any-nonempty? | failed?)
    // Recompute any-nonempty? for the loop condition.
    any-nonempty? := #f;
    let q-i = 0;
    until (q-i = in-n | any-nonempty?)
      if (~ queue-empty?(inputs[q-i])) any-nonempty? := #t; end;
      q-i := q-i + 1;
    end;
    if (any-nonempty?)
      // Find a good head — first front that's not in any other queue's
      // tail.
      let picked = #f;
      let pick-i = 0;
      until (pick-i = in-n | picked)
        let q = inputs[pick-i];
        if (~ queue-empty?(q))
          let candidate = queue-front(q);
          // Bad if some other queue has candidate at index >= 1.
          let bad? = #f;
          let other-i = 0;
          until (other-i = in-n | bad?)
            if (queue-has-in-tail?(inputs[other-i], candidate))
              bad? := #t;
            end;
            other-i := other-i + 1;
          end;
          if (~ bad?) picked := candidate; end;
        end;
        pick-i := pick-i + 1;
      end;
      if (picked)
        // Pop `picked` from the front of every queue where it leads.
        // NESTED ifs so we don't call queue-front on an empty queue —
        // Dylan's `&` is currently eager (see the stdlib short-circuit
        // task), so a single combined condition would dereference the
        // queue's head past its size.
        let pop-i = 0;
        until (pop-i = in-n)
          let q = inputs[pop-i];
          if (~ queue-empty?(q))
            if (queue-front(q) = picked)
              queue-pop-front!(q);
            end;
          end;
          pop-i := pop-i + 1;
        end;
        add!(result, picked);
      else
        failed? := #t;
      end;
    end;
  end;

  if (failed?)
    make-inconsistent-result(name)
  else
    make-ok-result(result)
  end
end function;
