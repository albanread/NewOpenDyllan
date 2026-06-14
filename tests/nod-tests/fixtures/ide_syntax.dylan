Module: nod-ide
Precedence: c

// Sprint 44 — IDE module split (part 4 of 5: editor scan + syntax + gutter).
//
// Cursor motion helpers (scan-line-start, scan-line-end,
// move-cursor-vertical) for arrow-key + Home/End + Page/Ctrl-arrow
// navigation; tokenisation predicates (is-ident-start?, is-ident-cont?,
// is-dylan-keyword?, is-digit?) and the whole-buffer syntax-highlight
// pass (highlight-dylan-syntax); plus the gutter's line-number block
// (integer-to-string, build-line-numbers-block).
//
// All of these are stateless functions over `<byte-string>` and
// integers. `main` calls them on every WM_PAINT; the WNDPROC's
// keyboard handlers (in nod-ide.dylan) call the scan/move helpers
// for cursor placement.

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
