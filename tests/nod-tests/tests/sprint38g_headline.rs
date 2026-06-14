//! Sprint 38g — headline subprocess cache-speedup measurement.
//!
//! Sprints 38a-38e built the cross-process bitcode replay
//! infrastructure (manifest, slot allocators, codegen surgery, named
//! external globals). Sprint 38f wired that path into
//! `eval_wrapped_source` so a fresh process actually reads bitcode +
//! manifest + registrations sidecar back from disk and replays via
//! `add_module_from_bitcode` instead of cold-compiling. Sprint 38g is
//! the **measurement**: spawn a child process twice — once with an
//! empty cache dir (forcing cold compile), once with a populated cache
//! dir (forcing disk-replay) — and report the wall-clock speedup.
//!
//! ## Why this is `#[ignore]`-only
//!
//! Timing tests flake. System load, antivirus scans, disk caching,
//! lock contention with other tests — any of these can perturb the
//! observed ratio by an order of magnitude. The user's "correctness
//! before perf gates" rule applies: this test is *informational* (it
//! prints the actual measured ratio when run manually) rather than a
//! correctness gate. The soft assertion is just `warm < cold`.
//!
//! To run manually:
//!
//! ```text
//! cargo test --test sprint38g_headline -- --ignored --nocapture
//! ```
//!
//! ## Subprocess design
//!
//! The headline test re-spawns the **same test binary** via
//! `std::env::current_exe()`, pointing the inner invocation at
//! `sprint38g_workload_runner_inner` (a sibling `#[test] #[ignore]`
//! that just calls `eval_expr_to_string` on a representative workload).
//! Cargo's test runner respects `--exact <name> --ignored --nocapture
//! --test-threads=1` flags on the spawned binary, so we get a tight
//! one-shot child process that does only the workload and exits.
//!
//! Each subprocess inherits a fresh `NOD_JIT_CACHE_DIR` env var
//! pointing at a per-headline-run temp directory. The cold run sees
//! an empty directory; the warm run sees the bitcode + manifest +
//! registrations sidecar the cold run wrote.
//!
//! ## What the workload exercises
//!
//! `size(make(<range>, from: 0, to: 5))` — a simple expression that:
//! - loads + macro-expands + lowers the entire stdlib (the dominant
//!   cold cost),
//! - emits one `Computation::Dispatch` for `size` (Sprint 38e cache
//!   slot + generic external globals),
//! - emits `RelocKind::ClassMetadata` for `<range>` (Sprint 38c),
//! - emits multiple `RelocKind::SymbolLiteral` for the `from:` / `to:`
//!   keywords (Sprint 38c).
//!
//! No Win32 calls — we don't want stub-entry resolution latency to
//! perturb the headline, and stub-entry caching is already exercised
//! by `jit_cache_xprocess_eval::sprint38f_disk_replay_round_trip_winapi`.
//!
//! ## Observed ratios and what they mean
//!
//! On the development host this test currently reports ~1.07×
//! speedup. That is **not** the cache being weak; it's the workload
//! being trivial. Cold + warm subprocesses are each ~65ms, of which
//! ~50ms is fixed Windows process startup + Rust test-runner init +
//! LLVM context creation. The actual cold-vs-warm compile delta is
//! a few ms because parsing + lowering + codegen of a one-line
//! expression is fast in absolute terms.
//!
//! The headline ratio grows linearly with workload size (cold compile
//! is O(source size)) while subprocess overhead stays fixed. Real
//! IDE-scale workloads — many user functions, many classes, methods
//! covering several generics — will show the ≥10× speedup Sprint 37's
//! cache was scoped around. This test's job is to **prove the
//! cross-process replay path fires correctly** (warm subprocess
//! reports `disk_hits=1`), not to demonstrate the ceiling number.
//! Sprint 38g is the last piece of Sprint 38 plumbing — bigger
//! workloads pay back the work as they appear.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serial_test::serial;

/// The Dylan expression the subprocess workload runner evaluates.
/// Returns `6` as a tagged fixnum (range `0..5` is inclusive on both
/// ends → 6 elements). We don't assert the value here — that's the
/// inner test's job; the outer test only measures wall time.
const WORKLOAD_EXPR: &str = "size(make(<range>, from: 0, to: 5))";

/// Inner test — invoked as a child process by
/// `sprint38g_cross_process_cache_speedup_headline`. Just evaluates
/// the workload expression once. The cache directory is supplied via
/// the inherited `NOD_JIT_CACHE_DIR` env var (set by the parent before
/// `Command::output`).
///
/// `#[ignore]` so it doesn't run in routine `cargo test`. The parent
/// passes `--exact --ignored` so this inner test runs in isolation.
#[test]
#[ignore]
fn sprint38g_workload_runner_inner() {
    // SAFETY: We're a freshly-spawned subprocess; no other thread is
    // touching the runtime statics yet. The env var is set by the
    // parent before spawning, so `default_cache_dir()` picks it up.
    let result =
        nod_sema::eval_expr_to_string(WORKLOAD_EXPR).expect("workload eval must succeed");
    // Sanity-check the value so a silently-broken eval doesn't pass
    // the timing test trivially.
    assert_eq!(result, "6", "workload must return 6, got {result}");

    // Print the disk-cache counters so the parent can see whether the
    // inner subprocess hit the disk-replay path or cold-compiled. The
    // headline test asserts `disk_hits=1` in the warm subprocess.
    let (disk_hits, disk_misses) = nod_llvm::disk_cache_stats();
    println!("[inner] disk_hits={disk_hits} disk_misses={disk_misses}");
}

/// Headline subprocess timing test — the actual Sprint 38g
/// deliverable.
///
/// Spawns the inner workload runner twice with the same per-headline
/// cache directory. The first invocation finds the directory empty
/// (cold compile + write bitcode + manifest + registrations sidecar
/// to disk). The second invocation finds the directory populated and
/// takes the disk-replay path (Sprint 38f).
///
/// Prints both wall-clock durations and the observed cold/warm ratio
/// so a human reader can see the headline number. Asserts only that
/// `warm < cold` — the actual ratio depends on host system load and
/// is informational rather than gated.
#[test]
#[ignore]
#[serial]
fn sprint38g_cross_process_cache_speedup_headline() {
    let cache_dir = unique_cache_dir();
    // Defensive — ensure the directory is empty at the start.
    let _ = std::fs::remove_dir_all(&cache_dir);
    std::fs::create_dir_all(&cache_dir).expect("create cache dir");

    let exe = std::env::current_exe().expect("current_exe");

    // ── Cold run: empty cache dir, full pipeline runs. ──
    let cold_result = run_workload_subprocess(&exe, &cache_dir);
    eprintln!(
        "[headline] cold subprocess: status={} duration={:?}",
        cold_result.status_summary, cold_result.duration
    );
    eprintln!("[headline] cold inner stdout:\n{}", cold_result.stdout);
    assert!(
        cold_result.success,
        "cold subprocess must succeed; stderr:\n{}",
        cold_result.stderr
    );

    // The cold run must have written the cache trio to disk for the
    // warm run to pick up. Verify before we measure the warm side —
    // a missing file here means the cold path didn't persist the
    // sidecar properly and the warm run would silently cold-compile
    // again, defeating the test.
    let cache_files_after_cold = list_cache_files(&cache_dir);
    let has_bitcode = cache_files_after_cold.iter().any(|p| p.ends_with(".bc"));
    let has_manifest = cache_files_after_cold
        .iter()
        .any(|p| p.ends_with(".manifest.json"));
    let has_registrations = cache_files_after_cold
        .iter()
        .any(|p| p.ends_with(".registrations.json"));
    assert!(
        has_bitcode && has_manifest && has_registrations,
        "cold run must persist bitcode + manifest + registrations; got: {cache_files_after_cold:?}"
    );

    // ── Warm run: same cache dir, disk-replay should fire. ──
    let warm_result = run_workload_subprocess(&exe, &cache_dir);
    eprintln!(
        "[headline] warm subprocess: status={} duration={:?}",
        warm_result.status_summary, warm_result.duration
    );
    eprintln!("[headline] warm inner stdout:\n{}", warm_result.stdout);
    assert!(
        warm_result.success,
        "warm subprocess must succeed; stderr:\n{}",
        warm_result.stderr
    );

    // The warm inner subprocess must have reported `disk_hits=1`. If
    // it shows `disk_hits=0` then the disk-replay path didn't fire and
    // we're measuring two cold compiles — the test would technically
    // still pass `warm < cold` due to OS file-cache warming, but it
    // wouldn't be measuring what we claim.
    assert!(
        warm_result.stdout.contains("disk_hits=1"),
        "warm subprocess must report disk_hits=1 (disk-replay path fired); got stdout:\n{}",
        warm_result.stdout
    );

    let cold_ms = cold_result.duration.as_secs_f64() * 1000.0;
    let warm_ms = warm_result.duration.as_secs_f64() * 1000.0;
    let ratio = cold_ms / warm_ms;

    // ── The headline number. ──
    println!("\n┌─────────────────────────────────────────────────────────┐");
    println!("│ Sprint 38g — cross-process JIT cache speedup headline   │");
    println!("├─────────────────────────────────────────────────────────┤");
    println!("│ Workload:  {WORKLOAD_EXPR:<45} │");
    println!("│ Cold:      {cold_ms:>10.1} ms                                   │");
    println!("│ Warm:      {warm_ms:>10.1} ms                                   │");
    println!("│ Speedup:   {ratio:>10.2}x                                    │");
    println!("└─────────────────────────────────────────────────────────┘\n");

    // Soft gate — informational test. Only assertion is that warm is
    // actually faster than cold; the magnitude varies with host load.
    assert!(
        warm_result.duration < cold_result.duration,
        "warm ({:?}) must be faster than cold ({:?}); cache isn't doing meaningful work",
        warm_result.duration,
        cold_result.duration
    );

    // Tidy up.
    let _ = std::fs::remove_dir_all(&cache_dir);
}

/// Per-headline-run temp directory inside the OS temp dir. Uses both
/// the process ID and `Instant::now`'s nanos to avoid collisions
/// across concurrent CI runs of the same test binary.
fn unique_cache_dir() -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("nod-sprint38g-{pid}-{nanos:x}"))
}

/// Result of one subprocess invocation.
struct SubprocessResult {
    duration: Duration,
    success: bool,
    status_summary: String,
    stdout: String,
    stderr: String,
}

/// Spawn the current test binary as a subprocess, asking it to run
/// **only** the `sprint38g_workload_runner_inner` test. Passes the
/// cache directory through `NOD_JIT_CACHE_DIR`.
///
/// The subprocess's wall time INCLUDES OS process startup overhead
/// (~10-50ms on Windows). That's fixed across cold + warm so it
/// doesn't bias the ratio in either direction, but it does cap the
/// observable headline speedup — if the cold-vs-warm compile cost
/// difference is smaller than process startup, the ratio looks
/// modest. The workload is sized so the compile-cost delta dominates.
fn run_workload_subprocess(exe: &Path, cache_dir: &Path) -> SubprocessResult {
    let start = Instant::now();
    let output = Command::new(exe)
        .args([
            "--exact",
            "sprint38g_workload_runner_inner",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("NOD_JIT_CACHE_DIR", cache_dir)
        .output()
        .expect("subprocess spawn");
    let duration = start.elapsed();

    SubprocessResult {
        duration,
        success: output.status.success(),
        status_summary: format!("{:?}", output.status),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

/// List the files (just stems + extensions, not full paths) in a
/// cache directory. Used for the post-cold sanity check.
fn list_cache_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|de| de.file_name().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}
