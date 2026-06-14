//! **Platform-specific module — Windows-only.** See
//! `docs/PLATFORMS.md`. SEH coupling
//! (`SetUnhandledExceptionFilter`, structured exception records) is
//! intentional — this is one of the named modules that hosts
//! Windows-specific code. The macOS variant will ship a Mach-exception-
//! port-based equivalent covering the same role: signal-safe GC state
//! emission on unhandled exceptions.
//!
//! Signal-safe crash dump: GC state + heap metrics written to stderr
//! on panic or unhandled Windows structured exception.
//!
//! # Signal-safety strategy
//!
//! A Windows `SetUnhandledExceptionFilter` callback (and to a lesser
//! degree a Rust panic hook) must not allocate memory, must not wait
//! on locks, and must complete quickly.  The crash dump avoids these
//! hazards by:
//!
//! 1. Maintaining a **shadow copy** of GC metrics in process-global
//!    `AtomicU64` cells written (with `SeqCst` fence) *after* every
//!    GC cycle — outside all heap locks.  The crash handler reads
//!    these without acquiring any mutex.
//!
//! 2. Maintaining a **GC-phase byte** (`GC_PHASE`, `AtomicU8`) set on
//!    entry to `collect_minor`/`collect_full` and cleared on exit.
//!    If the crash fires inside a GC the handler notes the metrics may
//!    reflect a partially-completed cycle.
//!
//! 3. Writing output via `WriteFile` (Windows) or `write(2)` (Unix)
//!    directly to the stderr handle — no libc buffering, no heap
//!    allocation.
//!
//! 4. Reading current-thread safepoint depth from thread-locals.
//!    On Windows, `SetUnhandledExceptionFilter` callbacks run on the
//!    faulting thread, so thread-locals are accessible.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

// ──────────────────────────────────────────────────────────────────── //
// GC phase flag                                                        //
// ──────────────────────────────────────────────────────────────────── //

/// 0 = idle; 1 = minor GC in progress; 2 = major GC in progress.
static GC_PHASE: AtomicU8 = AtomicU8::new(0);

pub(crate) const GC_PHASE_IDLE: u8 = 0;
pub(crate) const GC_PHASE_MINOR: u8 = 1;
pub(crate) const GC_PHASE_MAJOR: u8 = 2;

/// Set the GC phase visible to the crash handler.
/// Called by `heap.rs` immediately before/after each collection.
pub(crate) fn set_gc_phase(phase: u8) {
    GC_PHASE.store(phase, Ordering::SeqCst);
}

// ──────────────────────────────────────────────────────────────────── //
// GC metrics shadow                                                    //
// ──────────────────────────────────────────────────────────────────── //

static SHADOW_MINOR_COLLECTIONS: AtomicU64 = AtomicU64::new(0);
static SHADOW_MAJOR_COLLECTIONS: AtomicU64 = AtomicU64::new(0);
static SHADOW_YOUNG_BYTES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static SHADOW_YOUNG_BYTES_LIVE: AtomicU64 = AtomicU64::new(0);
static SHADOW_OLD_BYTES_LIVE: AtomicU64 = AtomicU64::new(0);
static SHADOW_LAST_MINOR_PAUSE_NS: AtomicU64 = AtomicU64::new(0);
static SHADOW_LAST_MAJOR_PAUSE_NS: AtomicU64 = AtomicU64::new(0);
static SHADOW_TOTAL_MINOR_PAUSE_NS: AtomicU64 = AtomicU64::new(0);
static SHADOW_TOTAL_MAJOR_PAUSE_NS: AtomicU64 = AtomicU64::new(0);
static SHADOW_ROOTS_AT_LAST_MINOR: AtomicU64 = AtomicU64::new(0);
static SHADOW_ROOTS_AT_LAST_MAJOR: AtomicU64 = AtomicU64::new(0);
static SHADOW_BYTES_PROMOTED: AtomicU64 = AtomicU64::new(0);
static SHADOW_PEAK_YOUNG_BYTES_LIVE: AtomicU64 = AtomicU64::new(0);
static SHADOW_PEAK_OLD_BYTES_LIVE: AtomicU64 = AtomicU64::new(0);

/// Publish a fresh snapshot of GC counters into the signal-safe
/// shadow.  Called by both GC backends after every
/// `collect_minor` / `collect_full`, outside all heap locks.
///
/// Each store is `SeqCst` so that a crash handler executing on the
/// same or another thread sees a fully-published update.  The shadow
/// may lag behind by at most one GC cycle if a crash fires during the
/// window between the GC completing and this function being called,
/// but `GC_PHASE` will already have been cleared to `IDLE` by then.
pub(crate) fn update_gc_metrics(s: &crate::heap::HeapStatsSnapshot) {
    SHADOW_MINOR_COLLECTIONS.store(s.minor_collections, Ordering::SeqCst);
    SHADOW_MAJOR_COLLECTIONS.store(s.major_collections, Ordering::SeqCst);
    SHADOW_YOUNG_BYTES_ALLOCATED.store(s.young_bytes_allocated, Ordering::SeqCst);
    SHADOW_YOUNG_BYTES_LIVE.store(s.young_bytes_live, Ordering::SeqCst);
    SHADOW_OLD_BYTES_LIVE.store(s.old_bytes_live, Ordering::SeqCst);
    SHADOW_LAST_MINOR_PAUSE_NS.store(s.last_minor_pause_ns, Ordering::SeqCst);
    SHADOW_LAST_MAJOR_PAUSE_NS.store(s.last_major_pause_ns, Ordering::SeqCst);
    SHADOW_TOTAL_MINOR_PAUSE_NS.store(s.total_minor_pause_ns, Ordering::SeqCst);
    SHADOW_TOTAL_MAJOR_PAUSE_NS.store(s.total_major_pause_ns, Ordering::SeqCst);
    SHADOW_ROOTS_AT_LAST_MINOR.store(s.roots_at_last_minor, Ordering::SeqCst);
    SHADOW_ROOTS_AT_LAST_MAJOR.store(s.roots_at_last_major, Ordering::SeqCst);
    SHADOW_BYTES_PROMOTED.store(s.bytes_promoted, Ordering::SeqCst);
    SHADOW_PEAK_YOUNG_BYTES_LIVE.store(s.peak_young_bytes_live, Ordering::SeqCst);
    SHADOW_PEAK_OLD_BYTES_LIVE.store(s.peak_old_bytes_live, Ordering::SeqCst);
}

// ──────────────────────────────────────────────────────────────────── //
// Public snapshot API                                                  //
// ──────────────────────────────────────────────────────────────────── //

/// Point-in-time snapshot of the GC metrics shadow copy.
/// Readable from any thread; lock-free.
#[derive(Copy, Clone, Debug, Default)]
pub struct GcMetricsSnapshot {
    pub minor_collections:     u64,
    pub major_collections:     u64,
    pub young_bytes_allocated: u64,
    pub young_bytes_live:      u64,
    pub old_bytes_live:        u64,
    pub last_minor_pause_ns:   u64,
    pub last_major_pause_ns:   u64,
    pub total_minor_pause_ns:  u64,
    pub total_major_pause_ns:  u64,
    pub roots_at_last_minor:   u64,
    pub roots_at_last_major:   u64,
    pub bytes_promoted:        u64,
    pub peak_young_bytes_live: u64,
    pub peak_old_bytes_live:   u64,
}

/// Read the current GC metrics shadow.  Lock-free; `Relaxed` loads —
/// may lag behind by at most one GC cycle (sufficient for post-test
/// reporting).
pub fn gc_metrics_snapshot() -> GcMetricsSnapshot {
    GcMetricsSnapshot {
        minor_collections:     SHADOW_MINOR_COLLECTIONS.load(Ordering::Relaxed),
        major_collections:     SHADOW_MAJOR_COLLECTIONS.load(Ordering::Relaxed),
        young_bytes_allocated: SHADOW_YOUNG_BYTES_ALLOCATED.load(Ordering::Relaxed),
        young_bytes_live:      SHADOW_YOUNG_BYTES_LIVE.load(Ordering::Relaxed),
        old_bytes_live:        SHADOW_OLD_BYTES_LIVE.load(Ordering::Relaxed),
        last_minor_pause_ns:   SHADOW_LAST_MINOR_PAUSE_NS.load(Ordering::Relaxed),
        last_major_pause_ns:   SHADOW_LAST_MAJOR_PAUSE_NS.load(Ordering::Relaxed),
        total_minor_pause_ns:  SHADOW_TOTAL_MINOR_PAUSE_NS.load(Ordering::Relaxed),
        total_major_pause_ns:  SHADOW_TOTAL_MAJOR_PAUSE_NS.load(Ordering::Relaxed),
        roots_at_last_minor:   SHADOW_ROOTS_AT_LAST_MINOR.load(Ordering::Relaxed),
        roots_at_last_major:   SHADOW_ROOTS_AT_LAST_MAJOR.load(Ordering::Relaxed),
        bytes_promoted:        SHADOW_BYTES_PROMOTED.load(Ordering::Relaxed),
        peak_young_bytes_live: SHADOW_PEAK_YOUNG_BYTES_LIVE.load(Ordering::Relaxed),
        peak_old_bytes_live:   SHADOW_PEAK_OLD_BYTES_LIVE.load(Ordering::Relaxed),
    }
}

// ──────────────────────────────────────────────────────────────────── //
// No-alloc stack formatter                                             //
// ──────────────────────────────────────────────────────────────────── //

/// Fixed-capacity byte buffer implementing `fmt::Write` without any
/// heap allocation.  Excess bytes beyond `N` are silently dropped.
struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    const fn new() -> Self {
        Self { buf: [0u8; N], len: 0 }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn as_str(&self) -> &str {
        // SAFETY: we only ever write from `&str` slices, so the buffer
        // is always valid UTF-8.
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }
}

impl<const N: usize> core::fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = N - self.len;
        let to_copy = bytes.len().min(remaining);
        self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Core dump writer                                                     //
// ──────────────────────────────────────────────────────────────────── //

/// Write the crash dump to stderr.
///
/// Signal-safe: no heap allocation, no mutex acquisition.  Uses
/// `WriteFile` (Windows) or `write(2)` (Unix) to bypass libc
/// buffering.
///
/// `exception_info` is an optional one-line string describing the
/// exception kind, e.g.  `"EXCEPTION_ACCESS_VIOLATION at 0x00000040"`.
/// Pass `""` from the Rust panic hook; supply exception detail from
/// the SEH filter.
fn write_crash_dump(exception_info: &str) {
    use core::fmt::Write as _;

    let phase = GC_PHASE.load(Ordering::SeqCst);
    let phase_str = match phase {
        GC_PHASE_MINOR => "MINOR GC IN PROGRESS  (metrics show last completed cycle)",
        GC_PHASE_MAJOR => "MAJOR GC IN PROGRESS  (metrics show last completed cycle)",
        _ => "idle",
    };

    // Load all shadow fields.
    let minor   = SHADOW_MINOR_COLLECTIONS.load(Ordering::Relaxed);
    let major   = SHADOW_MAJOR_COLLECTIONS.load(Ordering::Relaxed);
    let yal     = SHADOW_YOUNG_BYTES_ALLOCATED.load(Ordering::Relaxed);
    let ylive   = SHADOW_YOUNG_BYTES_LIVE.load(Ordering::Relaxed);
    let olive   = SHADOW_OLD_BYTES_LIVE.load(Ordering::Relaxed);
    let lmin_ns = SHADOW_LAST_MINOR_PAUSE_NS.load(Ordering::Relaxed);
    let lmaj_ns = SHADOW_LAST_MAJOR_PAUSE_NS.load(Ordering::Relaxed);
    let tmin_ns = SHADOW_TOTAL_MINOR_PAUSE_NS.load(Ordering::Relaxed);
    let tmaj_ns = SHADOW_TOTAL_MAJOR_PAUSE_NS.load(Ordering::Relaxed);
    let rlmin   = SHADOW_ROOTS_AT_LAST_MINOR.load(Ordering::Relaxed);
    let rlmaj   = SHADOW_ROOTS_AT_LAST_MAJOR.load(Ordering::Relaxed);
    let prom    = SHADOW_BYTES_PROMOTED.load(Ordering::Relaxed);
    let pylive  = SHADOW_PEAK_YOUNG_BYTES_LIVE.load(Ordering::Relaxed);
    let polive  = SHADOW_PEAK_OLD_BYTES_LIVE.load(Ordering::Relaxed);

    // Thread-local safepoint depths.  Safe to read from the SEH
    // handler because it runs on the faulting thread.
    let jit_depth = crate::stack_map::active_jit_safepoint_depth();
    let aot_depth = crate::aot::active_aot_safepoint_depth();

    let mut buf = StackBuf::<8192>::new();

    let _ = write!(
        buf,
        "\n\
        ============================================================\n\
        === NOD CRASH DUMP ==========================================\n\
        ============================================================\n"
    );

    if !exception_info.is_empty() {
        let _ = writeln!(buf, "  exception            : {}", exception_info);
    }

    let _ = write!(
        buf,
        "  gc phase             : {}\n\
        ------------------------------------------------------------\n\
        GC METRICS  (last completed cycle)\n\
          minor collections    : {}\n\
          major collections    : {}\n\
          young allocated      : {} bytes\n\
          young live           : {} bytes\n\
          old live             : {} bytes\n\
          last minor pause     : {} ns  ({} us)\n\
          last major pause     : {} ns  ({} us)\n\
          total minor pause    : {} ns  ({} us)\n\
          total major pause    : {} ns  ({} us)\n\
          roots at last minor  : {}\n\
          roots at last major  : {}\n\
          bytes promoted       : {} bytes\n\
                    peak young live      : {} bytes\n\
                    peak old live        : {} bytes\n\
        ------------------------------------------------------------\n\
        SAFEPOINT STATE  (faulting thread)\n\
          JIT active frames    : {}\n\
          AOT active frames    : {}\n\
        ============================================================\n\n",
        phase_str,
        minor, major,
        yal,
        ylive,
        olive,
        lmin_ns, lmin_ns / 1_000,
        lmaj_ns, lmaj_ns / 1_000,
        tmin_ns, tmin_ns / 1_000,
        tmaj_ns, tmaj_ns / 1_000,
        rlmin,
        rlmaj,
        prom,
        pylive,
        polive,
        jit_depth,
        aot_depth,
    );

    write_bytes_to_stderr(buf.as_bytes());
}

// ──────────────────────────────────────────────────────────────────── //
// Platform write-to-stderr                                             //
// ──────────────────────────────────────────────────────────────────── //

#[cfg(windows)]
fn write_bytes_to_stderr(bytes: &[u8]) {
    // Declare the minimum Windows console/IO surface we need without
    // pulling in windows-sys features.  These are stable kernel32
    // exports that have existed since Win NT 3.1.
    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
        fn WriteFile(
            hFile: *mut core::ffi::c_void,
            lpBuffer: *const core::ffi::c_void,
            nNumberOfBytesToWrite: u32,
            lpNumberOfBytesWritten: *mut u32,
            lpOverlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    // STD_ERROR_HANDLE = (DWORD)-12 = 0xFFFF_FFF4
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;

    if bytes.is_empty() {
        return;
    }
    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        // INVALID_HANDLE_VALUE = (HANDLE)(LONG_PTR)-1
        if handle.is_null() || handle as isize == -1 {
            return;
        }
        let mut written = 0u32;
        WriteFile(
            handle,
            bytes.as_ptr().cast(),
            bytes.len().min(u32::MAX as usize) as u32,
            &mut written,
            core::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn write_bytes_to_stderr(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // SAFETY: write(2) is async-signal-safe per POSIX.
    unsafe {
        libc::write(2, bytes.as_ptr().cast(), bytes.len());
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Handler installation                                                 //
// ──────────────────────────────────────────────────────────────────── //

/// Install the crash dump handlers.  Called once from
/// `nod_runtime_init`.  Idempotent: the `OnceLock` ensures the Rust
/// panic hook and the Windows SEH filter are installed at most once
/// per process.
pub(crate) fn install() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        install_panic_hook();
        #[cfg(windows)]
        {
            install_seh_filter();
            // A STATUS_STACK_OVERFLOW leaves no stack for the unhandled
            // exception filter to run, so without this a stack overflow
            // terminates the process *silently* (this was the GAP-010
            // symptom: empty stderr, no crash dump). Reserve a guaranteed
            // stack reserve + install a vectored handler that fires
            // first-chance — see install_stack_overflow_handler.
            install_stack_overflow_handler();
        }
    });
}

/// Windows-only: make stack overflows reportable instead of silent.
///
/// Two parts:
///   1. `SetThreadStackGuarantee` reserves a slice of stack that stays
///      available *after* the guard page is hit, so an exception handler
///      can actually run on the overflowing thread.
///   2. `AddVectoredExceptionHandler` installs a first-chance handler.
///      `SetUnhandledExceptionFilter` (our `install_seh_filter`) is only
///      reached on the *second chance*, and a stack overflow never gets
///      there — frame-based dispatch can't unwind without stack. A VEH
///      runs first-chance, within the guaranteed reserve, so it can emit
///      a dump before the OS tears the process down.
///
/// Call once per process for the main thread; worker/mutator threads that
/// want the same protection should call
/// [`ensure_stack_overflow_reserve_this_thread`] after they start.
#[cfg(windows)]
fn install_stack_overflow_handler() {
    ensure_stack_overflow_reserve_this_thread();
    unsafe extern "system" {
        fn AddVectoredExceptionHandler(
            first: u32,
            handler: *const core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
    }
    unsafe {
        // first = 1 → run before any previously-registered VEH.
        AddVectoredExceptionHandler(
            1,
            vectored_stack_overflow_handler
                as unsafe extern "system" fn(*mut ExceptionPointers) -> i32
                as *const core::ffi::c_void,
        );
    }
}

/// Reserve enough stack on the *current* thread that a handler can run
/// after a stack-overflow guard-page fault. Idempotent and cheap; safe to
/// call from every thread that runs Dylan/GC code.
#[cfg(windows)]
pub(crate) fn ensure_stack_overflow_reserve_this_thread() {
    unsafe extern "system" {
        fn SetThreadStackGuarantee(stack_size_in_bytes: *mut u32) -> i32;
    }
    // 64 KiB is comfortably more than `report_stack_overflow` needs
    // (it uses a 512-byte stack buffer) while leaving slack for the OS
    // exception-dispatch machinery.
    let mut guarantee: u32 = 64 * 1024;
    unsafe {
        SetThreadStackGuarantee(&mut guarantee);
    }
}

/// Vectored handler: report (only) stack overflows, then let normal
/// dispatch proceed. Returns `EXCEPTION_CONTINUE_SEARCH` for everything,
/// so it never swallows an exception — including the runtime's own
/// deliberately-handled access violations, which it ignores entirely.
#[cfg(windows)]
unsafe extern "system" fn vectored_stack_overflow_handler(
    info: *mut ExceptionPointers,
) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const STATUS_STACK_OVERFLOW: u32 = 0xC000_00FD;
    if !info.is_null() {
        let exc_record = unsafe { (*info).exception_record };
        if !exc_record.is_null() {
            let code = unsafe { (*exc_record).exception_code };
            if code == STATUS_STACK_OVERFLOW {
                let addr = unsafe { (*exc_record).exception_address };
                report_stack_overflow(addr);
            }
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Lean stack-overflow report. Deliberately uses a *small* stack buffer
/// (we are, by definition, nearly out of stack) and the same direct
/// `write_bytes_to_stderr` path as the full crash dump.
#[cfg(windows)]
fn report_stack_overflow(addr: *mut core::ffi::c_void) {
    use core::fmt::Write as _;
    let phase = GC_PHASE.load(Ordering::SeqCst);
    let phase_str = match phase {
        GC_PHASE_MINOR => "MINOR GC in progress",
        GC_PHASE_MAJOR => "MAJOR GC in progress",
        _ => "idle (mutator)",
    };
    let aot_depth = crate::aot::active_aot_safepoint_depth();
    let jit_depth = crate::stack_map::active_jit_safepoint_depth();
    // Small but enough for the whole message; we are nearly out of stack,
    // but `SetThreadStackGuarantee` reserved 64 KiB so 1 KiB is safe.
    let mut buf = StackBuf::<1024>::new();
    let _ = write!(
        buf,
        "\n\
        ============================================================\n\
        === NOD CRASH DUMP: STACK OVERFLOW =========================\n\
        ============================================================\n  \
        EXCEPTION_STACK_OVERFLOW (code 0xc00000fd) at {addr:p}\n  \
        gc phase             : {phase_str}\n  \
        AOT safepoint frames : {aot_depth}\n  \
        JIT safepoint frames : {jit_depth}\n  \
        note                 : a thread exhausted its stack. A frame with a\n                         \
        huge/looping alloca is the usual cause (a loop-body alloca leaks\n                         \
        stack every iteration at -O0; hoist it to the entry block).\n\
        ============================================================\n\n"
    );
    write_bytes_to_stderr(buf.as_bytes());
}

fn install_panic_hook() {
    // Take the previous hook so we can chain it (Rust's default hook
    // prints the message + optional backtrace).
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A successful `block (return) … return(x) … end` non-local exit
        // is implemented as `panic_any(NlxPayload)` and is *control
        // flow*, not a crash. The enclosing `nod_run_block` catches it.
        // Suppress the crash dump (and the chained default hook) for it
        // so a working block/return doesn't spew a diagnostic to stderr.
        // A genuine unhandled `error("boom")` carries a `<simple-error>`
        // condition (not an `NlxPayload`), so it still crash-dumps.
        if info
            .payload()
            .downcast_ref::<crate::conditions::NlxPayload>()
            .is_some()
        {
            return;
        }
        // Build a one-line summary from panic location.
        let mut context = StackBuf::<1024>::new();
        use core::fmt::Write as _;
        if let Some(loc) = info.location() {
            let _ = write!(
                context,
                "Rust panic at {}:{}:{}",
                loc.file(),
                loc.line(),
                loc.column()
            );
        } else {
            let _ = write!(context, "Rust panic (no location)");
        }
        #[cfg(feature = "newgc-backend")]
        if let Some(stall) = info
            .payload()
            .downcast_ref::<newgc_core::page_heap::evac::GcStallError>()
        {
            let _ = write!(
                context,
                " | gc-stall reason={:?} from={:?} dest={:?} attempted-kind={:?} attempted-cells={} pages(free/g0/g1/tenured)={}/{}/{}/{} pin-set={} reserve-pages={} copied(objects/cells)={}/{} mark(live-bytes/live-pages/zero-live-released)={}/{}/{} recycled-mid-evac={}",
                stall.reason,
                stall.from_gen,
                stall.dest_gen,
                stall.attempted_kind,
                stall.attempted_cells,
                stall.free_pages,
                stall.g0_pages,
                stall.g1_pages,
                stall.tenured_pages,
                stall.pin_set_size,
                stall.reserve_pages,
                stall.objects_copied_before_failure,
                stall.cells_copied_before_failure,
                stall.mark_live_bytes,
                stall.mark_live_pages,
                stall.zero_live_pages_released,
                stall.pages_recycled_mid_evac,
            );
        }
        write_crash_dump(context.as_str());
        prev(info);
    }));
}

// ── Windows SEH filter ────────────────────────────────────────────── //

/// Raw struct layouts for the minimal EXCEPTION_POINTERS /
/// EXCEPTION_RECORD surface we need.  These match the Win32 ABI on
/// x86-64 exactly.
#[cfg(windows)]
#[repr(C)]
struct ExceptionRecord {
    /// NTSTATUS / DWORD exception code.
    exception_code: u32,
    exception_flags: u32,
    exception_record_chain: *mut ExceptionRecord,
    exception_address: *mut core::ffi::c_void,
    // NumberParameters + ExceptionInformation[15] follow but are not
    // accessed here.
}

#[cfg(windows)]
#[repr(C)]
struct ExceptionPointers {
    exception_record: *mut ExceptionRecord,
    context_record: *mut core::ffi::c_void,
}

#[cfg(windows)]
fn install_seh_filter() {
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(
            handler: *const core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
    }
    unsafe {
        SetUnhandledExceptionFilter(
            unhandled_exception_filter as unsafe extern "system" fn(*mut ExceptionPointers) -> i32
                as *const core::ffi::c_void,
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn unhandled_exception_filter(
    info: *mut ExceptionPointers,
) -> i32 {
    let mut context = StackBuf::<256>::new();
    use core::fmt::Write as _;

    if !info.is_null() {
        let exc_record = unsafe { (*info).exception_record };
        if !exc_record.is_null() {
            let code = unsafe { (*exc_record).exception_code };
            let addr = unsafe { (*exc_record).exception_address };
            let _ = write!(
                context,
                "{} (code {:#010x}) at {:p}",
                exception_code_name(code),
                code,
                addr
            );
        }
    }

    write_crash_dump(context.as_str());

    // EXCEPTION_CONTINUE_SEARCH (0): let Windows Error Reporting /
    // JIT debugger take over so the normal crash infrastructure
    // still fires.
    0
}

#[cfg(windows)]
fn exception_code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
        0xC000_0006 => "EXCEPTION_IN_PAGE_ERROR",
        0x8000_0003 => "EXCEPTION_BREAKPOINT",
        0x8000_0004 => "EXCEPTION_SINGLE_STEP",
        0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION",
        0xC000_0025 => "EXCEPTION_NONCONTINUABLE_EXCEPTION",
        0xC000_008C => "EXCEPTION_ARRAY_BOUNDS_EXCEEDED",
        0xC000_008D => "EXCEPTION_FLT_DENORMAL_OPERAND",
        0xC000_008E => "EXCEPTION_FLT_DIVIDE_BY_ZERO",
        0xC000_008F => "EXCEPTION_FLT_INEXACT_RESULT",
        0xC000_0090 => "EXCEPTION_FLT_INVALID_OPERATION",
        0xC000_0091 => "EXCEPTION_FLT_OVERFLOW",
        0xC000_0093 => "EXCEPTION_FLT_UNDERFLOW",
        0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_0095 => "EXCEPTION_INT_OVERFLOW",
        0xC000_0096 => "EXCEPTION_PRIV_INSTRUCTION",
        0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
        _ => "EXCEPTION_UNKNOWN",
    }
}
