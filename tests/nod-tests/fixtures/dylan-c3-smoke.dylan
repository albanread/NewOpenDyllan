Module: dylan-c3-smoke

// Sprint 51a — Dylan-side C3 linearisation.
//
// First port of a piece of sema (`src/nod-sema/src/c3.rs`) into Dylan.
// C3 is a self-contained algorithm: given a class name, its direct
// superclasses, and the CPLs of those superclasses, produce the class's
// own CPL — the linearised method resolution order. No state, no
// dependencies on other sema passes; pure function.
//
// Match the Rust reference's behaviour byte-for-byte. The Rust tests
// in `c3.rs` assert specific outputs for canonical input shapes
// (single-inheritance chains, the classic diamond, three-parent MI,
// inconsistent-merge cycle detection). This smoke runs those same
// shapes and prints each result; the integration test
// `tests/nod-tests/tests/c3_oracle.rs` asserts the stdout matches.
//
// The algorithm itself follows Python's MRO formulation we mirrored in
// Rust: repeatedly pick a "good head" — an element that appears at
// the front of at least one input list AND in no tail of any other —
// from the per-parent CPL queues plus a final "parents in declaration
// order" queue. Fail when no good head can be found (inconsistent MI).

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

// ─── smoke ────────────────────────────────────────────────────────────────
//
// Run the same canonical shapes the Rust tests in `c3.rs` assert.
// Print each linearisation (or the error tag) one per line. The
// integration test `tests/nod-tests/tests/c3_oracle.rs` asserts the
// stdout matches the values pre-computed (by hand and cross-checked
// against Python's MRO + Dylan's `dispatch.dylan` reference).

define function print-cpl (label :: <byte-string>, r :: <c3-result>) => ()
  if (c3-result-kind(r) = #"ok")
    let cpl = c3-result-cpl(r);
    let n = size(cpl);
    format-out("%s:", label);
    let i = 0;
    until (i = n)
      format-out(" %s", cpl[i]);
      i := i + 1;
    end;
    format-out("\n");
  else
    format-out("%s: ERROR %s for %s\n",
               label,
               // No symbol-to-string in stdlib yet — hardcode the two
               // error tags we actually produce.
               if-string(c3-result-kind(r), #"inconsistent-merge",
                         "inconsistent-merge", "unresolved-parent"),
               c3-result-failing-class(r));
  end;
end function;

// Tiny inline if-as-byte-string helper. Statement-form to dodge the
// GAP-011-family heap-typed-if-expression issue.
define function if-string (kind :: <symbol>, expected :: <symbol>,
                           yes :: <byte-string>, no :: <byte-string>)
 => (s :: <byte-string>)
  let result = no;
  if (kind = expected) result := yes; end;
  result
end function;

// Builders for stretchy vectors of strings — Dylan doesn't have
// vector literals, so a tiny helper makes the smoke readable.
define function strs (items :: <stretchy-vector>) => (v :: <stretchy-vector>)
  items
end function;

define function s0 () => (v :: <stretchy-vector>)
  make(<stretchy-vector>)
end function;

define function s1 (a :: <byte-string>) => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  v
end function;

define function s2 (a :: <byte-string>, b :: <byte-string>)
 => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  add!(v, b);
  v
end function;

define function s3 (a :: <byte-string>, b :: <byte-string>,
                    c :: <byte-string>) => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  add!(v, b);
  add!(v, c);
  v
end function;

define function s4 (a :: <byte-string>, b :: <byte-string>,
                    c :: <byte-string>, d :: <byte-string>)
 => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  add!(v, b);
  add!(v, c);
  add!(v, d);
  v
end function;

define function pcpls (a :: <stretchy-vector>) => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  v
end function;

define function pcpls2 (a :: <stretchy-vector>, b :: <stretchy-vector>)
 => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  add!(v, b);
  v
end function;

define function c3-main () => ()
  // T1 — empty class.
  let r1 = c3-linearise("<x>", s0(), s0());
  print-cpl("T1-empty", r1);

  // T2 — SI two-deep: <object> -> <a> -> <b>.
  let r-obj = c3-linearise("<object>", s0(), s0());
  let cpl-obj = c3-result-cpl(r-obj);
  let r-a = c3-linearise("<a>", s1("<object>"), pcpls(cpl-obj));
  let cpl-a = c3-result-cpl(r-a);
  let r-b = c3-linearise("<b>", s1("<a>"), pcpls(cpl-a));
  print-cpl("T2-si2", r-b);

  // T3 — SI four-deep: <object> -> <a> -> <b> -> <c> -> <d>.
  let cpl-b = c3-result-cpl(r-b);
  let r-c3 = c3-linearise("<c>", s1("<b>"), pcpls(cpl-b));
  let cpl-c3 = c3-result-cpl(r-c3);
  let r-d3 = c3-linearise("<d>", s1("<c>"), pcpls(cpl-c3));
  print-cpl("T3-si4", r-d3);

  // T4 — diamond:
  //   <a>  ->  <b>
  //          \
  //           <e>
  //          /
  //   <a>  ->  <c>
  // Both <b> and <c> share <a> as a direct super.
  let r-a4 = c3-linearise("<a>", s0(), s0());
  let cpl-a4 = c3-result-cpl(r-a4);
  let r-b4 = c3-linearise("<b>", s1("<a>"), pcpls(cpl-a4));
  let cpl-b4 = c3-result-cpl(r-b4);
  let r-c4 = c3-linearise("<c>", s1("<a>"), pcpls(cpl-a4));
  let cpl-c4 = c3-result-cpl(r-c4);
  let r-e4 = c3-linearise("<e>", s2("<b>", "<c>"),
                          pcpls2(cpl-b4, cpl-c4));
  print-cpl("T4-diamond", r-e4);

  // T5 — MI with shared grandparent (X → A, X → B, then C(A, B)).
  let r-x5 = c3-linearise("<x>", s0(), s0());
  let cpl-x5 = c3-result-cpl(r-x5);
  let r-a5 = c3-linearise("<a>", s1("<x>"), pcpls(cpl-x5));
  let r-b5 = c3-linearise("<b>", s1("<x>"), pcpls(cpl-x5));
  let r-c5 = c3-linearise("<c>", s2("<a>", "<b>"),
                          pcpls2(c3-result-cpl(r-a5),
                                 c3-result-cpl(r-b5)));
  print-cpl("T5-mi-shared", r-c5);

  // T6 — inconsistent merge: parents with conflicting tail order.
  // p1 CPL: [<p1>, <x>, <y>];  p2 CPL: [<p2>, <y>, <x>]
  let r6 = c3-linearise("<child>",
                        s2("<p1>", "<p2>"),
                        pcpls2(s3("<p1>", "<x>", "<y>"),
                               s3("<p2>", "<y>", "<x>")));
  print-cpl("T6-cycle", r6);
end function;
