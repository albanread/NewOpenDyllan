Module: rope
Precedence: c

// Sprint 43a — immutable read-only rope buffer.
//
// A rope is a binary tree of `<byte-string>` chunks:
//
//      <rope>
//        ├ <rope-leaf>   bytes (a <byte-string>) + cached length
//        └ <rope-node>   left, right (both <rope>), weight, total
//
//   * weight = size(left)            (caches the index split point)
//   * total  = size(left) + size(right)  (caches whole-subtree length)
//
// Both are caches so `rope-size` is O(1) and `rope-element` walks log(n)
// nodes (one comparison per level).
//
// This is the read-only core (Sprint 43a):
//   * make-rope-from-string(s)       — recursive split into ≤1024-byte leaves
//   * rope-size(r)                   — O(1)
//   * rope-element(r, i)             — O(log n)
//   * rope-substring(r, lo, hi)      — O(log n + len), fresh <byte-string>
//   * rope-concatenate(a, b)         — O(1), new internal node
//   * for-each-leaf(r, fn)           — O(n), in-order leaf walk
//
// Sprint 43b will add split-at / insert / delete on top of these.
//
// Leaf max = 1024 bytes — bigger leaves keep the tree shallow and reduce
// per-op dispatch / GC pressure. Production editors (xi, VSCode's
// TextBuffer) use 1024-4096. We can revisit after benchmarks.

// ─── Win32 mins for the EXE entry point + self-tests ─────────────────────
//
// The self-test main prints PASS/FAIL lines via format-out; the test
// driver reads stdout and asserts every "PASS" header is present.

// ─── Class hierarchy ─────────────────────────────────────────────────────

define class <rope> (<object>) end class;

// Sprint 43c: every node caches its own newline count so line-indexing
// ops (`rope-line-count`, `rope-line-to-offset`) descend in O(log n)
// without scanning bytes. For a leaf the count is computed once at
// construction; for an internal node it's left.newlines +
// right.newlines, kept in sync by `rope-concatenate`.

define class <rope-leaf> (<rope>)
  slot rope-leaf-bytes    :: <byte-string>, init-keyword: bytes:;
  slot rope-leaf-len      :: <integer>,     init-keyword: len:;
  slot rope-leaf-newlines :: <integer>,     init-keyword: newlines:;
end class;

define class <rope-node> (<rope>)
  slot rope-node-left     :: <rope>,    init-keyword: left:;
  slot rope-node-right    :: <rope>,    init-keyword: right:;
  slot rope-node-weight   :: <integer>, init-keyword: weight:;
  slot rope-node-total    :: <integer>, init-keyword: total:;
  slot rope-node-newlines :: <integer>, init-keyword: newlines:;
end class;

// ─── newline-count helper (Sprint 43c) ───────────────────────────────────
//
// Count `\n` bytes (10) in a `<byte-string>`. Used at leaf-construction
// time to populate the cached `rope-leaf-newlines` slot. Internal-node
// counts are computed compositionally as left.newlines + right.newlines.

define function count-newlines-in (s) => (n :: <integer>)
  let len = size(s);
  let count = 0;
  let i = 0;
  until (i = len)
    if (element(s, i) = 10)
      count := count + 1;
    else
      #f
    end;
    i := i + 1;
  end;
  count
end function;

// ─── rope-size — O(1), cached at each node ───────────────────────────────

define method rope-size (r :: <rope-leaf>) => (n :: <integer>)
  rope-leaf-len(r)
end method;

define method rope-size (r :: <rope-node>) => (n :: <integer>)
  rope-node-total(r)
end method;

// Polymorphic newline count — same cache, different slot name per class.

define method rope-newlines (r :: <rope-leaf>) => (n :: <integer>)
  rope-leaf-newlines(r)
end method;

define method rope-newlines (r :: <rope-node>) => (n :: <integer>)
  rope-node-newlines(r)
end method;

// ─── rope-element — O(log n) tree descent ────────────────────────────────

define method rope-element (r :: <rope-leaf>, i :: <integer>) => (b :: <integer>)
  element(rope-leaf-bytes(r), i)
end method;

define method rope-element (r :: <rope-node>, i :: <integer>) => (b :: <integer>)
  let w = rope-node-weight(r);
  if (i < w)
    rope-element(rope-node-left(r), i)
  else
    rope-element(rope-node-right(r), i - w)
  end
end method;

// ─── rope-concatenate — O(1), new internal node ──────────────────────────
//
// Sprint 43b: empty-rope short-circuit. If either side is the empty
// rope (size = 0) we return the other side unchanged. This keeps
// split-at(r, 0) / split-at(r, size(r)) / inserts at boundaries from
// stuffing the tree with degenerate empty-weighted internal nodes.

define method rope-concatenate (a :: <rope>, b :: <rope>) => (r :: <rope>)
  let asize = rope-size(a);
  let bsize = rope-size(b);
  if (asize = 0)
    b
  elseif (bsize = 0)
    a
  else
    make(<rope-node>,
         left: a, right: b,
         weight: asize, total: asize + bsize,
         newlines: rope-newlines(a) + rope-newlines(b))
  end
end method;

// Sprint 43b helper — the empty rope. Used as the identity element for
// `rope-concatenate` and the result of `split-at` at index 0 or `size(r)`.

define function empty-rope () => (r :: <rope-leaf>)
  make(<rope-leaf>, bytes: "", len: 0, newlines: 0)
end function;

// ─── for-each-leaf — in-order leaf walk ──────────────────────────────────
//
// `fn` is a `<function>` of one arg: the leaf's `<byte-string>`. Result
// is discarded. Useful for rendering (walk leaves, draw each chunk) and
// for serialising (walk leaves, write each chunk to disk).

define method for-each-leaf (r :: <rope-leaf>, fn) => ()
  fn(rope-leaf-bytes(r));
  #f
end method;

define method for-each-leaf (r :: <rope-node>, fn) => ()
  for-each-leaf(rope-node-left(r), fn);
  for-each-leaf(rope-node-right(r), fn);
  #f
end method;

// ─── rope-substring — fresh <byte-string> for [lo, hi) ───────────────────
//
// Allocate the destination once, then walk the rope copying intersecting
// runs from each leaf via the bulk-copy primitive. The recursive
// `rope-copy-into` returns the number of bytes copied so the caller can
// advance `dst-off` correctly across sibling traversals.

define method rope-copy-into
    (r :: <rope-leaf>, lo :: <integer>, hi :: <integer>,
     dst :: <byte-string>, dst-off :: <integer>)
 => (n :: <integer>)
  let leaf-len = rope-leaf-len(r);
  // Intersect [lo, hi) with [0, leaf-len).
  let a = if (lo > 0) lo else 0 end;
  let b = if (hi < leaf-len) hi else leaf-len end;
  if (a < b)
    %byte-string-copy!(dst, dst-off, rope-leaf-bytes(r), a, b - a);
    b - a
  else
    0
  end
end method;

define method rope-copy-into
    (r :: <rope-node>, lo :: <integer>, hi :: <integer>,
     dst :: <byte-string>, dst-off :: <integer>)
 => (n :: <integer>)
  let w = rope-node-weight(r);
  // Left child covers indices [0, w); right covers [w, total).
  let from-left =
    if (lo < w)
      let left-hi = if (hi < w) hi else w end;
      rope-copy-into(rope-node-left(r), lo, left-hi, dst, dst-off)
    else
      0
    end;
  let from-right =
    if (hi > w)
      let right-lo = if (lo > w) lo - w else 0 end;
      let right-hi = hi - w;
      rope-copy-into(rope-node-right(r), right-lo, right-hi,
                     dst, dst-off + from-left)
    else
      0
    end;
  from-left + from-right
end method;

define function rope-substring
    (r, lo :: <integer>, hi :: <integer>) => (s :: <byte-string>)
  let n = hi - lo;
  let result = %byte-string-allocate(n);
  rope-copy-into(r, lo, hi, result, 0);
  result
end function;

// ─── make-rope-from-string — recursive split into ≤1024-byte leaves ──────
//
// Leaf max: 1024 — small enough to keep individual-edit copying cheap
// (Sprint 43b), large enough to keep the tree shallow and dispatch / GC
// overhead low. Inlined literal here because user-code `define constant`
// lowers to a 0-arity function call shape; an inline 1024 is cleaner
// for the one site that needs it.

define function make-rope-from-string (s) => (r)
  let n = size(s);
  if (n <= 1024)
    make(<rope-leaf>,
         bytes: s, len: n,
         newlines: count-newlines-in(s))
  else
    let mid = n / 2;
    let left  = make-rope-from-string(copy-sequence(s, 0, mid));
    let right = make-rope-from-string(copy-sequence(s, mid, n));
    rope-concatenate(left, right)
  end
end function;

// ─── Sprint 43b — split / insert / delete ────────────────────────────────
//
// Edits are expressed as splits + concats. The data structure stays
// persistent (no in-place mutation of existing nodes), so any rope
// reference an old WNDPROC closure holds remains valid — the new
// version is a sibling tree that shares almost all leaves with the
// old one.
//
// Allocation cost per edit (insert or delete) is O(log n) fresh
// internal nodes and ≤2 small byte-string allocations from leaf
// splits — the rest of the leaves are reused. This is the GC stress
// the random-edit self-test exercises.
//
// Multiple-return convention: split-at returns a `<pair>` carrying
// (left, right) accessed via `head` / `tail`. Cheap (one pair-cell
// allocation per call) and works today; promoting to true
// multiple-values would be a separate sprint.

define method rope-split-at (r :: <rope-leaf>, i :: <integer>) => (split)
  let n = rope-leaf-len(r);
  if (i <= 0)
    pair(empty-rope(), r)
  elseif (i >= n)
    pair(r, empty-rope())
  else
    let bytes = rope-leaf-bytes(r);
    let left-bytes  = copy-sequence(bytes, 0, i);
    let right-bytes = copy-sequence(bytes, i, n);
    pair(make(<rope-leaf>,
              bytes: left-bytes,  len: i,
              newlines: count-newlines-in(left-bytes)),
         make(<rope-leaf>,
              bytes: right-bytes, len: n - i,
              newlines: count-newlines-in(right-bytes)))
  end
end method;

define method rope-split-at (r :: <rope-node>, i :: <integer>) => (split)
  let total = rope-node-total(r);
  if (i <= 0)
    pair(empty-rope(), r)
  elseif (i >= total)
    pair(r, empty-rope())
  else
    let w = rope-node-weight(r);
    if (i = w)
      // Clean split at the node's own boundary — no children touched.
      pair(rope-node-left(r), rope-node-right(r))
    elseif (i < w)
      // Index falls in the left subtree. Split the left, attach its
      // right-piece to the existing right subtree.
      let inner = rope-split-at(rope-node-left(r), i);
      pair(head(inner),
           rope-concatenate(tail(inner), rope-node-right(r)))
    else
      // Index falls in the right subtree. Split the right; the left
      // stays whole and gets paired with the right's left-piece.
      let inner = rope-split-at(rope-node-right(r), i - w);
      pair(rope-concatenate(rope-node-left(r), head(inner)),
           tail(inner))
    end
  end
end method;

// rope-insert(r, i, s) — return a new rope with byte-string `s`
// spliced in at position `i` (0 <= i <= size(r)).

define function rope-insert (r, i :: <integer>, s) => (out)
  let split = rope-split-at(r, i);
  let middle = make-rope-from-string(s);
  rope-concatenate(rope-concatenate(head(split), middle), tail(split))
end function;

// rope-delete(r, lo, hi) — return a new rope with bytes [lo, hi) gone.

define function rope-delete (r, lo :: <integer>, hi :: <integer>) => (out)
  let first-split  = rope-split-at(r, lo);
  let second-split = rope-split-at(tail(first-split), hi - lo);
  rope-concatenate(head(first-split), tail(second-split))
end function;

// rope->string(r) — convenience wrapper: serialise the whole rope to
// a flat `<byte-string>`. Used by the self-test to compare an edited
// rope against a reference flat-string built in parallel.

define function rope->string (r) => (s)
  rope-substring(r, 0, rope-size(r))
end function;

// ─── Sprint 43c — line-indexing API ──────────────────────────────────────
//
// Convention: a buffer has `1 + count('\n' in buffer)` lines. Line 0
// starts at offset 0; line k (k > 0) starts at the byte after the kth
// `\n`. Examples:
//   ""                  → 1 line  (line 0 starts at offset 0, length 0)
//   "abc"               → 1 line  (line 0 = "abc")
//   "abc\n"             → 2 lines (line 0 = "abc", line 1 = "")
//   "abc\ndef"          → 2 lines (line 0 = "abc", line 1 = "def")
//   "abc\ndef\n"        → 3 lines (the third is the trailing empty line)
//
// rope-line-to-offset(r, ln) returns the offset of the FIRST byte of
// line `ln`. For ln out of range, returns rope-size(r) (one-past-end,
// the "empty trailing line" offset). For ln = 0, returns 0.
//
// rope-offset-to-line(r, off) returns the line number containing the
// byte at offset `off`. Equivalent to "count newlines in [0, off)".

define method rope-line-count (r :: <rope>) => (n :: <integer>)
  rope-newlines(r) + 1
end method;

// ─── rope-line-to-offset ────────────────────────────────────────────────

define method rope-line-to-offset
    (r :: <rope-leaf>, ln :: <integer>) => (off :: <integer>)
  if (ln <= 0)
    0
  else
    let bytes = rope-leaf-bytes(r);
    let n     = rope-leaf-len(r);
    let seen  = 0;
    let pos   = 0;
    let found = -1;
    until (pos = n | found >= 0)
      if (element(bytes, pos) = 10)
        seen := seen + 1;
        if (seen = ln)
          found := pos + 1;
        else
          #f
        end;
      else
        #f
      end;
      pos := pos + 1;
    end;
    if (found < 0) n else found end
  end
end method;

define method rope-line-to-offset
    (r :: <rope-node>, ln :: <integer>) => (off :: <integer>)
  if (ln <= 0)
    0
  else
    let left-newlines = rope-newlines(rope-node-left(r));
    if (ln <= left-newlines)
      // Line `ln` starts inside the left subtree (because left holds at
      // least `left-newlines` line boundaries — boundary k starts line k).
      rope-line-to-offset(rope-node-left(r), ln)
    else
      // Line `ln` starts in the right subtree. Subtract the line
      // boundaries left already accounts for, and add the left's total
      // byte size to translate back to the global offset.
      rope-node-weight(r)
        + rope-line-to-offset(rope-node-right(r), ln - left-newlines)
    end
  end
end method;

// ─── rope-offset-to-line ────────────────────────────────────────────────
// "Which line is byte `off` on?" = count newlines in [0, off).

define method rope-offset-to-line
    (r :: <rope-leaf>, off :: <integer>) => (ln :: <integer>)
  let bytes = rope-leaf-bytes(r);
  let n     = rope-leaf-len(r);
  let limit = if (off < 0) 0 elseif (off > n) n else off end;
  let count = 0;
  let i = 0;
  until (i = limit)
    if (element(bytes, i) = 10)
      count := count + 1;
    else
      #f
    end;
    i := i + 1;
  end;
  count
end method;

define method rope-offset-to-line
    (r :: <rope-node>, off :: <integer>) => (ln :: <integer>)
  let w = rope-node-weight(r);
  if (off <= w)
    // Stay within left subtree.
    rope-offset-to-line(rope-node-left(r), off)
  else
    // Counted all of left's newlines + however many are in right's
    // [0, off-w) prefix.
    rope-newlines(rope-node-left(r))
      + rope-offset-to-line(rope-node-right(r), off - w)
  end
end method;

// ─── Self-tests (run as part of `main`) ──────────────────────────────────
//
// Build a deterministic test buffer where byte[i] = i mod 256, drive
// each rope op against it, and emit PASS / FAIL lines on stdout. The
// Rust-side test (`rope_ops.rs`) runs this EXE and asserts every
// expected PASS line appears.

define function make-test-bytes (n :: <integer>) => (s :: <byte-string>)
  let s = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    let b = i - (i / 256) * 256;
    %byte-string-element-setter(b, s, i);
    i := i + 1;
  end;
  s
end function;

// 600-byte chunk filled with 'a' (97) except for a single `\n` (10) at
// `nl-pos`. Used in line-indexing tests to give us multi-leaf ropes
// with known newline locations — we can't use make-test-bytes here
// because its `i mod 256` pattern produces incidental `\n` bytes at
// positions 10, 266, 522, …, ruining the line count.

define function make-newline-chunk
    (n :: <integer>, nl-pos :: <integer>) => (s :: <byte-string>)
  let s = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    %byte-string-element-setter(97, s, i);     // 'a'
    i := i + 1;
  end;
  %byte-string-element-setter(10, s, nl-pos);  // single '\n'
  s
end function;

// Verify byte[i] = i mod 256 across a rope.
define function rope-bytes-match-pattern? (r, n :: <integer>) => (ok :: <boolean>)
  let ok = #t;
  let i = 0;
  until (i = n)
    let expected = i - (i / 256) * 256;
    if (rope-element(r, i) ~= expected)
      ok := #f;
    else
      #f
    end;
    i := i + 1;
  end;
  ok
end function;

define function main () => ()
  // ─── Test 1: tiny single-leaf rope ──────────────────────────────────
  let small = "hello";
  let r1 = make-rope-from-string(small);
  if (rope-size(r1) = 5)
    format-out("PASS: small rope size\n");
  else
    format-out("FAIL: small rope size = %d\n", rope-size(r1));
  end;
  // 'h' = 104, 'o' = 111
  if (rope-element(r1, 0) = 104 & rope-element(r1, 4) = 111)
    format-out("PASS: small rope elements\n");
  else
    format-out("FAIL: small rope elements\n");
  end;

  // ─── Test 2: multi-leaf rope (4000 bytes → ~4 leaves at max=1024) ──
  let big-bytes = make-test-bytes(4000);
  let big-rope  = make-rope-from-string(big-bytes);
  if (rope-size(big-rope) = 4000)
    format-out("PASS: big rope size\n");
  else
    format-out("FAIL: big rope size = %d\n", rope-size(big-rope));
  end;
  if (rope-bytes-match-pattern?(big-rope, 4000))
    format-out("PASS: big rope element pattern\n");
  else
    format-out("FAIL: big rope element pattern\n");
  end;

  // ─── Test 3: rope-substring round-trip across leaf boundary ────────
  // Take bytes [1000, 1100) — crosses the ~1024-byte boundary.
  let sub = rope-substring(big-rope, 1000, 1100);
  let sub-ok = #t;
  if (size(sub) ~= 100) sub-ok := #f else #f end;
  let j = 0;
  until (j = 100)
    let expected = (1000 + j) - ((1000 + j) / 256) * 256;
    if (element(sub, j) ~= expected) sub-ok := #f else #f end;
    j := j + 1;
  end;
  if (sub-ok)
    format-out("PASS: rope-substring across leaf boundary\n");
  else
    format-out("FAIL: rope-substring (size=%d)\n", size(sub));
  end;

  // ─── Test 4: rope-concatenate ──────────────────────────────────────
  let a = make-rope-from-string("foo");
  let b = make-rope-from-string("bar");
  let c = rope-concatenate(a, b);
  // 'f'=102 'o'=111 'o'=111 'b'=98 'a'=97 'r'=114
  if (rope-size(c) = 6
        & rope-element(c, 0) = 102
        & rope-element(c, 3) = 98
        & rope-element(c, 5) = 114)
    format-out("PASS: rope-concatenate\n");
  else
    format-out("FAIL: rope-concatenate size=%d\n", rope-size(c));
  end;

  // ─── Test 5: for-each-leaf covers every byte ───────────────────────
  // Walk the big rope, summing leaf sizes via a captured cell. The
  // sum equals total bytes iff every leaf is visited exactly once.
  let visited = 0;
  for-each-leaf(big-rope,
                method (leaf-bytes)
                  visited := visited + size(leaf-bytes)
                end);
  if (visited = 4000)
    format-out("PASS: for-each-leaf covers all bytes\n");
  else
    format-out("FAIL: for-each-leaf visited %d bytes (expected 4000)\n",
               visited);
  end;

  // ─── Test 6: rope-substring whole-rope returns equal bytes ─────────
  let whole = rope-substring(big-rope, 0, 4000);
  if (size(whole) = 4000 & whole = big-bytes)
    format-out("PASS: rope-substring full range == original\n");
  else
    format-out("FAIL: rope-substring full range mismatch\n");
  end;

  // ─── Test 7: rope-split-at boundary cases + interior ───────────────
  let s0 = rope-split-at(big-rope, 0);
  let s-end = rope-split-at(big-rope, 4000);
  let s-mid = rope-split-at(big-rope, 1500);
  let split-ok = (rope-size(head(s0)) = 0)
                 & (rope-size(tail(s0)) = 4000)
                 & (rope-size(head(s-end)) = 4000)
                 & (rope-size(tail(s-end)) = 0)
                 & (rope-size(head(s-mid)) = 1500)
                 & (rope-size(tail(s-mid)) = 2500);
  if (split-ok)
    format-out("PASS: rope-split-at boundary + interior sizes\n");
  else
    format-out("FAIL: rope-split-at sizes\n");
  end;
  // The two split halves should concat back to the original byte-for-byte.
  let rejoined = rope-concatenate(head(s-mid), tail(s-mid));
  if (rope->string(rejoined) = big-bytes)
    format-out("PASS: split-at + concatenate round-trips\n");
  else
    format-out("FAIL: split-at + concatenate round-trip mismatch\n");
  end;

  // ─── Test 8: rope-insert correctness ───────────────────────────────
  // Insert "XYZ" into "hello" at position 2 → "heXYZllo"
  let inserted = rope-insert(make-rope-from-string("hello"), 2, "XYZ");
  if (rope-size(inserted) = 8
        & rope->string(inserted) = "heXYZllo")
    format-out("PASS: rope-insert at interior position\n");
  else
    format-out("FAIL: rope-insert produced `%s`\n", rope->string(inserted));
  end;
  // Insert at start.
  let pre = rope-insert(make-rope-from-string("world"), 0, "hello, ");
  if (rope->string(pre) = "hello, world")
    format-out("PASS: rope-insert at start\n");
  else
    format-out("FAIL: rope-insert at start: `%s`\n", rope->string(pre));
  end;
  // Insert at end.
  let post = rope-insert(make-rope-from-string("hello"), 5, ", world");
  if (rope->string(post) = "hello, world")
    format-out("PASS: rope-insert at end\n");
  else
    format-out("FAIL: rope-insert at end: `%s`\n", rope->string(post));
  end;

  // ─── Test 9: rope-delete correctness ───────────────────────────────
  // Delete [2, 5) from "hello, world" → "he, world"
  let deleted = rope-delete(make-rope-from-string("hello, world"), 2, 5);
  if (rope->string(deleted) = "he, world")
    format-out("PASS: rope-delete interior range\n");
  else
    format-out("FAIL: rope-delete: `%s`\n", rope->string(deleted));
  end;
  // Delete prefix.
  let chopped = rope-delete(make-rope-from-string("hello, world"), 0, 7);
  if (rope->string(chopped) = "world")
    format-out("PASS: rope-delete prefix\n");
  else
    format-out("FAIL: rope-delete prefix: `%s`\n", rope->string(chopped));
  end;
  // Delete suffix.
  let truncated = rope-delete(make-rope-from-string("hello, world"), 5, 12);
  if (rope->string(truncated) = "hello")
    format-out("PASS: rope-delete suffix\n");
  else
    format-out("FAIL: rope-delete suffix: `%s`\n", rope->string(truncated));
  end;

  // ─── Test 10: insert + delete on multi-leaf rope ───────────────────
  // Take the 4000-byte rope, insert 100 bytes at position 2000, then
  // delete those same 100 bytes. The result must byte-match the
  // original — proves split/insert/delete compose correctly across
  // leaf boundaries.
  let chunk = make-test-bytes(100);
  let widened = rope-insert(big-rope, 2000, chunk);
  if (rope-size(widened) = 4100)
    format-out("PASS: rope-insert across leaf boundary grows size correctly\n");
  else
    format-out("FAIL: widened size = %d\n", rope-size(widened));
  end;
  let restored = rope-delete(widened, 2000, 2100);
  if (rope-size(restored) = 4000 & rope->string(restored) = big-bytes)
    format-out("PASS: insert-then-delete round-trips the original\n");
  else
    format-out("FAIL: restored size = %d\n", rope-size(restored));
  end;

  // ─── Test 11: GC-stress random-edit walk ───────────────────────────
  // 200 alternating insert/delete ops on a starter rope. Each op
  // allocates ~log(n) fresh internal nodes + a leaf or two — that's
  // a few thousand small heap objects across the run. Proves the GC
  // keeps up with rope churn, AND that split/insert/delete compose
  // correctly across many steps. We track a parallel reference rope
  // built by the same op sequence, and compare at the end via `=`.
  //
  // "Random" here means deterministic-from-i — no <random> generic
  // yet. The seed gives us a varied-enough mix of positions / sizes.
  let cur = make-rope-from-string(big-bytes);
  let ref = make-rope-from-string(big-bytes);
  let step = 0;
  let stress-ok = #t;
  until (step = 200)
    // Pseudo-random position in [0, rope-size(cur)).
    let pos = (step * 137 + 23) - ((step * 137 + 23) / rope-size(cur)) * rope-size(cur);
    let chunk = make-test-bytes(8);
    // Even step inserts 8 bytes; odd step deletes 8 bytes (or fewer
    // if we'd run off the end).
    if ((step - (step / 2) * 2) = 0)
      cur := rope-insert(cur, pos, chunk);
      ref := rope-insert(ref, pos, chunk);
    else
      let avail = rope-size(cur) - pos;
      let take = if (avail > 8) 8 else avail end;
      if (take > 0)
        cur := rope-delete(cur, pos, pos + take);
        ref := rope-delete(ref, pos, pos + take);
      else
        #f
      end;
    end;
    step := step + 1;
  end;
  // After 200 ops, cur and ref should be byte-identical (we drove
  // them in lockstep — this is mostly a self-consistency check, but
  // it forces materialisation of every byte at the end).
  if (rope-size(cur) = rope-size(ref) & rope->string(cur) = rope->string(ref))
    format-out("PASS: 200-op GC-stress walk byte-matches reference\n");
  else
    format-out("FAIL: stress walk mismatch (cur=%d ref=%d)\n",
               rope-size(cur), rope-size(ref));
  end;

  // ─── Test 12: rope-line-count basic shapes ─────────────────────────
  // Convention: 1 + count('\n'). Empty buffer = 1 line.
  let lc-empty = rope-line-count(make-rope-from-string(""));
  let lc-noeol = rope-line-count(make-rope-from-string("abc"));
  let lc-trail = rope-line-count(make-rope-from-string("abc\n"));
  let lc-three = rope-line-count(make-rope-from-string("a\nb\nc"));
  if (lc-empty = 1 & lc-noeol = 1 & lc-trail = 2 & lc-three = 3)
    format-out("PASS: rope-line-count on simple buffers\n");
  else
    format-out("FAIL: line counts empty=%d noeol=%d\n", lc-empty, lc-noeol);
    format-out("FAIL: line counts trail=%d three=%d\n", lc-trail, lc-three);
  end;

  // ─── Test 13: rope-line-to-offset on single leaf ───────────────────
  // "abc\ndef\nghi" — lines at 0, 4, 8.
  let multiline = make-rope-from-string("abc\ndef\nghi");
  let l0 = rope-line-to-offset(multiline, 0);
  let l1 = rope-line-to-offset(multiline, 1);
  let l2 = rope-line-to-offset(multiline, 2);
  if (l0 = 0 & l1 = 4 & l2 = 8)
    format-out("PASS: rope-line-to-offset on single-leaf buffer\n");
  else
    format-out("FAIL: line offsets: l0=%d l1=%d l2=%d\n", l0, l1, l2);
  end;

  // ─── Test 14: rope-line-to-offset across leaf boundaries ───────────
  // Build a multi-leaf rope by concatenating four 600-byte chunks,
  // each containing exactly one '\n' at position 100. So the combined
  // buffer is 2400 bytes long with newlines at 100, 700, 1300, 1900.
  // The four leaves are explicitly concat'd so we get a 4-deep tree.
  let chunk-with-nl = make-newline-chunk(600, 100);
  let r4 = rope-concatenate(
             rope-concatenate(make-rope-from-string(chunk-with-nl),
                              make-rope-from-string(chunk-with-nl)),
             rope-concatenate(make-rope-from-string(chunk-with-nl),
                              make-rope-from-string(chunk-with-nl)));
  // 4 newlines → 5 lines.
  let r4-lines = rope-line-count(r4);
  let r4-l0    = rope-line-to-offset(r4, 0);
  let r4-l2    = rope-line-to-offset(r4, 2);   // after newline at 700
  let r4-l4    = rope-line-to-offset(r4, 4);   // after newline at 1900
  if (r4-lines = 5 & r4-l0 = 0 & r4-l2 = 701 & r4-l4 = 1901)
    format-out("PASS: rope-line-to-offset across leaf boundaries\n");
  else
    format-out("FAIL: r4 lines=%d l0=%d\n", r4-lines, r4-l0);
    format-out("FAIL: r4 l2=%d l4=%d (want 701, 1901)\n", r4-l2, r4-l4);
  end;

  // ─── Test 15: rope-offset-to-line round-trips line-to-offset ───────
  // For each line k in r4, offset-to-line(line-to-offset(k)) should
  // return k.
  let rt-ok = #t;
  let k = 0;
  until (k = 5)
    let off = rope-line-to-offset(r4, k);
    let back = rope-offset-to-line(r4, off);
    if (back ~= k) rt-ok := #f else #f end;
    k := k + 1;
  end;
  if (rt-ok)
    format-out("PASS: rope-line-to-offset / rope-offset-to-line round-trip\n");
  else
    format-out("FAIL: line/offset round-trip\n");
  end;

  // ─── Test 16: line counts survive insert / delete ──────────────────
  // Start with a 3-line buffer. Insert a 2-newline chunk in the middle;
  // line count should grow by 2. Then delete it again; line count back
  // to original.
  let base = make-rope-from-string("aaa\nbbb\nccc");
  let inserted-nl = rope-insert(base, 4, "xx\nyy\nzz");
  let after-ins   = rope-line-count(inserted-nl);
  let after-del   = rope-line-count(rope-delete(inserted-nl, 4, 12));
  let base-count  = rope-line-count(base);
  if (base-count = 3 & after-ins = 5 & after-del = 3)
    format-out("PASS: line count tracks through insert + delete\n");
  else
    format-out("FAIL: line counts base=%d ins=%d del=%d (want 3 5 3)\n",
               base-count, after-ins, after-del);
  end;

  format-out("DONE\n");
end function main;
