# NewOpenDylan — roadmap to a usable Dylan

Sequenced by **unlock-per-effort**, grounded in the 2026-06-16 language-coverage
audit and the corpus-blocker analysis (87 failing of 161 language/stdlib files:
59 lowering, 16 parse). **Out of scope:** DUIM (GUI, non-portable) and, deferred,
the platform-specific libs it drags in — OLE/COM, raw Win32 UI, network/HTTP
servers, file-system *locators*. We target the portable language + core stdlib.

Effort key: **S** ≈ a day or two · **M** ≈ a week · **L** ≈ 2–4 weeks · **XL** ≈ months.
"Unlocks" = rough corpus files freed (single-file `dump-dfm`), where measurable.

> **Implementation principle — macros first.** When a feature *can* be a Dylan
> macro in `stdlib/macros.dylan` (expanding over primitive special forms +
> runtime primitives), do that instead of growing the Rust parser/lowerer. Keep
> the Rust core to primitives + the macro engine; grow the surface in Dylan.
> Non-macro work (must stay in Rust): lexer/parser primitives, `#key`/`#rest`
> binding, dispatch, GC, codegen, FFI, and the runtime primitives macros expand
> onto. Items below are tagged **[macro]**, **[rust]**, or **[mixed]**.

> **Recon notes (2026-06-16, corrected stale audit):** `select`/`case`/`when`/
> `unless`/`cond`/`when-let`/`if-let`/`iterate` already work (parser desugar or
> existing macros). `handler-case`/`handler-bind` as *forms* have **0** corpus
> uses — the condition tests use the `block … exception …` primitive directly
> (79 files), which works — so don't bother macro-izing them for the corpus.
> A recurring real gap: operator function-refs in **call** position
> (`\=(a,b)`, `select … by \=`, `disjoin(\>)`) lower to an unresolved
> `DirectCall` and also miss `<=`/`>=` shims — a small **[rust]** lower_call fix
> (route `\op(args)` to the inline op / shim funcall). Belongs in Tier 1.

Status snapshot (audit): language-core ~65–70%, full platform ~40%. GC correct
as of 2026-06-15. Corpus compile 79/161 (per-file).

---

## Tier 1 — Quick wins (close audit gaps, free easy corpus files)

- **1a. Parser edge cases — S, unlocks ~5–8.**
  Operator/punctuation method names (`binary=`, `test->=`, `=hash`, `\<`),
  no-`end` definer forms (`define benchmark x = expr;`), adjacent-string-literal
  folding, keyword-symbol in value position. Buckets: the 16 parse failures.
- **1b. `select` lowering — S.** Parsed but stubbed; lower to the same
  decision-tree path `case` already uses.
- **1c. Tractable stdlib fns — S/M.** `subtype?` (6 refs; needs CPL access),
  `push`/`push-last`/`pop` on `<deque>`/`<stretchy-vector>`, `as` (coercion),
  `type-for-copy`, and the arg-root-coverage gaps in `reduce`/`map`/`do`/
  `concatenate` flagged by the audit.
- **1d. Numeric low-hanging fruit — S/M.** Don't *error* on integer overflow
  (promote, or at least a clean condition); float exponent literals (`1.5e-3`);
  stub/port a few transcendentals (`sqrt`, `abs`, …) where pure-Dylan.

## Tier 2 — Keyword arguments end-to-end — M, broad value

`#key` is parsed but not bound in the lowerer, not passed at call sites, and not
used in dispatch. Pervasive in real Dylan + stdlib APIs (`make(<x>, foo: …)`
partially works via `init-keyword`; general `#key` does not). Wire: lowerer
binds `#key`/defaults/`#all-keys`; call sites build keyword frames; dispatch
ignores keywords for selection (per DRM) but binds them. Also: optional-param
defaults, `#key`+`#rest` together, explicit `next-method(args…)`.

## Tier 3 — Multi-file (single-library) compilation — M/L, unlocks ~10–15

**Infrastructure landed (2026-06-16):** `dump-dfm` now accepts multiple files
and compiles them as one unit (AST-merge → single expand+lower), so
intra-library cross-file references resolve (`$tiny-size` & friends — the
bit-vector cluster compiles clean as a unit; the files fail individually). The
`.lid` `Files:` lists give the true library groupings (`/tmp/cc-lid.sh`).

**Remaining:** the per-library pass count is still low because one buggy file in
a library (a `*-utilities` / harness file using an unsupported feature) poisons
the whole-library compile. So the headline metric stays per-file (79) until the
support files in each library compile cleanly — which is the Tier 4–6 long tail,
not a Tier 3 problem. Net: the cross-file *plumbing* is done; the unlock now
depends on grinding the remaining per-file errors *within* each library. (Full
*cross*-library separate compilation + module encapsulation enforcement is
Tier 7.)

## Tier 4 — Condition system depth — M

`signal`/`error`/`handler-case`/`block`-exit work; missing the rest of the DRM
protocol: `handler-bind`/`let handler`, the restart protocol
(`<restart>`/`invoke-restart`/`return-allowed?`/`abort`, currently a stub that
panics), and `cerror`. Needed for real programs and a faithful testworks.

## Tier 5 — Type-system depth — L

`limited(<integer>, …)`, `type-union`, `singleton`, `subclass` types in the
dispatch lattice; slot-type enforcement at store time; introspection
(`object-class`/`class-of`/`class-name`/`class-direct-superclasses`/slot
descriptors). Reconnect the numeric tower so `instance?(1, <number>)` etc. hold.

## Tier 6 — Standard-library port — XL, the bulk of remaining corpus

Where most of the other ~half of the corpus unlocks. Port, Dylan-first:
- `common-dylan` (the everyday prelude), `machine-words` (`<machine-word>`),
  `transcendentals`.
- `io`: `<stream>` family, `print`/`format`/`format-out`, `print-object`.
- `collections`: `<deque>`, `<set>`, concrete `<array>`/`<vector>`,
  `<byte-vector>`, `<unicode-string>`, the full collection-generic surface.
- `system` (date/file-system/locators — portable parts only).
- **testworks** harness end-to-end (`define test/suite/benchmark`, `check-*`,
  `with-test-unit`, suite runner) — currently partial; this alone gates the
  bodies of most test files.
- **Numeric tower proper**: `<big-integer>`/bignum arithmetic, ratios, complex.

## Tier 7 — Full library/module system — XL, the "real platform" milestone

Separate compilation + linking per library (`.lid` boundaries, one object per
library), and *enforcing* `use`/`export`/`import`/`exclude`/`rename`/`prefix`
during name resolution (today the headers parse but encapsulation isn't
enforced). Cross-library sealing/`add-method` checks. This is what turns the
compiler from "single-unit" into a Dylan *platform*.

---

## Sequencing rationale

Tiers 1–2 are cheap and close concrete audit gaps. Tier 3 is the highest
single-lever corpus unlock that isn't a full library port. Tiers 4–5 deepen
correctness for real programs. Tier 6 is the long tail (and the bulk of corpus
%). Tier 7 is the structural finish line. 1–5 are weeks each and unblock
incrementally; 6–7 are the months-long majority of "finished," but neither is
blocked on an unsolved problem — it's well-understood engineering.
