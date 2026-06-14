//! Sprint 37 — JIT object-code cache tests.
//!
//! See `nod-llvm/src/cache.rs` for the cache mechanism rationale and
//! the architectural decisions documented per phase. The MCJIT
//! ObjectCache C-API is not exposed by either `llvm-sys` 221 or
//! `inkwell` 0.9 — Sprint 37 lands an in-process JIT-output cache
//! (skipping codegen + MCJIT on hit) plus on-disk bitcode + sidecar
//! infrastructure for future cross-process replay (Sprint 38 AOT).

use std::path::PathBuf;
use std::time::Instant;

use nod_sema::{eval_expr_to_string, eval_expr_with_items_to_string};
use serial_test::serial;

/// Time `f` once; return (result, elapsed).
fn time_once<F: FnOnce() -> R, R>(f: F) -> (R, std::time::Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

fn sample_items() -> &'static str {
    include_str!("../fixtures/jit_cache_sample_items.dylan")
}

fn sample_expr() -> &'static str {
    include_str!("../fixtures/jit_cache_sample.dylan")
}

fn cache_dir_for_test(name: &str) -> PathBuf {
    let mut dir = nod_llvm::default_cache_dir();
    dir.push(format!("test-{name}"));
    dir
}

fn reset_cache_state(dir: &std::path::Path) {
    nod_llvm::in_process_clear();
    nod_llvm::reset_stats();
    nod_llvm::clear_cache_dir(dir);
}

/// Wire NOD_JIT_CACHE_DIR so the disk side of the cache lands in a
/// per-test directory; clean it out before the test runs.
fn with_test_cache_dir<F: FnOnce(&std::path::Path)>(name: &str, f: F) {
    let dir = cache_dir_for_test(name);
    // SAFETY: env mutation requires unsafe in 2024 edition. Tests are
    // #[serial], so no other thread races.
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &dir) };
    // Ensure parent exists
    let _ = std::fs::create_dir_all(&dir);
    reset_cache_state(&dir);
    f(&dir);
    reset_cache_state(&dir);
    // Drop the env var so other tests aren't poisoned. Tests are
    // #[serial], so the env mutation is single-threaded.
    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
}

#[test]
#[serial]
fn cache_miss_then_hit_is_at_least_10x_faster() {
    with_test_cache_dir("hit-10x", |_dir| {
        let items = sample_items();
        let expr = sample_expr();

        // Warm-up — run a small unrelated eval first so the stdlib's
        // JIT engine, the eval-context, and the C runtime are all
        // ready. Without this, the cold compile of the sample fixture
        // also has to pay for one-shot process-wide init costs that
        // can skew the t1 measurement under heavy parallel test load.
        let _ = eval_expr_to_string("1 + 2").expect("warm-up eval");

        // Take the best-of-3 cold-compile timing to dampen scheduling
        // noise from `cargo test --workspace` running other binaries
        // in parallel. The cache itself isn't noisy — its speedup is
        // structural — but the cold compile's wall-clock measurement
        // can swing 2-3× under load. Best-of-3 keeps the headline
        // ratio reflecting the cache's actual speedup, not jitter.
        let mut cold_results: Vec<(String, std::time::Duration)> = Vec::with_capacity(3);
        for _ in 0..3 {
            nod_llvm::in_process_clear();
            let (r, t) = time_once(|| {
                eval_expr_with_items_to_string(items, expr).expect("cold eval")
            });
            cold_results.push((r, t));
        }
        let (r1, t1) = cold_results
            .iter()
            .min_by_key(|(_, t)| *t)
            .map(|(r, t)| (r.clone(), *t))
            .unwrap();

        let (r2, t2) = time_once(|| {
            eval_expr_with_items_to_string(items, expr).expect("hot eval")
        });

        assert_eq!(r1, r2, "cold/hot results must match");

        // Headline: t2 should be ≥10x faster than t1.
        let ratio = (t1.as_micros() as f64 / t2.as_micros().max(1) as f64).max(0.0);
        eprintln!(
            "JIT-cache headline: t1={t1:?} t2={t2:?} ratio={ratio:.1}x"
        );
        assert!(
            t2.saturating_mul(10) <= t1,
            "cache hit should be ≥10x faster; got t1={t1:?} t2={t2:?} ratio={ratio:.1}x"
        );
    });
}

#[test]
#[serial]
fn cache_invalidates_on_source_change() {
    with_test_cache_dir("source-change", |_dir| {
        let _ = eval_expr_to_string("1 + 2 + 3").expect("eval 1");
        let stats_before = nod_llvm::read_stats(&nod_llvm::default_cache_dir());
        // Same source again → hit.
        let _ = eval_expr_to_string("1 + 2 + 3").expect("eval 1 hot");
        let stats_after_hot = nod_llvm::read_stats(&nod_llvm::default_cache_dir());
        assert_eq!(
            stats_after_hot.hits, stats_before.hits + 1,
            "second eval of identical source should hit cache"
        );
        // Different source → miss.
        let _ = eval_expr_to_string("1 + 2 + 4").expect("eval 2");
        let stats_after_miss = nod_llvm::read_stats(&nod_llvm::default_cache_dir());
        assert_eq!(
            stats_after_miss.misses, stats_after_hot.misses + 1,
            "different source must miss"
        );
    });
}

#[test]
#[serial]
fn cache_invalidates_on_runtime_abi_bump() {
    // Verify the cache key depends on ABI version. We can't actually
    // bump the constant at test time, so we hash directly and compare.
    let dfm = "fn <eval-entry> () -> i64:\n  b0:\n    Return\n";
    let k1 = nod_llvm::cache_key(
        dfm, "0.0.1", 1, 22, "x86_64-pc-windows-msvc", 2,
    );
    let k2 = nod_llvm::cache_key(
        dfm, "0.0.1", 2, 22, "x86_64-pc-windows-msvc", 2,
    );
    assert_ne!(k1, k2, "key must change when ABI version bumps");
}

#[test]
#[serial]
fn cache_stats_track_hits_and_misses() {
    with_test_cache_dir("stats", |dir| {
        // Fresh stats. Note: stats are process-global, so any earlier
        // test's eval calls have already bumped them. We compare deltas.
        let s0 = nod_llvm::read_stats(dir);
        let _ = eval_expr_to_string("100 + 23").expect("eval");
        let s1 = nod_llvm::read_stats(dir);
        assert_eq!(s1.misses, s0.misses + 1, "first eval = miss");
        assert_eq!(s1.hits, s0.hits, "no hits yet");
        let _ = eval_expr_to_string("100 + 23").expect("hot eval");
        let s2 = nod_llvm::read_stats(dir);
        assert_eq!(s2.misses, s1.misses, "second eval = no new miss");
        assert_eq!(s2.hits, s1.hits + 1, "second eval = +1 hit");
        // After two cold evals + one hot, the on-disk size should be
        // non-zero — we wrote bitcode on each miss.
        let _ = eval_expr_to_string("100 + 24").expect("third eval");
        let s3 = nod_llvm::read_stats(dir);
        assert!(s3.entries >= 2, "expected ≥2 cache entries, got {}", s3.entries);
        assert!(s3.bytes_on_disk > 0, "expected non-zero on-disk size");
    });
}

#[test]
#[serial]
fn lru_evicts_oldest_when_over_max() {
    with_test_cache_dir("lru", |dir| {
        // Populate the cache with a handful of entries, then evict to
        // a very small cap. The oldest entries should disappear.
        let _ = eval_expr_to_string("1 + 1").expect("e1");
        // Force a non-zero accessed_at delta between entries by
        // sleeping briefly. Without this two entries can share an
        // unix-ms timestamp and the sort becomes implementation-
        // defined.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = eval_expr_to_string("2 + 2").expect("e2");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = eval_expr_to_string("3 + 3").expect("e3");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = eval_expr_to_string("4 + 4").expect("e4");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = eval_expr_to_string("5 + 5").expect("e5");

        let before = nod_llvm::read_stats(dir);
        assert!(before.entries >= 5, "expected ≥5 entries, got {}", before.entries);

        // Evict to a 1-byte cap → everything goes.
        let evicted = nod_llvm::evict_to(dir, 1);
        let after = nod_llvm::read_stats(dir);
        assert!(evicted >= 5, "expected ≥5 evicted, got {evicted}");
        assert_eq!(after.entries, 0, "everything should be gone");
        assert_eq!(after.bytes_on_disk, 0);
    });
}

#[test]
#[serial]
fn cache_corruption_recovers_via_recompile() {
    with_test_cache_dir("corrupt", |dir| {
        // Populate the cache with a known entry.
        let r1 = eval_expr_to_string("7 * 8").expect("e1");
        assert_eq!(r1, "56");

        // Find the .bc file (there should be exactly one).
        let bc_path = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(|d| d.ok())
            .find(|d| d.path().extension().and_then(|s| s.to_str()) == Some("bc"))
            .map(|d| d.path())
            .expect("expected a .bc file in cache dir");
        // Overwrite with garbage.
        std::fs::write(&bc_path, b"GARBAGE NOT VALID BITCODE").expect("garble");

        // Clear the in-process cache so the next eval consults the
        // disk. The disk read sees a corrupted .bc → falls back to a
        // fresh compile, which overwrites the bad file with a valid
        // one. The headline assertion: the eval still produces the
        // right answer.
        nod_llvm::in_process_clear();
        let r2 = eval_expr_to_string("7 * 8").expect("e1 after corruption");
        assert_eq!(r2, "56");
    });
}

#[test]
#[serial]
fn dfm_ir_is_deterministic_across_two_eval_calls() {
    // Build two LoweredModules for the same source and compare their
    // DFM IR text byte-for-byte. This is the Phase A determinism
    // regression test.
    use nod_dfm::format_for_cache_key;
    use nod_reader::{SourceMap, lex, scan_preamble};

    let src = "Module: __t__\n\
        \n\
        define function f (x :: <integer>) => (<integer>)\n  \
          x * x + 1\nend;\n\
        define function <eval-entry> ()\n  \
          f(7) + f(11)\nend;\n";

    nod_sema::stdlib::ensure_loaded();

    fn lower_to_text(src: &str) -> String {
        let mut sm = SourceMap::new();
        let id = sm.add("<t>", src.to_string()).unwrap();
        let toks = lex(src, id);
        let pre = scan_preamble(src);
        let module = nod_reader::parse_module(src, &toks, pre.as_ref())
            .expect("parse");
        let lm = nod_sema::lower_module_full(&module).expect("lower");
        format_for_cache_key(&lm.functions)
    }

    let a = lower_to_text(src);
    let b = lower_to_text(src);
    assert_eq!(a, b, "DFM IR must be byte-identical across two lowering calls");
}

#[test]
#[serial]
fn cache_directory_respects_env_override() {
    // Tests using with_test_cache_dir already verify this indirectly,
    // but assert the API directly too: setting NOD_JIT_CACHE_DIR
    // changes default_cache_dir.
    let probe = PathBuf::from(r"C:\nod-test-cache-probe");
    unsafe { std::env::set_var("NOD_JIT_CACHE_DIR", &probe) };
    let resolved = nod_llvm::default_cache_dir();
    assert_eq!(resolved, probe);
    unsafe { std::env::remove_var("NOD_JIT_CACHE_DIR") };
}
