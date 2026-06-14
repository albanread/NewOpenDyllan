//! GAP-011 GC collection tracer (NewOpenDylan addition).
//!
//! Runtime-gated by the `NOD_GC_TRACE` env var: set it to a file path and
//! every collection emits JSONL records to that file —
//!
//!   * one `collect_begin` per cycle (seq, kind, young-alloc bytes),
//!   * one `root` per registered root slot (provenance + slot address + the
//!     `Word` the slot currently holds),
//!   * one `root_rewrite` per root slot the evacuator visits (old -> new), and
//!   * one `collect_end` per cycle (post-cycle stats).
//!
//! Records sharing a `seq` belong to the same collection cycle. The file is
//! flushed after every line, so an abort/`exit 9` mid-cycle loses nothing —
//! the last record written is the last thing the collector did before the
//! crash. Zero cost when `NOD_GC_TRACE` is unset (one `OnceLock` probe).
//!
//! Conditional / zoom-in tracing (optional, for trimming a noisy trace down
//! to one object of interest):
//!
//!   * `NOD_GC_TRACE_WATCH=0xADDR[,0xADDR...]` — only emit `root` /
//!     `root_rewrite` records that touch one of these addresses (matched
//!     UNTAGGED, so the tagged Word and the bare pointer both hit, and a
//!     watched *slot* address works too). `collect_begin`/`collect_end`
//!     are always emitted as cycle scaffolding. Unset == emit everything.
//!   * `NOD_GC_TRACE_FOLLOW=1` — when watching, auto-extend the watch-list
//!     as the object relocates: any `root_rewrite` that touches a watched
//!     address adds its `old` and `new` addresses, so a move chain
//!     (A→B→C…) stays tracked across passes and cycles without having to
//!     pre-list every intermediate address.
//!
//! Typical zoom-in: seed `NOD_GC_TRACE_WATCH` with the stale address a crash
//! reports and set `NOD_GC_TRACE_FOLLOW=1` to get just that object's full
//! lifecycle.
//!
//! Safety: the tracer only ever touches the Rust/system heap and a file
//! handle — never the GC'd Dylan heap — so it is safe to call from inside a
//! collection (where allocating on the Dylan heap would be a bug).

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The JSONL sink, opened lazily from `NOD_GC_TRACE`. `None` when the env
/// var is unset or the file can't be created (tracing then no-ops).
fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("NOD_GC_TRACE")?;
        match File::create(&path) {
            Ok(f) => {
                eprintln!("[NOD_GC_TRACE] writing GC collection trace to {path:?}");
                Some(Mutex::new(f))
            }
            Err(e) => {
                eprintln!("[NOD_GC_TRACE] could not create {path:?}: {e}");
                None
            }
        }
    })
    .as_ref()
}

/// True when `NOD_GC_TRACE` named a writable file.
pub fn enabled() -> bool {
    sink().is_some()
}

/// Optional address watch-list, parsed once from `NOD_GC_TRACE_WATCH`
/// (comma-separated hex, `0x` optional). Stored UNTAGGED (low bit cleared)
/// so a watched object matches whether a record carries it tagged or not.
/// Empty == no filter (emit everything). Behind a `Mutex` because
/// `NOD_GC_TRACE_FOLLOW` mutates it as objects relocate.
fn watch_set() -> &'static Mutex<HashSet<u64>> {
    static WATCH: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    WATCH.get_or_init(|| {
        let mut set = HashSet::new();
        if let Some(v) = std::env::var_os("NOD_GC_TRACE_WATCH") {
            if let Some(s) = v.to_str() {
                for tok in s.split(',') {
                    let t = tok.trim().trim_start_matches("0x").trim_start_matches("0X");
                    if !t.is_empty() {
                        if let Ok(a) = u64::from_str_radix(t, 16) {
                            set.insert(a & !1);
                        }
                    }
                }
            }
        }
        Mutex::new(set)
    })
}

/// Whether `NOD_GC_TRACE_FOLLOW` is set — auto-extend the watch-list to a
/// relocated object's new (and prior) address so a move chain stays tracked.
fn follow_enabled() -> bool {
    static F: OnceLock<bool> = OnceLock::new();
    *F.get_or_init(|| std::env::var_os("NOD_GC_TRACE_FOLLOW").is_some())
}

/// True if no watch filter is configured, or any of `addrs` (compared
/// untagged) is currently watched.
fn watch_matches(addrs: &[u64]) -> bool {
    let set = watch_set().lock().expect("gc_trace watch poisoned");
    set.is_empty() || addrs.iter().any(|a| set.contains(&(a & !1)))
}

/// In follow mode (and only when a seed watch-list exists), add `addrs` to
/// the watch-list so the relocated object keeps matching on later cycles.
fn watch_extend(addrs: &[u64]) {
    if !follow_enabled() {
        return;
    }
    let mut set = watch_set().lock().expect("gc_trace watch poisoned");
    if set.is_empty() {
        return; // follow is only meaningful with an initial seed
    }
    for &a in addrs {
        if a > 1 {
            set.insert(a & !1);
        }
    }
}

static CYCLE_COUNTER: AtomicU64 = AtomicU64::new(0);
static CURRENT_CYCLE: AtomicU64 = AtomicU64::new(0);

/// The cycle id assigned by the most recent [`begin_cycle`]. Read by
/// `visit_roots` so per-root-rewrite records carry the owning cycle's seq.
pub fn current_cycle() -> u64 {
    CURRENT_CYCLE.load(Ordering::Relaxed)
}

fn write_line(line: &str) {
    if let Some(m) = sink() {
        if let Ok(mut f) = m.lock() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// Open a new collection cycle; returns its seq. Emits `collect_begin`.
pub fn begin_cycle(kind: &str, young_alloc: u64) -> u64 {
    let n = CYCLE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    CURRENT_CYCLE.store(n, Ordering::Relaxed);
    write_line(&format!(
        "{{\"seq\":{n},\"ev\":\"collect_begin\",\"kind\":\"{kind}\",\"young_alloc\":{young_alloc}}}"
    ));
    n
}

/// Record one registered root: its provenance, slot address, and the raw
/// `Word` bits the slot currently holds (pre-collection).
pub fn root(cycle: u64, idx: usize, src: &str, slot_addr: usize, word: u64) {
    if !watch_matches(&[slot_addr as u64, word]) {
        return;
    }
    write_line(&format!(
        "{{\"seq\":{cycle},\"ev\":\"root\",\"i\":{idx},\"src\":\"{src}\",\
         \"slot\":\"0x{slot_addr:016x}\",\"word\":\"0x{word:016x}\"}}"
    ));
}

/// Record that the evacuator visited a root slot, rewriting `old` -> `new`
/// (equal when the target wasn't relocated).
pub fn root_rewrite(cycle: u64, slot_addr: usize, old: u64, new: u64) {
    if !watch_matches(&[slot_addr as u64, old, new]) {
        return;
    }
    // Follow the object across this relocation so its new home (and prior
    // address) keep matching on subsequent passes/cycles.
    watch_extend(&[old, new]);
    write_line(&format!(
        "{{\"seq\":{cycle},\"ev\":\"root_rewrite\",\"slot\":\"0x{slot_addr:016x}\",\
         \"old\":\"0x{old:016x}\",\"new\":\"0x{new:016x}\",\"moved\":{}}}",
        if old != new { "true" } else { "false" }
    ));
}

/// Close a collection cycle. Emits `collect_end` with post-cycle stats.
#[allow(clippy::too_many_arguments)]
pub fn end_cycle(
    cycle: u64,
    kind: &str,
    minor: u64,
    major: u64,
    young_live: u64,
    old_live: u64,
    promoted: u64,
) {
    write_line(&format!(
        "{{\"seq\":{cycle},\"ev\":\"collect_end\",\"kind\":\"{kind}\",\"minor\":{minor},\
         \"major\":{major},\"young_live\":{young_live},\"old_live\":{old_live},\
         \"promoted\":{promoted}}}"
    ));
}
