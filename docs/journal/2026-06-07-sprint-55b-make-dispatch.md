# 2026-06-07 — Sprint 55b: make + dispatch (first class programs in Dylan)

*The first full class programs — `point.dylan`, `gc_precise_two_makes.dylan`,
`translate-class.dylan` — now lower in Dylan and byte-match through the flip.
This is the form that needed the flip: `make` carries a process-global class-id,
`Dispatch` is rewritten by the host resolver, and both functions carry
host-populated safepoints. None of it is text-gateable; all of it byte-matches
once the host runs the same passes on the reconstructed DFM.*

## The class-id problem, and the clean way through

`make(<C>, …)` emits `Const ClassMetadataPtr(<id>)` — a process-global id the
Dylan lowering can't know (it runs BEFORE the host registers classes). Two
realizations made this tractable without a new runtime primitive or a reorder:

1. **Param/var class TYPES don't need the real id.** A `p :: <user-point>` param
   prints `<class>` (via `TypeEstimate::name()`, which drops the id). The
   post-passes that touch `point` don't depend on the id either: `x`/`y` are
   *unsealed* slot generics, so the resolver leaves them as `Dispatch`; and
   `needs_gc_protection` is true for any `Class(_)`. So the Dylan side types a
   user-class param as bare `<class>` → reconstructs as `Class(0)` → still
   prints `<class>`. No resolution needed.
2. **`make` emits the class BY NAME; the parser resolves it.** The Dylan dump
   writes `ClassMetadataPtr(<user-point>, tagged=false)`. The reconstruction
   (`parse_dfm_module`) runs INSIDE lowering, AFTER `analyse_module` has
   registered the classes, so `resolve_class_id_by_name("<user-point>")` → the
   live id (1081). `parse_const` now accepts a non-numeric `ClassMetadataPtr`
   payload and resolves it (numeric ids still round-trip unchanged).

So the only Rust change is a few lines in `parse_const`. Everything else is in
the Dylan lowering.

## What the Dylan side emits

- **`make`** → `Const ClassMetadataPtr(<C-name>, …)`, then interleaved
  `Const SymbolLiteralRef("kw")` + lowered value per keyword arg (key via
  `keyword-name-token-name`, colon stripped), dst minted last, then
  `DirectCall %make(class_ptr, sym0, val0, …)` dst `<top>` (mirrors `lower_make`).
- **Generic calls** (slot getters `x(p)`, …) → `Dispatch g(args)` dst `<top>`,
  EMPTY safepoint set — the host liveness pass populates it, and the resolver may
  rewrite to Direct/SealedDirectCall (it doesn't for unsealed `point`).
- **Class-typed params/returns/constants** → `<class>` (via a new
  `label-for-type-name` + a module-wide `build-user-class-names` set, threaded
  through `lower-function` / `build-name-ret-map` / `lower-constant-defn`).
- `make` is no longer an "intrinsic bail" (the dead `is-lower-intrinsic?` is
  removed); a generic call now emits a Dispatch instead of bailing.

## Verification

`point` (make + slot dispatch + class param), `gc_precise_two_makes` (two makes,
populated cross-make safepoints, `x(a)`/`x(b)` dispatches), and `translate-class`
all byte-match `dump-dfm` through `--lower-with-dylan`. They're **flip-only**
(safepoints + dispatch resolution make the standalone pre-pass dump differ), so
they join `FLIP_ONLY_LOWER_FIXTURES`. `richards-shape` still bails — it uses
`define method` / sealed classes (method-body lowering + the sealed resolver are
the next pieces). `sema_topnames` 6/6, `codegen` 8/8, all prior fixtures
unregressed; the `parse_dfm_module` round-trip + a new
`classmetadataptr_by_name_resolves` unit test cover the parser change.

## Where it leaves us

`make`, single-dispatch generic calls, slot accessors, `instance?`, and the
straight-line/control-flow core all lower in Dylan and feed the back-end. The
Dylan front end is load-bearing through lowering for non-method class programs.
Remaining 55b: `define method` bodies (named `g$classid`) + `define generic`,
which unlock the sealed-class resolver path (richards) and slot-`:=`
(`<slot>-setter` dispatch). Then 55c (closures / blocks).
