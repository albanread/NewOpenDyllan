# 2026-06-07 — Sprint 55b: call-path soundness (the unknown→DirectCall trap)

*A whole-corpus survey after the make/dispatch commit found 4 NEW mismatches
(`gap-007`/`gap011` repros). Enabling `make` let those fixtures lower past the
`make` they previously bailed on — and they walked straight into a latent
unsoundness: the Phase-0 call path emitted a `DirectCall` for ANY callee name,
which is wrong for stdlib generics and `%`-primitives.*

## The trap

`dump-dfm` for the repros showed the divergences:
- `size(v)` / `add!(v, i)` → Rust `Dispatch` (they're generics); we emitted
  `DirectCall`.
- `%make-stretchy-vector(...)` → Rust `DirectCall nod_make_stretchy_vector` (a
  `%`-prim maps to a `nod_` runtime name); we emitted `DirectCall %make-...`.
- `v :: <stretchy-vector>` param → Rust `<class>`; we typed `<top>`.
- a `=> ()` function with `let i = 0; while … end` → Rust bare `Return`; we
  returned the `let`'s temp.

The first two are the same root cause: **the Dylan side can't classify a
non-local callee.** `format-out` (a stdlib *function*, correctly `DirectCall`),
`size` (a *generic*, must be `Dispatch`), and `%make-stretchy-vector` (a `%`-prim,
a `DirectCall` to a renamed symbol) are indistinguishable to it without a runtime
generic-registry query — which needs a Dylan-callable primitive the compiler
doesn't expose yet. `is_generic_defined` exists in `nod-runtime` but isn't a
shim-callable intrinsic.

## The fix: only DirectCall *known top-level functions*

The call path now emits a `DirectCall` **only** when the callee is a known
top-level `define function` (in the `ret-map`). Slot generics → `Dispatch`;
`make`/`instance?` → their intrinsics; everything else → **bail to Rust**. The
old "unknown ident → DirectCall" was an unsound Phase-0 shortcut that only
happened to work for non-generic stdlib functions; this makes the call path
sound. Also fixed the void bug: a function is void iff it's declared `=> ()`
(`defn-is-void?`), not merely when the last statement produced no value.

## Cost + verification

The whole-corpus survey is back to **0 mismatches** (19 fixtures Dylan-lowered
through the flip, 23 bail, 3 Rust-error). `hello` (format-out) and
`translate-loop` (done/consume — undefined idents that Rust legitimately
`DirectCall`s) now bail too — collateral of the conservative rule — so they leave
the gates until a generic-name primitive (+ `%`-prim name mapping) lets the
Dylan side classify non-local callees. The class programs (`point`,
`gc_precise_two_makes`, `translate-class`) keep working (they call only
top-level functions, slot generics, and `make` of user classes).
`sema_topnames` 6/6, `nod-dfm` 12/12, `codegen` 8/8.

## Lesson

The byte-match GATE was green (the repros aren't gated), but the INVARIANT —
"never emit a wrong dump for ANY corpus fixture" — was violated. The
whole-corpus survey is the real safety net, not the curated gate; run it after
any change that widens what the Dylan lowering accepts. A new form that lets
previously-bailing fixtures lower further can expose latent unsoundness
downstream.

## Where it leaves us

The next real unlock is a **generic-name primitive** (shim-callable
`is-generic?`) plus the `%`-prim → `nod_` name map. Together they'd let the call
path emit `Dispatch` for generics and `DirectCall nod_…` for prims — restoring
`hello`/`translate-loop` and unlocking the `gap`/`rope`/`ide` fixtures. That, and
`define method` / `define generic` (method bodies + the sealed resolver, for
richards), are the substantial remaining 55b/55c work.
