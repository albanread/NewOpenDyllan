# DUIM Bit-Rot Survey

**Scope.** Read-only assessment of decay in `opendylan/sources/duim`. Source examined
on disk at `E:\opendylan\sources\duim` (full clone, master branch as of 2026-05-15).
Cross-referenced against `documentation/source/release-notes/*.rst` (2011.1 through
2026.2) and `documentation/source/news/`.

**Method caveat.** Bash, PowerShell, and WebFetch were disabled in this sandbox, so
per-file `git log` dating against GitHub was not possible. The findings below rest
on artifacts I *can* read directly: copyright headers (which Dylan tradition
preserves on every file), release notes, NEWS posts, in-source `//---***` markers,
LID-file platform declarations, test bodies, and the textual archaeology of the
sources themselves. Where I cite a fact I cite the file and line.

---

## 1. Activity timeline

### Copyright headers are uniform and frozen

Every single Dylan source, LID file, and module declaration under `sources/duim`
carries the same header:

```
Copyright:    Original Code is Copyright (c) 1995-2004 Functional Objects, Inc.
```

A grep for `Copyright (c)` against the tree returns ~300 matches and **every one**
ends in `1995-2004`. The `1995-2004` end date is the year Functional Objects open-
sourced the codebase. No file in the tree has had its copyright bumped since the
original donation. This is itself a strong signal: most active codebases roll their
copyright forward at least when license text is touched.

The `duim.faq` (`sources/duim/duim.faq`) is dated `1995-2000`. The release outline
(`sources/duim/outline.text`) is dated **"July 22 1996"**.

### Release-notes evidence (definitive)

Release notes live in `documentation/source/release-notes/` and span 2011.1 through
2026.2. A grep for `DUIM` across all 14 release-note files returns matches in
**exactly one** of them: `2020.1.rst`. That single mention reads:

```
DUIM
====

* The obsolete ``GDK_SOLID`` constant is no longer referenced from the
  DUIM back-end for GTK.
```

That is the entire DUIM changelog for the last 15 years of releases. Releases
2011.1, 2012.1, 2013.1, 2013.2, 2014.1, 2019.1, 2022.1, 2023.1, 2024.1, 2025.1,
2026.1, and 2026.2 have **no DUIM section at all** — no bug fixes, no features,
no platform support.

### Per-library status (inferred from artifact evidence)

I cannot date individual commits without `git log`, but the artifact pattern is
unambiguous:

| Library | Status | Evidence |
| --- | --- | --- |
| `utilities/`, `geometry/`, `dcs/`, `graphics/`, `sheets/`, `layouts/`, `gadgets/`, `frames/`, `core/`, `recording/`, `presentations/`, `formatting/`, `panes/`, `extended-geometry/`, `user/` | **Frozen** (back-end-independent core) | 1995-2004 headers, no release-note mentions |
| `win32/` (33 source files) | **Maintained on demand, declining** | LID file declares `Platforms: x86-win32` only — no x86_64 or arm64. Still references Windows 95/98 quirks in live code paths. |
| `gtk/` (25 source files) | **Half-resurrected then re-frozen** | Touched 2013-2020 to port from GTK 2 to GTK 3 (per 2013-08-15 news post and 2020.1 release note), idle since. |
| `tests/core/`, `tests/gui/`, `tests/win32/`, `tests/regression/` | **Stub-heavy, unrun for years** | 118 "Fill this in…" placeholder tests across 9 files in `tests/core/` alone. |
| `examples/win32-scribble/`, `examples/windows-viewer/`, `examples/web-browser/`, `examples/helpmate/`, `examples/interface-builder/` | **Museum pieces** | Same 1995-2004 headers; `interface-builder/README` still labeled "work in progress" with hot items. |
| `benchmarks/` | **Frozen** | One `win32-duim-graphics-benchmarks.lid`, no GTK counterpart. |

The recent-author signal I can get without `git log` is from the release-notes
contributor lists 2023-2026: **Carl Gay, Peter S. Housel, Bruce Mitchener,
Fernando Raya, Jan Sucan**. None of those have appeared as commit authors *on
DUIM specifically* in release-note summaries since 2020.1's one-line GTK fix.

---

## 2. Comment archaeology

### `//---***` "hot items"

The README explicitly says:

> Where possible, we have flagged "hot" items with comments starting with "//---***".

So this marker is the closest thing DUIM has to a curated TODO list. Current count
in `sources/duim`: **557 occurrences across 110 files**. Top offenders:

- `win32/wgadgets.dylan` — 38 markers
- `win32/wcontrols.dylan` — 28
- `win32/wdraw.dylan` — 22
- `gtk/gtk-gadgets.dylan` — 22
- `gtk/gtk-medium.dylan` — 19
- `win32/wmedium.dylan` — 15
- `win32/wdialogs.dylan` — 14
- `recording/recording-classes.dylan` — 15
- `recording/figure-recording.dylan` — 11
- `tests/core/geometry.dylan` — 33 (test stubs)
- `tests/core/styles.dylan` — 33 (test stubs)

Generic TODO/FIXME/HACK/XXX/`---***` count: **533 occurrences across 107 files.**

### Sample of representative hot markers (`gtk/gtk-gadgets.dylan`)

```
58:  // /*---*** Not used yet!
163: /*---*** Not ready yet!
196: //---*** DO WE NEED THIS?
215: /*---*** Do we need any of this?
713: /* ---*** Implement me
1393://---*** Need to implement add-item etc...
2168://---*** Someday we should do these for real!
2219:/*---*** No status bar for now...
2381:/*---*** This doesn't work, so let's just fix up the label by hand
2400:/*---*** The simple label code doesn't work for some reason
```

Nine separate "doesn't work / not ready / implement me / someday" comments inside a
single GTK gadgets file — the file is `define module gtk-duim` and is in the live
LID, not commented out.

### Old-platform references in live code

- `win32/wclipboard.dylan:114` — `//---*** The error code is not setup in Windows 95/98.`
- `win32/wkeyboard.dylan:371` — `// Input is encoded in the ANSI code page in Windows 95`
- `win32/wkeyboard.dylan:540-553` — entire `get-altgr-state` function has a live
  branch handling Windows 95/98 keyboard-layout quirks (`when (_port.%os-name ==
  #"Windows-NT") … else …`). The "else" branch is still reachable from the build.
- `frames/help.dylan:203` — `// Help topics page (supersedes index and contents in Win95)`

No references to OS/2, VMS, DEC Alpha, MIPS, PowerPC, Carbon, Classic Mac, System 7,
68k, WIN16, near/far pointers, or FAR PASCAL turned up. So DUIM's archaeology is
specifically **Windows 95-era**, not earlier-Mac or 16-bit era. The Mac port from
Apple Dylan never lived in this tree.

### Old company / vendor names

`grep -i 'Harlequin|Apple Dylan|Functional Developer|Functional Objects|Gwydion'` —
~110 files match. Representative:

- "Functional Objects, Inc." in every copyright line (this is the dissolved
  commercial Dylan vendor).
- `tests/gui/README.html` still titled `Functional Developer Example: duim-gui-test-suite`
  and references "the Functional Developer IDE" — Functional Developer was the
  commercial product retired ~2003.
- `examples/interface-builder/README` carries copyright "1995, 1996 Functional
  Objects, Inc."
- The 2014 Call-for-Help news post (`documentation/source/news/2014/01/28/call-for-help.rst`)
  is still in the published documentation and remains accurate: "Despite being built
  to be cross-platform, the only currently fully functional backend for DUIM is the
  Windows version."

### Pre-XP UI screenshots

`documentation/source/building-with-duim/images/` contains ~30 PNG screenshots
(`pushb.png`, `textfld.png`, `lbox.png`, etc.). The screenshots show Windows
classic-theme widgets — flat grey, 3D-bevel buttons, Tahoma/MS Sans Serif chrome.
Pre-Vista, possibly pre-XP. Not strictly broken (the manual is illustrating
gadget concepts, not OS chrome), but visually dated.

---

## 3. Code-style bit rot

### Hungarian-ish abbreviations

Heavy. The entire `win32/` directory follows the convention `w<noun>.dylan`:
`wclipboard, wcolors, wcontrols, wdebug, wdialogs, wdisplay, wdraw, wevents, wfonts,
wframem, wgadgets, whandler, whelp, wkeyboard, wmedium, wmenus, wmirror, wpixmaps,
wport, wresources, wtop, wutils` — 22 files using a one-letter prefix that hasn't
been a common Dylan convention. Plus `c-com.c` (whole file: 9 lines, dated 1996,
unwraps one `IMalloc::Free` vtbl call).

The `dxwduim.dll.manifest` filename hints at a now-vanished convention
(`d` = Dylan? `xw` = ?). The `Comment:` field in `win32/duim.lid` flags it as a
"kludge":

```
Comment:       'C-Header-Files' is a kludge to get the manifest included
```

This is build-system stretching present-day shapes back into a tool that wasn't
designed for them.

### No Dylan-side reference counting

Greps for `add-ref` / `release` patterns outside the COM context returned nothing —
DUIM does *not* show signs of having grown its own retain/release scheme. The
existing manual-management surface is in proper places (`c-com.c` for COM vtbls,
`<gtk-mirror>` mirror lifetimes managed via GC + GTK ref counts). That's healthier
than I'd feared.

### `#if false` / `#if 0` blocks

None — Dylan doesn't have a preprocessor. Instead, dead code is wrapped in `/* …
*/` block comments. `gtk/gtk-help.dylan` is the worst example: of its 199 lines,
the entire file body (lines 11-199) is one big `/*---*** No help for now…` comment
block. The library still loads — `gtk-help` is named in `gtk/gtk-duim.lid` — but
exports essentially no live code.

### Always-true platform conditionals

The LID system gates by `Platforms:` declarations, not by Dylan-side conditionals,
so I didn't see "always-true `#if WIN32`" smells. But `sources/duim/win32/duim.lid`
declares `Platforms: x86-win32` — i.e. only 32-bit Windows is a build target for
DUIM despite Open Dylan 2026.2 advertising Apple Silicon (arm64) support and
shipping a 64-bit compiler. The Windows DUIM backend is x86-only at the package
declaration level. (The gtk variant declares aarch64-linux, arm-linux, x86-linux/
freebsd/netbsd, x86_64-darwin/linux/freebsd/netbsd — but not Windows.)

---

## 4. Build-system rot

### LID-file health

I sampled all 60 LID files. Source-file lists match what's on disk; I found **no
orphaned `Files:` entries** pointing at deleted sources. The `Files:` lists are
short and clean — DUIM was small, well-organized, then frozen.

What's stale is the *metadata*:

- `Library-Pack: GUI` appears in 14 LID files. `Library-Pack` is a Functional
  Developer-era distribution-system attribute; the modern `dylan-tool` /
  `deft`-based workflow doesn't consume it.
- `Base-address: 0x65c00000` in `win32/duim.lid` is a fixed DLL load address — a
  Windows-9x-era PE optimization to avoid relocation. Modern Windows uses ASLR;
  the address is harmless but pointless.
- `Major-version: 2 / Minor-version: 1` throughout — no version has been bumped.

### `duim.faq` is from 1995-2000

The FAQ file in `sources/duim/duim.faq` contains entries like:

> 1. Where is the DUIM documentation?
>
> The documentation group is writing both a user's guide and a reference
> manual for DUIM, which are on the verge of being ready/useful.

…and:

> 2. Why is there so little in here?
>
> Because Scott and I are too lazy to write anymore...

26 years on, still in the tree.

---

## 5. Documentation rot

`documentation/source/building-with-duim/` contains the user-guide tutorial,
which is reasonably current-looking — RST format, builds with Sphinx,
cross-linked. But its instructional content references the legacy environment:
"Use the New Project wizard…", "Tools > Open Example Project…", "Tools > Open
Playground…" — i.e. the old Windows IDE. Users following this guide on macOS or
Linux today have no IDE to open these from.

`documentation/source/hacker-guide/duim/index.rst` is a 67-line stub that opens:

> We have a lot to learn about hacking on DUIM, so there isn't much to document
> yet as we haven't learned yet.

The single news post about DUIM since 2013 is the GTK-revival post
(`2013/08/15/duim-gtk.rst`). The 2014 call-for-help post lists DUIM as needing
help on **all three** of Windows ("quite dated and doesn't fully respect modern
interface standards"), GTK ("needs a good bit of work"), and Cocoa ("not yet
been started"). Nothing on the page is wrong today.

No DUIM library-reference docs at `documentation/library-reference/source/duim*`
exist in the tree.

---

## 6. Test coverage

DUIM ships its own test harness in `sources/duim/tests/`:

- `tests/core/` — 22 dylan files plus `duim-test-suite.lid` and
  `duim-test-suite-app.lid`. Headed by `main-suite.dylan`, which composes
  `duim-test-suite, duim-graphics-suite, duim-geometry-suite, duim-regions-suite,
  duim-transforms-suite, duim-colors-suite, duim-layouts-suite, duim-frames-suite,
  duim-gadgets-suite, duim-menus-suite, duim-dialogs-suite, duim-events-suite,
  duim-gestures-suite, duim-commands-suite`. Uses the modern `testworks` API
  (`check-true`, `check-equal`, `define test`).
- `tests/gui/` — 24 files, both win32 and gtk LIDs.
- `tests/win32/` — 6 files (Win32-specific).
- `tests/regression/` — Win32-only.

**118 "Fill this in…" stubs** across 9 files of `tests/core/`. Distribution:

```
tests/core/styles.dylan:33
tests/core/geometry.dylan:33
tests/core/gadgets.dylan:15
tests/core/dialogs.dylan:11
tests/core/layouts.dylan:9
tests/core/classes.dylan:8
tests/core/commands.dylan:4
tests/core/frames.dylan:3
tests/core/graphics.dylan:2
```

So in `geometry.dylan`'s 37 `define test` definitions, 33 are empty stubs.
Whatever coverage the harness produces, it isn't meaningful — for geometry and
styles in particular, ~90% of the named tests are no-ops. There are no signs of
the suite being run in CI (no `.github/workflows/` references to it, no
`run-all-tests` invocation in release notes).

Practical implication: if a port broke gadget layout or geometry transforms,
the test suite would not catch it.

---

## 7. Issue tracker signal

I cannot fetch from GitHub in this sandbox (WebFetch denied). What I can
confirm from on-disk artifacts:

- The `dylan-lang/opendylan` repo is the live tracker (cited in every release
  note).
- The 2013-08-15 news post links to `https://github.com/dylan-lang/opendylan/labels/lib-DUIM%20%2F%20Gtk`,
  so there is at least one DUIM-specific label.
- The 2014 call-for-help post lists DUIM as in need of contributors. The page is
  still in the published docs as of 2026, suggesting nobody has stepped up.
- Release-note Contributors lists 2023-2026 are 4-5 people each. The Dylan
  community is small; if a DUIM rewrite or removal RFC existed it would surface
  in release notes — none does.

---

## 8. Known-broken paths

- **`gtk/gtk-help.dylan`** — entire file body wrapped in
  `/*---*** No help for now… */`. The library still loads it; it exports nothing
  meaningful. Help-on-keyword, help-on-context, etc. are all no-ops on GTK.
- **PostScript backend, "Vanilla" backend** — listed in the top-level `README` as
  shipping libraries ("PostScript-DUIM", "Vanilla-DUIM"). They are not in the
  tree. Whatever donation happened, these were never reconstituted.
- **`Othello`** — README mentions it as a "Sample DUIM Othello game"; the tree
  has `reversi/` instead. The README is out of sync.
- **`examples/life/README.rst`** — marks `*.ico` and `life-resources.rc` as
  `*(Unused)*`. Resource files are kept in the tree but not loaded.
- **Interface Builder** — `examples/interface-builder/README` calls itself
  "Beginnings of a DUIM Interface Builder, used to test a bunch of DUIM's
  dynamic gadget stuff" — i.e. never finished. The companion `README.html`
  references "Functional Developer IDE" search dialogs.
- **CL utility library, `functional-extensions`** — the top-level README says
  "the dependency on this library will eventually be removed." 26 years on, the
  CL library still exists at `sources/lib/cl/` per the toplevel listing.

No files in `sources/duim/` are orphaned (every Dylan file in a backend dir is
named in its `.lid`), so the source set is at least self-consistent.

---

## 9. The honest answer

**How healthy is DUIM, really?** The back-end-independent core (`utilities`,
`geometry`, `dcs`, `graphics`, `sheets`, `layouts`, `gadgets`, `frames`,
`presentations`, `recording`, `formatting`) is **frozen but coherent**: clean
LID files, no orphans, consistent module structure, the abstraction layers from
the 1996 outline still match the 2026 code. The Windows backend (`win32/`) is
**maintained on demand and declining**: it still works (it's what runs the
legacy Open Dylan IDE), but it's 32-bit-only at the LID level, riddled with
~180 hot markers, and contains live code paths for Windows 95/98 quirks. The
GTK backend (`gtk/`) is **half-resurrected then re-frozen**: it got real work
in 2013-2020 to move to GTK 3, then the contributor energy evaporated; entire
files like `gtk-help.dylan` are commented-out stubs. The Cocoa backend is
**non-existent** (was wanted in 2014, never started). The test suite is
**de facto abandoned** — ~118 named-but-empty stubs in the core suite alone.

**Which subsystems are safe to port and which are landmines?**

*Safe-ish, in roughly descending order of confidence:*
1. `geometry/`, `extended-geometry/`, `dcs/` (colors, brushes, pens, text-styles,
   stipples, palettes) — pure data types and arithmetic, no I/O, well-tested by
   the API surface even if the unit tests are stubs. Port these first; they're
   the foundation.
2. `utilities/` — short, mostly basic-macros and string helpers. A few hot markers.
3. `sheets/` — the abstract window model. Big but conceptually clean; the
   abstractions match the spec.
4. `graphics/`, `layouts/`, `gadgets/`, `frames/` — large, with hot markers
   scattered throughout, but architecturally sound.

*Landmines:*
1. **`gtk/gtk-help.dylan`** and the entire help subsystem in `frames/help.dylan` —
   built around HtmlHelp / `HHCTRL.OCX` (Windows-specific) with the GTK side
   stubbed out. The OLE-embedding hooks in `frames/frames.dylan` (lines 997+
   reference "OLE, which has a simplistic notion of status bars") are residual.
2. **`win32/wkeyboard.dylan`** Win95/98 codepage logic and AltGr detection —
   needs replacing with modern Unicode-via-`WM_UNICHAR` paths anyway.
3. **`recording/`** — 15 hot markers in `recording-classes.dylan`, 11 in
   `figure-recording.dylan`. The output-recording subsystem inherits from
   Symbolics CLIM and is the most CLIM-like (presentations + commands +
   recording). It is unclear how much is actually exercised by anything in the
   suite — the `presentations/presentation-tests.dylan` file exists but is one
   of the empty-stub families.
4. **`examples/interface-builder/`** — was an exploratory bench, not a
   maintained tool. Treat as documentation, not as a porting target.
5. **`presentations/`** — the CLIM presentation-type system. It compiles and
   the API is exported, but I see no evidence it's used by any in-tree example
   or test. High risk of bit-rot the moment something pokes at it.

**The brutal truth.** DUIM as it sits on master is a museum-quality artifact:
the 1996 specification, the 2004 code base, the 2013 attempt at a GTK 3 revival,
the 2020 last GTK constant fix, and then five years of silence. The official
documentation still tells contributors (in the 2014 page) that the Windows backend
is "quite dated", GTK "needs work", and Cocoa "would need to progress in parallel
with the creation of Cocoa bindings". None of those three statements has aged
out. The test suite is empty enough that you cannot use it as a porting safety
net. The release notes confirm: in 14 published releases over 15 years, **DUIM
appears in exactly one** with a one-line `GDK_SOLID` fix.

If the goal is a UI for NewOpenDylan, my advice is to port DUIM's **abstract
model** (sheets, panes, layouts, gadgets, geometry, DCs — the 1996 outline plus
present-day refactoring) and **rewrite the backend from scratch** against a
modern toolkit. The Windows backend's investment is in win32-isms that don't
translate to anything you'd want today; the GTK backend is half-finished and
GTK 3 itself is now in maintenance mode (GTK 4 has been out since 2020). The
abstract layer is the value; the backends are 25-year-old plumbing where the
parts that work and the parts that don't are intermixed at the file level.

Don't try to incrementally modernize the Windows backend in place — too many
small dead-Windows-9x hairs and OLE-embedding hooks. Don't try to finish the
GTK 3 backend either — the unfinished pieces (help, dialogs, gadgets) are the
politically hard parts. Take the API surface, treat the existing backend code as
a *reference implementation* you read to understand intent, and write a new
backend (Direct2D / Quartz / GTK 4 / Win32 over windows-rs / take your pick)
that uses NewOpenDylan's strengths instead of working around 2002's
constraints.
