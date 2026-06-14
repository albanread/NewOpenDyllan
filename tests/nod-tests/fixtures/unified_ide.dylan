Module: nod-ide
Precedence: c

// Sprint 41g — Save, Save As, Recent files submenu on top of Sprint 41e's
// File / Help menu bar.
//
// Sprint 41e shipped File → Open / Exit + Help → About. Sprint 41g extends
// the File menu so it now looks like:
//
//   File
//     Open...      (cmd-id 100)
//     Save         (cmd-id 101)
//     Save As...   (cmd-id 102)
//     ────────
//     Recent ▶                                  (NEW submenu)
//       1. F:\scratch\foo.dylan   (cmd-id 301)
//       2. F:\scratch\bar.txt     (cmd-id 302)
//       (etc., max 5 entries)
//     ────────
//     Exit         (cmd-id 199)
//   Help
//     About        (cmd-id 200)
//
// The window title shows the current file's basename, e.g.
// "foo.dylan - NewOpenDylan IDE".
//
// The recent-files list persists across runs in
// F:\scratch\nod-ide-recent.txt — one absolute path per line,
// most-recent first, capped at 5. Sprint 42a: persistence + dedup + cap
// is now pure Dylan (`nod-load-recent` / `nod-add-recent` /
// `nod-save-recent`) built on the byte-string ops landed in stdlib —
// the old `nod_load_recent` / `nod_add_recent` Rust shims are gone.
//
// IMPORTANT — the editor is still read-only (no cursor, no editing —
// that's Sprint 41h or later). Save in this sprint rewrites the file
// with its current in-memory contents. That's intentional: the plumbing
// (file picker, byte-string write, recent-list maintenance, title bar)
// is ready for when editing arrives.
//
// MessageBoxW-from-WNDPROC remains broken (Sprint 41f investigation,
// see docs/duim-research/07-probe-findings.md); Help → About still
// uses the SetWindowTextW workaround.

define c-function CreateWindowExW
  (dwExStyle :: <c-int>, lpClassName :: <c-pointer>, lpWindowName :: <c-wide-string>,
   dwStyle :: <c-int>, x :: <c-int>, y :: <c-int>, nWidth :: <c-int>, nHeight :: <c-int>,
   hWndParent :: <c-pointer>, hMenu :: <c-pointer>, hInstance :: <c-pointer>,
   lpParam :: <c-pointer>)
 => (hwnd :: <c-pointer>);
    library: "user32.dll";
end;

define c-function ShowWindow
  (hwnd :: <c-pointer>, nCmdShow :: <c-int>)
 => (was-visible :: <c-bool>);
    library: "user32.dll";
end;

define c-function UpdateWindow
  (hwnd :: <c-pointer>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function InvalidateRect
  (hwnd :: <c-pointer>, lpRect :: <c-pointer>, bErase :: <c-bool>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function DefWindowProcW
  (hwnd :: <c-pointer>, msg :: <c-int>,
   wparam :: <c-pointer>, lparam :: <c-pointer>)
 => (lresult :: <c-pointer>);
    library: "user32.dll";
end;

define c-function PostQuitMessage
  (exit-code :: <c-int>)
 => ();
    library: "user32.dll";
end;

// Sprint 41e — menu API declarations (explicit so the AppendMenuW
// 4th-arg lpNewItem stays `<c-wide-string>` for menu items; we pass
// the HMENU for popup submenus via the 3rd-arg `uIDNewItem` which is
// typed `<c-pointer>` to accept both fixnum ids and HMENU values).
define c-function CreateMenu
  ()
 => (hmenu :: <c-pointer>);
    library: "user32.dll";
end;

define c-function CreatePopupMenu
  ()
 => (hmenu :: <c-pointer>);
    library: "user32.dll";
end;

define c-function AppendMenuW
  (hmenu :: <c-pointer>, uFlags :: <c-int>, uIDNewItem :: <c-pointer>,
   lpNewItem :: <c-wide-string>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

// Sprint 41g — menu rebuild helpers. `RemoveMenu` with MF_BYPOSITION
// (1024) removes the item at the given index; positions shift after
// removal so calling with position 0 repeatedly tears the submenu
// down. `DrawMenuBar` forces the OS to repaint the menu bar after
// programmatic changes (the submenu's own popup is rebuilt on the
// next click so we don't have to invalidate it explicitly).
define c-function RemoveMenu
  (hmenu :: <c-pointer>, uPosition :: <c-int>, uFlags :: <c-int>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function DrawMenuBar
  (hwnd :: <c-pointer>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

// SetWindowTextW is the Help → About workaround (see Sprint 41e
// notes) and is also what we use for the per-file title.
define c-function SetWindowTextW
  (hwnd :: <c-pointer>, lpString :: <c-wide-string>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function MessageBoxW
  (hwnd :: <c-pointer>, lpText :: <c-wide-string>, lpCaption :: <c-wide-string>,
   uType :: <c-int>)
 => (result :: <c-int>);
    library: "user32.dll";
end;

// ─── Sprint 43d — inlined rope buffer ────────────────────────────────────
//
// The IDE's text buffer is a `<rope>` — a classical Boehm-style binary
// tree of `<byte-string>` chunks. Reads + edits are O(log n) instead
// of O(n) for the previous flat-`<byte-string>` buffer, which matters
// once the file is non-trivial (~256k+) and the user is editing.
//
// The full data structure is documented and tested standalone in
// `tests/nod-tests/fixtures/rope.dylan` (~650 lines + 24 self-tests
// passing under AOT). What's reproduced below is the production
// subset: classes, construction, read ops, concat, split, insert,
// delete, line indexing, and `rope->string` for serialisation. The
// self-test main and the deterministic test-buffer helpers stay in
// the standalone fixture.
//
// Why inlined and not imported: nod-driver builds one Dylan file per
// invocation today. Multi-file modules are a future sprint. Until
// then, the rope source is duplicated — kept in sync by hand. Any
// fix to the standalone fixture should be mirrored here (and
// vice-versa).

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

// ─── Sprint 42a — pure-Dylan helpers (replace retired Rust shims) ─────────
//
// All the IDE's text-buffer scanning and recent-files persistence is now
// pure Dylan over the Sprint 42a `<byte-string>` ops: `size`, `element`
// (the byte at an index), `concatenate`, `copy-sequence`, plus the
// Sprint 42a `=` method on `<byte-string>` (content equality). The five
// Rust shims (`nod_count_newlines`, `nod_max_line_chars`, `nod_basename`,
// `nod_load_recent`, `nod_add_recent`) are gone — this is the proof
// that the byte-string stdlib methods are usable for real work.
//
// Where we still call Rust:
//   * `%read-file` / `%write-file` — file I/O proper (Sprint 41b / 41g).
//   * `%byte-string-element` — primitive byte read (5-op surface).
//   * `pair` / `head` / `tail` / `empty?` / `nil` — list builtins
//     (Sprint 16; can't be specialised on `<byte-string>` yet — see the
//     `empty?` note in stdlib.dylan).
// Everything else here is plain Dylan calling the stdlib methods.

// ── Count newlines in a byte-string buffer ────────────────────────────────
// Returns 1 + the number of `\n` bytes (the line count, matching the
// "lines = newlines + 1" convention the IDE used pre-42a).

define function nod-count-newlines (s) => (lines)
  let n = size(s);
  let count = 1;
  let i = 0;
  until (i = n)
    if (element(s, i) = 10)        // 10 = '\n'
      count := count + 1;
    else
      #f
    end;
    i := i + 1;
  end;
  count
end function;

// ── Longest line length in bytes ──────────────────────────────────────────

define function nod-max-line-chars (s) => (best)
  let n = size(s);
  let best = 0;
  let cur = 0;
  let i = 0;
  until (i = n)
    if (element(s, i) = 10)        // 10 = '\n'
      if (cur > best) best := cur; else #f end;
      cur := 0;
    else
      cur := cur + 1;
    end;
    i := i + 1;
  end;
  if (cur > best) cur else best end
end function;

// ── basename(path) — last `\`-or-`/`-separated component ──────────────────
// Returns the empty string for nil or an empty byte-string.

define function nod-basename (path) => (base)
  if (empty?(path))               // empty?(nil) = #t for the list-builtin
    ""
  else
    let n = size(path);
    if (n = 0)
      path
    else
      // Scan for the LAST '\' (92) or '/' (47). sep-pos = -1 means
      // "no separator found, return path unchanged".
      let sep-pos = -1;
      let i = 0;
      until (i = n)
        let b = element(path, i);
        if (b = 92 | b = 47)
          sep-pos := i;
        else
          #f
        end;
        i := i + 1;
      end;
      if (sep-pos < 0)
        path
      else
        copy-sequence(path, sep-pos + 1, n)
      end
    end
  end
end function;

// ── List helpers — reverse, filter-out, take-first ────────────────────────
//
// Recent-files manipulation is built on a small kit of list ops. The
// stdlib's `do` / `map` / `reduce` exist but want a first-class function
// arg; for the simple loops we have, raw `head`/`tail`/`pair` walks are
// clearer.

define function nod-reverse-list (lst) => (rev)
  let result = nil();
  let cursor = lst;
  until (empty?(cursor))
    result := pair(head(cursor), result);
    cursor := tail(cursor);
  end;
  result
end function;

define function nod-remove-from-list (item, lst) => (filtered)
  // Walk lst, dropping any entry that equals `item` (byte-string `=`).
  // Build reversed, then flip — keeps the original order without an
  // append.
  let acc = nil();
  let cursor = lst;
  until (empty?(cursor))
    let p = head(cursor);
    if (p = item)
      #f   // skip — drop this entry
    else
      acc := pair(p, acc);
    end;
    cursor := tail(cursor);
  end;
  nod-reverse-list(acc)
end function;

define function nod-take-first (lst, n) => (taken)
  let acc = nil();
  let cursor = lst;
  let i = 0;
  until (empty?(cursor) | i = n)
    acc := pair(head(cursor), acc);
    cursor := tail(cursor);
    i := i + 1;
  end;
  nod-reverse-list(acc)
end function;

// ── Split a byte-string on `\n`, returning a list of byte-strings ─────────

define function nod-split-on-newline (bytes) => (lines)
  let n = size(bytes);
  if (n = 0)
    nil()
  else
    let acc = nil();
    let lo = 0;
    let i = 0;
    until (i = n)
      if (element(bytes, i) = 10)
        let line = copy-sequence(bytes, lo, i);
        acc := pair(line, acc);
        lo := i + 1;
      else
        #f
      end;
      i := i + 1;
    end;
    // Trailing segment (after the last '\n', or the only segment if
    // there's no '\n' at all). Skip if the buffer ended with a newline.
    if (lo < n)
      let tail-line = copy-sequence(bytes, lo, n);
      acc := pair(tail-line, acc);
    else
      #f
    end;
    nod-reverse-list(acc)
  end
end function;

// ── Join a list of byte-strings with `\n` ─────────────────────────────────

define function nod-join-with-newline (lst) => (joined)
  if (empty?(lst))
    ""
  else
    let cursor = lst;
    let result = head(cursor);
    cursor := tail(cursor);
    until (empty?(cursor))
      result := concatenate(result, "\n");
      result := concatenate(result, head(cursor));
      cursor := tail(cursor);
    end;
    result
  end
end function;

// ── Recent-files load / save / prepend-dedupe ─────────────────────────────
// Persists to F:\scratch\nod-ide-recent.txt — one path per line,
// most-recent first, capped at 5 entries. Missing/empty file → empty
// list. Write errors are silently ignored (best-effort persistence;
// the IDE keeps working, the user just loses the entry on next launch).

define function nod-load-recent () => (lst)
  let bytes = %read-file("F:\\scratch\\nod-ide-recent.txt");
  nod-split-on-newline(bytes)
end function;

define function nod-save-recent (recent-list) => ()
  let serialized = nod-join-with-newline(recent-list);
  %write-file("F:\\scratch\\nod-ide-recent.txt", serialized);
  #f
end function;

define function nod-add-recent (path, recent-list) => (new-list)
  // nil or empty-string path → no-op (treat as "nothing to remember").
  if (empty?(path))
    recent-list
  elseif (size(path) = 0)
    recent-list
  else
    let deduped  = nod-remove-from-list(path, recent-list);
    let prepended = pair(path, deduped);
    let capped    = nod-take-first(prepended, 5);
    nod-save-recent(capped);
    capped
  end
end function;

// ─── Helper: walk a recent-paths list, rebuild a submenu ────────────────
//
// Tears down every item in `recent-menu` (RemoveMenu at position 0
// while it returns success) and re-appends one MF_STRING entry per
// path. If the list is empty, appends a single disabled "(empty)" item
// (MF_GRAYED = 1) so the submenu is still visible to the user.
//
// Command ids are 301..305 — five slots for the five recent entries.
// Walking the spine with `pair`/`head`/`tail`/`empty?` is the standard
// Sprint 16 list-iteration pattern; the loop terminates either when
// the list is exhausted or when `i` reaches the 5-entry cap (defensive
// — `nod_add_recent` already caps at 5).

define function rebuild-recent-submenu (recent-menu, paths) => ()
  // Tear down whatever was there. RemoveMenu returns #t (BOOL true)
  // on success / #f when the position is out of range — that's our
  // natural loop guard.
  let removed = RemoveMenu(recent-menu, 0, 1024);
  until (~ removed)
    removed := RemoveMenu(recent-menu, 0, 1024);
  end;
  if (empty?(paths))
    // (empty) placeholder — disabled (MF_GRAYED = 1), no cmd-id.
    AppendMenuW(recent-menu, 1, 0, "(empty)");
  else
    let cursor = paths;
    let i = 0;
    until (empty?(cursor) | i > 4)
      let p = head(cursor);
      let label = nod-basename(p);
      AppendMenuW(recent-menu, 0, 301 + i, label);
      cursor := tail(cursor);
      i := i + 1;
    end;
  end;
end function;

// ─── Helper: set the window title to "basename - NewOpenDylan IDE" ──────
//
// If `path` is nil / empty, sets the title to the bare program name.
// Otherwise: "<basename> — NewOpenDylan IDE". Sprint 42a finally lets
// us build the title via `concatenate` (was held back in 41g for lack
// of string-concat on `<byte-string>`).

define function update-title (hwnd, path) => ()
  if (empty?(path))
    SetWindowTextW(hwnd, "NewOpenDylan IDE");
  else
    let base   = nod-basename(path);
    let suffix = concatenate(base, " - NewOpenDylan IDE");
    SetWindowTextW(hwnd, suffix);
  end;
end function;

// ─── Sprint 43e-2 — cursor movement helpers ──────────────────────────────
//
// Compute the byte offset that VK_UP / VK_DOWN should land the cursor on.
// `direction = -1` for up, `+1` for down. The math:
//
//   1. Find the start of the current line by scanning backward for '\n'.
//   2. Current column = offset - current-line-start.
//   3. Find the previous / next line's start + length.
//   4. Clamp column to fit on the target line.
//   5. Return target-line-start + clamped-column.
//
// At the top / bottom of the buffer the move is a no-op (returns the
// input offset unchanged). The caller checks for "no change" and skips
// the InvalidateRect when it's a no-op.
//
// Column behaviour: we recompute the column from the cursor's current
// byte position each time, so a long line → short line → long line
// vertical walk does NOT preserve the "ideal" column (the cursor sticks
// to whatever shorter intermediate line allowed). Most editors keep a
// remembered ideal column; we'll add that in a follow-up if it bothers.
//
// **Note on `|` and `&`.** Dylan's `|` and `&` short-circuit per
// spec; task #251 fixed our compiler to honour that (3-block CFG
// lowering in `nod-sema/src/lower.rs::lower_short_circuit`). The
// sentinel-loop helpers (`scan-line-start` / `scan-line-end`) below
// were originally written to dodge the eager-| bug; they are kept
// as-is because they read cleanly and the manual bounds-guard makes
// the scan invariant obvious. New code can use plain
// `until (i = 0 | element(bytes, i - 1) = 10)` style now.

define function scan-line-start
    (bytes :: <byte-string>, from :: <integer>) => (i :: <integer>)
  // Find the largest `i <= from` such that either `i = 0` or
  // `element(bytes, i - 1) = '\n'`.
  let i = from;
  let done = #f;
  until (done)
    if (i = 0)
      done := #t;
    elseif (element(bytes, i - 1) = 10)
      done := #t;
    else
      i := i - 1;
    end;
  end;
  i
end function;

define function scan-line-end
    (bytes :: <byte-string>, from :: <integer>, n :: <integer>)
 => (i :: <integer>)
  // Find the smallest `i >= from` such that either `i = n` or
  // `element(bytes, i) = '\n'`. `n` is `size(bytes)`, passed in by
  // the caller to avoid re-computing it.
  let i = from;
  let done = #f;
  until (done)
    if (i = n)
      done := #t;
    elseif (element(bytes, i) = 10)
      done := #t;
    else
      i := i + 1;
    end;
  end;
  i
end function;

define function move-cursor-vertical
    (bytes :: <byte-string>, offset :: <integer>, direction :: <integer>,
     ideal-col :: <integer>)
 => (new :: <integer>)
  // Sprint 43e-7 — `ideal-col` is the column the caller wants the
  // cursor restored to on the target line, clamped to the target
  // line's length. Letting the caller pass it (rather than us
  // recomputing from `offset`) is what makes a long → short → long
  // vertical walk preserve the original column.
  let n = size(bytes);
  let off = if (offset > n) n else offset end;
  let cur-line-start = scan-line-start(bytes, off);
  if (direction < 0)
    if (cur-line-start = 0)
      offset
    else
      let prev-line-end = cur-line-start - 1;     // index of the '\n'
      let prev-line-start = scan-line-start(bytes, prev-line-end);
      let prev-line-len = prev-line-end - prev-line-start;
      let target-col = if (ideal-col < prev-line-len) ideal-col else prev-line-len end;
      prev-line-start + target-col
    end
  else
    let cur-line-end = scan-line-end(bytes, off, n);
    if (cur-line-end = n)
      offset
    else
      let next-line-start = cur-line-end + 1;
      let next-line-end = scan-line-end(bytes, next-line-start, n);
      let next-line-len = next-line-end - next-line-start;
      let target-col = if (ideal-col < next-line-len) ideal-col else next-line-len end;
      next-line-start + target-col
    end
  end
end function;

// ─── Sprint 43f-1 — syntax-colouring helpers ─────────────────────────────
//
// Walk a byte-string buffer looking for runs of Dylan identifier
// characters. For each run, check against a hand-rolled keyword list.
// If a match, ask DirectWrite to apply the keyword brush to that text
// range via SetDrawingEffect.
//
// Identifier characters (Dylan's actual set is broader; this is the
// usable subset for keyword detection):
//   start:    [a-zA-Z]
//   continue: [a-zA-Z0-9_-?!]
//
// All operands of `|` here are pure byte-comparisons, so the eager-|
// compiler bug (task #251) doesn't bite.

define function is-ident-start? (b :: <integer>) => (yes? :: <boolean>)
  // ASCII A..Z (65..90) or a..z (97..122).
  (b >= 65 & b <= 90) | (b >= 97 & b <= 122)
end function;

define function is-ident-cont? (b :: <integer>) => (yes? :: <boolean>)
  // ident-start chars + digits 0..9 (48..57) + '-' (45) + '_' (95)
  // + '?' (63) + '!' (33).
  is-ident-start?(b) | (b >= 48 & b <= 57) | (b = 45) | (b = 95)
    | (b = 63) | (b = 33)
end function;

define function bytes-equal-string?
    (bytes :: <byte-string>, start :: <integer>, limit :: <integer>,
     kw :: <byte-string>) => (eq? :: <boolean>)
  let len = limit - start;
  if (len ~= size(kw))
    #f
  else
    // Sentinel-loop comparison. Task #251 fixed `|` to short-circuit
    // properly, so a `|`-condition `until` would also work now —
    // kept as-is for readability.
    let i = 0;
    let ok = #t;
    let done = #f;
    until (done)
      if (i = len)
        done := #t;
      elseif (element(bytes, start + i) ~= element(kw, i))
        ok := #f;
        done := #t;
      else
        i := i + 1;
      end;
    end;
    ok
  end
end function;

define function is-dylan-keyword?
    (bytes :: <byte-string>, start :: <integer>, limit :: <integer>)
 => (kw? :: <boolean>)
  let len = limit - start;
  if (len = 2)
    bytes-equal-string?(bytes, start, limit, "if")
      | bytes-equal-string?(bytes, start, limit, "or")
  elseif (len = 3)
    bytes-equal-string?(bytes, start, limit, "end")
      | bytes-equal-string?(bytes, start, limit, "let")
      | bytes-equal-string?(bytes, start, limit, "for")
      | bytes-equal-string?(bytes, start, limit, "and")
      | bytes-equal-string?(bytes, start, limit, "not")
      | bytes-equal-string?(bytes, start, limit, "use")
  elseif (len = 4)
    bytes-equal-string?(bytes, start, limit, "else")
      | bytes-equal-string?(bytes, start, limit, "when")
      | bytes-equal-string?(bytes, start, limit, "case")
      | bytes-equal-string?(bytes, start, limit, "slot")
      | bytes-equal-string?(bytes, start, limit, "from")
      | bytes-equal-string?(bytes, start, limit, "make")
  elseif (len = 5)
    bytes-equal-string?(bytes, start, limit, "while")
      | bytes-equal-string?(bytes, start, limit, "until")
      | bytes-equal-string?(bytes, start, limit, "begin")
      | bytes-equal-string?(bytes, start, limit, "local")
      | bytes-equal-string?(bytes, start, limit, "block")
      | bytes-equal-string?(bytes, start, limit, "macro")
      | bytes-equal-string?(bytes, start, limit, "class")
  elseif (len = 6)
    bytes-equal-string?(bytes, start, limit, "define")
      | bytes-equal-string?(bytes, start, limit, "method")
      | bytes-equal-string?(bytes, start, limit, "select")
      | bytes-equal-string?(bytes, start, limit, "elseif")
      | bytes-equal-string?(bytes, start, limit, "unless")
      | bytes-equal-string?(bytes, start, limit, "export")
      | bytes-equal-string?(bytes, start, limit, "module")
      | bytes-equal-string?(bytes, start, limit, "signal")
      | bytes-equal-string?(bytes, start, limit, "return")
  elseif (len = 7)
    bytes-equal-string?(bytes, start, limit, "library")
      | bytes-equal-string?(bytes, start, limit, "cleanup")
      | bytes-equal-string?(bytes, start, limit, "finally")
      | bytes-equal-string?(bytes, start, limit, "generic")
      | bytes-equal-string?(bytes, start, limit, "keyword")
  elseif (len = 8)
    bytes-equal-string?(bytes, start, limit, "constant")
      | bytes-equal-string?(bytes, start, limit, "function")
      | bytes-equal-string?(bytes, start, limit, "variable")
  elseif (len = 9)
    bytes-equal-string?(bytes, start, limit, "otherwise")
  else
    #f
  end
end function;

define function is-digit? (b :: <integer>) => (yes? :: <boolean>)
  b >= 48 & b <= 57
end function;

// Sprint 43f-4 — find a syntactically safe byte offset to seed the
// tokeniser from, by walking lines backward from the first visible
// line until we hit one beginning with "define ". Dylan top-level
// forms (`define class`, `define method`, `define function`,
// `define library`, `define module`, `define variable`, etc.) are
// self-contained — once we're between two top-level forms, the
// tokeniser state is known-clean (not in a comment, not in a
// string). Starting the scan there gives correct colouring no
// matter where the user scrolled.
//
// If no `define` line is found above (e.g. very top of file, or a
// `module:`/`library:` header file), fall back to offset 0.
//
// Why "starts with `define `" specifically:
//   - // line comments don't start with `define`
//   - /* block comments */ — if `define` appears INSIDE a block
//     comment, it would still need to be at column 0 of its line
//     for us to false-positive; that's a contrived enough case
//     to ignore. Real Dylan code never has top-level-aligned
//     `define ` inside a block comment.
//   - String literals — same argument.

define function find-safe-scan-start
    (bytes :: <byte-string>, source, first-visible-line :: <integer>)
 => (offset :: <integer>)
  let candidate = first-visible-line;
  let result = 0;
  let done = #f;
  let n = size(bytes);
  until (done)
    if (candidate <= 0)
      done := #t;
    else
      let off = rope-line-to-offset(source, candidate);
      // Need 7 bytes for "define " — keyword (6) plus space.
      if (off + 7 > n)
        candidate := candidate - 1;
      elseif (~ bytes-equal-string?(bytes, off, off + 6, "define"))
        candidate := candidate - 1;
      elseif (element(bytes, off + 6) ~= 32)  // ' '
        candidate := candidate - 1;
      else
        result := off;
        done := #t;
      end;
    end;
  end;
  result
end function;

// Sprint 43f-2 — full tokenising syntax-colouring pass. Walks the
// buffer recognising five token kinds:
//
//   * line comment   "// ... <newline>"          → comment-brush
//   * block comment  "/* ... */"                 → comment-brush
//   * string literal "\"...\""                   → string-brush
//   * number literal digits [+ . e E + -]        → number-brush
//   * class name     "<ident>"                   → class-brush
//   * identifier     ident chars                 → keyword-brush iff
//                                                  matches keyword list
//
// Anything else stays default (black). The scan is single-pass O(n)
// over `bytes`; per-paint cost is sub-millisecond for IDE-size files.
//
// All `&` / `|` chains used here have pure operands only (byte
// comparisons + arithmetic), so the eager-| compiler bug (task #251)
// is harmless. Two-byte lookahead (e.g. `//`, `/*`) uses nested if
// to defensively bounds-check before reading the second byte.

// Sprint 43f-3 — viewport-bounded scan. Callers pass [start, limit)
// instead of always tokenising the whole buffer; SetDrawingEffect
// positions are absolute (relative to the whole layout), so applying
// effects only to the visible byte range is sound. Off-screen tokens
// get no colour, and DirectWrite's render clip means the user can't
// see them anyway.
//
// For correctness across long block comments / strings that started
// well above the viewport, the caller is expected to back `start`
// up by a few lines of overscan. The tokeniser itself doesn't care
// where `start` is — it just walks bytes within [start, limit).

define function highlight-dylan-syntax
    (layout, bytes :: <byte-string>,
     start :: <integer>, limit :: <integer>,
     keyword-brush, comment-brush, string-brush,
     number-brush, class-brush) => ()
  let n = limit;
  let i = start;
  let done = #f;
  until (done)
    if (i >= n)
      done := #t;
    else
      let b = element(bytes, i);
      if (b = 47)              // '/' — comment lookahead
        if (i + 1 < n)
          let b2 = element(bytes, i + 1);
          if (b2 = 47)         // line comment "//..."
            let start = i;
            i := i + 2;
            let scan-done = #f;
            until (scan-done)
              if (i >= n)
                scan-done := #t;
              elseif (element(bytes, i) = 10)  // '\n'
                scan-done := #t;
              else
                i := i + 1;
              end;
            end;
            %dwrite-set-drawing-effect(layout, comment-brush, start, i - start);
          elseif (b2 = 42)     // block comment "/* ... */"
            let start = i;
            i := i + 2;
            let scan-done = #f;
            until (scan-done)
              if (i >= n)
                scan-done := #t;
              elseif (i + 1 < n & element(bytes, i) = 42 & element(bytes, i + 1) = 47)
                i := i + 2;
                scan-done := #t;
              else
                i := i + 1;
              end;
            end;
            %dwrite-set-drawing-effect(layout, comment-brush, start, i - start);
          else
            i := i + 1;
          end;
        else
          i := i + 1;
        end;
      elseif (b = 34)          // '"' — string literal
        let start = i;
        i := i + 1;
        let scan-done = #f;
        until (scan-done)
          if (i >= n)
            scan-done := #t;
          elseif (element(bytes, i) = 92 & i + 1 < n)  // '\\' escape
            i := i + 2;
          elseif (element(bytes, i) = 34)              // closing '"'
            i := i + 1;
            scan-done := #t;
          elseif (element(bytes, i) = 10)              // unterminated — stop at EOL
            scan-done := #t;
          else
            i := i + 1;
          end;
        end;
        %dwrite-set-drawing-effect(layout, string-brush, start, i - start);
      elseif (b = 60)          // '<' — class name "<ident>"
        if (i + 1 < n & is-ident-start?(element(bytes, i + 1)))
          let start = i;
          i := i + 2;
          let scan-done = #f;
          until (scan-done)
            if (i >= n)
              scan-done := #t;
            elseif (element(bytes, i) = 62)  // '>'
              i := i + 1;
              scan-done := #t;
            elseif (is-ident-cont?(element(bytes, i)))
              i := i + 1;
            else
              // Not a clean class name — bail without colouring.
              i := start + 1;
              scan-done := #t;
            end;
          end;
          // Only colour if we actually closed with '>'.
          if (i > start + 1 & element(bytes, i - 1) = 62)
            %dwrite-set-drawing-effect(layout, class-brush, start, i - start);
          else 0 end;
        else
          i := i + 1;
        end;
      elseif (is-digit?(b))    // number literal
        let start = i;
        i := i + 1;
        let scan-done = #f;
        until (scan-done)
          if (i >= n)
            scan-done := #t;
          else
            let c = element(bytes, i);
            if (is-digit?(c) | c = 46 | c = 101 | c = 69 | c = 43 | c = 45)
              i := i + 1;
            else
              scan-done := #t;
            end;
          end;
        end;
        %dwrite-set-drawing-effect(layout, number-brush, start, i - start);
      elseif (is-ident-start?(b))
        let start = i;
        i := i + 1;
        let scan-done = #f;
        until (scan-done)
          if (i >= n)
            scan-done := #t;
          elseif (is-ident-cont?(element(bytes, i)))
            i := i + 1;
          else
            scan-done := #t;
          end;
        end;
        if (is-dylan-keyword?(bytes, start, i))
          %dwrite-set-drawing-effect(layout, keyword-brush, start, i - start);
        else 0 end;
      else
        i := i + 1;
      end;
    end;
  end;
end function;

// ─── Sprint 43g — gutter helpers (line numbers) ──────────────────────────

// Render a non-negative integer to a decimal byte-string. Uses
// repeated /10 + mod (`n - (n / 10) * 10`) to extract digits.
define function integer-to-string (n :: <integer>) => (s :: <byte-string>)
  if (n = 0)
    "0"
  else
    // Count digits.
    let m = n;
    let digits = 0;
    until (m = 0)
      digits := digits + 1;
      m := m / 10;
    end;
    // Write digits right-to-left.
    let s = %byte-string-allocate(digits);
    let m = n;
    let i = digits - 1;
    let done = #f;
    until (done)
      if (i < 0)
        done := #t;
      else
        let d = m - (m / 10) * 10;     // m mod 10
        %byte-string-element-setter(48 + d, s, i);   // '0' + digit
        m := m / 10;
        i := i - 1;
      end;
    end;
    s
  end
end function;

// Build a multi-line right-padded line-numbers string covering the
// inclusive range [first .. last], each number padded to `width`
// characters. Lines joined with '\n'. Caller hands the result to
// DirectWrite as the text of a layout sized to width × line-height.
//
// Right-alignment is achieved with leading spaces — the font is
// monospaced so visual alignment is exact without needing DirectWrite's
// SetTextAlignment shim.

define function build-line-numbers-block
    (from-line :: <integer>, to-line :: <integer>, width :: <integer>)
 => (out :: <byte-string>)
  let acc = "";
  let i = from-line;
  let done = #f;
  until (done)
    if (i > to-line)
      done := #t;
    else
      let n-str = integer-to-string(i);
      let pad-count = width - size(n-str);
      let padded = if (pad-count <= 0)
                     n-str
                   else
                     let p = %byte-string-allocate(pad-count);
                     let k = 0;
                     until (k = pad-count)
                       %byte-string-element-setter(32, p, k);   // ' '
                       k := k + 1;
                     end;
                     concatenate(p, n-str)
                   end;
      acc := if (i = from-line)
               padded
             else
               concatenate(acc, concatenate("\n", padded))
             end;
      i := i + 1;
    end;
  end;
  acc
end function;

define function main () => ()
  let arg-path = %argv1();
  // Sprint 43d — the buffer is a `<rope>` now. Load the file (or the
  // no-file placeholder) into a flat byte-string, then wrap it via
  // make-rope-from-string so every later read / edit goes through
  // the rope's O(log n) ops.
  let initial-bytes = if (empty?(arg-path))
                        "nod-ide: no argv[1] supplied; pass a Dylan source path as the first argument."
                      else
                        let bytes = %read-file(arg-path);
                        if (empty?(bytes))
                          "nod-ide: could not read the file passed via argv[1]."
                        else
                          bytes
                        end
                      end;
  let source-text = make-rope-from-string(initial-bytes);
  // Sprint 43d hotfix — cache the serialised flat-string view of the
  // rope across WM_PAINT calls. The old byte-string buffer cost zero
  // allocation per paint (the byte-string Word was passed straight
  // to DirectWrite); the rope serialisation costs O(n) per paint.
  // Win32 sends WM_PAINT on InvalidateRect, focus changes, drags,
  // etc., so caching is the difference between "stable IDE" and
  // "GC pressure crash". Invalidate the cache (set to "") at every
  // mutation site — Open, Recent, WM_CHAR, VK_BACK, Save-As reload.
  let cached-flat = initial-bytes;
  // Sprint 43d — cursor-offset is the byte position where the next
  // WM_CHAR insertion lands (and what backspace removes the byte
  // before). Captured by the WNDPROC closure → auto-promoted to a
  // cell. Sprint 43e will surface this as a visible blinking
  // caret + click-to-position; for now it tracks invisibly so we
  // can prove insert/delete plumbing works end-to-end.
  let cursor-offset = 0;
  // Sprint 43e-6 — blinking cursor. SetTimer(500ms) toggles this
  // cell on every WM_TIMER; WM_PAINT only draws the cursor bar
  // when it's 1. Each cursor-mutating handler resets it to 1 so
  // the cursor stays solid during active typing/movement.
  let cursor-on = 1;
  // Sprint 43e-7 — ideal-column memory. Every horizontal cursor
  // move (left/right/home/end/click/typing/backspace) updates this
  // to the cursor's current column. Vertical moves (up/down/pgup/
  // pgdn) pass it to move-cursor-vertical so the cursor restores
  // to the original column when a long → short → long walk crosses
  // a shorter intermediate line.
  let ideal-col = 0;
  // Sprint 43e-8 — track Ctrl modifier state manually. WM_KEYDOWN
  // for VK_CONTROL (17) sets this to 1; WM_KEYUP (msg 257) for
  // VK_CONTROL clears it to 0. Avoids needing GetKeyState (which
  // isn't currently in the Win32 projection for unknown reasons
  // — investigate separately as a follow-up).
  let ctrl-down = 0;
  // Sprint 41g — current-path is a captured cell (Sprint 24 auto cell
  // promotion: any `let`-bound name assigned inside the WNDPROC
  // closure becomes a cell). Same machinery that promoted source-text
  // in Sprint 41e.
  let current-path = arg-path;
  let recent-paths = nod-load-recent();
  let d3d-device   = %d3d11-create-device();
  let dxgi-factory = %dxgi-factory-from-d3d-device(d3d-device);
  let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device);
  let d2d-factory  = %d2d-create-factory();
  let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device);
  let dc           = %d2d-create-device-context(d2d-device);
  let dwrite       = %dwrite-create-factory();
  let format       = %dwrite-create-text-format(dwrite, "Consolas", 1400, "en-us");
  // Sprint 43g-fix — force uniform 18.0 DIP line height with baseline
  // at 14.4 DIPs so DirectWrite's line stride matches the Dylan-side
  // line-height constant exactly. Without this, Consolas's natural
  // line height (~17 px at 14 DIPs) produces a cumulative 1 px/line
  // drift that makes the gutter's line numbers visibly slide up/down
  // by one relative to the source text as the user scrolls.
  // Encoding: x10 tag — 180 means 18.0 DIPs, 144 means 14.4 DIPs.
  %dwrite-set-line-spacing(format, 180, 144);
  let buffer-lines    = nod-rope-line-count(source-text);
  let buffer-max-cols = nod-rope-max-line-chars(source-text);
  let char-width  = 8;
  let line-height = 18;
  let pad = 8;
  // Sprint 43g — left gutter. Three columns reserved:
  //   * fold-gutter   — placeholder for collapse/expand triangles
  //   * error-gutter  — placeholder for diagnostic markers
  //   * line-num-gutter — visible 1-based line numbers (functional)
  // total-gutter-px is the width added BEFORE pad on the left side
  // of the source-text viewport. Everywhere the text used to be
  // drawn at x = pad - scroll-x-px, it now uses x = gutter-px + pad
  // - scroll-x-px. Click handler subtracts gutter-px from the
  // client X before computing the buffer position; clicks landing
  // inside the gutter are ignored (no cursor move).
  let fold-gutter-px     = 14;
  let error-gutter-px    = 14;
  let line-num-gutter-px = 40;     // fits 5 monospace digits at 8 px each
  let gutter-px = fold-gutter-px + error-gutter-px + line-num-gutter-px;
  let client-width-px  = buffer-max-cols * char-width;
  let client-height-px = buffer-lines * line-height;
  let window-width    = 1024;
  let window-height   = 768;
  let viewport-width-px  = 1024;
  let viewport-height-px = 768;
  let scroll-x-px = 0;
  let scroll-y-px = 0;
  let swap   = 0;
  let bitmap = 0;
  // Sprint 41g — build the menu bar HERE (before the WNDPROC closure
  // captures `recent-menu`) so the WM_COMMAND handler can call
  // `rebuild-recent-submenu` on `recent-menu` when the recent list
  // changes.
  let menu-bar = CreateMenu();
  let file-menu = CreatePopupMenu();
  let recent-menu = CreatePopupMenu();
  // AppendMenuW flag values (Win32 MF_*):
  //   MF_STRING    = 0      — plain text item (default)
  //   MF_GRAYED    = 1      — disabled / greyed
  //   MF_POPUP     = 16     — uIDNewItem is a submenu HMENU
  //   MF_SEPARATOR = 2048   — horizontal divider (lpNewItem ignored)
  AppendMenuW(file-menu, 0,    100, "&Open...\tCtrl+O");
  AppendMenuW(file-menu, 0,    101, "&Save\tCtrl+S");
  AppendMenuW(file-menu, 0,    102, "Save &As...\tCtrl+Shift+S");
  AppendMenuW(file-menu, 2048, 0,   "");
  AppendMenuW(file-menu, 16,   recent-menu, "&Recent");
  AppendMenuW(file-menu, 2048, 0,   "");
  AppendMenuW(file-menu, 0,    199, "E&xit\tAlt+F4");
  AppendMenuW(menu-bar,  16,   file-menu, "&File");
  let help-menu = CreatePopupMenu();
  AppendMenuW(help-menu, 0,    200, "&About");
  AppendMenuW(menu-bar,  16,   help-menu, "&Help");
  rebuild-recent-submenu(recent-menu, recent-paths);
  // Sprint 43e-4 — auto-scroll-to-cursor helper.
  //
  // Called by every cursor-mutating handler (arrow keys, Home/End,
  // WM_CHAR, VK_BACK). Computes the cursor's pixel position in
  // buffer coordinates, then nudges scroll-x-px / scroll-y-px so the
  // cursor sits inside the viewport. If the cursor is already
  // visible the scrolls are left alone; if it's off the left/right
  // edge, scrolls horizontally; if off the top/bottom, scrolls
  // vertically.
  //
  // Reads cached-flat, cursor-offset, scroll-{x,y}-px, viewport-
  // {width,height}-px, client-{width,height}-px, char-width, line-
  // height; mutates scroll-{x,y}-px; calls %set-scroll-info if a
  // scroll changed. Closes over main()'s lexical scope so callers
  // pass only the HWND.
  //
  // Line-number computation walks cached-flat once per call. O(n)
  // per cursor move where n is the byte distance from start of
  // buffer to the cursor. Sub-millisecond for typical files; rope-
  // aware line lookup is a follow-up if it ever matters.
  // Sprint 43e-7 — record the cursor's current column as the new
  // ideal-col. Called by horizontal moves (left/right/home/end/
  // click/typing/backspace); vertical moves (up/down/pgup/pgdn)
  // skip this so the ideal column survives the vertical walk.
  let update-ideal-col = method ()
    let ls = scan-line-start(cached-flat, cursor-offset);
    ideal-col := cursor-offset - ls;
    0
  end;
  let ensure-cursor-visible = method (hwnd)
    // Sprint 43e-6 — reset the blink phase. Any caller that moved
    // the cursor wants it visibly solid for the next ~500 ms;
    // otherwise the bar flickers mid-keystroke.
    cursor-on := 1;
    let bytes = cached-flat;
    let cur = cursor-offset;
    let line-start = scan-line-start(bytes, cur);
    let col = cur - line-start;
    // Count newlines in bytes[0 .. line-start) → line index.
    let line = 0;
    let i = 0;
    until (i = line-start)
      if (element(bytes, i) = 10) line := line + 1; else 0 end;
      i := i + 1;
    end;
    let cx = col * char-width;
    let cy = line * line-height;
    // Desired scroll positions: closest to current that keeps the
    // cursor's char-width × line-height rect inside the viewport.
    let new-sx = scroll-x-px;
    if (cx < new-sx)
      new-sx := cx;
    elseif (cx + char-width > new-sx + viewport-width-px)
      new-sx := cx + char-width - viewport-width-px;
    else 0 end;
    let new-sy = scroll-y-px;
    if (cy < new-sy)
      new-sy := cy;
    elseif (cy + line-height > new-sy + viewport-height-px)
      new-sy := cy + line-height - viewport-height-px;
    else 0 end;
    // Clamp to [0, max] for each axis. Negative scroll would draw
    // the buffer past the pad; over-max would draw past the buffer.
    let h-max = if (client-width-px > viewport-width-px)
                  client-width-px - viewport-width-px
                else 0 end;
    let v-max = if (client-height-px > viewport-height-px)
                  client-height-px - viewport-height-px
                else 0 end;
    if (new-sx < 0) new-sx := 0 else 0 end;
    if (new-sx > h-max) new-sx := h-max else 0 end;
    if (new-sy < 0) new-sy := 0 else 0 end;
    if (new-sy > v-max) new-sy := v-max else 0 end;
    if (new-sx ~= scroll-x-px)
      scroll-x-px := new-sx;
      %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, new-sx, 1);
    else 0 end;
    if (new-sy ~= scroll-y-px)
      scroll-y-px := new-sy;
      %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, new-sy, 1);
    else 0 end;
    0
  end;
  // Sprint 11d — split the WNDPROC into two parts:
  //
  //   `handle-wm-message`: a regular Dylan function. Allowed to
  //     allocate freely; runs from a precisely-tracked Dylan frame so
  //     Sprint 11b's safepoint-root machinery sees every live Word
  //     across each allocating call.
  //
  //   `wp`: the OS-facing callback shell. Does no allocation. The only
  //     thing it does is forward to `handle-wm-message`. The Rust
  //     trampoline in nod-runtime/src/callbacks.rs::wndproc_dispatch
  //     calls into `wp` via `nod_funcall4`; `wp` makes exactly one
  //     Dylan-to-Dylan call into `handle-wm-message`, which is wrapped
  //     by 11b's begin_safepoint / end_safepoint pair.
  //
  // The rule: never let the registered callback closure itself
  // allocate. Win32 callbacks aren't part of the normal Dylan call
  // flow — they're re-entered out of a native frame the GC can't
  // describe — so we keep the work in a separate frame that *is*
  // part of the normal flow. Long-term, a `define-window-class` macro
  // generates this shell shape; for now we hand-wire it.
  let handle-wm-message = method (hwnd, msg, wparam, lparam)
             if (msg = 15)  // WM_PAINT
               if (swap ~= 0)
                 if (bitmap = 0)
                   bitmap := %d2d-create-bitmap-from-swap-chain(dc, swap);
                 else 0 end;
                 %d2d-set-target(dc, bitmap);
                 %d2d-begin-draw(dc);
                 %d2d-clear(dc, 255, 255, 255, 255);
                 let brush  = %d2d-create-solid-color-brush(dc, 0, 0, 0, 255);
                 // Sprint 43g — gutter brushes.
                 //   gutter-bg-brush:   light grey background fill.
                 //   gutter-text-brush: medium grey for line numbers.
                 //   gutter-edge-brush: darker grey for the 1px
                 //                      right-edge separator line.
                 let gutter-bg-brush   = %d2d-create-solid-color-brush(dc, 240, 240, 240, 255);
                 let gutter-text-brush = %d2d-create-solid-color-brush(dc, 130, 130, 130, 255);
                 let gutter-edge-brush = %d2d-create-solid-color-brush(dc, 200, 200, 200, 255);
                 // Sprint 43f-1 / 43f-2 — syntax-colour brushes.
                 //   keyword: medium blue (define, end, method, …)
                 //   comment: muted green (// and /* */)
                 //   string:  brick red ("...")
                 //   number:  purple (literals)
                 //   class:   teal (<foo>, <byte-string>, …)
                 // Picked from common editor palettes; colours are
                 // distinguishable on both light and dark text without
                 // overpowering the default black.
                 let keyword-brush = %d2d-create-solid-color-brush(dc, 30, 90, 200, 255);
                 let comment-brush = %d2d-create-solid-color-brush(dc, 30, 130, 50, 255);
                 let string-brush  = %d2d-create-solid-color-brush(dc, 170, 50, 30, 255);
                 let number-brush  = %d2d-create-solid-color-brush(dc, 130, 50, 170, 255);
                 let class-brush   = %d2d-create-solid-color-brush(dc, 20, 130, 140, 255);
                 // Sprint 43d hotfix — `cached-flat` is refreshed at every
                 // mutation; WM_PAINT just reuses it. Before caching
                 // we were paying an O(n) byte-string allocation per
                 // paint, which under Win32's WM_PAINT cadence (drags,
                 // focus changes, scrolls) outran the GC's destination
                 // generation and tripped GcStallError::mid_evac_oom.
                 let layout = %dwrite-create-text-layout(dwrite, cached-flat, format,
                                                         client-width-px, client-height-px);
                 // Sprint 43f-5 — colour the WHOLE buffer, let DirectWrite
                 // clip rendering to the visible region.
                 //
                 // 43f-3 (fixed overscan) and 43f-4 (scan back to the
                 // previous `define ...`) both tried to bound the
                 // tokeniser to the visible region for performance.
                 // Both produced visible artefacts at scroll boundaries:
                 // 43f-3 mis-coloured when block comments spanned more
                 // than `overscan` lines above the viewport; 43f-4
                 // mis-coloured when editing non-Dylan files (Rust,
                 // markdown, anything without a column-0 `define `).
                 //
                 // The robust shape, per the user's instinct: think of
                 // the window as a VIEW into a backing buffer that's
                 // wholly tokenised. Per-paint cost on IDE-sized files
                 // (~30 KB) is sub-millisecond; on much larger files
                 // we can revisit with a per-line tokeniser-state cache
                 // (compute on edit, lookup on paint).
                 //
                 // SetDrawingEffect calls for off-screen ranges store
                 // metadata in the layout without painting; DirectWrite
                 // clips the actual glyph rendering to the layout box
                 // intersected with the render target — so the GPU
                 // work is still bounded by the viewport.
                 let scan-start = 0;
                 let scan-end = size(cached-flat);
                 highlight-dylan-syntax(layout, cached-flat,
                                        scan-start, scan-end,
                                        keyword-brush, comment-brush,
                                        string-brush, number-brush,
                                        class-brush);
                 // Sprint 43g — text-layout origin shifts right by
                 // `gutter-px` so the gutter columns get the left
                 // strip of the viewport.
                 %d2d-draw-text-layout(dc, gutter-px + pad - scroll-x-px, pad - scroll-y-px, layout, brush);
                 // Sprint 43e-1 (revised) — visible cursor via
                 // DirectWrite hit-testing. Ask the text layout we
                 // just drew where the cursor offset lives in pixels;
                 // this is exact, no matter the font / size / kerning.
                 //
                 // %dwrite-hit-test-position returns a packed u64 with
                 // y-pixels in the high 32 bits and x-pixels in the
                 // low 32 bits, relative to the layout origin (which
                 // we passed as `pad - scroll-x-px, pad - scroll-y-px`
                 // above). Trailing-edge flag = 0 → leading edge of
                 // the character AT cursor-offset, i.e. cursor BEFORE
                 // that character. cursor-offset clamped to flat-len
                 // so an EOF cursor still hit-tests cleanly.
                 let cur-off = cursor-offset;
                 let flat-len = size(cached-flat);
                 let hit-pos = if (cur-off < flat-len) cur-off else flat-len end;
                 let packed = %dwrite-hit-test-position(layout, hit-pos, 0);
                 // Bit 31 might be set in the low 32 bits for large x;
                 // we'll mask defensively. Use mod by 2^32 for the low
                 // half and integer div for the high half.
                 let two-to-32 = 4294967296;
                 let hx = packed - (packed / two-to-32) * two-to-32;
                 let hy = packed / two-to-32;
                 let cx = gutter-px + pad - scroll-x-px + hx;
                 let cy = pad - scroll-y-px + hy;
                 // Sprint 43e-6 — blink: only draw when cursor-on = 1.
                 // Bar is 3px wide (was 2px) for visibility — at 1Hz
                 // blink the eye latches the off-state better with a
                 // slightly thicker beam.
                 if (cursor-on = 1)
                   %d2d-fill-rectangle(dc, cx, cy, cx + 3, cy + line-height, brush);
                 else 0 end;
                 // Sprint 43g — gutter rendering. Drawn AFTER the
                 // text so the gutter sits on top, hiding any text
                 // that might've been horizontally-scrolled into
                 // negative x territory (text origin = gutter-px +
                 // pad - scroll-x-px; for scroll-x-px > pad the
                 // text could otherwise bleed under the gutter).
                 //
                 //   1. Fill the gutter background (light grey).
                 //   2. Draw a 1px darker separator at the right edge.
                 //   3. Build a multi-line line-numbers string for
                 //      the visible range + small overscan.
                 //   4. Create a temporary text layout sized to the
                 //      line-num gutter column; draw it at the
                 //      line-num gutter origin.
                 %d2d-fill-rectangle(dc, 0, 0, gutter-px, viewport-height-px, gutter-bg-brush);
                 %d2d-fill-rectangle(dc, gutter-px - 1, 0, gutter-px, viewport-height-px, gutter-edge-brush);
                 let first-visible-line = scroll-y-px / line-height;
                 let lines-on-screen = viewport-height-px / line-height + 1;
                 let total-lines = buffer-lines;
                 let last-line-uncapped = first-visible-line + lines-on-screen;
                 let last-line = if (last-line-uncapped < total-lines)
                                   last-line-uncapped
                                 else total-lines end;
                 let ln-block = build-line-numbers-block(first-visible-line + 1, last-line, 5);
                 let ln-layout = %dwrite-create-text-layout(dwrite, ln-block, format,
                                                            line-num-gutter-px, viewport-height-px);
                 let ln-origin-x = fold-gutter-px + error-gutter-px;
                 let ln-origin-y = pad + first-visible-line * line-height - scroll-y-px;
                 %d2d-draw-text-layout(dc, ln-origin-x, ln-origin-y, ln-layout, gutter-text-brush);
                 %com-release(ln-layout);
                 %d2d-end-draw(dc);
                 %com-release(brush);
                 %com-release(keyword-brush);
                 %com-release(comment-brush);
                 %com-release(string-brush);
                 %com-release(number-brush);
                 %com-release(class-brush);
                 %com-release(gutter-bg-brush);
                 %com-release(gutter-text-brush);
                 %com-release(gutter-edge-brush);
                 %com-release(layout);
                 %dxgi-swap-chain-present(swap);
               else 0 end;
               0
             elseif (msg = 275)  // WM_TIMER — Sprint 43e-6 cursor blink
               // Toggle the blink state. Phrased as explicit if /
               // else := 0 / := 1 (rather than `cursor-on := if (...)
               // 0 else 1 end`) because the latter occasionally
               // didn't visibly blink during testing — possibly a
               // Dylan-side eval quirk, possibly just hard to see at
               // 500ms with a 2px bar. This form makes the toggle
               // unambiguous from the compiler's POV.
               if (cursor-on = 1)
                 cursor-on := 0;
               else
                 cursor-on := 1;
               end;
               InvalidateRect(hwnd, 0, 0);
               0
             elseif (msg = 5)  // WM_SIZE
               if (swap ~= 0 & wparam ~= 1)
                 let new-w = %lo-word(lparam);
                 let new-h = %hi-word(lparam);
                 if (new-w > 0 & new-h > 0)
                   if (bitmap ~= 0)
                     %d2d-set-target(dc, 0);
                     %com-release(bitmap);
                     bitmap := 0;
                   else 0 end;
                   window-width  := new-w;
                   window-height := new-h;
                   viewport-width-px  := new-w;
                   viewport-height-px := new-h;
                   %dxgi-swap-chain-resize-buffers(swap, new-w, new-h);
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                   %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                 else 0 end;
               else 0 end;
               0
             elseif (msg = 277)  // WM_VSCROLL
               let action = %lo-word(wparam);
               let new-pos = if (action = 0)        // SB_LINEUP
                               scroll-y-px - line-height
                             elseif (action = 1)    // SB_LINEDOWN
                               scroll-y-px + line-height
                             elseif (action = 2)    // SB_PAGEUP
                               scroll-y-px - (viewport-height-px - line-height)
                             elseif (action = 3)    // SB_PAGEDOWN
                               scroll-y-px + (viewport-height-px - line-height)
                             elseif (action = 4)    // SB_THUMBPOSITION
                               %hi-word(wparam)
                             elseif (action = 5)    // SB_THUMBTRACK
                               %hi-word(wparam)
                             elseif (action = 6)    // SB_TOP (Home)
                               0
                             elseif (action = 7)    // SB_BOTTOM (End)
                               client-height-px - viewport-height-px
                             else
                               scroll-y-px           // SB_ENDSCROLL / unknown
                             end;
               let max-scroll = if (client-height-px > viewport-height-px)
                                  client-height-px - viewport-height-px
                                else 0 end;
               let clamped = if (new-pos < 0) 0
                             elseif (new-pos > max-scroll) max-scroll
                             else new-pos end;
               if (clamped ~= scroll-y-px)
                 scroll-y-px := clamped;
                 %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, clamped, 1);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 276)  // WM_HSCROLL
               let action = %lo-word(wparam);
               let new-pos = if (action = 0)        // SB_LINELEFT
                               scroll-x-px - char-width
                             elseif (action = 1)    // SB_LINERIGHT
                               scroll-x-px + char-width
                             elseif (action = 2)    // SB_PAGELEFT
                               scroll-x-px - (viewport-width-px - char-width)
                             elseif (action = 3)    // SB_PAGERIGHT
                               scroll-x-px + (viewport-width-px - char-width)
                             elseif (action = 4)    // SB_THUMBPOSITION
                               %hi-word(wparam)
                             elseif (action = 5)    // SB_THUMBTRACK
                               %hi-word(wparam)
                             elseif (action = 6)    // SB_LEFT
                               0
                             elseif (action = 7)    // SB_RIGHT
                               client-width-px - viewport-width-px
                             else
                               scroll-x-px
                             end;
               let max-scroll = if (client-width-px > viewport-width-px)
                                  client-width-px - viewport-width-px
                                else 0 end;
               let clamped = if (new-pos < 0) 0
                             elseif (new-pos > max-scroll) max-scroll
                             else new-pos end;
               if (clamped ~= scroll-x-px)
                 scroll-x-px := clamped;
                 %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, clamped, 1);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 522)  // WM_MOUSEWHEEL
               let raw-delta = %hi-word(wparam);
               let signed-delta = if (raw-delta > 32767)
                                    raw-delta - 65536
                                  else
                                    raw-delta
                                  end;
               let flags = %lo-word(wparam);
               let shift-bit = (flags / 4) - (flags / 8) * 2;
               if (shift-bit = 1)
                 let chars-to-scroll = -1 * signed-delta * 3 / 120;
                 let new-pos = scroll-x-px + chars-to-scroll * char-width;
                 let max-scroll = if (client-width-px > viewport-width-px)
                                    client-width-px - viewport-width-px
                                  else 0 end;
                 let clamped = if (new-pos < 0) 0
                               elseif (new-pos > max-scroll) max-scroll
                               else new-pos end;
                 if (clamped ~= scroll-x-px)
                   scroll-x-px := clamped;
                   %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, clamped, 1);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               else
                 let lines-to-scroll = -1 * signed-delta * 3 / 120;
                 let new-pos = scroll-y-px + lines-to-scroll * line-height;
                 let max-scroll = if (client-height-px > viewport-height-px)
                                    client-height-px - viewport-height-px
                                  else 0 end;
                 let clamped = if (new-pos < 0) 0
                               elseif (new-pos > max-scroll) max-scroll
                               else new-pos end;
                 if (clamped ~= scroll-y-px)
                   scroll-y-px := clamped;
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, clamped, 1);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               end;
               0
             elseif (msg = 257)  // WM_KEYUP — Sprint 43e-8 track Ctrl release
               let vk = %lo-word(wparam);
               if (vk = 17)        // VK_CONTROL
                 ctrl-down := 0;
               else 0 end;
               0
             elseif (msg = 256)  // WM_KEYDOWN
               let vk = %lo-word(wparam);
               if (vk = 17)        // Sprint 43e-8 — track Ctrl press
                 ctrl-down := 1;
               else 0 end;
               let v-max = if (client-height-px > viewport-height-px)
                             client-height-px - viewport-height-px
                           else 0 end;
               let h-max = if (client-width-px > viewport-width-px)
                             client-width-px - viewport-width-px
                           else 0 end;
               if (vk = 33)        // VK_PRIOR (PgUp) — Sprint 43e-4b cursor move
                 // Move the cursor up by one screenful of lines. The
                 // ensure-cursor-visible call then pulls the viewport
                 // along so the cursor stays on screen. Walk one line
                 // at a time via move-cursor-vertical — simple but
                 // O(page) cached-flat walks per press; optimisation
                 // (single-pass walk preserving the ideal column) is a
                 // follow-up if PgUp/PgDn ever feels sluggish.
                 let lines-per-page = viewport-height-px / line-height;
                 let new-off = cursor-offset;
                 let i = 0;
                 until (i = lines-per-page)
                   new-off := move-cursor-vertical(cached-flat, new-off, -1, ideal-col);
                   i := i + 1;
                 end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 34)    // VK_NEXT (PgDn) — Sprint 43e-4b cursor move
                 let lines-per-page = viewport-height-px / line-height;
                 let new-off = cursor-offset;
                 let i = 0;
                 until (i = lines-per-page)
                   new-off := move-cursor-vertical(cached-flat, new-off, 1, ideal-col);
                   i := i + 1;
                 end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 36)    // VK_HOME — Sprint 43e-3 / 43e-8
                 // Plain HOME → start of current line.
                 // Ctrl+HOME → start of buffer (offset 0).
                 // Modifier state from the `ctrl-down` cell that
                 // VK_CONTROL up/down events maintain.
                 let new-off = if (ctrl-down = 1) 0
                               else scan-line-start(cached-flat, cursor-offset) end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 35)    // VK_END — Sprint 43e-3 / 43e-8
                 // Plain END → end of current line.
                 // Ctrl+END → end of buffer (size(cached-flat)).
                 let new-off = if (ctrl-down = 1) size(cached-flat)
                               else scan-line-end(cached-flat, cursor-offset, size(cached-flat)) end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 37)    // VK_LEFT — Sprint 43e-2 cursor move
                 // Rebound from horizontal scroll to cursor move per the
                 // universal text-editor convention. Horizontal scroll
                 // stays available via Shift+MouseWheel, the horizontal
                 // scrollbar, and (if we add them) Ctrl+arrows.
                 if (cursor-offset > 0)
                   cursor-offset := cursor-offset - 1;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 39)    // VK_RIGHT — Sprint 43e-2 cursor move
                 let buf-len = size(cached-flat);
                 if (cursor-offset < buf-len)
                   cursor-offset := cursor-offset + 1;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 38)    // VK_UP — Sprint 43e-2 cursor move
                 // move-cursor-vertical returns the input unchanged at
                 // the top of the buffer, so the `~=` guard skips the
                 // pointless repaint.
                 let new-off = move-cursor-vertical(cached-flat, cursor-offset, -1, ideal-col);
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 40)    // VK_DOWN — Sprint 43e-2 cursor move
                 let new-off = move-cursor-vertical(cached-flat, cursor-offset, 1, ideal-col);
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 8)     // VK_BACK — Sprint 43d backspace
                 // Delete the byte at cursor-offset - 1. The rope is
                 // persistent — `source-text := rope-delete(...)` makes
                 // a new sibling tree sharing almost every leaf with
                 // the old one. Update cursor, recompute metrics,
                 // refresh cached-flat (so WM_PAINT doesn't pay for
                 // serialisation), re-issue scroll info, repaint.
                 if (cursor-offset > 0)
                   source-text := rope-delete(source-text, cursor-offset - 1, cursor-offset);
                   cursor-offset := cursor-offset - 1;
                   cached-flat := rope->string(source-text);
                   buffer-lines := nod-rope-line-count(source-text);
                   buffer-max-cols := nod-rope-max-line-chars(source-text);
                   client-width-px  := buffer-max-cols * char-width;
                   client-height-px := buffer-lines * line-height;
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                   %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                   update-ideal-col();
                   // Sprint 43e-4 — after backspace the cursor may have
                   // walked off the left edge of the viewport on a wrap;
                   // pull the viewport back to keep it visible.
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               else 0 end;
               0
             elseif (msg = 513)  // WM_LBUTTONDOWN — Sprint 43e-5 / 43g cursor positioning
               // lParam packs the click position as (y << 16) | x in
               // client-area coordinates (top-left of the window's
               // client area = (0, 0)). Subtract gutter-px + pad to
               // convert to layout-relative coordinates; add scroll
               // offsets. Clicks inside the gutter (cx-client <
               // gutter-px) don't move the cursor — later sprints can
               // bind those to fold-toggle or error-tooltip.
               //
               // We create a fresh text layout per click — cheaper
               // than caching it across mutations and clicks are
               // rare compared to keystrokes. The layout is released
               // immediately after the hit-test.
               let cx-client = %lo-word(lparam);
               let cy-client = %hi-word(lparam);
               if (cx-client < gutter-px)
                 0   // ignore gutter clicks for now
               else
               let layout-x = cx-client + scroll-x-px - pad - gutter-px;
               let layout-y = cy-client + scroll-y-px - pad;
               let layout = %dwrite-create-text-layout(dwrite, cached-flat, format,
                                                       client-width-px, client-height-px);
               let new-off = %dwrite-hit-test-point(layout, layout-x, layout-y);
               %com-release(layout);
               // Clamp to buffer bounds. HitTestPoint returns the
               // closest valid offset even for out-of-bounds clicks
               // but we belt-and-brace it.
               let buf-len = size(cached-flat);
               let clamped = if (new-off < 0) 0
                             elseif (new-off > buf-len) buf-len
                             else new-off end;
               if (clamped ~= cursor-offset)
                 cursor-offset := clamped;
                 update-ideal-col();
                 ensure-cursor-visible(hwnd);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               end;   // close the gutter-click if/else
               0
             elseif (msg = 258)  // WM_CHAR — Sprint 43d character input
               // wparam carries the character as a UTF-16 code unit.
               // Phase-2 simplicity: accept only ASCII-printable
               // (32..126), Tab (9), or Enter (13, translated to
               // '\n'=10 for our internal representation). Backspace
               // (8) is handled in WM_KEYDOWN; everything else is
               // dropped silently. Full Unicode/IME input is a later
               // sprint.
               let ch = wparam;
               let insert? = (ch >= 32 & ch <= 126) | (ch = 9) | (ch = 13);
               if (insert?)
                 let byte-code = if (ch = 13) 10 else ch end;
                 let one-byte = %byte-string-allocate(1);
                 %byte-string-element-setter(byte-code, one-byte, 0);
                 source-text := rope-insert(source-text, cursor-offset, one-byte);
                 cursor-offset := cursor-offset + 1;
                 cached-flat := rope->string(source-text);
                 buffer-lines := nod-rope-line-count(source-text);
                 buffer-max-cols := nod-rope-max-line-chars(source-text);
                 client-width-px  := buffer-max-cols * char-width;
                 client-height-px := buffer-lines * line-height;
                 %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                 %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                 update-ideal-col();
                 // Sprint 43e-4 — keep the cursor on screen after the
                 // insertion advanced it past the right edge of the
                 // viewport. Common case: typing at the end of a
                 // long line.
                 ensure-cursor-visible(hwnd);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 273)  // WM_COMMAND — Sprint 41e/g menu dispatch
               // Menu items pack the command id in the wparam LOWORD;
               // wparam HIWORD is 0 for menu (vs accelerator/control).
               let cmd-id = %lo-word(wparam);
               if (cmd-id = 100)        // File → Open...
                 let new-path = %show-open-file-dialog(hwnd);
                 if (~ empty?(new-path))
                   let new-source = %read-file(new-path);
                   if (~ empty?(new-source))
                     // Sprint 43d — wrap the freshly read bytes in a
                     // rope before storing. All subsequent reads /
                     // edits use rope ops. Reset cursor + cache.
                     let new-rope = make-rope-from-string(new-source);
                     source-text := new-rope;
                     cursor-offset := 0;
                     cursor-on := 1;
                     ideal-col := 0;
                     cached-flat := new-source;
                     current-path := new-path;
                     buffer-lines := nod-rope-line-count(new-rope);
                     buffer-max-cols := nod-rope-max-line-chars(new-rope);
                     client-width-px  := buffer-max-cols * char-width;
                     client-height-px := buffer-lines * line-height;
                     scroll-x-px := 0;
                     scroll-y-px := 0;
                     %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
                     %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
                     recent-paths := nod-add-recent(new-path, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, new-path);
                     InvalidateRect(hwnd, 0, 0);
                   else 0 end;
                 else 0 end;
                 0
               elseif (cmd-id = 101)    // File → Save
                 // If no current-path yet, fall through to Save As: pop
                 // the save dialog so the user can name the file. If
                 // we have a path, just rewrite that file with the
                 // in-memory contents (currently identical to what's
                 // on disk — Sprint 41h+ adds dirty-flag tracking).
                 if (empty?(current-path))
                   let chosen = %show-save-file-dialog(hwnd);
                   if (~ empty?(chosen))
                     // Sprint 43d — serialise rope to flat bytes for
                     // %write-file. Sprint 43e+ can switch to leaf-
                     // by-leaf streaming once we have %write-file-append.
                     let ok = %write-file(chosen, cached-flat);
                     if (ok = 1)
                       current-path := chosen;
                       recent-paths := nod-add-recent(chosen, recent-paths);
                       rebuild-recent-submenu(recent-menu, recent-paths);
                       DrawMenuBar(hwnd);
                       update-title(hwnd, chosen);
                     else 0 end;
                   else 0 end;
                 else
                   %write-file(current-path, cached-flat);
                   0
                 end;
                 0
               elseif (cmd-id = 102)    // File → Save As...
                 let chosen = %show-save-file-dialog(hwnd);
                 if (~ empty?(chosen))
                   let ok = %write-file(chosen, cached-flat);
                   if (ok = 1)
                     current-path := chosen;
                     recent-paths := nod-add-recent(chosen, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, chosen);
                   else 0 end;
                 else 0 end;
                 0
               elseif (cmd-id = 199)    // File → Exit
                 PostQuitMessage(0);
                 0
               elseif (cmd-id = 200)    // Help → About
                 // Sprint 41f workaround — see SetWindowTextW
                 // declaration comment above.
                 SetWindowTextW(hwnd,
                                "NewOpenDylan IDE - Sprint 41g (About)");
                 0
               elseif (cmd-id > 300 & cmd-id < 306)  // Recent items 301..305
                 // Convert 1-based menu position to 0-based list index.
                 let idx = cmd-id - 301;
                 let cursor = recent-paths;
                 let i = 0;
                 // Walk to the requested index. If the list is shorter
                 // than expected (stale menu vs. live list — shouldn't
                 // happen but defensive), `cursor` lands on nil and we
                 // bail out.
                 until (i = idx | empty?(cursor))
                   cursor := tail(cursor);
                   i := i + 1;
                 end;
                 if (~ empty?(cursor))
                   let path = head(cursor);
                   let bytes = %read-file(path);
                   if (~ empty?(bytes))
                     // Sprint 43d — wrap in rope, same as Open does;
                     // also reset cursor + cache for the new buffer.
                     let rope = make-rope-from-string(bytes);
                     source-text := rope;
                     cursor-offset := 0;
                     cursor-on := 1;
                     ideal-col := 0;
                     cached-flat := bytes;
                     current-path := path;
                     buffer-lines := nod-rope-line-count(rope);
                     buffer-max-cols := nod-rope-max-line-chars(rope);
                     client-width-px  := buffer-max-cols * char-width;
                     client-height-px := buffer-lines * line-height;
                     scroll-x-px := 0;
                     scroll-y-px := 0;
                     %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
                     %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
                     recent-paths := nod-add-recent(path, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, path);
                     InvalidateRect(hwnd, 0, 0);
                   else 0 end;
                 else 0 end;
                 0
               else
                 // Unknown command id — defer to the OS default.
                 DefWindowProcW(hwnd, msg, wparam, lparam)
               end
             elseif (msg = 2)  // WM_DESTROY
               PostQuitMessage(0);
               0
             else
               DefWindowProcW(hwnd, msg, wparam, lparam)
             end
           end;
  // Sprint 11d — the OS-facing shell. One Dylan call, no allocations.
  // See the `handle-wm-message` definition above for the contract.
  let wp = method (hwnd, msg, wparam, lparam)
             handle-wm-message(hwnd, msg, wparam, lparam)
           end;
  let cb = as-wndproc-callback(wp);
  let atom = %register-window-class(cb, "NodIDE");
  // dwStyle = WS_OVERLAPPEDWINDOW (0xCF0000)
  //         | WS_VSCROLL          (0x00200000)
  //         | WS_HSCROLL          (0x00100000)
  //         = 16711680.
  // hMenu = `menu-bar` HMENU (10th arg).
  let hwnd = CreateWindowExW(0, atom, "NewOpenDylan IDE",
                             16711680, -2147483648, -2147483648, 1024, 768,
                             0, menu-bar, 0, 0);
  swap := %dxgi-create-swap-chain-for-hwnd(dxgi-factory, d3d-device, hwnd, 1024, 768);
  %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
  %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
  update-title(hwnd, current-path);
  ShowWindow(hwnd, 5);
  UpdateWindow(hwnd);
  // Sprint 43e-6 — start a 500 ms blink timer. WM_TIMER (msg 275)
  // fires on this thread's message pump every ~500 ms; the
  // handler toggles `cursor-on` and invalidates the window so the
  // cursor bar appears / disappears between paints.
  // Args: (hwnd, idEvent=1, uElapse=500ms, lpTimerFunc=NULL).
  SetTimer(hwnd, 1, 500, 0);
  %run-message-loop();
end function main;
