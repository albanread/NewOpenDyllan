# NCL GC — Feedback from the NewOpenDylan Port

*Drafted 2026-05-17. Author: NewOpenDylan team. Audience: NewCormanLisp
team. Subject: lessons learned and one latent issue identified while
lifting NCL's semispace GC (`ncl-runtime/src/heap.rs` + `heap_common.rs`
+ `mutator.rs` + `universe.rs`) into NewOpenDylan's runtime in
Sprint 11.*

---

## TL;DR

We lifted your semispace GC into NewOpenDylan (`E:\NewOpenDylan\NewOpenDylan\src\nod-runtime\`) and adapted for Dylan's 1-bit pointer tag and uniform-header object model. Net result: 23 new tests, the port runs `factorial(10)` through a JIT that allocates literal strings into a pinned static area while the moveable heap GCs around it. Solid.

**Two things worth feeding back:**

1. **Test coverage gap on a specific encoding.** We deviated from your `Tag::Forward(7)` forwarding scheme (since Dylan has no 3-bit tag) and our first attempt at a 1-bit-tag-compatible encoding was lossy. The bug slipped past a round-trip unit test that used one happens-to-be-aligned address. **Your `Tag::Forward(7)` encoding has no equivalent failure mode** — but the testing pattern is worth a glance regardless.

2. **One design observation, not a bug:** the page-heap conservative pinner in `heap.rs:236-302` is *correct* (pinned objects rewound past, not promoted). Our agent initially shortcut to "pin then promote into old" — which is unsound under conservative scanning. We caught it on review but it's the kind of thing that could slip into NCL's page-heap port if a contributor were to "simplify" the rewind logic. Worth a comment marking the rewind as load-bearing for correctness, not an optimisation.

Everything else: your design held up beautifully. The semispace mechanics, the Cheney loop, the card-table write barrier, the start-bit bitmap, and the literal-pool-into-static-area discipline all ported cleanly.

---

## Detail — what we lifted and how

### Files lifted near-verbatim (algorithm unchanged)

| Source | Lines | Destination | Diff vs source |
|---|---|---|---|
| `ncl-runtime/src/heap.rs` | 1505 | `nod-runtime/src/heap.rs` | Tag-scheme adaptation only (see §2); algorithms identical |
| `ncl-runtime/src/heap_common.rs` | 282 | `nod-runtime/src/heap_common.rs` | `HeapHeader` dropped (Dylan uses `Wrapper`); `CardTable`/`StartBits` lifted as-is |
| `ncl-runtime/src/mutator.rs` | (partial; ~600 lines used) | (embedded in `nod-runtime/src/heap.rs` + lib.rs) | TLAB + stop-the-world + park protocol lifted; multi-threaded mutator deferred (Sprint 11b) |
| `ncl-runtime/src/universe.rs` | 255 | `nod-runtime/src/lib.rs` | Folded into our `LITERAL_POOL`-keyed `LazyLock` singleton |

### Files we adapted heavily (Dylan-specific shape)

- **`word.rs`** — Dylan uses **1-bit pointer tag** (bit 0 = 0 fixnum, 1 = pointer) instead of NCL's 3-bit tag. This is a manifesto commitment (`PLAN.md` §1.11). All tag-aware operations in your `heap.rs` had to be rewritten around the 1-bit scheme.

- **`Wrapper` header** (Dylan's equivalent of your `HeapHeader`) — 8 bytes with the 32-bit class id in low bits, 16 GC bits at offset 48 (`Mark`/`Tenured`/`Pinned`/`Forwarded`). This is functionally equivalent to your `HeapHeader` but the bit layout differs.

- **`classes.rs`** — added `ScanFn` + `SizeFn` function pointers to `ClassMetadata`. This is the Dylan-specific win: every class describes its own pointer-slot layout via the metadata, so the GC scanner is one uniform `(class.scan)(addr, &mut visitor)` call. No `match heap_type` switch. See §3 below.

### What we explicitly did NOT lift

- `ncl-runtime/src/page_heap/*` — the in-progress next-gen backend. We took the working `gc-semispace` backend per your default-feature selection in `gc.rs:53`.

- `ncl-runtime/src/stack_map.rs` — the data shapes are useful but Sprint 11 ships with conservative scanning. The `gc.statepoint` compiler-side wiring lands in our Sprint 11b alongside precise roots.

- `ncl-runtime/src/gc_function.rs`, `gc_string.rs`, `gc_symbol.rs` — the heap-object kinds. Dylan has different shapes (`ByteString` / `Symbol` / `SimpleObjectVector` from our Sprint 10) — same general layout (header + length + payload) but the symbol shape diverges: Dylan symbols don't carry function/value cells (those live in our `Binding` cells, per the Sprint 08 spec).

---

## 1. The encoding bug we found (and the testing pattern that masked it)

Dylan's 1-bit tag scheme means we can't use your `Tag::Forward(7)` (which depends on the 3-bit tag space). Our first encoding shifted the new address right by 8 into the wrapper's 32-bit class-id slot:

```rust
// BUGGY — first attempt:
pub const fn forward_to(new_addr: usize) -> Self {
    let encoded = (new_addr as u64) >> 8;     // assumes low 8 bits are zero
    Wrapper { raw: (FORWARDED_FLAG << 48) | (encoded & CLASS_MASK) }
}
pub const fn forwarding_addr(self) -> usize {
    ((self.raw & CLASS_MASK) << 8) as usize
}
```

This is **lossy** for heap addresses where bits 3–7 are non-zero. Heap objects in our semispace are 8-byte aligned (low 3 bits zero) but **not** 256-byte aligned, so addresses like `0x12345608` survive the encode/decode round-trip as `0x12345600` — 8 bytes lost.

Demonstration (standalone Rust):
```
0x12345600 -> encoded 0x...123456 -> decoded 0x12345600  OK
0x12345608 -> encoded 0x...123456 -> decoded 0x12345600  LOSSY ❌
0x12345610 -> encoded 0x...123456 -> decoded 0x12345600  LOSSY ❌
0x12345638 -> encoded 0x...123456 -> decoded 0x12345600  LOSSY ❌
```

**The unit test that should have caught this:**

```rust
#[test]
fn forwarding_round_trip() {
    let new_addr: usize = 0x0001_2345_6700;   // ⚠ low 8 bits zero
    let f = Wrapper::forward_to(new_addr);
    assert!(f.is_forwarded());
    assert_eq!(f.forwarding_addr(), new_addr);
}
```

The address chosen happens to satisfy the lossy invariant (`& 0xFF == 0`). The test passed; the encoding was broken.

**14 of our 14 Sprint 11 GC tests also passed with the buggy encoding** because most check *content survival* by reading bytes (which happens to land at the address `forwarding_addr` reports for objects that land at the start of to-space), not arbitrary post-evac addresses. Only after writing a multi-case alignment test did the failure surface:

```rust
#[test]
fn forwarding_round_trip() {
    for &new_addr in &[
        0x0001_2345_6700_usize,
        0x0001_2345_6708,
        0x0001_2345_6710,
        0x0001_2345_6738,
        0x0000_7FF8_DEAD_BEE0,
    ] {
        let f = Wrapper::forward_to(new_addr);
        assert_eq!(f.forwarding_addr(), new_addr, "lossy at {new_addr:#x}");
    }
}
```

**Our fix**: store the address verbatim in bits 0..48 (x64 user-space pointers fit in 48 bits) with the `Forwarded` flag at bit 51. Lossless for every 8-byte-aligned heap pointer.

```rust
const ADDR_MASK_48: u64 = (1 << 48) - 1;

pub const fn forward_to(new_addr: usize) -> Self {
    Wrapper {
        raw: ((GcBit::Forwarded as u64) << GC_SHIFT) | ((new_addr as u64) & ADDR_MASK_48),
    }
}

pub const fn forwarding_addr(self) -> usize {
    (self.raw & ADDR_MASK_48) as usize
}
```

**Why this is feedback-worthy for NCL even though your `Tag::Forward(7)` encoding is fine:**

- Your encoding stores the full 64-bit address in the upper 61 bits (after the 3-bit tag), so the lossy-encoding failure mode can't happen.
- *But* the testing pattern — "one canonical address used in the round-trip test" — could mask future bugs in adjacent encoding work (e.g. if your `page_heap` introduces a new on-page metadata encoding). Encoding tests should sample multiple alignments deliberately, not pick a single nice constant.

Recommendation: when reviewing future encoding/decoding work, scan for tests that use a single hand-picked round-trip address. If they don't sweep across the meaningful alignment space, that's a coverage gap.

---

## 2. The pin-then-promote question (NCL: correct; us: latent unsoundness, documented)

Our agent's first cut of conservative pinning shortcut to "pin → copy into old gen → forward". That's unsound: a stack word that conservatively pinned an object would, post-promotion, dereference into the from-space wrapper (now overwritten by future allocations once young is reset). Result: dangling pointer dereferencable as garbage.

We caught this on review. **NCL handles this correctly** in `heap.rs:383-411`:

```rust
/// After a minor GC, keep pinned objects in place and rewind
/// `top` to one cell past the highest pinned object. Free cells
/// below pinned objects are wasted until those objects are
/// unpinned (next cycle if no conservative ref still points at
/// them).
pub fn rewind_past_pinned(&mut self) -> usize {
    …
}
```

Pinned objects **stay at their from-space address**. Free cells below them become fragmentation, freed only when the next cycle's conservative scan no longer pins them. That's the only correct option under conservative scanning.

**The shortcut we initially had** would have produced this bug:
1. GC marks object at young addr X as Pinned.
2. Collector copies it to old addr Y, writes forwarding wrapper at X.
3. Young is reset; future allocation lands at X, overwriting the forwarding wrapper.
4. Stack word still pointing at X now references arbitrary data.

In NewOpenDylan this is **latent**, not exercised — `pin_stack_range` is currently only invoked from a unit test, never from the GC trigger path. Real allocation flows don't reach the conservative scanner because Sprint 07 codegen doesn't emit safepoint polls and Rust-side allocation in `nod-sema` doesn't hold raw `Word` values on the stack across allocation points. We documented this as a Sprint 11 simplification with a Sprint 11b retirement path (precise roots via `gc.statepoint`). The bug isn't reachable today.

**Feedback for NCL**: `rewind_past_pinned` looks like an *optimisation* — "we could just promote pinned objects to old, why all this rewind logic?" It's not an optimisation; it's load-bearing for soundness. A future contributor unfamiliar with the conservative-scanning soundness story might be tempted to simplify it. Consider adding a doc-comment marker:

```rust
/// After a minor GC, keep pinned objects in place and rewind …
///
/// SOUNDNESS — DO NOT "SIMPLIFY" TO PROMOTE-AND-FORWARD.
/// Conservative scanning identifies *candidate* roots without knowing
/// which stack words are real pointers; the only correct disposition
/// is to leave pinned objects at their original address so any
/// conservative root that happened to point at them still does.
/// Promoting + forwarding works only if every dereference site
/// (including stale stack words from before the GC) goes through a
/// forwarding-aware loader — which conservative scanning explicitly
/// cannot guarantee.
pub fn rewind_past_pinned(&mut self) -> usize { … }
```

---

## 3. One Dylan-specific win we'd suggest CL could adopt (if cons-cell privilege ever bends)

NCL scans heap objects by a per-`HeapType` switch:

```rust
match header.heap_type() {
    HeapType::Cons => /* scan 2 cells */,
    HeapType::Symbol => /* scan name + package + value + function + plist + flags */,
    HeapType::Vector => /* scan length cells */,
    HeapType::String => /* no out-pointers */,
    HeapType::Function => /* scan closure_env + name */,
    …
}
```

That's correct, but every new heap kind touches the scanner. Dylan's manifesto commits to **uniform headered objects** (no cons privilege), which let us push scanning down into class metadata:

```rust
pub type ScanFn = unsafe fn(addr: usize, visit: &mut dyn FnMut(*mut Word));
pub type SizeFn = unsafe fn(addr: usize) -> usize;

pub struct ClassMetadata {
    pub name: &'static str,
    pub id: ClassId,
    pub parent: Option<ClassId>,
    pub scan: ScanFn,         // visits each tagged-Word slot in an instance
    pub size_of: SizeFn,      // returns byte footprint of this instance
}
```

The GC scanner becomes:

```rust
unsafe {
    let wrapper = *(addr as *const Wrapper);
    let class = wrapper.class();
    let metadata = class_metadata_for(class);
    let total = (metadata.size_of)(addr);
    (metadata.scan)(addr, &mut |slot| forward_word(slot));
}
```

No switch. New heap classes just register a `ScanFn` + `SizeFn` and the GC handles them automatically. Same machinery serves the `tracer` (heap inspection) and the `evacuator`.

CL likely *can't* adopt this cleanly because cons cells are headerless — their class is implicit in the pointer tag, not in the object. But it's worth noting as a structural cost of cons-cell privilege: the scanner stays per-type-switched forever.

This is the "OD strengths" point the user flagged when authorising the GC port: Dylan's uniform header model is genuinely easier to scan precisely.

---

## 4. What worked beautifully (don't change)

- **The semispace layout** — young + 2-semispace old, swap on full GC. Solid; no contention with the Dylan port.
- **The Cheney scan** — copy-and-forward in to-space, scan as you go. Standard but well-implemented.
- **The card-table write barrier** — `CARD_SIZE_BYTES = 512`, one byte per card, marked atomically on store, scanned during minor for old→young pointers. We took it whole.
- **The start-bit bitmap** — letting the scanner parse pages linearly without an object table. Especially clean.
- **Literal-pool-in-static-area discipline** — your `static_area.try_alloc_with_header(HeapType::String, …)` pattern for compile-time literals. Our Sprint 10 initially got this wrong (literals went through the moveable heap; first minor GC would have invalidated every JIT-baked literal address); we caught it during the Sprint 11 port and fixed it by routing through static. Your discipline is the correct shape.
- **`docs/GC.md` and `docs/GC_DESIGN.md`** — invaluable as porting reference. The "diagnosis" section in `GC_DESIGN.md` §0 (the macroexpand-all over-pinning story) is the kind of design-narrative engineering doc more projects need.

---

## 5. Two questions back

1. **Is `page_heap` going to ship before NCL needs Phase 4+ workloads?** We're considering whether to track your page-heap port or stay with semispace. Our use case (Dylan REPL + JIT'd kernel) is closer to your "Lisp IDE" framing than to a long-running server, so semispace probably suffices indefinitely. But if your page-heap turns out to be the de-facto shape, we'd like to know.

2. **Multi-threaded mutator** — we deferred it (Sprint 11b / 28). Your `mutator.rs` (2149 lines) carries the full park protocol + per-thread TLAB. Anything we should know before porting that piece — race conditions you discovered during testing, ABA hazards in the TLAB refill, etc.?

---

## 6. Concrete artefacts

Files in the NewOpenDylan port that may be of interest if you want to cross-reference:

- `E:\NewOpenDylan\NewOpenDylan\src\nod-runtime\src\wrapper.rs` — the Dylan-side 8-byte header with the fixed forwarding encoding (`forward_to` / `forwarding_addr`).
- `E:\NewOpenDylan\NewOpenDylan\src\nod-runtime\src\heap.rs` — the lifted semispace, Cheney scan, conservative pin scaffold (`pin_stack_range`).
- `E:\NewOpenDylan\NewOpenDylan\src\nod-runtime\src\classes.rs` — the `ScanFn`/`SizeFn` metadata pattern.
- `E:\NewOpenDylan\NewOpenDylan\tests\nod-tests\tests\gc.rs` — 14 GC integration tests, including the forwarding-round-trip multi-alignment regression test.
- `E:\NewOpenDylan\DEFERRED.md` — Sprint 11 section with the open follow-ups (precise roots, multi-threaded mutator, JIT-side write barrier emission).

Reachable via `cd E:\NewOpenDylan\NewOpenDylan && cargo test --workspace` (182 passing) and `cargo clippy --workspace --all-targets -- -D warnings` (clean).

---

*Sent in the spirit of the portfolio-wide "lift-with-attribution" convention. The NCL GC saved us roughly six weeks of design work; the two findings above are the only things we found worth feeding back. Everything else: please keep doing what you're doing.*
