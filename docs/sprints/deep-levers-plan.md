# Deep-levers implementation plans (design → challenge → synthesis)

Source: a multi-agent workflow (2026-06-14) that, for each remaining deep corpus
lever, ran a **design** agent, an **adversarial challenge** agent (tasked with
finding the AOT-consistency traps that reverted earlier attempts), and a
**synthesis** agent producing a go/no-go plan. Captured here so the next focused
effort can execute without re-deriving.

> Status at capture: `main` clean at the head that ships iters 8–13 (corpus compile
> 58/161, in-tree 55/55). None of the plans below are landed yet — see THE GATE.

## THE GATE — fix the cold-build `LNK2005 nod_user_main` first

All three AOT levers (block-return, func-refs, collection-classes) need a **built
exe** to verify, and AOT EXE linking is **broken whenever `nod-runtime`'s CGU
partition colocates the `nod_user_main` stub** (`aot_user_main_stub.rs`) with an
always-pulled object. A warm/incremental `main` often links; *any* non-trivial
`nod-runtime` edit (e.g. the block-return fix) re-partitions and trips
`LNK2005 nod_user_main already defined` / `LNK1169`, so smoke-aot and every `build`
fail. Confirmed three times (agents B, C, and the block-return verification).

- **`codegen-units = 1` is NOT the fix** (the stub doc claims it gives "one CGU per
  archive member" — that is backwards; `=1` is ONE object for the whole crate, so
  the stub is *always* pulled → LNK2005 always). Do not pin it.
- **`/FORCE:MULTIPLE` is the wrong instrument** (silences ALL duplicate-symbol
  errors, masking real ODR bugs). Only acceptable as a throwaway to get a one-off
  runnable exe during manual verification.
- **Robust fix, option A (recommended): a dedicated stub crate.** Move
  `nod_user_main` into a tiny new crate `nod-aot-stub` (one fn, its own object).
  As a separate crate it can never be CGU-merged with `aot.rs`, so MSVC on-demand
  archive extraction works as designed: pulled only when `nod_user_main` is
  unresolved (cargo test), never when the user supplies it (the AOT EXE link).
- **Robust fix, option B: `/ALTERNATENAME`.** Rename the stub to
  `nod_user_main_default` and add `/ALTERNATENAME:nod_user_main=nod_user_main_default`
  to the AOT link step (`main.rs`). Fiddlier: the same flag must also reach the
  `cargo test -p nod-runtime` and `cargo build -p nod-driver` links (whichever
  actually reference `nod_aot_main_wrapper`) via build-script `rustc-link-arg`.
- After fixing, add a `block (return)` case to `tools/smoke-aot.sh` as a permanent
  guard, and the LNK2005 case is gone for fresh clones / CI / self-rebuild too.

## 1. block-return  — GO (priority 1, ~50 LOC, challenge: sound)

`block (return) … return(x) … end` crashes in a built AOT exe. **Three compounding
bugs** (the fix is already drafted; re-apply from this plan, then build+RUN-verify):

1. **AOT-specific root bug:** `nod_run_block` truncated only the JIT safepoint
   shadow stack on a non-local exit, never the parallel AOT thread-local stack
   `aot::ACTIVE_AOT_SAFEPOINTS` (entries go stale when `panic_any(NlxPayload)`
   unwinds through AOT Dylan frames that skip their `nod_aot_end_safepoint`
   epilogues); the unconditional fast-path assert at `aot.rs:564` then crashes.
   Fix: add `aot::truncate_active_aot_safepoints` + capture an
   `aot_safepoint_baseline` in `nod_run_block` and `CleanupGuard` (symmetric with
   the existing JIT truncate; a verified no-op in pure-JIT).
2. **Masked diagnostic:** the AOT safepoint shims are `extern "C"` (nounwind); a
   panic across them aborts with "panic in a function that cannot unwind". Fix:
   `extern "C-unwind"` (LLVM decls are attribute-free → ABI byte-identical).
3. **block_id overflow (affects BOTH JIT and AOT):** `lower.rs` ORed bit 62 →
   value ≥ 2^62 > FIXNUM_MAX, so `make_exit_procedure`'s `Word::from_fixnum`
   panics. Fix: mask to 61 bits, OR bit 61 → `[2^61, 2^62-1]`.
   Plus crash-dump robustness: `try_borrow` in the safepoint-depth readers; panic
   hook ignores `NlxPayload` (so a *successful* `block(return)` doesn't spew a
   crash dump). Also delete dead `allocate_block_id` (conditions.rs) + fix stale
   comments.
- Files: `lower.rs` (block_id), `aot.rs`, `conditions.rs`, `crash_dump.rs`,
  `stack_map.rs`.
- Verify: build+RUN `trial(5)→99`, `trial(1)→7`, block-with-cleanup→1, exit 0,
  clean stderr; negative control (drop the aot-truncate hunk) double-panics;
  gabriel `puzzle` runs end-to-end. **Blocked on THE GATE for the build step.**

## 2. func-refs-aot — needs-revision (priority 2, ~60 LOC, mechanism sound)

`\==`, `\~=`, `instance?` as *values* crash a built exe (`no registered function
==`) — the shims are JIT-only. Fix: add `nod_op_eq_eq` (identity `==`), `nod_op_ne`
(value `~=` — **compare `nod_object_equal_p`'s returned Word to the `true_`
immediate and invert**, do NOT clone the identity path), `nod_op_ne_eq`,
`nod_instance_p` (untag the class Word → `ClassMetadata.id` → `nod_is_instance_of_word`)
in `functions.rs`, registered in `ensure_operator_shims_registered` (which DOES run
in AOT startup); re-export in `lib.rs`; add `== ~= ~== instance?` to
`operator_arity` in `lower.rs`. Caveats: `make_function_ref` checks
`is_generic_defined` first (latent shadowing trap if a stdlib `==` generic ever
lands — document it); rebuild `-p nod-runtime`. Verify: smoke-aot cases
`apply2(\==,3,3)→#t`, `\~=("a","a")→#f` (proves value not identity),
`instance?` value applied. **Blocked on THE GATE.**

## 3. collection-classes — needs-revision (priority 2, large; phase it)

Register `<vector>/<array>/<simple-vector>/<byte-vector>/<bit-vector>` as
**pure-Dylan `define class` forms** in a new stdlib file — NOT Rust
`ensure_registered` (that caused the iter-10 class-id drift). Stdlib classes mint
`FIRST_USER` ids in the shared load and replay via `nod_aot_register_user_class`
with the agreement assert, never perturbing the Rust seed band — the same route
`<stream>`/`<string-stream>` already AOT-run. **Must `tools/self-rebuild.sh`** after
adding stdlib classes (the prebuilt shim bakes class ids → drift on the default
path until rebuilt), or validate only via `--parse-with-rust`.
- **`<bit-vector>` ops are a sub-library, not one iteration.** The design's "ops in
  pure Dylan" half is broken — three confirmed fatal corrections:
  1. `make(<bit-vector>)` must be a **Rust redirect in `lower_make`** (mirror the
     `<table>`→`nod_make_table` arm); `define method make` is dead code
     (`lower_make` intercepts at the call site, `initialize` drops keywords).
  2. `instance?(x,<vector>)` currently emits an **exact id==9** SOV check, so
     `instance?(bitvector,<vector>)`→#f; route `<bit-vector>` (and user
     `make(<vector>)` results) through `ClassCheck::UserClass`/`nod_is_instance_of`
     (CPL walk), keeping the SOV fast path.
  3. **Word-level bitwise primitives don't exist** (`logand/logior/logxor/lognot/
     ash`) — add them as Rust primitives + `lower.rs` PRIMITIVE rows first.
  - **Phase A (do first):** classes + `<bit-vector>` Rust `make` redirect + the
    small primitive set (`%bit-vector-allocate/-ref/-set!/-size/-word`, `%bit-count`,
    word bitwise) + `element/-setter/size/set-bits/unset-bits/bit-count` Dylan
    generics + the `instance?` CPL fix; validate with built-exe `make(<bit-vector>)`
    + `bit-count` + `instance?(bitvector,<vector>)`.
  - **Phase B (follow-up):** `bit-vector-and/or/xor/andc2` (multi-value, `pad1:/pad2:`
    keywords), variadic `vector(...)`, `limited(...)`. **Also blocked on THE GATE
    for built-exe validation.**

## 4. test-macro keyword heads — GO but DEPRIORITIZE (priority 7, ~0 ROI)

Add a `{ define test ?name:name ?opts:parameter-list ?body:body end }` rule to
`test`/`suite` in `macros.dylan`, placed AFTER the `()` rule and BEFORE the
catch-all. **Caveats:** (a) ~0 corpus compile gain — every sampled keyword-head
file immediately hits downstream stdlib gaps (`<semaphore>`, `<recursive-lock>`,
`$single-pi`); (b) **silent miscompile hazard** — a body whose first statement is a
parenthesized expression is swallowed as `?opts` and dropped (corpus exposure NIL
today); proper fix needs a NEW `keyword-property-list` pattern kind that requires a
KeywordColon — do NOT edit the shared `ParameterList` matcher (broad blast radius).
(c) A local trace found the keyword head also aborts the *initial* module parse on
the KeywordColon before expansion — reconcile with the agent's claim that the rule
alone suffices before relying on it. Not worth doing before the AOT levers.
