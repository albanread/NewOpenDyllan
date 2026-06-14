# DUIM research synthesis

Consolidated from four parallel research passes:

- [`01-project-shape.md`](01-project-shape.md) — library inventory, dependency graph, scope
- [`02-win32-backend.md`](02-win32-backend.md) — Win32 backend audit (drawing model, ABI, APIs)
- [`03-bit-rot.md`](03-bit-rot.md) — bit-rot survey (activity, abandonment, code-style age)
- [`04-language-features.md`](04-language-features.md) — Dylan-feature gap analysis vs NewOpenDylan

The four agents did not communicate with each other. They converged independently on the
same recommendation: **do not port DUIM verbatim.** This document explains why, then
proposes three concrete options and recommends one.

---

## TL;DR

DUIM is **architecturally excellent and operationally moribund.** The CLIM/Silica-style
abstract framework — `sheet` / `medium` / `port` / `gadget` / `frame` / `recording` — is
a coherent, layered design that has aged well as an *interface specification*. The two
implementations of that specification (Win32 and GTK backends) have not aged well. The
Win32 backend is 18,674 lines of pure-GDI, pure-ANSI, 32-bit-pointer-truncating code that
hasn't seen a real change since 2020. The GTK backend has accumulated equivalent rot.

NewOpenDylan today cannot compile DUIM unmodified — there are ~13 sprints of compiler /
runtime work first (multi-library project system, virtual + class + weak slots, `##`
token-pasting in macros, multi-mutator GC, threads, weak tables, full `format`). Most of
those sprints will pay off for *any* serious Dylan UI work, not just DUIM, so they're not
wasted. But the question becomes: once those sprints land, **should the Win32 backend we
port be the existing one, or a new one built on D2D + DirectWrite?**

All four agents — independently — said **new backend**.

The recommendation that flows from this: **port the abstract layers** (utilities,
geometry, DCs, sheets, layouts, gadgets, frames — about 80 KLOC of portable Dylan), then
**write a fresh backend** on the D2D/DirectWrite stack we already have (Sprint 35) plus a
real WNDPROC pump (Sprint 41a). This preserves DUIM's protocol design — the design that's
the actual value — while ditching the 21-year-old GDI + ANSI code that nobody is going to
maintain.

---

## Numbers, at a glance

From the four reports:

| | Count | Source |
|---|---:|---|
| Total `.dylan` files | 366 | Agent 1 |
| Total source lines | ~97 KLOC | Agent 1 |
| Total libraries | 22 (Agent 1) / 43 if you include tests + examples + non-Win32 (Agent 4) | — |
| Libraries needed for a Win32 runtime | 17 | Agent 4 |
| `define class` | 624 (Agent 1) / 749 with mixins counted differently (Agent 4) | — |
| `define generic` | 166 | Both |
| `define method` | ~4,700–4,800 | Both |
| Method/generic ratio | 28:1 | Agent 4 |
| `define macro` | ~85 (Agent 4) / ~147 if helpers counted (Agent 1) | — |
| `define c-function` | 8 in DUIM, 178 in its private Win32 binding | Agents 1 + 2 |
| `define C-struct` | 50 in DUIM | Agent 1 |
| Win32 backend lines | 17,615 (Agent 1) / 18,674 if you include the `.c` shim (Agent 2) | — |
| GTK backend lines | ~7,500 | Agent 1 |
| Distinct GDI calls used | ~24 | Agent 2 |
| ANSI vs Unicode Win32 calls | 52 `*A`, 0 `*W` in DUIM's own FFI | Agent 2 |
| Mentions in OpenDylan release notes 2011–2026 | **1** (in 2020.1, one-line `GDK_SOLID` removal) | Agent 3 |
| Copyright year on every Dylan file in DUIM | 1995–2004 | Agent 3 |
| Empty `//---*** Fill this in` test stubs (one file alone) | 33 of 37 (geometry tests) | Agent 3 |

---

## The four convergent findings

### 1. The abstract layer is the value

Agent 1 mapped the dependency graph and noted that the layering is strict CLIM/Silica:
`utilities → geometry → DCs → sheets → graphics → layouts → gadgets → frames → recording`.
Agent 3 said the abstract layers are "frozen but coherent — worth porting." Agent 4 said
the protocol design (sheet/medium/port split, gadget mixin hierarchy, recording/replay)
is "DUIM's actual contribution and the part worth preserving."

The portable layers are about 80 KLOC of pure Dylan — math, abstract protocols,
layout algorithms, the gadget mixin tower. They have no Win32 dependencies. They have
the bulk of DUIM's intellectual content.

### 2. The Win32 backend has at least one hard crash bug on x64

From Agent 2 (the most consequential single finding in any of the reports):

> **`SetWindowLong(handle, $GWL-WNDPROC, pointer-address(SubclassedWndProc))` at
> `wgadgets.dylan:491` (and `wmirror.dylan:474`). This truncates 64-bit function pointers
> to 32 bits on Win64 — instant AV the first time any subclassed gadget receives a
> message. Needs `SetWindowLongPtrW` instead.**

Plus:

> **Pointer-sized types declared as 32-bit:** `<LPARAM> = <C-both-long>`, `<WPARAM> =
> <C-both-unsigned-int>`, `<LRESULT> = <C-both-long>` at
> `sources/win32/win32-common/first.dylan:68-70`. Every `SendMessage(handle, ...,
> pointer-address(buffer))` site silently corrupts on x64.

This isn't "needs cleanup" — this is "every interactive gadget crashes on first use."
It would be the first thing we'd hit and would force us to audit the entire Win32 layer
before any port could even start running.

### 3. Nobody has touched DUIM in 15 years of OpenDylan releases

From Agent 3:

> Release notes live in `documentation/source/release-notes/` and span 2011.1 through
> 2026.2. A grep for `DUIM` across all 14 release-note files returns matches in **exactly
> one** of them: `2020.1.rst`. That single mention reads: "The obsolete `GDK_SOLID`
> constant is no longer referenced from the DUIM back-end for GTK."

The 2014 "Call for Help" page is still in the docs, still accurate: Windows backend
"quite dated," GTK "needs a good bit of work," Cocoa "not yet been started." Twelve
years on.

This is the empirical refutation of "but it still works in OpenDylan today, so it can't
be that bad." It works *as it did in 2004*, on 32-bit x86. Nobody has tried to push it
since then. Subclassing gadgets on x64 likely crashes their own IDE; nobody noticed
because nobody runs the IDE seriously.

### 4. The drawing model is incompatible with what NewOpenDylan has

Agent 2:

> DUIM is **100% GDI**, drawing through `HDC`, `HGDIOBJ`, `HPEN`, `HBRUSH`, `HFONT`,
> `HBITMAP`. There is **no GDI+, no Direct2D, no DirectWrite, no Direct3D**.

NewOpenDylan's stdlib (Sprint 35) is the opposite: Direct2D + DirectWrite + DXGI + D3D11,
no GDI. We could add GDI to NewOpenDylan's stdlib alongside D2D (Agent 2's suggestion if
we wanted to minimize DUIM-port surgery) — but doing that means we'd ship a 2025 Dylan
compiler with a 1995-era drawing model as our flagship UI backend. That's a backward step.

D2D/DirectWrite is hardware-accelerated, DPI-aware by default, subpixel-correct text
rendering, gradient brushes, anti-aliased everything, compatible with all modern Windows
compositing. GDI is none of those things and Microsoft has stopped investing in it.

---

## The Dylan-language gap

From Agent 4, the things NewOpenDylan needs to add or extend before DUIM can compile *at
all*:

| Feature | Status in NewOpenDylan | DUIM usage |
|---|---|---|
| Multi-library project system | Not implemented | 17 libraries needed |
| Module exports across libraries | Partial | `duim-sheets/module.dylan` alone: 567 lines, ~250 exports |
| `virtual slot` | Not supported (Sprint 12 explicitly rejects) | `<basic-sheet>` uses 2 |
| `class slot` | Not supported | `<basic-sheet>` uses 2 |
| Weak tables (`make(<table>, weak: #"key")`) | Not supported | `<basic-sheet>` uses 2 |
| `subclass(<X>)` specialiser | Not supported (we have `class == <X>`) | Used 10+ times |
| `##` macro token-pasting | Not supported | Frame macros depend on it heavily |
| Auxiliary-rule macros | Not supported | `define frame-definer` is mutually-recursive across 4 auxiliary macros |
| Multi-mutator GC | Not implemented (Sprint 11c is single-mutator) | `<event-queue>` blocks on `<notification>` + `<lock>`; threading is structural |
| `<thread>` / `<lock>` / `<notification>` | Not implemented | 21 files use `define thread variable` |
| `<deque>` | Not implemented | Event queue uses it |
| `<single-float>` arithmetic depth | Partial (boxed types exist; coverage uncertain) | 187 references across 17 files in transforms + colors |
| Full `format` (`format-to-string`, `concatenate-as`, custom directives) | Partial | Used pervasively |

Agent 4's estimate: **~13 NewOpenDylan-side sprints, ~6 calendar months at current
cadence.** This is the prerequisite work even before we start porting DUIM source.

Important caveat from Agent 4: nearly all of those sprints (multi-library, weak tables,
threading, multi-mutator GC, full `format`) pay off for *any* significant Dylan project,
not just DUIM. They are pre-requisites for NodIDE just as much as for DUIM. So even if we
don't end up porting DUIM, the 13 sprints are work we'd do anyway.

---

## Three options

### Option A — Port DUIM verbatim (the maximalist option)

1. Do the ~13 NewOpenDylan-side sprints first (multi-lib, weak slots, threading, etc.)
2. Port the abstract layers (utilities, geometry, DCs, sheets, layouts, gadgets, frames)
3. Port the Win32 backend, fixing every 32-bit-pointer bug as we hit it
4. End state: DUIM-as-it-was, working on NewOpenDylan, on x64

**Pros:** Faithful preservation. Anyone who knew DUIM in 2004 can use it.
**Cons:** We end up with a 2025 Dylan compiler shipping a 1995-era UI backend. The
hard-crash bugs from Agent 2 mean every Win32 path needs verification, not just compile.
GDI-everywhere means we underspend our investment in Direct2D / DirectWrite from Sprint 35.
Roughly **6 months of compiler work + 6+ months of port + audit work = ~12 months minimum.**

### Option B — Port the abstract layers, write a fresh backend (the convergent recommendation)

1. Do the ~13 NewOpenDylan-side sprints (same prerequisite as A)
2. Port the abstract layers (utilities, geometry, DCs, sheets, layouts, gadgets, frames)
3. Throw away DUIM's Win32 backend entirely
4. Write a fresh backend on D2D + DirectWrite, implementing the DUIM `<port>` / `<medium>`
   / `<mirror>` / `<gadget>` protocols against the modern Win32 + COM stack we already have
5. End state: DUIM's interfaces, modern implementation

**Pros:** All of DUIM's design value preserved. None of the bit rot. Backend matches
what NewOpenDylan invested in. DPI-aware, hardware-accelerated, antialiased everything.
**Cons:** The new backend is greenfield work — figure 5-8 KLOC over 4-6 sprints.
Roughly **6 months of compiler work + 4 months of abstract-layer port + 2 months of new
backend = ~12 months total**, comparable to Option A but with a much better landing state.

### Option C — Write a fresh DUIM-inspired framework (the minimalist option)

1. Do the ~13 NewOpenDylan-side sprints (same prerequisite)
2. Take DUIM's *protocol design* (sheet/medium/port split, gadget mixin tower, recording
   protocol, command tables) as a *specification document*
3. Implement the protocols from scratch in idiomatic modern Dylan, sized for actual
   NodIDE needs, not for theoretical generality
4. End state: a UI framework that DUIM users would recognize but that has no shared code
   with DUIM

**Pros:** Smallest possible total footprint. No dependence on libraries we'd need to
keep updated. Can simplify the design (DUIM's CLIM heritage includes some
over-abstraction that practical use would prune).
**Cons:** Loses backward compatibility with anyone who has DUIM code today.
Re-implementing 80 KLOC even at half the size is still 40 KLOC of new code (~6 months
of dedicated Dylan-side work). Total roughly **6 + 6 = ~12 months** also.

Note that all three options land at roughly the same calendar cost — about a year —
because the 13 prerequisite sprints dominate. The differentiator is the *landing state*.

---

## Recommendation

**Option B.**

Reasoning:

1. **DUIM's protocol design is the genuine asset.** The sheet/medium/port split, the
   gadget mixin hierarchy, the recording/replay layer — these are well-thought-out
   abstractions that emerged from CLIM after decades of iteration. Throwing them away
   (Option C) means losing 30 years of UI-framework design experience for no real upside.

2. **DUIM's Win32 implementation is not the asset.** It's a 2004 snapshot. Keeping it
   (Option A) means we ship a flagship app on 1995-era GDI with x64 crash bugs we'd have
   to discover and fix one at a time. That's a hostile place to live.

3. **The user's stated goal — "remain compatible with its task and purpose"** — is best
   served by inheriting the API surface and rebuilding the implementation. A DUIM
   application written against the abstract layers will compile and run; only the
   backend-internal details change.

4. **Total calendar cost is similar across the three options.** Option B doesn't cost
   meaningfully more than Option A or C. The differentiator is what we have at the end,
   and Option B's "DUIM interfaces, D2D implementation" is the best of the three.

5. **The convergent recommendation across all four agents.** Four independent passes
   over the codebase, no communication between them, all reached the same conclusion.
   That's a strong signal.

---

## If we go with Option B, what comes next?

This is *not* "start Sprint 42 tomorrow." There's significant preparatory work. Roughly:

### Phase 1 — language and runtime prerequisites (~6 months, ~13 sprints)

These pay off regardless of DUIM and unblock NodIDE too. Order by dependency:

1. **Multi-library project system + LID file support** (probably 2-3 sprints — the biggest
   single piece; the project system is the gating dependency for everything else)
2. **`virtual slot` + `class slot` + `each-subclass` slot allocation** (1 sprint)
3. **Weak tables** (`make(<table>, weak: #"key" | #"value" | #"all")`) (1 sprint)
4. **`subclass(<X>)` specialiser** (1 sprint)
5. **Macro extensions**: `##` token-pasting + auxiliary-rule recursion (2 sprints —
   this is the highest-risk gap per Agent 4)
6. **`<deque>`** (small — 1 sprint)
7. **Full `format` family** (`format-to-string`, custom directives) (1 sprint)
8. **Single-float arithmetic depth** (probably 1 sprint of plumbing — coverage audit
   first to confirm scope)
9. **Threading library** (`<thread>`, `<lock>`, `<notification>`, `<recursive-lock>`) —
   probably the biggest single piece since it requires (10) below first
10. **Multi-mutator GC** — the biggest single sprint; Sprint 11c's thread-local roots
    need to become a proper stop-the-world or concurrent collector. **This is the largest
    single piece of work in the whole plan.** Might split across 2-3 sprints.

After Phase 1, NewOpenDylan can compile multi-library projects with the language features
DUIM needs.

### Phase 2 — port DUIM's portable layers (~3 months, ~6 sprints)

In dependency order:
1. `duim-utilities` (small; mostly Dylan extension macros)
2. `duim-geometry` (pure math; has test coverage — port the tests too)
3. `duim-DCs` (color, pen, brush, font abstractions)
4. `duim-sheets` (the big one; the abstract window system)
5. `duim-graphics` + `duim-layouts` (smaller pieces on top of sheets)
6. `duim-gadgets` + `duim-frames` (the abstract widget + frame layers)

After Phase 2, we have DUIM's abstract layers compiling. Nothing draws yet — there's no
backend.

### Phase 3 — fresh D2D backend (~2-3 months, ~4-6 sprints)

Implement the DUIM port / medium / mirror / gadget-implementation protocols against
NewOpenDylan's existing Sprint 35 COM stack (D2D / DirectWrite / DXGI / D3D11). Borrow
the *shape* of the existing Win32 backend (which messages to handle, which window-class
flags to use, which gadget classes to wrap) but write fresh code on the modern stack.

Each gadget can be a sprint: `<button>`, `<edit-field>`, `<list-box>`, `<scroll-bar>`,
`<menu>`, `<dialog>`. Start with `<label>` (zero input handling, just D2D draw) to prove
the pipeline.

### Phase 4 — port NodIDE on top (open-ended)

NodIDE becomes the first real DUIM consumer. Each missing piece becomes a small fix-up
sprint.

---

## What this means for the next sprint

Two viable next-sprint shapes given the research findings:

**Sprint 41b — start Phase 1 by tackling the multi-library project system.** Biggest
single piece of pre-work; everything else in Phase 1 depends on it. Probably 2-3 sprints
in itself.

**Sprint 41b alt — tactical: WM_SIZE handling on the existing Sprint 41a window.** The
current `ide-shell.exe` doesn't redraw on resize. Fixing this is a small focused sprint
that exercises the existing infrastructure and provides immediate visible progress
without committing to the year-long DUIM/NodIDE arc.

The choice is strategic: commit to the long arc now, or stay tactical for one more
sprint while you decide?

---

## Honest limitations of this research

- **No GitHub-issue temperature.** Agents 2/3/4 had no WebFetch in their sandbox. The
  rate of open issues mentioning DUIM (and whether they're being closed) would add
  weight either way. Worth a quick manual check before committing to Option B.
- **No per-library `git log` dates.** Agent 3 inferred activity from copyright headers
  and release notes. A `git log --since="2 years ago" -- sources/duim/` would
  corroborate or contradict the "everything is frozen" finding. Probably corroborates,
  but worth verifying.
- **The 13-sprint / 6-month estimate is rough.** Agent 4 said "honest" but acknowledged
  uncertainty. Multi-mutator GC alone could be 3 sprints, not 1.
- **No estimate of porting hazards we haven't seen.** All four agents looked at static
  artifacts. The first real port attempt will surface surprises.

These don't change the recommendation; they just calibrate confidence. The recommendation
holds at high confidence on "don't port the Win32 backend verbatim" (Agent 2's `SetWindowLong`
finding alone is enough). It holds at medium-high confidence on "port the abstract layers
on Option B's plan" — pending the deeper review the next sprint will provide.

---

*All four reports live in this directory. Read them for the file:line citations behind
every claim above. The synthesis is opinionated; the underlying reports are factual.*
