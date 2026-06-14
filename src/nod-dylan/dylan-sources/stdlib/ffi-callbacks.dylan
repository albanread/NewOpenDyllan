Module: dylan
Author: NewOpenDylan stdlib

// ─── Sprint 32: closure → C callback pointer ──────────────────────────────
//
// `as-wndproc-callback(cb)` and `as-wndenumproc-callback(cb)` register
// a Dylan closure as a Win32-callable function pointer for the named
// signature, returning a `<c-pointer>` value (fixnum-tagged raw
// address — the FFI ABI Sprint 28 adopted for `<c-pointer>` values).
//
// Sprint 32 ships two signatures: `WNDPROC` (window procedure, used by
// `RegisterClass(W)`) and `WNDENUMPROC` (passed to `EnumWindows`).
// Later sprints add TIMERPROC, THREADPROC, DLGPROC, hook procs, etc.
// A unified `as-c-callback(cb, signature-symbol)` form is deferred
// until `select` lowers.
//
// Registrations are leak-by-design in Sprint 32 — the pool of 32
// slots per signature is allocated once and never freed. A later
// sprint adds release semantics.

define function as-wndproc-callback (closure) => (ptr)
  %register-wndproc(closure)
end function;

define function as-wndenumproc-callback (closure) => (ptr)
  %register-wndenumproc(closure)
end function;

