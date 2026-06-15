# Platforms

NewOpenDylan is platform-first by design. Each platform variant integrates
deeply with its host OS rather than abstracting over a
lowest-common-denominator surface. The Windows-first variant is the supported
target today; a Mac-first parallel variant is planned. Source-level reuse
between variants is high; the runtime substrate is rebuilt per platform.

## Philosophy

There are two stances a compiler/runtime project can take on platform:

1. **Cross-platform**: one binary runs everywhere. Abstract over OS
   differences via a portable runtime layer (libuv, Boost.Asio, POSIX-ish
   shims on Windows). The cost is that every platform-specific feature you
   would want to integrate (SEH, ETW, Mach exception ports, Instruments,
   Metal, DirectX, COM, AppKit) is either unavailable or wrapped behind a
   leaky abstraction. The benefit is one release artefact.

2. **Platform-first x N**: separate variants per platform, each integrating
   idiomatically with its host. The cost is N runtime substrates, N release
   artefacts, N FFI projections. The benefit is that each variant can be
   *excellent* on its target, using SEH on Windows, Mach exception ports on
   macOS, ETW for tracing, native GUI frameworks, native crash dumps, and
   native debugger integration.

NewOpenDylan picks the second. Windows-first is the shipping variant; Mac-first
is the next planned variant. Linux is not currently on the roadmap. The
strategy is to commit to one substrate, do it well, then do the next one well
in parallel.

## What this means concretely

**For users**: NewOpenDylan-Windows produces real Windows-idiomatic EXEs that
call `SetWindowTextW` directly, use DXGI swap chains, dispatch through COM
vtables, integrate with Win32 messages, register WNDPROC callbacks, and handle
SEH. A future NewOpenDylan-macOS would do the analogous thing with Cocoa/UIKit,
Mach exception ports, Metal, and Objective-C runtime bridging. There is no
portability abstraction in between; users get a native experience on their
platform.

**For the implementation**: most of the compiler is portable. The
platform-specific work is contained, listed below, and structurally isolated
so the Mac variant can be a parallel re-implementation of just those parts.

## What is portable (source-level reuse across variants)

The bulk of NewOpenDylan ports unchanged. These compile to identical
Dylan-and-Rust source on both platforms:

- **`nod-reader`** — lexer, parser, AST. Platform-agnostic by construction.
- **`nod-macro`** — macro engine. Pure transformation.
- **`nod-sema`** — lowering, type estimates, environment management.
- **`nod-dfm`** — IR, liveness, safepoint root computation.
- **`nod-llvm`** — codegen. LLVM is cross-platform; the target triple differs
  between variants but the IR-emission code is shared.
- **`stdlib/*.dylan`** — the Dylan-side stdlib. Anything
  that does not cross into platform FFI ports unchanged.
- **NewGC core** (page heap, write barriers, layout) — the algorithms are
  platform-agnostic; only the commit/decommit syscalls differ.
- **JIT cache infrastructure** — bitcode replay, ObjectCache, sidecar
  metadata. Filesystem paths are the only platform-specific part, easily
  abstracted via Rust's `std::path`.
- **The safepoint scheme** — the precise per-callsite safepoint maps and the
  runtime registry are platform-agnostic; only the signal-driven non-local-exit
  path crosses into OS specifics.

This is the majority of the codebase. The Mac variant inherits all of it for
free.

## What is platform-specific (rebuilt per variant)

The thin layer where Dylan code meets the OS. These crates and modules are
Windows-specific today and would have macOS-specific siblings:

| Component | Windows | macOS (planned) |
|---|---|---|
| **API projection** | `nod-winapi` (projection of the Win32 surface from `windows_api.db`) | `nod-macapi` or similar — projection from Cocoa/Foundation headers + Mach syscalls |
| **GUI / graphics shims** | `nod-runtime/src/com_shim.rs` — DXGI / D3D11 / D2D / DirectWrite | Cocoa/UIKit + Metal + Core Text |
| **Callback trampolines** | `nod-runtime/src/callbacks.rs` — Win64 WNDPROC / WNDENUMPROC ABI | Objective-C method-IMP / NSInvocation, or fat function pointers per System V AArch64 |
| **Crash dump / signal handling** | `nod-runtime/src/crash_dump.rs` — SEH-coupled minidump emission | Mach exception ports + `os_log` + crash report dirs |
| **NLX unwind** | `CleanupGuard` in `conditions.rs` — Rust panic + SEH coordination | Rust panic + Mach exception coordination (or `libunwind` directly) |
| **AOT linker** | `nod-driver build` shells out to `link.exe` with `.lib` import libs | shells out to `ld` (or `clang`) with `-framework`/`-l` flags |
| **Virtual memory** | NewGC uses `VirtualAlloc`/`VirtualProtect` (via newgc-core) | NewGC uses `mmap`/`mprotect` (newgc-core has both backends already) |
| **Process/env primitives** | `nod_read_file_to_string`, `nod_get_argv1`, etc. in `com_shim.rs` | macOS equivalents via Foundation or POSIX |

That is roughly a dozen files / one mid-sized crate. Everything else ports
cleanly.

## Rules for keeping the platform layer thin

Platform-specific code added outside the listed components makes the Mac
variant harder. These rules prevent leakage:

### 1. Platform-specific code lives in named modules

Win32-, COM-, or SEH-specific code goes in `com_shim.rs`, `callbacks.rs`,
`crash_dump.rs`, or `nod-winapi`. Do not sprinkle `windows::Win32::*` imports
across general-purpose runtime modules.

### 2. Cross-module FFI uses C-ABI primitives

When `conditions.rs` needs to coordinate with SEH, it exposes a
platform-neutral Rust function and lets the Windows-specific glue live in a
dedicated module that the rest of the runtime calls into. The Mac variant
rebuilds that module; `conditions.rs` stays portable.

### 3. The Dylan side never speaks platform vocabulary

`stdlib.dylan` should not reference `WNDPROC` or `HWND` directly. Win32-specific
helpers (`as-wndproc-callback`, `<wndclassexw>` field accessors, etc.) belong
in a platform-themed source file (`win32-callbacks.dylan` or similar, separate
from `stdlib.dylan`). The Mac variant ships its own `cocoa-callbacks.dylan`;
the language core stays neutral.

> **Note on current state**: some Win32-specific helpers currently live in
> `stdlib.dylan` (FFI wrappers, c-struct field accessors). This is accumulated
> drift. The migration is to split them into `win32-stdlib.dylan` (or similar)
> when convenient; it is not urgent.

### 4. Tests should not assume Windows file paths

Ad-hoc scratch paths are fine on the developer's own machine, but checked-in
test fixtures should use `std::env::temp_dir()` or per-test `tempfile`
directories that work on both platforms.

### 5. Build-system platform conditionals stay in `[target.*]` blocks

`nod-driver`'s linker invocation and `nod-runtime`'s `build.rs` Windows SDK
discovery are legitimate platform-specific bits. They belong in
`[target.'cfg(windows)']` / `[target.'cfg(target_os = "macos")']` sections of
`Cargo.toml` so the Mac variant's structure mirrors them cleanly.

## The path to macOS / Linux

When Mac-first work begins, the mechanical shape is:

- Fork (or branch) the project to a sibling `NewOpenDylan-mac/` (or similar).
  The portable crates stay byte-identical; the platform-specific crates get
  rewritten.
- `nod-winapi` becomes `nod-macapi` — likely smaller than `nod-winapi`
  (Foundation + AppKit + Core Frameworks rather than the whole Win32 surface).
- `com_shim.rs` becomes `cocoa_shim.rs` (or equivalent) — different APIs, same
  role: bridge Dylan to platform-native graphics and windowing.
- `crash_dump.rs` is rewritten against Mach exception ports.
- The `CleanupGuard` / NLX coordination gets a Mach-exception-port version.
- The AOT linker switches `link.exe` for `clang` (or `ld`).
- newgc-core's backend selection moves to `mmap`-based; the abstraction is
  already present.

The Dylan-side stdlib, the macro engine, the lexer, the parser, the sema, the
lowering, the LLVM codegen, the JIT cache, and the safepoint machinery do not
change. The port task is primarily about unwinding, frame walking, and OS
integration, not about still-changing GC/compiler architecture. For that
reason, the non-Windows ports are sequenced after Windows has stable precise
roots for JIT and AOT, stable callback/reentry behavior, stable polymorphic
dispatch caches, and stable IDE shell runtime behavior under GC pressure.

## File pointers (where this policy is referenced)

- `src/nod-runtime/src/com_shim.rs` — file header
- `src/nod-runtime/src/callbacks.rs` — file header
- `src/nod-runtime/src/crash_dump.rs` — file header
- `src/nod-winapi/src/lib.rs` — crate-root comment
- `src/nod-driver/src/main.rs` — comment near the linker invocation
- `src/nod-runtime/src/conditions.rs` — note about SEH coupling

## See also

- [Upstream Open Dylan](upstream-opendylan.md) — Open Dylan has separate
  Win32/macOS variants in its source tree; when lifting, lift the portable
  piece into the core stdlib and the platform-specific piece into the
  platform-themed source file.

---

Reference: [Performance](performance.md) | [Known limitations](known-limitations.md) | [Upstream Open Dylan](upstream-opendylan.md) | [Tracing](tracing.md) | [Architecture](../architecture.md) | [Glossary](../glossary.md)
