# Sprint 45 — Dylan lexer in Dylan

**Strategic frame.** Year 3 of NewOpenDylan: the compiler frontend is
implemented in Dylan, not Rust. LLVM remains the backend. This sprint
is the **first concrete step** toward that endpoint — a production-
quality lexer, written in Dylan, exercised first by the IDE.

Not a port of `src/nod-reader/src/lexer.rs`. Designed fresh from a
Dylan perspective. The two implementations diverging on subtle cases
is the **feature**, not a bug — disagreements surface either bugs or
language-spec ambiguities that a single-implementation codebase would
never notice. Both lexers stay alive indefinitely; the oracle test
between them becomes a permanent correctness gate.

---

## 1. Design principles

1. **Tokens are a class hierarchy.** Generic dispatch over enum-tags.
   `colour-of(token)`, `print-token(token, source, stream)`,
   `token-source-text(token, source)` all dispatch on token class.
   No giant `select (kind)` ever appears in consumer code.

2. **Conditions for errors.** The lexer signals `<lex-error>` on
   malformed input. The IDE installs a handler that collects them
   into a diagnostics list; batch consumers let them propagate to
   the driver's top-level handler. No `Result<T, E>` shape.

3. **Lossless lexing.** Comments and whitespace are first-class
   tokens (`<comment-token>`, `<whitespace-token>`). The IDE needs
   them for accurate redisplay and colouring; the parser uses a
   `non-trivia-tokens` filter to skip them. This matches modern
   compiler-as-service designs (rust-analyzer, Roslyn).

4. **Read like Dylan.** `for (token in tokens) end` not
   `tokens.iter().for_each(...)`. Generic methods, not free
   functions where dispatch helps. Idiomatic at the cost of
   sometimes looking different from the Rust lexer.

5. **Reusable, not IDE-bound.** The lexer is its own library-shaped
   thing. The IDE is its first consumer; in year 3, the production
   compiler is its second. The lexer file does NOT know the IDE
   exists.

6. **Transparent / introspectable.** Every phase ships a
   `nod-driver dump-dylan-tokens <path>` command (or similar) that
   prints a human-readable textual representation we can `cat`,
   diff, paste into bug reports, and feed into the oracle test.

---

## 2. Architecture

### 2.1 File layout

```
tests/nod-tests/fixtures/
    dylan-lexer.dylan        — the lexer (one file, ~800 lines)

tests/nod-tests/tests/
    dylan_lexer.rs           — Rust harness that drives the
                               Dylan lexer through the JIT/AOT path,
                               + runs oracle test against nod-reader

src/nod-driver/src/main.rs
    Subcommand DumpDylanTokens — read a .dylan file, lex it with
                                 the Dylan lexer, print human-
                                 readable token dump to stdout
```

The lexer is currently a fixture, not yet a stdlib module. When
multiple consumers materialise (parser in sprint 46, IDE in 45e),
it will graduate to its own library file structure. For sprint 45
the fixture path is enough.

### 2.2 Class hierarchy

```
<token>                        — abstract base
├── <keyword-token>            — `define`, `end`, `if`, …
│                                slot: keyword :: <symbol>
├── <identifier-token>         — `foo`, `<rope-leaf>`, `+`, …
│                                slot: name :: <byte-string>
├── <keyword-name-token>       — `key:` (Dylan keyword-arg form)
│                                slot: name :: <byte-string>
├── <number-token>             — abstract; sub-divided:
│   ├── <integer-token>        — `42`, `#x2A`, `#b101010`, `#o52`
│   │                            slots: value, radix
│   └── <float-token>          — `3.14`, `1.0e-3`
│                                slots: value (or raw text if undecoded)
├── <string-literal-token>     — `"hello\n"`
│                                slots: raw-text, decoded-value
├── <character-literal-token>  — `'a'`, `'\n'`
│                                slot: codepoint :: <integer>
├── <symbol-literal-token>     — `#"foo"`
│                                slot: name :: <byte-string>
├── <boolean-literal-token>    — `#t`, `#f`
│                                slot: value :: <boolean>
├── <nil-literal-token>        — `#nil`
├── <literal-vector-open>      — `#(`
├── <literal-sequence-open>    — `#[`
├── <punctuation-token>        — `(`, `)`, `;`, `,`, `=>`, `::`, …
│                                slot: form :: <symbol>
├── <comment-token>            — `// …` and `/* … */`
│                                slots: text, is-block?
├── <whitespace-token>         — runs of space/tab/newline
├── <error-token>              — recovery sentinel for bad input
│                                slot: message :: <byte-string>
└── <eof-token>                — end of input (one per token stream)
```

Every token has:
```dylan
slot token-span :: <span>, init-keyword: span:;
```

### 2.3 The `<span>` class

```dylan
define class <span> (<object>)
  slot span-start :: <integer>, init-keyword: start:;
  slot span-end   :: <integer>, init-keyword: end:;
end class;

define method span-text (span :: <span>, source :: <byte-string>)
 => (text :: <byte-string>)
  copy-sequence(source, start: span-start(span), end: span-end(span))
end method;

define method span-contains? (span :: <span>, offset :: <integer>)
 => (yes? :: <boolean>)
  offset >= span-start(span) & offset < span-end(span)
end method;
```

Sprint 46+ adds a `file-id` slot when cross-file context arrives.
Slot addition is API-compatible.

### 2.4 Error model

The lexer **emits an `<error-token>`** for bad input AND **signals a
`<lex-error>` condition**. The condition carries:

```dylan
define class <lex-error> (<error>)
  slot lex-error-span    :: <span>;
  slot lex-error-message :: <byte-string>;
end class;
```

Callers choose:
- IDE installs `block / exception (<lex-error>) ...end` that
  appends to a `<diagnostic-list>` and returns to continue lexing
- Batch caller lets it propagate; the driver's top-level handler
  prints `<file>:<line>:<col>: lex error: <message>` and exits non-zero

The `<error-token>` stays in the token stream regardless — the IDE
needs it to render the bad span visually.

### 2.5 Public API

```dylan
// Whole-buffer lex.
define function lex
    (source :: <byte-string>)
 => (tokens :: <stretchy-vector>)
  ...
end function;

// Convenience: non-trivia view.
define function non-trivia-tokens
    (tokens :: <stretchy-vector>)
 => (filtered :: <stretchy-vector>)
  ...
end function;

// Introspection: human-readable textual dump.
define function dump-tokens
    (tokens :: <stretchy-vector>, source :: <byte-string>)
 => (text :: <byte-string>)
  ...
end function;

// Generic methods every token responds to.
define generic colour-of (t :: <token>) => (c :: <integer>);    // RGB
define generic print-token (t :: <token>, source :: <byte-string>,
                            stream :: <stream>) => ();
define generic token-source-text (t :: <token>, source :: <byte-string>)
 => (text :: <byte-string>);
```

---

## 3. Token kinds — complete catalogue

Matches what `nod-reader/src/lexer.rs` recognises today (cross-check
list maintained in the oracle test). Disposition for each:

| Source form | Token class | Notes |
|---|---|---|
| `define`, `end`, `if`, `then`, `else`, `elseif`, `while`, `until`, `for`, `unless`, `when`, `case`, `select`, `block`, `cleanup`, `exception`, `let`, `local`, `method`, `function`, `class`, `slot`, `signal`, `error` | `<keyword-token>` | full list TBD via oracle |
| `foo`, `<rope-leaf>`, `bar?`, `set!`, `+`, `nod-rope-line-count` | `<identifier-token>` | Dylan idents include `<>?!+-*/=` chars |
| `keyword:` | `<keyword-name-token>` | Dylan's slot/keyword-arg form |
| `42`, `0`, `-7` | `<integer-token>` radix=10 | sign as separate `<punctuation-token>` for `-7` (parser combines) |
| `#x2A`, `#X1F` | `<integer-token>` radix=16 | |
| `#b1010`, `#B1010` | `<integer-token>` radix=2 | |
| `#o52`, `#O52` | `<integer-token>` radix=8 | |
| `3.14`, `1.0e-3` | `<float-token>` | |
| `"hello\nworld"` | `<string-literal-token>` | decoded value stored alongside raw text |
| `'a'`, `'\n'`, `'\\'` | `<character-literal-token>` | |
| `#"foo"` | `<symbol-literal-token>` | |
| `#t`, `#f` | `<boolean-literal-token>` | |
| `#nil`, `#()` | `<nil-literal-token>` | special-case the empty literal form |
| `#(` | `<literal-vector-open>` | |
| `#[` | `<literal-sequence-open>` | |
| `(`, `)`, `[`, `]`, `{`, `}`, `;`, `,`, `.` | `<punctuation-token>` | one form per character |
| `=>`, `::`, `:=`, `~=`, `==`, `~==`, `=`, `<`, `>`, `<=`, `>=`, `|`, `&`, `+`, `-`, `*`, `/`, `?` | `<punctuation-token>` | multi-char first |
| `// text` (rest of line) | `<comment-token>` is-block?=#f | |
| `/* text */` (nestable? TBD) | `<comment-token>` is-block?=#t | |
| spaces, tabs, newlines, carriage returns | `<whitespace-token>` | one token per run |
| unexpected byte | `<error-token>` | recovery: skip one byte |
| (end of input) | `<eof-token>` | always last |

**Punctuation philosophy:** every distinct multi-char operator is
ONE token (`=>` not `=` + `>`). The `form :: <symbol>` slot
distinguishes them: `#"arrow"`, `#"colon-colon"`, `#"assign"`,
`#"not-equal"`, etc. The parser dispatches on the symbol.

**Decisions deferred to lexer code** (will be settled during 45b
implementation; documented here for the agent brief):
- Whether `-7` is lexed as one token or `-` + `7` (Rust lexer
  emits it as `Minus` + `Integer(7)`; the parser folds. Dylan
  lexer should do the same for consistency.)
- Whether `/*...*/` block comments nest. (Rust lexer says no.
  Dylan spec is ambiguous. Pick: NO nesting, document choice.)
- Whether `1.0e3` and `1e3` and `1.` are all valid floats.
  (Mirror Rust behaviour; the oracle test will catch drift.)

---

## 4. Stdlib additions

Discovered list (will grow during 45b):

| Function | Sig | Sprint |
|---|---|---|
| `is-ascii-alpha?(b :: <integer>) => <boolean>` | byte in `A-Za-z` | 45c |
| `is-ascii-digit?(b :: <integer>) => <boolean>` | byte in `0-9` | 45c |
| `is-hex-digit?(b :: <integer>) => <boolean>` | byte in `0-9A-Fa-f` | 45c |
| `is-name-start?(b :: <integer>) => <boolean>` | first char of Dylan ident | 45c |
| `is-name-cont?(b :: <integer>) => <boolean>` | trailing chars of Dylan ident | 45c |
| `is-whitespace?(b :: <integer>) => <boolean>` | space/tab/CR | 45c |
| `is-newline?(b :: <integer>) => <boolean>` | byte = 10 (LF) | 45c |

Each is a Rust shim (`nod_is_ascii_alpha_p` etc.) wired through the
standard four-place pattern, with a Dylan wrapper in
`src/nod-sema/src/stdlib.dylan`. Each gets a one-line unit test.

If we discover more needs (a `<string-builder>` is plausible if
naive vector-of-bytes turns out painful), they get queued for
sprint 45c too.

---

## 5. Test strategy

### 5.1 Tests (in increasing scope)

| Layer | Where | What |
|---|---|---|
| Self-lex | `tests/nod-tests/tests/dylan_lexer.rs` | The Dylan lexer lexes its own source file. Asserts: builds OK, lex produces no `<error-token>`s, EOF at end, byte-coverage = source length |
| Unit | same file | Per-token-kind tests: lex `"42"`, assert one `<integer-token>` with value 42, span 0..2 |
| Recovery | same file | Lex `"foo @ bar"`, assert `[<id>, <ws>, <error>, <ws>, <id>, <eof>]`, condition raised |
| Oracle | same file | For each fixture in a corpus, run Dylan lexer + Rust lexer, normalise both to a textual canonical form, diff |
| IDE | manual | Open `nod-ide.dylan` itself, eyeball colouring uses real-token classification not regex |

### 5.2 Corpus for oracle test

Three fixtures, progressively harder:
1. `tests/nod-tests/fixtures/hello.dylan` — a trivial program
2. `tests/nod-tests/fixtures/rope.dylan` — the standalone rope tests
3. `tests/nod-tests/fixtures/unified_ide.dylan` — the 2125-line IDE

If we wanted more breadth, the entire `src/nod-sema/src/stdlib.dylan`
+ `tests/nod-tests/fixtures/*.dylan` corpus is sitting right there.

### 5.3 Normalisation format for oracle

Both lexers emit to a canonical text format:
```
<line>:<col>-<line>:<col>  KIND  text-of-token
1:1-1:7      KEYWORD     define
1:8-1:16     KEYWORD     function
1:17-1:22    IDENTIFIER  hello
1:22-1:23    PUNCTUATION (
...
```

Diff is plain `diff -u`. Any non-empty diff → investigate.
Disagreements between the two lexers are the **bug-finding feature**
this sprint exists to provide.

---

## 6. Phase breakdown

Each phase: ≤ 1 day's work, ≤ 1 commit, ≤ 1 agent invocation.
Every phase ends with a runnable `dump-X` command or test that the
human can inspect.

### 45a — types + spans + dump infrastructure

**Deliverables:**
- `tests/nod-tests/fixtures/dylan-lexer.dylan` skeleton:
  `<span>`, `<token>` + ALL subclasses, `print-token` method per class,
  `colour-of` per class (constants for now — sprint 45e will tune)
- `dump-tokens(tokens, source) => <byte-string>` — produces the
  canonical textual format
- A trivial `lex` stub that returns `[<eof-token>]` so the dump path
  is exercisable
- `nod-driver dump-dylan-tokens <file>` subcommand wired up — runs
  the lex stub, prints the dump

**Acceptance:**
- `cargo run -- dump-dylan-tokens tests/nod-tests/fixtures/hello.dylan`
  prints `1:1-1:1 EOF` (nothing else, because lex is stubbed)
- The dump format is locked in, ready for 45b to fill out

**Introspection output:** the dump command IS the introspection.

### 45b — the real lex function

**Deliverables:**
- Full implementation of `lex(source) => <stretchy-vector>`
- All token kinds from §3 supported
- Error recovery + `<lex-error>` condition + `<error-token>`
- Self-lex passes (the lexer lexes its own source cleanly)
- ~30 unit tests, one per token category

**Acceptance:**
- `cargo run -- dump-dylan-tokens dylan-lexer.dylan` produces a
  long, well-formed token dump
- `cargo test --test dylan_lexer` passes
- Self-lex emits zero `<error-token>`s

### 45c — stdlib character predicates

**Deliverables:**
- The Rust shims listed in §4
- Dylan-side bindings in stdlib
- One test per predicate (existing pattern from byte_string_ops.rs)

**Acceptance:**
- Predicates resolve from Dylan
- All character-predicate tests pass
- 45b's lex implementation switches from inline byte-range checks to
  the named predicates (for readability)

May run BEFORE or interleaved with 45b — 45b's first cut can inline
the byte ranges; 45c lifts them to named helpers.

### 45d — oracle test against Rust lexer

**Deliverables:**
- Rust-side: a `normalize_rust_lexer_output(path) -> String` helper
  that runs `nod_reader::lex` and produces the canonical format
- Dylan-side: the `dump-tokens` from 45a IS the canonical format
- A `cargo test --test dylan_lexer oracle_*` per fixture that runs
  both, diffs, asserts identical
- Documented disagreements (we expect some — language-spec ambiguities)
  go in a per-disagreement comment with a follow-up task

**Acceptance:**
- Oracle test runs against `hello.dylan` and passes
- Oracle test against `rope.dylan` and `unified_ide.dylan` either
  passes OR fails with documented expected disagreements
- Every disagreement has either a fix landing in this sprint or a
  follow-up task with reasoning

### 45e — wire into IDE colouring

**Deliverables:**
- IDE side (`ide_syntax.dylan`):
  - Retire `highlight-dylan-syntax` regex
  - Replace with: lex visible buffer → walk tokens → for each token
    apply colour-of() → emit drawing-effect for that span
- The `colour-of` method specialisations get tuned for IDE display
  (keywords blue, comments green, strings red, numbers purple, etc.
  — same palette as today)
- Whole-buffer relex on every paint (incremental is sprint 47)

**Acceptance:**
- Open `nod-ide.dylan` in the IDE, see real-token colouring
- Stress cases that the regex got wrong:
  - `define` inside a comment stays green (not blue)
  - `"the define keyword"` stays red (not partly-blue)
  - `<rope-leaf>` colours as identifier (or class — whichever
    `colour-of` decides)

---

## 7. Future sprints (informational, not in scope here)

- **Sprint 46** — Dylan parser in Dylan. Recursive descent over the
  token vector. Produces AST classes that parallel `nod-reader::ast`.
  Unlocks go-to-definition.
- **Sprint 47** — Incremental relex. The IDE tracks edits at the
  rope level; lex only the affected token span, not the whole buffer.
- **Sprint 48** — Scope analysis. Knows what names are in scope at
  every cursor position. Unlocks naive autocomplete.
- **Sprint 49+** — Real sema. Type estimates, dispatch resolution,
  unused-var warnings.

When sprint 46 begins, this lexer is its first non-IDE consumer.
That's when it earns its own library file structure (split
`dylan-lexer.dylan` into `tokens.dylan` + `spans.dylan` +
`lex.dylan`, lift to a directory).

---

## 8. Agent briefing template

Each phase gets one agent. The template is:

```
You are implementing Sprint 45<phase> of NewOpenDylan. Read
docs/SPRINT_45_DYLAN_LEXER.md sections 1-3 and the §6.<phase>
deliverables block. Stay STRICTLY within scope; do NOT advance
into later phases.

Constraints:
- Dylan-centric design (§1). No Rust-port idioms.
- All introspection: every phase ends with a runnable text-output
  command we can inspect.
- Components reusable: the lexer file does NOT import IDE-specific
  things. The IDE imports FROM the lexer file in 45e.
- Standard test pattern: tests in tests/nod-tests/tests/dylan_lexer.rs.
- Per the project rule: Dylan-only edits get `cargo build` not a
  full test sweep (memory: dylan_only_changes_skip_test_sweep).
  Rust edits do warrant the sweep.
- Commit when the phase's acceptance criteria pass. Do not push.
```

---

## 9. Open questions reserved for sprint kickoff

1. **Block comment nesting.** Decide and document during 45b.
   Recommend: no nesting (matches Rust lexer; simpler).
2. **Negative-integer lex.** Decide: lex `-` as separate
   `<punctuation-token>`, parser folds. (Matches Rust lexer.)
3. **Identifier byte set.** Dylan allows `<>?!+-*/=` in identifiers.
   Confirm the exact set against the Rust lexer; the
   `is-name-start?` / `is-name-cont?` predicates encode the answer.
4. **`<character-literal-token>` codepoint vs byte.** Dylan source
   is UTF-8. Character literals are conceptually code points.
   For sprint 45 we go ASCII-only (panic on non-ASCII inside `'…'`);
   Unicode handling is a separate sprint with `<character>` design.
