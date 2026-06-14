# DUIM language-feature gap analysis

*Research pass over `E:\opendylan\sources\duim\` against NewOpenDylan
Sprint 1-41a. Goal: catalogue every Dylan feature DUIM consumes that
NewOpenDylan does not yet implement, so the port's scope is honest.*

Source corpus: `E:\opendylan\sources\duim\` (existing local clone of
`dylan-lang/opendylan`). I did not modify anything under either tree
except writing this file.

---

## 1. Library and module surface

**DUIM defines 43 `define library` forms.** Filtering out tests,
benchmarks, examples and the GTK back-end, the runtime payload for a
Win32 DUIM application is **17 libraries**:

```
duim-utilities             duim-geometry      duim-DCs
duim-sheets                duim-graphics      duim-extended-geometry
duim-layouts               duim-gadgets       duim-frames
duim-recording             duim-formatting    duim-presentations
duim-gadget-panes (panes)  duim-core          duim-user
win32-duim (back-end)      duim (façade)
```

The dependency graph is acyclic and roughly layered, with the
`duim-core` façade pulling in all ten foundation libraries with
`export: all`:

```dylan
// E:\opendylan\sources\duim\core\library.dylan
define library duim-core
  use dylan;
  use duim-utilities,         export: all;
  use duim-geometry,          export: all;
  use duim-DCs,               export: all;
  use duim-sheets,            export: all;
  use duim-graphics,          export: all;
  use duim-extended-geometry, export: all;
  use duim-layouts,           export: all;
  use duim-gadgets,           export: all;
  use duim-frames,            export: all;
  use duim-recording,         export: all;
  export duim;
  export duim-internals;
end library duim-core;
```

A typical leaf library like `duim-frames` uses six other DUIM
libraries plus `dylan` and `commands`. Each library splits the surface
two ways — `duim-frames` exports the user-facing module, and
`duim-frames-internals` re-exports plus exposes the implementation
hooks (`<basic-frame>`, `do-handle-event`, `note-sheet-attached`,
etc.). Both forms appear in every DUIM library.

**Module exports are large.** The five biggest module surfaces:

| Library         | `module.dylan` lines | Notes                            |
|-----------------|---------------------:|----------------------------------|
| duim-sheets     |                  567 | Two modules, ~200 creates+exports each |
| duim-gadgets    |                  472 | Single big module                |
| duim-frames     |                  382 | Big single module                |
| duim-DCs        |                  187 | Colors, brushes, fonts, palettes |
| duim-geometry   |                  182 |                                  |
| duim-layouts    |                  130 |                                  |

The `duim-sheets` module alone declares ~250 cross-library names. The
`create` versus `export` distinction also matters: DUIM uses `create`
in the public module so subordinate internal modules can `export` the
same names (an LCD pattern not unlike Common Lisp's
`shadowing-import-from`).

**NewOpenDylan state.** Spec `05-library-module-graph.md` is written
and a `Graph` type exists in `nod-namespace`, but the Sprint 5
DEFERRED note still reads "`use` / `import:` / `exclude:` / `rename:`
/ `prefix:` / `export:` resolution — Sprint 05 → Sprint 06 [...]
`uses: Vec::new()` populated [as empty]". Sprint 31's `BindingSource`
work pinned the first real consumer (Win32 c-function provenance) but
multi-module / multi-library resolution is still pending. Today the
loader handles `stdlib.dylan` as a single merged module
(Sprint 29's `STDLIB_FILES` list). **Multi-library projects do not
work yet.** A first DUIM port either (a) waits for the
project-system sprint, or (b) concatenates all 17 libraries into one
giant module with name conflicts resolved by hand — viable for a
proof of life but ugly.

---

## 2. Class hierarchy

**DUIM defines 749 classes across 169 files.** The root abstract
protocol classes live in `sheets/classes.dylan`:

```dylan
// E:\opendylan\sources\duim\sheets\classes.dylan
define protocol-class event-handler (<object>) end;
define open abstract class <abstract-sheet> (<event-handler>) end;
define protocol-class sheet (<abstract-sheet>) end;
define open abstract class <abstract-medium> (<object>) end;
define protocol-class medium (<abstract-medium>) end;
define open abstract class <abstract-port> (<object>) end;
define protocol-class port (<abstract-port>) end;
define open abstract class <abstract-display> (<abstract-sheet>) end;
define protocol-class display (<sheet>, <abstract-display>) end;
define protocol-class mirror (<object>) end;
define protocol-class event (<object>) end;
define protocol-class frame (<abstract-frame>) end;
```

`define protocol-class` is a DUIM-defined macro from
`utilities/protocols.dylan` that synthesises a `<name>` class plus a
`name?` generic and two methods. Six other protocols (`mirror`,
`event`, `pointer`, `caret`, `clipboard`, `frame-manager`) use the
same machine.

**Multiple inheritance is pervasive and goes wide.** Sample heavy-MI
classes from `layouts/panes.dylan` and `panes/graph-control-panes.dylan`:

```dylan
// 7 supers
define open abstract class <drawing-pane>
    (<standard-input-mixin>,
     <standard-repainting-mixin>,
     <permanent-medium-mixin>,
     <mirrored-sheet-mixin>,
     <sheet-with-caret-mixin>,
     <pane-display-function-mixin>,
     <multiple-child-wrapping-pane>)
end class <drawing-pane>;

// 6 supers
define sealed class <graph-control-pane>
    (<standard-input-mixin>,
     <standard-repainting-mixin>,
     <permanent-medium-mixin>,
     <homegrown-tree-control-mixin>,
     <graph-control>,
     <single-child-wrapping-pane>)
end class <graph-control-pane>;

// 5 supers; itself supers transitively to 5-7 more
define sealed class <table-control-pane>
    (<standard-input-mixin>,
     <standard-repainting-mixin>,
     <permanent-medium-mixin>,
     <homegrown-control-mixin>,
     <table-control>,
     <single-child-wrapping-pane>)
end class <table-control-pane>;
```

Each `*-mixin` is itself an `<abstract-sheet>`-rooted class, so the
linearised CPL for `<drawing-pane>` is roughly 15-20 entries deep.
The longest chain I traced (`<graph-control-pane>` →
`<single-child-wrapping-pane>` → `<basic-sheet>` → `<sheet>` →
`<abstract-sheet>` → `<event-handler>` → `<object>`) is **7 deep on
the primary spine**, with mixin branches adding lateral CPL entries.

Slot counts are generally modest per class (3-10), but the
**`<basic-sheet>` root class shows the worst-case slot pattern**:

```dylan
// E:\opendylan\sources\duim\sheets\sheets.dylan:259
define open abstract primary class <basic-sheet> (<sheet>)
  sealed slot sheet-parent :: false-or(<sheet>) = #f, setter: %parent-setter;
  sealed slot sheet-region :: <region> = $nowhere, init-keyword: region:,
    setter: %region-setter;
  sealed slot sheet-transform :: <transform> = $identity-transform,
    init-keyword: transform:, setter: %transform-setter;
  sealed slot sheet-cached-device-region :: false-or(<region>) = #f;
  sealed slot sheet-cached-device-transform :: false-or(<transform>) = #f;
  sealed slot port :: false-or(<port>) = #f, init-keyword: port:, setter: %port-setter;
  sealed slot sheet-flags :: <integer> = $initial-sheet-flags;
  sealed slot %style-descriptor :: false-or(<style-descriptor>) = #f,
    init-keyword: style-descriptor:;
  virtual slot sheet-help-context, init-keyword: help-context:;
  class slot %help-contexts :: <object-table> = make(<table>, weak: #"key"), setter: #f;
  virtual slot sheet-help-source, init-keyword: help-source:;
  class slot %help-sources :: <object-table> = make(<table>, weak: #"key"), setter: #f;
end class <basic-sheet>;
```

That's 8 instance slots plus **2 virtual slots** (no storage; getter
is a method) plus **2 class slots** holding **weak tables**. None of
these slot kinds beyond plain instance allocation works in
NewOpenDylan today — Sprint 12's `SlotAllocation` rejects `virtual`,
`class`, and `each-subclass` with
`LoweringError::UnsupportedSlotAllocation` (DEFERRED.md Sprint 12).

**`define open class` count: 185** files. Open classes can be
subclassed across library boundaries; sealing-resolver work in
Sprint 15 added the cross-library subclass diagnostic, so the
semantics are at least understood, but the dispatch resolver can
never close an open-class call to a `DirectCall`.

`define sealed domain make (singleton(<X>))` and `define sealed domain
initialize (<X>)` appear on virtually every concrete class — the
Dylan idiom for declaring "I will never extend `make` or `initialize`
for this class from outside this library." NewOpenDylan parses
`define sealed domain` via the catch-all `Item::DefineOther` path
(Sprint 04 DEFERRED) without modelling the sealing constraint. That's
acceptable for correctness; it costs some dispatch-resolver wins.

---

## 3. Generic and method volume

| Form              | Count |
|-------------------|------:|
| `define generic`  | **166** |
| `define method`   | **4681** |
| `define class`    | **749** |

Ratio: about **28 methods per generic on average**, but the
distribution is extremely skewed. The biggest-fan-in generics are the
sheet/event protocol ones:

- `handle-event` / `do-handle-event` — at least 175 method definitions
  across the corpus (rough lower bound from the regex; the true count
  is higher because event handlers also appear as anonymous local
  methods).
- `initialize` — appears on essentially every concrete class
  (~400-500 methods, hard to count cleanly because of `next-method`
  forwarding chains).
- `note-sheet-mapped`, `note-sheet-attached`, `note-region-changed`,
  `note-transform-changed` — 30-80 methods each (every sheet mixin
  hooks them).
- `make` (`define method make (class == <X>, …)`) — hundreds of
  override methods using the `class == <singleton>` specialiser
  pattern.

The Sprint 13 inline cache is **monomorphic** ("Cache slot holds ONE
receiver class" — DEFERRED). For DUIM's `handle-event` and `make` —
which see 5+ receiver classes at typical call sites — the cache will
miss constantly and fall back to the linear `nod_dispatch`
walk. That's still correct, just slow. The polymorphic-inline-cache
upgrade flagged for Sprint 18+ becomes load-bearing for any DUIM
benchmark.

There is also extensive use of `subclass(<X>)` as a method
specialiser (the metaclass-dispatch idiom: a method that fires on the
*class* `<X>` itself, not an instance of `<X>`):

```dylan
// gadgets/text-gadgets.dylan:315 and many more
define method gadget-text-parser
    (type :: subclass(<string>), text :: <string>) => (value :: <string>)
  text
end method;

// panes/graph-control-panes.dylan:653
define sealed method layout-class-for-graph
    (graph :: <tree-graph-pane>) => (class :: subclass(<graph-control-layout>))
  <tree-graph-layout>
end method;
```

NewOpenDylan does not yet model `subclass(...)` as a specialiser
shape. The `class == <X>` pattern (which appears even more often —
~200 occurrences via `define sealed inline method make (class == <X>,
#rest, #key, #all-keys)`) **is** what Sprint 12 calls a singleton
specialiser, but the precise lowering needs verification against the
sealed-domain pattern Sprint 15 introduced.

---

## 4. Macros that DUIM defines

DUIM defines **~85 of its own macros**. Categorising by complexity:

**Tier A — trivial templates (~25 macros).** Things like `swap!`,
`inc!`, `dec!`, `push!`, `pop!`, `with-temporary-gdi-object`,
`with-clipboard-lock`, the per-medium `with-pen` / `with-brush` /
`with-text-style` body-shaped forms in `sheets/macros.dylan`. Each is
a single-rule pattern that wraps the body in a `method` and calls a
`do-` helper. NewOpenDylan's Sprint 25-27 body-shaped macro
infrastructure should swallow these without ceremony — the Sprint 25
`unless` retirement validated the same shape.

**Tier B — token-pasting `##` macros (~10 macros).** The protocol
machinery in `utilities/protocols.dylan` is representative:

```dylan
define macro protocol-class-definer
  { define protocol-class ?:name (?supers:*) ?slots:* end }
    => { define open abstract class "<" ## ?name ## ">" (?supers)
           ?slots
         end class;
         define protocol-predicate ?name; }
 slots: { } => { }
        { ?slot:*; ... } => { ?slot; ... }
 slot:  { virtual slot ?:variable, #rest ?options:expression }
          => { virtual slot ?variable, ?options }
end macro protocol-class-definer;

define macro protocol-predicate-definer
  { define protocol-predicate ?:name }
    => { define open generic ?name ## "?" (x) => (true? :: <boolean>);
         define method ?name ## "?" (x :: "<" ## ?name ## ">") => (true? :: <boolean>) #t end;
         define method ?name ## "?" (x :: <object>) => (true? :: <boolean>) #f end; }
end macro protocol-predicate-definer;
```

Note `"<" ## ?name ## ">"` synthesising a class name from a bare
identifier, and the macro emitting *three* top-level forms from one
input. NewOpenDylan's Sprint 25-27 macro system has hygienic
substitution but it isn't documented whether token-pasting with
literal-string fragments is wired. **This is the single highest-risk
language gap for DUIM.** Roughly 60-80 DUIM classes are minted
through `define protocol-class`, so without `##` support the port
either patches the macro to use ad-hoc Dylan names or rewrites every
`define protocol-class` call to an explicit `define class` + `define
generic` + `define method` triple.

**Tier C — auxiliary-rule body-shaped macros (~15 macros).**
`define command-table-definer`, `define frame-definer` (which itself
expands to four sub-macros), `define frame-panes-definer`,
`define frame-gadget-bars-definer`, `define frame-layout-definer`,
the `define pane-definer` family in `layouts/panes.dylan`, the
formatting-table family in `formatting/formatting-macros.dylan`. These
use recursive expansion: the macro matches one item in the body, emits
code for it, and re-invokes itself on the rest. `frame-gadget-bars-definer`
in `frames/frames.dylan` is 100+ lines, eight pattern arms, each
emitting a `define method` against the frame class.

A representative head:

```dylan
// E:\opendylan\sources\duim\frames\frames.dylan:1318
define macro frame-definer
  { define ?modifiers:* frame ?:name (?superclasses:*) ?slots:* end }
    => { define ?modifiers frame-class ?name (?superclasses) ?slots end;
         define frame-panes ?name (?superclasses) ?slots end;
         define frame-gadget-bars ?name (?superclasses) ?slots end;
         define frame-layout ?name (?superclasses) ?slots end; }
end macro frame-definer;

define macro frame-panes-definer
  { define frame-panes ?class:name (?superclasses:*) end }
    => { }
  { define frame-panes ?class:name (?superclasses:*)
      pane ?:name (?frame:variable) ?:body; ?more-slots:*
    end }
    => { define method ?name (?frame :: ?class) => (pane :: <sheet>)
           let _framem = frame-manager(?frame);
           ?frame.?name ## "-pane"
           | (?frame.?name ## "-pane"
                := with-frame-manager (_framem)
                     ?body
                   end)
         end method ?name;
         define frame-panes ?class (?superclasses) ?more-slots end; }
  // … five more arms …
end macro frame-panes-definer;
```

Every DUIM application uses `define frame`. The macro is **load-
bearing** — and it combines token-pasting (`?name ## "-pane"`),
auxiliary rules (`slot:`, `slots:`), recursive self-invocation,
pattern variables typed as `?frame:variable`, and embedded
body-shaped macros (`with-frame-manager`). This is the macro stress
test for whatever Sprint 18 builds.

**Tier D — definers that synthesise classes and methods together
(~10 macros).** `command-table-menu-definer`, `pane-definer`,
`gadget-definer` (in interface-builder), `output-record-constructor-
definer`, `stencil-definer`, `pattern-definer`. Each one matches a
DSL-like header and emits multiple top-level forms.

**Sprint 18's "twelve most-common macro shapes" brief is going to be
load-bearing for DUIM. If `##` token-pasting and auxiliary-rule
recursion don't both work, DUIM stops at the parsing stage.**

---

## 5. Multi-threading

DUIM is **inherently multi-threaded.** Evidence:

```dylan
// E:\opendylan\sources\duim\sheets\event-queue.dylan:13
define sealed class <event-queue> (<object>)
  sealed constant slot %deque :: <object-deque> = make(<object-deque>);
  sealed constant slot %non-empty :: <notification>
    = make(<notification>, lock: make(<lock>));
end class <event-queue>;

define sealed method event-queue-pop (queue :: <event-queue>) => (event :: <event>)
  with-lock (associated-lock(queue.%non-empty))
    while (empty?(queue.%deque))
      wait-for(queue.%non-empty);  // BLOCKS
    end;
    pop(queue.%deque)
  end
end method event-queue-pop;
```

The event queue uses `<lock>`, `<notification>`, `wait-for`,
`release-all`, `with-lock` — every event delivery into a DUIM frame
blocks the UI thread on a condition variable until the OS event
loop pushes the next event. `duim-utilities` re-exports the whole
`threads` library:

```dylan
// E:\opendylan\sources\duim\utilities\module.dylan:24
use threads, export: all;
```

Counts of thread/sync primitive references: ~20 in the core (mostly
`sheets/event-queue.dylan` and `sheets/ports.dylan`); examples like
`life` use `<thread>` directly to run the simulation off the UI
thread.

In addition, `define thread variable` is used:

```dylan
// E:\opendylan\sources\duim\panes\list-control-mixins.dylan:29
define thread variable *layout-delayed?* = #f;

define macro delaying-layout
  { delaying-layout (?pane:expression) ?:body end }
    => { begin
           let _pane = ?pane;
           block ()
             dynamic-bind (*layout-delayed?* = _pane)
               ?body
             end
           cleanup
             layout-homegrown-control(_pane)
           end
         end }
end macro delaying-layout;
```

`define thread variable` + `dynamic-bind` is the Dylan idiom for
fluid/thread-local binding. The `recording` module exports
**`-dynamic-binder`** convenience names for several of its slots
(`sheet-drawing?-dynamic-binder`, `sheet-output-record-dynamic-binder`,
`sheet-recording?-dynamic-binder`). Dynamic-bind references appear in
21 files; most are in `recording/`, `sheets/`, `frames/`, and
`gadgets/`.

**NewOpenDylan state.** Sprint 11c made the root registry
thread-local but explicitly deferred multi-mutator GC to Sprint 28
("Multi-threaded mutator + per-thread root registries enumerable by
the collector — Sprint 11c → Sprint 28"). Sprint 32 callbacks
*registered* per-thread roots but the brief notes "Cross-thread
callback semantics … will need to lock the closure's environment
frames" as deferred. There is no Dylan-side `<thread>` /
`make-thread` / `<lock>` / `<notification>` machinery at all.
**DUIM cannot run as a faithful port without multi-mutator GC and
the threads-library port.** Either a sprint-block lights those up
(Sprint 28's nominal slot — but it's a multi-sprint effort, not one
2-week slice), or the port rewrites `<event-queue>` and the
`recording` dynamic-binders for single-threaded operation. The
latter is doable for a first-pass IDE — single-threaded GUI loops are
the norm on Windows.

---

## 6. Streams and I/O

DUIM uses Dylan stream APIs lightly in core, heavily in tests and
examples. `format-to-string` shows up 60+ times for building user-
visible labels and error messages. `format-out` is rare (~3 files).
`<file-stream>` / `with-open-file` is mostly in examples (saving game
state in `reversi`, reading help files). The core libraries don't do
any direct file I/O; the Win32 back-end (`win32-duim`) uses Win32
APIs (CreateFile, ReadFile via the `win32-core` library's
`define c-function` declarations) for things like the registry and
clipboard, not the Dylan `streams` module.

So: **DUIM core can be ported without a working Dylan streams
library**, but `format-to-string` is a must-have. NewOpenDylan's
Sprint 10 `format-out` shim handles `%d`, `%s`, `%%` only — that's
strictly weaker than `format-to-string`. Sprint 24 (per DEFERRED) was
slated as the full `format` directive set and the `streams` library
port. Until then, every DUIM call site that uses `%c`, `%=`, `%S`,
field-width, etc. fails.

---

## 7. Collections beyond `<table>`

DUIM uses these collection types beyond what NewOpenDylan ships:

- **`<deque>` / `<object-deque>`** (event queues, ports) —
  `sheets/event-queue.dylan`, `sheets/ports.dylan`.
- **`<stretchy-vector>`** (universal — sheet child lists, mirror
  caches, command-table menu items) — most heavily-used non-`<list>`
  collection.
- **`<object-table>`** with **weak references** (`weak: #"key"`) for
  the help-context and help-source class slots on `<basic-sheet>`.
- **`<string-or-object-table>`** (re-exported from
  `duim-utilities` — likely a `common-extensions` type that hashes by
  string-equality OR identity depending on the key).
- **Custom `<bit-vector>`-shaped flags** packed into a single integer
  slot (the `gadget-flags` and `sheet-flags` patterns) — these are
  written in plain `<integer>` + `logior`/`logand` so they don't add a
  collection requirement.

I found no DUIM uses of `<set>` or `<priority-queue>` in core. There
are user-defined collections (e.g. `<sequence-record>` for output
recording inherits from `<composite-output-record>` but isn't a
`<sequence>` in the Dylan sense).

**NewOpenDylan state.** `<stretchy-vector>` and `<table>` exist
(Sprints 17, 22). `<deque>` / `<object-deque>` do not. Weak
references on tables are not modelled (Sprint 22 brief doesn't
mention weakness). **Deque + weak tables are required for DUIM
core's event-queue plus class-slot pattern.** Both could be added
in a single sprint if the GC's weak-reference design is borrowed
from NewGC's existing machinery.

---

## 8. Foreign code beyond Win32

The `win32-duim` LID file declares its non-Win32 footprint:

```
// E:\opendylan\sources\duim\win32\win32-duim.lid:32
C-Source-Files: c-com.c
C-libraries: ole32.lib uuid.lib shell32.lib comdlg32.lib comctl32.lib
             user32.lib gdi32.lib advapi32.lib htmlhelp.lib
```

One C source file (`c-com.c` — almost certainly the COM IMalloc
free thunk plus a few minor shims) and nine Windows system libraries.
**No third-party C dependencies.** No libpng, libjpeg, FreeType,
libxml, openssl. Image decoding routes through Win32 GDI/GDI+;
font selection through `CreateFontIndirect`; help through
`htmlhelp.lib`. The whole back-end is COM + Win32 USER32 + GDI32 +
common controls.

For NewOpenDylan this is a near-best case: every DLL on that list is
already discoverable via Sprint 31's bare-name materialisation, and
Sprint 35's COM bring-up (per SPRINTS Sprint 35) handles the
`ole32`/`uuid` interface stubs. No extra C toolchain dance.

---

## 9. Floats and bignums

**Floats are pervasive.** The transform algebra in
`extended-geometry/transforms.dylan` is `<single-float>` throughout:

```dylan
// E:\opendylan\sources\duim\extended-geometry\transforms.dylan:13
define sealed class <general-transform> (<transform>)
  sealed constant slot %mxx :: <single-float>, required-init-keyword: mxx:;
  sealed constant slot %mxy :: <single-float>, required-init-keyword: mxy:;
  sealed constant slot %myx :: <single-float>, required-init-keyword: myx:;
  sealed constant slot %myy :: <single-float>, required-init-keyword: myy:;
  sealed constant slot %tx  :: <single-float>, required-init-keyword: tx:;
  sealed constant slot %ty  :: <single-float>, required-init-keyword: ty:;
  ...
end class <general-transform>;
```

`<single-float>` references: 187 across 17 files, concentrated in
`extended-geometry/`, `geometry/`, `dcs/colors.dylan` (the colour
component math), and the GTK back-end (which uses GDK doubles).
Core sheet/sheet-geometry math uses `<integer>` for pixel
coordinates (sheets are integer-coordinate by spec), so the float
exposure is concentrated in the optional `extended-geometry` library
and the colour model. **A subset DUIM port can probably defer
extended-geometry until float boxing lands.**

**`<big-integer>` and `<double-integer>` are imported by the Win32
back-end:**

```dylan
// E:\opendylan\sources\duim\win32\module.dylan:18
use dylan-extensions,
 import: { <abstract-integer>, <big-integer>,
           <double-integer>, %double-integer-low, %double-integer-high,
           \last-handler-definer };
```

These appear when packing `WPARAM` and `LPARAM` (which are 64-bit on
Win64). The reference count is low (5-10 use sites), but on Win64 the
`WPARAM` 64-bit values that exceed fixnum range *must* round-trip
through `<double-integer>` or `<big-integer>`. NewOpenDylan's 63-bit
fixnum + no-overflow-promotion plus the Sprint 28 `<c-pointer>` Word
shape covers most cases (pointers tag fits), but any `WPARAM` value
in the top bit of `usize` will trap. **For an IDE that mostly passes
HWNDs and small integers, this is a non-issue 99% of the time;
the back-end's COM HRESULT plumbing already uses `<C-raw-signed-
long>` and `<machine-word>` directly.**

---

## 10. Tricky language features

**Restarts.** Defined and exported by `duim-utilities` but used
narrowly — `with-restart`, `with-simple-restart`,
`simple-restart-loop`, `with-abort-restart`, `with-abort-restart-loop`,
`restart-query`. Only 5 references across the corpus to actual
`<restart>` / `restart-query` plus the macro definitions. DUIM
exposes the surface but doesn't lean on it; user code might (the
dialog cancellation flow uses `with-abort-restart`).

**Dynamic binding.** Heavy. 60+ references in 21 files. Combined
with `define thread variable`. This is the Dylan analogue of
Common Lisp's `*special*` + `let`. **Required for DUIM's recording
library** (the output-recording engine relies on per-call
output-record context binding to be thread-local).

**Virtual slots.** 6 occurrences across `protocols.dylan`,
`sheets.dylan`, `gadget-mixins.dylan`, `gtk-gadgets.dylan`. Used to
expose getter/setter pairs that *don't* allocate storage —
`sheet-help-context` is a virtual slot backed by a class-slot
`<object-table>`. **NewOpenDylan rejects virtual-slot allocation
today** (Sprint 12 DEFERRED).

**Class slots.** Same 4 files. The pattern is always paired with a
virtual slot in front for the getter. Same Sprint 12 deferral.

**`each-subclass` slots.** Not used in core. (Three commented-out
lines in `gtk-gadgets.dylan`.) Safe to defer.

**`define open class`.** 185 files. Open classes can be subclassed
cross-library; the dispatch resolver can never close their
methods. NewOpenDylan handles this correctly per Sprint 15.

**`subclass(<X>)` specialisers.** 10+ occurrences (text-gadget
type coercion, layout-class-for-graph, sealed-domain make
declarations). NewOpenDylan models `class == <X>` (singleton
specialiser) but the **`subclass(<X>)`** form — meaning "any class
that is a subclass of `<X>`, not an instance of `<X>`" — is a
distinct specialiser shape on the method *class* parameter, not on
an instance. This is a separate dispatch path. Required for at
least the `make` overrides in `gadgets/text-gadgets.dylan`.

**`compute-applicable-methods`** — not used in DUIM core. Safe to
defer indefinitely.

**`define protocol-class`** with `virtual slot` in the body —
DUIM's protocol-class macro itself permits `virtual slot` in the
protocol class body (see the `slot:` aux-rule in `protocols.dylan`).
That's the only `virtual slot` use that flows out of `protocol-class`
expansion; whether downstream classes use it is the question
answered above (yes, in four places).

**`define sealed domain`** (~200 occurrences). Catch-all parsing
in NewOpenDylan today; semantics deferred. Not blocking — sealing-
resolver decisions just won't get the constraint.

**`define protocol`** (separate from `define protocol-class`).
Defines a bundle of generics without a backing class. Used to declare
abstract protocols like `<<sheet-protocol>>`, `<<changing-value-
gadget-protocol>>`. Expands to a sequence of `define open generic`
forms via the protocols.dylan macro. Needs no special support
beyond hygienic macro expansion of `?slots-and-generics:*`.

**`define inline-only`** modifier on functions and methods. Used
liberally in the Win32 back-end's C-function declarations (`define
inline-only C-function Arc ...`). NewOpenDylan parses modifiers but
the inline-only specifier has no enforcement.

**`weak: #"key"`** keyword on `<table>` construction (twice). Plus
the implicit weakness in `<table>`-class-slot patterns. Sprint 22's
`<table>` doesn't model weakness.

**`define last-handler`** — imported by Win32 back-end from
`dylan-extensions`. A specialised condition handler form. Three or
four call sites. Not blocking but needs the macro path.

**MOP introspection** — `class-of`, `subclass(...)` as a *value*
(not a specialiser), `instance?` of user classes are all used.
NewOpenDylan ships `instance?` for user classes per Sprint 12; the
rest are mostly fine.

---

## 11. Honest summary

**Biggest Dylan-language gap.** It's not any one feature — it's the
**combined weight of multi-library projects, multi-threading, and
the macro stress test**. In order:

1. **Multi-library compilation** is the gating problem. DUIM is 17
   libraries with deep `use` graphs and big public-module surfaces;
   the single-file `nod build` driver can't even *start* on it
   until the project system lands. This is what Sprint 5 sketched
   and what Sprints 39-41a (AOT) deliberately deferred.
2. **Thread library + multi-mutator GC** — the event queue genuinely
   blocks on a condition variable, and `define thread variable` plus
   `dynamic-bind` is used in 21 files including the recording engine.
   The Sprint 28 "threads library" line in the plan is a *cluster of
   sprints* in reality (NewGC's multi-mutator story alone is
   non-trivial), not one slot.
3. **Macro features**: token-pasting (`##`), auxiliary rules with
   recursion, and pattern-variable types like `?:variable` and
   `?:expression`. The `define frame` and `define protocol-class`
   macros use all of these together, and they are not avoidable —
   every DUIM application uses `define frame`.

Second-tier gaps that each take a focused sprint: virtual slots,
class slots, `<deque>`, weak tables, `subclass(<X>)` specialiser,
`<single-float>` boxing on the JIT return path, `format` directive
expansion.

**Estimate.** Counting only NewOpenDylan-side work (treating the
DUIM port itself as out of scope), the minimum sprint list to make
a *first DUIM proof-of-life* compile and run is:

| Sprint | Cost | Why |
|--------|------|-----|
| Project system (multi-library, real `use` resolution) | 2 | Spec exists, machinery doesn't |
| Macro: `##` token-paste + auxiliary-rule recursion + `?:variable` | 2 | The DUIM macro stress test |
| Virtual + class slots | 1 | `<basic-sheet>` needs both |
| `<deque>` + weak tables | 1 | Event queue + help-context slots |
| `subclass(<X>)` specialiser + verify `class == <X>` | 1 | `make`-override pattern |
| Float boxing on JIT return path + `<single-float>` arithmetic | 1 | `<general-transform>` |
| Multi-mutator GC + per-thread roots enumeration | 2 | DUIM's UI thread model |
| Threads library port (`<lock>`, `<notification>`, `wait-for`, `release-all`, `<thread>`, `make-thread`, `define thread variable`, `dynamic-bind`) | 2 | Event queue + recording library |
| `format-to-string` full directive set | 1 | Pervasive in user-visible strings |
| **Total** | **13 sprints** | **~6 calendar months at the current cadence** |

That's for an integer-coordinate, Win32-only, single-frame DUIM
demo. A full faithful port that includes recording, presentations,
formatting tables, and the layout-pane algebra would add 3-5 more
sprints (mostly to lock down the macro coverage for the formatting
DSL and to validate the dispatch resolver against the wide-MI panes
in the layouts library).

**Recommendation.** Don't port DUIM verbatim. The reasons:

- DUIM-Win32 was last touched in 2004 (per the Functional Objects
  copyright on every file). It targets pre-Vista USER32 + GDI32; no
  HiDPI awareness, no UWP composition, no Direct2D, no DirectWrite.
  The other agents' bit-rot pass (`03-bit-rot.md`) covers this in
  more detail, but the language angle is: even if NewOpenDylan
  caught up to every feature in this report, you'd end up with a
  GUI that looks and feels twenty years old.
- The 13-sprint NewOpenDylan investment above is real work that
  pays dividends regardless — multi-library compilation, multi-
  mutator GC, the threads library, macros that token-paste, virtual
  slots — but you'd be paying it *for* DUIM, then spending the next
  six months porting DUIM, then discovering you want to draw with
  Direct2D anyway. NodIDE's existence (per the memory entry on
  `project_nodide.md`) suggests the direction is already set: a
  Direct2D/DirectWrite MDI shell wins over an UpdateLayered-window-
  era DUIM port.
- The right value to extract from DUIM is the **protocol design**:
  the sheet/medium/port abstraction, the gadget mixin hierarchy,
  the `handle-event` dispatch, the recording-vs-replay separation.
  These are decoupleable from the Win32 implementation. A fresh
  framework that draws inspiration from DUIM — built atop the
  modernised D2D/DWrite stack NodIDE is already using — gets the
  conceptual win without the bit-rot tax.

In other words: **port-and-modernize is theoretically possible
but economically dominated; write a fresh framework that takes the
abstractions from DUIM and builds them on a 2026 Windows backend.**
The 13 NewOpenDylan-side sprints get done either way (NodIDE itself
needs most of them — threads, multi-library, virtual slots,
`<deque>`, format), so paying them down lets *both* a fresh
framework and a DUIM port be possible; the choice of which to
actually build with them is then a clean question of taste rather
than a forced hand.
