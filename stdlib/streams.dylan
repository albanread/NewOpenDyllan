Module: dylan
Author: NewOpenDylan stdlib

// ─── GAP-001 / Sprint 45-prerequisite: <stream> + <string-stream> ─────────
//
// First Dylan-side stream surface. `<stream>` is the abstract base —
// real consumers (write-byte / write-string / as-byte-string) dispatch
// on it. `<string-stream>` is the only concrete subclass today: a
// write-only accumulator that builds a fresh `<byte-string>` from a
// sequence of `write-*` calls.
//
// Use case driving this: Sprint 45's Dylan-in-Dylan lexer wants a
// `print-token(token, source, stream :: <stream>)` generic so each
// token class renders itself to a stream. Previously the lexer
// faked it with a `print-token-to-string` helper returning a
// fresh byte-string per token; `dump-tokens` then concatenated
// every token's slice. That's `O(N²)` allocation. With the stream,
// `dump-tokens` allocates ONE stream, every token appends into it,
// one final `as-byte-string` materialises the dump.
//
// Future subclasses (planned, not in this commit):
//   <file-stream>       — write to a real file handle
//   <console-stream>    — wraps `format-out` for line-buffered I/O
//   <input-stream>      — read-side; lexer-on-stream eventually
//
// Storage: <string-stream> uses a <stretchy-vector> of integer
// bytes. The byte-string materialisation copies them out at the
// end via `%byte-string-element-setter` over a freshly-allocated
// byte-string of the right size. The intermediate stretchy-vector
// is GC-reclaimed once the materialised byte-string outlives it.
//
// Generic-dispatch shape: every method here dispatches on the stream
// argument. `<string-stream>` is the only concrete class today, but
// the discipline means a future `<file-stream>` slots in with no
// changes to consumer code.

define class <stream> (<object>)
end class;

define class <string-stream> (<stream>)
  slot string-stream-bytes :: <stretchy-vector>, init-keyword: bytes:;
end class;

// File stream (io). Operations are call-position (tolerated at compile);
// `with-open-file` makes one. Pinned in class_pins.rs.
define class <file-stream> (<stream>)
end class;

// Convenience constructor: a fresh empty string-stream.
define function make-string-stream () => (s :: <string-stream>)
  make(<string-stream>, bytes: %make-stretchy-vector(64))
end function;

// Append one byte (0..255 integer) to the stream.
define generic write-byte (stream :: <stream>, b :: <integer>) => ();

define method write-byte (stream :: <string-stream>, b :: <integer>) => ()
  %stretchy-vector-push(string-stream-bytes(stream), b);
end method;

// Append every byte of `s` to the stream.
define generic write-string (stream :: <stream>, s :: <byte-string>) => ();

define method write-string (stream :: <string-stream>, s :: <byte-string>) => ()
  let v = string-stream-bytes(stream);
  let n = %byte-string-size(s);
  let i = 0;
  until (i = n)
    %stretchy-vector-push(v, %byte-string-element(s, i));
    i := i + 1;
  end;
end method;

// Materialise the accumulated bytes as a fresh `<byte-string>`. The
// stream itself is unchanged — successive calls return successively
// longer strings if more bytes have been written between them.
define generic as-byte-string (stream :: <stream>) => (s :: <byte-string>);

define method as-byte-string (stream :: <string-stream>) => (s :: <byte-string>)
  let v = string-stream-bytes(stream);
  let n = %stretchy-vector-size(v);
  let out = %byte-string-allocate(n);
  let i = 0;
  until (i = n)
    %byte-string-element-setter(%stretchy-vector-element(v, i), out, i);
    i := i + 1;
  end;
  out
end method;
