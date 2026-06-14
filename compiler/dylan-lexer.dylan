Module: dylan-lexer
Precedence: c

// Sprint 45a — Dylan lexer in Dylan, scaffolding phase.
//
// What lives here:
//   * `<span>` — start/end byte offsets into the source buffer.
//   * `<token>` and its concrete subclasses — the full hierarchy from
//     §2.2 of `docs/SPRINT_45_DYLAN_LEXER.md`. Each token is a class,
//     not an enum tag; everything dispatches on token class via generic
//     methods (`print-token`, `colour-of`, `token-source-text`, …) so
//     consumer code never writes a giant `select (kind)`.
//   * `dump-tokens(tokens, source) => <byte-string>` — the canonical
//     textual representation, locked in as the oracle-test contract
//     for sprint 45d.
//   * `lex(source) => <stretchy-vector>` — STUB for sprint 45a; returns
//     a one-element vector holding a single `<eof-token>` at offset 0.
//     Sprint 45b fills out the real implementation.
//   * A tiny `main` stub used by the `nod-driver dump-dylan-tokens`
//     subcommand. Reads argv[1], lexes it, prints the dump.
//
// The file knows NOTHING about the IDE. Sprint 45e is the IDE-side
// consumer that imports from this file via `colour-of` and the token
// hierarchy.

// ─── <span> — byte-offset range into a source buffer ──────────────────────

define class <span> (<object>)
  slot span-start :: <integer>, init-keyword: start:;
  slot span-end   :: <integer>, init-keyword: end:;
end class;

// `copy-sequence` on `<byte-string>` is positional (`s, start, stop`),
// not keyword — Sprint 42a's stdlib hasn't grown the keyword surface yet.
define method span-text (span :: <span>, source :: <byte-string>)
 => (text :: <byte-string>)
  copy-sequence(source, span-start(span), span-end(span))
end method;

define method span-contains? (span :: <span>, offset :: <integer>)
 => (yes? :: <boolean>)
  offset >= span-start(span) & offset < span-end(span)
end method;

// ─── <token> — abstract base ──────────────────────────────────────────────
//
// Every concrete token carries a `<span>` plus whatever extra slots its
// class needs. The hierarchy is FLAT in the sense that consumers never
// special-case it via subclass instanceof checks; they call the generic
// methods declared at the bottom of this section.

define class <token> (<object>)
  slot token-span :: <span>, init-keyword: span:;
end class;

// Concrete tokens. Slot lists mirror §2.2 of the design doc.

define class <keyword-token> (<token>)
  slot keyword-token-keyword :: <symbol>, init-keyword: keyword:;
end class;

define class <identifier-token> (<token>)
  slot identifier-token-name :: <byte-string>, init-keyword: name:;
end class;

define class <keyword-name-token> (<token>)
  slot keyword-name-token-name :: <byte-string>, init-keyword: name:;
end class;

// `<number-token>` is the abstract intermediate; never instantiated.
define class <number-token> (<token>) end class;

define class <integer-token> (<number-token>)
  slot integer-token-value :: <integer>, init-keyword: value:;
  slot integer-token-radix :: <integer>, init-keyword: radix:;
end class;

define class <float-token> (<number-token>)
  // Sprint 45a stores the raw text; sprint 45b can add a decoded
  // value slot once we have <float> / <double-float> in the runtime.
  slot float-token-raw-text :: <byte-string>, init-keyword: raw-text:;
end class;

// Ratio literal: `3/4`, `-7/8`. Stores raw text; runtime parsing deferred.
define class <ratio-token> (<number-token>)
  slot ratio-token-raw-text :: <byte-string>, init-keyword: raw-text:;
end class;

define class <string-literal-token> (<token>)
  slot string-literal-token-raw-text :: <byte-string>, init-keyword: raw-text:;
  slot string-literal-token-decoded  :: <byte-string>, init-keyword: decoded:;
end class;

define class <character-literal-token> (<token>)
  slot character-literal-token-codepoint :: <integer>, init-keyword: codepoint:;
end class;

define class <symbol-literal-token> (<token>)
  slot symbol-literal-token-name :: <byte-string>, init-keyword: name:;
end class;

define class <boolean-literal-token> (<token>)
  slot boolean-literal-token-value :: <boolean>, init-keyword: value:;
end class;

define class <nil-literal-token> (<token>) end class;

// Backslash-escaped operator name: `\+`, `\if`, `\=`, etc.
// The stored `name` does NOT include the leading `\`.
define class <escaped-ident-token> (<token>)
  slot escaped-ident-token-name :: <byte-string>, init-keyword: name:;
end class;

define class <literal-vector-open>   (<token>) end class;
define class <literal-sequence-open> (<token>) end class;

define class <punctuation-token> (<token>)
  slot punctuation-token-form :: <symbol>, init-keyword: form:;
end class;

// Comments carry their text plus a flag distinguishing `//` (line)
// from `/* */` (block). Sprint 45a uses the flag only for `dump-tokens`
// kind discrimination (COMMENT_LINE vs COMMENT_BLOCK); 45e tunes the
// colouring per kind.
define class <comment-token> (<token>)
  slot comment-token-text     :: <byte-string>, init-keyword: text:;
  slot comment-token-is-block? :: <boolean>,    init-keyword: is-block?:;
end class;

define class <whitespace-token> (<token>) end class;

define class <error-token> (<token>)
  slot error-token-message :: <byte-string>, init-keyword: message:;
end class;

define class <eof-token> (<token>) end class;

// ─── colour-of — RGB integer per token class ──────────────────────────────
//
// Constants for now; Sprint 45e tunes them for the IDE palette. Encoded
// as a 24-bit RGB integer (red << 16 | green << 8 | blue) so consumers
// can mask out channels with `/` and `mod` arithmetic. Whitespace
// colours to white (invisible against the white background); the IDE
// special-cases it anyway.

// RGB colours expressed as decimal — the Sprint 02 lexer hasn't
// taught the front-end the `16#RRGGBB` literal form yet (that's a
// Sprint 45b/c follow-up since our own lexer learns the same syntax
// then). Each comment notes the hex equivalent so the values are
// easy to cross-check against an editor palette.

define method colour-of (t :: <keyword-token>) => (rgb :: <integer>)
  255                              // 0x0000FF — blue
end method;

define method colour-of (t :: <identifier-token>) => (rgb :: <integer>)
  0                                // 0x000000 — black
end method;

define method colour-of (t :: <keyword-name-token>) => (rgb :: <integer>)
  128                              // 0x000080 — navy
end method;

define method colour-of (t :: <integer-token>) => (rgb :: <integer>)
  8388736                          // 0x800080 — purple
end method;

define method colour-of (t :: <float-token>) => (rgb :: <integer>)
  8388736                          // 0x800080 — purple
end method;

define method colour-of (t :: <ratio-token>) => (rgb :: <integer>)
  8388736                          // 0x800080 — purple
end method;

define method colour-of (t :: <string-literal-token>) => (rgb :: <integer>)
  16711680                         // 0xFF0000 — red
end method;

define method colour-of (t :: <character-literal-token>) => (rgb :: <integer>)
  16711680                         // 0xFF0000 — red
end method;

define method colour-of (t :: <symbol-literal-token>) => (rgb :: <integer>)
  16711680                         // 0xFF0000 — red
end method;

define method colour-of (t :: <boolean-literal-token>) => (rgb :: <integer>)
  8388736                          // 0x800080 — purple
end method;

define method colour-of (t :: <nil-literal-token>) => (rgb :: <integer>)
  8388736                          // 0x800080 — purple
end method;

define method colour-of (t :: <literal-vector-open>) => (rgb :: <integer>)
  8421504                          // 0x808080 — grey
end method;

define method colour-of (t :: <literal-sequence-open>) => (rgb :: <integer>)
  8421504                          // 0x808080 — grey
end method;

define method colour-of (t :: <punctuation-token>) => (rgb :: <integer>)
  0                                // 0x000000 — black
end method;

define method colour-of (t :: <comment-token>) => (rgb :: <integer>)
  32768                            // 0x008000 — green
end method;

define method colour-of (t :: <whitespace-token>) => (rgb :: <integer>)
  16777215                         // 0xFFFFFF — white (invisible)
end method;

define method colour-of (t :: <error-token>) => (rgb :: <integer>)
  16711680                         // 0xFF0000 — red
end method;

define method colour-of (t :: <eof-token>) => (rgb :: <integer>)
  0                                // 0x000000 — black
end method;

define method colour-of (t :: <escaped-ident-token>) => (rgb :: <integer>)
  16777130                         // 0xFFFFAA — pale yellow (operator names)
end method;

// ─── token-kind-name — uppercase tag for dump-tokens ──────────────────────
//
// The canonical dump format uses an uppercase kind tag without the
// angle-brackets or `-token` suffix. Lives here as a generic so adding
// a new token class only takes one method, never a giant `select`.

define method token-kind-name (t :: <keyword-token>) => (s :: <byte-string>)
  "KEYWORD"
end method;

define method token-kind-name (t :: <identifier-token>) => (s :: <byte-string>)
  "IDENTIFIER"
end method;

define method token-kind-name (t :: <escaped-ident-token>) => (s :: <byte-string>)
  "ESCAPED_IDENT"
end method;

define method token-kind-name (t :: <keyword-name-token>) => (s :: <byte-string>)
  "KEYWORD_NAME"
end method;

define method token-kind-name (t :: <integer-token>) => (s :: <byte-string>)
  "INTEGER"
end method;

define method token-kind-name (t :: <float-token>) => (s :: <byte-string>)
  "FLOAT"
end method;

define method token-kind-name (t :: <ratio-token>) => (s :: <byte-string>)
  "RATIO"
end method;

define method token-kind-name (t :: <string-literal-token>) => (s :: <byte-string>)
  "STRING"
end method;

define method token-kind-name (t :: <character-literal-token>) => (s :: <byte-string>)
  "CHAR"
end method;

define method token-kind-name (t :: <symbol-literal-token>) => (s :: <byte-string>)
  "SYMBOL"
end method;

define method token-kind-name (t :: <boolean-literal-token>) => (s :: <byte-string>)
  "BOOLEAN"
end method;

define method token-kind-name (t :: <nil-literal-token>) => (s :: <byte-string>)
  "NIL"
end method;

define method token-kind-name (t :: <literal-vector-open>) => (s :: <byte-string>)
  "LIT_VEC_OPEN"
end method;

define method token-kind-name (t :: <literal-sequence-open>) => (s :: <byte-string>)
  "LIT_SEQ_OPEN"
end method;

define method token-kind-name (t :: <punctuation-token>) => (s :: <byte-string>)
  "PUNCT"
end method;

// Comments distinguish line vs block via the `is-block?` slot so
// dump consumers see a stable two-token vocabulary.
define method token-kind-name (t :: <comment-token>) => (s :: <byte-string>)
  if (comment-token-is-block?(t)) "COMMENT_BLOCK" else "COMMENT_LINE" end
end method;

define method token-kind-name (t :: <whitespace-token>) => (s :: <byte-string>)
  "WS"
end method;

define method token-kind-name (t :: <error-token>) => (s :: <byte-string>)
  "ERROR"
end method;

define method token-kind-name (t :: <eof-token>) => (s :: <byte-string>)
  "EOF"
end method;

// ─── token-source-text — span-text wrapper ────────────────────────────────
//
// Generic so future token classes can override (e.g. a synthesised
// `<error-token>` whose message isn't a substring of `source`). The
// default just slices the span out of the source buffer.

define method token-source-text (t :: <token>, source :: <byte-string>)
 => (text :: <byte-string>)
  span-text(token-span(t), source)
end method;

// ─── print-token — write one canonical dump line for a token ──────────────
//
// One canonical line per token; fields separated by EXACTLY two spaces:
//
//   <start-line>:<start-col>-<end-line>:<end-col>  <KIND>  <escaped-text>
//
// EOF tokens stop after the KIND tag (no source text to show). The
// trailing newline is added by `dump-tokens`, not here.
//
// Stream-based: writes directly into the caller's `<string-stream>`
// accumulator rather than returning a freshly-allocated byte-string per
// token (the GAP-001-pre shape was O(N²) on the whole-buffer dump).
// GAP-001 (`a689fcd`) lit up the stream surface; this method is the
// first real consumer.

// GAP-007 workaround: this method ignores the `stream` parameter and
// writes to the module-variable `*dump-stream*` instead. The variable
// lives in a cell-backed slot registered as a GC root, so it survives
// the many allocations that happen inside `nod-int-to-string`,
// `write-string`, and `write-escaped-source-text`. (The function-arg
// form clobbers around the 92nd iteration of `dump-tokens`.) Callers
// MUST set `*dump-stream*` to a fresh string-stream before calling.
define method print-token
    (t :: <token>, source :: <byte-string>, stream :: <string-stream>)
 => ()
  let span = token-span(t);
  // Sprint 47 — GAP-003 fixed; we destructure the two return values
  // directly rather than packing them through `line * 1_000_000 + col`.
  let (start-line, start-col) = offset-to-line-col(source, span-start(span));
  let (end-line, end-col)     = offset-to-line-col(source, span-end(span));
  write-line-col-to-dump-stream(start-line, start-col);
  write-byte(*dump-stream*, 45);  // '-'
  write-line-col-to-dump-stream(end-line, end-col);
  write-string(*dump-stream*, "  ");
  write-string(*dump-stream*, token-kind-name(t));
  if (~instance?(t, <eof-token>))
    write-string(*dump-stream*, "  ");
    write-escaped-source-text-to-dump-stream(token-source-text(t, source));
  end;
end method;

// ─── write-line-col — small helper: `<line>:<col>` into a stream ─────────

define function write-line-col
    (stream :: <string-stream>, line :: <integer>, col :: <integer>) => ()
  write-string(stream, nod-int-to-string(line));
  write-byte(stream, 58);  // ':'
  write-string(stream, nod-int-to-string(col));
end function;

// GAP-007 workaround variant: writes to `*dump-stream*` so the stream
// reference lives in a GC-root cell, not a function-arg slot that can
// go stale across the int-to-string allocation.
define function write-line-col-to-dump-stream
    (line :: <integer>, col :: <integer>) => ()
  write-string(*dump-stream*, nod-int-to-string(line));
  write-byte(*dump-stream*, 58);  // ':'
  write-string(*dump-stream*, nod-int-to-string(col));
end function;

// ─── nod-int-to-string — local digit formatter ────────────────────────────
//
// The line-numbers gutter in `ide_syntax.dylan` already has an
// `integer-to-string` — we copy that body here (under a `nod-`
// prefix to avoid clashing with any future stdlib generic) so the
// lexer file stays self-contained. Sprint 45c will lift this to a
// stdlib helper alongside the character predicates.

define function nod-int-to-string (n :: <integer>) => (s :: <byte-string>)
  if (n = 0)
    "0"
  else
    let m = n;
    let digits = 0;
    until (m = 0)
      digits := digits + 1;
      m := m / 10;
    end;
    let s = %byte-string-allocate(digits);
    let m = n;
    let i = digits - 1;
    let done = #f;
    until (done)
      if (i < 0)
        done := #t;
      else
        let d = m - (m / 10) * 10;
        %byte-string-element-setter(48 + d, s, i);
        m := m / 10;
        i := i - 1;
      end;
    end;
    s
  end
end function;

// ─── offset-to-line-col ───────────────────────────────────────────────────
//
// Walk the source bytes up to `offset` counting line breaks. Lines and
// columns are 1-indexed. Newline = byte 10; carriage returns inside
// `\r\n` count as column-bumps only (the LF advances the line) — for
// Sprint 45a's hello.dylan the input is LF-only so the simple form is
// enough. Sprint 45b will revisit if/when we hit CRLF fixtures.
//
// Sprint 47 (GAP-003 fix) — returns `(line, col)` via SBCL-style
// secondary values. Replaces the earlier `line * 1_000_000 + col`
// packing workaround: the compiler now lowers `values(a, b)` and
// multi-binder `let (a, b) = …`, so the natural shape is the right
// shape.
//
// O(N) sliding-cursor optimization (issue #291): tokens come out of
// the lexer in monotonically increasing source-offset order, so
// `dump-tokens` calls this 2× per token with offsets that never
// regress. Rather than rescanning from byte 0 each call (O(N²) over
// the whole dump), we cache the last `(pos, line, col)` in module-
// level state and walk forward from there. Each source byte is
// visited at most once per `dump-tokens` invocation; for a 100 K-byte
// fixture the dump goes from "noticeable pause" to "instant".
//
// Defensiveness:
//   * Backwards seek (offset < cache pos) → reset to byte 0. Handles
//     any non-monotonic caller without producing wrong answers.
//   * Different source buffer → caller must `reset-line-col-cache()`
//     before switching sources (`dump-tokens` does so at entry). If
//     they don't, the defensive reset still kicks in the first time
//     the new source's tokens land before the stale cache position.

define variable *line-col-cache-pos*  :: <integer> = 0;
define variable *line-col-cache-line* :: <integer> = 1;
define variable *line-col-cache-col*  :: <integer> = 1;

define function reset-line-col-cache () => ()
  *line-col-cache-pos*  := 0;
  *line-col-cache-line* := 1;
  *line-col-cache-col*  := 1;
end function;

define function offset-to-line-col
    (source :: <byte-string>, offset :: <integer>)
 => (line :: <integer>, col :: <integer>)
  let n = %byte-string-size(source);
  let stop = if (offset > n) n elseif (offset < 0) 0 else offset end;
  // Defensive: if the caller seeks backwards from where the cache
  // sits, restart from byte 0. The common (and intended) case is
  // forwards from `*line-col-cache-pos*`, no reset needed.
  if (*line-col-cache-pos* > stop)
    reset-line-col-cache();
  end;
  let line = *line-col-cache-line*;
  let col = *line-col-cache-col*;
  let i = *line-col-cache-pos*;
  // Shaped to mirror `count-newlines-in` in ide_rope.dylan: the `else`
  // arm of every assignment-flavoured `if` returns `#f` so the loop
  // body's join point sees no SSA disagreement (the Sprint 42-pre
  // fix to `lower_if` only catches cases where one arm assigns and
  // the other arm has a meaningful value).
  until (i = stop)
    let b = %byte-string-element(source, i);
    if (b = 10)
      line := line + 1;
      col := 1;
    else
      col := col + 1;
    end;
    i := i + 1;
  end;
  // Save the new high-water mark for the next monotonic call.
  *line-col-cache-pos*  := i;
  *line-col-cache-line* := line;
  *line-col-cache-col*  := col;
  values(line, col)
end function;

// ─── write-escaped-source-text — escape control bytes into a stream ──────
//
// Replace control bytes and quote/backslash with their canonical dump
// escapes, writing directly into the caller's stream:
//   * byte 10  (LF)  → `\n`   (two characters: backslash + 'n')
//   * byte 9   (TAB) → `\t`
//   * byte 92  (`\`) → `\\`
//   * byte 34  (`"`) → `\"`
//   * byte 32  (` `) → `\s`   (so whitespace runs are visible)
//   * other bytes pass through unchanged — Sprint 45a doesn't bother
//     with hex escapes; the corpus we care about is LF-only.
//
// Pre-GAP-001 this allocated a fresh byte-string per byte (concatenate-
// as-you-go to dodge Sprint 42-pre's `lower_if` SSA-join bug). The
// stream-flavour writes single bytes via `write-byte` and 2-byte
// escapes via `write-string` of a literal — no `acc := concatenate(...)`
// chain, no O(N²) blow-up, no SSA-join trip-wire.

define function write-escaped-source-text
    (stream :: <string-stream>, s :: <byte-string>) => ()
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    if (b = 10)
      write-string(stream, "\\n");
    elseif (b = 9)
      write-string(stream, "\\t");
    elseif (b = 92)
      write-string(stream, "\\\\");
    elseif (b = 34)
      write-string(stream, "\\\"");
    elseif (b = 32)
      write-string(stream, "\\s");
    else
      write-byte(stream, b);
    end;
    i := i + 1;
  end;
end function;

// GAP-007 workaround variant: writes to `*dump-stream*` directly.
define function write-escaped-source-text-to-dump-stream
    (s :: <byte-string>) => ()
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    let b = %byte-string-element(s, i);
    if (b = 10)
      write-string(*dump-stream*, "\\n");
    elseif (b = 9)
      write-string(*dump-stream*, "\\t");
    elseif (b = 92)
      write-string(*dump-stream*, "\\\\");
    elseif (b = 34)
      write-string(*dump-stream*, "\\\"");
    elseif (b = 32)
      write-string(*dump-stream*, "\\s");
    else
      write-byte(*dump-stream*, b);
    end;
    i := i + 1;
  end;
end function;

// ─── dump-tokens ──────────────────────────────────────────────────────────
//
// Build the whole-buffer dump. Allocates ONE `<string-stream>` accumulator,
// walks the token vector calling `print-token` on each (which writes the
// canonical line into the stream), then materialises the stream as a
// `<byte-string>` once at the end.
//
// Pre-GAP-001 this was the O(N²) site — every token allocated a fresh
// dump-line byte-string, every iteration concatenated it onto a growing
// accumulator (allocating a fresh result). With the stream surface, the
// only allocations are (a) the stream's own stretchy-vector growth and
// (b) the final `as-byte-string` materialisation.

// Per-token line build — returns the canonical dump line for ONE
// token as a freshly-allocated byte-string, with no trailing newline.
// Allocates a fresh stream into the *dump-stream* module variable so
// the GC's root-tracking of the variable cell keeps the stream live
// across the many allocations that happen inside `print-token` itself.
// See GAP-007.
define function print-token-to-string
    (t :: <token>, source :: <byte-string>) => (line :: <byte-string>)
  *dump-stream* := make-string-stream();
  print-token(t, source, *dump-stream*);
  as-byte-string(*dump-stream*)
end function;

// Dump the token vector. Concatenate per-token lines using
// `acc := concatenate(acc, …)` — the IDE syntax fixture's
// `build-line-numbers-block` uses the same shape and works through
// thousands of tokens. See GAP-007 for the stream-local clobber that
// pushed us off the stream-streaming form.
//
// GAP-007 workaround: reads from the `*tokens*` module variable rather
// than the `tokens` parameter so the vector stays reachable through
// the heavy per-iteration allocation in `print-token-to-string`. The
// caller MUST set `*tokens*` before calling (the `lex` function does
// this on every invocation).
define function dump-tokens
    (tokens, source :: <byte-string>) => (text :: <byte-string>)
  *tokens* := tokens;
  // O(N) fix (issue #291): the offset-to-line-col cache is module-
  // state; whatever the previous caller did is irrelevant to us.
  // Reset so the first token's `span-start` walks from byte 0 with a
  // (line=1, col=1) seed, and every subsequent call benefits from
  // the monotonic forward walk.
  reset-line-col-cache();
  let n = %stretchy-vector-size(*tokens*);
  let acc = "";
  let i = 0;
  until (i = n)
    let t = %stretchy-vector-element(*tokens*, i);
    let line = print-token-to-string(t, source);
    acc := concatenate(acc, line);
    acc := concatenate(acc, "\n");
    i := i + 1;
  end;
  acc
end function;

// ─── lex — Sprint 45b real implementation ─────────────────────────────────
//
// Strategy:
//   * Module-level `*src*` + `*pos*` variables hold the immutable source
//     buffer and the moving byte cursor. GAP-004 (define variable) is
//     fixed, so the cursor can be a true mutable scalar.
//   * `lex(source)` resets the variables, then loops calling
//     `next-token()` until an `<eof-token>` is appended.
//   * Each scanner consumes ≥ 1 byte even on malformed input (the
//     `<error-token>` recovery path) so the loop is guaranteed to
//     terminate after at most `size(source) + 1` iterations.
//   * Lossless: whitespace and comments come back as first-class
//     tokens, one per run. The parser will use `non-trivia-tokens` to
//     skip them (a sprint-46 helper).
//
// Open questions from §9 of SPRINT_45_DYLAN_LEXER.md, settled here:
//   * Negative integers lex as `-` + digits (two tokens). Parser folds.
//   * `/* … */` block comments DO NOT nest. First `*/` closes them.
//   * `<error-token>` always carries an explanatory `error-message` and
//     advances pos by at least 1 byte.
//
// Things deliberately NOT covered in 45b (queued for follow-ups):
//   * Float literals (`3.14`, `1.0e-3`). The token class exists but
//     `lex` does not produce it yet; SPRINT_45_DYLAN_LEXER.md §3 marks
//     floats as nice-to-have.
//   * Header preambles (`Module: foo`, `Author: bar`). The Rust lexer
//     skips them before scanning; we lex them as ordinary identifiers
//     plus a trailing `:` (`<keyword-name-token>`). The 45d oracle
//     will document any disagreement.
//   * Triple-quoted strings, raw-string `#r"..."`, ratio numerics,
//     hex `\<HHHH>` char escapes, leading-dot floats — all deferred to
//     follow-up sprints with their own tests.

define variable *src* :: <byte-string> = "";
define variable *pos* :: <integer> = 0;
// GAP-007 workaround: also stash the in-progress token vector as a
// module variable so it lives in a `<cell>` slot (registered as a GC
// root). The function-local form clobbers around the 1000th token under
// heavy allocation pressure.
define variable *tokens* :: <object> = #f;
// Same GAP-007 workaround on the dump side. The dump-token stream
// is the only one we ever materialise in this file; stashing it in
// a module-variable cell keeps it reachable across the many
// allocations inside `print-token`.
define variable *dump-stream* :: <object> = #f;

// ─── tiny cursor helpers ──────────────────────────────────────────────────

define function at-end? () => (yes? :: <boolean>)
  *pos* >= %byte-string-size(*src*)
end function;

// `peek-at(off)` returns the byte at `*pos* + off` or -1 when past end.
// Using -1 as the EOF sentinel keeps every classification predicate
// pure-integer; no token-stream code ever pattern-matches on it.

define function peek-at (off :: <integer>) => (b :: <integer>)
  let i = *pos* + off;
  if (i >= 0 & i < %byte-string-size(*src*))
    %byte-string-element(*src*, i)
  else
    -1
  end
end function;

define function current-byte () => (b :: <integer>)
  peek-at(0)
end function;

define function advance (n :: <integer>) => ()
  *pos* := *pos* + n;
end function;

// ─── character classification ─────────────────────────────────────────────
//
// Dylan identifier alphabet (mirrors `is_ident_start` /
// `is_ident_continue` in `src/nod-reader/src/lexer.rs`). For 45c these
// lift into stdlib character predicates; the inline form is fine for
// 45b and lets the lexer stay self-contained.

// Sprint 45c — the bare byte-classification predicates moved to
// `stdlib.dylan` (`ascii-digit?`, `ascii-alpha?`, `ascii-hex-digit?`,
// `ascii-bin-digit?`, `ascii-oct-digit?`, `ascii-whitespace?`, etc).
// The Dylan-grammar-specific predicates below (`is-name-start?`,
// `is-name-cont?`, `is-exponent-marker?`) stay here — they encode
// Dylan's identifier alphabet and exponent letters, not generic ASCII.
//
// Thin local aliases keep the lexer source diff small and the byte-
// constant cross-reference (b = 48 etc) co-located with the Dylan
// scanner code. If you remove a callee, prune its alias too.

define function is-ascii-digit? (b :: <integer>) => (yes? :: <boolean>)
  ascii-digit?(b)
end function;

define function is-ascii-alpha? (b :: <integer>) => (yes? :: <boolean>)
  ascii-alpha?(b)
end function;

define function is-bin-digit? (b :: <integer>) => (yes? :: <boolean>)
  ascii-bin-digit?(b)
end function;

define function is-oct-digit? (b :: <integer>) => (yes? :: <boolean>)
  ascii-oct-digit?(b)
end function;

define function is-hex-digit? (b :: <integer>) => (yes? :: <boolean>)
  ascii-hex-digit?(b)
end function;

// Dylan's "name-start" alphabet: letters plus the punctuation graphics
// allowed at the head of an identifier. Note `-` is NOT in the start
// set (so `-7` lexes as `-` + `7`). `@` is NOT here either — a lone
// `@` is an error (caught by the unrecognised-byte fallback). `@`
// appears in `?@var:name` macro-rest-splice patterns, which the `?`
// scanner handles directly as a multi-byte operator; it can also
// appear inside an identifier as a continuation character (see
// `is-name-cont?` below).
define function is-name-start? (b :: <integer>) => (yes? :: <boolean>)
  is-ascii-alpha?(b)
    | b = 95   // '_'
    | b = 33   // '!'
    | b = 36   // '$'
    | b = 37   // '%'
    | b = 38   // '&'
    | b = 42   // '*'
    | b = 60   // '<'
    | b = 62   // '>'
    | b = 94   // '^'
    | b = 124  // '|'
    | b = 126  // '~'
end function;

// Name-continuation also accepts digits, `?`, `-`, `+`, `=`, `/`, `@`.
// `@` is allowed mid-identifier but not as a name-start (DRM-style
// rule: keeps lone `@` an error while permitting `foo@bar`-shaped
// names if a Dylan dialect ever uses them).
define function is-name-cont? (b :: <integer>) => (yes? :: <boolean>)
  is-name-start?(b)
    | is-ascii-digit?(b)
    | b = 45   // '-'
    | b = 43   // '+'
    | b = 61   // '='
    | b = 47   // '/'
    | b = 63   // '?'
    | b = 64   // '@'
end function;

// Name-continuation except `=`, used to disambiguate `<foo>` from `<=`.
define function is-name-cont-not-eq? (b :: <integer>) => (yes? :: <boolean>)
  is-name-cont?(b) & b ~= 61
end function;

// Float exponent markers: e/E (decimal), s/S (single), d/D (double), x/X (extended).
define function is-exponent-marker? (b :: <integer>) => (yes? :: <boolean>)
  b = 101 | b = 69   // 'e' 'E'
    | b = 115 | b = 83   // 's' 'S'
    | b = 100 | b = 68   // 'd' 'D'
    | b = 120 | b = 88   // 'x' 'X'
end function;

// Whitespace bytes treated as a single run. Newline (10) is included;
// `offset-to-line-col` separately tracks line breaks via its multi-
// value return.
define function is-whitespace-byte? (b :: <integer>) => (yes? :: <boolean>)
  ascii-whitespace?(b)
end function;

// ─── identifier classification: keyword vs ordinary ───────────────────────
//
// Dylan has a fairly long keyword list. Rather than allocating a hash
// table at lex-time we just compare against the literal strings via the
// stdlib `=` method on `<byte-string>` (Sprint 42a). One comparison per
// candidate keyword; for the typical token-stream this is a few hundred
// nanoseconds total per identifier. If profiling ever flags this hot,
// a perfect-hash table is a follow-up sprint.

define function classify-keyword (name :: <byte-string>)
 => (kw :: <object>)   // either a <symbol> on match or #f on miss
  if (name = "define") #"define"
  elseif (name = "end") #"end"
  elseif (name = "otherwise") #"otherwise"
  elseif (name = "let") #"let"
  elseif (name = "local") #"local"
  elseif (name = "if") #"if"
  elseif (name = "else") #"else"
  elseif (name = "elseif") #"elseif"
  elseif (name = "then") #"then"
  elseif (name = "begin") #"begin"
  elseif (name = "method") #"method"
  elseif (name = "function") #"function"
  elseif (name = "class") #"class"
  elseif (name = "module") #"module"
  elseif (name = "library") #"library"
  elseif (name = "use") #"use"
  elseif (name = "export") #"export"
  elseif (name = "import") #"import"
  elseif (name = "constant") #"constant"
  elseif (name = "variable") #"variable"
  elseif (name = "slot") #"slot"
  elseif (name = "make") #"make"
  elseif (name = "instance?") #"instance?"
  elseif (name = "singleton") #"singleton"
  elseif (name = "inherited") #"inherited"
  elseif (name = "next") #"next"
  elseif (name = "signal") #"signal"
  elseif (name = "condition") #"condition"
  elseif (name = "block") #"block"
  elseif (name = "cleanup") #"cleanup"
  elseif (name = "exception") #"exception"
  elseif (name = "select") #"select"
  elseif (name = "case") #"case"
  elseif (name = "cond") #"cond"
  elseif (name = "unless") #"unless"
  elseif (name = "while") #"while"
  elseif (name = "until") #"until"
  elseif (name = "for") #"for"
  elseif (name = "from") #"from"
  elseif (name = "to") #"to"
  elseif (name = "by") #"by"
  elseif (name = "in") #"in"
  elseif (name = "handler") #"handler"
  elseif (name = "generic") #"generic"
  elseif (name = "domain") #"domain"
  // Sprint 46b — `define macro` and `define c-function`.
  // Both are body-word shapes (terminate at `end`) the Rust
  // reader already handles; teaching the Dylan-side lexer the
  // keyword classification + the Dylan-side parser's body-word
  // predicate (see is-define-body-word?) closes six of the seven
  // failing fixtures in the Sprint-46-closure corpus run.
  elseif (name = "macro") #"macro"
  elseif (name = "c-function") #"c-function"
  elseif (name = "sealed") #"sealed"
  elseif (name = "open") #"open"
  elseif (name = "abstract") #"abstract"
  elseif (name = "concrete") #"concrete"
  elseif (name = "primary") #"primary"
  elseif (name = "free") #"free"
  elseif (name = "virtual") #"virtual"
  elseif (name = "each-subclass") #"each-subclass"
  elseif (name = "required-init-keyword") #"required-init-keyword"
  elseif (name = "init-keyword") #"init-keyword"
  elseif (name = "init-value") #"init-value"
  elseif (name = "init-function") #"init-function"
  elseif (name = "setter") #"setter"
  elseif (name = "getter") #"getter"
  elseif (name = "type") #"type"
  elseif (name = "subclass") #"subclass"
  elseif (name = "super") #"super"
  elseif (name = "next-method") #"next-method"
  else
    #f
  end
end function;

// ─── span construction + small wrappers ───────────────────────────────────

define function span-here (lo :: <integer>) => (s :: <span>)
  make(<span>, start: lo, end: *pos*)
end function;

// Materialise the bytes between `lo` and `*pos*` as a fresh
// `<byte-string>`. Used by scanners that capture token text (idents,
// numbers, comments).
define function slice-from (lo :: <integer>) => (s :: <byte-string>)
  copy-sequence(*src*, lo, *pos*)
end function;

// ─── individual scanners ──────────────────────────────────────────────────
//
// Every scanner is called with `*pos*` pointing at the first byte of the
// token. Each one advances `*pos*` to the byte after the last consumed
// byte and returns a fully-built token.

// Run of whitespace bytes — one token per maximal run.
define function scan-whitespace (lo :: <integer>) => (t :: <whitespace-token>)
  until (at-end?() | ~ is-whitespace-byte?(current-byte()))
    advance(1);
  end;
  make(<whitespace-token>, span: span-here(lo))
end function;

// `// …` to end of line. Newline byte is NOT consumed (it becomes a
// whitespace token on the next iteration).
define function scan-line-comment (lo :: <integer>) => (t :: <comment-token>)
  until (at-end?() | current-byte() = 10)
    advance(1);
  end;
  make(<comment-token>,
       span: span-here(lo),
       text: slice-from(lo),
       is-block?: #f)
end function;

// `/* … */` — does NOT nest. The first `*/` closes the comment. EOF
// inside an unterminated block comment produces an `<error-token>` so
// callers can flag it visually.
define function scan-block-comment (lo :: <integer>) => (t :: <token>)
  advance(2);  // consume the opening "/*"
  let closed = #f;
  until (at-end?() | closed)
    if (current-byte() = 42 & peek-at(1) = 47)  // '*' '/'
      advance(2);
      closed := #t;
    else
      advance(1);
    end;
  end;
  if (closed)
    make(<comment-token>,
         span: span-here(lo),
         text: slice-from(lo),
         is-block?: #t)
  else
    make(<error-token>,
         span: span-here(lo),
         message: "unterminated block comment")
  end
end function;

// String literal `"…"` with escapes `\n \t \\ \" \r`. Returns either a
// `<string-literal-token>` (raw + decoded text) or an `<error-token>`
// for unterminated/invalid forms.
define function scan-string (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume opening quote
  // Build the decoded value into a stretchy-vector of bytes; the raw
  // text comes from a slice of the source. Two allocations per string,
  // which is fine for an editor-shaped workload.
  let decoded-bytes = %make-stretchy-vector(16);
  let done = #f;
  let result = #f;
  until (done)
    if (at-end?())
      result := make(<error-token>,
                     span: span-here(lo),
                     message: "unterminated string literal");
      done := #t;
    else
      let b = current-byte();
      if (b = 34)  // closing '"'
        advance(1);
        let n = %stretchy-vector-size(decoded-bytes);
        let decoded = %byte-string-allocate(n);
        let i = 0;
        until (i = n)
          %byte-string-element-setter(%stretchy-vector-element(decoded-bytes, i),
                                      decoded, i);
          i := i + 1;
        end;
        result := make(<string-literal-token>,
                       span: span-here(lo),
                       raw-text: slice-from(lo),
                       decoded: decoded);
        done := #t;
      elseif (b = 10)  // bare newline — unterminated
        result := make(<error-token>,
                       span: span-here(lo),
                       message: "newline inside string literal");
        done := #t;
      elseif (b = 92)  // backslash escape
        advance(1);
        if (at-end?())
          result := make(<error-token>,
                         span: span-here(lo),
                         message: "trailing backslash in string literal");
          done := #t;
        else
          let esc = current-byte();
          let decoded-byte =
            if (esc = 110) 10        // \n
            elseif (esc = 116) 9     // \t
            elseif (esc = 114) 13    // \r
            elseif (esc = 92) 92     // \\
            elseif (esc = 34) 34     // \"
            elseif (esc = 39) 39     // \'
            elseif (esc = 48) 0      // \0
            else
              esc                    // unknown escape — pass-through
            end;
          %stretchy-vector-push(decoded-bytes, decoded-byte);
          advance(1);
        end;
      else
        %stretchy-vector-push(decoded-bytes, b);
        advance(1);
      end;
    end;
  end;
  result
end function;

// Character literal `'a'` or `'\n'` — same escape vocabulary as strings
// but exactly one codepoint. Sprint 45b ASCII-only; Unicode characters
// in the source produce an error token (Dylan source IS UTF-8 but
// `<character>` design waits for a later sprint).
define function scan-character (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume opening quote
  if (at-end?())
    make(<error-token>,
         span: span-here(lo),
         message: "unterminated character literal")
  else
    let codepoint = -1;
    let b = current-byte();
    if (b = 39)  // empty '' — invalid
      advance(1);
      make(<error-token>,
           span: span-here(lo),
           message: "empty character literal")
    elseif (b = 92)  // escape
      advance(1);
      if (at-end?())
        make(<error-token>,
             span: span-here(lo),
             message: "trailing backslash in character literal")
      else
        let esc = current-byte();
        codepoint :=
          if (esc = 110) 10
          elseif (esc = 116) 9
          elseif (esc = 114) 13
          elseif (esc = 92) 92
          elseif (esc = 34) 34
          elseif (esc = 39) 39
          elseif (esc = 48) 0
          else esc
          end;
        advance(1);
        scan-character-close(lo, codepoint)
      end
    else
      codepoint := b;
      advance(1);
      scan-character-close(lo, codepoint)
    end
  end
end function;

// After the character body has been consumed, check for the closing
// quote and emit either a character-literal or an error token. Kept
// separate so both the escaped and bare branches share the logic.
define function scan-character-close
    (lo :: <integer>, codepoint :: <integer>) => (t :: <token>)
  if (at-end?() | current-byte() ~= 39)
    make(<error-token>,
         span: span-here(lo),
         message: "expected closing quote in character literal")
  else
    advance(1);
    make(<character-literal-token>,
         span: span-here(lo),
         codepoint: codepoint)
  end
end function;

// Float suffix scanner. Called by `scan-punctuation` when a `.` is
// followed by a digit (leading-dot float like `.5`). `*pos*` points at
// the `.`; `lo` marks the start of the token. Returns `<float-token>`
// with raw source text, or `<error-token>` when the exponent has no digits.
define function scan-float-suffix (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume '.'
  // Consume optional fractional digits (underscore separators allowed).
  until (at-end?() | (~ is-ascii-digit?(current-byte()) & current-byte() ~= 95))
    advance(1);
  end;
  // Optional exponent: e/E/s/S/d/D/x/X, optional sign, then digits.
  if (~ at-end?() & is-exponent-marker?(current-byte()))
    advance(1);
    if (~ at-end?() & (current-byte() = 43 | current-byte() = 45))  // '+' | '-'
      advance(1);
    end;
    if (at-end?() | ~ is-ascii-digit?(current-byte()))
      make(<error-token>,
           span: span-here(lo),
           message: "float exponent has no digits")
    else
      until (at-end?() | (~ is-ascii-digit?(current-byte()) & current-byte() ~= 95))
        advance(1);
      end;
      make(<float-token>, span: span-here(lo), raw-text: slice-from(lo))
    end
  else
    make(<float-token>, span: span-here(lo), raw-text: slice-from(lo))
  end
end function;

// Decimal integer literal. Caller has verified the first byte is a
// digit. NB: negative numbers are lexed as `-` + digits — this scanner
// never sees a leading sign. `_` digit-group separators are skipped.
// Handles float suffixes, ratio literals (`3/4`), and numeric-alpha
// identifiers (spec §3.10: `3foo` → single IDENT token).
define function scan-integer (lo :: <integer>) => (t :: <token>)
  // ─── Integer digit body (strict underscore rules) ──────────────────
  let value = 0;
  let had-digit? = #f;
  let last-underscore? = #f;
  let done? = #f;
  until (done? | at-end?())
    let b = current-byte();
    if (is-ascii-digit?(b))
      value := value * 10 + (b - 48);
      had-digit? := #t;
      last-underscore? := #f;
      advance(1);
    elseif (b = 95   // '_' — only between digit runs (no double/trailing)
              & had-digit?
              & ~ last-underscore?
              & is-ascii-digit?(peek-at(1)))
      last-underscore? := #t;
      advance(1);
    else
      done? := #t;
    end;
  end;
  let is-float? = #f;
  // ─── Fraction part: `.` not part of `..` or `...` ─────────────────
  if (~ at-end?() & current-byte() = 46 & peek-at(1) ~= 46)
    is-float? := #t;
    advance(1);  // consume '.'
    until (at-end?() | (~ is-ascii-digit?(current-byte()) & current-byte() ~= 95))
      advance(1);
    end;
  end;
  // ─── Exponent part: e/E/s/S/d/D/x/X ───────────────────────────────
  if (~ at-end?() & is-exponent-marker?(current-byte()))
    is-float? := #t;
    advance(1);  // consume exponent letter
    if (~ at-end?() & (current-byte() = 43 | current-byte() = 45))
      advance(1);  // optional sign
    end;
    until (at-end?() | (~ is-ascii-digit?(current-byte()) & current-byte() ~= 95))
      advance(1);
    end;
  end;
  // ─── Ratio: `<digits>/<digits>` ────────────────────────────────────
  if (~ is-float? & ~ at-end?() & current-byte() = 47 & is-ascii-digit?(peek-at(1)))
    advance(1);  // consume '/'
    until (at-end?() | (~ is-ascii-digit?(current-byte()) & current-byte() ~= 95))
      advance(1);
    end;
    make(<ratio-token>, span: span-here(lo), raw-text: slice-from(lo))
  // ─── Numeric-alpha (spec §3.10): `3foo` → single identifier ────────
  elseif (~ at-end?() & is-name-cont?(current-byte()))
    until (at-end?() | ~ is-name-cont?(current-byte()))
      advance(1);
    end;
    make(<identifier-token>, span: span-here(lo), name: slice-from(lo))
  elseif (is-float?)
    make(<float-token>, span: span-here(lo), raw-text: slice-from(lo))
  else
    make(<integer-token>, span: span-here(lo), value: value, radix: 10)
  end
end function;

// Radix-prefixed integer. Caller has consumed `#` and the letter (`b`,
// `o`, or `x`); `radix` plus the matching digit predicate are passed
// in. Empty digit run produces an error token.
define function scan-radix-integer
    (lo :: <integer>, radix :: <integer>) => (t :: <token>)
  let value = 0;
  let any-digit? = #f;
  let last-underscore? = #f;
  let done = #f;
  until (done)
    if (at-end?())
      done := #t;
    else
      let b = current-byte();
      if (b = 95)  // '_' — only between digit runs (no leading/double/trailing)
        if (~ any-digit? | last-underscore?)
          done := #t;  // leading or double underscore: stop
        else
          last-underscore? := #t;
          advance(1);
        end;
      else
        let digit-value =
          if (is-ascii-digit?(b)) b - 48
          elseif (b >= 97 & b <= 102) b - 87   // a..f → 10..15
          elseif (b >= 65 & b <= 70) b - 55    // A..F → 10..15
          else -1
          end;
        if (digit-value < 0 | digit-value >= radix)
          done := #t;
        else
          value := value * radix + digit-value;
          any-digit? := #t;
          last-underscore? := #f;
          advance(1);
        end;
      end;
    end;
  end;
  if (any-digit? & ~ last-underscore?)
    make(<integer-token>,
         span: span-here(lo),
         value: value,
         radix: radix)
  else
    make(<error-token>,
         span: span-here(lo),
         message: "radix literal with no digits")
  end
end function;

// Identifier (or identifier-shaped keyword). Trailing `:` (NOT part of
// `::` / `:=`) folds in as a `<keyword-name-token>`. Recognised
// keyword bodies map to `<keyword-token>` via `classify-keyword`.
define function scan-identifier (lo :: <integer>) => (t :: <token>)
  until (at-end?() | ~ is-name-cont?(current-byte()))
    advance(1);
  end;
  // Check for trailing keyword-name colon: a `:` that is not part of
  // `::` (type ann) or `:=` (assignment). Peek both bytes.
  if (~ at-end?() & current-byte() = 58
        & peek-at(1) ~= 58 & peek-at(1) ~= 61)
    advance(1);
    let name = copy-sequence(*src*, lo, *pos* - 1);
    make(<keyword-name-token>,
         span: span-here(lo),
         name: name)
  else
    let name = slice-from(lo);
    let kw = classify-keyword(name);
    if (kw)
      make(<keyword-token>,
           span: span-here(lo),
           keyword: kw)
    else
      make(<identifier-token>,
           span: span-here(lo),
           name: name)
    end
  end
end function;

// Hash-prefixed forms — `#t`, `#f`, `#(`, `#[`, `#"…"`, `#x…`, `#b…`,
// `#o…`. The caller has NOT yet consumed the `#`. Falls through to
// `<error-token>` for unrecognised follow-up bytes.
define function scan-hash (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume '#'
  if (at-end?())
    make(<error-token>,
         span: span-here(lo),
         message: "lone `#` at end of input")
  else
    let b = current-byte();
    if (b = 116 | b = 84)  // 't' | 'T'
      advance(1);
      make(<boolean-literal-token>, span: span-here(lo), value: #t)
    elseif (b = 102 | b = 70)  // 'f' | 'F'
      advance(1);
      make(<boolean-literal-token>, span: span-here(lo), value: #f)
    elseif (b = 40)  // '('
      advance(1);
      make(<literal-vector-open>, span: span-here(lo))
    elseif (b = 91)  // '['
      advance(1);
      make(<literal-sequence-open>, span: span-here(lo))
    elseif (b = 120 | b = 88)  // 'x' | 'X'
      advance(1);
      scan-radix-integer(lo, 16)
    elseif (b = 98 | b = 66)   // 'b' | 'B'
      advance(1);
      scan-radix-integer(lo, 2)
    elseif (b = 111 | b = 79)  // 'o' | 'O'
      advance(1);
      scan-radix-integer(lo, 8)
    elseif (b = 34)  // '"' — symbol literal #"foo"
      scan-hash-symbol(lo)
    elseif (b = 35)  // '#' — ##
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"hash-hash")
    elseif (b = 123)  // '{' — #{
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"hash-lbrace")
    elseif (b = 58)  // ':' — #:name colon-symbol
      scan-hash-colon-symbol(lo)
    elseif (is-name-start?(b))
      // #next, #rest, #key, #all-keys hash-word forms; also #nil.
      let word-lo = *pos*;
      until (at-end?() | ~ is-name-cont?(current-byte()))
        advance(1);
      end;
      let word = slice-from(word-lo);
      if (word = "next")
        make(<keyword-token>, span: span-here(lo), keyword: #"hash-next")
      elseif (word = "rest")
        make(<keyword-token>, span: span-here(lo), keyword: #"hash-rest")
      elseif (word = "key")
        make(<keyword-token>, span: span-here(lo), keyword: #"hash-key")
      elseif (word = "all-keys")
        make(<keyword-token>, span: span-here(lo), keyword: #"hash-all-keys")
      elseif (word = "nil")
        make(<nil-literal-token>, span: span-here(lo))
      else
        make(<error-token>,
             span: span-here(lo),
             message: "unrecognised `#` word form")
      end
    else
      // Unrecognised: consume one byte so we make progress.
      advance(1);
      make(<error-token>,
           span: span-here(lo),
           message: "unrecognised `#` form")
    end
  end
end function;

// Body of `#"foo"`. The `#` is already consumed; the `"` is at *pos*.
// Uses the same escape vocabulary as string literals.
define function scan-hash-symbol (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume opening '"'
  let name-bytes = %make-stretchy-vector(8);
  let done = #f;
  let result = #f;
  until (done)
    if (at-end?())
      result := make(<error-token>,
                     span: span-here(lo),
                     message: "unterminated symbol literal");
      done := #t;
    else
      let b = current-byte();
      if (b = 34)
        advance(1);
        let n = %stretchy-vector-size(name-bytes);
        let name = %byte-string-allocate(n);
        let i = 0;
        until (i = n)
          %byte-string-element-setter(%stretchy-vector-element(name-bytes, i),
                                      name, i);
          i := i + 1;
        end;
        result := make(<symbol-literal-token>,
                       span: span-here(lo),
                       name: name);
        done := #t;
      elseif (b = 10)
        result := make(<error-token>,
                       span: span-here(lo),
                       message: "newline inside symbol literal");
        done := #t;
      elseif (b = 92)
        advance(1);
        if (~ at-end?())
          %stretchy-vector-push(name-bytes, current-byte());
          advance(1);
        end;
      else
        %stretchy-vector-push(name-bytes, b);
        advance(1);
      end;
    end;
  end;
  result
end function;

// `#:name` colon-symbol literal. The `#` is already consumed and
// `*pos*` is at the `:`. Scans the identifier name and returns a
// `<symbol-literal-token>` with the same value as `#"name"`.
define function scan-hash-colon-symbol (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume ':'
  if (at-end?() | ~ is-name-start?(current-byte()))
    make(<error-token>,
         span: span-here(lo),
         message: "expected name after `#:`")
  else
    let name-lo = *pos*;
    until (at-end?() | ~ is-name-cont?(current-byte()))
      advance(1);
    end;
    let name = slice-from(name-lo);
    make(<symbol-literal-token>,
         span: span-here(lo),
         name: name)
  end
end function;

// ─── `~`, `~=`, `~==` ────────────────────────────────────────────────────
define function scan-tilde (lo :: <integer>) => (t :: <token>)
  advance(1);  // consume '~'
  if (~ at-end?() & current-byte() = 61 & peek-at(1) = 61)  // `~==`
    advance(2);
    make(<punctuation-token>, span: span-here(lo), form: #"tilde-equal-equal")
  elseif (~ at-end?() & current-byte() = 61)  // `~=`
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"tilde-equal")
  else
    make(<punctuation-token>, span: span-here(lo), form: #"tilde")
  end
end function;

// ─── `<`, `<=`, or identifier starting with `<` e.g. `<integer>` ─────────
define function scan-less-or-ident (lo :: <integer>) => (t :: <token>)
  if (is-name-cont-not-eq?(peek-at(1)))
    scan-identifier(lo)
  else
    advance(1);  // consume '<'
    if (~ at-end?() & current-byte() = 61)  // `<=`
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"less-equal")
    else
      make(<punctuation-token>, span: span-here(lo), form: #"less")
    end
  end
end function;

// ─── `>`, `>=`, or identifier starting with `>` ──────────────────────────
define function scan-greater-or-ident (lo :: <integer>) => (t :: <token>)
  if (is-name-cont-not-eq?(peek-at(1)))
    scan-identifier(lo)
  else
    advance(1);  // consume '>'
    if (~ at-end?() & current-byte() = 61)  // `>=`
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"greater-equal")
    else
      make(<punctuation-token>, span: span-here(lo), form: #"greater")
    end
  end
end function;

// Punctuation dispatch — single-byte operators plus the multi-char
// combinations `==`, `=>`, `::`, `:=`, `...`, `??`, `?=`, `?@`. The
// form slot uses canonical short symbols so the parser can dispatch
// with a single `select` on the punctuation symbol later.
define function scan-punctuation (lo :: <integer>) => (t :: <token>)
  let b = current-byte();
  if (b = 40)        // '('
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"lparen")
  elseif (b = 41)    // ')'
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"rparen")
  elseif (b = 91)    // '['
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"lbracket")
  elseif (b = 93)    // ']'
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"rbracket")
  elseif (b = 123)   // '{'
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"lbrace")
  elseif (b = 125)   // '}'
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"rbrace")
  elseif (b = 59)    // ';'
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"semicolon")
  elseif (b = 44)    // ','
    advance(1);
    make(<punctuation-token>, span: span-here(lo), form: #"comma")
  elseif (b = 46)    // '.'  -- check for "...", ".5" leading-dot float
    if (peek-at(1) = 46 & peek-at(2) = 46)
      advance(3);
      make(<punctuation-token>, span: span-here(lo), form: #"ellipsis")
    elseif (is-ascii-digit?(peek-at(1)))  // ".5" → leading-dot float
      scan-float-suffix(lo)
    else
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"dot")
    end
  elseif (b = 58)    // ':' -- check for "::" then ":="
    if (peek-at(1) = 58)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"colon-colon")
    elseif (peek-at(1) = 61)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"assign")
    else
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"colon")
    end
  elseif (b = 61)    // '=' -- check for "==", "=>"
    if (peek-at(1) = 61)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"equal-equal")
    elseif (peek-at(1) = 62)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"arrow")
    else
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"equal")
    end
  elseif (b = 63)    // '?' -- check for "??" "?=" "?@"
    if (peek-at(1) = 63)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"query-query")
    elseif (peek-at(1) = 61)
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"query-equal")
    elseif (peek-at(1) = 64)  // '?@' — macro rest-splice
      advance(2);
      make(<punctuation-token>, span: span-here(lo), form: #"query-at")
    else
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"query")
    end
  else
    // Unknown punctuation — make progress, emit error.
    advance(1);
    make(<error-token>,
         span: span-here(lo),
         message: "unrecognised character")
  end
end function;

// ─── next-token dispatcher ────────────────────────────────────────────────
//
// Single point of dispatch: peek the first byte and route to the right
// scanner. Each scanner is responsible for advancing `*pos*` past the
// token it consumes (and for advancing at least one byte even on
// error). The dispatcher never decides "skip this" — every input byte
// ends up in exactly one token.
//
// The `else` arm is the catch-all `<error-token>` producer for bytes
// that no scanner accepted.

define function next-token () => (t :: <token>)
  let lo = *pos*;
  if (at-end?())
    make(<eof-token>, span: span-here(lo))
  else
    let b = current-byte();
    if (is-whitespace-byte?(b))
      scan-whitespace(lo)
    elseif (b = 47 & peek-at(1) = 47)  // "//"
      advance(2);
      scan-line-comment(lo)
    elseif (b = 47 & peek-at(1) = 42)  // "/*"
      scan-block-comment(lo)
    elseif (b = 34)  // '"'
      scan-string(lo)
    elseif (b = 39)  // '\''
      scan-character(lo)
    elseif (b = 35)  // '#'
      scan-hash(lo)
    elseif (is-ascii-digit?(b))
      scan-integer(lo)
    elseif (b = 126)  // '~' — tilde / ~= / ~==
      scan-tilde(lo)
    elseif (b = 60)   // '<' — less, less-equal, or identifier like <integer>
      scan-less-or-ident(lo)
    elseif (b = 62)   // '>' — greater, greater-equal, or identifier
      scan-greater-or-ident(lo)
    elseif (b = 42 | b = 94 | b = 38 | b = 124)  // '*' '^' '&' '|'
      // Followed by ident-continuation → operator-name identifier; alone → punct.
      if (is-name-cont?(peek-at(1)))
        scan-identifier(lo)
      else
        advance(1);
        let form = if (b = 42) #"star"
                   elseif (b = 94) #"caret"
                   elseif (b = 38) #"amp"
                   else #"bar" end;
        make(<punctuation-token>, span: span-here(lo), form: form)
      end
    elseif (is-name-start?(b))
      scan-identifier(lo)
    elseif (b = 40 | b = 41 | b = 91 | b = 93 | b = 123 | b = 125
              | b = 59 | b = 44 | b = 46 | b = 58 | b = 61 | b = 63)
      scan-punctuation(lo)
    elseif (b = 45)  // '-' — bare minus (signs are NOT folded; §9 #2)
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"minus")
    elseif (b = 43)  // '+'
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"plus")
    elseif (b = 47)  // '/' — division operator (// and /* already caught above)
      advance(1);
      make(<punctuation-token>, span: span-here(lo), form: #"slash")
    elseif (b = 92)  // '\' — backslash-escaped identifier: `\+`, `\if`, `\=`
      advance(1);  // consume '\'
      if (at-end?() | ~ is-name-cont?(current-byte()))
        make(<error-token>, span: span-here(lo),
             message: "backslash not followed by identifier")
      else
        let name-start = *pos*;
        until (at-end?() | ~ is-name-cont?(current-byte()))
          advance(1);
        end;
        make(<escaped-ident-token>,
             span: span-here(lo),
             name: slice-from(name-start))
      end
    else
      // Catch-all: unrecognised byte. Advance one byte and emit an
      // error-token so the loop terminates.
      advance(1);
      make(<error-token>,
           span: span-here(lo),
           message: "unrecognised byte")
    end
  end
end function;

// ─── lex — public entry point ─────────────────────────────────────────────
//
// Reset the cursor, walk through the source one token at a time, append
// each to a stretchy vector, stop after pushing the EOF token. Always
// produces at least one token (the EOF).

// Inner accumulation loop. Reads `*tokens*` from the cell-backed module
// variable each iteration so the stretchy vector stays reachable even
// when local roots go stale under sustained allocation pressure.
// See GAP-007.
define function lex-into () => ()
  let done = #f;
  until (done)
    let t = next-token();
    %stretchy-vector-push(*tokens*, t);
    if (instance?(t, <eof-token>))
      done := #t;
    end;
  end;
end function;

define function lex (source :: <byte-string>) => (tokens)
  *src* := source;
  *pos* := 0;
  *tokens* := %make-stretchy-vector(64);
  lex-into();
  *tokens*
end function;

// ─── Minimal rope load path for lexer input ──────────────────────────────
//
// Retry after GAP-010: load source into a Dylan rope, then flatten it
// back to a byte-string before lexing. This keeps the public lexer API
// stable (`lex(<byte-string>)`) while exercising real rope-backed file
// ingestion inside Dylan.

define class <rope> (<object>) end class;

define class <rope-leaf> (<rope>)
  slot rope-leaf-bytes :: <byte-string>, init-keyword: bytes:;
  slot rope-leaf-len   :: <integer>,     init-keyword: len:;
end class;

define class <rope-node> (<rope>)
  slot rope-node-left   :: <rope>,    init-keyword: left:;
  slot rope-node-right  :: <rope>,    init-keyword: right:;
  slot rope-node-weight :: <integer>, init-keyword: weight:;
  slot rope-node-total  :: <integer>, init-keyword: total:;
end class;

define method rope-size (r :: <rope-leaf>) => (n :: <integer>)
  rope-leaf-len(r)
end method;

define method rope-size (r :: <rope-node>) => (n :: <integer>)
  rope-node-total(r)
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
         left: a,
         right: b,
         weight: asize,
         total: asize + bsize)
  end
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
    (r :: <rope>, lo :: <integer>, hi :: <integer>) => (s :: <byte-string>)
  let n = hi - lo;
  let result = %byte-string-allocate(n);
  rope-copy-into(r, lo, hi, result, 0);
  result
end function;

define function rope->string (r :: <rope>) => (s :: <byte-string>)
  rope-substring(r, 0, rope-size(r))
end function;

define function make-rope-from-string (s :: <byte-string>) => (r :: <rope>)
  let n = size(s);
  if (n <= 1024)
    make(<rope-leaf>, bytes: s, len: n)
  else
    let mid = n / 2;
    let left = make-rope-from-string(copy-sequence(s, 0, mid));
    let right = make-rope-from-string(copy-sequence(s, mid, n));
    rope-concatenate(left, right)
  end
end function;

define function load-source-via-rope (path :: <byte-string>) => (source :: <byte-string>)
  rope->string(make-rope-from-string(%read-file(path)))
end function;

// ─── main ─────────────────────────────────────────────────────────────────
//
// main() lives in dylan-lexer-main.dylan so that dylan-parser.dylan can
// compile together with this file (as a two-file build) without a
// duplicate-main conflict.  The `dump-dylan-tokens` driver subcommand
// builds [dylan-lexer.dylan, dylan-lexer-main.dylan]; the `parse-dylan`
// subcommand builds [dylan-lexer.dylan, dylan-parser.dylan].
