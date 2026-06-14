//! Curated OpenDylan-flavoured fixtures, run end-to-end through the
//! NewOpenDylan JIT. Each test names a fixture under `fixtures/`,
//! compiles it, calls a designated entry point (typically `main`),
//! and asserts the i64 return value.
//!
//! These are the substitute for self-hosting that PLAN.md §2.7
//! commits to: every fixture is a small program a Dylan programmer
//! would recognise, expressed using only features the current
//! compiler implements. Each is small enough to debug by hand if it
//! regresses.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nod_od_suite::test_support::run_command_with_watchdog;
use serial_test::serial;

use nod_sema::run_function_to_i64;
use nod_runtime;

const CHILD_FIXTURE_ENV: &str = "NOD_OD_CHILD_FIXTURE";
const CHILD_ENTRY_ENV: &str = "NOD_OD_CHILD_ENTRY";
const CHILD_RESULT_PREFIX: &str = "__NOD_OD_RESULT__=";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn gc_stats_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target")
        .join("gc-stats");
    std::fs::create_dir_all(&dir).expect("create gc stats dir");
    dir
}

fn run_main(fixture: &str) -> i64 {
    let exe = std::env::current_exe().expect("test exe path");
    let mut child = Command::new(&exe);
    child
        .args([
            "--ignored",
            "--exact",
            "__nod_od_child_run_fixture",
            "--nocapture",
        ])
        .env(CHILD_FIXTURE_ENV, fixture)
        .env(CHILD_ENTRY_ENV, "main");
    let output = run_command_with_watchdog(
        fixture,
        "jit-run-main",
        fixture_timeout(fixture),
        &mut child,
    );
    assert!(
        output.status.success(),
        "child fixture runner failed for {}\nstdout:\n{}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
        fixture,
        output.stdout,
        output.stderr,
        output.stdout_path.display(),
        output.stderr_path.display(),
        output.meta_path.display()
    );
    output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix(CHILD_RESULT_PREFIX))
        .unwrap_or_else(|| {
            panic!(
                "missing child result marker for {}\nstdout:\n{}\nstderr:\n{}\nstdout log: {}\nstderr log: {}\nmeta: {}",
                fixture,
                output.stdout,
                output.stderr,
                output.stdout_path.display(),
                output.stderr_path.display(),
                output.meta_path.display()
            )
        })
        .parse::<i64>()
        .unwrap_or_else(|err| panic!("parse child result for {fixture}: {err}"))
}

fn fixture_timeout(fixture: &str) -> Duration {
    if fixture.contains("gc-rope") {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(120)
    }
}

#[test]
#[ignore]
#[serial]
fn __nod_od_child_run_fixture() {
    let fixture = match std::env::var(CHILD_FIXTURE_ENV) {
        Ok(value) => value,
        Err(_) => return,
    };
    let entry = std::env::var(CHILD_ENTRY_ENV).unwrap_or_else(|_| "main".to_string());
    let path = fixtures_dir().join(&fixture);
    let result = run_function_to_i64(&path, &entry)
        .unwrap_or_else(|e| panic!("run {fixture}::{entry}: {e:?}"));
    println!("{CHILD_RESULT_PREFIX}{result}");
}

/// Sprint 07-shape: pure recursion + branching + i64 arithmetic.
#[test]
#[serial]
fn fibonacci_10_is_55() {
    assert_eq!(run_main("fibonacci.dylan"), 55);
}

/// Sprint 07-shape: recursion with `mod`. Euclid's GCD on (48, 18).
#[test]
#[serial]
fn euclid_gcd_48_18_is_6() {
    assert_eq!(run_main("euclid-gcd.dylan"), 6);
}

/// Sprint 07-shape: mutual recursion across two `define function`s.
/// `is-even(8) = 1` after a 9-frame stack walk through is-odd/is-even.
#[test]
#[serial]
fn mutual_recursion_is_even_8() {
    assert_eq!(run_main("even-rec.dylan"), 1);
}

/// Sprint 12-shape: single-dispatch generic with two methods over a
/// shape hierarchy. `area(circle{radius=2}) + area(square{side=5})`
/// = 12 + 25 = 37.
#[test]
#[serial]
fn single_dispatch_over_shapes_sums_to_37() {
    assert_eq!(run_main("area-shapes.dylan"), 37);
}

/// Sprint 12-shape: inherited slot access through a CPL walk. A
/// <point-3d> reads its own `z` slot and the inherited `x` / `y`
/// slots. `1 + 2 + 3 = 6`.
#[test]
#[serial]
fn inherited_slot_access_sums_coords() {
    assert_eq!(run_main("point-3d-sum.dylan"), 6);
}

/// GC allocation loop: 1000 <box> objects allocated inside a while
/// loop; slot read from each before it dies.  Sum must be 500500.
/// Exercises allocation + slot reads under repeated object churn.
#[test]
#[serial]
fn gc_alloc_loop_1000_boxes() {
    assert_eq!(run_main("gc-alloc-loop.dylan"), 500500);
}

/// Rope buffer loaded from a real 86 296-byte file on disk.  Exercises
/// every rope op on real data: size, element, line-count,
/// line-to-offset, offset-to-line, for-each-leaf, rope-substring,
/// rope-concatenate, rope-split-at, rope-insert, rope-delete,
/// rope->string.  Returns rope-line-count = 2221 iff all assertions
/// pass; returns 0 on any failure.
///
/// The Dylan fixture loops 150 times (load → all-ops → discard) to
/// build up GC pressure.  After the run we force a full collection so
/// the report shows what the GC actually reclaimed.
#[test]
#[serial]
fn gc_rope_file_load_all_ops() {
    let gc_before    = nod_runtime::gc_metrics_snapshot();
    let young_before = nod_runtime::with_literal_pool(|p| p.heap.young_used_bytes());
    let old_before   = nod_runtime::with_literal_pool(|p| p.heap.old_used_bytes());

    let result = run_main("gc-rope-file-load.dylan");

    let young_after_run  = nod_runtime::with_literal_pool(|p| p.heap.young_used_bytes());
    let old_after_run    = nod_runtime::with_literal_pool(|p| p.heap.old_used_bytes());

    // Force a full collection so the shadow metrics reflect what was reclaimed.
    nod_runtime::with_literal_pool(|p| p.heap.collect_full());

    let gc_after     = nod_runtime::gc_metrics_snapshot();
    let young_after_gc = nod_runtime::with_literal_pool(|p| p.heap.young_used_bytes());
    let old_after_gc   = nod_runtime::with_literal_pool(|p| p.heap.old_used_bytes());

    let minor_delta = gc_after.minor_collections - gc_before.minor_collections;
    let major_delta = gc_after.major_collections - gc_before.major_collections;
    let prom_delta  = gc_after.bytes_promoted.saturating_sub(gc_before.bytes_promoted);

    let mut report = format!(
        "=== GC activity: gc-rope-file-load (150 passes) ===\n  \
         heap before run\n    \
         young used   : {} bytes\n    \
         old used     : {} bytes\n  \
         heap after run (before forced GC)\n    \
         young used   : {} bytes  (+{} bytes garbage)\n    \
         old used     : {} bytes\n  \
         after forced full GC\n    \
         young used   : {} bytes\n    \
         old used     : {} bytes\n    \
         reclaimed    : {} bytes\n  \
         GC counters (delta over full test)\n    \
         minor collections  : +{}\n    \
         major collections  : +{}\n    \
         bytes promoted     : +{} bytes\n  \
         absolute counters at report time\n    \
         minor collections  : {}\n    \
         major collections  : {}\n    \
         young allocated    : {} bytes\n    \
         peak young live    : {} bytes\n    \
         peak old live      : {} bytes\n",
        young_before,
        old_before,
        young_after_run,
        young_after_run.saturating_sub(young_before),
        old_after_run,
        young_after_gc,
        old_after_gc,
        (young_after_run + old_after_run).saturating_sub(young_after_gc + old_after_gc),
        minor_delta,
        major_delta,
        prom_delta,
        gc_after.minor_collections,
        gc_after.major_collections,
        gc_after.young_bytes_allocated,
        gc_after.peak_young_bytes_live,
        gc_after.peak_old_bytes_live,
    );
    if gc_after.last_major_pause_ns > gc_before.last_major_pause_ns || major_delta > 0 {
        report.push_str(&format!(
            "    last major pause   : {} us\n    roots at last major: {}\n",
            gc_after.last_major_pause_ns / 1_000,
            gc_after.roots_at_last_major
        ));
    }
    report.push_str("====================================================\n");
    std::fs::write(
        gc_stats_dir().join("gc-rope-file-load.stats.txt"),
        &report,
    )
    .expect("write rope gc stats report");
    print!("\n{report}");

    assert_eq!(result, 2221);
}
