//! **Platform-specific module — Windows-only.** See
//! `docs/PLATFORMS.md`. The trampoline pool's Win64 ABI assumptions
//! (Win32 callback signatures, x64 calling convention) are
//! Windows-specific. The macOS variant will ship a sibling module
//! against AArch64 System V or Objective-C method-IMP conventions.
//!
//! Sprint 32 — closure-to-C-function-pointer trampoline pool.
//!
//! Win32 callbacks (`WNDPROC`, `WNDENUMPROC`, …) need function pointers
//! callable through the standard Win64 C ABI. Dylan closures cannot be
//! called directly by C code because their calling convention takes
//! tagged `Word` args, not C-typed args.
//!
//! This module provides a fixed pool of pre-compiled trampolines per
//! signature. [`register_callback`] finds a free slot, stores the
//! closure Word, and returns the slot's function-pointer address that
//! the user passes to Win32.
//!
//! ## Architecture
//!
//! - **Pre-allocated trampoline pool per signature class.** Each
//!   Win32 callback signature has its own pool of slots. Each slot is
//!   a unique fixed function-pointer the OS can call. The slot's
//!   address is what we return as the "C callback pointer."
//! - **32 slots per signature** (Sprint 32 cap; tunable later).
//!   Allocation is process-global.
//! - **Sprint 32 does not free callbacks** — register-only,
//!   leak-by-design. Reclamation is a later sprint.
//! - **Slot trampoline shape**: a macro generates `wndproc_slot_0` …
//!   `wndproc_slot_31` (one `extern "system" fn` per slot). Each baked
//!   function knows its slot ID at compile time and calls a
//!   per-signature dispatcher with `(slot_id, args…)`.
//! - **Registry per signature**: an array of `Word` cells in stable
//!   memory (`Box<[UnsafeCell<Word>; 32]>` reached through a
//!   `OnceLock`). Each cell's address is registered as a GC root once
//!   on first use of the registry so a registered closure can't be
//!   reclaimed while the callback is alive.
//!
//! ## Sprint 32 scope
//!
//! Two signatures only:
//!
//! - `Wndproc`: `extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT`
//! - `Wndenumproc`: `extern "system" fn(HWND, LPARAM) -> BOOL`
//!
//! More signatures (TIMERPROC, THREADPROC, DLGPROC, hook procs) are
//! later-sprint territory.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::word::Word;

/// Sprint 11d — runtime gate for the "tenure the closure graph at
/// register_callback time" workaround. See `register_callback`'s doc
/// for the rationale.
///
/// Default: `false`. The AOT wrapper (`aot::nod_aot_main_wrapper`)
/// sets it to `true` at startup so production EXEs get the workaround.
/// JIT-eval test paths leave it `false`, which keeps `collect_full`
/// out of `register_callback` and avoids disturbing un-rooted JIT
/// intermediate state held by the test harness across the call.
///
/// Set explicitly via [`set_callback_tenure_mode`].
static CALLBACK_TENURE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn the Sprint 11d "tenure on register_callback" workaround on or
/// off at runtime. Called by `nod_aot_main_wrapper` at AOT process
/// startup; JIT-eval setups leave it off.
pub fn set_callback_tenure_mode(on: bool) {
    CALLBACK_TENURE_ENABLED.store(on, Ordering::SeqCst);
}

/// True iff the Sprint 11d workaround is currently active. Inspectable
/// for diagnostics and tests.
pub fn callback_tenure_mode_enabled() -> bool {
    CALLBACK_TENURE_ENABLED.load(Ordering::SeqCst)
}

// Per-thread "have I installed GC roots for this thread's
// `ROOT_STACK`?" guards. Sprint 11c's root stack is thread-local, so
// every mutator thread that triggers GC needs to see the callback
// registry's cells in ITS root stack — otherwise the collector
// running on that thread misses them and stale Words remain in
// occupied slots.
//
// We register on first touch per (thread, signature). Idempotent
// guard so subsequent calls on the same thread are O(1).
thread_local! {
    static WNDPROC_ROOTS_INSTALLED: Cell<bool> = const { Cell::new(false) };
    static WNDENUMPROC_ROOTS_INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Fixed number of slots per signature. Tunable later; 32 covers all
/// realistic message-loop use cases in Sprint 32.
pub const POOL_SIZE: usize = 32;

/// The two callback signatures Sprint 32 supports.
#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum CallbackSignature {
    /// `extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT`.
    Wndproc = 0,
    /// `extern "system" fn(HWND, LPARAM) -> BOOL`.
    Wndenumproc = 1,
}

// ─── Storage: stable backing slabs + occupancy bitmap ─────────────────────

/// A per-signature registry. The `closures` array's element addresses
/// are stable for the process lifetime — we register each as a GC root
/// once at the first `register_callback` call so that registered
/// closures stay reachable while they're a callback.
///
/// `UnsafeCell<Word>` gives us interior mutability of each cell
/// without offering an aliasing reference. Writes to a cell happen
/// only inside `register_callback` under the registry mutex; reads
/// happen inside the dispatchers, which also take the mutex.
struct Registry {
    /// One `Word` per slot. `Word(0)` is the "unoccupied" sentinel
    /// (zero is a valid fixnum 0, but it's never registered as a
    /// callback because zero is not a `<function>` Word). The
    /// `occupied` array is the actual source of truth.
    closures: Box<[UnsafeCell<Word>; POOL_SIZE]>,
    /// Which slots are currently registered. Sprint 32: monotonic in
    /// practice (no unregistration), but a per-slot bool is still
    /// cheap and lets `_reset_callbacks_for_tests` clear state.
    occupied: [bool; POOL_SIZE],
}

// SAFETY: All access goes through `Mutex<Registry>`. The inner
// `UnsafeCell<Word>` cells never escape the mutex guard except as
// `*const Word` GC roots, which the collector reads atomically.
unsafe impl Send for Registry {}
unsafe impl Sync for Registry {}

impl Registry {
    fn new() -> Self {
        // SAFETY: `[UnsafeCell::new(Word::from_raw(0)); POOL_SIZE]`
        // doesn't work because `UnsafeCell` isn't `Copy`. Build a Vec
        // and convert.
        let v: Vec<UnsafeCell<Word>> = (0..POOL_SIZE)
            .map(|_| UnsafeCell::new(Word::from_raw(0)))
            .collect();
        let boxed: Box<[UnsafeCell<Word>; POOL_SIZE]> = v
            .into_boxed_slice()
            .try_into()
            .expect("vec of length POOL_SIZE");
        Self {
            closures: boxed,
            occupied: [false; POOL_SIZE],
        }
    }

    /// Stable pointers to every slot's `Word` cell. Used by
    /// [`install_gc_roots_for_this_thread`] to register all slots as
    /// roots on the current mutator thread.
    fn slot_pointers(&self) -> Vec<*const Word> {
        self.closures
            .iter()
            .map(|cell| cell.get() as *const Word)
            .collect()
    }
}

/// Ensure every slot in `registry` is registered as a GC root in the
/// current thread's [`ROOT_STACK`](crate::heap). Idempotent per thread
/// per signature via [`WNDPROC_ROOTS_INSTALLED`] /
/// [`WNDENUMPROC_ROOTS_INSTALLED`].
///
/// Why per-thread: Sprint 11c's `register_root` is a thread-local
/// push. The collector running on any given thread snapshots ITS root
/// stack and only rewrites Words reachable through those slots. A
/// callback whose closure Word lives in the moveable heap can move
/// during a collection on a thread that hasn't installed these roots
/// — the registry cell would then point at a forwarded address. The
/// `Word`'s tag bit is preserved across forwarding, but the payload
/// is not. We install on every thread that touches the registry.
fn install_gc_roots_for_this_thread(
    sig: CallbackSignature,
    registry: &Mutex<Registry>,
) {
    let already = match sig {
        CallbackSignature::Wndproc => WNDPROC_ROOTS_INSTALLED.with(|c| c.get()),
        CallbackSignature::Wndenumproc => WNDENUMPROC_ROOTS_INSTALLED.with(|c| c.get()),
    };
    if already {
        return;
    }
    let slots = {
        let g = registry.lock().expect("registry poisoned");
        g.slot_pointers()
    };
    for slot in slots {
        // SAFETY: the slab is in a stable heap allocation (`Box`),
        // and the `Registry` itself sits inside a `OnceLock` —
        // neither moves for the process lifetime. The cell's
        // pointer is stable.
        crate::heap::register_root(slot);
    }
    match sig {
        CallbackSignature::Wndproc => WNDPROC_ROOTS_INSTALLED.with(|c| c.set(true)),
        CallbackSignature::Wndenumproc => {
            WNDENUMPROC_ROOTS_INSTALLED.with(|c| c.set(true))
        }
    }
}

// ─── Process-global registries ────────────────────────────────────────────

static WNDPROC_REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
static WNDENUMPROC_REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn wndproc_registry() -> &'static Mutex<Registry> {
    WNDPROC_REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

fn wndenumproc_registry() -> &'static Mutex<Registry> {
    WNDENUMPROC_REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

// ─── Slot trampolines per signature ───────────────────────────────────────
//
// Each slot has its own `extern "system"` function with a unique
// address. Win32 stores this address (e.g. as the `WNDENUMPROC` arg to
// `EnumWindows`) and calls it through standard Win64 ABI. The body
// dispatches to the per-signature shared dispatcher with the baked-in
// slot ID.

macro_rules! make_wndproc_slot {
    ($name:ident, $id:expr) => {
        /// Slot trampoline for `WNDPROC`. The OS calls this through
        /// standard Win64 C ABI; the body forwards to the shared
        /// dispatcher with the baked-in slot ID.
        ///
        /// # Safety
        ///
        /// The OS supplies the four `WNDPROC` args. The dispatcher
        /// re-enters Dylan via `nod_funcall4`, which `panic_any`s on
        /// unhandled conditions — that unwinds through this slot;
        /// `extern "system"` is `C-unwind`-compatible on Windows
        /// (panics across the FFI boundary are UB in stable Rust
        /// today, but they were also UB in Sprint 28's
        /// `nod_winffi_call_N` path and have not yet been a problem
        /// in practice; Sprint 32 does not change this risk profile).
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $name(
            hwnd: u64,
            msg: u32,
            wparam: u64,
            lparam: u64,
        ) -> u64 {
            wndproc_dispatch($id, hwnd, msg, wparam, lparam)
        }
    };
}

macro_rules! make_wndenumproc_slot {
    ($name:ident, $id:expr) => {
        /// Slot trampoline for `WNDENUMPROC`. The OS calls this
        /// through standard Win64 C ABI; the body forwards to the
        /// shared dispatcher with the baked-in slot ID.
        ///
        /// # Safety
        ///
        /// The OS supplies the two `WNDENUMPROC` args. See the
        /// `make_wndproc_slot!` Safety note for unwinding caveats.
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $name(hwnd: u64, lparam: u64) -> i32 {
            wndenumproc_dispatch($id, hwnd, lparam)
        }
    };
}

// Generate 32 slots per signature.
make_wndproc_slot!(wndproc_slot_0, 0);
make_wndproc_slot!(wndproc_slot_1, 1);
make_wndproc_slot!(wndproc_slot_2, 2);
make_wndproc_slot!(wndproc_slot_3, 3);
make_wndproc_slot!(wndproc_slot_4, 4);
make_wndproc_slot!(wndproc_slot_5, 5);
make_wndproc_slot!(wndproc_slot_6, 6);
make_wndproc_slot!(wndproc_slot_7, 7);
make_wndproc_slot!(wndproc_slot_8, 8);
make_wndproc_slot!(wndproc_slot_9, 9);
make_wndproc_slot!(wndproc_slot_10, 10);
make_wndproc_slot!(wndproc_slot_11, 11);
make_wndproc_slot!(wndproc_slot_12, 12);
make_wndproc_slot!(wndproc_slot_13, 13);
make_wndproc_slot!(wndproc_slot_14, 14);
make_wndproc_slot!(wndproc_slot_15, 15);
make_wndproc_slot!(wndproc_slot_16, 16);
make_wndproc_slot!(wndproc_slot_17, 17);
make_wndproc_slot!(wndproc_slot_18, 18);
make_wndproc_slot!(wndproc_slot_19, 19);
make_wndproc_slot!(wndproc_slot_20, 20);
make_wndproc_slot!(wndproc_slot_21, 21);
make_wndproc_slot!(wndproc_slot_22, 22);
make_wndproc_slot!(wndproc_slot_23, 23);
make_wndproc_slot!(wndproc_slot_24, 24);
make_wndproc_slot!(wndproc_slot_25, 25);
make_wndproc_slot!(wndproc_slot_26, 26);
make_wndproc_slot!(wndproc_slot_27, 27);
make_wndproc_slot!(wndproc_slot_28, 28);
make_wndproc_slot!(wndproc_slot_29, 29);
make_wndproc_slot!(wndproc_slot_30, 30);
make_wndproc_slot!(wndproc_slot_31, 31);

make_wndenumproc_slot!(wndenumproc_slot_0, 0);
make_wndenumproc_slot!(wndenumproc_slot_1, 1);
make_wndenumproc_slot!(wndenumproc_slot_2, 2);
make_wndenumproc_slot!(wndenumproc_slot_3, 3);
make_wndenumproc_slot!(wndenumproc_slot_4, 4);
make_wndenumproc_slot!(wndenumproc_slot_5, 5);
make_wndenumproc_slot!(wndenumproc_slot_6, 6);
make_wndenumproc_slot!(wndenumproc_slot_7, 7);
make_wndenumproc_slot!(wndenumproc_slot_8, 8);
make_wndenumproc_slot!(wndenumproc_slot_9, 9);
make_wndenumproc_slot!(wndenumproc_slot_10, 10);
make_wndenumproc_slot!(wndenumproc_slot_11, 11);
make_wndenumproc_slot!(wndenumproc_slot_12, 12);
make_wndenumproc_slot!(wndenumproc_slot_13, 13);
make_wndenumproc_slot!(wndenumproc_slot_14, 14);
make_wndenumproc_slot!(wndenumproc_slot_15, 15);
make_wndenumproc_slot!(wndenumproc_slot_16, 16);
make_wndenumproc_slot!(wndenumproc_slot_17, 17);
make_wndenumproc_slot!(wndenumproc_slot_18, 18);
make_wndenumproc_slot!(wndenumproc_slot_19, 19);
make_wndenumproc_slot!(wndenumproc_slot_20, 20);
make_wndenumproc_slot!(wndenumproc_slot_21, 21);
make_wndenumproc_slot!(wndenumproc_slot_22, 22);
make_wndenumproc_slot!(wndenumproc_slot_23, 23);
make_wndenumproc_slot!(wndenumproc_slot_24, 24);
make_wndenumproc_slot!(wndenumproc_slot_25, 25);
make_wndenumproc_slot!(wndenumproc_slot_26, 26);
make_wndenumproc_slot!(wndenumproc_slot_27, 27);
make_wndenumproc_slot!(wndenumproc_slot_28, 28);
make_wndenumproc_slot!(wndenumproc_slot_29, 29);
make_wndenumproc_slot!(wndenumproc_slot_30, 30);
make_wndenumproc_slot!(wndenumproc_slot_31, 31);

// ─── Slot tables ──────────────────────────────────────────────────────────

/// `WNDPROC` slot trampolines indexed by slot ID. The slot ID is also
/// the index into [`WNDPROC_REGISTRY`]'s `closures` / `occupied`
/// arrays.
#[allow(clippy::type_complexity)]
static WNDPROC_SLOTS: [unsafe extern "system" fn(u64, u32, u64, u64) -> u64; POOL_SIZE] = [
    wndproc_slot_0, wndproc_slot_1, wndproc_slot_2, wndproc_slot_3,
    wndproc_slot_4, wndproc_slot_5, wndproc_slot_6, wndproc_slot_7,
    wndproc_slot_8, wndproc_slot_9, wndproc_slot_10, wndproc_slot_11,
    wndproc_slot_12, wndproc_slot_13, wndproc_slot_14, wndproc_slot_15,
    wndproc_slot_16, wndproc_slot_17, wndproc_slot_18, wndproc_slot_19,
    wndproc_slot_20, wndproc_slot_21, wndproc_slot_22, wndproc_slot_23,
    wndproc_slot_24, wndproc_slot_25, wndproc_slot_26, wndproc_slot_27,
    wndproc_slot_28, wndproc_slot_29, wndproc_slot_30, wndproc_slot_31,
];

/// `WNDENUMPROC` slot trampolines indexed by slot ID.
#[allow(clippy::type_complexity)]
static WNDENUMPROC_SLOTS: [unsafe extern "system" fn(u64, u64) -> i32; POOL_SIZE] = [
    wndenumproc_slot_0, wndenumproc_slot_1, wndenumproc_slot_2, wndenumproc_slot_3,
    wndenumproc_slot_4, wndenumproc_slot_5, wndenumproc_slot_6, wndenumproc_slot_7,
    wndenumproc_slot_8, wndenumproc_slot_9, wndenumproc_slot_10, wndenumproc_slot_11,
    wndenumproc_slot_12, wndenumproc_slot_13, wndenumproc_slot_14, wndenumproc_slot_15,
    wndenumproc_slot_16, wndenumproc_slot_17, wndenumproc_slot_18, wndenumproc_slot_19,
    wndenumproc_slot_20, wndenumproc_slot_21, wndenumproc_slot_22, wndenumproc_slot_23,
    wndenumproc_slot_24, wndenumproc_slot_25, wndenumproc_slot_26, wndenumproc_slot_27,
    wndenumproc_slot_28, wndenumproc_slot_29, wndenumproc_slot_30, wndenumproc_slot_31,
];

// ─── Dispatchers ──────────────────────────────────────────────────────────

/// Shared dispatcher for every `WNDPROC` slot. Reads the registered
/// closure for `slot_id`, marshals C-style args to Dylan `Word`s,
/// invokes the closure via `nod_funcall4`, marshals the return.
fn wndproc_dispatch(slot_id: usize, hwnd: u64, msg: u32, wparam: u64, lparam: u64) -> u64 {
    // The OS may invoke this on a thread different from the mutator
    // that registered the closure. Make sure THIS thread has the
    // registry's cells in its root stack, otherwise any GC triggered
    // on this thread won't see the registered closure as live.
    install_gc_roots_for_this_thread(CallbackSignature::Wndproc, wndproc_registry());

    let closure = {
        let guard = wndproc_registry().lock().expect("wndproc registry poisoned");
        debug_assert!(
            guard.occupied[slot_id],
            "wndproc slot {slot_id} dispatched but not registered"
        );
        // SAFETY: held the mutex, no other writer; the cell at
        // `slot_id` is a stable `UnsafeCell<Word>` in the heap-pinned
        // slab. We Copy out the Word.
        unsafe { *guard.closures[slot_id].get() }
    };

    let a0 = Word::fixnum_unchecked(hwnd as i64);
    let a1 = Word::fixnum_unchecked(msg as i64);
    let a2 = Word::fixnum_unchecked(wparam as i64);
    let a3 = Word::fixnum_unchecked(lparam as i64);

    // SAFETY: `closure` is a registered Dylan `<function>` Word
    // (verified at register time). `nod_funcall4` takes the function +
    // four fixnum args; arity mismatch produces a Dylan signal.
    let result = unsafe {
        crate::functions::nod_funcall4(closure.raw(), a0.raw(), a1.raw(), a2.raw(), a3.raw())
    };
    let w = Word::from_raw(result);
    rebox_lresult(w)
}

/// Shared dispatcher for every `WNDENUMPROC` slot. The return is a
/// `BOOL` (`i32`): non-zero means "continue enumeration", zero means
/// "stop". `#t` / `#f` and fixnums all map cleanly.
fn wndenumproc_dispatch(slot_id: usize, hwnd: u64, lparam: u64) -> i32 {
    install_gc_roots_for_this_thread(
        CallbackSignature::Wndenumproc,
        wndenumproc_registry(),
    );

    let closure = {
        let guard = wndenumproc_registry()
            .lock()
            .expect("wndenumproc registry poisoned");
        debug_assert!(
            guard.occupied[slot_id],
            "wndenumproc slot {slot_id} dispatched but not registered"
        );
        // SAFETY: see `wndproc_dispatch`.
        unsafe { *guard.closures[slot_id].get() }
    };

    let a0 = Word::fixnum_unchecked(hwnd as i64);
    let a1 = Word::fixnum_unchecked(lparam as i64);

    // SAFETY: see `wndproc_dispatch`.
    let result =
        unsafe { crate::functions::nod_funcall2(closure.raw(), a0.raw(), a1.raw()) };
    let w = Word::from_raw(result);
    rebox_bool32(w)
}

/// Rebox a Dylan-return Word as an `LRESULT` (u64). `<boolean>`
/// singletons map to 1 / 0; a fixnum passes through unmodified.
/// Everything else degrades to 0 (the "default WndProc didn't
/// handle it" sentinel) — Sprint 32 doesn't add structured
/// callback-return marshaling beyond fixnums and booleans.
fn rebox_lresult(w: Word) -> u64 {
    let imm = crate::literal_pool_immediates();
    if w == imm.true_ {
        1
    } else if w == imm.false_ {
        0
    } else if let Some(n) = w.as_fixnum() {
        n as u64
    } else {
        0
    }
}

/// Rebox a Dylan-return Word as a Win32 `BOOL` (i32). `#t` → 1, `#f` →
/// 0; a fixnum passes through truncated to i32. Pointer-tagged Words
/// fall back to 1 (Dylan truthiness: every non-`#f` value is true).
fn rebox_bool32(w: Word) -> i32 {
    let imm = crate::literal_pool_immediates();
    if w == imm.true_ {
        1
    } else if w == imm.false_ {
        0
    } else if let Some(n) = w.as_fixnum() {
        // Map any non-zero fixnum to 1 (BOOL is 0/1); preserve zero
        // → 0 so explicit `0` returns stop enumeration.
        if n != 0 { 1 } else { 0 }
    } else {
        // Pointer-tagged Word that isn't a boolean singleton — Dylan
        // truthiness says "true".
        1
    }
}

// ─── Registration ─────────────────────────────────────────────────────────

/// Outcome of an attempted callback registration.
#[derive(Debug)]
pub enum RegisterError {
    /// The pool for this signature is fully occupied. Sprint 32's
    /// fixed cap is [`POOL_SIZE`] slots per signature.
    PoolFull,
}

/// Register a Dylan closure as a C-callable trampoline for the given
/// signature. Returns the **slot ID** (in `0..POOL_SIZE`) on success;
/// callers translate to the trampoline's function-pointer address via
/// [`slot_address`].
///
/// Sprint 32: registrations are leak-by-design — there's no
/// unregistration path. A later sprint adds release semantics.
///
/// ## Sprint 11d workaround — eager tenure of the closure reachable graph
///
/// After the closure Word is stored in the registry, we drive a full
/// GC cycle (`collect_full`). Rationale, recorded in
/// [`docs/NEWGC_BUG_ENV_RECLAIM_ANALYSIS.md`]:
///
/// The IDE crashed under keyboard input with the closure's
/// `<environment>` reclaimed by minor GC despite being reachable
/// through `registry → closure → env-ptr → env`. The GC team built
/// four synthetic reproducers covering the production callback path
/// with real trampoline dispatch + cell mutation + heavy allocation
/// pressure; all four pass. The remaining differentiator is the
/// JIT-compiled closure body: if the Sprint 11b alloca-root brackets
/// around `env_ptr` are missing for some JIT call site, env becomes
/// unreachable after the minor cycle that fires inside the body.
///
/// `collect_full` promotes every reachable object to Tenured (see
/// `heap.rs::collect_full`'s "G0→G1 → G1→Tenured → Tenured→Tenured
/// with live root closure" cascade). After this call, the closure,
/// its `<environment>`, the cells SOV, every `<cell>`, and every
/// cell's *current* value Word are all Tenured. Subsequent minor GCs
/// never move them — so a stale env-ptr Word inside the JIT body
/// (the suspected bug) refers to a still-valid address. Mutations
/// to cell values still work via the write-barrier + dirty-card path,
/// which the trampoline reproducer
/// (`gc_callback_env::callback_closure_env_survives_cell_mutation_under_promotion`)
/// proves is correct.
///
/// **This is a workaround, not the fix.** The structural fix lives in
/// nod-sema's lowering of closure-body env_ptr argument roots. When
/// that lands and the JIT-fixture reproducer test
/// (`gc_callback_env_jit_fixture`, planned) is green without this
/// tenuring, the `collect_full` call can be removed. Keeping it as
/// belt-and-suspenders is also semantically defensible — long-lived
/// callback closures shouldn't churn through generations.
///
/// One-time cost: a single full GC per `register_callback` (typically
/// 1-2 calls per process at startup for an IDE-shaped workload).
pub fn register_callback(
    closure: Word,
    sig: CallbackSignature,
) -> Result<usize, RegisterError> {
    let registry: &'static Mutex<Registry> = match sig {
        CallbackSignature::Wndproc => wndproc_registry(),
        CallbackSignature::Wndenumproc => wndenumproc_registry(),
    };
    install_gc_roots_for_this_thread(sig, registry);

    {
        let mut guard = registry.lock().expect("callback registry poisoned");

        let free = (0..POOL_SIZE).find(|&i| !guard.occupied[i]);
        let Some(slot_id) = free else {
            return Err(RegisterError::PoolFull);
        };
        // SAFETY: under the registry mutex; cell address is stable; we
        // are the sole writer.
        unsafe {
            *guard.closures[slot_id].get() = closure;
        }
        guard.occupied[slot_id] = true;

        // Drop the registry guard BEFORE driving `collect_full` — the
        // collector visits this slot as a root and re-acquires no
        // registry mutex, but holding our registry guard across a
        // GC cycle would be a needless and lock-order-fragile thing
        // to do. Save the slot_id, release the guard, GC, return.
        let slot_id_local = slot_id;
        drop(guard);

        // Sprint 11d workaround — see fn doc above. Force the closure
        // graph to Tenured so the suspected JIT-body root-bracketing
        // bug can't reclaim its env between minor cycles.
        //
        // **Runtime-gated on `CALLBACK_TENURE_ENABLED`.** The AOT
        // wrapper (`nod_aot_main_wrapper`) flips this on at process
        // startup so production EXEs get the workaround. JIT-eval
        // paths (the `eval_expr_to_string` test harness, the
        // `nod-runtime --lib` parallel unit tests, the `ide_shell_*`
        // integration tests) leave it off. Rationale:
        //
        //   * In an AOT-built IDE, by the time `register_callback`
        //     runs, all live Words are precisely tracked: cells the
        //     WNDPROC captures are reachable through the closure
        //     itself, the closure is reachable through the registry
        //     slot we just wrote, and Sprint 11b's spill-to-runtime
        //     slots protect every live Word in JIT frames currently
        //     on the stack. `collect_full` runs cleanly.
        //
        //   * In a JIT-eval test harness, intermediate values the
        //     evaluator built while assembling the test's `let` chain
        //     may live in non-registered slots that 11b's
        //     populate-roots pass didn't see (the harness pulls Words
        //     in and out of its own scaffolding). `collect_full`
        //     evacuates those values' targets, leaves the harness
        //     pointing at recycled memory, and the test
        //     STATUS_ACCESS_VIOLATIONs the next time it dereferences.
        //
        // The gate matches the operative distinction: "are we running
        // as a Dylan EXE that owns its process" vs "are we a JIT
        // eval embedded in a Rust test harness".
        if callback_tenure_mode_enabled() {
            crate::collect_full();
        }

        Ok(slot_id_local)
    }
}

/// Return the raw address of the trampoline function for the given
/// `(signature, slot_id)` pair. Win32 calls this address through the
/// standard Win64 C ABI.
pub fn slot_address(sig: CallbackSignature, slot_id: usize) -> usize {
    match sig {
        CallbackSignature::Wndproc => WNDPROC_SLOTS[slot_id] as usize,
        CallbackSignature::Wndenumproc => WNDENUMPROC_SLOTS[slot_id] as usize,
    }
}

// ─── JIT-callable externs ─────────────────────────────────────────────────

/// JIT-callable: convert a Dylan closure Word to a `WNDPROC` function
/// pointer. Returns a Dylan Word whose payload is the trampoline
/// address (fixnum-tagged — the `<c-pointer>` ABI Sprint 28+ uses for
/// raw addresses). On pool exhaustion, signals a `<c-ffi-error>` via
/// `nod_signal` (diverges).
///
/// # Safety
///
/// `closure_raw` must be a Dylan `<function>` Word. The dispatcher
/// will invoke it via `nod_funcall4` when the OS calls the
/// trampoline; arity mismatches surface as Dylan signals at call
/// time.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_register_wndproc(closure_raw: u64) -> u64 {
    let closure = Word::from_raw(closure_raw);
    match register_callback(closure, CallbackSignature::Wndproc) {
        Ok(slot_id) => {
            let addr = WNDPROC_SLOTS[slot_id] as usize as i64;
            Word::fixnum_unchecked(addr).raw()
        }
        Err(RegisterError::PoolFull) => {
            let err = crate::winffi::make_c_ffi_error(
                "<callback-pool>",
                "register_wndproc",
                0,
                &format!(
                    "callback pool exhausted (Sprint 32 cap: {POOL_SIZE} WNDPROCs)"
                ),
            );
            // SAFETY: `nod_signal` takes a Dylan condition Word and
            // diverges via NLX. The c-ffi-error was constructed above.
            unsafe { crate::conditions::nod_signal(err.raw()) }
        }
    }
}

/// JIT-callable: convert a Dylan closure Word to a `WNDENUMPROC`
/// function pointer. Returns the trampoline address as a fixnum-tagged
/// Word. On pool exhaustion, signals a `<c-ffi-error>` (diverges).
///
/// # Safety
///
/// See [`nod_register_wndproc`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_register_wndenumproc(closure_raw: u64) -> u64 {
    let closure = Word::from_raw(closure_raw);
    match register_callback(closure, CallbackSignature::Wndenumproc) {
        Ok(slot_id) => {
            let addr = WNDENUMPROC_SLOTS[slot_id] as usize as i64;
            Word::fixnum_unchecked(addr).raw()
        }
        Err(RegisterError::PoolFull) => {
            let err = crate::winffi::make_c_ffi_error(
                "<callback-pool>",
                "register_wndenumproc",
                0,
                &format!(
                    "callback pool exhausted (Sprint 32 cap: {POOL_SIZE} WNDENUMPROCs)"
                ),
            );
            // SAFETY: see `nod_register_wndproc`.
            unsafe { crate::conditions::nod_signal(err.raw()) }
        }
    }
}

// ─── Test helpers ─────────────────────────────────────────────────────────

/// Test-only: clear every slot in both signatures' registries. Used
/// by `callback_pool_full_signals_error` so it doesn't permanently
/// fill the pool for subsequent tests.
///
/// Does NOT unregister GC roots — the cells stay in the root set but
/// hold `Word(0)` (a fixnum the collector ignores). Idempotent.
pub fn _reset_callbacks_for_tests() {
    if let Some(reg) = WNDPROC_REGISTRY.get() {
        let mut g = reg.lock().expect("wndproc registry poisoned");
        for i in 0..POOL_SIZE {
            // SAFETY: under the registry mutex; sole writer.
            unsafe {
                *g.closures[i].get() = Word::from_raw(0);
            }
            g.occupied[i] = false;
        }
    }
    if let Some(reg) = WNDENUMPROC_REGISTRY.get() {
        let mut g = reg.lock().expect("wndenumproc registry poisoned");
        for i in 0..POOL_SIZE {
            // SAFETY: see above.
            unsafe {
                *g.closures[i].get() = Word::from_raw(0);
            }
            g.occupied[i] = false;
        }
    }
}

/// Test-only: how many slots are currently registered for `sig`. Used
/// to assert pool occupancy in tests.
pub fn _occupied_count(sig: CallbackSignature) -> usize {
    let reg = match sig {
        CallbackSignature::Wndproc => wndproc_registry(),
        CallbackSignature::Wndenumproc => wndenumproc_registry(),
    };
    let g = reg.lock().expect("registry poisoned");
    g.occupied.iter().filter(|b| **b).count()
}

// ─── Unit tests ───────────────────────────────────────────────────────────
//
// Pure-Rust unit tests that exercise the trampoline plumbing without
// going through parse/sema/JIT. They use `nod_funcall2` directly on a
// Rust-registered closure Word.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::{
        FUNCTION_KIND_TOP_LEVEL, make_function, register_rust_function,
    };
    use serial_test::serial;

    /// Build a top-level Rust function and register it as a Dylan
    /// `<function>` Word. The body just increments a process-global
    /// counter and returns a fixnum.
    fn make_counting_closure(arity: usize, name: &str) -> Word {
        // Use a top-level Rust function (FUNCTION_KIND_TOP_LEVEL).
        // Sprint 32 doesn't need a real captured environment — the
        // counter lives in a `static AtomicUsize` so a top-level
        // function suffices for "did the body run?" tests.
        unsafe extern "C-unwind" fn body_arity2(_a: u64, _b: u64) -> u64 {
            crate::callbacks::tests::TEST_COUNTER
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Return a fixnum 1 — both `rebox_bool32` (continue) and
            // `rebox_lresult` (handled) treat non-zero as "yes".
            Word::fixnum_unchecked(1).raw()
        }
        unsafe extern "C-unwind" fn body_arity4(_a: u64, _b: u64, _c: u64, _d: u64) -> u64 {
            crate::callbacks::tests::TEST_COUNTER
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Word::fixnum_unchecked(42).raw()
        }
        let code: *const u8 = match arity {
            2 => body_arity2 as *const u8,
            4 => body_arity4 as *const u8,
            _ => panic!("unsupported arity for counting closure: {arity}"),
        };
        // SAFETY: code_ptr is a pinned `extern "C-unwind"` Rust
        // function with matching arity.
        unsafe {
            register_rust_function(name, arity, code);
        }
        make_function(name, arity, code, FUNCTION_KIND_TOP_LEVEL, 0)
    }

    pub(super) static TEST_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    #[serial]
    fn wndenumproc_synthetic_dispatch() {
        _reset_callbacks_for_tests();
        TEST_COUNTER.store(0, std::sync::atomic::Ordering::SeqCst);

        let closure = make_counting_closure(2, "wndenumproc-test-cb");
        let slot_id =
            register_callback(closure, CallbackSignature::Wndenumproc).expect("slot free");

        // Invoke the slot's trampoline directly, as if Win32 called it.
        let slot_fn: unsafe extern "system" fn(u64, u64) -> i32 =
            WNDENUMPROC_SLOTS[slot_id];
        // SAFETY: the slot was just registered; calling its trampoline
        // is the documented Sprint 32 surface.
        let r = unsafe { slot_fn(0x1234_5678, 0xDEAD_BEEF) };

        assert_eq!(
            r, 1,
            "WNDENUMPROC trampoline should return 1 (BOOL TRUE) for a closure returning fixnum 1"
        );
        assert_eq!(
            TEST_COUNTER.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "closure body must have run exactly once"
        );

        _reset_callbacks_for_tests();
    }

    #[test]
    #[serial]
    fn wndproc_synthetic_dispatch() {
        _reset_callbacks_for_tests();
        TEST_COUNTER.store(0, std::sync::atomic::Ordering::SeqCst);

        let closure = make_counting_closure(4, "wndproc-test-cb");
        let slot_id =
            register_callback(closure, CallbackSignature::Wndproc).expect("slot free");

        let slot_fn: unsafe extern "system" fn(u64, u32, u64, u64) -> u64 =
            WNDPROC_SLOTS[slot_id];
        // SAFETY: see `wndenumproc_synthetic_dispatch`.
        let r = unsafe { slot_fn(0x1234_5678, 0x0011, 0xAA, 0xBB) };

        assert_eq!(r, 42, "WNDPROC trampoline should pass through fixnum 42 from closure");
        assert_eq!(
            TEST_COUNTER.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "closure body must have run exactly once"
        );

        _reset_callbacks_for_tests();
    }

    #[test]
    #[serial]
    fn distinct_slots_have_distinct_addresses() {
        _reset_callbacks_for_tests();
        let cb1 = make_counting_closure(2, "addr-cb-1");
        let cb2 = make_counting_closure(2, "addr-cb-2");
        let s1 = register_callback(cb1, CallbackSignature::Wndenumproc).unwrap();
        let s2 = register_callback(cb2, CallbackSignature::Wndenumproc).unwrap();
        assert_ne!(s1, s2, "two distinct registrations should land in different slots");
        let a1 = WNDENUMPROC_SLOTS[s1] as usize;
        let a2 = WNDENUMPROC_SLOTS[s2] as usize;
        assert_ne!(a1, a2, "two distinct slots must have different trampoline addresses");
        _reset_callbacks_for_tests();
    }

    #[test]
    #[serial]
    fn pool_full_returns_err() {
        _reset_callbacks_for_tests();
        let cb = make_counting_closure(2, "pool-full-cb");
        // Fill all 32 slots.
        for _ in 0..POOL_SIZE {
            register_callback(cb, CallbackSignature::Wndenumproc)
                .expect("first 32 must succeed");
        }
        // The 33rd must fail.
        let err = register_callback(cb, CallbackSignature::Wndenumproc);
        assert!(
            matches!(err, Err(RegisterError::PoolFull)),
            "33rd registration must report PoolFull, got {err:?}"
        );
        assert_eq!(
            _occupied_count(CallbackSignature::Wndenumproc),
            POOL_SIZE,
            "pool should be saturated"
        );
        _reset_callbacks_for_tests();
        assert_eq!(
            _occupied_count(CallbackSignature::Wndenumproc),
            0,
            "reset should clear all slots"
        );
    }

    // Sprint 32: the in-module `closure_survives_gc_pressure` test
    // landed in `tests/nod-tests/tests/winffi_callbacks.rs` instead.
    // The pure-Rust mod-level home would have had to interleave with
    // the rest of `nod-runtime`'s unit-test suite (which uses
    // parallel test threads), and the closure-tied GC roots are
    // thread-local — they can't be fully validated without going
    // through the end-to-end test crate. See
    // `closure_survives_gc_pressure` in the integration suite.

    #[test]
    #[serial]
    fn rebox_bool_helpers() {
        let imm = crate::literal_pool_immediates();
        assert_eq!(rebox_bool32(imm.true_), 1);
        assert_eq!(rebox_bool32(imm.false_), 0);
        assert_eq!(rebox_bool32(Word::fixnum_unchecked(0)), 0);
        assert_eq!(rebox_bool32(Word::fixnum_unchecked(42)), 1);
        assert_eq!(rebox_bool32(Word::fixnum_unchecked(-1)), 1);

        assert_eq!(rebox_lresult(imm.true_), 1);
        assert_eq!(rebox_lresult(imm.false_), 0);
        assert_eq!(rebox_lresult(Word::fixnum_unchecked(0)), 0);
        assert_eq!(rebox_lresult(Word::fixnum_unchecked(7)), 7);
    }
}
