# MessageBoxW from inside a WNDPROC: research notes

**Date:** 2026-05-23
**Context:** NewOpenDylan IDE (`F:\scratch\nod-ide-v3.exe`, Sprint 41e) calls
`MessageBoxW(parent_hwnd, L"NewOpenDylan IDE  -  Sprint 41e", L"About", MB_OK)`
from inside the WNDPROC's `WM_COMMAND` handler (menu click).

**Observed symptom:** the message box does not appear, the UI briefly shows the
busy spinner, then `MessageBoxW` returns and the WNDPROC continues normally.
With an explicit `define c-function MessageBoxW` declaration *and* the parent
HWND, the same call CRASHED instead of hanging. A standalone Sprint 30 test
calling `MessageBoxW(NULL, ...)` from `main()` works fine.

This document is read-only research. It does NOT modify any IDE code; it
collects what Microsoft and the community say about calling `MessageBoxW` from
inside a `WM_COMMAND` WNDPROC so we can pick a fix.

---

## 1. What MSDN documents about MessageBoxW from a WNDPROC

The canonical reference is the [MessageBox function (winuser.h)](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-messagebox)
page. Relevant facts:

* **`hWnd` is `[in, optional]`.** "A handle to the owner window of the message
  box to be created. If this parameter is NULL, the message box has no owner
  window." So calling with `NULL` is documented and legal.
* **`MB_APPLMODAL` is the default modality**: "The user must respond to the
  message box before continuing work in the window identified by the *hWnd*
  parameter. ... All child windows of the parent of the message box are
  automatically disabled, but pop-up windows are not."
* **No documented restriction on calling from inside a WNDPROC.** MSDN does
  not list "must not call from inside a WNDPROC" or "must not call during
  `WM_COMMAND`" anywhere on the MessageBox page or on
  [Dialog Box Programming Considerations](https://learn.microsoft.com/en-us/windows/win32/dlgbox/dlgbox-programming-considerations).
  Calling `MessageBox` from a `WM_COMMAND` handler is in fact the textbook
  pattern (see the MSDN code samples and the
  [Windows-classic-samples](https://github.com/microsoft/Windows-classic-samples)
  repository).
* **One specific warning that matters here**: "If you create a message box
  while a dialog box is present, use a handle to the dialog box as the *hWnd*
  parameter. **The *hWnd* parameter should not identify a child window**,
  such as a control in a dialog box." (winuser.h Remarks section).
* **Flags relevant to visibility**:
  * `MB_SETFOREGROUND` (0x00010000): "The message box becomes the foreground
    window. Internally, the system calls the
    [SetForegroundWindow](https://learn.microsoft.com/en-us/windows/desktop/api/winuser/nf-winuser-setforegroundwindow)
    function for the message box." On modern Windows, `SetForegroundWindow`
    is restricted (foreground-lock rules); a process not in the foreground
    may get only a taskbar flash.
  * `MB_TOPMOST` (0x00040000): adds `WS_EX_TOPMOST` style. This forces the
    box above all non-topmost windows but does not bypass foreground rules.
  * `MB_TASKMODAL` (0x00002000): same as `MB_APPLMODAL` except that *if
    `hWnd` is NULL* it disables all top-level windows belonging to the
    current thread. Specifically useful when the caller has no owner handle.
  * `MB_SYSTEMMODAL` (0x00001000): same as `MB_APPLMODAL` plus
    `WS_EX_TOPMOST`. Despite the name it does **not** block other apps;
    it is just a topmost taskbar-respecting box.
* **Failure mode**: "If the function fails, the return value is zero. To get
  extended error information, call
  [GetLastError](https://learn.microsoft.com/en-us/windows/desktop/api/errhandlingapi/nf-errhandlingapi-getlasterror)."
  The most common failure when called from a WNDPROC is GLE = 1400
  `ERROR_INVALID_WINDOW_HANDLE` (see [§3](#3-known-failure-modes-search-matches)).

There is no MSDN sentence anywhere that says "MessageBox must not be called
re-entrantly" or "WNDPROCs must not be reentered." Reentry is in fact the
*designed* behaviour: a modal dialog runs its own nested message loop and
pumps queued messages (`WM_PAINT`, `WM_TIMER`, `WM_NCPAINT`, etc.) back into
the same WNDPROC. See
[Using Messages and Message Queues](https://learn.microsoft.com/en-us/windows/win32/winmsg/using-messages-and-message-queues)
and Raymond Chen's
[Modality, part 4](https://devblogs.microsoft.com/oldnewthing/20050223-00/?p=36383)
and
[part 5](https://devblogs.microsoft.com/oldnewthing/20050224-00/?p=36373).

## 2. The standard pattern (C / C++ / Rust)

Every reference example calls `MessageBox(hwnd, ...)` directly from inside
`case WM_COMMAND:` with no special preparation:

```c
case WM_COMMAND:
    switch (LOWORD(wParam)) {
        case ID_FILE_ABOUT:
            MessageBoxW(hwnd, L"About", L"App", MB_OK | MB_ICONINFORMATION);
            break;
    }
    break;
```

This pattern is documented in
[WM_COMMAND message](https://learn.microsoft.com/en-us/windows/win32/menurc/wm-command),
in the Microsoft
[Windows-classic-samples](https://github.com/microsoft/Windows-classic-samples)
sample code, in the windows-rs
[MessageBoxW docs](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/WindowsAndMessaging/fn.MessageBoxW.html),
and in the well-known
[Rust Hello-World MessageBox](https://wesleywiser.github.io/post/rust-windows-messagebox-hello-world/)
tutorial. None require:

* `CoInitializeEx(NULL, COINIT_APARTMENTTHREADED)` (a search for "MessageBox"
  + "CoInitialize" turned up no requirement; MessageBox lives in user32.dll
  and does not touch COM).
* `SetProcessDpiAwarenessContext`. The
  [dotnet/wpf#6775 issue](https://github.com/dotnet/wpf/issues/6775)
  documents that per-monitor-DPI v2 makes MessageBox *blurry on secondary
  monitors*, not invisible — and Microsoft has said they will not fix
  MessageBox; they want callers to migrate to `TaskDialog` instead.
* Any window-style restriction (WS_OVERLAPPEDWINDOW, WS_VSCROLL, WS_HSCROLL).
  No documentation surfaced for any combination here causing invisibility.
* `MessageBoxEx` vs `MessageBoxIndirect`: `MessageBoxEx` only adds a
  language-ID parameter; `MessageBoxIndirect` lets you supply a custom icon
  and `WM_HELP` callback. Neither has different reentrancy or visibility
  semantics relative to `MessageBoxW`.

## 3. Known failure modes (search matches)

The two failure modes that match our symptom (box does not appear, call
returns) are:

### 3a. `WM_QUIT` posted into the nested loop

If the queue contains a `WM_QUIT` when `MessageBoxW` starts its nested message
loop, the box's loop sees the quit, exits *immediately* without painting, and
`MessageBoxW` returns. This is documented in:

* [How to handle WM_QUIT in nested message loop](https://www.mirabulus.com/it/blog/2019/10/18/how-to-handle-wmquit-in-nested-message-loop)
* MS KB Q89738
  [Handling WM_QUIT While Not in Primary GetMessage() Loop](https://jeffpar.github.io/kbarchive/kb/089/Q89738/)
* Raymond Chen
  [Modality, part 3: The WM_QUIT message](https://devblogs.microsoft.com/oldnewthing/20050222-00?p=36393)

A reply on the MS Q&A thread
[Value of main window hWnd becomes invalid](https://learn.microsoft.com/en-us/answers/questions/1156109/value-of-main-window-hwnd-becomes-invalid)
also notes that MFC may post `WM_QUIT` early, "so when you pop up the message
box, it immediately gets the WM_QUIT message and closes itself."

**Why this is plausible for nod-ide-v3**: if any earlier code path called
`PostQuitMessage(0)` (e.g. as a default `WM_DESTROY` branch in the WNDPROC, or
as a shutdown helper that ran during init), the `WM_QUIT` would sit in the
queue. The first nested loop to drain it (here, MessageBoxW's loop) would
return without ever displaying.

### 3b. `MessageBoxW` returns 0 with `GLE == 1400` (invalid HWND)

Several sources match this:

* MS Q&A
  [Value of main window hWnd becomes invalid?](https://learn.microsoft.com/en-us/answers/questions/1156109/value-of-main-window-hwnd-becomes-invalid)
* GameDev.net
  [CreateWindow() has Error 1400](https://gamedev.net/forums/topic/586926-createwindow-has-error-1400-invalid-window-handle/4729766/)
* exchangetuts
  [Win32 MessageBox doesn't appear](https://exchangetuts.com/index.php/win32-messagebox-doesnt-appear-1640828823912325)

The consistent advice is: **use the `hwnd` passed into the WNDPROC, not a
globally-stored handle**, because the latter can race with destruction or can
simply be wrong (e.g. zero, or the handle of a child window which the MSDN
remarks forbid). The fact that adding the parent HWND *and* an explicit
`define c-function` declaration made the previous call **crash** rather than
hang strongly suggests Dylan was passing an HWND that was either invalid or
of the wrong type (e.g. an `HMENU` cast to HWND, or a stale handle).

### 3c. Box appears behind the parent / foreground lock

Forum threads ([Bring MessageBox to Front, InstallShield](https://community.flexera.com/t5/InstallShield-Forum/Bring-MessageBox-to-Front/m-p/92903),
[Window unfocus when MessageBox must be shown](https://cplusplus.com/forum/windows/59077/))
report symptoms of a MessageBox that is "shown but hidden behind other
windows", with the user noticing only a taskbar flash. On modern Windows this
is the
[SetForegroundWindow](https://learn.microsoft.com/en-us/windows/desktop/api/winuser/nf-winuser-setforegroundwindow)
foreground-lock rule biting: a thread that did not initiate the foreground
change cannot raise a window. The fix is to add `MB_TOPMOST | MB_SETFOREGROUND`
to the `uType` flags.

However, this does **not** match our exact symptom ("UI shows busy spinner,
then returns"). A box that is merely hidden behind the parent does not
auto-dismiss. The spinner suggests the nested message loop *ran briefly* and
exited — i.e. the WM_QUIT scenario in 3a is closer.

### 3d. DPI-awareness blur on secondary monitor

Documented in [dotnet/wpf#6775](https://github.com/dotnet/wpf/issues/6775).
This makes the box *blurry*, not *invisible*; not a match.

### 3e. CoInitialize / STA requirement

No source I could find says `MessageBoxW` requires COM apartment
initialisation. MessageBox is a user32 dialog implemented on top of the
standard window manager; it does not call into COM. **Couldn't find** any
evidence this is the issue.

### 3f. WNDPROC reentrancy / TLS

There is no documented Win32 thread-local state that needs saving across a
recursive WNDPROC call. The whole modal-dialog mechanism is designed around
reentry. References:
[Sharing Message Loops Between Win32 and WPF](https://learn.microsoft.com/en-us/dotnet/desktop/wpf/advanced/sharing-message-loops-between-win32-and-wpf)
("Nested message pumps cause reentrancy. APIs that are running their own
nested message pumps call GetMessage() and DispatchMessage().").

But this is the angle most likely to bite a JIT-compiled language with FFI
trampolines: **what reenters is your trampoline, not Win32**. If the
trampoline that bridges Win32's stdcall WNDPROC into Dylan stores any
per-thread state in a global (current continuation, stack-walk root, GC
safepoint state, exception-handler chain), a recursive WNDPROC invocation
through MessageBox's nested loop will overwrite that state. When MessageBox
returns and the outer WNDPROC's trampoline tries to resume, it finds the
state belonging to the inner invocation. Depending on what was stored, this
could manifest as: crash with explicit FFI declaration (because the
declaration changed the trampoline shape), or "return without doing anything
visible" if the nested loop's first action was to handle a `WM_PAINT` that
itself caused another reentry that quietly aborted.

For NewOpenDylan specifically, the relevant subsystems are: the Sprint-30 FFI
marshaller, the GC safepoint mechanism, and the WNDPROC trampoline. Sprint 30
worked at top level (no reentry); the failure surfaces only when the WNDPROC
is reentered. **Couldn't find** any external source on this; it is a
Dylan-side hypothesis.

### 3g. Stack / shadow-space corruption (x64)

The Microsoft x64 ABI requires the caller to reserve 32 bytes of shadow space
and to keep `RSP` 16-byte aligned at the point of the `call`. See
[x64 Calling Convention](https://learn.microsoft.com/en-us/cpp/build/x64-calling-convention?view=msvc-170).
A trampoline that gets shadow-space or alignment subtly wrong will *usually*
crash on the first call, not hang. But MessageBox internally uses a lot of
COM-style indirection and `SendMessage` to its own controls; mis-aligned
stack at the call point can manifest as either a crash (matching the
"explicit declaration crashes" variant) or as a corrupted return-address
that lands back inside the same function and "succeeds" silently (matching
the "hangs briefly then returns" variant). The fact that two *different
declarations* produced two *different failure modes* (hang vs crash) is a
strong tell that the trampoline ABI is the variable.

## 4. Most likely root cause

The single most plausible explanation, weighing all the above:

> **The trampoline that bridges Win32's stdcall WNDPROC into Dylan is not
> reentry-safe.** Sprint 30's MessageBoxW call from `main()` works because
> nothing has yet stored per-thread state in the trampoline. The IDE's call
> from `WM_COMMAND` fails because MessageBox's nested message loop reenters
> the same WNDPROC trampoline (delivering `WM_PAINT`, `WM_NCPAINT`, etc. to
> the IDE window while the box is being constructed), and that re-entry
> corrupts a per-thread state slot the outer invocation depends on. The
> "explicit `define c-function`" variant crashes because the explicit
> declaration emits a different (less defensive) ABI shape that happens to
> trip immediately.

Secondary contributing causes that may be at work simultaneously:

* The `parent_hwnd` argument may actually be NULL, the wrong handle, or
  pointing at a child control — explaining why the version *with* explicit
  HWND crashes harder than the version that omits it.
* A `WM_QUIT` may have been queued by earlier code, so even on the rare
  invocations where the trampoline survives, MessageBox's nested loop exits
  immediately.

## 5. Three fixes to try, smallest first

1. **Pass `NULL` (or `0`) as the owner HWND, and use
   `MB_OK | MB_TASKMODAL | MB_SETFOREGROUND | MB_TOPMOST`.** This is the
   highest-confidence, smallest-blast-radius change. It tests the "wrong
   HWND" hypothesis directly: `MB_TASKMODAL` makes the lack of an owner
   safe, and the foreground/topmost flags work around the foreground-lock
   rule if the IDE thread is not foreground at the moment of the call. If
   the box appears with these flags but not without, the HWND was the
   problem. Cost: change one constant in the call site.

2. **Defer the call out of the WM_COMMAND handler with `PostMessage`.** Add
   a private message ID (e.g. `WM_APP + 1`), have `WM_COMMAND` `PostMessage`
   it to the same window, return immediately, then handle that message in a
   separate WNDPROC branch where the call to `MessageBoxW` happens with the
   outer `WM_COMMAND` already off the stack. This eliminates the reentrancy
   path entirely. This is the standard workaround documented in the
   "MessageBox called from WndProc menu hangs" community advice. If this
   makes the symptom go away, the reentrant-trampoline hypothesis is
   confirmed and you have a robust workaround until the trampoline is
   audited. Cost: one extra message dispatch branch.

3. **Audit the WNDPROC trampoline for reentry safety.** Specifically: any
   thread-local state the trampoline stashes around an FFI-in call (current
   continuation, GC safepoint pointer, exception-handler chain, "current
   Dylan task" handle) must be saved on entry and restored on exit, not
   stored in a global. This is the structural fix; fixes 1 and 2 are
   workarounds. Cost: depends on the trampoline; potentially a sprint of
   work. Defer until 1 and 2 confirm the diagnosis.

## 6. One diagnostic to run first

Before changing the call site, instrument the existing WNDPROC to log:

```c
LRESULT result = MessageBoxW(parent_hwnd, ..., MB_OK);
DWORD   err    = (result == 0) ? GetLastError() : 0;
// log: result, err, parent_hwnd, IsWindow(parent_hwnd), GetCurrentThreadId()
```

The three possible outcomes pin down the diagnosis cleanly:

* **`result == 0`, `err == 1400` (`ERROR_INVALID_WINDOW_HANDLE`)** → fix #1
  is the right answer; the HWND was wrong.
* **`result == IDOK` (1) but no box ever appeared** → `WM_QUIT` was in the
  queue (case 3a). Fix is to grep for stray `PostQuitMessage` calls in the
  IDE init/shutdown paths, not in the menu handler.
* **Crashes inside `MessageBoxW`, or `result` is some other non-zero
  value, or `IsWindow(parent_hwnd) == FALSE` just before the call** →
  trampoline / FFI-ABI bug (case 3f/3g); proceed to fix #2 as a workaround
  and schedule fix #3.

Running this three-line diagnostic costs about ten minutes and tells you
which of the three fixes to apply.

---

## Sources

* Microsoft Learn — [MessageBox function (winuser.h)](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-messagebox)
* Microsoft Learn — [MessageBoxW function (winuser.h)](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-messageboxw)
* Microsoft Learn — [WM_COMMAND message (Winuser.h)](https://learn.microsoft.com/en-us/windows/win32/menurc/wm-command)
* Microsoft Learn — [Dialog Box Programming Considerations](https://learn.microsoft.com/en-us/windows/win32/dlgbox/dlgbox-programming-considerations)
* Microsoft Learn — [Using Messages and Message Queues](https://learn.microsoft.com/en-us/windows/win32/winmsg/using-messages-and-message-queues)
* Microsoft Learn — [Window Messages (Get Started with Win32)](https://learn.microsoft.com/en-us/windows/win32/learnwin32/window-messages)
* Microsoft Learn — [Sharing Message Loops Between Win32 and WPF](https://learn.microsoft.com/en-us/dotnet/desktop/wpf/advanced/sharing-message-loops-between-win32-and-wpf)
* Microsoft Learn — [SetForegroundWindow function](https://learn.microsoft.com/en-us/windows/desktop/api/winuser/nf-winuser-setforegroundwindow)
* Microsoft Learn — [x64 Calling Convention](https://learn.microsoft.com/en-us/cpp/build/x64-calling-convention?view=msvc-170)
* Microsoft Learn — [SetProcessDpiAwarenessContext function](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setprocessdpiawarenesscontext)
* Microsoft Learn — [Setting the default DPI awareness for a process](https://learn.microsoft.com/en-us/windows/win32/hidpi/setting-the-default-dpi-awareness-for-a-process)
* Microsoft Learn (MS Q&A) — [Value of main window hWnd becomes invalid?](https://learn.microsoft.com/en-us/answers/questions/1156109/value-of-main-window-hwnd-becomes-invalid)
* Raymond Chen — [Modality, part 3: The WM_QUIT message](https://devblogs.microsoft.com/oldnewthing/20050222-00?p=36393)
* Raymond Chen — [Modality, part 4: The importance of setting the correct owner for modal UI](https://devblogs.microsoft.com/oldnewthing/20050223-00/?p=36383)
* Raymond Chen — [Modality, part 5: Setting the correct owner for modal UI](https://devblogs.microsoft.com/oldnewthing/20050224-00/?p=36373)
* Raymond Chen — [Modality, part 9: practical exam](https://devblogs.microsoft.com/oldnewthing/20110121-00/?p=11703)
* MS KB Archive — [Q89738 INFO: Handling WM_QUIT While Not in Primary GetMessage() Loop](https://jeffpar.github.io/kbarchive/kb/089/Q89738/)
* mirabulus.com — [How to handle WM_QUIT in nested message loop](https://www.mirabulus.com/it/blog/2019/10/18/how-to-handle-wmquit-in-nested-message-loop)
* GitHub — [Microsoft/Windows-classic-samples](https://github.com/microsoft/Windows-classic-samples)
* GitHub — [dotnet/wpf #6775: per-monitor-DPI v2 MessageBox blurred](https://github.com/dotnet/wpf/issues/6775)
* GitHub — [MicrosoftDocs/sdk-api MessageBoxW source](https://github.com/MicrosoftDocs/sdk-api/blob/docs/sdk-api-src/content/winuser/nf-winuser-messageboxw.md)
* GitHub — [microsoft/windows-rs](https://github.com/microsoft/windows-rs)
* windows-docs-rs — [MessageBoxW](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/WindowsAndMessaging/fn.MessageBoxW.html)
* Wesley Wiser — [Hello World MesssageBox example in Rust](https://wesleywiser.github.io/post/rust-windows-messagebox-hello-world/)
* exchangetuts — [Win32 MessageBox doesn't appear](https://exchangetuts.com/index.php/win32-messagebox-doesnt-appear-1640828823912325)
* cplusplus.com — [Window unfocus when MessageBox must be shown](https://cplusplus.com/forum/windows/59077/)
* Flexera Community — [Bring MessageBox to Front (InstallShield)](https://community.flexera.com/t5/InstallShield-Forum/Bring-MessageBox-to-Front/m-p/92903)
* GameDev.net — [WndProc wont show messagebox](https://gamedev.net/forums/topic/617349-wndproc-wont-show-messagebox/4896392)
* GameDev.net — [CreateWindow() has Error 1400 invalid window handle](https://gamedev.net/forums/topic/586926-createwindow-has-error-1400-invalid-window-handle/4729766/)
* cprogramming.com board — [Modeless MessageBox() Internals](https://cboard.cprogramming.com/windows-programming/170912-modeless-messagebox-internals.html)
* Wikipedia — [Message loop in Microsoft Windows](https://en.wikipedia.org/wiki/Message_loop_in_Microsoft_Windows)
