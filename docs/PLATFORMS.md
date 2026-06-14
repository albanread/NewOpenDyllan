# Platform strategy — depth, not breadth

> **NewOpenDylan is platform-first by design.** Each platform variant
> integrates deeply with its host OS rather than abstracting over a
> lowest-common-denominator surface. The Windows-first variant is
> shipping now; a Mac-first parallel variant is planned. Source-level
> reuse between them is high; runtime substrate is rebuilt per platform.

## Philosophy

There are two stances a compiler/runtime project can take on platform:

1. **Cross-platform**: one binary runs everywhere. Abstract over OS
   differences via a portable runtime layer (libuv, Boost.Asio,
   POSIX-ish shims on Windows, …). Cost: every platform-specific
   feature you'd want to integrate (SEH, ETW, Mach exception ports,
   Instruments, Metal, DirectX, COM, AppKit) is either unavailable
   or wrapped behind a leaky abstraction. Benefit: one release
   artefact.

2. **Platform-first × N**: separate variants per platform, each
   integrating idiomatically with its host. Cost: N runtime
   substrates, N release artefacts, N FFI projections. Benefit:
   each variant can be *excellent* on its target, using SEH on
   Windows, Mach exception ports on macOS, ETW for tracing, native
   GUI frameworks, native crash dumps, native debugger integration.

NewOpenDylan picks the second. **Windows-first is shipping; Mac-first
is the next planned variant.** Linux is not currently on the roadmap.
This is structurally the same choice NewFactor made — "we want
Factor's VM, not Factor the language" — commit to one substrate,
do it well, then do the next one well in parallel.

## What this means concretely

**For users**: NewOpenDylan-Windows produces real Windows-idiomatic
EXEs that call SetWindowTextW directly, use DXGI swap chains, dispatch
through COM vtables, integrate with Win32 messages, register WNDPROC
callbacks, handle SEH. The eventual NewOpenDylan-macOS will do the
analogous thing with Cocoa/UIKit, Mach exception ports, Metal,
Objective-C runtime bridging. **No** PortableX abstraction lives in
between. Users get native experience on their platform.

**For us**: most of the compiler is portable. The platform-specific
work is contained, listed below, and structurally isolated so the Mac
variant can be a parallel re-implementation of just those parts.

## What's portable (source-level reuse across variants)

The bulk of NewOpenDylan ports unchanged. These compile to identical
.dylan-and-Rust source on both platforms:

- **`nod-reader`** — lexer, parser, AST. Platform-agnostic by
  construction.
- **`nod-macro`** — macro engine. Pure transformation.
- **`nod-sema`** — lowering, type estimates, env management.
- **`nod-dfm`** — IR, liveness, safepoint root computation.
- **`nod-llvm`** — codegen. LLVM is cross-platform; the target triple
  differs between variants but the IR-emission code is shared.
- **`nod-dylan/dylan-sources/stdlib.dylan`** — Dylan-side stdlib.
  Anything that doesn't cross into platform FFI ports unchanged.
- **NewGC core** (page heap, write barriers, layout) — the algorithms
  are platform-agnostic; only commit/decommit syscalls differ.
- **Sprint 37-38 JIT cache infrastructure** — bitcode replay,
  ObjectCache, sidecar metadata. Filesystem paths are the only
  platform-specific part, easily abstracted via Rust's `std::path`.
- **Sprint 45c-h safepoint scheme** — the precise per-callsite
  safepoint maps and the runtime registry are platform-agnostic; only
  the signal-driven NLX path crosses into OS specifics.

This is the majority of the codebase. **The Mac variant inherits all
of this for free.**

## What's platform-specific (rebuilt per variant)

The thin layer where Dylan code meets the OS. These crates/modules
are Windows-specific today and will have macOS-specific siblings:

| Component | Windows | macOS (planned) |
|---|---|---|
| **API projection** | `nod-winapi` (15,067 functions from windows_api.db) | `nod-macapi` (or similar — projection from Cocoa/Foundation headers + Mach syscalls) |
| **GUI / graphics shims** | `nod-runtime/src/com_shim.rs` — DXGI / D3D11 / D2D / DirectWrite | Cocoa/UIKit + Metal + Core Text |
| **Callback trampolines** | `nod-runtime/src/callbacks.rs` — Win64 WNDPROC / WNDENUMPROC ABI | Objective-C method-IMP / NSInvocation, or fat function pointers per System V AArch64 |
| **Crash dump / signal handling** | `nod-runtime/src/crash_dump.rs` — SEH-coupled minidump emission | Mach exception ports + `os_log` + crash report dirs |
| **NLX unwind** | `CleanupGuard` in `conditions.rs` — Rust panic + SEH coordination | Rust panic + Mach exception coordination (or `libunwind` directly) |
| **AOT linker** | `nod-driver build` shells out to `link.exe` with `.lib` import libs | shells out to `ld` (or `clang`) with `-framework`/`-l` flags |
| **Virtual memory** | NewGC uses `VirtualAlloc`/`VirtualProtect` (via newgc-core) | NewGC uses `mmap`/`mprotect` (newgc-core has both backends already) |
| **Process/env primitives** | `nod_read_file_to_string`, `nod_get_argv1` etc. in `com_shim.rs` | macOS equivalents via Foundation or POSIX |

That's it. About a dozen files / one mid-sized crate. Everything else
ports cleanly.

## Rules for keeping the platform layer thin

If we add platform-specific code outside the listed components, the
Mac variant gets harder. These rules prevent leakage:

### 1. Platform-specific code lives in named modules
Win32-, COM-, or SEH-specific code goes in `com_shim.rs`,
`callbacks.rs`, `crash_dump.rs`, or `nod-winapi`. Don't sprinkle
`windows::Win32::*` imports across general-purpose runtime modules.

### 2. Cross-module FFI uses C-ABI primitives
When `conditions.rs` needs to coordinate with SEH, it should expose a
platform-neutral Rust function and have the Windows-specific glue
live in a dedicated module that the rest of the runtime calls into.
The Mac variant rebuilds that module; `conditions.rs` stays portable.

### 3. The Dylan side never speaks platform vocabulary
`stdlib.dylan` shouldn't reference `WNDPROC` or `HWND` directly.
Win32-specific helpers (`as-wndproc-callback`, `<wndclassexw>` field
accessors, etc.) belong in a platform-themed source file
(`win32-callbacks.dylan` or similar — separate from `stdlib.dylan`).
The Mac variant ships its own `cocoa-callbacks.dylan`; the language
core stays neutral.

**Note on current state**: Win32-specific helpers ARE currently in
`stdlib.dylan` (Sprint 32-36 wrappers, c-struct field accessors).
That's accumulated drift. Migration mode: split them into
`win32-stdlib.dylan` (or similar) when convenient. Not urgent.

### 4. Tests in `tests/nod-tests/` should not assume Windows file paths
F:\\scratch is fine for ad-hoc test data on the user's Windows
machine, but checked-in test fixtures should use `std::env::temp_dir()`
or per-test `tempfile` directories that work on both platforms.

### 5. Build-system platform conditionals stay in `[target.*]` blocks
`nod-driver`'s linker invocation, `nod-runtime`'s `build.rs` Win SDK
discovery — these are legitimate platform-specific bits. Keep them in
`[target.'cfg(windows)']` / `[target.'cfg(target_os = "macos")']`
sections of Cargo.toml so the Mac variant's structure mirrors them
cleanly.

## What the Mac variant probably looks like, mechanically

When Mac-first work begins (post-Sprint 50-ish? unclear timing):

- Fork (or branch) `NewOpenDylan/` to a sibling project
  `NewOpenDylan-mac/` or similar. The portable crates stay byte-
  identical; the platform-specific crates get rewritten.
- `nod-winapi` → `nod-macapi`. Likely smaller than nod-winapi
  (Foundation + AppKit + Core Frameworks rather than the whole
  Win32 surface).
- `com_shim.rs` → `cocoa_shim.rs` (or equivalent). Different APIs
  but same role: bridge Dylan to platform-native graphics/windowing.
- `crash_dump.rs` rewrites against Mach exception ports.
- The `CleanupGuard`/NLX coordination gets a Mach-exception-port
  version.
- AOT linker switches `link.exe` → `clang` (or `ld`).
- newgc-core's backend selection moves to `mmap`-based (it already
  has the abstraction).

The Dylan-side stdlib, the macro engine, the lexer, the parser, the
sema, the lowering, the LLVM codegen, the JIT cache, the safepoint
machinery — **none of those change**.

## How this links to other policies

- **`docs/STDLIB_BOUNDARY.md`** — the platform-stdlib split is
  orthogonal to the Rust-vs-Dylan split. Stdlib code goes in Dylan;
  platform-specific stdlib (e.g. Win32 wrappers) goes in a separate
  Dylan file from the portable stdlib.
- **`docs/MACRO_BOUNDARY.md`** — macros are portable by construction;
  no platform concern.
- **`docs/UPSTREAM_OPENDYLAN.md`** — Open Dylan has separate
  Win32/macOS variants in its source tree (`win32-loop.dylan` etc.).
  When lifting, lift the portable piece into the core stdlib and the
  platform-specific piece into the platform-themed source file.

## File pointers (where this policy is referenced)

- `src/nod-runtime/src/com_shim.rs` — file header
- `src/nod-runtime/src/callbacks.rs` — file header
- `src/nod-runtime/src/crash_dump.rs` — file header
- `src/nod-winapi/src/lib.rs` — crate-root comment
- `src/nod-driver/src/main.rs` — comment near the linker invocation
- `src/nod-runtime/src/conditions.rs` — note about SEH coupling
- This file from any future `NewOpenDylan-mac/` README, so the Mac
  variant inherits the same discipline from day 1.
