# Dylan Token Wire Format — Sprint 51b

This document specifies the **byte-stable wire format** the Dylan-side
lexer (and, later, parser) uses to hand a token stream back to the host
process across the FFI boundary. It exists so we can side-load
`dylan-lex-shim` into the JIT, redirect `nod_reader::lex` into it
behind `--use-dylan-lex`, and assert byte-identical output against the
Rust reference for every fixture in the corpus.

The contract here is the load-bearing piece — once the wire-format is
stable, we can swap implementations on either side without breaking the
other. Same pattern will scale to the parser (`dylan-parse-shim`), to
sema, and to DFM lowering.

---

## 1. Record layout

One token = one fixed-size 16-byte little-endian record:

```
offset 0  u32   kind     // TokenKind discriminant, table §3
offset 4  u32   span_lo  // start byte offset into the source buffer
offset 8  u32   span_hi  // end   byte offset into the source buffer
offset 12 u32   _pad     // reserved, MUST be zero
```

All fields are unsigned 32-bit little-endian. The 16-byte record size
is chosen so the buffer pointer alignment requirement is at most 4 bytes
on every supported target (currently x86_64 Windows; future ARM64
inherits). The trailing `_pad` field is there so the record stays
16-byte-aligned and gives us a free expansion slot for things like a
keyword-symbol index or a literal-decoded-value offset without a
breaking format change.

> **Why not pack to 12 bytes?** Because the natural sweet spot for the
> shim's output buffer is one record per cache line of `[Token]` on the
> Rust side (`Token` is also 16 bytes: `kind: u8` + 3 bytes pad +
> `span: Span` = 4+4 bytes). Matching the on-disk record size to the
> in-memory `Token` size lets the unmarshalling loop be a straight
> bytewise interpretation with zero pointer arithmetic per field.

---

## 2. Calling convention

```c
// One C function exported by dylan-lex-shim.dylan via the (future)
// `define c-callable function` surface. Until that surface exists, the
// caller marshals via the Dylan calling convention with <machine-word>
// arguments — same on the wire.
int64_t dylan_lex_c(
    const uint8_t *src_ptr,  // UTF-8 source bytes, caller-owned
    int64_t        src_len,  // byte length
    uint8_t       *out_ptr,  // caller-allocated buffer, see §2.1
    int64_t        out_cap   // capacity in BYTES (= 16 × max tokens)
);
```

### 2.1 Buffer ownership and sizing

The **caller** owns `src_ptr` for the entire call. The shim does not
retain a reference past return — every byte it cares about is either
copied into a Dylan-side `<byte-string>` (because the Dylan lexer
internally calls `copy-sequence` to grab lexeme text for some tokens)
or recorded as a span offset into `src_ptr` that the caller resolves
against its own copy of the source.

The **caller** allocates `out_ptr` with capacity `out_cap` bytes.
Capacity should be at least `16 * estimated_token_count`. A practical
upper bound that always fits: `16 * (1 + src_len)` — every byte
contributes at most one token, plus one for the trailing `EOF`.

### 2.2 Return value

The return value is the **number of tokens written** to `out_ptr`,
including the trailing `EOF`. Three sentinel ranges:

```
>= 0       Success. `result` × 16 bytes were written to `out_ptr`,
           starting with the first token and ending with one `EOF`.

< 0        Negative sentinel codes (table §2.3).
```

### 2.3 Error sentinels

```
-1   E_BUFFER_TOO_SMALL    out_cap was insufficient.
                           |return value| - 1 is then the required
                           capacity in BYTES (so caller can grow and
                           retry).  TODO(51b-followup): the first cut
                           may just panic-on-overflow until the retry
                           loop is wired.
-2   E_BAD_UTF8            src_ptr/src_len did not decode as UTF-8.
-3   E_LEXER_INTERNAL      Unexpected condition raised inside the
                           Dylan-side lexer.  Stderr will carry a
                           diagnostic.
```

The first cut may choose to abort on `E_BUFFER_TOO_SMALL` rather than
implement the retry loop; that's a perf footnote, not a correctness
issue (the upper bound from §2.1 is cheap to compute).

---

## 3. Kind table

Kind discriminants are the **`#[repr(u8)]` ordinals** of
`nod_reader::token::TokenKind`. Ordinals are **append-only**: adding a
new kind goes at the bottom, never in the middle. The shim's
`token-wire-kind` method MUST stay in lockstep with this table — a
Sprint-51b-followup test asserts the Rust enum and the Dylan classifier
agree on every fixture in the corpus.

| Ordinal | Rust `TokenKind`     | Dylan-side construction                       |
|---------|----------------------|-----------------------------------------------|
|       0 | `Ident`              | `<identifier-token>` (any non-`define`/`end`/`otherwise` keyword classified by the Dylan lexer as `<keyword-token>` also lowers here, because §2.1 of the lexer spec says only those three are hard-reserved). |
|       1 | `KwDefine`           | `<keyword-token>` with `keyword: #"define"`.  |
|       2 | `KwEnd`              | `<keyword-token>` with `keyword: #"end"`.     |
|       3 | `KwOtherwise`        | `<keyword-token>` with `keyword: #"otherwise"`. |
|       4 | `EscapedIdent`       | `<escaped-ident-token>`.                      |
|       5 | `HashTrue`           | `<boolean-literal-token>` with `value: #t`.   |
|       6 | `HashFalse`          | `<boolean-literal-token>` with `value: #f`.   |
|       7 | `HashLParen`         | `<literal-sequence-open>`.                    |
|       8 | `HashLBracket`       | `<literal-vector-open>`.                      |
|       9 | `HashLBrace`         | `<punctuation-token>` form `#"hash-lbrace"`. (Dylan-side construction TBD per §4.) |
|      10 | `HashHash`           | `<punctuation-token>` form `#"hash-hash"`.    |
|      11 | `HashRest`           | `<punctuation-token>` form `#"hash-rest"`.    |
|      12 | `HashKey`            | `<punctuation-token>` form `#"hash-key"`.     |
|      13 | `HashAllKeys`        | `<punctuation-token>` form `#"hash-all-keys"`. |
|      14 | `HashNext`           | `<punctuation-token>` form `#"hash-next"`.    |
|      15 | `HashIncludeMarker`  | `<punctuation-token>` form `#"hash-include"`. |
|      16 | `Symbol`             | `<symbol-literal-token>`.                     |
|      17 | `HashKeyword`        | `<punctuation-token>` form `#"hash-keyword"` carrying name in span. |
|      18 | `IntegerBin`         | `<integer-token>` with `radix: 2`.            |
|      19 | `IntegerOct`         | `<integer-token>` with `radix: 8`.            |
|      20 | `IntegerHex`         | `<integer-token>` with `radix: 16`.           |
|      21 | `KeywordColon`       | `<keyword-name-token>`.                       |
|      22 | `Integer`            | `<integer-token>` with `radix: 10`.           |
|      23 | `Float`              | `<float-token>`.                              |
|      24 | `Ratio`              | `<ratio-token>`.                              |
|      25 | `String`             | `<string-literal-token>` — single-quoted standard form. |
|      26 | `StringMulti`        | `<string-literal-token>` whose span begins with `"""`. |
|      27 | `StringRaw`          | `<string-literal-token>` whose span begins with `#"`. |
|      28 | `Char`               | `<character-literal-token>`.                  |
|      29 | `LParen`             | `<punctuation-token>` form `#"lparen"`.       |
|      30 | `RParen`             | `<punctuation-token>` form `#"rparen"`.       |
|      31 | `LBracket`           | `<punctuation-token>` form `#"lbracket"`.     |
|      32 | `RBracket`           | `<punctuation-token>` form `#"rbracket"`.     |
|      33 | `LBrace`             | `<punctuation-token>` form `#"lbrace"`.       |
|      34 | `RBrace`             | `<punctuation-token>` form `#"rbrace"`.       |
|      35 | `Comma`              | `<punctuation-token>` form `#"comma"`.        |
|      36 | `Semicolon`          | `<punctuation-token>` form `#"semicolon"`.    |
|      37 | `Dot`                | `<punctuation-token>` form `#"dot"`.          |
|      38 | `Ellipsis`           | `<punctuation-token>` form `#"ellipsis"`.     |
|      39 | `Colon`              | `<punctuation-token>` form `#"colon"`.        |
|      40 | `ColonColon`         | `<punctuation-token>` form `#"colon-colon"`.  |
|      41 | `ColonEqual`         | `<punctuation-token>` form `#"assign"` (Dylan-side name predates this spec). |
|      42 | `Equal`              | `<punctuation-token>` form `#"equal"`.        |
|      43 | `EqualEqual`         | `<punctuation-token>` form `#"equal-equal"`.  |
|      44 | `Arrow`              | `<punctuation-token>` form `#"arrow"`.        |
|      45 | `Tilde`              | `<punctuation-token>` form `#"tilde"`.        |
|      46 | `TildeEqual`         | `<punctuation-token>` form `#"tilde-equal"`.  |
|      47 | `TildeEqualEqual`    | `<punctuation-token>` form `#"tilde-equal-equal"`. |
|      48 | `Plus`               | `<punctuation-token>` form `#"plus"`.         |
|      49 | `Minus`              | `<punctuation-token>` form `#"minus"`.        |
|      50 | `Star`               | `<punctuation-token>` form `#"star"`.         |
|      51 | `Slash`              | `<punctuation-token>` form `#"slash"`.        |
|      52 | `Caret`              | `<punctuation-token>` form `#"caret"`.        |
|      53 | `Amp`                | `<punctuation-token>` form `#"amp"`.          |
|      54 | `Bar`                | `<punctuation-token>` form `#"bar"`.          |
|      55 | `Less`               | `<punctuation-token>` form `#"less"`.         |
|      56 | `Greater`            | `<punctuation-token>` form `#"greater"`.      |
|      57 | `LessEqual`          | `<punctuation-token>` form `#"less-equal"`.   |
|      58 | `GreaterEqual`       | `<punctuation-token>` form `#"greater-equal"`. |
|      59 | `Query`              | `<punctuation-token>` form `#"query"`.        |
|      60 | `QueryQuery`         | `<punctuation-token>` form `#"query-query"`.  |
|      61 | `QueryEqual`         | `<punctuation-token>` form `#"query-equal"`.  |
|      62 | `QueryAt`            | `<punctuation-token>` form `#"query-at"`.     |
|      63 | `Eof`                | `<eof-token>`.                                |
|      64 | `Invalid`            | `<error-token>`.                              |

---

## 4. Filtering rules

The Rust `nod_reader::lex` **skips** preamble + trivia (whitespace +
comments) before returning. The Dylan-side `lex` keeps them all (it
also feeds the IDE colourer). So before packing records, the shim
applies two filters:

1. **Drop the preamble.** If the Dylan-side lexer's first emitted token
   is a `Module:` keyword-name (a `<keyword-name-token>` whose name
   slot reads `"Module"`), advance past the blank-line terminator
   exactly as `nod_reader::scan_preamble` does. The shim already
   shares its source buffer with the caller, so this can be a span
   compare against the caller-supplied preamble end (passed as a
   separate `int64_t preamble_end` parameter? — TBD with the first
   cut; alternative is to bake the preamble-skip into the shim).
2. **Drop trivia.** Skip every `<whitespace-token>`, `<comment-token>`,
   and `<error-token>` whose source text is empty. The first cut MAY
   choose to keep `<error-token>`s and surface them as `Invalid` (kind
   64) so callers can report unrecoverable lex failures — TBD with the
   oracle test.

The trailing `<eof-token>` survives both filters and ends every stream.

---

## 5. Endianness, alignment, stability

* All multi-byte fields are **little-endian** because every currently
  supported target is. If a future big-endian port lands, this doc
  becomes a swap-on-read footnote in the unmarshalling code.
* Records do not require any alignment beyond 4 bytes; the caller's
  `Vec<u8>` is fine.
* The format is **stable across compiler versions** for a given
  `MAJOR.MINOR` tag tracked in `docs/DYLAN_TOKEN_WIRE.md`'s header:
  Sprint 51b ships as `1.0`. Append-only new kinds bump `MINOR`; any
  reshuffle of existing ordinals or layout changes bump `MAJOR`.

Wire-format version: **1.0**.

---

## 6. Out-of-scope (deferred)

* Token-text payloads (decoded string literal bytes, raw character
  codepoints, ratio numerator/denominator) are not transferred via
  this wire format. The caller resolves the span and re-decodes from
  the source buffer. This keeps the wire format simple and makes the
  format trivially comparable to `nod_reader::Token` (which also
  carries only kind + span).
* A separate `dylan-parse-wire` format will land with the parser side-
  load. Same record-shape philosophy: a fixed-size record per AST node
  type plus span. That work is Sprint 51c.

