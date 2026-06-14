Module: nod-ide
Precedence: c

// Sprint 44 — IDE module split (part 3 of 5: pure-Dylan helpers).
//
// All the IDE's text-buffer scanning, list manipulation, basename
// extraction, recent-files persistence, and the title-bar update
// helper. Sprint 42a's `<byte-string>` ops (`size`, `element`,
// `concatenate`, `copy-sequence`, content `=`) were enough to retire
// the previous Rust shims (`nod_count_newlines`, `nod_max_line_chars`,
// `nod_basename`, `nod_load_recent`, `nod_add_recent`) — this file is
// the proof that the byte-string stdlib methods are usable for real
// work without dropping back to Rust.
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
