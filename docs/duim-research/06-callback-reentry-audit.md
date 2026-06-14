# Callback re-entry audit — why `MessageBoxW(hwnd, …)` fails from inside a WNDPROC

Sprint 41e's `Help > About` handler calls `MessageBoxW(hwnd, "…", "…", $MB-OK)` from inside the Dylan WNDPROC closure. The dialog never displays; an earlier variant crashed when the parent HWND was passed. Sprint 39c's `aot_messagebox_w_ignored` test (`tests/nod-tests/tests/aot_dylan.rs:337`) calls `MessageBoxW($NULL, …)` directly from `main()` and works.

This document audits the internal NewOpenDylan code paths exercised on the failing call. A sibling document covers Win32 documentation / community patterns separately.

---

## 1. Sprint 32 callback trampoline pool — re-entrancy analysis

`src/nod-runtime/src/callbacks.rs:345-377` is the WNDPROC dispatch path. The interesting structural facts:

- **The registry mutex is acquired ONCE per dispatch, copies the closure Word out, then drops the guard before invoking the closure** (`callbacks.rs:352-362`). The body of the closure runs with no callback-registry lock held. So a nested WNDPROC dispatch on the same thread can re-acquire the mutex without deadlocking.
- The slot trampolines themselves (`wndproc_slot_0` … `wndproc_slot_31` at `callbacks.rs:244-275`) hold no per-slot state across the call — every call is a fresh frame.
- The thread-local `WNDPROC_ROOTS_INSTALLED` guard (`callbacks.rs:57-60`, checked at `:152`) is set after first dispatch on a thread, so the nested dispatch's `install_gc_roots_for_this_thread` call is an O(1) noop. The slot pointers stay registered as GC roots throughout (they live in a `Box<[UnsafeCell<Word>; 32]>` whose addresses never move — `callbacks.rs:92`, `:163-168`).
- The closure Word stored in the slot is the same instance across the outer and nested invocations. The GC root keeps it from being collected during any nested allocation that fires GC.

**Conclusion: the trampoline pool itself is structurally safe for re-entry on the same thread.** No counters, no per-call state, no held lock spans the user closure body, and the registered closure stays GC-reachable through the slot's root registration.

The same pool is exercised non-trivially today: every `WM_PAINT`, `WM_VSCROLL`, `WM_HSCROLL`, `WM_MOUSEWHEEL`, etc. in `tests/nod-tests/fixtures/nod-ide.dylan:155-414` goes through `wndproc_dispatch`, and from inside the closure calls `InvalidateRect` (`nod-ide.dylan:59-63, :224, :257, :281, …`) which goes through the same winffi marshaling path as `MessageBoxW` would. That works. So the failure is not generic "callback calls Win32".

---

## 2. Sprint 30 string marshaling — TempBuf lifetime

`src/nod-runtime/src/winffi.rs:716-794` defines `TempBuf` and the marshalers.

The lifetime model is **scoped to a single trampoline frame**:

- Every `nod_winffi_call_N` trampoline (e.g. `winffi.rs:1209-1233` for arity 4) declares `let mut temps: Vec<TempBuf> = Vec::new();` on its own Rust stack.
- For each `<c-wide-string>` / `<c-string>` arg, `marshal_wide_string` / `marshal_narrow_string` (`winffi.rs:765-794`) allocates a Rust `Vec<u8>` or `Vec<u16>` (with null terminator), grabs its `as_ptr()` *while still in scope*, then `temps.push(...)` to move ownership into `temps`.
- After the `extern "system" fn(…) -> u64` call returns, `box_return` runs, then `drop(temps)` (`winffi.rs:1231`) explicitly frees every buffer.

The buffer storage lives on the standard Rust allocator (libc malloc on Windows), **not** on the Dylan moveable heap. So a Dylan GC cycle cannot relocate the bytes. The pointer baked into the Win64 register on the way into MessageBoxW remains valid until `drop(temps)`.

Critical question: **is the TempBuf still alive while MessageBoxW pumps its nested modal loop?** Yes — `nod_winffi_call_4`'s Rust stack frame is suspended at the `f(c0, c1, c2, c3)` call site (`winffi.rs:1226-1229`). Until MessageBoxW returns, `temps` is alive. The nested loop's WM_PAINT dispatches re-enter `nod_winffi_call_N` for *other* calls (InvalidateRect, EndPaint, etc.) but each of those has its **own** stack-local `temps` — they don't interfere with the outer call's TempBufs.

Failure mode that's *not* possible: TempBuf double-free, TempBuf use-after-free, TempBuf overwritten by a nested call. None of these can happen because each invocation gets its own Vec.

Failure mode that **is** possible but unlikely here: the Sprint 30 ASCII-fast-path for narrow strings (`winffi.rs:730-735`) doesn't do real CP_ACP conversion. But the About handler uses `<c-wide-string>` via the materialized `MessageBoxW` (W variant), which goes through `marshal_wide_string` and `str::encode_utf16()`. Pure ASCII text encodes cleanly to UTF-16; no marshaling bug there.

---

## 3. The WM_COMMAND → MessageBoxW call chain

Walked end-to-end:

1. `nod_run_message_loop` (`src/nod-runtime/src/com_shim.rs:1862-1889`) sits in the main thread blocking on `GetMessageW`.
2. User clicks `Help > About`; the OS posts WM_COMMAND with wParam = 200; `DispatchMessageW` calls the registered WNDPROC.
3. The WNDPROC is the trampoline slot the OS got from `RegisterClassExW` (`com_shim.rs:1539-1580`), which got its address from `as-wndproc-callback` (`stdlib.dylan:228-230`) → `nod_register_wndproc` (`callbacks.rs:514-536`) → `WNDPROC_SLOTS[slot_id]` (`callbacks.rs:316-325`).
4. The trampoline body forwards to `wndproc_dispatch` (`callbacks.rs:345-377`). It marshals four `u64`s to four fixnum Words and calls `nod_funcall4(closure, hwnd_word, msg_word, wparam_word, lparam_word)`.
5. The closure body runs the `if (msg = 273)` branch (`nod-ide.dylan:363-408`), evaluates `cmd-id = 200`, and reaches `MessageBoxW(hwnd, "NewOpenDylan IDE - Sprint 41e", "About", $MB-OK)` (`nod-ide.dylan:400-403`).
6. Bare-name materialization (Sprint 31) resolved `MessageBoxW` against the embedded `nod-winapi` index, producing an `ApiStubEntry` with `signature.arg_count = 4` and `arg_kinds = [Handle, WideString, WideString, UInt32]`. The JIT lowered the call to `nod_winffi_call_4(entry_ptr, a0, a1, a2, a3)`.
7. `nod_winffi_call_4` (`winffi.rs:1210-1233`) does:
   - `trampoline_prelude` loads `fn_ptr` from the entry (`winffi.rs:1072-1091`) — a simple Acquire load on the AtomicPtr.
   - `unbox_arg` for arg 0 (`Handle`): `winffi.rs:878-910`. The Word is a fixnum (the hwnd that arrived in step 4), so `w.as_fixnum() → Some(hwnd_i64)`, returns `hwnd as u64`.
   - `unbox_arg` for args 1 & 2 (`WideString`): allocates a UTF-16 `Vec<u16>`, pushes to `temps`, returns the pointer.
   - `unbox_arg` for arg 3 (`UInt32`): unwraps the fixnum payload of `$MB-OK`.
   - Transmute `fn_ptr` to `extern "system" fn(u64, u64, u64, u64) -> u64`, call it.
   - **Suspend on user32.dll's MessageBoxW frame.**

While suspended, state held by **this** trampoline frame:
- `temps`: alive on the Rust stack — both wide-string buffers are reachable; their pointers are still the ones MessageBoxW reads.
- The Dylan call stack: the JIT frame that called `nod_winffi_call_4` is alive (no exception unwound it).
- GC roots: the Sprint 32 slot registry's 32 cells are still rooted on this thread. The WNDPROC closure's environment cell pointers (Sprint 24 cell-promotion for `swap`, `bitmap`, `source-text`, `scroll-x-px`, etc., per `nod-ide.dylan:136-152`) are reachable through the captured environment vector pinned through the closure Word in the slot.

Then MessageBoxW's nested modal loop dispatches messages. WM_PAINT to OUR window re-enters `wndproc_dispatch` (re-entrancy analysed in section 1 — safe). The paint code allocates a fresh `<dwrite-text-layout>` wrapper (`nod-ide.dylan:165-166`) which triggers `alloc_movable_raw` (`heap.rs:492-533`) which **may run `collect_minor` if the young region is full** (`heap.rs:502-503`).

Each round-trip through `wndproc_dispatch` from the nested loop:
- locks/unlocks the registry mutex briefly (`callbacks.rs:352-362`) — safe;
- copies the closure Word — safe;
- marshals HWND/msg/wparam/lparam to fixnums (`callbacks.rs:364-367`) using `Word::fixnum_unchecked` (`word.rs:76-82`) which truncates the value to 63 bits via `<< 1`;
- calls the same closure recursively.

---

## 4. Why Sprint 39c works but Sprint 41e doesn't

Sprint 39c (`aot_dylan.rs:337-346`):
```dylan
format-out("%d\n",
  MessageBoxW($NULL, "Sprint 39c AOT MessageBoxW test", "NewOpenDylan", $MB-OK));
```

Path comparison:

| Step | Sprint 39c (works) | Sprint 41e (fails) |
|---|---|---|
| Bare-name materialization of `MessageBoxW` | Yes — same hook (`winffi.rs:winffi_record_materialized` accounted via `STAT_MATERIALIZED`) | Yes — same hook |
| `ApiStubEntry` signature | `[Handle, WideString, WideString, UInt32]` | identical |
| Trampoline | `nod_winffi_call_4` | `nod_winffi_call_4` |
| HWND arg value | `$NULL` = fixnum 0 → `0` | fixnum-encoded real HWND from `wndproc_dispatch` |
| Lives inside a WNDPROC? | No — called from `main()` | **Yes — called from the Dylan WNDPROC closure invoked by `wndproc_dispatch`** |
| TempBuf model | One outer call, no concurrent calls | Outer call + nested-loop calls from re-entered WNDPROC |
| Caller thread | Main thread (first to touch runtime) | Main thread (same — `nod_run_message_loop` is the message-loop primitive) |

The two ENVIRONMENTAL differences are (a) the HWND passed and (b) being inside a WNDPROC. Everything else in the marshaling/dispatch chain is byte-identical.

**(a) HWND round-trip.** The HWND first enters Rust at `wndproc_dispatch` as a `u64` (`callbacks.rs:345`). It is encoded as `Word::fixnum_unchecked(hwnd as i64)` (`callbacks.rs:364`) — a left-shift by one (`word.rs:76-82`). Dylan passes it through to MessageBoxW; `unbox_arg`'s `Handle` arm (`winffi.rs:899-900`) does `w.as_fixnum().map(|n| n as u64)` — an arithmetic right-shift by one (`word.rs:111-118`). For any HWND whose original value fits in `[FIXNUM_MIN, FIXNUM_MAX] = [-2^62, 2^62 - 1]` (`word.rs:33-34`), the round-trip is lossless. Modern x64 HWNDs are kernel-handle-table indices small enough to fit (typically under 2^32). **HWND truncation is not the root cause.** Sprint 41d's already-working code does the same round-trip on every `InvalidateRect(hwnd, 0, 0)` call without incident (`nod-ide.dylan:224` etc.).

**(b) Inside a WNDPROC.** The unique structural difference is that the call to `MessageBoxW` itself pumps a NESTED message loop while the outer trampoline frame is suspended. The OS, while inside that nested loop, will dispatch WM_PAINT, WM_NCPAINT, WM_ACTIVATE, WM_KILLFOCUS, WM_SETFOCUS, etc., to our own window. Every one re-enters our WNDPROC trampoline through the slot table and runs the same closure recursively.

---

## 5. Possible re-entrancy hazards — per-mechanism verdict

| Mechanism | Source | Re-entry safe? | Why / why not |
|---|---|---|---|
| Sprint 11c thread-local GC roots | `heap.rs:355-356` | Yes for re-entry on the same thread (the same root stack is grown by inner calls then shrunk LIFO). Concern: `RefCell` borrow held across allocation — only `for_each_root` holds the borrow (`heap.rs:432-438`), and the collector uses the `snapshot_roots` copy (`heap.rs:423-425`) precisely to avoid the cross-allocation borrow. So `register_root` / `unregister_root` from a nested call can mutate the stack while the outer frame is suspended without panicking the RefCell. | |
| Sprint 28 stub-table call counters | `winffi.rs:404-408` | Yes — they're `AtomicUsize` with `Relaxed`. No critical section. | |
| Sprint 32 callback pool slot allocation | `callbacks.rs:475` | Yes for dispatch. The registration mutex is only acquired during `register_callback` and during the brief Word-copy at dispatch start. No lock spans the user closure. | |
| `ApiStubEntry::fn_ptr` AtomicPtr | `winffi.rs:1076` | Yes — atomic load. | |
| TempBuf allocator state | Section 2 above | Yes — per-frame `Vec<TempBuf>`. | |
| Cell-promotion state of captured WNDPROC variables (`source-text`, `swap`, `bitmap`, `scroll-x-px`, …) | `nod-ide.dylan:136-152`; cell mechanics in `src/nod-runtime/src/closures.rs:5-32` | **Suspect.** The cells are `<cell>` heap objects pointed at from a `<simple-object-vector>` inside the environment. A nested WM_PAINT runs the SAME closure with the SAME environment vector. If WM_PAINT reads, e.g., `source-text` while the outer About handler holds no exclusive access, that's fine (cells are scalar reads/writes, no critical section). But if a nested WM_PAINT triggers a GC that moves the `<cell>` allocations, the environment vector slots are updated; the outer suspended Dylan frame, when it eventually resumes after MessageBoxW returns, must re-read through the cell — which the JIT emits as a `%cell-get` per access, so it should always reload. **However:** the outer Dylan frame already loaded SOME captured locals into JIT registers BEFORE entering MessageBoxW (everything used to build the call site — `hwnd`, the string Words, `$MB-OK`). Those are pinned by the JIT's spill/reload conventions which use root registration. Sprint 11c's spill/reload doc (`heap.rs:613`) is the production path. | |
| `with_literal_pool` Mutex (`lib.rs:265, :292`) | Process-global `LazyLock<Mutex<…>>` — every call to `intern_string_literal`, `literal_pool_immediates`, `class_metadata_for`, `mark_card_for`, `collect_minor`, `collect_full` re-acquires it. | **High suspicion.** `literal_pool_immediates` is called from `wndproc_dispatch`'s callees (e.g. `rebox_lresult` at `callbacks.rs:416`), inside `unbox_arg`'s `Bool32` arm (`winffi.rs:865`), inside `box_return`'s `Bool32` arm (`winffi.rs:953`). Each call locks + unlocks. If MessageBoxW is suspended mid-nested-loop and OUR window's nested WM_PAINT calls into `literal_pool_immediates`, the outer trampoline frame is fine (it's not holding the lock during the C call). But: `nod_signal` (`callbacks.rs:533`), `make_c_ffi_error`, GC trigger via `alloc_movable_raw` all acquire `with_literal_pool`. The latter holds it across the entire `collect_minor` cycle (`lib.rs:973-974`). The collector iterates the thread-local root snapshot (`heap.rs:423-425`); the snapshot is taken before the lock is meaningful to the iteration, so a re-entrant `register_root` on the SAME thread cannot run concurrently with that thread's own collector. The literal-pool Mutex is therefore not a deadlock risk on a single mutator. |
| `Mutex<HeapInner>` for heap state (`heap.rs:326`) | Locked inside `alloc_movable_raw` and `collect_*`. | Same single-thread story — no deadlock because a single thread can't deadlock with itself on a `std::sync::Mutex` only if it never re-acquires; in practice every allocation lock/unlock pair is atomic in the sense that no Dylan-callable code runs while the lock is held. | |
| Handler stack for conditions | `conditions.rs:440-441` thread-local `RefCell<Vec<HandlerFrame>>` | Same RefCell discipline as the root stack: borrow is taken briefly inside `nod_push_handler` / pop / snapshot; never held across user code. Safe. | |

The notable hazard not in the table above: **`std::sync::Mutex` on Windows is NOT a recursive mutex.** A second `lock()` from the same thread on the same Mutex while still holding the first lock deadlocks. We scanned every lock site reached on the MessageBoxW call path and the nested-WM_PAINT call path; **no held lock spans a user-callable code section**, so this hazard does not actually fire. But the audit confirms the design depends on the discipline.

---

## Most likely root cause from the code-path audit

The internal code-path audit reveals **no bug in callbacks.rs, winffi.rs, or the GC/root machinery that would cause MessageBoxW to not display**. Every state-holding mechanism (trampoline pool, TempBufs, root stack, atomics, mutexes, AtomicPtr in the stub-table entries) is structurally re-entrant-safe for a single-thread nested-modal-loop scenario.

This is significant: it means the failure is **not** a corruption of internal NewOpenDylan state. The MessageBoxW call really does reach `user32!MessageBoxW` with the correct arguments. So:

**Most likely root cause:** This is a **Win32-side window-message dispatch problem**, not a NewOpenDylan-runtime problem. The most probable specific cause is that the NewOpenDylan-side WNDPROC slot trampoline does NOT call `IsDialogMessage` or correctly handle the modal-dialog message-pumping protocol — and worse, when the OS dispatches messages destined for MessageBoxW's modal dialog *back through our outer message loop's `DispatchMessageW`*, those messages don't end up at the dialog because MessageBoxW's nested loop runs ITS OWN `GetMessage` loop that we don't participate in. The dialog window is created, its messages get pumped (so MessageBoxW does eventually return IDOK), but **the dialog gets created with a parent window state that prevents painting** — most likely because either (a) `nod_run_message_loop`'s outer `GetMessageW` (`com_shim.rs:1877`) was filtering by HWND in a way that swallows the dialog's messages (it doesn't — it passes `None` for HWND filter, so this is fine) or (b) MessageBoxW is rejecting the call early because the parent HWND owner-chain has a hidden modality / disabled-input state inherited from somewhere.

The most likely *specific* internal contributor to (b) is this: **NO `WS_VISIBLE` style check is happening**, and the parent HWND passed as the owner is the still-being-painted (or briefly-Z-ordered-wrong) main window. But even that doesn't fit cleanly — Win32 documentation says MessageBoxW will display regardless of parent visibility.

The internal-only diagnosis that fits cleanly is: **everything works at the runtime level, but MessageBoxW silently returns 0 (= failure, NOT an IDOK/IDCANCEL value) because of a Win32-side error in the call. Since the closure ignores the return value (`nod-ide.dylan:400-404` does `MessageBoxW(...); 0`), the silent failure is invisible — the closure returns 0 to the trampoline and the trampoline returns to DefWindowProc, indistinguishable from a successful "user clicked OK" path.**

The earlier crash variant — "earlier crashed when passed the parent HWND" — was almost certainly a SEPARATE bug: the user had an explicit `define c-function MessageBoxW(...)` that mis-marshaled (the in-source comment at `nod-ide.dylan:109-114` confirms this was the case). That declaration was removed in favour of bare-name materialization. The current code does NOT crash; it silently no-displays. That distinction matters for the diagnosis.

---

## Three specific code locations to investigate further

1. **`tests/nod-tests/fixtures/nod-ide.dylan:400-404`** — capture the MessageBoxW return value and `format-out` it. If the return is 0, `GetLastError()` will reveal the real Win32 error code (MessageBoxW does set last-error on failure: ERROR_NOT_ENOUGH_QUOTA is one documented cause, but more likely is something like ERROR_INVALID_WINDOW_HANDLE if the HWND round-trip *did* corrupt the value in some edge case we missed).

2. **`src/nod-runtime/src/callbacks.rs:364-367`** — log the raw `hwnd` value coming into `wndproc_dispatch` and the value going back out as `unbox_arg` reads it through fixnum decoding. Compare against the HWND that `RegisterClassExW`+`CreateWindowExW` returned (which the Dylan code sees as a fixnum in `nod-ide.dylan:437`). If they differ — even by sign-extension or by the low bit — that's the bug.

3. **`src/nod-runtime/src/winffi.rs:1209-1233`** — temporarily add a debug log after `trampoline_prelude` printing `(fn_ptr, sig.arg_count, sig.arg_kinds)` for every MessageBoxW call. Compare the Sprint 39c (works) and Sprint 41e (fails) traces. If the stub-table entry is somehow getting a DIFFERENT signature in the AOT vs JIT path, or if the resolved `fn_ptr` is different, we'd see it.

---

## One small instrumented test that would confirm the diagnosis

Add this Dylan-side test to nod-ide.dylan's About handler (or a smaller standalone EXE):

```dylan
elseif (cmd-id = 200)    // Help → About
  let result = MessageBoxW(hwnd, "test", "test", 0);
  let err = GetLastError();
  // Print to stderr so we see it even if the window doesn't repaint.
  format-out("[about] MessageBoxW returned %d, GetLastError = %d, hwnd = %d\n",
             result, err, hwnd);
  0
```

with `GetLastError` declared as:
```dylan
define c-function GetLastError () => (code :: <c-dword>);
  library: "kernel32.dll";
end;
```

**Three possible outcomes and what each tells us:**

1. **Output: `MessageBoxW returned 0, GetLastError = <nonzero>`** — confirms the Win32-side failure diagnosis. The error code identifies the exact Win32 problem (likely `ERROR_INVALID_WINDOW_HANDLE = 1400` if HWND is bad, `ERROR_INVALID_PARAMETER = 87` if the strings are bad, `ERROR_ACCESS_DENIED = 5` if there's a UIPI / WS_DISABLED issue). The internal NewOpenDylan code is correct; the fix is at the Win32 protocol level.

2. **Output: `MessageBoxW returned 1, GetLastError = 0, hwnd = <huge negative number>`** — MessageBoxW thinks it succeeded but the dialog never showed. This points at a Win32 modality / window-station issue, almost certainly NOT a NewOpenDylan-runtime bug.

3. **Output: anything that doesn't match the expected hwnd value passed in step 4 of section 3** — confirms HWND corruption in the Sprint 32 dispatch → Sprint 30 marshaling round-trip. That would localise the bug to `callbacks.rs:364` or `winffi.rs:899-910`.

The instrumented test is small (5 lines of Dylan + 3 lines of c-function declaration), zero-risk (only adds logging), and gives a definitive answer with one user click on Help > About.
