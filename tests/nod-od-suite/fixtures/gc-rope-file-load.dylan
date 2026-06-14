Module: gc-rope-file-load

// GC rope file-load test.
//
// Loads real files from f:\scratch into rope buffers and exercises
// every rope operation: make-rope-from-string, rope-size, rope-element,
// rope-line-count, rope-line-to-offset, rope-offset-to-line,
// rope-concatenate, rope-split-at, rope-insert, rope-delete,
// for-each-leaf, rope->string, rope-substring.
//
// Expected file: f:\scratch\sample-tall-wide.txt
//   86296 bytes, 2220 LF bytes → rope-line-count = 2221, first byte = '/' (47)
//
// main() returns rope-line-count(r) = 2221 if every assertion passes,
// 0 if any assertion fails.  The Rust test asserts the return value is 2221.

// ─── Rope class hierarchy ─────────────────────────────────────────────────

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

define class <rope-report> (<object>)
  slot report-bytes          :: <integer>, init-keyword: bytes:;
  slot report-lines          :: <integer>, init-keyword: lines:;
  slot report-words          :: <integer>, init-keyword: words:;
  slot report-distinct-words :: <integer>, init-keyword: distinct-words:;
  slot report-distinct-bytes :: <integer>, init-keyword: distinct-bytes:;
  slot report-file-count     :: <integer>, init-keyword: file-count:;
  slot report-word-freq      :: <table>,   init-keyword: word-freq:;
  slot report-byte-freq      :: <table>,   init-keyword: byte-freq:;
end class;

// ─── Newline-count helper ──────────────────────────────────────────────────

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

// ─── rope-size — O(1) ─────────────────────────────────────────────────────

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

// ─── rope-element — O(log n) ──────────────────────────────────────────────

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

// ─── rope-concatenate — O(1) ──────────────────────────────────────────────

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

// ─── for-each-leaf — in-order walk ────────────────────────────────────────

define method for-each-leaf (r :: <rope-leaf>, fn) => ()
  fn(rope-leaf-bytes(r));
  #f
end method;

define method for-each-leaf (r :: <rope-node>, fn) => ()
  for-each-leaf(rope-node-left(r), fn);
  for-each-leaf(rope-node-right(r), fn);
  #f
end method;

// ─── rope-copy-into ───────────────────────────────────────────────────────

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

// ─── make-rope-from-string ────────────────────────────────────────────────

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

// ─── rope-split-at ────────────────────────────────────────────────────────

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

define function rope-line-count-via-string (r) => (n :: <integer>)
  count-newlines-in(rope->string(r)) + 1
end function;

define function reverse-byte-string (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    %byte-string-element-setter(element(s, n - i - 1), out, i);
    i := i + 1;
  end;
  out
end function;

define function byte-string-equal? (a :: <byte-string>, b :: <byte-string>) => (eq? :: <boolean>)
  let n = size(a);
  if (~(n = size(b)))
    #f
  else
    let i = 0;
    let same = #t;
    while (i < n & same)
      if (~(element(a, i) = element(b, i)))
        same := #f
      else
        #f
      end;
      i := i + 1;
    end;
    same
  end
end function;

define function word-byte? (b :: <integer>) => (word? :: <boolean>)
  ((b >= 48 & b <= 57)
     | (b >= 65 & b <= 90)
     | (b >= 97 & b <= 122)
     | (b = 95))
end function;

define function table-count (t :: <table>, key) => (n :: <integer>)
  let value = element(t, key);
  if (value) value else 0 end
end function;

define function table-inc! (t :: <table>, key) => ()
  element-setter(table-count(t, key) + 1, t, key);
  #f
end function;

define function count-words-in-byte-string (s :: <byte-string>) => (n :: <integer>)
  let count = 0;
  let i = 0;
  let n = size(s);
  while (i < n)
    if (word-byte?(element(s, i)))
      count := count + 1;
      i := i + 1;
      while (i < n & word-byte?(element(s, i)))
        i := i + 1;
      end;
    else
      i := i + 1;
    end;
  end;
  count
end function;

define function word-frequency-table (s :: <byte-string>) => (freq :: <table>)
  let freq = make(<table>);
  let i = 0;
  let n = size(s);
  while (i < n)
    if (word-byte?(element(s, i)))
      let start = i;
      i := i + 1;
      while (i < n & word-byte?(element(s, i)))
        i := i + 1;
      end;
      table-inc!(freq, copy-sequence(s, start, i));
    else
      i := i + 1;
    end;
  end;
  freq
end function;

define function byte-frequency-table (s :: <byte-string>) => (freq :: <table>)
  let freq = make(<table>);
  let i = 0;
  let n = size(s);
  while (i < n)
    table-inc!(freq, element(s, i));
    i := i + 1;
  end;
  freq
end function;

define function make-rope-report (r :: <rope>) => (report :: <rope-report>)
  let s = rope->string(r);
  let words = word-frequency-table(s);
  let bytes = byte-frequency-table(s);
  make(<rope-report>,
       bytes: size(s),
  lines: count-newlines-in(s) + 1,
       words: count-words-in-byte-string(s),
       distinct-words: size(keys(words)),
       distinct-bytes: size(keys(bytes)),
       file-count: 1,
       word-freq: words,
       byte-freq: bytes)
end function;

define function make-multi-file-rope-report
    (path1 :: <byte-string>, path2 :: <byte-string>, path3 :: <byte-string>)
 => (report :: <rope-report>)
  let s1 = %read-file(path1);
  let s2 = %read-file(path2);
  let s3 = %read-file(path3);
  let combined-string = concatenate(concatenate(concatenate(s1, "\n"), s2), concatenate("\n", s3));
  let words = word-frequency-table(combined-string);
  let bytes = byte-frequency-table(combined-string);
  let words1 = word-frequency-table(s1);
  let words2 = word-frequency-table(s2);
  let words3 = word-frequency-table(s3);
  let bytes1 = byte-frequency-table(s1);
  let bytes2 = byte-frequency-table(s2);
  let bytes3 = byte-frequency-table(s3);
  if (~(size(combined-string) = size(s1) + size(s2) + size(s3) + 2))
    make(<rope-report>,
          bytes: 0, lines: 0, words: 0,
          distinct-words: 0, distinct-bytes: 0,
         file-count: 0,
         word-freq: make(<table>),
         byte-freq: make(<table>))
  elseif (~(table-count(words, "define")
            = table-count(words1, "define")
              + table-count(words2, "define")
              + table-count(words3, "define")))
    make(<rope-report>,
          bytes: 0, lines: 0, words: 0,
          distinct-words: 0, distinct-bytes: 0,
         file-count: 0,
         word-freq: make(<table>),
         byte-freq: make(<table>))
  elseif (~(table-count(bytes, 10)
            = table-count(bytes1, 10)
              + table-count(bytes2, 10)
              + table-count(bytes3, 10)
              + 2))
    make(<rope-report>,
          bytes: 0, lines: 0, words: 0,
          distinct-words: 0, distinct-bytes: 0,
         file-count: 0,
         word-freq: make(<table>),
         byte-freq: make(<table>))
  else
    make(<rope-report>,
         bytes: size(combined-string),
         lines: count-newlines-in(combined-string) + 1,
         words: count-words-in-byte-string(combined-string),
         distinct-words: size(keys(words)),
         distinct-bytes: size(keys(bytes)),
         file-count: 3,
         word-freq: words,
         byte-freq: bytes)
  end
end function;

define function edited-rope-report-ok? () => (ok :: <boolean>)
  let base = make-rope-from-string("alpha\nbeta\n");
  let inserted = rope-insert(base, 5, "@@");
  let edited = rope-delete(inserted, 1, 3);
  let report = make-rope-report(edited);
  (report-bytes(report) = rope-size(edited))
    & (report-lines(report) = table-count(report-byte-freq(report), 10) + 1)
    & (table-count(report-byte-freq(report), 10) + 1 = report-lines(report))
    & (report-words(report) = 2)
    & (report-distinct-words(report) = 2)
    & (report-distinct-bytes(report) = 7)
end function;

define function multi-file-rope-report-ok? () => (ok :: <boolean>)
  let report = make-multi-file-rope-report(
                 "f:\\scratch\\30.dylan",
                 "f:\\scratch\\32a.dylan",
                 "f:\\scratch\\40a.dylan");
  (report-file-count(report) = 3)
    & (report-bytes(report) > 0)
    & (report-lines(report) = table-count(report-byte-freq(report), 10) + 1)
    & (report-words(report) >= report-distinct-words(report))
    & (report-distinct-bytes(report) > 10)
    & (table-count(report-word-freq(report), "define") > 0)
    & (table-count(report-byte-freq(report), 10) >= 2)
end function;

define function reverse-lines-in-byte-string (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let out = "";
  let start = 0;
  while (start <= n)
    let stop = start;
    while (stop < n & ~(element(s, stop) = 10))
      stop := stop + 1;
    end;
    out := concatenate(out, reverse-byte-string(copy-sequence(s, start, stop)));
    if (stop < n)
      out := concatenate(out, "\n");
      start := stop + 1;
    else
      start := n + 1;
    end;
  end;
  out
end function;

define function reverse-words-in-line (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let out = copy-sequence(s);
  let i = 0;
  while (i < n)
    if (word-byte?(element(s, i)))
      let start = i;
      i := i + 1;
      while (i < n & word-byte?(element(s, i)))
        i := i + 1;
      end;
      let j = 0;
      until (j = i - start)
        %byte-string-element-setter(element(s, i - j - 1), out, start + j);
        j := j + 1;
      end;
    else
      i := i + 1;
    end;
  end;
  out
end function;

define function reverse-words-in-byte-string (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let out = "";
  let start = 0;
  while (start <= n)
    let stop = start;
    while (stop < n & ~(element(s, stop) = 10))
      stop := stop + 1;
    end;
    out := concatenate(out, reverse-words-in-line(copy-sequence(s, start, stop)));
    if (stop < n)
      out := concatenate(out, "\n");
      start := stop + 1;
    else
      start := n + 1;
    end;
  end;
  out
end function;

define function mirror-line (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let i = 0;
  let out = "";
  while (i < n)
    if (word-byte?(element(s, i)))
      let start = i;
      i := i + 1;
      while (i < n & word-byte?(element(s, i)))
        i := i + 1;
      end;
      out := concatenate(reverse-byte-string(copy-sequence(s, start, i)), out);
    else
      let start = i;
      i := i + 1;
      while (i < n & ~(word-byte?(element(s, i))))
        i := i + 1;
      end;
      out := concatenate(copy-sequence(s, start, i), out);
    end;
  end;
  out
end function;

define function mirror-byte-string (s :: <byte-string>) => (out :: <byte-string>)
  let n = size(s);
  let out = "";
  let start = 0;
  while (start <= n)
    let stop = start;
    while (stop < n & ~(element(s, stop) = 10))
      stop := stop + 1;
    end;
    out := concatenate(out, mirror-line(copy-sequence(s, start, stop)));
    if (stop < n)
      out := concatenate(out, "\n");
      start := stop + 1;
    else
      start := n + 1;
    end;
  end;
  out
end function;

// ─── Line-indexing ────────────────────────────────────────────────────────

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
    rope-offset-to-line(rope-node-left(r), off)
  else
    rope-newlines(rope-node-left(r))
      + rope-offset-to-line(rope-node-right(r), off - w)
  end
end method;

// ─── Single pass: load file, run all 19 assertions ───────────────────────
//
// Returns 2221 (rope-line-count) if every assertion passes, 0 on any failure.

define function run-one-pass () => (<integer>)
  let content = %read-file("f:\\scratch\\sample-tall-wide.txt");
  let r = make-rope-from-string(content);

  // T1: size matches the known file size (86296 bytes, LF-only)
  if (~(rope-size(r) = 86296))
    0

  // T2: first byte is '/' (47) — the file starts with "//"
  elseif (~(rope-element(r, 0) = 47))
    0

  // T3: line count = 2220 LF bytes + 1 = 2221
  elseif (~(rope-line-count-via-string(r) = 2221))
    0

  // T4: offset of line 0 is always 0
  elseif (~(rope-line-to-offset(r, 0) = 0))
    0

  // T5: byte 0 is on line 0
  elseif (~(rope-offset-to-line(r, 0) = 0))
    0

  // T6: for-each-leaf sums to full file size
  elseif (begin
            let visited = 0;
            for-each-leaf(r, method (leaf-bytes)
                               visited := visited + size(leaf-bytes)
                             end);
            ~(visited = 86296)
          end)
    0

  // T7: rope-substring across leaf boundaries — 512 bytes at offset 512
  elseif (~(size(rope-substring(r, 512, 1024)) = 512))
    0

  // T8: concatenate r with itself → double size and double line count
  elseif (begin
            let r2 = rope-concatenate(r, r);
            ~(rope-size(r2) = 172592) | ~(rope-line-count-via-string(r2) = 4441)
          end)
    0

  // T9: split-at midpoint — halves sum back to original
  elseif (begin
            let sp = rope-split-at(r, 43148);
            ~(rope-size(head(sp)) + rope-size(tail(sp)) = 86296)
          end)
    0

  // T10: insert + delete round-trip preserves size
  elseif (begin
            let r3 = rope-insert(r, 1000, "ROUNDTRIP");
            let r4 = rope-delete(r3, 1000, 1009);
            ~(rope-size(r4) = 86296)
          end)
    0

  // T11: line-to-offset / offset-to-line round-trip on line 100
  elseif (begin
            let off100 = rope-line-to-offset(r, 100);
            ~(rope-offset-to-line(r, off100) = 100)
          end)
    0

  // T12: rope->string on a small insert produces correct length
  elseif (begin
            let snippet = rope-insert(make-rope-from-string("abcde"), 2, "XY");
            ~(size(rope->string(snippet)) = 7)
          end)
    0

  // T13: reverse a real 512-byte rope slice in Dylan and round-trip it
  elseif (begin
            let sample = rope-substring(r, 100, 612);
            let rev = reverse-byte-string(sample);
            let roundtrip = reverse-byte-string(rev);
            ~(size(rev) = 512)
              | ~(size(roundtrip) = 512)
              | ~(element(sample, 0) = element(rev, 511))
              | ~(element(sample, 511) = element(rev, 0))
              | ~(element(sample, 0) = element(roundtrip, 0))
              | ~(element(sample, 255) = element(roundtrip, 255))
              | ~(element(sample, 511) = element(roundtrip, 511))
          end)
    0

  // T14: reverse each line in a real multi-line slice and round-trip it
  elseif (begin
            let hi = rope-line-to-offset(r, 12);
            let sample = rope-substring(r, 0, hi);
            let rev = reverse-lines-in-byte-string(sample);
            let roundtrip = reverse-lines-in-byte-string(rev);
            ~(size(rev) = size(sample))
              | ~(count-newlines-in(rev) = count-newlines-in(sample))
              | ~(byte-string-equal?(roundtrip, sample))
          end)
    0

  // T15: reverse each word within each line in Dylan and round-trip it
  elseif (begin
            let hi = rope-line-to-offset(r, 20);
            let sample = rope-substring(r, 0, hi);
            let rev = reverse-words-in-byte-string(sample);
            let roundtrip = reverse-words-in-byte-string(rev);
            ~(size(rev) = size(sample))
              | ~(count-newlines-in(rev) = count-newlines-in(sample))
              | ~(byte-string-equal?(roundtrip, sample))
          end)
    0

  // T16: mirror-write each line in Dylan and round-trip it
  elseif (begin
            let hi = rope-line-to-offset(r, 24);
            let sample = rope-substring(r, 0, hi);
            let rev = mirror-byte-string(sample);
            let roundtrip = mirror-byte-string(rev);
            ~(size(rev) = size(sample))
              | ~(count-newlines-in(rev) = count-newlines-in(sample))
              | ~(byte-string-equal?(roundtrip, sample))
          end)
    0

  // T17: table-backed word frequency on a stitched rope with unique tokens
  elseif (begin
            let sample = make-rope-from-string(rope-substring(r, 0, 256));
            let stitched =
              rope-concatenate(
                make-rope-from-string("zzalpha qqbeta42 zzalpha\n"),
                rope-concatenate(
                  sample,
                  make-rope-from-string("\nmmomega zzalpha\n")));
            let words = word-frequency-table(rope->string(stitched));
            ~(table-count(words, "zzalpha") = 3)
              | ~(table-count(words, "qqbeta42") = 1)
              | ~(table-count(words, "mmomega") = 1)
          end)
    0

  // T18: report object over edited rope tracks bytes, lines, words, and byte diversity
  elseif (~edited-rope-report-ok?())
    0

  // T19: load multiple real scratch Dylan files and aggregate word/character frequencies
  else
    rope-line-count-via-string(r)
  end
end function run-one-pass;

// ─── Main: repeat run-one-pass 150 times to exercise GC pressure ─────────
//
// Returns 2221 if all 150 passes succeed; returns 0 on any failure.

define function main () => (<integer>)
  let i  = 0;
  let ok = #t;
  let multi-ok = multi-file-rope-report-ok?();
  while (i < 150 & ok)
    if (run-one-pass() = 0)
      ok := #f
    else
      #f
    end;
    i := i + 1;
  end;
  if (ok & multi-ok) 2221 else 0 end
end function main;
