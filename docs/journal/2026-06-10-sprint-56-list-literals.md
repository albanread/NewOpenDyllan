# 2026-06-10 — Sprint 56 (axis-1): `#(…)` list literals in Dylan lowering

*A small lowering-coverage unlock: the Dylan AST→DFM lowering handles `#(a, b, c)`
list literals, emitting the `%nil` / `%pair-alloc` cons chain. Unlocks
`stdlib-size-call`. Notable for a wrong first guess about the AST node.*

## What we did

`#(10, 20, 30)` lowers (lower.rs 5994-6042) to: each element as a `Const`
(source order), then `tail = %nil()` (dst `<class>`), then `%pair-alloc(elt,
tail)` per element RIGHT-to-left (dst `<class>`), returning the head. Mirrored
that in `tests/nod-tests/fixtures/dylan-lower.dylan`.

**The trap (worth recording):** the `dump-ast` output renders the literal as
`Call(Ident("#list"), [a, b, c])`, so the first attempt added a `name = "#list"`
branch *inside* the `<ast-call>` classification chain. It never fired — the
`<ast-call>` branch first requires the callee to be an `<ast-variable-ref>`, and
the Dylan parser does NOT build `#(…)` as a call: `parse-list-literal` builds a
distinct **`<ast-list-lit>`** node (`lit-elems` + `lit-tail`); `#list` is only a
display rendering. The fix was to handle `<ast-list-lit>` at the `lower-expr`
node-dispatch level instead. Lesson: `dump-ast`'s Call-rendering of a literal is
not the node's class — check the parser, not the dump.

Improper lists (`#(a . b)`, `lit-tail` set) bail to Rust (Rust represents them
differently); empty `#()` → just `%nil()`.

## Verification

- Minimal list-literal fixture + `stdlib-size-call` (`size(#(10,20,30))`, where
  `size`→Dispatch already worked) both byte-match Rust `dump-dfm`. The dump has
  no safepoints, so `stdlib-size-call` is **text-gateable** (standalone==rust) —
  added to `PHASE0_LOWER_FIXTURES`, not flip-only.
- Whole-corpus survey: **0 mismatches**; standalone lowered **30→31 / 62**.
- Phase-0 + curated-flip + survey gates all green.

## Where it leaves us

Remaining bails are the macro cluster (needs expander integration — gated behind
the whole-front-end milestone, not a lowering form), the big self-hosting
sources, and the IDE/rope family (other forms). Next clean axis-1 candidates:
bare top-level expressions (`jit_cache_sample`), `#[…]` vector literals
(`<ast-vector-lit>`, the sibling node).
