//! Cost measurement for the scan/clean/verify pipeline.
//!
//! Ignored by default: this is a measuring tool, not an assertion. Timings vary
//! with the machine and the disk, so failing a build on them would be noise.
//!
//! ```text
//! cargo test --lib bench -- --ignored --nocapture
//! ```
//!
//! What it answers: how much the v0.5 scan and verification add on top of the
//! v0.4 clean, at batch sizes the product actually supports.

use std::time::{Duration, Instant};

use crate::testkit::{sample_for_format, scratch};
use crate::{clean_and_verify, run_scan, IdRegistry};

const FORMATS: [&str; 6] = ["mp4", "mov", "m4v", "mkv", "webm", "avi"];

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Build `count` fixtures, cycling through every supported container so the
/// mixed batches really are mixed.
fn build_batch(dir: &std::path::Path, count: usize) -> Vec<String> {
    let mut paths = Vec::with_capacity(count);
    for index in 0..count {
        let extension = FORMATS[index % FORMATS.len()];
        let per_file = dir.join(format!("f{index}"));
        std::fs::create_dir_all(&per_file).unwrap();
        paths.push(
            sample_for_format(&per_file, extension)
                .to_string_lossy()
                .into_owned(),
        );
    }
    paths
}

/// Three phases, measured separately against the same inputs:
///
/// * **scan** — what the user waits for after dropping files in;
/// * **clean only** — the v0.4 pipeline, unchanged;
/// * **clean + verify** — the v0.5 pipeline.
///
/// The verification overhead is the difference between the last two, measured
/// rather than estimated, so the cost of the new guarantee is explicit.
fn measure(label: &str, count: usize) {
    let dir = scratch(&format!("bench-{count}"));
    let baseline_out = dir.join("out-clean-only");
    let verified_out = dir.join("out-clean-verify");
    std::fs::create_dir_all(&baseline_out).unwrap();
    std::fs::create_dir_all(&verified_out).unwrap();
    let paths = build_batch(&dir, count);

    // Phase 1: scan. One ffprobe run per file, plus classification.
    let started = Instant::now();
    let scans = run_scan(&paths, |_, _, _| {});
    let scan_time = started.elapsed();
    let findings: usize = scans.iter().map(|s| s.summary.total).sum();

    // Phase 2: the v0.4 cleaning path on its own.
    let mut registry = IdRegistry::open(&dir.join("ids-a.txt")).unwrap();
    let started = Instant::now();
    for path in &paths {
        let input = std::path::PathBuf::from(path);
        crate::clean_one(&input, &baseline_out, "BASE", &mut registry).expect("bench clean failed");
    }
    let clean_time = started.elapsed();

    // Phase 3: the v0.5 path, which adds a re-inspection of the input and an
    // inspection of the output.
    let mut registry = IdRegistry::open(&dir.join("ids-b.txt")).unwrap();
    let started = Instant::now();
    let mut verified = 0usize;
    for path in &paths {
        let input = std::path::PathBuf::from(path);
        let (outcome, report) = clean_and_verify(&input, &verified_out, "BENCH", &mut registry);
        assert!(outcome.is_ok(), "bench clean failed: {outcome:?}");
        if report.is_some_and(|r| r.verified) {
            verified += 1;
        }
    }
    let clean_verify_time = started.elapsed();
    let overhead = clean_verify_time.saturating_sub(clean_time);

    let per = |d: Duration| millis(d) / count as f64;
    println!(
        "\n{label:<10} files={count:<3} findings={findings:<4} verified={verified}/{count}\n  \
         scan                {:>8.1} ms ({:>6.1} ms/file)\n  \
         clean only  (v0.4)  {:>8.1} ms ({:>6.1} ms/file)\n  \
         clean+verify (v0.5) {:>8.1} ms ({:>6.1} ms/file)\n  \
         verify overhead     {:>8.1} ms ({:>6.1} ms/file)\n  \
         scan + clean+verify {:>8.1} ms",
        millis(scan_time),
        per(scan_time),
        millis(clean_time),
        per(clean_time),
        millis(clean_verify_time),
        per(clean_verify_time),
        millis(overhead),
        per(overhead),
        millis(scan_time) + millis(clean_verify_time),
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// Isolates the cost of one inspection, which is the unit the whole scan is
/// built from and the number that decides whether 100 files stays usable.
#[test]
#[ignore]
fn bench_single_inspection() {
    let dir = scratch("bench-single");
    let input = sample_for_format(&dir, "mp4");

    // Warm the process cache so the first run does not dominate.
    let _ = crate::inspect::inspect(&input);

    const RUNS: usize = 20;
    let started = Instant::now();
    for _ in 0..RUNS {
        crate::inspect::inspect(&input).unwrap();
    }
    let elapsed = started.elapsed();
    println!(
        "single inspection  {:>6.1} ms/run over {RUNS} runs",
        millis(elapsed) / RUNS as f64
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore]
fn bench_one_small_file() {
    measure("1 small", 1);
}

#[test]
#[ignore]
fn bench_six_mixed_files() {
    measure("6 mixed", 6);
}

#[test]
#[ignore]
fn bench_twenty_files() {
    measure("20 mixed", 20);
}
