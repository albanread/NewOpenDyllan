# REPL Loop + Live Bindings — NewOpenDylan Sprint 08 Spec

*Drafted 2026-05-16. Implements Sprint 08 of [`../SPRINTS.md`](../SPRINTS.md). Pre-condition for Sprint 11 (GC) and Sprint 13 (multimethod dispatch) — the binding primitive committed here is what those sprints layer on top of. Authoritative on the redefinition primitive; the broader manifesto commitment lives in [`../MANIFESTO.md`](../MANIFESTO.md) §"Live incremental compilation".*

---

## 1. Status and scope

Sprint 08 lands the **first stateful pipeline stage that survives across input turns** — every prior pass is per-file, batch-mode. After this sprint, `nod-driver repl` is a persistent session that accepts `define …` forms and expressions one at a time, builds them up in a live image, and lets later turns reach the bindings introduced by earlier turns. The same machinery accepts **redefinitions** atomically.

**In scope for v1:**

- `nod-runtime::Binding` cell — the heap-resident, mutable code-pointer carrier for every top-level name.
- `nod-loader` first appearance: the `(library, module, name) → Binding` map, plus a generation counter per binding.
- Codegen change: `DirectCall` lowers to an indirect call through a binding cell, not a direct `call @name`.
- REPL turn lifecycle: parse → lower → codegen → JIT → install-or-update binding.
- Cross-turn reachability: a function defined in turn N is callable from turn N+M.
- Atomic redefinition of `define function` (non-generic). Old code stays in the arena (no GC yet); new calls hit the new code immediately.
- Signature-compatibility check on redefinition. Shape-incompatible changes produce a structured diagnostic and refuse the patch.
- One built-in FFI shim: `format-out(fmt, args…)` lowering to a Rust `extern "C"` thunk that prints to stdout. Real `c-ffi` is Sprint 23.
- `define constant` and `define function` work across REPL lines.
- Arena allocator for any heap data (intentionally leaks; Sprint 11 reclaims).

**Out of scope** (deferred, with explicit pointers below):

- Method redefinition (`add-method` / `remove-method` on generic functions) — Sprint 13. The `Binding` shape leaves room.
- Sealed-direct call invalidation — Sprint 15. Back-reference index is empty until then.
- Old-code reclamation — Sprint 11 GC. Until then, every redefinition leaks the previous machine code into the arena.
- Class redefinition — explicitly **refused** by the REPL in v1. A previously-defined class name cannot be re-`define class`-d in the same session. The CLOS-style lazy-migration story is a multi-sprint feature on its own.
- Macro redefinition — Sprint 17+. v1 REPL refuses to redefine a macro within a session.
- Hot-reload of cross-library exports — Sprint 29 / library-merge territory.
- File-watch / save-triggered recompilation — manifested in [`../MANIFESTO.md`](../MANIFESTO.md) but the trigger sits in the IDE work (Sprint 26+); the *loader* primitives land here.

---

## 2. The `Binding` primitive

Every top-level name (`define function`, `define constant`, `define variable`, eventually `define generic` and `define method`) is materialised as a heap-resident `Binding`. The binding is the unit of redefinition; it is the only thing that can be atomically swapped.

```rust
// nod-runtime::binding
pub struct Binding {
    pub name: Symbol,                       // interned via nod-namespace's interner
    pub library: LibraryId,
    pub module: ModuleId,
    pub kind: BindingKind,
    pub signature: Signature,               // canonical shape — see §5
    pub code: AtomicPtr<u8>,                // machine-code entry; updated atomically
    pub generation: AtomicU64,              // bumped on every successful redefinition
    pub source_span: Span,                  // most-recent definition site
    // Sprint 13 will add:
    //   methods: AtomicPtr<MethodTable>    // for BindingKind::Generic
    // Sprint 15 will add:
    //   sealed_dependents: RwLock<Vec<CallSiteId>>  // back-references
    // Sprint 11 will add:
    //   retired_code: Vec<RetiredCodeRegion>        // GC pin list
}

pub enum BindingKind {
    Function,                               // define function (non-generic)
    Constant,                               // define constant
    Variable,                               // define variable
    Generic,                                // Sprint 13
    Class,                                  // Sprint 12; immutable post-creation in v1
    Macro,                                  // Sprint 17; immutable post-creation in v1
}

pub struct Signature {
    pub param_types: Vec<TypeEstimate>,
    pub return_type: TypeEstimate,
    pub modifiers: Vec<Modifier>,           // open / sealed / inline / sideways
}
```

**Invariants the `Binding` upholds:**

1. The `code` field holds either a null pointer (newly-allocated, not yet installed) or a valid pointer into JIT-mapped executable memory whose lifetime is at least the lifetime of the binding.
2. `generation` is monotonically non-decreasing across the binding's lifetime. Every successful redefinition bumps it by exactly 1.
3. `signature` is fixed at first install. Redefinition that would change `signature` is **refused**, not allowed-with-cascade — see §5.
4. The binding itself never moves in memory once installed. Its address is stable for the life of the session. This is what makes compiled callers' indirect-call cells valid forever.

**Allocation.** Bindings live in a dedicated arena (`nod-runtime::binding_arena`) — Sprint 08 leaks; Sprint 11 turns this into the pinned-static area described in [`../MANIFESTO.md`](../MANIFESTO.md) §"The garbage collector". `Box::leak` for v0.1 is acceptable; the allocator must guarantee pointer stability.

---

## 3. Codegen change: `DirectCall` through binding cells

The Sprint 07 codegen emits, for a `Computation::DirectCall { callee: "sq", args }`:

```llvm
%call = call i64 @sq(i64 %x)
```

Sprint 08 changes this to indirect:

```llvm
@sq.binding = external constant ptr                ; address of the Binding for `sq`
...
%cell = load ptr, ptr @sq.binding                  ; load Binding's `code` field
%call = call i64 %cell(i64 %x)
```

The `@sq.binding` global is bound at JIT-install time to the Rust-side address of the `Binding` struct (via inkwell's `add_global_mapping`). The `code` field of the binding sits at a known offset inside that struct. So the indirect load is one memory access against a stable address, regardless of how many times `sq` is redefined.

**Why one indirection instead of two.** A simpler design is: each name → a global function-pointer slot; codegen loads that slot. Equivalent for non-generic functions. But generic function dispatch in Sprint 13 needs to read the binding *and* its method-table generation in the same go, which means the binding has to be the addressable thing. So we pay the one-indirection cost up front and reuse the design for generics.

**Performance.** Modern x86-64 indirect-branch prediction makes this effectively free for a stable binding — the branch target buffer learns the destination after one call. Cost shows up only when redefinition actively churns the cell, which is exactly when we want it to happen (rare, user-triggered).

**Recursive calls.** A function calling itself goes through its own binding. Redefining `factorial` mid-recursion means the *next* recursive call sees the new code; in-flight frames finish the old code. This matches the manifesto.

**Forward declaration.** Codegen still does the Sprint 07 two-pass: pass 1 forward-declares every function and emits its `@<name>.binding` global; pass 2 emits bodies. Bindings for names introduced in earlier REPL turns are looked up in the loader's map; bindings for names introduced in *this* turn are created during pass 1 and installed at JIT-link time.

---

## 4. REPL turn lifecycle

```
turn input (one or more top-level forms or one expression)
        │
        ▼
nod-reader::lex + parse_module
        │
        ▼
nod-sema::lower_module                ← Sprint 06
        │
        ▼  Vec<nod_dfm::Function>
        │
        ▼
For each function in the lowering:
    │
    ├── nod-loader::lookup_binding(name)  → Option<&Binding>
    │
    ├── If absent: allocate a new Binding (signature from lowering).
    │             Install in loader map.
    │
    ├── If present: signature-compare. If incompatible, return
    │              StructuredDiagnostic::SignatureChangeRefused.
    │              Refuse the WHOLE turn — atomic at the turn level.
    │
    └── In either case, codegen against the (possibly fresh) Binding
        address.
        │
        ▼
nod-llvm::codegen_module
        │
        ▼
nod-llvm::Jit::add_module (one fresh inkwell module per turn)
        │
        ▼
For each newly-codegen'd function `f`:
    │
    └── Atomically store the JIT'd code pointer into f.binding.code,
        bump f.binding.generation by 1.
        │
        ▼
If the turn was an expression rather than a definition:
    │
    └── Wrap in synthetic `<repl-N>` function, JIT, call once, format
        the result. Discard the binding (or keep it under a hidden name
        for `:redo` later — implementer's call, document in DEFERRED).
```

**Turn-level atomicity.** A turn either fully installs or fully rejects. If it would install 3 new functions and 1 redefinition, and the redefinition fails signature-compatibility, none of the 3 new functions install. This requires staging: build all bindings into a side buffer, validate all, then commit. The commit step is a sequence of atomic stores; it is NOT itself atomic across multiple bindings, but the validation guarantees no commit-step can fail by the time we start committing.

**Concurrent calls.** A redefinition store can race with a JIT'd function reading the cell. This is fine: `AtomicPtr::store` with `Ordering::Release` paired with `AtomicPtr::load` with `Ordering::Acquire` on the call side guarantees the new code is fully initialised by the time the cell points at it. Inkwell's call instruction must be marked `volatile` only if we want to forbid the optimiser from caching the load — which we do across loop iterations, so we mark it volatile.

**REPL meta-commands** (Sprint 08 deliverables `:dump-dfm <name>`, `:dump-llvm <name>` from the revised SPRINTS.md): look up the binding by name, walk to its stored DFM/LLVM textual representation (the loader keeps a per-binding `last_dfm: Option<String>` for diagnostic purposes), print.

---

## 5. Signature compare and refusal

Two definitions of the same name are **signature-compatible** iff:

- Same `BindingKind`.
- Same `param_types.len()`.
- Each `param_types[i]` is *layout-compatible* with the prior. For Sprint 08's kernel-subset type lattice (`Integer`, `SingleFloat`, `DoubleFloat`, `Boolean`, `Character`, `String`, `Unit`, `Top`, `Bottom`), layout-compatibility is exact equality except: `Top` is compatible with anything (placeholder for unannotated types).
- `return_type` follows the same rule.
- `modifiers` agree on `inline` vs. `not-inline`. Other modifiers (`sealed`, `open`) are recorded but do not block redefinition in Sprint 08; the sealed-domain consequences are Sprint 15's problem.

**On a signature-incompatible redefinition**, the loader emits:

```
error: redefinition of `foo` changes its signature
  original: (Integer, Integer) -> Integer       (turn 4, generation 1)
  new:      (Integer, String)  -> Integer       (this turn)
  hint: rename the new function or restart the REPL
```

The turn is refused; the old binding's `code` and `generation` are untouched.

**Why refuse rather than cascade-invalidate-and-recompile.** Cascade invalidation requires the back-reference index that Sprint 15 introduces for sealing. Until then, we have no way to find callers that compiled against the old signature. Refusing is the conservative move and matches the manifesto's "structured diagnostic and refuses the patch" wording for sealed-domain violations.

**Sprint 13 will extend** this to handle `add-method` / `remove-method`: those are signature-stable on the *generic*, but mutate its method table. Method-table mutation will land as `bump generic-binding.generation` + atomic `MethodTable` pointer swap.

---

## 6. What stays leaked (until Sprint 11 GC)

Every successful redefinition leaves three carcasses in the arena:

1. **The old machine code** at the address the binding *used* to point at. Still mapped executable, still cold — but nothing calls it (every caller goes through the binding cell, which now points at new code). Stack frames executing it at the moment of the swap finish naturally.
2. **The old DFM IR** (if the loader cached it per-binding for `:dump-dfm` purposes). Sprint 08 may either cache or not — implementer's call, but if cached, that cache leaks across redefinitions until Sprint 11.
3. **The old inkwell `Module`** that hosted the prior codegen — its memory and metadata. inkwell's lifetime model ties this to the `Context`, which the JIT engine keeps alive for the session. Negligible per-turn but it accumulates.

For a Sprint 08 user — multi-hour REPL session, hundreds of redefinitions — this is fine. It is **not** fine for a long-running production process. Sprint 11 GC will:

- Scan all `Binding.code` fields, build the live-pointer set.
- Walk all thread stacks (via `gc.statepoint` stack maps) for in-flight return addresses, add those to the live set.
- Anything in the JIT-mapped pages that is neither in the live set nor pinned by an execution scope (see §7) is freed.

Until then, the arena grows. Document this prominently in `--help` output and `DEFERRED.md`.

---

## 7. What this design must leave room for

Three forward-looking concerns the Sprint 08 binding shape has to accommodate without forcing a redesign later.

### 7.1 Method-table mutation (Sprint 13)

When `BindingKind::Generic` lights up, the binding gains a `methods: AtomicPtr<MethodTable>` field. `add-method`/`remove-method` allocate a new `MethodTable`, swap atomically, bump the generation. Inline caches at call sites read both the method table pointer AND the generation in the same lockless read; cache invalidation is generation-compare-and-retry.

The Sprint 08 `Binding` reserves space for this. Recommended: keep `Binding` `#[repr(C)]` and put `code` + `generation` + `methods` in adjacent slots so JIT'd dispatch lowerings can compute the load offsets cheaply.

### 7.2 Sealed-direct call back-references (Sprint 15)

When the optimiser lowers a sealed-domain dispatch to a direct call (skipping the binding indirection for performance), that call site has no cell to read from. Redefinition of a method participating in that sealed shape has to:

1. Identify every direct call site that depended on the sealing fact.
2. Invalidate those call sites (patch to fall back through the binding) or recompile the callers.
3. *Then* apply the redefinition.

For this the binding needs `sealed_dependents: RwLock<Vec<CallSiteId>>`. Sprint 08 leaves this empty — no optimiser pass currently emits sealed-direct calls. Sprint 15 turns it on. The Sprint 08 binding shape **must** include the field (initialised empty) so Sprint 15 doesn't have to migrate every existing binding.

### 7.3 Class redefinition (deferred indefinitely)

Class redefinition is fundamentally different: instances of the old layout exist on the heap. Three options later sprints might pick:

1. **Forbid** (v1's choice). `define class <foo>` is one-shot per session.
2. **Lazy migration** (CLOS-style). Every instance carries a class-version stamp; access through the slot accessor checks against the current class version and migrates on touch. Needs a class-version cell in the class metaobject and a slot-layout-translation function generated at redefinition time.
3. **Whole-heap migration**. Stop the world, walk every instance, allocate the new layout, copy with translation. Coarse but simple.

We have not committed to a path. The Sprint 08 binding for `BindingKind::Class` carries no class-redefinition machinery — class redefinition in the REPL returns `RedefinitionRefused::ClassRedefinitionNotSupported`. When we pick a path, the class binding grows the fields it needs and the REPL un-refuses.

---

## 8. Test plan

Tests live in `tests/nod-tests/tests/repl.rs` (new file). Use the existing `env!("CARGO_MANIFEST_DIR")` ancestors-walk idiom.

| # | Test | Verifies |
|---|---|---|
| 1 | `single_define_then_call` | `define function sq (x :: <integer>) x * x end;` then `sq(7)` returns `49`. Two-turn REPL session via a programmatic `Repl` driver, no actual stdin reading. |
| 2 | `cross_turn_visibility` | Turn 1 defines `sq`; turn 2 defines `quad` calling `sq`; turn 3 calls `quad(3)` and gets 81. Confirms the binding map persists. |
| 3 | `redefinition_atomic` | Define `f` returning 1. Redefine `f` returning 2. The first definition's binding generation is 1; the second's is 2. Calling `f()` after redefinition returns 2. |
| 4 | `redefinition_does_not_break_caller` | Define `f` returning 10. Define `caller` calling `f` and returning `f() + 1` (= 11). Redefine `f` returning 20. Call `caller` — gets 21. Confirms the indirect-through-binding lowering works. |
| 5 | `signature_change_refused` | Define `f (x :: <integer>) => <integer>`. Try to redefine `f (x :: <string>) => <integer>`. Expect `RedefinitionRefused::SignatureChanged`. Then call `f(7)` — still returns the original value (binding untouched). |
| 6 | `format_out_works` | Defines a function that calls `format-out("answer: %d\n", 42)`; output buffer (captured via redirected stdout or a test-only FFI shim) contains `"answer: 42\n"`. |
| 7 | `class_redefinition_refused` | Define `<point>` class (Sprint 12 stub or skipped if Sprint 08 lands before Sprint 12 — see §10 OQ4). Try to redefine. Expect refusal. |
| 8 | `generation_monotonic` | Redefine a function five times. Final binding generation is 5. |
| 9 | `recursion_through_binding` | Define `factorial`. Confirm the LLVM IR emitted for it shows an indirect call through `@factorial.binding` (not `call @factorial`). |
| 10 | `repl_dump_dfm_meta_command` | Define `f`; `:dump-dfm f` returns the DFM text of the most-recent installation. Define `f` again; `:dump-dfm f` returns the new text. |

The `Repl` driver is a programmatic API — `Repl::new() -> Repl`, `Repl::turn(input: &str) -> TurnResult` — so tests don't need a TTY. The `nod-driver repl` subcommand thinly wraps it with a line reader.

---

## 9. Edge cases and traps

### 9.1 Self-referential definition in the same turn

```
define function f (n) if (n = 0) 1 else f(n - 1) * n end;
```

Pass 1 must allocate `f`'s binding before pass 2 codegens its body, otherwise the recursive `f(n - 1)` has nothing to bind to. This is the same forward-declaration pattern Sprint 07 already uses for mutual recursion within a single codegen module — extend it across REPL turns: every binding in the turn is allocated up front, then bodies codegen, then JIT.

### 9.2 Turn defines two functions where the second redefines a previously-installed name

```
turn 1: define function f () 1 end;
turn 2: define function g () 2 end;
        define function f () 3 end;   // this is a redefinition
```

Stage all bindings, validate all (the redefinition of `f` against turn-1's binding must signature-check), commit all atomically. If `f`'s redefinition is rejected, `g` does NOT install. The user gets a single diagnostic for the failing redefinition.

### 9.3 Definition of a name that conflicts with an `import:`-ed binding

`define module` declarations from Sprint 04 + namespace resolution from Sprint 05 (deferred to Sprint 06 per `DEFERRED.md`) determine whether a name refers to a local binding or an imported one. The REPL operates in an implicit `dylan-user` module by default. If a turn defines `format-out` (which is imported from elsewhere), the REPL **shadows** the import with the local binding — same semantics as a normal Dylan module that defines a name it also imports. v1 surfaces a warning ("shadowing imported binding from module M") but does not refuse.

### 9.4 Expression turns vs. definition turns

A turn whose input is `1 + 2 * 3` is an *expression turn*. The REPL wraps it in a synthetic `<repl-N>` function (where `N` is the turn number), JITs it, calls it once, prints, and discards. The synthetic binding is hidden from the user's namespace but kept in a per-session ring buffer (16 entries) so a future `:redo` meta-command can re-run. This ring is a Sprint 08 deliverable only if cheap; otherwise document as a "future" in `DEFERRED.md`.

### 9.5 Aborting a turn mid-execution

User hits Ctrl-C while a JIT'd function is mid-call. Sprint 08 leaves this **unhandled** — the process dies. Real interrupt handling needs cooperative safepoints (Sprint 11) and exception machinery (Sprint 19). Document.

### 9.6 Memory leak rate

A pathological tight loop that redefines `f` once per second leaks ~1 KB of arena per redefinition (a fresh JIT'd `Module`'s metadata + the new code page). After 24 hours that's ~85 MB. Acceptable for the Sprint 08 window. Mention in `--help` output.

---

## 10. Open questions

Things this spec cannot resolve from prior commitments alone. Implementer decides and documents.

1. **Per-binding DFM cache.** Should `Binding` carry the last-installed DFM textual dump (for `:dump-dfm <name>` and time-travel debugging) or should the loader keep a separate `Name → DFM` map? *Recommendation: separate map, owned by `nod-loader`, indexed by binding pointer. Keeps `Binding` lean and cache-friendly.*

2. **Inkwell module-per-turn vs. one growing module.** Each REPL turn could codegen into a fresh inkwell module (clean lifetime, harder to cross-reference earlier-turn definitions) or extend a single growing module (cheaper, but inkwell's module-merge story is awkward). *Recommendation: module-per-turn, with the JIT engine holding all of them; cross-turn calls work because they go through binding cells, not through LLVM-level symbol resolution.*

3. **REPL ring buffer for expression turns.** Implement `:redo` and the 16-entry history in Sprint 08 or defer to Sprint 26 IDE work? *Recommendation: defer the ring buffer; build the synthetic `<repl-N>` naming but discard immediately. The IDE will rebuild this on top of its own session log.*

4. **Class-redefinition test (`class_redefinition_refused`).** Sprint 08 ships before Sprint 12 (classes). The test cannot run end-to-end until Sprint 12 lands. Either: (a) write the test now with a `#[ignore]` and a comment, (b) defer the test to a Sprint 12 amendment. *Recommendation: (a), so the deferred-test list is concrete.*

5. **Signature compatibility across `Top` placeholders.** A user-defined function with no type annotations gets every param typed as `Top`. After Sprint 12 adds richer types, a redefinition that *narrows* `Top` to `Integer` is arguably compatible. Is narrowing allowed silently, with a warning, or refused? *Recommendation: silently allowed for v1; warn in v2 when the type lattice is rich enough to distinguish principled narrowing from accidental.*

6. **Atomicity of cross-turn definition order.** Per §9.2, a turn that defines `g` then redefines `f` rejects the whole turn on `f`'s signature change. But what about a turn that defines `g` (calling `f`) then redefines `f` to a *signature-compatible* but semantically-broken body? `g` was codegen'd against the old `f`'s ABI; the ABI is unchanged; `g` works. Semantics drift is the user's problem. Document this explicitly so reviewers don't expect the loader to detect it.

7. **Binding-cell address stability across `cargo build` reruns.** A persisted REPL session (Sprint 26+) might want to checkpoint bindings to disk and restore. The binding cell's runtime address won't be stable across process restart. v1 doesn't checkpoint. When checkpointing lands, bindings need a *symbolic* identity (Symbol) distinct from their *runtime address*. The `Binding::name + library + module` triple is that symbolic identity. v1 already populates it; no future-work hook needed.

---

*Companion to [`../SPRINTS.md`](../SPRINTS.md) Sprint 08. Sealing-aware redefinition lands in [`15-sealing-and-dispatch-invalidation.md`](15-sealing-and-dispatch-invalidation.md) (not yet written); method-table mutation lands in [`13-multimethod-dispatch.md`](13-multimethod-dispatch.md) (not yet written). Both will reference the binding shape committed here.*
