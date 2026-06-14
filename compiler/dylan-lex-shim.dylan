Module: dylan-lexer
Precedence: c

// dylan-lex-shim.dylan — Sprint 51b.
//
// Bridges the Dylan-side lexer (`lex(source)` in `dylan-lexer.dylan`)
// to a Rust-aligned token stream — the same kind ordinals
// `nod_reader::token::TokenKind` uses, with the same trivia-filtering
// + preamble-skip behaviour `nod_reader::lex` applies. The wire
// contract is locked in `docs/DYLAN_TOKEN_WIRE.md`.
//
// First-cut transport (Sprint 51b v1): text on stdout, one line per
// emitted token, `KIND SPAN_LO SPAN_HI\n`. The nod-driver adapter
// spawns this EXE, pipes the source as argv[1] (file path), reads
// the stream, and reconstructs `Vec<Token>`. Once the JIT side-load
// path lands (51b-followup), this same classifier wraps into a
// `c-callable` that emits the binary 16-byte records §1 specifies;
// the classifier itself never changes.
//
// Build:
//   nod-driver build --project dylan-lex-shim.prj
//
// The two-file build (this + dylan-lexer.dylan) reuses every existing
// helper — `<token>` hierarchy, `lex()`, `%read-file`, `%argv1`, etc.
// — so adding new kinds is just a method on the generic.

// ─── token-rust-kind — generic classifier (Rust ordinals) ─────────────────
//
// Discriminants below MUST stay aligned with the `#[repr(u8)]` order of
// `TokenKind` in `src/nod-reader/src/token.rs`. The mapping table in
// `docs/DYLAN_TOKEN_WIRE.md` §3 is the human-readable reference; this
// file is the executable form.

define method token-rust-kind (t :: <token>, source :: <byte-string>)
 => (kind :: <integer>)
  // Default for unrecognised classes — surfaces as Invalid (64) so the
  // oracle test fails loudly rather than silently producing wrong codes.
  64
end method;

// — Keyword tokens. Only the three hard-reserveds map to dedicated Rust
//   kinds; every other Dylan-classified keyword falls through to Ident (0)
//   because §2.1 of `specs/01-lexer.md` says they're not lexer reserveds.
//   `#next`/`#rest`/`#key`/`#all-keys` enter `<keyword-token>` too — they
//   map to HashNext/HashRest/HashKey/HashAllKeys.
define method token-rust-kind (t :: <keyword-token>, source :: <byte-string>)
 => (kind :: <integer>)
  let kw = keyword-token-keyword(t);
  if (kw = #"define")        1   // KwDefine
  elseif (kw = #"end")       2   // KwEnd
  elseif (kw = #"otherwise") 3   // KwOtherwise
  elseif (kw = #"hash-next")     14  // HashNext
  elseif (kw = #"hash-rest")     11  // HashRest
  elseif (kw = #"hash-key")      12  // HashKey
  elseif (kw = #"hash-all-keys") 13  // HashAllKeys
  else                       0   // Ident — all other Dylan-classified keywords
  end
end method;

define method token-rust-kind (t :: <identifier-token>, source :: <byte-string>)
 => (kind :: <integer>)
  0   // Ident
end method;

define method token-rust-kind (t :: <escaped-ident-token>, source :: <byte-string>)
 => (kind :: <integer>)
  4   // EscapedIdent
end method;

define method token-rust-kind (t :: <keyword-name-token>, source :: <byte-string>)
 => (kind :: <integer>)
  21  // KeywordColon
end method;

// — Hash-prefixed literals.
define method token-rust-kind (t :: <boolean-literal-token>, source :: <byte-string>)
 => (kind :: <integer>)
  if (boolean-literal-token-value(t)) 5 else 6 end   // HashTrue / HashFalse
end method;

define method token-rust-kind (t :: <literal-sequence-open>, source :: <byte-string>)
 => (kind :: <integer>)
  7   // HashLParen — `#(`
end method;

define method token-rust-kind (t :: <literal-vector-open>, source :: <byte-string>)
 => (kind :: <integer>)
  8   // HashLBracket — `#[`
end method;

define method token-rust-kind (t :: <symbol-literal-token>, source :: <byte-string>)
 => (kind :: <integer>)
  16  // Symbol — covers both `#"foo"` and `#:foo`
end method;

define method token-rust-kind (t :: <nil-literal-token>, source :: <byte-string>)
 => (kind :: <integer>)
  // `#nil` doesn't have a dedicated Rust kind. The Rust lexer doesn't
  // recognise it — the closest analogue is Invalid (64), but the
  // Dylan-side parser does accept `#nil` as a literal. For now we emit
  // it as HashHash (10) which is also the unsupported-`##` slot; the
  // oracle test will flag this if it diverges.
  10  // HashHash
end method;

// — Numerics.
define method token-rust-kind (t :: <integer-token>, source :: <byte-string>)
 => (kind :: <integer>)
  let r = integer-token-radix(t);
  if (r = 2)       18  // IntegerBin
  elseif (r = 8)   19  // IntegerOct
  elseif (r = 16)  20  // IntegerHex
  else             22  // Integer (decimal)
  end
end method;

define method token-rust-kind (t :: <float-token>, source :: <byte-string>)
 => (kind :: <integer>)
  23  // Float
end method;

define method token-rust-kind (t :: <ratio-token>, source :: <byte-string>)
 => (kind :: <integer>)
  24  // Ratio
end method;

// — Strings + chars. v1 lumps all three Rust subkinds (String, StringMulti,
//   StringRaw) into String (25) since the Dylan lexer doesn't expose the
//   subkind on the token class. Sprint-51b-followup: peek the source bytes
//   at the span start to distinguish `"`, `"""`, `r"`.
define method token-rust-kind (t :: <string-literal-token>, source :: <byte-string>)
 => (kind :: <integer>)
  25  // String
end method;

define method token-rust-kind (t :: <character-literal-token>, source :: <byte-string>)
 => (kind :: <integer>)
  28  // Char
end method;

// — Punctuation: discriminate on form symbol. The Dylan-side `#"assign"`
//   maps to Rust's ColonEqual; everything else uses the literal name from
//   the spec's table.
define method token-rust-kind (t :: <punctuation-token>, source :: <byte-string>)
 => (kind :: <integer>)
  let f = punctuation-token-form(t);
  if (f = #"lparen")             29
  elseif (f = #"rparen")         30
  elseif (f = #"lbracket")       31
  elseif (f = #"rbracket")       32
  elseif (f = #"lbrace")         33
  elseif (f = #"rbrace")         34
  elseif (f = #"comma")          35
  elseif (f = #"semicolon")      36
  elseif (f = #"dot")            37
  elseif (f = #"ellipsis")       38
  elseif (f = #"colon")          39
  elseif (f = #"colon-colon")    40
  elseif (f = #"assign")         41  // `:=` — Dylan-side symbol predates the spec
  elseif (f = #"equal")          42
  elseif (f = #"equal-equal")    43
  elseif (f = #"arrow")          44
  elseif (f = #"tilde")          45
  elseif (f = #"tilde-equal")    46
  elseif (f = #"tilde-equal-equal") 47
  elseif (f = #"plus")           48
  elseif (f = #"minus")          49
  elseif (f = #"star")           50
  elseif (f = #"slash")          51
  elseif (f = #"caret")          52
  elseif (f = #"amp")            53
  elseif (f = #"bar")            54
  elseif (f = #"less")           55
  elseif (f = #"greater")        56
  elseif (f = #"less-equal")     57
  elseif (f = #"greater-equal")  58
  elseif (f = #"query")          59
  elseif (f = #"query-query")    60
  elseif (f = #"query-equal")    61
  elseif (f = #"query-at")       62
  elseif (f = #"hash-hash")      10  // HashHash
  elseif (f = #"hash-lbrace")     9  // HashLBrace
  else                           64  // Invalid — unrecognised form
  end
end method;

// — Trivia + end-markers.
define method token-rust-kind (t :: <comment-token>, source :: <byte-string>)
 => (kind :: <integer>)
  // Trivia; filtered out before emission. Returning Invalid here would be
  // misleading; the value should never reach the wire. Use Eof (63) as a
  // sentinel so a misuse causes the oracle test to diverge clearly.
  63
end method;

define method token-rust-kind (t :: <whitespace-token>, source :: <byte-string>)
 => (kind :: <integer>)
  63  // Same rationale as `<comment-token>`.
end method;

define method token-rust-kind (t :: <error-token>, source :: <byte-string>)
 => (kind :: <integer>)
  64  // Invalid
end method;

define method token-rust-kind (t :: <eof-token>, source :: <byte-string>)
 => (kind :: <integer>)
  63  // Eof
end method;

// ─── token-emit? — trivia + comments are dropped before printing ──────────

define method token-emit? (t :: <token>) => (yes? :: <boolean>)
  // Default — keep every concrete class not explicitly overridden below.
  #t
end method;

define method token-emit? (t :: <whitespace-token>) => (yes? :: <boolean>)
  #f
end method;

define method token-emit? (t :: <comment-token>) => (yes? :: <boolean>)
  #f
end method;

// ─── preamble-end — port of nod_reader::scan_preamble ─────────────────────
//
// Find the byte offset where the Dylan source's `Key: value` preamble
// ends, i.e. the byte just after the terminating blank line. Returns 0
// if the source does not begin with a preamble.
//
// Heuristic (matches the Rust path's effective behaviour for every
// well-formed `.dylan` file in the corpus):
//   1. Source begins with [A-Za-z_] (header key start).
//   2. The first line contains a colon before its LF.
//   3. Find `"\n\n"` (or `"\r\n\r\n"`); the preamble ends one byte past
//      the second LF.
//   4. If no blank line exists in the source, the whole file is
//      conservatively treated as preamble-free (return 0).

define function preamble-end (source :: <byte-string>) => (cursor :: <integer>)
  let n = size(source);
  if (n = 0)
    0
  else
    let b0 = %byte-string-element(source, 0);
    // Header key must start with a letter or underscore.
    let key-start? = (b0 >= 65 & b0 <= 90)   // A-Z
                       | (b0 >= 97 & b0 <= 122)  // a-z
                       | (b0 = 95);              // _
    if (~ key-start?)
      0
    else
      // Find the first LF and verify a colon precedes it.
      let i = 0;
      let line-end = -1;
      let saw-colon? = #f;
      until (i = n | line-end >= 0)
        let b = %byte-string-element(source, i);
        if (b = 10) line-end := i;
        elseif (b = 58) saw-colon? := #t;
        end;
        i := i + 1;
      end;
      if (line-end < 0 | ~ saw-colon?)
        0
      else
        // Walk forward looking for blank line.
        let j = line-end + 1;
        let result = 0;
        let done = #f;
        until (done)
          if (j >= n)
            done := #t;
          else
            let b = %byte-string-element(source, j);
            if (b = 10)
              result := j + 1;
              done := #t;
            elseif (b = 13 & j + 1 < n
                      & %byte-string-element(source, j + 1) = 10)
              // CRLF blank line — skip both bytes.
              result := j + 2;
              done := #t;
            elseif (b = 32 | b = 9)
              // Leading whitespace on the line — continuation of previous
              // header value. Skip to next LF and continue.
              until (j >= n | %byte-string-element(source, j) = 10)
                j := j + 1;
              end;
              if (j < n) j := j + 1; end;
            else
              // Non-blank line; skip past its LF and continue.
              until (j >= n | %byte-string-element(source, j) = 10)
                j := j + 1;
              end;
              if (j < n) j := j + 1; end;
            end;
          end;
        end;
        result
      end
    end
  end
end function;

// ─── precedence-c-header? — port of the Rust `Precedence: c` detection ────
//
// Sprint 51e. The `Precedence:` module-header pragma lives in the source
// preamble (the `Key: value` block) that the lexer SKIPS, so the parser
// never sees it through the token stream. We surface it here, exactly
// where the host already has the raw `source`, by scanning the preamble
// byte range (`[0, preamble-end)`) for a header line whose key is
// `Precedence` and whose value is `c`, both compared case-insensitively
// after trimming surrounding whitespace. This mirrors nod-reader
// parser.rs:137-142 and src/nod-driver/src/dylan_to_ast.rs:87-92
// (`k.eq_ignore_ascii_case("precedence") && v.trim().eq_ignore_ascii_case("c")`),
// so both parsers agree on which files climb the C-style ladder.
//
// The verdict is threaded into the parser via `parse-dylan-with-precedence`
// (dylan-parser.dylan), which sets `ts-precedence-c?` on the token stream.

// ASCII-lowercase one byte (A-Z → a-z); other bytes unchanged.
define function ascii-lower (b :: <integer>) => (lo :: <integer>)
  if (b >= 65 & b <= 90) b + 32 else b end
end function;

// Does the source slice [lo, hi) equal `lit` (a lowercase literal),
// comparing case-insensitively? Used for the `precedence`/`c` match.
define function slice-ci=? (source :: <byte-string>, lo :: <integer>,
                            hi :: <integer>, lit :: <byte-string>)
 => (yes? :: <boolean>)
  let len = hi - lo;
  if (len ~= size(lit))
    #f
  else
    let i = 0;
    let ok = #t;
    until (i >= len | ~ ok)
      if (ascii-lower(%byte-string-element(source, lo + i)) ~= %byte-string-element(lit, i))
        ok := #f;
      end;
      i := i + 1;
    end;
    ok
  end
end function;

// Advance `lo` past leading spaces/tabs; return the trimmed start.
define function trim-left (source :: <byte-string>, lo :: <integer>,
                           hi :: <integer>) => (start :: <integer>)
  let i = lo;
  until (i >= hi | (%byte-string-element(source, i) ~= 32
                      & %byte-string-element(source, i) ~= 9))
    i := i + 1;
  end;
  i
end function;

// Retract `hi` past trailing spaces/tabs/CR; return the trimmed end.
define function trim-right (source :: <byte-string>, lo :: <integer>,
                            hi :: <integer>) => (stop :: <integer>)
  let i = hi;
  until (i <= lo | (%byte-string-element(source, i - 1) ~= 32
                      & %byte-string-element(source, i - 1) ~= 9
                      & %byte-string-element(source, i - 1) ~= 13))
    i := i - 1;
  end;
  i
end function;

define function precedence-c-header? (source :: <byte-string>)
 => (yes? :: <boolean>)
  let pre = preamble-end(source);
  let found = #f;
  let line-start = 0;
  // Walk each preamble line. A header line is `key: value`; we compare the
  // trimmed key to "precedence" and the trimmed value to "c". Continuation
  // lines (leading whitespace) have no colon and are skipped harmlessly.
  until (line-start >= pre | found)
    // Find this line's LF (or the preamble end).
    let line-end = line-start;
    until (line-end >= pre | %byte-string-element(source, line-end) = 10)
      line-end := line-end + 1;
    end;
    // Find the first colon within the line.
    let colon = line-start;
    let saw-colon = #f;
    until (colon >= line-end | saw-colon)
      if (%byte-string-element(source, colon) = 58)
        saw-colon := #t;
      else
        colon := colon + 1;
      end;
    end;
    if (saw-colon)
      let key-lo = trim-left(source, line-start, colon);
      let key-hi = trim-right(source, key-lo, colon);
      let val-lo = trim-left(source, colon + 1, line-end);
      let val-hi = trim-right(source, val-lo, line-end);
      if (slice-ci=?(source, key-lo, key-hi, "precedence")
            & slice-ci=?(source, val-lo, val-hi, "c"))
        found := #t;
      end;
    end;
    line-start := line-end + 1;
  end;
  found
end function;

// ─── emit-tokens — print kind + span for each emit-eligible token ─────────

define function emit-tokens (tokens, source :: <byte-string>) => ()
  let pre = preamble-end(source);
  let n = %stretchy-vector-size(tokens);
  let i = 0;
  until (i = n)
    let t = %stretchy-vector-element(tokens, i);
    let lo = span-start(token-span(t));
    if (token-emit?(t) & lo >= pre)
      let hi = span-end(token-span(t));
      let kind = token-rust-kind(t, source);
      format-out("%d %d %d\n", kind, lo, hi);
    end;
    i := i + 1;
  end;
end function;

// ─── dylan-lex-collect — in-process JIT side-load entry ──────────────────
//
// Sprint 51b Phase B entry. Same classification + filtering as
// `emit-tokens` but instead of writing to stdout it accumulates into a
// `<stretchy-vector>` of integers — three per emitted token:
// `kind, lo, hi`. The host (`src/nod-driver/src/dylan_lex_jit.rs`) walks
// the vector pulling out triples and reconstructs `Vec<Token>`.
//
// Why three flat ints rather than a `<list>` of triples? Stretchy-
// vectors of immediate integers are the cheapest readback shape:
// `nod_stretchy_vector_size` + `nod_stretchy_vector_element` already
// exist in the runtime ABI, and immediate-tagged integers unbox in
// O(1) on the Rust side. A list-of-triples would force three pair
// allocations per token plus a third-level structure walk.

define function dylan-lex-collect (source :: <byte-string>)
 => (records :: <object>)
  let pre = preamble-end(source);
  let tokens = lex(source);
  let records = %make-stretchy-vector(64);
  let n = %stretchy-vector-size(tokens);
  let i = 0;
  until (i = n)
    let t = %stretchy-vector-element(tokens, i);
    let lo = span-start(token-span(t));
    if (token-emit?(t) & lo >= pre)
      let hi = span-end(token-span(t));
      let kind = token-rust-kind(t, source);
      %stretchy-vector-push(records, kind);
      %stretchy-vector-push(records, lo);
      %stretchy-vector-push(records, hi);
    end;
    i := i + 1;
  end;
  records
end function;

// ─── dylan-parse-collect — Sprint 51c verify-mode entry ──────────────────
//
// Lex + parse `source` end-to-end on the Dylan side, then return the
// number of `<ast-error-node>`s in the top-level body. The host runs
// the Rust parser AND this one; agreement on the "did this source
// parse" verdict (count == 0 vs. count > 0) gates the build under
// `--verify-parse`. A nonzero divergence means one of the two parsers
// disagrees with the corpus, and we surface it loudly.
//
// Why count only top-level errors: the existing parser's error
// recovery emits `<ast-error-node>` at the constituent level when it
// bails on a definition / statement; nested errors propagate up.
// That's enough to answer the binary question "did the Dylan parser
// accept this file" — which is the contract this entry is making.
//
// Sprint 51d (deferred): a tree-shaped wire format that lets the
// Rust side actually consume the AST instead of just spot-checking
// it. This entry stays useful as the verify path even once
// replacement mode lands.

define function count-top-level-errors (body :: <ast-body>) => (n :: <integer>)
  let constituents = body-constituents(body);
  let size = %stretchy-vector-size(constituents);
  let count = 0;
  let i = 0;
  until (i = size)
    let c = %stretchy-vector-element(constituents, i);
    if (instance?(c, <ast-error-node>))
      count := count + 1;
    end;
    i := i + 1;
  end;
  count
end function;

define function dylan-parse-collect (source :: <byte-string>)
 => (error-count :: <integer>)
  let tokens = lex(source);
  let ast = parse-dylan(tokens);
  count-top-level-errors(ast)
end function;

// ─── dylan-parse-emit — Sprint 51d AST wire emitter ──────────────────────
//
// Per docs/DYLAN_AST_WIRE.md, pre-order walk of the parser's output
// emitting 4-int records into a flat stretchy-vector:
//   (kind, span_lo, span_hi, subtree_size)
//
// Sprint 51d v1 handles: Body, DefineFunction, Call, VariableRef,
// StringLit, IntegerLit, BinaryOp. Anything else lowers to Error
// (kind 7) with the span covering the unrecognised constituent.
// The host falls back to the Rust parser for the whole file on Error.

define constant $ast-kind-body            = 0;
define constant $ast-kind-define-function = 1;
define constant $ast-kind-call            = 2;
define constant $ast-kind-variable-ref    = 3;
define constant $ast-kind-string-lit      = 4;
define constant $ast-kind-integer-lit     = 5;
define constant $ast-kind-binary-op       = 6;
define constant $ast-kind-error           = 7;
// Sprint 51e — definition kinds.
// `class` and `generic` parse to DEDICATED nodes
// (<ast-class-definition> / <ast-generic-definition>), each with its
// own emit-node below. `function` and `method` are <ast-body-definition>
// distinguished by body-word, handled by `define-kind-for-word`.
define constant $ast-kind-define-class    = 8;
define constant $ast-kind-define-method   = 9;
define constant $ast-kind-define-generic  = 10;
// Sprint 51e — the <ast-statement> family: if / until / while / begin /
// select / block / for, all one node distinguished by stmt-word. The
// statement keyword is the span; the host recovers which statement it
// is from &src. Trailing clauses (elseif/else/cleanup/otherwise) are
// StatementClause children.
define constant $ast-kind-statement        = 11;
define constant $ast-kind-statement-clause = 12;
// Sprint 51e — `let <pattern> = <init>` local declaration. Span is the
// `let` keyword; the binding pattern + init expression are the
// `ldecl-list` body.
define constant $ast-kind-local-decl       = 13;
// Sprint 51e — one slot spec inside a `define class`. Span is the slot
// word; children are the slot's type expression and init expression,
// when present.
define constant $ast-kind-slot-spec        = 14;
// Sprint 51e — common expression forms.
define constant $ast-kind-dot-call         = 15;   // receiver.name
define constant $ast-kind-subscript        = 16;   // receiver[args]
define constant $ast-kind-unary-op         = 17;   // OP operand
define constant $ast-kind-kw-arg           = 18;   // key: value
define constant $ast-kind-paren-list       = 19;   // (a, b) / (e :: <t>)
// Sprint 51e — literal subtypes. All leaves; the host recovers the
// value by re-reading &src[span] (the parser now retains each
// literal's source token, so the span is real — see parse-leaf).
define constant $ast-kind-bool-lit         = 20;   // #t / #f
define constant $ast-kind-char-lit         = 21;   // 'a'
define constant $ast-kind-symbol-lit       = 22;   // #"foo" / foo:
define constant $ast-kind-float-lit        = 23;   // 3.14
define constant $ast-kind-ratio-lit        = 24;   // 1/3

// Sprint 51e — definition signatures, so the host can rebuild
// ast::Item::DefineFunction/Method {name, params, return_, body}.
define constant $ast-kind-param-list        = 25;   // ( ... ) param list
define constant $ast-kind-return-spec       = 26;   // => ( ... )
define constant $ast-kind-def-name          = 27;   // the definition's name token
define constant $ast-kind-param             = 28;   // one required param (name [+ type child])
define constant $ast-kind-var-marker        = 29;   // #rest/#key/#all-keys/#next present → host bails
define constant $ast-kind-return-value      = 30;   // one return value (name|type [+ type child])

// Sprint 51e — slot metadata, so the host can rebuild ast::SlotDef
// {name, allocation, init_keyword, required_init_keyword, type_, init}.
// Children of a SlotSpec, all KIND-tagged (order-independent):
define constant $ast-kind-slot-alloc        = 31;   // allocation adjective token (omit → Instance)
define constant $ast-kind-slot-init-kw      = 32;   // init-keyword name token (e.g. `x:`)
define constant $ast-kind-slot-required     = 33;   // marker: required-init-keyword
define constant $ast-kind-slot-type         = 34;   // 1 child: the `:: type` expr
define constant $ast-kind-slot-init         = 35;   // 1 child: the `= init` expr

// Sprint 51e — `#(…)` / `#[…]` / `#{…}` literal. Children are the
// element expressions; the host reads the open token (the node span) to
// pick the synthetic constructor (#list / #vector / #set).
define constant $ast-kind-hash-lit          = 36;

// Sprint 51e — `define constant`/`variable NAME [:: TYPE] = INIT`
// (an <ast-list-definition>). Span is the constant/variable keyword; the
// single child is the binding list (Body holding the `binder = init`).
define constant $ast-kind-define-binding    = 37;

// Sprint 51e.4 — one definition adjective (`sealed`/`open`/`abstract`/…).
// Leaf; span is the modifier word's token. Emitted as a leading child of
// the definition node; the host maps &src[span] to ast::Modifier and
// collects them in source order.
define constant $ast-kind-modifier          = 38;

// Map an <ast-body-definition> body-word to its wire kind, or -1 if the
// emitter doesn't structure that form yet (→ Error). `class`/`generic`
// are NOT here — they are their own node types, not body-definitions.
define function define-kind-for-word (word-name :: <byte-string>)
 => (kind :: <integer>)
  if (word-name = "function")
    $ast-kind-define-function
  elseif (word-name = "method")
    $ast-kind-define-method
  else
    -1
  end
end function;

// Emit one record (kind, lo, hi, subtree_size). subtree_size patched
// later — initial push is 1 (just self).
define function emit-record (out :: <stretchy-vector>,
                             kind :: <integer>,
                             lo :: <integer>,
                             hi :: <integer>)
 => (record-index :: <integer>)
  let idx = %stretchy-vector-size(out);
  %stretchy-vector-push(out, kind);
  %stretchy-vector-push(out, lo);
  %stretchy-vector-push(out, hi);
  %stretchy-vector-push(out, 1);   // subtree_size placeholder
  idx
end function;

// After children are emitted, patch the subtree_size = (current_size
// - record_index) / 4.
define function patch-subtree-size (out :: <stretchy-vector>,
                                    record-index :: <integer>)
 => ()
  let total-ints = %stretchy-vector-size(out);
  let subtree-records = (total-ints - record-index) / 4;
  %stretchy-vector-element-setter(subtree-records, out, record-index + 3);
end function;

// Sprint 51e.4 — emit one Modifier leaf per definition adjective
// (sealed/open/abstract/…), span = the modifier word's token. Emitted as
// leading children of a definition node; the host maps each &src[span] to
// an ast::Modifier and collects them in source order. Empty vector → no
// children, so unmodified definitions are unchanged.
define function emit-modifiers (mods :: <stretchy-vector>,
                                out :: <stretchy-vector>) => ()
  let n = %stretchy-vector-size(mods);
  let i = 0;
  until (i = n)
    let tok = %stretchy-vector-element(mods, i);
    let s = token-span(tok);
    let mi = emit-record(out, $ast-kind-modifier, span-start(s), span-end(s));
    patch-subtree-size(out, mi);
    i := i + 1;
  end;
end function;

define function span-of (node :: <ast-node>) => (lo :: <integer>, hi :: <integer>)
  let tok = node-token(node);
  if (instance?(tok, <token>))
    let s = token-span(tok);
    values(span-start(s), span-end(s))
  else
    values(0, 0)
  end
end function;

// Sprint 51e — span backfill. Container nodes (<ast-body>, <ast-call>,
// <ast-binary-op>) carry no leading <token>, so `span-of` returns
// (0,0) for them. After a container's children have been emitted, this
// recovers the container's span as the union of its descendants'
// spans. The walk is bottom-up: each child's own `emit-node` already
// backfilled it before we patch the parent, so descendant spans are
// final by the time we read them here. Only fires when the node's own
// span is empty — a real token-derived span is never overwritten.
define function backfill-span-from-children (out :: <stretchy-vector>,
                                             idx :: <integer>) => ()
  let cur-lo = %stretchy-vector-element(out, idx + 1);
  let cur-hi = %stretchy-vector-element(out, idx + 2);
  if (cur-lo = 0 & cur-hi = 0)
    let total = %stretchy-vector-size(out);
    let min-lo = 0;
    let max-hi = 0;
    let seen = #f;
    let i = idx + 4;
    until (i >= total)
      let lo = %stretchy-vector-element(out, i + 1);
      let hi = %stretchy-vector-element(out, i + 2);
      // A real span always has hi > lo >= 0, so hi > 0 ⟺ spanned;
      // (0,0) is the unspanned marker. Positive condition avoids an
      // empty `if` branch (the lowerer rejects empty `begin` blocks).
      if (hi > 0)
        if (seen = #f | lo < min-lo) min-lo := lo end;
        if (hi > max-hi) max-hi := hi end;
        seen := #t;
      end;
      i := i + 4;
    end;
    if (seen)
      %stretchy-vector-element-setter(min-lo, out, idx + 1);
      %stretchy-vector-element-setter(max-hi, out, idx + 2);
    end;
  end;
end function;

// Forward declared via define generic semantics — each method below
// emits one record (plus children) and returns nothing. The caller is
// responsible for patching the parent's subtree size if it cares.

define method emit-node (node :: <ast-node>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(node);
  emit-record(out, $ast-kind-error, lo, hi);
end method;

define method emit-node (b :: <ast-body>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(b);
  let idx = emit-record(out, $ast-kind-body, lo, hi);
  let constituents = body-constituents(b);
  let n = %stretchy-vector-size(constituents);
  let i = 0;
  until (i = n)
    let c = %stretchy-vector-element(constituents, i);
    emit-node(c, source, out);
    i := i + 1;
  end;
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

define method emit-node (d :: <ast-body-definition>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  // `define function|class|method … end` → the matching kind, with the
  // definition body emitted as a child. Other body-words (macro, etc.)
  // still lower to Error until their kinds land.
  let word-tok = defn-word(d);
  let word-name = token-name(word-tok);
  let kind = define-kind-for-word(word-name);
  if (kind < 0)
    let (lo, hi) = span-of(d);
    emit-record(out, $ast-kind-error, lo, hi);
  else
    let word-span = token-span(word-tok);
    let lo = span-start(word-span);
    let hi = span-end(word-span);
    let idx = emit-record(out, kind, lo, hi);
    // Sprint 51e.4 — definition adjectives (sealed/open/inline/…) as
    // leading Modifier children.
    emit-modifiers(defn-modifiers(d), out);
    // Sprint 51e — emit the signature so the host can rebuild the full
    // ast::Item {name, params, return_, body}. Children, in order:
    //   DefName (the name token), ParamList, ReturnSpec (only when an
    //   `=>` was present), then the Body. The host dispatches children
    //   by KIND, so an omitted optional child just reads as "absent".
    let nm = defn-method-name(d);
    if (instance?(nm, <token>))
      let ns = token-span(nm);
      let ni = emit-record(out, $ast-kind-def-name, span-start(ns), span-end(ns));
      patch-subtree-size(out, ni);
    end;
    let ps = defn-params(d);
    if (instance?(ps, <ast-param-list>))
      emit-node(ps, source, out);
    end;
    let rs = defn-return(d);
    if (instance?(rs, <ast-return-spec>) & ret-present?(rs) = #t)
      emit-node(rs, source, out);
    end;
    emit-node(defn-body(d), source, out);
    patch-subtree-size(out, idx);
  end
end method;

// Sprint 51e — `define constant`/`variable NAME [:: TYPE] = INIT`. Span
// is the `constant`/`variable` keyword (host reads &src[span] to pick
// DefineConstant vs DefineVariable); the single child is the binding
// list Body. `define domain` (the other list-word) has no nod-reader
// `Item` analogue → Error (host falls back).
define method emit-node (d :: <ast-list-definition>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = defn-word(d);
  let word-name = token-name(word-tok);
  let s = token-span(word-tok);
  if (word-name = "constant" | word-name = "variable")
    let idx = emit-record(out, $ast-kind-define-binding,
                          span-start(s), span-end(s));
    emit-modifiers(defn-modifiers(d), out);
    emit-node(defn-list(d), source, out);
    patch-subtree-size(out, idx);
  else
    emit-record(out, $ast-kind-error, span-start(s), span-end(s));
  end
end method;

// Sprint 51e — parameter list. One <param> child per required
// parameter (each with an optional type child); plus a single
// <var-marker> when the list has #rest/#key/#all-keys/#next, which the
// v1 host translator doesn't model (it falls back to the Rust parser).
define method emit-node (p :: <ast-param-list>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let idx = emit-record(out, $ast-kind-param-list, 0, 0);
  let req = params-required(p);
  let n = %stretchy-vector-size(req);
  let i = 0;
  until (i = n)
    emit-typed-name(%stretchy-vector-element(req, i), $ast-kind-param, source, out);
    i := i + 1;
  end;
  if (variadic-param-list?(p))
    let mi = emit-record(out, $ast-kind-var-marker, 0, 0);
    patch-subtree-size(out, mi);
  end;
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — return spec. One <return-value> child per value; a
// <var-marker> when `#rest` appears (host bails). Span is the `=>`
// arrow token (the parser sets node-token on the return-spec).
define method emit-node (r :: <ast-return-spec>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(r);
  let idx = emit-record(out, $ast-kind-return-spec, lo, hi);
  let vals = ret-values(r);
  let n = %stretchy-vector-size(vals);
  let i = 0;
  until (i = n)
    emit-typed-name(%stretchy-vector-element(vals, i), $ast-kind-return-value, source, out);
    i := i + 1;
  end;
  if (instance?(ret-rest(r), <token>))
    let mi = emit-record(out, $ast-kind-var-marker, 0, 0);
    patch-subtree-size(out, mi);
  end;
  patch-subtree-size(out, idx);
end method;

// Emit one <ast-typed-name> as `kind` (Param or ReturnValue): the
// record span is the name token; the optional `:: type` becomes a
// single child expression. For a bare return type (`=> (<integer>)`)
// the typed-name's tok IS the type and there is no child — the host
// reads that as name:None, type:Ident(span).
define function emit-typed-name (tn :: <ast-typed-name>, kind :: <integer>,
                                 source :: <byte-string>,
                                 out :: <stretchy-vector>) => ()
  let nt = typed-name-tok(tn);
  let s = token-span(nt);
  let idx = emit-record(out, kind, span-start(s), span-end(s));
  let ty = typed-name-type(tn);
  if (instance?(ty, <ast-node>))
    emit-node(ty, source, out);
  end;
  patch-subtree-size(out, idx);
end function;

// #rest / #key / #all-keys / #next present?  Then the v1 host bails.
define function variadic-param-list? (p :: <ast-param-list>) => (v :: <boolean>)
  instance?(params-rest(p), <token>)
    | (params-key?(p) = #t)
    | (params-all-keys?(p) = #t)
    | instance?(params-next(p), <token>)
end function;

// Sprint 51e — `define class NAME (supers) slot-spec … end`. Span is
// the `class` keyword; children are the superclass expressions
// (mostly variable-refs → structured) followed by the slot specs
// (<ast-slot-spec> → Error for now, but spanned + visible in the
// punch-list). The host recovers the class name from `&src`.
define method emit-node (d :: <ast-class-definition>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = defn-word(d);
  let s = token-span(word-tok);
  let idx = emit-record(out, $ast-kind-define-class, span-start(s), span-end(s));
  // Sprint 51e.4 — class adjectives (sealed/open/abstract/…) first.
  emit-modifiers(defn-modifiers(d), out);
  // Sprint 51e — class name as a DefName child (same kind as a
  // function's name), so the host needn't re-scan source after `class`.
  let nm = class-name(d);
  if (instance?(nm, <token>))
    let ns = token-span(nm);
    let ni = emit-record(out, $ast-kind-def-name, span-start(ns), span-end(ns));
    patch-subtree-size(out, ni);
  end;
  let supers = class-supers(d);
  let ns = %stretchy-vector-size(supers);
  let i = 0;
  until (i = ns)
    emit-node(%stretchy-vector-element(supers, i), source, out);
    i := i + 1;
  end;
  let slots = class-slots(d);
  let nslots = %stretchy-vector-size(slots);
  let j = 0;
  until (j = nslots)
    emit-node(%stretchy-vector-element(slots, j), source, out);
    j := j + 1;
  end;
  patch-subtree-size(out, idx);
end method;

// Sprint 51e.4 — `define [modifiers] generic NAME (params) => (returns);`.
// No body, no `end`. Children, in order: Modifier* (adjectives), DefName
// (the generic's name), ParamList, ReturnSpec (when `=>` present). Same
// shape as a body-definition minus the Body, so the host rebuilds
// ast::Item::DefineGeneric {modifiers, name, params, return_}.
define method emit-node (d :: <ast-generic-definition>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = gen-word(d);
  let s = token-span(word-tok);
  let idx = emit-record(out, $ast-kind-define-generic, span-start(s), span-end(s));
  emit-modifiers(defn-modifiers(d), out);
  let nm = gen-name(d);
  if (instance?(nm, <token>))
    let ns = token-span(nm);
    let ni = emit-record(out, $ast-kind-def-name, span-start(ns), span-end(ns));
    patch-subtree-size(out, ni);
  end;
  let ps = gen-params(d);
  if (instance?(ps, <ast-param-list>))
    emit-node(ps, source, out);
  end;
  let rs = gen-return(d);
  if (instance?(rs, <ast-return-spec>) & ret-present?(rs) = #t)
    emit-node(rs, source, out);
  end;
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `<ast-statement>`: if / until / while / begin / select /
// block / for / method-literal, all one node distinguished by the
// leading `stmt-word` keyword (which is the node's span). Children:
//   1. the leading body (`stmt-body`) — for `if`, its first
//      constituent is the condition expression;
//   2. each trailing clause (`stmt-clauses`: elseif/else/cleanup/
//      exception/otherwise), as a StatementClause child.
// The `for` iteration header (`stmt-for-header`) is NOT emitted in v1 —
// the loop is structured as a Statement with its body, but the
// iteration spec is left for a later pass (recoverable from &src).
define method emit-node (s :: <ast-statement>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = stmt-word(s);
  let sp = token-span(word-tok);
  let idx = emit-record(out, $ast-kind-statement, span-start(sp), span-end(sp));
  // Sprint 51e — anonymous `method (params) => (ret) body end` /
  // `function …` literals carry a signature on the statement (set by
  // parse-function-literal). Emit the ParamList + ReturnSpec FIRST (same
  // child order as a definition), so the host can rebuild
  // `Expr::Method { params, body }`. A plain statement (if/while/begin/…)
  // leaves both `#f`, so nothing extra is emitted and the wire shape is
  // unchanged for those.
  let ps = stmt-params(s);
  if (instance?(ps, <ast-param-list>))
    emit-node(ps, source, out);
  end;
  let rs = stmt-return(s);
  if (instance?(rs, <ast-return-spec>) & ret-present?(rs) = #t)
    emit-node(rs, source, out);
  end;
  emit-node(stmt-body(s), source, out);
  let clauses = stmt-clauses(s);
  if (instance?(clauses, <stretchy-vector>))
    let n = %stretchy-vector-size(clauses);
    let i = 0;
    until (i = n)
      emit-node(%stretchy-vector-element(clauses, i), source, out);
      i := i + 1;
    end;
  end;
  patch-subtree-size(out, idx);
end method;

// One trailing clause of a multi-clause statement (`else`, `elseif`,
// `cleanup`, `exception`, `otherwise`). Span is the clause keyword; the
// child is the clause body (for `elseif`, its first constituent is the
// clause's condition, same shape as the leading `if`).
define method emit-node (c :: <ast-statement-clause>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = clause-word(c);
  let sp = token-span(word-tok);
  let idx = emit-record(out, $ast-kind-statement-clause, span-start(sp), span-end(sp));
  emit-node(clause-body(c), source, out);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `let <pattern> = <init>`. Span is the `let` keyword; the
// single child is the `ldecl-list` body, which holds the binding
// pattern (a variable-ref or paren-list for `let (a, b) = …`) followed
// by the `= init` expression.
define method emit-node (d :: <ast-local-decl>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let word-tok = ldecl-word(d);
  let sp = token-span(word-tok);
  let idx = emit-record(out, $ast-kind-local-decl, span-start(sp), span-end(sp));
  emit-node(ldecl-list(d), source, out);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — one `slot NAME :: TYPE = INIT` (etc.) inside a class
// body. Span is the slot word (`node-token`); children are the type
// expression and the init expression, each emitted only when present
// (an unset slot has `#f` there, not an <ast-node>).
// The allocation-bearing adjective token (`class`/`each-subclass`/
// `virtual`/`constant`), or #f for the default instance allocation.
// Matches nod-reader's parser: open/sealed/abstract/concrete are
// ignored for allocation.
define function slot-alloc-adjective (s :: <ast-slot-spec>) => (tok :: <object>)
  let adjs = slot-adjectives(s);
  let n = %stretchy-vector-size(adjs);
  let result = #f;
  let i = 0;
  until (i = n)
    let t = %stretchy-vector-element(adjs, i);
    let w = token-name(t);
    if (w = "class" | w = "each-subclass" | w = "virtual" | w = "constant")
      result := t;
    end;
    i := i + 1;
  end;
  result
end function;

// Sprint 51e — full slot metadata, so the host rebuilds ast::SlotDef.
// Span stays the `slot` word; children are KIND-tagged and
// order-independent: DefName (slot name), then optional SlotAlloc,
// SlotInitKw, SlotRequired, SlotType, SlotInit.
define method emit-node (s :: <ast-slot-spec>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(s);
  let idx = emit-record(out, $ast-kind-slot-spec, lo, hi);
  let nt = slot-name-tok(s);
  if (instance?(nt, <token>))
    let ns = token-span(nt);
    let ni = emit-record(out, $ast-kind-def-name, span-start(ns), span-end(ns));
    patch-subtree-size(out, ni);
  end;
  let alloc = slot-alloc-adjective(s);
  if (instance?(alloc, <token>))
    let asp = token-span(alloc);
    let ai = emit-record(out, $ast-kind-slot-alloc, span-start(asp), span-end(asp));
    patch-subtree-size(out, ai);
  end;
  let ik = slot-init-kw(s);
  if (instance?(ik, <token>))
    let ks = token-span(ik);
    let ki = emit-record(out, $ast-kind-slot-init-kw, span-start(ks), span-end(ks));
    patch-subtree-size(out, ki);
  end;
  if (slot-required?(s) = #t)
    let ri = emit-record(out, $ast-kind-slot-required, 0, 0);
    patch-subtree-size(out, ri);
  end;
  let ty = slot-type(s);
  if (instance?(ty, <ast-node>))
    let ti = emit-record(out, $ast-kind-slot-type, 0, 0);
    emit-node(ty, source, out);
    backfill-span-from-children(out, ti);
    patch-subtree-size(out, ti);
  end;
  let ini = slot-init(s);
  if (instance?(ini, <ast-node>))
    let ii = emit-record(out, $ast-kind-slot-init, 0, 0);
    emit-node(ini, source, out);
    backfill-span-from-children(out, ii);
    patch-subtree-size(out, ii);
  end;
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `receiver.name` dot call. Span backfills from the
// receiver child (the `.name` is a trailing token, not a node).
define method emit-node (d :: <ast-dot-call>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let idx = emit-record(out, $ast-kind-dot-call, 0, 0);
  emit-node(dot-receiver(d), source, out);
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `receiver[args]` subscript. Children: receiver, then
// each index arg. Span backfills over the lot.
define method emit-node (s :: <ast-subscript>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let idx = emit-record(out, $ast-kind-subscript, 0, 0);
  emit-node(sub-receiver(s), source, out);
  let args = sub-args(s);
  let n = %stretchy-vector-size(args);
  let i = 0;
  until (i = n)
    emit-node(%stretchy-vector-element(args, i), source, out);
    i := i + 1;
  end;
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `OP operand` prefix unary. Span is the operator token.
define method emit-node (u :: <ast-unary-op>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let op-tok = unary-op(u);
  let sp = token-span(op-tok);
  let idx = emit-record(out, $ast-kind-unary-op, span-start(sp), span-end(sp));
  emit-node(unary-operand(u), source, out);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `key: value` keyword argument. Span is the keyword
// token; child is the value expression.
define method emit-node (k :: <ast-kw-arg>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let key-tok = kw-arg-key(k);
  let sp = token-span(key-tok);
  let idx = emit-record(out, $ast-kind-kw-arg, span-start(sp), span-end(sp));
  emit-node(kw-arg-value(k), source, out);
  patch-subtree-size(out, idx);
end method;

// Sprint 51e — `(a, b)` / `(e :: <type>)` parenthesised list (multi-item
// or typed head). Children are the items; span backfills over them.
define method emit-node (p :: <ast-paren-list>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let idx = emit-record(out, $ast-kind-paren-list, 0, 0);
  let items = paren-list-items(p);
  let n = %stretchy-vector-size(items);
  let i = 0;
  until (i = n)
    emit-node(%stretchy-vector-element(items, i), source, out);
    i := i + 1;
  end;
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

define method emit-node (c :: <ast-call>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(c);
  let idx = emit-record(out, $ast-kind-call, lo, hi);
  // First child: callee.
  emit-node(call-fn(c), source, out);
  // Remaining children: each arg.
  let args = call-args(c);
  let n = %stretchy-vector-size(args);
  let i = 0;
  until (i = n)
    let a = %stretchy-vector-element(args, i);
    emit-node(a, source, out);
    i := i + 1;
  end;
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

define method emit-node (v :: <ast-variable-ref>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let tok = varref-tok(v);
  let s = token-span(tok);
  let lo = span-start(s);
  let hi = span-end(s);
  emit-record(out, $ast-kind-variable-ref, lo, hi);
end method;

define method emit-node (s :: <ast-string-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(s);
  emit-record(out, $ast-kind-string-lit, lo, hi);
end method;

define method emit-node (i :: <ast-integer-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(i);
  emit-record(out, $ast-kind-integer-lit, lo, hi);
end method;

// Sprint 51e — the remaining literal leaves. Each now carries a real
// span (parser retains the token); the host re-reads &src[span] for
// the value (`#t`/`#f`, `'c'`, `#"sym"`/`sym:`, float, ratio text).
define method emit-node (b :: <ast-boolean-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(b);
  emit-record(out, $ast-kind-bool-lit, lo, hi);
end method;

define method emit-node (c :: <ast-char-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(c);
  emit-record(out, $ast-kind-char-lit, lo, hi);
end method;

define method emit-node (s :: <ast-symbol-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(s);
  emit-record(out, $ast-kind-symbol-lit, lo, hi);
end method;

define method emit-node (f :: <ast-float-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(f);
  emit-record(out, $ast-kind-float-lit, lo, hi);
end method;

define method emit-node (r :: <ast-ratio-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(r);
  emit-record(out, $ast-kind-ratio-lit, lo, hi);
end method;

// Sprint 51e — `#(elem, …)` list literal. Span is the `#(` open token;
// children are the element constants. An IMPROPER list (`#(a . tail)`)
// has no nod-reader analogue (parse_hash_literal is comma-only), so emit
// Error there and let the host fall back. The host reads &src[span] to
// see it's `#(` → rebuilds `Call(#list, elems)`.
define method emit-node (l :: <ast-list-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(l);
  if (instance?(lit-tail(l), <ast-node>))
    emit-record(out, $ast-kind-error, lo, hi);
  else
    let idx = emit-record(out, $ast-kind-hash-lit, lo, hi);
    let elems = lit-elems(l);
    let n = %stretchy-vector-size(elems);
    let i = 0;
    until (i = n)
      emit-node(%stretchy-vector-element(elems, i), source, out);
      i := i + 1;
    end;
    patch-subtree-size(out, idx);
  end
end method;

// Sprint 51e — `#[elem, …]` vector literal. Same shape; the `#[` open
// token in the span tells the host to rebuild `Call(#vector, elems)`.
define method emit-node (v :: <ast-vector-lit>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(v);
  let idx = emit-record(out, $ast-kind-hash-lit, lo, hi);
  let elems = lit-elems(v);
  let n = %stretchy-vector-size(elems);
  let i = 0;
  until (i = n)
    emit-node(%stretchy-vector-element(elems, i), source, out);
    i := i + 1;
  end;
  patch-subtree-size(out, idx);
end method;

define method emit-node (b :: <ast-binary-op>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(b);
  let idx = emit-record(out, $ast-kind-binary-op, lo, hi);
  emit-node(binop-left(b), source, out);
  emit-node(binop-right(b), source, out);
  backfill-span-from-children(out, idx);
  patch-subtree-size(out, idx);
end method;

define method emit-node (e :: <ast-error-node>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  let (lo, hi) = span-of(e);
  emit-record(out, $ast-kind-error, lo, hi);
end method;

// Sprint 51e — a typed binder `name :: <type>` appearing as a standalone
// node (the LHS of a `let name :: T = init` / `define variable name :: T
// = init` binding — parse-list-fragment wraps it into an <ast-typed-name>
// before the `=` fold). Without a dedicated method this fell to the
// default <ast-node> arm and emitted Error 0..0, masking the whole
// binding. Emit it as a `Param` record (kind 28): the span is the NAME
// token and the optional `:: type` is a single child — the same shape the
// host already decodes for parameters. The host (local_decl_parts) reads
// the binder name from the span; nod-reader's `Statement::Let` keeps the
// type but the dump prints only the name, so the binder NAME is all the
// gate needs.
define method emit-node (tn :: <ast-typed-name>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  emit-typed-name(tn, $ast-kind-param, source, out);
end method;

// <ast-pos-arg> is the parser's wrapper for "this is a positional
// call argument." Wire-format-wise it's transparent — we don't emit
// a record for it, just recurse into the wrapped value. That keeps
// the host's tree free of a wrapper kind that wouldn't translate to
// anything in `ast::Expr`.
define method emit-node (p :: <ast-pos-arg>, source :: <byte-string>,
                         out :: <stretchy-vector>) => ()
  emit-node(pos-arg-value(p), source, out);
end method;

define function dylan-parse-emit (source :: <byte-string>)
 => (records :: <object>)
  let tokens = lex(source);
  // Sprint 51e — honour the `Precedence: c` module-header pragma. The
  // lexer drops the preamble, so we re-scan the raw source here (where the
  // host hands us the bytes) and thread the verdict into the parser. A
  // `Precedence: c` file then climbs the legacy C-style operator ladder;
  // every other file keeps the flat DRM chain. This makes the wire tree's
  // operator nesting match nod-reader's C-precedence path byte-for-byte,
  // so the 9 grandfathered corpus files translate instead of falling back.
  let ast = parse-dylan-with-precedence(tokens, precedence-c-header?(source));
  let out = %make-stretchy-vector(64);
  emit-node(ast, source, out);
  out
end function;

// ─── dylan-sema-emit — Sprint 54b: in-process Dylan sema recording walk ──
//
// Lex + parse (honouring the `Precedence: c` pragma exactly as
// `dylan-parse-emit`) then run the Dylan-side sema recording walk
// (`collect-top-names`, bundled from `dylan-sema.dylan`) and return its
// deterministic four-section model dump as a `<byte-string>` — byte-identical
// to the Rust oracle's `nod_sema::format_sema_model` / `dump-sema`. The host
// calls this under `--sema-with-dylan`; 54b uses it for an in-process verify
// gate, 54c will parse it back into a `SemaModel` to make lowering consume it.
// Text transport mirrors `dylan-expand-source` (the macro engine's seam); the
// model dump is our own line-oriented format, so it round-trips losslessly
// (unlike source text — the Sprint 52.6 lesson).
define function dylan-sema-emit (source :: <byte-string>)
 => (model-text :: <byte-string>)
  let tokens = lex(source);
  let ast = parse-dylan-with-precedence(tokens, precedence-c-header?(source));
  collect-top-names(ast, source)
end function;

// ─── dylan-expand-source — Sprint 52.6 locus-(B) macro expander ──────────
//
// Expand every macro call in `source` to fixpoint — using the stdlib
// macros collected from `stdlib-source` plus the file's own `define
// macro`s — strip the `define macro` forms and the `Module:` preamble,
// and return the expanded (macro-free) source text. The host, under
// `NOD_EXPAND_WITH_DYLAN`, feeds the result straight to the normal parse
// path (`dylan-parse-emit`), so parse / wire / translate are unchanged —
// they just operate on already-expanded, kernel-shaped source. This is
// the locus-(B) "expand before the wire emit" step, implemented as a pure
// source→source transform (correctness gated byte-for-byte against the
// Rust expander by `macro_file_expand.rs`).
//
// In-file macros are collected first so a file redefining a stdlib macro
// shadows it (macro-table-lookup returns the first match).
//
// The stdlib macro set is invariant across files, but collecting it
// re-lexes the (large) stdlib source via the JIT'd Dylan lexer — the
// dominant cost when expanding a multi-file build. Cache it in a module
// variable (a GC root) so it's collected once per process, not per file.
define variable *stdlib-macros-cache* :: <object> = #f;

define function dylan-expand-source (source :: <byte-string>,
                                     stdlib-source :: <byte-string>)
 => (expanded :: <byte-string>)
  if (~ *stdlib-macros-cache*)
    *stdlib-macros-cache* := collect-macro-defs(stdlib-source);
  end;
  let table = collect-macro-defs(source);
  let stdlib-defs = *stdlib-macros-cache*;
  let i = 0;
  until (i = size(stdlib-defs))
    add!(table, stdlib-defs[i]);
    i := i + 1;
  end;
  expand-module-source(source, table, "0")
end function;

// ─── main — read argv[1] as a path, lex, emit ────────────────────────────

define function shim-main () => ()
  let path = %argv1();
  if (empty?(path))
    format-out("dylan-lex-shim: missing input path\n");
  else
    let source = load-source-via-rope(path);
    if (empty?(source))
      format-out("dylan-lex-shim: could not read %s\n", path);
    else
      let tokens = lex(source);
      emit-tokens(tokens, source);
    end;
  end;
end function shim-main;
