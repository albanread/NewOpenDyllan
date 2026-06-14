# Dylan Lexer — NewOpenDylan Sprint 02 Spec

*Drafted against upstream `E:\opendylan\sources\dfmc\reader\` and the test corpus mirror at `E:\NewOpenDylan\opendylan-tests\sources\dfmc\reader\tests\`. Implements the `nod-reader::lex` deliverable in `SPRINTS.md` Sprint 02. Verify all line-number citations against the upstream source — they were sampled from a snapshot.*

## 1. Status and scope

This spec is the contract between research and the Sprint 02 implementer. The output is the `nod-reader::lex` function, its `Token`/`Span` types, the `format_tokens` debug dump, and the corresponding `nod-driver dump-tokens` subcommand. The IDE shell that *displays* the tokens is a separate deliverable in the same sprint; this spec only constrains the lexer.

**In scope for v1 of the lexer:**

- The full token taxonomy in §2.
- UTF-8 source files, but only the DRM-permitted character set: `\t\n\f\r`, ASCII printable `0x20`–`0x7E`, and "ASCII 8-bit extensions" `0x80`–`0xFF` treated as identifier-continuation bytes (`lexer.dylan:17-25`). Non-ASCII identifiers fall out of that range pass through verbatim into `Ident` text; we do *not* validate them as letters.
- Maximal-munch tokenisation matching the upstream state machine in `lexer-transitions.dylan`.
- `Token { kind: TokenKind, span: Span, text: &str }`. `text` is a borrowed slice of the source buffer; the caller owns the buffer for the token's lifetime.
- `format_tokens(src: &str) -> String`: stable, line-oriented debug dump (§5).
- Error tokens (`TokenKind::Invalid`) instead of panics — the lexer never aborts.

**Out of scope** (deferred to Sprint 03 and beyond):

- Fragment trees / paren-matching. This sprint emits a flat token stream.
- Module-aware classification. Upstream classifies identifiers per-module at lex time via `classify-word-in` (`classification.dylan:80-89`) so that `if`, `let`, `case`, `for`, etc. get token-merged classes. **We do not.** A Dylan source identifier is a single `TokenKind::Ident` whose role is decided in the parser. See §3.1 for why.
- The conditional-read machinery (`#if/#elseif/#else/#endif`, gated `lexer.dylan:472-543`). It is commented out upstream and we will not resurrect it in Sprint 02.
- The `define macro` token-merging cascade (`classification.dylan:117-154`). These are parser-level concerns.
- Attaching doc comments to AST nodes. Comments are dropped (`lexer.dylan:427-431`).

## 2. Token taxonomy

The `TokenKind` enum. Every variant is followed by example surface forms and the upstream state(s) that accept it.

### 2.1 Identifiers and the three hard reserveds

Dylan's lexer is far less reserving than the SPRINTS.md draft suggests. **Only three words are hard-reserved at lex time:** `define`, `end`, `otherwise` (`classification.dylan:40-46`). Everything the Sprint 02 deliverables list as a "reserved word" — `if`, `let`, `class`, `method`, `library`, `module`, `for`, `while`, `sealed`, `open`, `abstract`, `concrete`, `case`, `select`, `when`, `unless`, `begin`, `block`, `exception`, `cleanup`, `local`, `signal` — is a perfectly ordinary identifier at the lexer level. Upstream's appearance of reservation comes from per-module classification (`classify-word-in`), which is parser territory.

**This spec overrides Sprint 02's deliverables list on this point.** Implementer: emit `TokenKind::Ident` for every word-shaped token and let Sprint 03 (the parser) distinguish them by string match. The only special-cased keywords in `nod-reader` are:

- `TokenKind::KwDefine` — matched as `define`
- `TokenKind::KwEnd` — matched as `end`
- `TokenKind::KwOtherwise` — matched as `otherwise`

| Variant | Example | Notes |
|---|---|---|
| `Ident` | `foo`, `name-with-dashes`, `<integer>`, `<my-class>`, `*global*`, `$constant`, `+`, `-`, `*`, `/`, `<=`, `==`, `mod`, `add!`, `set?` | See §3.1 |
| `KwDefine` / `KwEnd` / `KwOtherwise` | as written | The three hard reserveds |
| `EscapedIdent` | `\+`, `\=`, `\if`, `\<=`, `\~==` | Backslash-quoted operator-as-name. `text` excludes the leading `\` |

### 2.2 Hash-prefixed tokens

The `#` character introduces a family of distinct kinds (`lexer-transitions.dylan:93-150`):

| Variant | Example | Notes |
|---|---|---|
| `HashTrue` | `#t`, `#T` | `<true-fragment>` |
| `HashFalse` | `#f`, `#F` | `<false-fragment>` |
| `HashLParen` | `#(` | Start of list literal |
| `HashLBracket` | `#[` | Start of vector literal |
| `HashLBrace` | `#{` | Used in macros |
| `HashHash` | `##` | Macro concatenation |
| `HashRest` | `#rest` | Case-insensitive `rR-eE-sS-tT` (`lexer-transitions.dylan:134-139`) |
| `HashKey` | `#key` | Case-insensitive |
| `HashAllKeys` | `#all-keys` | Case-insensitive, including the hyphen (`lexer-transitions.dylan:143-150`) |
| `HashNext` | `#next` | Case-insensitive |
| `Symbol` | `#"foo"`, `#"with-dashes"` | Same body grammar as string literals; routed via the `sharp → string-start` transition |
| `HashKeyword` | `#:foo`, `#:!bang` | `<keyword-syntax-symbol-fragment>` form; alphabetic *or* graphic body (`lexer-transitions.dylan:114-126`). Do not confuse with §2.3 |
| `IntegerBin` | `#b1010`, `#B1111_0000` | Binary, underscores allowed between digits |
| `IntegerOct` | `#o755` | Octal |
| `IntegerHex` | `#xFF`, `#xDEAD_BEEF` | Hex, case-insensitive digits |

### 2.3 Trailing-colon keywords

`foo:` is a distinct lexer token, **not** an identifier followed by a colon. The state machine accepts it via `symbol → colon-keyword → make-keyword-symbol` at `lexer-transitions.dylan:339-344`. Body is the same wide alphabet as identifiers; the terminating `:` is consumed.

| Variant | Example | Notes |
|---|---|---|
| `KeywordColon` | `init-keyword:`, `slot:`, `foo:` | `text` includes the trailing colon for round-tripping; semantic value is the prefix |

A subtle case the upstream lexer handles via the `cname` / `qname` states (`lexer-transitions.dylan:185-452`): `foo:<bar>` and `foo:<bar>:<baz>` form "constrained" and "qualified" name tokens. These are macro-template forms (`?x:expression`, `module:library`) and are **deferred to Sprint 17/18** for our purposes. In Sprint 02, lex `foo:` as `KeywordColon` and then re-enter `start` — the trailing `<bar>` becomes a separate `Ident`. Flag a TODO comment so Sprint 17 can revisit.

### 2.4 Numeric literals

| Variant | Example | Notes |
|---|---|---|
| `Integer` | `0`, `123`, `+789`, `-456`, `1_000_000`, `1_2_3_4` | Decimal. Underscores must be *between* digits — `100_`, `_100`, `1__00` all fail (see `literal-test-suite.dylan:98-100`). Signs `+`/`-` are part of the token only when emitted from the `plus`/`minus` states with a digit following (`lexer-transitions.dylan:204-205, 306-308`); otherwise they are operators |
| `IntegerBin/Oct/Hex` | §2.2 | |
| `Float` | `3.0`, `3.`, `.5`, `3e0`, `3.0e0`, `3.e0`, `+6.`, `-3.0`, `3.0s0`, `30.0s-1`, `3.0d0`, `1.5e-10` | Exponent markers: `e`, `E`, `s`, `S` (single-precision), `d`, `D` (double-precision), `x`, `X` (extended). Optional `+`/`-` on the exponent. Underscores in fraction and exponent permitted (`lexer-transitions.dylan:612-648`). See `literal-test-suite.dylan:103-120` |
| `Ratio` | `3/4`, `-7/8` | Upstream recognises this via `decimal-slash → ratio` (`lexer-transitions.dylan:580-610`). Emit as `Ratio { num, den }`; the runtime decides what to do with it |

Edge case: a bare `.` is `TokenKind::Dot`. A `.` followed by digits enters `decimal-dot-decimal` and yields a `Float` (`.5`).

### 2.5 String literals

Three flavours (`lexer-transitions.dylan:454-569`):

| Variant | Example | Notes |
|---|---|---|
| `String` | `"hello"`, `"line1\nline2"`, `"a\<41>b"` | One-line, escapes processed |
| `StringMulti` | `"""hello"""`, `"""\n  one\n  two\n  """` | Multi-line `"""..."""` with whitespace-prefix stripping (`lexer.dylan:787-918`). Number of `"`s at start and end must match (≥ 3) |
| `StringRaw` | `#r"C:\path\to\file"`, `#r"""..."""` | No escape processing |
| `Symbol` | `#"foo"`, `#"""dashed name"""` | §2.2; lexes through the same string states |

**Escape sequences inside ordinary strings and char literals** (`lexer.dylan:743-758`):

```
\a  \b  \e  \f  \n  \r  \t  \0  \\  \'  \"   \<HHHH>
```

`\<HHHH>` is a hex character escape; `HHHH` is a run of hex digits delimited by `<` and `>` (`lexer-transitions.dylan:461-465`, 492-496). The hex value must fit in `$max-lexer-code` (255) or the lexer signals `<character-code-too-large>` (`lexer.dylan:767-779`). In our implementation, accept the token but emit a diagnostic via the `Invalid`-attached mechanism described in §3 — do not abort.

**The `""` carve-out.** A literal pair of double quotes is *always* the empty string. The `<lexer>` struct carries a `double-quote-start-count` (`lexer.dylan:275-277`), and the `2-double-quotes` state has `make-stringish-literal` *and* a transition on `"` to `multi-string-start` (`lexer-transitions.dylan:478-481`). The accepting action plus the `reset-double-quote-end-count` action in the multi-string body together guarantee `""` accepts as empty before the lexer ever considers it as the leader of a `"""..."""`. Implement this counter explicitly; do not try to be clever with regex.

### 2.6 Character literals

| Variant | Example | Notes |
|---|---|---|
| `Char` | `'a'`, `'\n'`, `'\\'`, `'\<41>'` | Single character or one escape sequence. Empty `''` is invalid; multi-character `'ab'` is invalid (`literal-test-suite.dylan:70-71`). Hex escape same as strings |

### 2.7 Operators and punctuators

Exhaustive list, with upstream states. Maximal-munch always wins (see §3.6).

| Variant | Surface | State |
|---|---|---|
| `LParen` `RParen` | `(` `)` | `lparen` / `rparen` |
| `LBracket` `RBracket` | `[` `]` | `lbracket` / `rbracket` |
| `LBrace` `RBrace` | `{` `}` | `lbrace` / `rbrace` |
| `Comma` | `,` | `comma` |
| `Semicolon` | `;` | `semicolon` |
| `Dot` | `.` | `dot` |
| `Ellipsis` | `...` | `dot-dot → ellipsis` (`lexer-transitions.dylan:179-183`) |
| `Colon` | `:` (standalone) | reached if `colon` state has no follow-on |
| `ColonColon` | `::` | `<colon-colon-fragment>` |
| `ColonEqual` | `:=` | `colon-equal` |
| `Equal` | `=` | `make-equal` |
| `EqualEqual` | `==` | `make-double-equal` |
| `Arrow` | `=>` | `<equal-greater-fragment>` |
| `Tilde` | `~` (unary) | `make-tilde` |
| `TildeEqual` | `~=` | `make-binary-operator` from `tilde-equal` |
| `TildeEqualEqual` | `~==` | `tilde-equal-equal` |
| `Plus` | `+` | `make-binary-operator` (but see §3.x: when followed by digit, folds into signed number) |
| `Minus` | `-` | `make-minus` (`<unary-and-binary-operator-fragment>`) |
| `Star` `Slash` `Caret` `Amp` `Bar` | `*` `/` `^` `&` `|` | `operator-graphic` |
| `Less` `Greater` | `<` `>` (operator role only) | `operator-graphic-pre-equal`. **Almost never reached** — see §3.1 |
| `LessEqual` `GreaterEqual` | `<=` `>=` | `operator-graphic-pre-equal → operator-graphic` |
| `Query` | `?` | `<query-fragment>` (macro pattern variable intro) |
| `QueryQuery` | `??` | `<query-query-fragment>` |
| `QueryEqual` | `?=` | `<query-equal-fragment>` |
| `QueryAt` | `?@` | `<query-at-fragment>` |

Comments are not tokens — they are dropped at accept time (`lexer.dylan:425-431`). The lexer restarts after each comment via `start-over`.

| Internal/skipped | Surface | Notes |
|---|---|---|
| line comment | `// …\n` | One-line; LF, CR, or CRLF terminates (`lexer-transitions.dylan:58-66`) |
| block comment | `/* … */` | **Nests.** See §3.3 |

### 2.8 End-of-file

`TokenKind::Eof` — emitted exactly once at end of buffer. After Eof the lexer is in a sticky state; further calls return Eof.

## 3. Edge cases and traps

### 3.1 `<foo>` is NOT a distinct token

The single most important misconception to clear. `lexer-transitions.dylan:339-342` is the `symbol` state, which accepts on the alphabet:

```
"a-zA-Z0-9!&*<=>|^$%@_+~?/"  plus '-'
```

That is: angle brackets, equals, tildes, plus, etc., are all **identifier-continuation** characters. Once you have started an identifier you stay in it until you hit whitespace, punctuation, or one of the few hard separators (paren, bracket, brace, semicolon, comma, dot, colon, hash, quote, backslash, `\f`).

Consequences the implementer must internalise:

- `<integer>` lexes as a single `Ident("<integer>")` token. There is no `LessThan + Ident + GreaterThan` decomposition.
- `a<b` with no spaces lexes as a single `Ident("a<b")`. To get the comparison you must write `a < b`. This is observable in the upstream test suite.
- `<=` standalone (after whitespace) lexes as `LessEqual` because it starts in the `operator-graphic-pre-equal` state, *not* `symbol`. The transition is `start → "<>" → operator-graphic-pre-equal` (`lexer-transitions.dylan:43`). The difference between operator-`<` and identifier-`<` is *which state you enter first*: from `start` with a leading `<`, you go to the operator path; if `<` appears inside or after another identifier character, it goes to the symbol path.

The IDE should not colour `<foo>` as a "type". It is just an identifier, conventionally used for classes; classification happens at use site.

### 3.2 `~` as operator vs identifier prefix

`~` is one of the operator characters that *can* introduce an identifier (the leading-graphic family includes `~` after the first char). At top level from `start`, `~` goes to the `tilde` state (`lexer-transitions.dylan:309-313`) and accepts as a unary operator immediately. Followed by `=` it becomes `~=`; followed by `==` it becomes `~==`. There is no `~` identifier in source; the `\~` escape (§3.3 below) is the way to refer to it as a name.

### 3.3 Operator-as-identifier — the backslash escape

`\+`, `\-`, `\=`, `\<=`, `\~==`, `\if`, `\#rest`, etc. The backslash-state cluster (`lexer-transitions.dylan:225-305`) walks through every operator and graphic-name shape and emits via `make-quoted-name` (`lexer.dylan:617-626`). The leading `\` is stripped — the token's *name* is the part after it, but its `text` (for round-trip dumps) includes the backslash.

Use `TokenKind::EscapedIdent` so the parser can tell `\+` from a regular `Ident("+")`. They have different semantic roles in Dylan (`\+` is a value-position reference to the binding named `+`; bare `+` is the binary-operator token).

### 3.4 Numeric-prefix ambiguity after `#`

The `sharp` state (`lexer-transitions.dylan:93-108`) dispatches on the character following `#`:

```
#(    → HashLParen           (list literal opener)
#[    → HashLBracket         (vector literal opener)
#{    → HashLBrace
##    → HashHash
#"    → Symbol               (routes to string-start)
#:    → HashKeyword
#b/#B → IntegerBin
#o/#O → IntegerOct
#x/#X → IntegerHex
#t/#T → HashTrue
#f/#F → HashFalse
#n/#N → HashNext (with full `#next` match)
#r/#R → HashRest or StringRaw (#r" enters raw-string-start)
#k/#K → HashKey
#a/#A → HashAllKeys
```

There is no ambiguity between these — the FSM is deterministic on the second character. The trap is that `#` followed by *any other* character is a `TokenKind::Invalid`. Emit a recoverable error token; do not produce a bare `Hash` token.

### 3.5 `#:foo` vs `foo:`

Both are "Dylan keyword-shaped" objects but they are distinct tokens with different uses:

- `#:foo` (`HashKeyword`) is a *keyword value* — it evaluates to a symbol-like keyword object, used in keyword-argument tables. Body grammar is the loose `<keyword-syntax-symbol>` alphabet (`lexer-transitions.dylan:114-126`).
- `foo:` (`KeywordColon`) is a *keyword-argument-name marker* — it tags the next expression as the value for argument `foo` in a call. Body grammar is the identifier alphabet (must start alphabetic or with a graphic-first-char).

Do not collapse them.

### 3.6 Maximal-munch and longest-match

The state machine "doesn't stop at the first accepting state… because the longest token is supposed to take precedence" (`lexer.dylan:344-348`). This is structural: `==` must not lex as two `=` tokens, `:=` must not lex as `:` then `=`, `...` must not lex as `.` `.` `.`. The Rust implementation must mirror the longest-match property exactly. Use a state-driven loop with a "last accepting state" remembered; only commit when no further transition is possible.

### 3.7 Block comments nest

`/* /* foo */ */` is *one* comment. The upstream lexer counts open/close pairs via two slots on `<lexer>` (`lexer.dylan:280-281`) and four conditionally-accepting states (`lexer-transitions.dylan:69-91`). The trap: a naïve `until "*/"` implementation will mis-lex `/* /* */ x */` and treat ` x */` as code.

A small Rust loop with a depth counter is enough; do not try to express it in a regex.

### 3.8 The `""` empty-string carve-out (already discussed in §2.5)

Without the `reset-double-quote-end-count` action, the `multi-string-start` path would speculatively swallow the second `"` and then look for a closing `"""`. The action resets the count so that `2-double-quotes` accepts immediately as the empty string. The implementation: maintain a `dq_count` on the lexer and, when a `"` is seen at start, peek to see whether the third char is also `"`. If yes, enter the multi-line state; otherwise the second `"` closes an empty `String("")`.

### 3.9 Signed numbers vs operators

`+3` lexes as `Integer(+3)` only when the `+` is in token-start position. Inside an expression, `1+2` lexes as `Integer(1)` `Plus` `Integer(2)` because `+` is reached from the `symbol` state's accepting transition, not from a fresh `start`. The upstream lexer enforces this via `plus → signed-decimal` only being reachable from `start` (`lexer-transitions.dylan:306-308`); the maximal-munch property handles the rest.

### 3.10 Floats with letters

`3e10` is a float. `3foo` is **not** a float-with-suffix and **not** an error — it lexes as a single `Ident("3foo")` via the `numeric-alpha` and `leading-numeric` states (`lexer-transitions.dylan:585-592`). This is a real "identifiers can start with a digit" wart of Dylan's lexer that the implementer should be aware of when writing test cases.

## 4. `Span` and source-location encoding

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Span {
    pub file_id: FileId,  // u32 newtype
    pub lo: u32,          // byte offset (UTF-8) into source
    pub hi: u32,          // exclusive
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FileId(u32);
```

A `SourceMap` (kept in `nod-reader::source`) maps `FileId → (path: PathBuf, contents: Arc<str>)`. The IDE's click-to-source resolves a `Span` by:

1. Look up `file_id` → `(path, src)`.
2. Compute `(line, col)` for `lo` by scanning newlines (cached per-file as a `Vec<u32>` of line-start offsets — build once on first use).
3. Open `path` in the editor pane and select `lo..hi`.

Spans are 32-bit. Source files larger than 4 GiB are rejected at load time with a structured diagnostic; this is consistent with NewM2 and NCL.

Line/column information is **not** stored on the token. Tokens stay 24 bytes (kind tag, span, length, optional value index). Line numbers are computed lazily from the source map. This matches the NewCormanLisp pattern.

## 5. `format_tokens` schema

Stable line-oriented dump, one token per line. Schema:

```
<line>:<col>-<line>:<col>  <KIND>  <text-display>
```

- `<line>` is 1-based; `<col>` is 1-based (humans read this).
- `<KIND>` is the `TokenKind` discriminant name, screaming-snake-case.
- `<text-display>` is the source slice, rendered as a Rust-style debug string (quoted, escapes for non-printables). For literals the *parsed* value is appended in parens.
- `Eof` has no text-display.

Worked example. Source:

```
define function sq (x :: <integer>)
  x * x
end function;
```

Dump:

```
1:1-1:7    KW_DEFINE         "define"
1:8-1:16   IDENT             "function"
1:17-1:19  IDENT             "sq"
1:20-1:21  LPAREN            "("
1:21-1:22  IDENT             "x"
1:23-1:25  COLON_COLON       "::"
1:26-1:35  IDENT             "<integer>"
1:35-1:36  RPAREN            ")"
2:3-2:4    IDENT             "x"
2:5-2:6    STAR              "*"
2:7-2:8    IDENT             "x"
3:1-3:4    KW_END            "end"
3:5-3:13   IDENT             "function"
3:13-3:14  SEMICOLON         ";"
4:1-4:1    EOF
```

The dump must be `\n`-terminated, deterministic, and stable across runs. CI compares byte-for-byte against checked-in expected outputs.

## 6. Test-fixture map

All paths are under `E:\NewOpenDylan\opendylan-tests\sources\dfmc\reader\tests\`.

| Fixture | Token kinds exercised |
|---|---|
| `literal-test-suite.dylan` lines 21-29 (`binary-integer-literal-test`) | `IntegerBin`, underscore rules, `Invalid` recovery |
| `literal-test-suite.dylan` lines 31-39 (`boolean-literal-test`) | `HashTrue`, `HashFalse` |
| `literal-test-suite.dylan` lines 41-75 (`character-literal-test`) | `Char`, escapes, hex escape, error paths |
| `literal-test-suite.dylan` lines 77-101 (`decimal-integer-literal-test`) | `Integer`, signed, underscore rules |
| `literal-test-suite.dylan` lines 103-120+ (`float-literal-test`) | `Float`, all exponent markers |
| `literal-test-suite.dylan` (`string-literal-test`, `symbol-literal-test`) | `String`, `StringMulti`, `StringRaw`, `Symbol` |
| `literal-test-suite.dylan` (`ratio-literal-test`) | `Ratio` |
| `comments-test-suite.dylan` | line comments, nested block comments, comment-eats-token boundaries |
| `expressions-test-suite.dylan` | operators, punctuators, identifiers, mixed streams |
| `test-token-classifier.dylan` | per-name classification — we treat all of these as plain `Ident` |

### Five-to-ten minimal smoke tests to land first

These are the ones to bring up before anything else. Each is one fixture call; pass these and the lexer is on its feet.

1. `boolean-literal-test` — `#t`/`#f`.
2. `decimal-integer-literal-test` (lines 78-96 only — skip the negative cases that test recovery).
3. `binary-integer-literal-test` (positive cases, lines 22-25).
4. `character-literal-test` (lines 42-69; skip the `assert-signals` cases for now).
5. `float-literal-test` (lines 104-120, basic forms; defer extended/super precision suffixes).
6. A hand-written **identifier smoke test** verifying `<integer>`, `name-with-dashes`, `make`, `+`, `<=`, `set?`, `add!`, `*global*` all lex as exactly one `Ident` each.
7. A hand-written **operator smoke test** verifying `:=`, `==`, `=>`, `~==`, `::`, `...` all maximal-munch correctly.
8. A hand-written **nested comment** test: `/* a /* b */ c */ x` → exactly one `Ident("x")` plus Eof.
9. The header of `opendylan-tests/sources/testing/cmu-test-suite/dylan-test.dylan` — a real file end-to-end. Used for the SPRINTS Sprint 02 acceptance demo.
10. **Empty-string regression**: source `""` → exactly one `String("")` token plus Eof. Source `"""` (three quotes) → start of a multi-line string, expects more input.

Run these as `tests/nod-tests/reader/*.rs`, one file per fixture group, using the Rust `insta` snapshot crate (already a portfolio dependency in NewM2 and NCL) for the dump output.

## 7. Open questions

Things this spec cannot resolve from the upstream source alone; the implementer should make a call and document it:

1. **`#string` and other `#`-prefixed identifiers not on the upstream list.** The upstream FSM dispatches `#` on `bBoOxXtTfFnNrRkKaA` plus the punctuation forms. Anything else (`#g`, `#z`) is invalid. Confirm the v1 lexer rejects them rather than back-tracking to identifier mode.

2. **Ratio literals.** Upstream has `parse-ratio-literal` (`lexer-transitions.dylan:594`, 609-610). The Dylan runtime does not, to my reading, ship a ratio type. Is `3/4` actually used anywhere in the test corpus, or is it dead syntax? If dead, emit `Invalid` with a `"ratio literals not supported"` diagnostic and skip the variant. *Recommendation: keep the variant in `TokenKind` for completeness but mark the runtime side as a Sprint 25+ concern.*

3. **`numeric-alpha` and `leading-numeric` token shape** (e.g. `3foo`, `1.5z`). These are reached from numeric states and accept as identifiers. Should they be `Ident` or a distinct `WeirdIdent` for the parser to reject? *Recommendation: `Ident`. The parser will refuse them by context.*

4. **8-bit-ASCII identifier bytes.** Upstream accepts `0x80-0xFF` as identifier-continuation. We accept the bytes too, but do we *normalise* (NFC) the resulting UTF-8 sequence? *Recommendation: no normalisation in Sprint 02; treat the byte stream verbatim. Revisit in Sprint 27 when we port the `unicode` library.*

5. **Macro-pattern constraint syntax `?x:expression`, `?x:name`.** The upstream `cname`/`qname` states emit `<constrained-name-fragment>` and `<variable-name-fragment>` tokens (`lexer-transitions.dylan:356-452`). For Sprint 02, the safe choice is to *not* implement these — lex `?x` as `Query` + `Ident`, lex the `:expression` as `KeywordColon` + `Ident`, and let Sprint 17 (the macro expander) reconstruct the constraint syntax from the token stream. The downside is that error spans inside macro patterns will be coarser. *Recommendation: defer. Add a `TODO(sprint-17)` next to the `KeywordColon` accept site.*

6. **`Module:` header comment.** Upstream parses the leading `Module: foo` line as a header-comment block (special preamble form) before lexing proper Dylan. Where does this happen? My read says it is handled *outside* the lexer, in `compilation-record` setup. The Sprint 02 lexer should treat the whole `Header-Key: value\n` preamble as a comment-like preamble and skip it, but this needs verification against `lexer-support.dylan` and `reader.lid`. *Recommendation: implement a tiny preamble pre-pass in `nod-reader` before invoking the state machine. Sprint 04 will re-parse it for the AST root.*
