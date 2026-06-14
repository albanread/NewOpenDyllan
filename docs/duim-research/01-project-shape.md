# DUIM at a Glance

Read-only research pass on Open Dylan's DUIM (Dylan User Interface Manager).
Source examined: `E:\opendylan\sources\duim\` (a local checkout of
`github.com/dylan-lang/opendylan`, master branch — same content as upstream).

All counts below are from `Grep`/`Glob` over `*.dylan` files under
`sources/duim/`. They are approximate; classes/methods inside macro bodies and
DSLs (`define frame`, `define command-table`) are **not** counted.

---

## 1. Library inventory

DUIM is split into ten core libraries, a "wrapper" library, two backends
(Win32, GTK), an extras library (panes), and the usual examples/tests. Each
library has its own `library.dylan` (defining the `define library` form) and a
matching `.lid` (project descriptor consumed by `dylan-compiler`).

### Core (portable, abstract)

| Library              | Files (.dylan) | Description                                                  | LID                                      |
| -------------------- | -------------- | ------------------------------------------------------------ | ---------------------------------------- |
| `duim-utilities`     | 7              | Dylan extensions DUIM relies on (macros, strings, native stubs). | `utilities/duim-utilities.lid`           |
| `duim-geometry`      | 9              | Points/regions/transforms/boxes. ~3500 lines of pure math.   | `geometry/duim-geometry.lid`             |
| `duim-DCs`           | 11             | Device-context types — colors, pens, brushes, palettes, text styles, images, stipples. | `dcs/duim-dcs.lid`                       |
| `duim-sheets`        | 18             | The "Silica" abstract-window layer — sheets, mediums, ports, displays, event queues, frame managers, mirrors. Heart of the framework. | `sheets/library.dylan` (no top-level LID here, sourced via `core/duim-core.lid` indirection) |
| `duim-graphics`      | 5              | Figure drawing, path drawing, pixmaps.                       | `graphics/duim-graphics.lid`             |
| `duim-extended-geometry` | 9          | Optional: ellipses, polygons, transforms over regions.       | `extended-geometry/duim-extended-geometry.lid` |
| `duim-layouts`       | 7              | Row/column/grid/table layouts; space requirements; constraint solving. | `layouts/duim-layouts.lid` (file synth) |
| `duim-gadgets`       | 19             | Abstract gadget protocol — buttons, menus, scrollers, trees, tables, text. **No concrete widget impl** lives here. | `gadgets/duim-gadgets.lid`               |
| `duim-frames`        | 16             | Top-level frames, dialogs, commands, command-tables, help, the `define frame` macrology. | `frames/duim-frames.lid`                 |
| `duim-recording`     | 9              | Output-recording layer (CLIM-style retained graphics).       | `recording/library.dylan`                |
| `duim-core`          | 2              | Pure umbrella — re-exports the ten libraries above as `duim` / `duim-internals`. | `core/duim-core.lid`                     |

### Wrapper / user-facing

| Library         | Files | Description                                                  |
| --------------- | ----- | ------------------------------------------------------------ |
| `duim`          | 1     | `core/library.dylan` doesn't define this — it's defined in `win32/duim-library.dylan` and `gtk/duim-library.dylan`. **Per-backend wrapper**: `use duim-core, export: all; use win32-duim, export: all;` (or gtk equivalent). |
| `duim-user`     | 1     | Convenience module: `use duim` + `use io`. For sample code.  |

### Optional portable libraries

| Library                 | Files | Description                                                  |
| ----------------------- | ----- | ------------------------------------------------------------ |
| `duim-gadget-panes`     | 14    | **Portable** concrete pane implementations for gadgets the native backend doesn't supply (splitters, spin boxes, graph/tree controls, dialogs). Effectively a fallback widget set written in pure Dylan on top of `duim-sheets`. |
| `duim-formatting`       | 7     | Table/graph/menu formatting helpers.                         |
| `duim-presentations`    | 11    | CLIM-style presentation system. **Mostly skeleton stubs** — most files are <60 lines (`accept.dylan` 58, `present.dylan` 30, `presentation-records.dylan` 13). |

### Backends (platform-specific)

| Library                    | Files | Lines | Notes |
| -------------------------- | ----- | ----- | ------------------------------------------------------------ |
| `win32-duim`               | 28    | ~17 600 | Production backend. **17 615 lines** dominated by `wgadgets.dylan` (2616), `wcontrols.dylan` (2791), `win32-c-definitions.dylan` (2166), `wtop.dylan` (1141), `wmenus.dylan` (1019). Marked `Platforms: x86-win32` (`win32/win32-duim.lid:47`). Links against `user32.lib`, `gdi32.lib`, `comctl32.lib`, `comdlg32.lib`, `ole32.lib`, `htmlhelp.lib`, etc. |
| `gtk-duim`                 | 23    | ~7 500  | GTK2-era backend. Uses Dylan FFI libs `glib`, `gobject`, `pango`, `cairo`, `gdk`, `gtk` (`gtk/library.dylan:20-25`). Much smaller than Win32 because GTK supplies more widgets natively. |
| `win32-duim-gadget-panes`  | (shares panes/ sources) | — | Win32 build of the optional panes library (the panes/ tree has both `win32-library.dylan` and `gtk-library.dylan`). |
| `gtk-duim-gadget-panes`    | (shares panes/ sources) | — | GTK build of the same. |

Notable absence: there is **no X11 backend** in the tree. The DUIM `README`
(`sources/duim/README:73-77`) mentions a PostScript backend and a "Vanilla"
skeleton — neither is present in the current source tree.

### Examples and tests

12 example apps under `examples/` (cookbook, helpmate, life, pente, reversi,
tetris, tic-tac-toe, scribble, win32-scribble, web-browser, graphing,
interface-builder, windows-viewer, resources). Most ship both a portable
`<name>.lid` and a `win32-<name>.lid`.

Tests are split into `tests/core/` (portable, 24 files, ~6300 lines, biggest:
`layouts.dylan` 1529, `test-port.dylan` 994), `tests/gui/` (interactive,
portable, 26 files), `tests/regression/` (8 files), `tests/win32/` (6 files).
A `benchmarks/graphics/` directory has 6 files (graphics drawing/text perf).

---

## 2. Dependency graph

Extracted from `use` clauses in each `library.dylan`. External Dylan stdlib
deps (`dylan`, `common-dylan`, `io`, `system`, `collections`, `c-ffi`,
`commands`) omitted for clarity.

```
                            duim-utilities
                                  |
                          duim-geometry
                                  |
                             duim-DCs
                                  |
                          duim-sheets   <-- abstract windows / mediums / events
                                  |
              +-------------------+--------------------+
              |                   |                    |
       duim-graphics       duim-extended-geometry      |
              |                                        |
         duim-layouts                                  |
              |                                        |
         duim-gadgets   (also uses commands)           |
              |                                        |
         duim-frames    (also uses commands)           |
              |                                        |
         duim-recording                                |
              |                                        |
         duim-core  (umbrella: re-exports all of the above)
              |
       +------+------+
       |             |
   win32-duim    gtk-duim                duim-gadget-panes (also depends on duim-frames)
       |             |                              |
       +------+------+------------------------------+
              |
            duim   (= duim-core + <one backend>, re-exported)
              |
         duim-user  (use duim + io)
              |
         user app  (e.g. `life`, `duim-examples`)
```

**Key observations:**

- `duim-sheets` is the structural choke point. Everything above the line uses
  it; everything below it is "data" (geometry, DCs).
- `duim-gadgets` and `duim-frames` both pull in an external `commands`
  library — this is *not* in `sources/duim/`; check
  `E:\opendylan\sources\app\command-line\` etc. for where it lives. Worth
  knowing because porting requires that dependency too.
- A user app depends on `duim` (the per-backend wrapper), NOT directly on
  `duim-core`. The wrapper picks the backend. Example: `examples/life/library.dylan:9-15`
  has just `use common-dylan; use system; use duim;` — backend selection
  happens via the `Library-Pack: GUI` directive plus which build target you pick
  (`life.lid` vs `win32-life.lid`).

---

## 3. Entry points

A DUIM app needs three artefacts:

**(a)** A library definition with `use duim;`:

```dylan
// E:\opendylan\sources\duim\examples\life\library.dylan:9-15
define library life
  use common-dylan;
  use system;
  use duim;
  export life;
end library life;
```

**(b)** A frame defined with `define frame`:

```dylan
// E:\opendylan\sources\duim\examples\helpmate\helpmate.dylan:26-51
define frame <helpmate> (<simple-frame>)
  pane helpmate-locator (frame)
    make(<text-field>, text: "...");
  ...
  layout (frame)
    vertically ()
      horizontally () make(<label>, ...); frame.helpmate-locator; end;
      ...
    end;
  command-table (frame) *helpmate-command-table*;
end frame <helpmate>;
```

**(c)** A start function that calls `start-frame`:

```dylan
// E:\opendylan\sources\duim\examples\life\life.dylan:18-26
define method life () => ()
  let frame = make(<life-frame>, title: "Life");
  start-frame(frame);
end method life;

begin life() end;
```

The LID file's `Start-Function: life` directive (`examples/life/life.lid:14`)
tells the compiler what to call.

`start-frame` for normal frames is at `frames/frames.dylan:1931`; the
dialog-frame override is at `frames/dialogs.dylan:152`. It owns the event
loop — runs synchronously until the user closes the frame, returns a status
code.

**Smallest possible DUIM app.** A 10-line program is feasible. Library:

```dylan
define library hello-duim
  use common-dylan;
  use duim;
  export hello-duim;
end;
define module hello-duim
  use common-dylan;
  use duim;
end;
```

Source:

```dylan
define frame <hello> (<simple-frame>)
  pane greeting (frame) make(<label>, label: "Hello, DUIM");
  layout (frame) frame.greeting;
end;
begin start-frame(make(<hello>, title: "Hello")) end;
```

LID: 5 lines (`Library:`, `Files:`, `Start-Function:` etc.). So roughly
**~25 lines total** for a window-with-a-label app. The cookbook's
`simple-window.dylan` is 141 lines but does a full menu bar + drawing pane,
not a minimal app.

---

## 4. Total scope

Counts across `sources/duim/**/*.dylan` (Grep `*.dylan` glob):

| Metric                       | Count       | Notes |
| ---------------------------- | ----------- | ----- |
| `.dylan` files               | **366**     | Includes examples, tests, benchmarks. Core-only (excluding examples/tests/benchmarks) is ~220. |
| Total source lines           | **96 793**  | `wc -l`-equivalent across all `.dylan` files. |
| LID files                    | 56          | Includes per-backend variants (e.g. `life.lid` + `win32-life.lid`). Unique projects ≈ 30. |
| `define library` forms       | ~22         | (Roughly matches the LID count minus per-backend duplicates.) |
| `define class` items         | **~624**    | Excludes `define frame` (an additional ~25 across examples) and `define C-struct` (46). |
| `define generic` items       | **~166**    | |
| `define method` items        | **~4 798**  | Includes test code. Core-only estimate ~3 200. |
| `define macro` items         | **~147**    | Concentrated in `sheets/macros.dylan` (14), `utilities/basic-macros.dylan` (17), `frames/frames.dylan` (6, but huge — see §6), `recording/recording-macros.dylan` (8), `frames/command-tables.dylan` (7). |
| `define c-function` items    | **8**       | All in `win32/ffi-bindings.dylan` (4) and `win32/win32-c-definitions.dylan` (4). |
| `define C-struct` items      | **50**      | Win32 FFI surface (RECT, POINT, LOGFONTA, …). Note: DUIM does *not* bind user32/gdi32 itself — it `use`s the separate `win32-core` libraries under `E:\opendylan\sources\win32\`. |

For estimating port effort: the core (portable) layers are roughly
**60–65 KLOC of Dylan** with ~400 classes and ~3 000 methods. The Win32
backend is another **~17.6 KLOC** of platform glue. GTK adds **~7.5 KLOC**.

---

## 5. Project metadata

**License.** MIT-style, dual-attributed:
> Copyright (c) 1995-2004 Functional Objects, Inc.
> Portions copyright (c) 2004-2023 Dylan Hackers. (`E:\opendylan\License.txt:1-3`)

DUIM dates back to 1995 (Harlequin/Functional Objects era). Every header
reads `Copyright: Original Code is Copyright (c) 1995-2004 Functional
Objects, Inc.` — i.e. no individual files have been re-headered. Authors
Scott McKay and Andy Armstrong appear on virtually every core file.

**README.** One file at `sources/duim/README` (88 lines). Self-describes
release version as `0.1` and explicitly disclaims:
> "This release constitutes a work in progress. Not everything in the code is
> in its final form. Where possible, we have flagged 'hot' items with
> comments starting with '//---***'." (`README:6-10`)

> "There is not yet any specification document as such." (`README:13-16`)

These are 1995-2004 statements but were never revised. The `//---***`
markers are still present in current source (e.g. `win32/library.dylan:15`,
`gtk/library.dylan:16`, `gadgets/menu-panes.dylan`).

**No NEWS / CHANGES / HISTORY files** anywhere in `sources/duim/`. The
top-level repo has only `README.md` and no changelog. There's a `Major-version:
2 / Minor-version: 1` in every LID — version 2.1, which has not changed in
the visible source.

**Git history.** I could not run `git log` (Bash denied), so I can't quote
commit dates directly. The auth and copyright headers say 1995-2004 origin
with hackers maintenance through 2023. Per-example `README.rst`/`README`
files are short status notes (e.g. `examples/life/README.rst`).

**Health signals.**
- The README and `//---***` "kludge" comments still in production code
  (e.g. `win32-duim.lid:11-13` admits the manifest-include is a kludge)
  suggest the code has been in maintenance — not active development — mode for
  some time.
- The presentation system is mostly stubs (most files <100 lines, several
  <30) — looks abandoned mid-port from a CLIM original.
- No X11 backend, no macOS/Cocoa backend. The portable claim is real only for
  Win32+GTK.
- License is clean MIT, no GPL contamination. Safe to port.

---

## 6. First impression

**(a) What is DUIM, structurally?** A textbook layered abstract-window
toolkit, ported from CLIM. Ten portable libraries form a strict stack
(`utilities → geometry → DCs → sheets → graphics → layouts → gadgets →
frames → recording`), an umbrella library (`duim-core`) re-exports the lot,
and each backend (`win32-duim`, `gtk-duim`) plugs in by defining
backend-specific subclasses of the abstract sheet/medium/port/gadget
classes from `duim-sheets`. A user's top-level `duim` library is then "core
+ one backend." It is **not** a plugin system — backend choice is made at
build time by picking which LID to compile, and the resulting executable
is statically linked to exactly one backend. The architecture is recognisably
CLIM/Silica: `<sheet>` (abstract windows), `<medium>` (drawing surface),
`<port>` (connection to display server), `<frame-manager>` are the
load-bearing protocols, all defined in `sheets/`.

**(b) Gnarliest piece.** Two contenders, very close:

1. **The `define frame` macro family** at `frames/frames.dylan:1318-1500`.
   `define frame` rewrites into *four* macros (`frame-class-definer`,
   `frame-panes-definer`, `frame-gadget-bars-definer`, `frame-layout-definer`),
   each of which recursively pattern-matches over a list of slot clauses and
   dispatches differently on `pane`, `resource`, `layout`, `menu-bar`,
   `tool-bar`, `status-bar`, `command-table`, `input-focus`, `pages` keywords.
   Each `pane` clause expands to a memoising method that builds the pane on
   first access using `with-frame-manager`. Porting DUIM requires NewOpenDylan's
   macro system to handle: (i) recursion over `?slots:*`, (ii) `##` token
   pasting (e.g. `?name ## "-pane"`), (iii) variable patterns
   (`?frame:variable`), (iv) `?modifiers:*` non-name modifier capture. This is
   the most aggressive macro use in the entire tree.

2. **The `win32-duim` event/handler plumbing.** `wcontrols.dylan` (2791
   lines), `wgadgets.dylan` (2616 lines), `wtop.dylan` (1141 lines), and
   `wmenus.dylan` (1019 lines) are the WndProc dispatch chain — every Win32
   message gets routed through generic-function dispatch into the right Dylan
   class. Dylan classes hold raw `HWND`s; weak references are needed so the
   GC can reclaim closed windows. The Win32 backend uses ~25 Win32 system
   libraries (`win32-c-definitions.dylan:7-200+`) and 50 C-structs. **Note for
   other agents**: this is exactly the kind of thing where my read-only pass
   can't judge what's still working — bit-rot and 64-bit pointer assumptions
   should be checked separately.

A distant third would be `command-tables.dylan` (1391 lines) — the
CLIM-style command/command-table system, with its own DSL for
`define command-table` (see `examples/helpmate/helpmate.dylan:11-22`).

**(c) What I'd port first.** A three-step bootstrap on NewOpenDylan:

1. **`duim-utilities` + `duim-geometry`** alone, as a pure-Dylan port — no
   sheets, no graphics, no backend. ~3 700 lines combined. These libraries
   are basically math + a handful of macros (`basic-macros.dylan` has 17
   macros, none recursive). If NewOpenDylan can compile these *and* run
   `tests/core/geometry.dylan` (270 lines) and `tests/core/regions.dylan`
   (480 lines) green, the foundation is proven.

2. **`duim-DCs`** — colour/pen/brush/font types. Another ~2 000 lines, still
   pure-Dylan, still no I/O. `tests/core/styles.dylan` (223) and
   `tests/core/test-port.dylan` (994) exercise this.

3. **A toy "console DUIM"** — define a `<test-port>` that doesn't open a
   window at all but just records draw calls. `tests/core/test-port.dylan`
   already does exactly this for the test suite. This lets you build
   `duim-sheets`, `duim-graphics`, `duim-layouts`, `duim-gadgets`,
   `duim-frames` without ever needing FFI, GDI, or a real display.
   `tests/core/main-suite.dylan` (26 lines) is the harness.

Only after step 3 succeeds should you tackle a real backend. When you do,
**target GTK first, not Win32** — `gtk-duim` is 7.5 KLOC vs Win32's 17.6
KLOC, GTK has more widgets natively so less of `duim-gadget-panes` needs to
work, and porting to a fresh Win32 stack on a fresh compiler at the same
time is two unknowns multiplying. A Cairo/Pango-based medium would also map
cleanly to D2D/DirectWrite if Win32 becomes the eventual goal.

**Uncertainty to flag for sibling agents.** I have not assessed: macro
features used vs. NewOpenDylan macro spec (`docs/MACROS.md`); how the FFI
shape (`define C-struct`, `define c-function`) maps onto NewOpenDylan's FFI;
whether `commands` (the external library `duim-gadgets` and `duim-frames`
both pull in) is itself portable. Those are explicitly out of scope for this
"project shape" pass.
