Module: nod-ide
Precedence: c

// Sprint 44 — IDE module split (part 2 of 5: rope buffer).
//
// The IDE's text buffer is a `<rope>` — a classical Boehm-style binary
// tree of `<byte-string>` chunks. Reads + edits are O(log n) instead
// of O(n) for the previous flat-`<byte-string>` buffer, which matters
// once the file is non-trivial (~256k+) and the user is editing.
//
// The full data structure is documented and tested standalone in
// `tests/nod-tests/fixtures/rope.dylan` (~650 lines + 24 self-tests
// passing under AOT). What's in this file is the production subset:
// classes, construction, read ops, concat, split, insert, delete,
// line indexing, `rope->string` for serialisation, plus two `nod-rope-*`
// wrappers (line-count + max-line-chars) used by main when sizing
// the scrollbars. The self-test main and the deterministic
// test-buffer helpers stay in the standalone fixture.
//
// Prior to the Sprint 44 multi-file split this code was inlined into
// the monolithic `nod-ide.dylan` (preserved as `unified_ide.dylan`)
// because nod-driver could only build one Dylan file per invocation.
// Now it sits in its own module file alongside the rest of the IDE.

define class <rope> (<object>) end class;

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

define method rope-size (r :: <rope-leaf>) => (n :: <integer>)
  rope-leaf-len(r)
end method;

define method rope-size (r :: <rope-node>) => (n :: <integer>)
  rope-node-total(r)
end method;

define method rope-newlines (r :: <rope-leaf>) => (n :: <integer>)
  rope-leaf-newlines(r)
end method;

define method rope-newlines (r :: <rope-node>) => (n :: <integer>)
  rope-node-newlines(r)
end method;

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

define function empty-rope () => (r :: <rope-leaf>)
  make(<rope-leaf>, bytes: "", len: 0, newlines: 0)
end function;

define method for-each-leaf (r :: <rope-leaf>, fn) => ()
  fn(rope-leaf-bytes(r));
  #f
end method;

define method for-each-leaf (r :: <rope-node>, fn) => ()
  for-each-leaf(rope-node-left(r), fn);
  for-each-leaf(rope-node-right(r), fn);
  #f
end method;

define method rope-copy-into
    (r :: <rope-leaf>, lo :: <integer>, hi :: <integer>,
     dst :: <byte-string>, dst-off :: <integer>)
 => (n :: <integer>)
  let leaf-len = rope-leaf-len(r);
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
      pair(rope-node-left(r), rope-node-right(r))
    elseif (i < w)
      let inner = rope-split-at(rope-node-left(r), i);
      pair(head(inner),
           rope-concatenate(tail(inner), rope-node-right(r)))
    else
      let inner = rope-split-at(rope-node-right(r), i - w);
      pair(rope-concatenate(rope-node-left(r), head(inner)),
           tail(inner))
    end
  end
end method;

define function rope-insert (r, i :: <integer>, s) => (out)
  let split = rope-split-at(r, i);
  let middle = make-rope-from-string(s);
  rope-concatenate(rope-concatenate(head(split), middle), tail(split))
end function;

define function rope-delete (r, lo :: <integer>, hi :: <integer>) => (out)
  let first-split  = rope-split-at(r, lo);
  let second-split = rope-split-at(tail(first-split), hi - lo);
  rope-concatenate(head(first-split), tail(second-split))
end function;

define function rope->string (r) => (s)
  rope-substring(r, 0, rope-size(r))
end function;

define method rope-line-count (r :: <rope>) => (n :: <integer>)
  rope-newlines(r) + 1
end method;

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
      rope-line-to-offset(rope-node-left(r), ln)
    else
      rope-node-weight(r)
        + rope-line-to-offset(rope-node-right(r), ln - left-newlines)
    end
  end
end method;

// ─── Sprint 43d — rope-aware buffer-measurement helpers ──────────────────
//
// Replace the old `nod-count-newlines` / `nod-max-line-chars` calls
// on `<byte-string>` with rope-aware versions. Line count is O(1)
// (cached at every internal node). Longest line uses for-each-leaf
// with running state — note this gives a CONSERVATIVE upper bound:
// it treats every leaf boundary as a potential line continuation,
// so the answer is right when the longest line is fully inside one
// leaf and an over-estimate when a long line straddles leaves. Good
// enough for sizing the horizontal scrollbar; we can tighten this
// later if needed.

define function nod-rope-line-count (r) => (n :: <integer>)
  rope-line-count(r)
end function;

define function nod-rope-max-line-chars (r) => (best :: <integer>)
  let best = 0;
  let cur  = 0;
  for-each-leaf(r,
                method (leaf-bytes)
                  let len = size(leaf-bytes);
                  let i = 0;
                  until (i = len)
                    if (element(leaf-bytes, i) = 10)
                      if (cur > best) best := cur; else #f end;
                      cur := 0;
                    else
                      cur := cur + 1;
                    end;
                    i := i + 1;
                  end;
                end);
  if (cur > best) cur else best end
end function;
