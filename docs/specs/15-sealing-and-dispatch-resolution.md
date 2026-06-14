# Sealing Analysis + Compile-Time Dispatch Resolution — NewOpenDylan Sprint 15 Spec

*Drafted 2026-05-18. Implements Sprint 15 of [`../SPRINTS.md`](../SPRINTS.md). Builds on Sprint 13 (multimethod dispatch + monomorphic inline caches) and Sprint 14 (MI + slot layout + next-method). Authoritative on the sealing semantics and the dispatch-resolution algorithm; the keystone document for Dylan's performance story.*

---

## 1. Status & scope

Sealing is the language feature that lets Dylan's "feels dynamic" surface compile down to "performs static". A sealed class cannot be subclassed across library boundaries; a sealed generic cannot gain methods across library boundaries; a sealed domain declaration locks down a specific dispatch shape. **The compiler exploits these guarantees at the call site:** when a `Computation::Dispatch` has type estimates that — under the active sealing facts — admit exactly one applicable method, the optimiser rewrites it to `Computation::DirectCall` with no inline cache, no dispatcher, no indirection.

**In scope for v1:**

- Parsing already lands in Sprint 04 — the `Modifier` enum carries `Sealed`, `Open`, `Abstract`, `Concrete`, `Primary`, `Free`, `Inline`, `NotInline`, `Sideways`, `Domain`. `define sealed domain` is captured as `Item::DefineOther { keyword: "domain", body_fragments }`.
- This sprint **acts on** those parsed modifiers. Sealing facts get recorded on `ClassMetadata` and `GenericFunction`; standalone `define sealed domain` declarations get their own table.
- New `nod-opt` crate (or a `nod-opt::dispatch` module in `nod-sema`) — the dispatch-resolution pass that consults sealing facts plus the type-estimate lattice and rewrites Dispatch → DirectCall when justified.
- `TypeEstimate::Class(ClassId)` lights up — currently deferred from Sprint 13. The type-estimate lattice grows narrowing rules (instance? guards, slot-type-implied, method-specialiser-implied).
- Redefinition-refusal extended: redefining a sealed class, adding/removing methods on a sealed generic, or breaking a sealed-domain assumption all return a structured diagnostic.
- CLI: `nod-driver dump-sealed` lists sealing facts; `dump-dispatch` annotates each call site as `sealed-direct` vs `cached` vs `cold`.
- Single-library scope — Sprint 15 only checks sealing within the currently-compiling library. Cross-library sealing requires the back-reference index that lands in Sprint 29.

**Out of scope** (deferred, with pointers below):

- Cross-library sealing checks. The infrastructure (back-reference index per generic) sits empty in Sprint 15; Sprint 29's library-merge optimisation populates it.
- Sealed-direct call invalidation on REPL-side method addition. Sprint 15 refuses such additions outright when they break a sealing assumption; Sprint 29 adds the cascade-invalidate-and-recompile path.
- Full inlining of sealed-direct call targets. Sprint 15 lowers Dispatch → DirectCall but the body still gets called through the function pointer. Inlining the body is Sprint 18 optimisation work.
- `unbound` slot defaults and `slot-allocation: virtual` — Sprint 12 stubbed those; Sprint 15 doesn't touch them.
- `sealed` on individual *methods* — Dylan allows method-level `sealed` modifiers; v1 reads them but doesn't act (use generic-level or domain-level sealing instead).

---

## 2. The four kinds of sealing

Dylan has four sealing primitives. Each generates a different fact for the dispatch resolver to consult.

### 2.1 `sealed class <C>`

```dylan
define sealed class <circle> (<shape>)
  slot radius :: <integer>, init-keyword: radius:;
end class;
```

**Semantic guarantee:** no class outside this library may have `<circle>` in its CPL. The compiler can therefore treat "is instance of `<circle>` or subclass" as equivalent to "is instance of `<circle>` directly" for the purpose of method applicability — **inside this library**.

**Fact recorded:** `ClassMetadata::sealed: bool`. Set when the `sealed` modifier is present on `define class`. The default (no modifier present, or `open` present) is `false`.

**Implication for dispatch:** when computing applicable methods on a generic where one specialiser is `<circle>` and the receiver's type estimate is `<= <circle>`, the optimiser can stop searching subclasses; the receiver IS exactly `<circle>` or a subclass declared in this library (which it can enumerate).

### 2.2 `sealed generic`

```dylan
define sealed generic area (s :: <shape>) => (<integer>);
```

**Semantic guarantee:** the set of methods on `area` is closed to additions from outside this library. The compiler can resolve `area(x)` at compile time once it knows enough about `x`'s class to pick a method, without worrying that another library will install a more-specific method.

**Fact recorded:** `GenericFunction::sealed: bool` (`AtomicBool` to match the existing `generation: AtomicU64` field's atomicity). Set by `define sealed generic` and unsettable.

**Implication for dispatch:** when computing applicable methods, the optimiser knows the method table is closed — no late-arriving method can disqualify the current resolution.

### 2.3 `define sealed domain g (<A>, <B>);`

```dylan
define sealed domain area (<shape>);
```

**Semantic guarantee:** no method on `area` whose specialiser tuple `(S0)` satisfies `S0 <: <shape>` may exist outside this library. This is the most fine-grained sealing — you can leave the generic itself unsealed (so other libraries can extend it with methods on `<other-shape>`) while pinning down dispatch shape inside a specific portion of the type lattice.

**Fact recorded:** a per-library `Vec<SealedDomain>` where each entry is `{ generic_id: GenericId, specialiser_tuple: Vec<ClassId> }`.

**Implication for dispatch:** when a `Dispatch` node's type estimates all fall inside some sealed domain `(S0, S1, …, Sn)` (i.e. each `arg_estimate[i] <: Si`), the resolver enumerates all methods M whose specialiser tuple is also `<: (S0, …, Sn)` — that set is complete and closed. The resolver picks the most-specific applicable one (per Sprint 13's CPL-driven specificity) and rewrites to DirectCall.

### 2.4 Default openness

Without any modifier, classes are **open** and generics are **open**. A new library may subclass `<shape>` with `<triangle>` and add `area(<triangle>) => …`. The dispatch resolver cannot prove single-method applicability and leaves the call as `Dispatch` with the inline cache.

**Fact recorded:** absence of `sealed = true` on the class or generic; absence of a covering sealed domain.

---

## 3. Where sealing facts live

### 3.1 On `ClassMetadata` (`nod-runtime/src/classes.rs`)

Extend Sprint 09/12/14's `ClassMetadata`:

```rust
pub struct ClassMetadata {
    pub name: String,
    pub id: ClassId,
    pub parents: Vec<ClassId>,
    pub cpl: Vec<ClassId>,
    pub slots: Vec<SlotInfo>,
    pub slot_origin: Vec<ClassId>,
    pub own_slot_count: usize,
    pub inherited_slot_count: usize,
    pub instance_size: usize,
    pub scan: ScanFn,
    pub size_of: SizeFn,
    // Sprint 15 additions:
    pub sealed: bool,
    /// In-library subclasses known at this class's registration time.
    /// Populated as later `define class`es land. The dispatch resolver
    /// reads this to enumerate "every possible subclass of <C>" when
    /// `<C>` is sealed.
    pub direct_subclasses: RwLock<Vec<ClassId>>,
}
```

`direct_subclasses` is populated when a child class registers — each `define class <Child> (<Parent>) …` appends `ChildId` to `<Parent>::direct_subclasses`. The dispatch resolver uses this to do bounded enumeration when needed.

### 3.2 On `GenericFunction` (`nod-runtime/src/dispatch.rs`)

Extend Sprint 13:

```rust
pub struct GenericFunction {
    pub name: String,
    pub methods: RwLock<Vec<Method>>,
    pub generation: AtomicU64,
    // Sprint 15 additions:
    pub sealed: AtomicBool,
    /// Sealed-domain declarations covering this generic. Each entry is
    /// a specialiser tuple; a Dispatch falling under any entry can be
    /// resolved using only the methods this library has installed.
    pub sealed_domains: RwLock<Vec<Vec<ClassId>>>,
}
```

### 3.3 Per-library sealing table

A new `nod-namespace::SealingFacts` struct or `Library` extension records standalone `define sealed domain` declarations. Single-library scope means each compilation unit has its own table; Sprint 29 merges them across libraries.

```rust
pub struct SealingFacts {
    /// `define sealed domain g (<A>, <B>);` — keyed by generic name.
    pub domains: HashMap<String, Vec<Vec<ClassId>>>,
    /// Generic names declared `sealed`.
    pub sealed_generics: HashSet<String>,
    /// Class names declared `sealed`.
    pub sealed_classes: HashSet<String>,
}
```

`nod-sema` populates this during lowering (`Item::DefineGeneric` / `Item::DefineClass` with the `Sealed` modifier; `Item::DefineOther { keyword: "domain", ... }`). The optimiser reads from it.

---

## 4. Type-estimate lattice — Sprint 15 strengthening

Sprint 06's `TypeEstimate` has `Top, Bottom, Integer, SingleFloat, DoubleFloat, Character, Boolean, String, Unit`. Sprint 13 deferred `Class(ClassId)`. **Sprint 15 lights it up.**

```rust
pub enum TypeEstimate {
    Top,
    Bottom,
    Integer,
    SingleFloat,
    DoubleFloat,
    Character,
    Boolean,
    String,
    Unit,
    /// Sprint 15: receiver is known to be an instance of this class or
    /// a subclass.
    Class(ClassId),
    /// Singleton — receiver is known to be EXACTLY this value (e.g.
    /// `#f` after an `if`-test branch). Sprint 17+ territory; carry the
    /// field but don't populate it yet.
    Singleton(Word),
}
```

**Lattice operations:**

- `TypeEstimate::join` (used at if-join, phi nodes): widens; e.g. `Class(<circle>) ⊔ Class(<square>)` = `Class(<shape>)` if they share `<shape>` in their CPLs, else `Class(<object>)`.
- `TypeEstimate::meet` (used at narrowing): intersects; e.g. `Class(<shape>) ⊓ Class(<circle>)` = `Class(<circle>)` (the more specific wins).
- `TypeEstimate::is_subtype_of(target: ClassId) -> bool`: returns `true` iff every concrete instance compatible with the estimate is `<: target`. The dispatch resolver's load-bearing predicate.

**Narrowing rules — where new type estimates come from:**

1. **Method specialiser narrowing.** Inside a method body `define method foo (p :: <circle>, …)`, the parameter `p` has `TypeEstimate::Class(<circle>)` for its entire scope.

2. **`instance?` guard narrowing.** After `if (instance?(p, <circle>)) then-branch else else-branch end`, the `then-branch` sees `p :: Class(<circle>)`; the `else-branch` sees `p :: <not Class(<circle>)>`. The "not" case is harder to represent — for v1, skip the else-branch narrowing (over-conservative, sound). The then-branch narrowing is the one that matters.

3. **Slot-type narrowing.** A slot declared `slot x :: <integer>` has known type `<integer>`; reading it yields a temp with `TypeEstimate::Integer`. Reading `slot p :: <point>` yields `TypeEstimate::Class(<point>)`.

4. **Class-allocation narrowing.** `make(<circle>, …)` returns a temp with `TypeEstimate::Class(<circle>)` (exactly — the receiver class is the literal class passed to make).

5. **Direct-call return type narrowing.** `Sprint 06's TopNames` already records return types; carry them through as type estimates. A function declared `=> (<circle>)` returns a temp with `TypeEstimate::Class(<circle>)`.

**The narrowing pass** is a forward dataflow analysis on the DFM, conservative at joins. Sprint 15 implements it as a single function-local pass (no inter-procedural type inference yet); Sprint 18+ adds whole-program propagation.

---

## 5. The dispatch resolution algorithm

This is the heart of Sprint 15. Implemented in `nod-opt::dispatch` (new module). Input: a function's DFM after type-estimate narrowing. Output: the same DFM with `Computation::Dispatch` nodes rewritten to `Computation::DirectCall` where justified.

### Algorithm

For each `Computation::Dispatch { generic_name, args, .. }` node:

1. **Look up the generic.** `let g = sealing_facts.get_generic(generic_name)?;` If unknown, leave as Dispatch.

2. **Read arg type estimates.** `let est: Vec<TypeEstimate> = args.iter().map(|a| temp_type(a)).collect();`

3. **Check the closure condition.** The method set is closed at this call site iff:
   - `g.sealed == true`, OR
   - every `est[i]` is `<: Si` for some sealed domain `(S0, …, Sn)` on `g`, OR
   - every `est[i]` is `Class(C)` where `C.sealed == true` (the receiver class itself is sealed, so no late subclass can show up).

   If none of these hold, the resolver cannot guarantee closure. Leave as Dispatch.

4. **Enumerate applicable methods under the closure.** Walk `g.methods` (Sprint 13's `RwLock<Vec<Method>>`). For each method `M`, check whether `M` is guaranteed applicable: for every position `i`, `est[i] <: M.specialisers[i]`.

   The "guaranteed" qualifier matters: `est[i] = Class(<shape>)` does NOT guarantee `<: <circle>` — a `<shape>` instance might be a `<triangle>`. Use `is_subtype_of`.

5. **Pick the most-specific.** Sort applicable methods by Sprint 13's CPL-driven specificity. If one is strictly more specific than every other (per position) → unique winner. If multiple are equally specific → ambiguous; leave as Dispatch and emit a `:warning:` diagnostic (the runtime dispatcher will panic at call time anyway).

6. **Rewrite.** Replace the `Computation::Dispatch { generic_name, args, safepoint_roots }` with `Computation::DirectCall { callee: <method-body-symbol>, args, safepoint_roots }`. Preserve safepoint roots.

7. **Record the dependency.** Add an entry `(this call site, generic_id, current_generation)` to a per-call-site index. When the generic's generation later bumps, the index tells us this call site needs invalidation. **Sprint 15 doesn't yet implement the invalidation cascade** (it's Sprint 29 cross-library work); the index goes into a global table and stays unused until then. But populate it now so Sprint 29 doesn't have to migrate every existing direct call.

### Example walkthrough

```dylan
define sealed class <shape> (<object>) end class;
define sealed class <circle> (<shape>) slot radius :: <integer>, init-keyword: radius:; end class;
define sealed class <square> (<shape>) slot side :: <integer>, init-keyword: side:; end class;

define sealed generic area (s :: <shape>) => (<integer>);
define method area (c :: <circle>) => (<integer>) c.radius * c.radius * 3 end;
define method area (s :: <square>) => (<integer>) s.side * s.side end;

define function total (c :: <circle>, s :: <square>) => (<integer>)
  area(c) + area(s)
end function;
```

In `total`:
- `c :: Class(<circle>)` (method specialiser narrowing).
- `s :: Class(<square>)`.
- `area(c)`: `area` is sealed; `est = [Class(<circle>)]`; closure holds; enumerate methods on `area`; both methods are checked; only `area(<circle>)` is applicable (`Class(<circle>) <: <circle>`); unique; **rewrite to DirectCall area_circle_method**.
- `area(s)`: same logic; **rewrite to DirectCall area_square_method**.

**Result:** `total` lowers to two direct calls. No inline cache. No `nod_dispatch`. The Sprint 13 cache slots for these call sites stay zero (never written).

The DEFM dump (`dump-dfm`) annotates each rewritten call:
```
t0: <integer> = DirectCall area_circle_method(c)        ; sealed-direct
t1: <integer> = DirectCall area_square_method(s)        ; sealed-direct
t2: <integer> = PrimOp AddInt t0 t1
```

### Counter-example

If `<shape>` were `open`:

```dylan
define open class <shape> (<object>) end class;
```

…then closure fails (no sealing fact covers `area`), the optimiser leaves `area(c)` as `Dispatch`, and Sprint 13's inline cache takes over at runtime. **Soundness rule: when in doubt, don't resolve.**

---

## 6. Redefinition refusal (single-library Sprint 15 scope)

Sprint 12 already refuses class redefinition outright. Sprint 15 extends refusal to sealing-related mutations:

| Operation | Within defining library | Across library boundary |
|---|---|---|
| `add-method` to sealed generic | Allowed (compile-time only — REPL gives a diagnostic) | **Refused** (Sprint 15: same, single-library Sprint 15 means we only see in-library calls so cross-library is moot) |
| `add-method` whose specialisers fall inside a sealed domain | Allowed | **Refused** |
| `define class` extending a sealed class | Allowed (the new subclass is within-library) | **Refused** |
| Removing a class declared `sealed` | Refused (Sprint 12 already does) | n/a |

The cross-library cases are moot in Sprint 15 because we don't model cross-library compilation yet — every library is its own compilation unit. Sprint 29 wires the cross-library check.

REPL-side: `add-method` against a sealed generic returns `MethodTableError::SealedGenericClosed`. Sprint 08's Binding-cell redefinition machinery surfaces this as a structured diagnostic.

---

## 7. CLI surface

### 7.1 `nod-driver dump-sealed`

```
$ nod-driver dump-sealed <input.dylan>
Sealing facts in `mylib`:

  Sealed classes (3):
    <shape>     direct_subclasses=[<circle>, <square>]
    <circle>    direct_subclasses=[]
    <square>    direct_subclasses=[]

  Sealed generics (1):
    area  (1 specialiser, 2 methods)

  Sealed domains (0):
```

### 7.2 `dump-dispatch` annotations

```
Generic area (generation=2, 2 methods, sealed):
  method (<circle>) → 0x...
  method (<square>) → 0x...

Call sites:
  site#0 in total: sealed-direct → area_circle_method  ✓ (sealing resolved at compile time)
  site#1 in total: sealed-direct → area_square_method  ✓
  site#2 in maybe_area: cached  class=<circle> method=… hits=42 misses=1
                       (caller's receiver type is <shape>, not sealed-narrowable)
```

### 7.3 `dump-dfm` annotation

Add `; sealed-direct` trailing comment on rewritten DirectCalls so the dump is self-explanatory.

---

## 8. Test plan

Tests live in `tests/nod-tests/tests/sealing.rs` (new file). Use `#[serial]` on every test that touches the process-global class/generic registries.

| # | Test | Verifies |
|---|---|---|
| 1 | **Sealed-class + sealed-generic + single-method generic → DirectCall** | The headline case. Verify via `dump-llvm` that the JIT IR has no cache load and a single `call @method_body` for an `area(circle)` site. |
| 2 | **Two sealed methods over disjoint specialisers → both resolve** | `area(<circle>)` and `area(<square>)`; two call sites; both lower to distinct DirectCalls. |
| 3 | **Open class + sealed generic → still Dispatch** | The receiver type can't be narrowed beyond `<shape>`; multiple subclasses possible; leave as Dispatch. |
| 4 | **`define sealed domain` covering an open generic → resolves** | `area` is open but `define sealed domain area (<shape>);` is declared; in-library calls with `<shape>` receivers resolve. |
| 5 | **`instance?` guard narrows the type estimate** | `if (instance?(s, <circle>)) area(s) else area(s) end` — the then-branch's `area(s)` lowers to `DirectCall area_circle_method`. |
| 6 | **Slot-type narrowing** | `slot c :: <circle>` followed by `area(self.c)` lowers to `DirectCall area_circle_method`. |
| 7 | **Sealed generic refuses out-of-domain add-method** | Adding a method to a sealed generic from a different (simulated) library returns `MethodTableError::SealedGenericClosed`. |
| 8 | **Sealed class refuses subclassing from another library** | Simulated cross-library `define class <triangle> (<shape>)` returns `LoweringError::SealedClassExtendedAcrossBoundary`. |
| 9 | **Sprint 13 cache is bypassed** — verify by reading the cache slot's `hits`/`misses` counters: a sealed-direct call site has zero of both (because no cache was generated for it). |
| 10 | **Redefining a sealed class is refused** (closes Sprint 12 stub). |
| 11 | **Type-estimate join at if-merge** — `if (cond) make(<circle>) else make(<square>) end` produces a value with `TypeEstimate::Class(<shape>)` (their common ancestor). Subsequent `area(joined_value)` cannot resolve. |
| 12 | **Sprint 13 regression** — multimethod tests still pass (no sealing → still Dispatch + cache). |
| 13 | **Sprint 14 regression** — MI + `next-method` tests still pass; sealing doesn't conflict with override-method dispatch. |
| 14 | **`<point>` + Sprint 12 regression**. |
| 15 | **GC under sealed-direct dispatch pressure** — 10K calls through sealed-direct sites, no crashes. |

---

## 9. Edge cases and traps

### 9.1 Sealing a class doesn't seal its parent

```dylan
define open class <shape> (<object>) end class;
define sealed class <circle> (<shape>) end class;
```

`<circle>` is sealed; `<shape>` is not. A future library may add `<triangle> (<shape>)`. The closure rule on `area(c :: <circle>)` still holds — because `c`'s type estimate is `Class(<circle>)`, and `<circle>` is sealed. The fact that `<shape>` is open doesn't matter.

### 9.2 `instance?` else-branch narrowing is hard

We deliberately skip it in v1. The else-branch sees "not `<circle>`" which is a *negation* in the type lattice — representing "any type except `<circle>` and its subclasses" requires a richer lattice (intersection types or co-typed-sets). v2 territory. Document.

### 9.3 Method specialisers can mention a sealed class from outside the generic's declaring library

If library A declares `define sealed class <foo>` and library B declares `define generic g (x)`, then B may legally add `define method g (x :: <foo>) …` — `<foo>`'s sealing is about its subclass tree, not about who may specialise on it.

### 9.4 The closure condition is necessary, not sufficient

Closure on the method table doesn't guarantee a unique winner. Two methods specialising on `<circle>` and `<shape>` are both applicable to a `<circle>` receiver; `<circle>` wins via specificity. The optimiser must run the specificity comparison after enumeration; it can't short-circuit on closure alone.

### 9.5 What if the type estimate is `Top`?

`Top` means "we know nothing". No closure possible (any class might be involved). Leave as Dispatch.

### 9.6 What if the type estimate is `Bottom`?

`Bottom` means "unreachable". The Dispatch is dead code. The optimiser can replace it with a placeholder (poison, or a runtime trap). For v1, leave alone — `Bottom` shouldn't appear in well-formed code.

### 9.7 Sealing + `next-method`

A sealed-direct call goes to a specific method body. That method body may use `next-method()`. The `next-method` chain is the SAME as it would have been under runtime dispatch — Sprint 14's thread-local frame is pushed by the dispatcher OR by the resolved direct call's preamble. For Sprint 15, the direct-call rewrite must include a preamble that pushes the chain frame just like `nod_dispatch` would have. Alternative: have the direct-call body itself responsible for chain setup. Pick one and document.

**Recommendation:** the direct-call preamble pushes the frame. The method body remains identical to its Dispatch-called counterpart. This way Sprint 13's `nod_dispatch` and Sprint 15's direct-call sites are interchangeable from the method body's perspective.

### 9.8 `define inline` methods + sealing

Sprint 04 captures `inline` and `not-inline` modifiers. Sprint 18+ uses them; Sprint 15 reads them but does not act. A sealed-direct call to an `inline` method does NOT get inlined yet — that's Sprint 18 optimiser work.

### 9.9 Polymorphic call site that "almost" resolves

A call site where two methods are guaranteed applicable but neither is more specific (true ambiguity within the closure) cannot be resolved to a single DirectCall. The optimiser could choose to emit a *bichotomy* — `if class == A: call M1 else: call M2` — but that's a Sprint 18 PIC optimisation. For Sprint 15, leave it as Dispatch.

### 9.10 Direct-call back-reference index sits empty

Sprint 15 records `(call_site_id, generic_id, recorded_generation)` for every sealed-direct rewrite. The data structure sits in `nod-runtime` ready for Sprint 29 to consult. Sprint 15's redefinition refusal makes the invalidation path unreachable, so the index isn't read by any code path during this sprint. That's fine — the index is a forward-compat hook.

---

## 10. Open questions

Things this spec cannot resolve from prior commitments alone. The Sprint 15 implementer decides and documents.

1. **`nod-opt` as a new crate vs a module in `nod-sema`.** The optimisation pass is small enough to live inside `nod-sema`, but `PLAN.md` carves `nod-opt` out as its own crate. *Recommendation: ship as a `nod-sema::optimise::dispatch` module for Sprint 15; promote to `nod-opt` crate when Sprint 18's CSE/inline/DCE land.*

2. **Type-estimate narrowing as a separate pass vs. interleaved with rewriting.** Two-pass (narrow then rewrite) is cleaner; one-pass (narrow while walking) is faster but harder to test. *Recommendation: two-pass for Sprint 15; collapse if profiling demands.*

3. **`Singleton(Word)` carried but not populated.** v1 lattice has Singleton for `#f`, `#t`, `nil` (all known unique values) but Sprint 15 doesn't populate it because the conditions where it'd matter (`if x == #f then …`) need pattern recognition in the analyser. *Recommendation: define the variant; don't populate; Sprint 17 macros + Sprint 19 conditions revisit.*

4. **Cross-library scope of `define sealed domain`.** The spec says single-library. But a `define sealed domain` declaration in library A affects what library B may do. Sprint 15 ignores library B (it doesn't exist for the compiler yet). When Sprint 29 lights up cross-library, the domain table needs to be shared. *Decision: per-library tables now; merge mechanism in Sprint 29.*

5. **`sealed` on method-level modifiers.** Dylan allows `define sealed method` — a single method is sealed against override. The semantic differs from sealed generic. Sprint 15 parses but ignores. *Recommendation: document; revisit when there's a fixture that exercises it.*

6. **Should sealing-driven direct calls preserve the `safepoint_roots` from the original Dispatch?** Yes, unconditionally — Sprint 11b's GC-precision contract applies to all calls, sealed or not. The rewrite preserves `safepoint_roots`. The Sprint 15 implementer must verify this in a test.

7. **Performance target.** The headline manifesto promise is "5× speedup vs all-Dispatch baseline on Richards" (Sprint 16). Sprint 15 must deliver enough resolution that Richards' tight inner loops compile to direct calls. *No formal benchmark in Sprint 15 itself; the Sprint 16 Richards integration tests this end-to-end.*

---

*Companion to [`08-repl-and-live-bindings.md`](08-repl-and-live-bindings.md) (the Binding cell that hosts redefinition) and to [`13-multimethod-dispatch.md`](13-multimethod-dispatch.md) (not yet written; the runtime inline cache that Sprint 15 makes optional). Sealing is the load-bearing piece of Dylan's performance story; the resolution algorithm specified here is the one the MANIFESTO commits to.*
