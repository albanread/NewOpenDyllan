# DUIM Win32 Backend Audit

Read-only audit of the Open Dylan DUIM Win32 backend (`sources/duim/win32/`),
focused on porting risks for NewOpenDylan on modern x64 Windows.

All file:line references are against the clone at `E:\opendylan\` (commit at
time of audit).

## TL;DR

DUIM's Win32 backend is a **18,674-line, pure-GDI, pure-ANSI, x86-only** widget
mirror layer written in 1995-2004 and only lightly maintained since. It does
not use Direct2D, GDI+, DirectWrite, OLE, or COM (beyond a one-function
`IMalloc::Free` shim). It does not use drag-and-drop, MDI, or any threading
of its own. It bundles its own private 178-function FFI binding
(`win32-c-definitions.dylan`) rather than reusing `sources/win32/`. The
backend was clearly architected with the assumption that pointers, `LPARAM`,
`WPARAM`, and `LRESULT` are 32-bit, and several concrete code paths bake that
in (most importantly `SetWindowLong` / `GetWindowLong` for subclassing). On
x64 that is a load-bearing breakage. On the upside: the API surface is
limited, the bindings are localized to one file, and there are no exotic
subsystems to chase.

## File inventory and sizes

`sources/duim/win32/` contains 26 Dylan source files plus one `.c` shim, one
manifest, and one `.rc`. Sizes:

```
   16  duim-library.dylan
  120  ffi-bindings.dylan       -- shell-folder bindings + IMalloc shim
   20  library.dylan            -- library def (only uses duim-core, duim-utilities, C-FFI)
   96  module.dylan
  196  wclipboard.dylan
  116  wcolors.dylan
 2791  wcontrols.dylan          -- common-controls wrappers (progress, trackbar, status, tab, ...)
    9  wdebug.dylan
  933  wdialogs.dylan
   49  wdisplay.dylan
  645  wdraw.dylan              -- GDI drawing primitives
  733  wevents.dylan            -- message pump, WndProc
  471  wfonts.dylan             -- LOGFONT + GetTextMetrics
  180  wframem.dylan
 2616  wgadgets.dylan           -- standard control wrappers (BUTTON, EDIT, LISTBOX, ...)
   65  whandler.dylan
  398  whelp.dylan              -- WinHelp + HtmlHelp
 2166  win32-c-definitions.dylan -- private 178-function FFI (ALL ANSI)
  813  win32-definitions.dylan   -- constant tables, structs
  575  wkeyboard.dylan
  589  wmedium.dylan            -- HDC graphics state caching
 1019  wmenus.dylan
  790  wmirror.dylan            -- HWND <-> mirror map, window-class registration
  521  wpixmaps.dylan           -- BitBlt + LoadBitmap + LoadImage
  578  wport.dylan              -- port object, GetVersionEx-based OS branching
  734  wresources.dylan         -- hand-parses RT_DIALOG/RT_BITMAP/RT_ICON/RT_CURSOR
 1141  wtop.dylan               -- top-level frame
  294  wutils.dylan
-----
18674 total
```

The `.lid` file (`sources/duim/win32/win32-duim.lid:47`) declares
`Platforms: x86-win32` and pulls in `ole32.lib`, `uuid.lib`, `shell32.lib`,
`comdlg32.lib`, `comctl32.lib`, `user32.lib`, `gdi32.lib`, `advapi32.lib`,
`htmlhelp.lib` — that's the link list. The DLL is built with a fixed base
address (`E:\opendylan\sources\duim\win32\duim.lid:9` -> `0x65c00000`,
x86-only), and `Platforms: x86-win32` again at `duim.lid:18`.

## 1. Drawing model — pure GDI, no Direct2D anywhere

**Finding: DUIM is 100% GDI**, drawing through `HDC`, `HGDIOBJ`, `HPEN`,
`HBRUSH`, `HFONT`, `HBITMAP`. There is **no GDI+, no Direct2D, no DirectWrite,
no Direct3D**, and the only D2D/DirectX-shaped indirection is `c-com.c`
freeing memory via `IMalloc::Free`.

Distinct GDI calls in `wdraw.dylan` (645 lines): `AbortPath`, `Arc`, `ArcTo`,
`BeginPath`, `CloseFigure`, `Ellipse`, `EndPath`, `FillPath`, `LineTo`,
`MoveToEx`, `Pie`, `PolyBezierTo`, `Polygon`, `Polyline`, `Rectangle`,
`RoundRect`, `SelectClipPath`, `SelectObject`, `SetPixel`, `SetTextAlign`,
`StrokeAndFillPath`, `StrokePath`, `TabbedTextOut`, `TextOut`
(24 distinct primitives, plus the path/clip helpers).

Pixmap (`wpixmaps.dylan`): `BitBlt`, `CreateCompatibleDC`,
`CreateCompatibleBitmap`, `LoadImage`, `LoadBitmap`, `CreatePatternBrush`,
`DrawIconEx`, `GetIconInfo`, `GetObject`, `GetSystemMetrics` —
the classic 1990s GDI compositing path.

Medium (`wmedium.dylan`): caches `<HPEN>`, `<HBRUSH>`, `<HFONT>` per medium
(slots `%hPen`/`%hBrush`/`%hFont` at lines 33–40); cycles them with
`SelectObject`/`DeleteObject`; selects clipping regions with `SelectClipRgn`;
sets the bit-rop mode with `SetROP2`/`SetBkMode`. Stock objects are pulled
with `GetStockObject` (`wmedium.dylan:60-69`) — and notably the wanted
constants include `$ANSI-FIXED-FONT` and `$ANSI-VAR-FONT` (`wmedium.dylan:68-69`),
which only make sense if you are also assuming an ANSI code page everywhere
else.

Fonts (`wfonts.dylan`): all measurement goes through GDI's `GetTextMetrics`
(line 256, 315), `GetTextFace` (line 325), `GetTextExtentPoint32` (line 463),
and `GetTabbedTextExtent` (line 438). There is **no DirectWrite** —
no `IDWriteFactory`, no `IDWriteTextLayout`, no glyph-run rendering, no font
fallback. Hi-DPI font selection is approximate (the `point-size` formula at
`wfonts.dylan:377-378` divides by `win32-pixels-per-inch`, which itself is set
once at port-init via `GetDeviceCaps(LOGPIXELSY)` — no per-monitor DPI).

**Implication for NewOpenDylan**: NewOpenDylan's stdlib draws via
Direct2D / DirectWrite, which is a wholly different model (retained-mode-ish,
GPU-composited, ID2D1RenderTarget-centred, premultiplied-alpha, no
GDI handle space). There is no path from D2D primitives to DUIM's protocol
without either:

  - (a) emulating a `<medium>`-like GDI-style facade *on top of* D2D, which is
    plausible but adds a non-trivial layer (a few thousand lines, similar to
    what Direct2D Effects libraries do); or
  - (b) adding actual GDI primitives — `BitBlt`, `LineTo`, `Polygon`,
    `TextOut`, etc. — to NewOpenDylan's stdlib alongside D2D, and letting
    DUIM keep its GDI lifestyle. This is the smaller change.

Option (b) is genuinely smaller because GDI is mostly thin: every primitive
above is a single `__stdcall` call into `gdi32.dll` taking integers and
handles. The HGDIOBJ lifetime is manual but DUIM already handles that
(macro `with-temporary-gdi-object` at `wdraw.dylan:19`). The reason to consider
(a) anyway is that GDI is increasingly second-class on modern Windows — no
per-monitor DPI awareness, no hardware compositing, jagged glyphs, and
Microsoft's Common-Controls v6 themed renderers themselves use D2D internally
for some controls.

## 2. API inventory by DLL

The link list at `win32-duim.lid:33-41` is the authoritative list of DLLs.
Counts of distinct `define C-function` declarations in DUIM's private FFI
(`win32-c-definitions.dylan`, 178 total) plus the bindings in
`ffi-bindings.dylan` (the shell + IMalloc helpers):

| DLL              | Approx call sites                            | Notes |
|------------------|---------------------------------------------|-------|
| `gdi32.dll`      | ~40 (Arc, BitBlt, all `Create*` brushes/pens/fonts, GDI text, paths, regions) | section beginning `win32-c-definitions.dylan:780` |
| `user32.dll`     | ~90 (windowing, messages, menus, keyboard, mouse, dialogs, scroll, caret, cursor) | bulk of `win32-c-definitions.dylan` |
| `kernel32.dll`   | 12 (`Global{Alloc,Lock,Unlock}`, `ExitProcess`, `Get/SetLastError`, `GetModuleFileName/Handle`, `GetProcAddress`, `GetStartupInfo`, `OutputDebugString`, `GetVersionEx`, `IsDBCSLeadByte`) | line 1129-1206 |
| `comctl32.dll`   | 6 (`InitCommonControls`, `ImageList_Create`, `ImageList_Destroy`, `ImageList_Add`, `ImageList_ReplaceIcon`, custom-draw notifications) | line 2031-2069 |
| `comdlg32.dll`   | 6 (`GetOpenFileName`, `GetSaveFileName`, `ChooseColor`, `ChooseFont`, `PrintDlg`, `CommDlgExtendedError`) | line 1996-2029 |
| `shell32.dll`    | 2 (`SHBrowseForFolder`, `SHGetPathFromIDList`) + `SHGetMalloc` | `ffi-bindings.dylan:89-106` |
| `ole32.lib`      | linked only for `SHGetMalloc`'s `IMalloc` vtable | `c-com.c` |
| `htmlhelp.lib`   | 1 (`HtmlHelp` at line 2071) plus WinHelp | `whelp.dylan` |
| `advapi32.dll`   | 2 (`RegCloseKey`, `RegOpenKeyEx` at line 2080-2094) | minimal use |
| `kernel32.dll` (SxS) | 2 (`CreateActCtx`, `ActivateActCtx` at line 2096-2109) | for Common-Controls v6 activation context |

No `winmm.dll`, no `winspool.drv`, no `dwrite.dll`, no `d2d1.dll`, no
`uxtheme.dll`, no `dwmapi.dll`, no `shcore.dll` (DPI awareness lives there).

The `htmlhelp.lib` dependency is also interesting: HTML Help (`hhctrl.ocx`)
is still present on Windows 10/11 but Microsoft considers it legacy and has
shipped no new features for it. The replacement is `IMicrosoftHelpServices` /
Edge-hosted help — porting WinHelp/HtmlHelp gracefully degrades to "this
just doesn't work on Windows 11" without much code change.

### Modern vs legacy form

Every API DUIM calls is from the pre-2000 Win32 surface. No
`RegisterClassExW`, no `CreateWindowExW`, no `SetWindowLongPtrA`, no
`GetSystemMetricsForDpi`, no `AdjustWindowRectExForDpi`, no
`SetThreadDpiAwarenessContext`. The single newest call is
`CreateActCtx`/`ActivateActCtx` for side-by-side manifest activation
(Windows XP era).

## 3. 32-bit vs 64-bit — the worst find

**This is the load-bearing finding.** DUIM (via `sources/win32/win32-common/`)
declares the kernel integer types as plain 32-bit:

`E:\opendylan\sources\win32\win32-common\first.dylan:68-70`:
```dylan
// These types could hold either an integer or a pointer:
define constant <LPARAM>  = <C-both-long>;
define constant <LRESULT> = <C-both-long>;
define constant <WPARAM>  = <C-both-unsigned-int>;
```

`<C-both-long>` and `<C-both-unsigned-int>` are 32-bit. On Win64 the real
ABI requires `LPARAM = LONG_PTR` (signed pointer-sized), `WPARAM = UINT_PTR`,
`LRESULT = LONG_PTR` — all 64 bits. Every `SendMessage(handle, ...,
pointer-address(buffer))` site (and there are dozens, e.g.
`wcontrols.dylan:496`, `wcontrols.dylan:592`, `wcontrols.dylan:706`) is
passing a 64-bit pointer through a 32-bit slot. Win64 will sign-extend on
return and truncate on call: silent data corruption, not a clean error.

The comment block at `first.dylan:14-21` even disclaims this:
```
/*   // was used temporarily before `limited' worked:
// integer subranges used internally:
define constant <U8> = <integer>;
...
*/
```
and the `winnt.dylan` autogen header at `winnt.dylan:20` says explicitly
"This module defines the **32-Bit** Windows types and constants" (emphasis
mine). The codebase is honest about being 32-bit. It just never made the
transition.

The DUIM-layer abstraction at `wutils.dylan:18-21` widens these to `<object>`:
```dylan
define constant <wparam-type>  = <object>;
define constant <lparam-type>  = <object>;
define constant <lresult-type> = <object>;
define constant <message-type> = <signed-long>;
```
…but `<object>` is just "open" — the FFI underneath still marshals to
`<C-both-long>`, so the truncation happens at the FFI boundary regardless.

### Concrete worst-case: WndProc subclassing

`wgadgets.dylan:486-493`:
```dylan
define sealed method note-mirror-created
    (gadget :: <win32-subclassed-gadget-mixin>, mirror :: <window-mirror>) => ()
  next-method();
  let handle = window-handle(mirror);
  let old-wndproc
    = SetWindowLong(handle, $GWL-WNDPROC, pointer-address(SubclassedWndProc));
  gadget.%old-WndProc := make(<WNDPROC>, address: old-wndproc)
end method note-mirror-created;
```

This is the canonical Win64 break. `SetWindowLong` is the 32-bit-result API;
**`SetWindowLongPtr` must be used on Win64** for `GWL_WNDPROC`. The
declaration at `win32-c-definitions.dylan:1801-1807` returns `<LONG>` and
takes `<C-both-long>` — both 32-bit. On Win64 the function pointer
`pointer-address(SubclassedWndProc)` will be truncated to its low 32 bits
when written, and the returned `old-wndproc` will be a 32-bit-truncated
old WNDPROC. The resulting `make(<WNDPROC>, address: old-wndproc)`
constructs a function pointer into low memory — call it and you have an
access violation.

The same pattern recurs at `wmirror.dylan:474`, `wcontrols.dylan:1523`,
`wcontrols.dylan:1530`, `wcontrols.dylan:1782`, `wcontrols.dylan:1790`
(the latter four use `$GWL-STYLE` which *is* 32-bit even on Win64 so they
happen to work, but the binding is wrong on principle).

### `GetVersionEx` and OS branching

`wport.dylan:50-67` calls `GetVersionEx` and branches on
`$VER-PLATFORM-WIN32S` (Win32s on Windows 3.1!),
`$VER-PLATFORM-WIN32-WINDOWS` (Windows 95/98/ME), and
`$VER-PLATFORM-WIN32-NT`. `GetVersionEx` is deprecated since Windows 8.1
and returns 6.2 (Windows 8) for any later OS unless the app manifest
specifies otherwise. The Win95/98 branches are dead code on any modern
Windows; the only branch ever taken is `Windows-NT`. The
`*rectangle-fudge-factor*` at `wdraw.dylan:11-17` ("rectangles on NT are
one pixel shorter than on Windows...") only ever fires the NT path.

### Other 32-bit assumptions

- `<HRESULT> = <machine-word>` at `ffi-bindings.dylan:24` is **correct**
  (machine-word is pointer-sized in NewOpenDylan). Good.
- `<C-HRESULT> = <C-raw-signed-long>` at `ffi-bindings.dylan:27` is
  correct — HRESULT really is 32-bit signed.
- The hash table mapping HWND -> mirror (`wmirror.dylan:24,29,34`) uses
  `pointer-address(handle)` as the key. As long as `pointer-address`
  returns a `<machine-word>`, this is portable to x64. Probably fine.
- `wresources.dylan:576-577` defines
  `$dlg-template-size :: <integer> = 18;` and walks the binary
  `DLGTEMPLATE`/`DLGITEMTEMPLATE` resource by hand using these fixed
  offsets. The DLGTEMPLATE format is OS-defined and unchanged on Win64,
  so this happens to still work, but it is fragile and the alternative
  (DLGTEMPLATEEX from richer .rc compilers) is unsupported by this parser.

## 4. ANSI vs Unicode — uniformly ANSI

DUIM's private FFI (`win32-c-definitions.dylan`) has **52 explicit
`*A` c-names and zero `*W` c-names**:

```
grep 'c-name: "[A-Za-z_]+A"' .../win32-c-definitions.dylan | wc -l  -> 52
grep 'c-name: "[A-Za-z_]+W"' .../win32-c-definitions.dylan | wc -l  -> 0
```

The auxiliary `sources/win32/win32-user/winuser.dylan` (which DUIM does *not*
directly import — it has its own bindings — but which the rest of the Open
Dylan tree uses) is even more lopsided: **107 ANSI, 0 Unicode** at
`E:\opendylan\sources\win32\win32-user\winuser.dylan`. Same for
`sources/win32/win32-gdi/wingdi.dylan`: **39 ANSI, 0 Unicode**.

DUIM defines `<LOGFONTA>` (line 54-72), `<DEVMODEA>` (line 76-110),
`<TEXTMETRICA>` (line 128-151), `<WNDCLASSA>` (line 363-376) — the
*A-suffixed* layouts with 8-bit `<CHAR>` arrays inside (e.g.
`lfFaceName-array :: <CHAR>, length: $LF-FACESIZE` at line 68-69).
There are no W-suffixed equivalents anywhere.

`sources/win32/win32-common/first.dylan:323-329` even has a TODO admitting
the gap:

```dylan
// This equivalent for the Win32 `TEXT' macro will need some more work
// if and when we support using the Unicode version of the API for NT.
// NOTE -- This implementation relies on the runtime padding strings with a null!
// WARNING -- This implementation can have problems with the garbage collector
//   as the GC won't know that C might have saved a pointer to the string!
//   But it should only be used on literals anyway.

define generic TEXT (string :: <string>) => (value :: <C-string>);
```

`TEXT` always returns a `<C-string>` (byte string), never a wide string.

**Implication**: NewOpenDylan defaults to W APIs (per Sprint 30). DUIM's
private FFI must be re-pointed: change every `c-name: "...A"` to `"...W"`,
change `<CHAR>` arrays in structs to `<WCHAR>`, change every `<LPSTR>` /
`<LPCSTR>` / `<C-string>` parameter and slot type to the Unicode counterpart,
and replumb the `TEXT` macro to actually transcode. The mechanical churn
is large — every one of the 178 C-function declarations plus their callers
needs verification — but the work is shallow.

A possible shortcut: keep the bindings as-is (call `*A` APIs) and let
Windows do the codepage translation. Modern Windows still has the *A APIs
and they work, but they have a CP_ACP-dependent string interpretation,
which means anything outside the active codepage round-trips badly. DUIM
text gadgets and clipboard would lose non-Latin-1 input. Acceptable for a
proof-of-concept; not acceptable as a final state.

The clipboard layer underscores this: `wclipboard.dylan:59,83,91` exclusively
uses `$CF-TEXT` (ANSI clipboard format) — not `$CF-UNICODETEXT`. So even
text copied through DUIM today is codepage-bound.

## 5. Common controls + MDI

DUIM wraps the standard Windows widgets via `comctl32`. Class names hard-coded
at `win32-definitions.dylan:805-811`:

```
$STATUSCLASSNAME = "msctls_statusbar32"
$TRACKBAR-CLASS  = "msctls_trackbar32"
$PROGRESS-CLASS  = "msctls_progress32"
$UPDOWN-CLASS    = "msctls_updown32"
$WC-LISTVIEW     = "SysListView32"
$WC-TABCONTROL   = "SysTabControl32"
$WC-TREEVIEW     = "SysTreeView32"
```

…plus the basic system classes used in `wgadgets.dylan`: `"BUTTON"`
(lines 1248, 1524), `"EDIT"` (536, 715), `"STATIC"` (559), `"SCROLLBAR"`
(1639), `"LISTBOX"` (1857), `"COMBOBOX"` (2073, 2236).

So DUIM uses: the seven common controls, plus all six basic controls. That
is essentially everything a 1990s-style Win32 app needs.

Initialisation: `wcontrols.dylan:74` calls `InitCommonControls()` — the
*old* signature, which loads only the v3 control set. Modern code uses
`InitCommonControlsEx(ICC_*)` to pick which classes to load. DUIM compensates
by side-loading the v6 visual-style assembly via a side-by-side manifest
(`dxwduim.dll.manifest`):

`dxwduim.dll.manifest:5,16` hard-code `processorArchitecture="X86"` — the
manifest itself would need updating to `amd64` for x64 DLLs, otherwise
the Common-Controls v6 activation context lookup fails and you fall back
to the v5 look-and-feel (gray 3D widgets, no themes).

**MDI: DUIM is not MDI-based.** The only mention of MDI in the whole
backend is a comment at `wevents.dylan:471` (`"What if there's more than one
top level sheet, e.g. MDI?"`). There is no `"MDICLIENT"` window class
registration, no `WM_MDI*` message handling, no `TranslateMDISysAccel`. Each
top-level frame is its own overlapped HWND. Good news for a modern IDE
UX — no MDI baggage to undo.

Window subclassing: DUIM does subclass widgets to inject custom event handling
(`wgadgets.dylan:484` `SubclassedWndProc`, `wgadgets.dylan:491`
`SetWindowLong(..., $GWL-WNDPROC, ...)`). On modern Windows the preferred
mechanism is `SetWindowSubclass` from `comctl32` (since v6) — using the
`SetWindowLong*` trick still works but reentrant subclass chains can be
fragile. Not strictly a porting blocker.

## 6. Resources

DUIM uses Win32 binary resources extensively:

- `version.rc` (the `.rc` script) at `sources/duim/win32/version.rc` embeds
  the Common-Controls v6 manifest plus a `VS_VERSION_INFO` block. Listed
  in the LID at `duim.lid:12` as `RC-Files: version.rc`.
- DUIM **loads** resources at runtime: `LoadBitmap` (line 1829),
  `LoadCursor` (1836), `LoadIcon` (1843), `LoadImage` (1850),
  `FindResourceEx` (1909), `LoadResource` (1918), `SizeofResource` (1925),
  `EnumResourceTypes`/`Names`/`Languages` (1882-1907).
- DUIM **parses dialog templates** by hand. `wresources.dylan:576-700` walks
  the binary `RT_DIALOG` blob — extracting menu name, class name, title
  string, font info, then a sequence of `DLGITEMTEMPLATE` records, with
  hard-coded constants `$dlg-template-size = 18` and `$item-template-size =
  18`. This is the legacy (non-Ex) template format. `DLGTEMPLATEEX` (with
  v6 extensions, larger fonts, etc.) is unsupported.
- A "grokker table" at `wresources.dylan:550-575` handles `$RT-BITMAP`,
  `$RT-ICON`, `$RT-CURSOR`, `$RT-DIALOG`. **Not handled** per comments at
  `wresources.dylan:529-538`: `$RT-ACCELERATOR`, `$RT-FONT`, `$RT-MENU`,
  `$RT-RCDATA`, `$RT-STRING`, `$RT-MESSAGETABLE`, `$RT-GROUP-CURSOR`,
  `$RT-GROUP-ICON`, `$RT-VERSION`. Yet the app uses
  `CreateAcceleratorTable`/`TranslateAccelerator` (line 1462-1481) and
  `LoadMenu` indirectly — so accelerators and menus must be authored
  programmatically, not via `.rc`.

**Implication**: NewOpenDylan needs `windres.exe` (or `rc.exe`, or equivalent)
in its build path to compile a `.rc` to a `.res`, and the linker needs to
embed it. The `LID` line `RC-Files: version.rc` shows Open Dylan's build
system already supports this — porting the convention is mostly a matter of
wiring `windres` or `rc.exe` into NewOpenDylan's tool chain. Without it,
DUIM apps lose icon resources, the side-by-side manifest (so no themed
common controls), and dialog templates. The first two are merely ugly; the
third actively breaks any DUIM application that ships dialog resources.

## 7. OLE / drag-and-drop / clipboard

**OLE / COM**: essentially absent. The only COM use in the entire DUIM Win32
backend is the one-function `c-com.c` shim calling `IMalloc::Free` via its
vtable, which is consumed only by the shell-folder picker
(`ffi-bindings.dylan:14-18`, `89-106`). DUIM does not register class objects,
does not call `CoInitialize`, does not consume `IDispatch`, does not implement
any COM interfaces. The `sources/ole/` subtree is a separate library outside
the backend's scope.

**Drag-and-drop**: zero. No `IDropTarget`, no `IDropSource`, no
`RegisterDragDrop`, no `DoDragDrop`, no `DragAcceptFiles`, no
`WM_DROPFILES` handler. DUIM apps cannot accept dropped files today.

**Clipboard**: classic, ANSI-only (`wclipboard.dylan`). Uses
`OpenClipboard`/`SetClipboardData`/`GetClipboardData`/`EnumClipboardFormats`
with `$CF-TEXT` exclusively (lines 59, 83, 91). Buffers are allocated via
`GlobalAlloc($GMEM-MOVEABLE | $GMEM-DDESHARE)` (line 124-125); `$GMEM-DDESHARE`
has been a no-op since Windows 2000 but is harmless. No `IDataObject`. No
Unicode text format. A "blew out on Win-95 from time to time" comment at
line 145 dates the code.

**Implication for NewOpenDylan COM shims**: minimal. Clipboard porting is
moving from `CF_TEXT` to `CF_UNICODETEXT` and that is the entire change.
Drag-and-drop would have to be added from scratch (it's not a port). The
shell folder picker can be retired and replaced with `IFileOpenDialog`
(Vista+, COM-based) — but that **does** need a real `IUnknown`/`IFileDialog`
COM call path in NewOpenDylan's runtime.

## 8. Threading model

DUIM does not spawn its own threads. There are zero `CreateThread` /
`_beginthreadex` / `make-thread` calls in the entire backend. The only thread-
related declarations are `define thread variable` for per-thread state caches:

- `wevents.dylan:55`: `*lpmsg*` (per-thread cached message struct)
- `wutils.dylan:29`: `*port-did-it?*`
- `wresources.dylan:356-359`: `*current-database*`, `*current-type*`,
  `*current-type-table*`, `*current-resource*`

…all of which are dynamic-bound during single-threaded resource enumeration.

The window-callback model assumes the UI thread is the only thread invoking
WndProcs (which Win32 guarantees per-HWND). There is no COM-apartment setup
(`CoInitializeEx` is never called). No `AttachThreadInput` either.

**Implication**: DUIM is single-threaded by construction. Compatible with
NewOpenDylan's current single-mutator model (Sprint 11c thread-local GC
roots) without any further work. If/when NewOpenDylan grows multi-threading,
DUIM will need none of the locking infrastructure other modern UI toolkits
require.

## 9. Bit-rotten subsystems flagged in passing

Detailed bit-rot analysis is another agent's job; what jumped out while
walking the backend:

- **OS branching for Windows 95/98/NT** at `wport.dylan:50-67`. Uses
  `GetVersionEx` (deprecated since Win 8.1). The `Win32s`, `Windows-95`,
  `Windows-98` branches are dead code on any supported modern Windows.
- **"Windows-NT" rectangle fudge factor** at `wdraw.dylan:11-17` — applies a
  one-pixel adjustment that originally compensated for NT vs Win9x GDI
  rounding. Always active now.
- **Win-95 comment debris** at `wclipboard.dylan:114` and `wclipboard.dylan:145`
  ("the error code is not setup in Windows 95/98", "blew out on Win-95 from
  time to time").
- **Win-95 keyboard quirks** at `wkeyboard.dylan:176,232,363,402,542,547` —
  whole code paths branching on `_port.%os-name == #"Windows-NT"` for AltGr
  detection, ANSI codepage input, etc. The else-branch handles Win9x.
- **WinHelp** (`whelp.dylan` line 1948 binding) — WinHlp32.exe was removed
  from Windows 10 by default. The HtmlHelp fallback at line 2071 still works.
- **MDI comment at `wevents.dylan:471`** — speculative future support that
  never landed.
- **Hard-coded `processorArchitecture="X86"`** in the manifest at
  `dxwduim.dll.manifest:5,16` and `Platforms: x86-win32` in both
  `duim.lid:18` and `win32-duim.lid:47`.
- **No DPI awareness**. No `SetThreadDpiAwarenessContext`, no manifest
  `<dpiAware>` block, no `WM_DPICHANGED` handling, no
  `GetSystemMetricsForDpi`. On a 4K monitor, DUIM apps render at 96-DPI
  bitmap-stretched — fuzzy text, fuzzy icons.
- **Comment at `wmirror.dylan:145`**: "`Should be 'own-dc?: _port.%os-name
  == #"Windows-NT"'`" — a stale TODO from the Win9x era.

## 10. Closing summary

### What's the Win32 backend's drawing-model situation?

It's **pure 1990s GDI**, with about two dozen distinct GDI primitives in
`wdraw.dylan`, classic `HDC`/`HGDIOBJ`/`HPEN`/`HBRUSH`/`HFONT` state caching
in `wmedium.dylan`, `BitBlt`-based pixmap compositing in `wpixmaps.dylan`,
and `GetTextMetrics`/`TextOut` font handling in `wfonts.dylan`. There is
zero Direct2D, zero DirectWrite, zero GDI+. The cheapest port path is to
**add GDI primitives to NewOpenDylan's stdlib alongside its Direct2D
support** — GDI is a thin static-link surface (everything is one
`__stdcall` into `gdi32.dll`) and DUIM already manages object lifetimes.
A from-scratch retarget of DUIM onto D2D would be a multi-month rewrite of
`wdraw.dylan`/`wmedium.dylan`/`wpixmaps.dylan`/`wfonts.dylan` (around
2200 lines) and would have ripple effects through the medium / region /
transform abstractions. The pragmatic choice is GDI primitives, with D2D
left as a "future modernisation" sprint.

### What APIs would NewOpenDylan's nod-runtime need to cover for DUIM?

To get DUIM minimally booting, NewOpenDylan needs FFI coverage for:

  - **`gdi32.dll`** — about 40 functions, all `__stdcall`, integer/handle
    args. The full list is the section beginning at
    `win32-c-definitions.dylan:780`.
  - **`user32.dll`** — about 90 functions (windowing, messages, menus,
    keyboard, mouse, cursors, carets, dialogs, scroll, accelerators).
    **Critically including the `*Ptr` variants for Win64**:
    `SetWindowLongPtrW`, `GetWindowLongPtrW`, `GetClassLongPtrW`,
    `SetClassLongPtrW`. The whole subclassing path depends on these.
  - **`kernel32.dll`** — `Global{Alloc,Lock,Unlock,Free}`, `GetLastError`,
    `SetLastError`, `GetModuleHandle`, `GetProcAddress`,
    `OutputDebugString`, `FormatMessage`, `CreateActCtx`, `ActivateActCtx`.
    Many of these may already exist for other stdlib reasons.
  - **`comctl32.dll`** — `InitCommonControlsEx`, the seven common control
    class names, image-list functions, and the `WM_NOTIFY` / `NMHDR`
    notification protocol.
  - **`comdlg32.dll`** — six dialog wrappers (`GetOpenFileNameW`, etc.).
    Could be skipped initially and stubbed out — apps that don't use file
    dialogs would still run.
  - **`shell32.dll`** — replaceable. The legacy `SHBrowseForFolder` /
    `SHGetPathFromIDList` /`IMalloc::Free` path can be retired in favour of
    `IFileOpenDialog` (Vista+ COM). If `IFileOpenDialog` is preferred, then
    NewOpenDylan also needs **basic COM vtable support** —
    `CoInitializeEx`, `CoCreateInstance`, vtable calls — which it
    apparently already has via the `windows` crate.
  - **Resource-compiler bridge**: invoke `rc.exe`/`windres` from the
    NewOpenDylan build to compile `.rc` -> `.res` -> linked. Otherwise
    icons, manifests, version blocks, and dialog templates are lost.

Total mechanical churn is real (the FFI re-pointing from `*A` to `*W`
across 178 declarations plus all their call sites) but the API surface is
fixed and finite — no surprises, no exotic subsystems.

### The worst find

**`SetWindowLong` for `GWL_WNDPROC` at `wgadgets.dylan:491`** (also
`wmirror.dylan:474`). DUIM uses `SetWindowLong` — the 32-bit-result API — to
install its `SubclassedWndProc` window-procedure trampoline for every
subclassed control. On Win64 the function pointer is 64 bits, but
`SetWindowLong` truncates to 32 bits. The truncated value is then both
written into the window slot (so Windows will jump to a nonsense address
on the next message) and returned and stuffed into the gadget's
`%old-WndProc` slot via `make(<WNDPROC>, address: old-wndproc)`. The latter
will be called by `CallWindowProc` at `wgadgets.dylan:481` — also into
low memory. Result: instant access violation the first time any
subclassed gadget receives a message. The fix is to bind `SetWindowLongPtrW`
/ `GetWindowLongPtrW` (return `<LONG_PTR>` ≈ `<machine-word>`) and use
those everywhere `$GWL-WNDPROC` is involved. The same wider fix applies to
the `WPARAM`/`LPARAM`/`LRESULT` declarations in
`sources/win32/win32-common/first.dylan:68-70` — those need to become
pointer-sized too, or every `SendMessage(handle, ..., pointer-address(buf))`
site silently corrupts on x64.

A close runner-up: **the entire FFI is ANSI**, including struct layouts
(`<LOGFONTA>`, `<DEVMODEA>`, `<WNDCLASSA>`, `<TEXTMETRICA>`), the
`TEXT` macro, the clipboard format (`CF_TEXT`), and the resource-string
parser. NewOpenDylan defaults to W. Either DUIM gets re-pointed to W
(large mechanical churn — 178 binding edits, plus calling-site audit), or
NewOpenDylan grows a parallel A-flavoured shim layer and the language
boundary just becomes lossy for non-Latin-1 text. There is no good "leave
it alone" path.
