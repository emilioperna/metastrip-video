//! The boundary between the Tauri commands and the synchronous pipeline.
//!
//! `scan_videos` and `clean_videos` hand their whole body to the blocking thread
//! pool through `run_blocking`, so the window's own thread keeps pumping
//! messages while FFmpeg runs. These tests pin what that move must and must not
//! change: where `run_blocking` runs the work, what a panic turns into, and that
//! results and progress come out exactly as the pipeline produced them, one
//! file after another, while the batch is running.
//!
//! The commands themselves need a live `AppHandle`, so they are checked for
//! shape only: each must be `async`. Their bodies are one `run_blocking` call
//! around the request functions tested here.
//!
//! No test here measures time. Order is proven by recording events, not by
//! racing them.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread::{self, ThreadId};

use serde_json::Value;
use tauri::async_runtime::block_on;
use tauri::AppHandle;

use crate::testkit::{sample_video, scratch};
use crate::{
    clean_request, run_blocking, scan_request, write_settings, BatchGuard, Settings, CLEAN_BUSY,
    CLEAN_INTERNAL_ERROR, SCAN_INTERNAL_ERROR, TEMP_PREFIX,
};

/// Compiles only for a function that returns a future: a command whose body
/// runs inline on the window's thread would not fit this signature.
fn assert_async_command<F, Fut, T>(_: F)
where
    F: FnOnce(AppHandle, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>> + Send + 'static,
{
}

#[test]
fn the_media_commands_are_async() {
    // Revert either one to a plain `fn` (the v0.4 freeze) and this stops building.
    assert_async_command(crate::scan_videos);
    assert_async_command(crate::clean_videos);
}

#[test]
fn run_blocking_hands_back_results_and_errors_unchanged() {
    assert_eq!(
        block_on(run_blocking(CLEAN_INTERNAL_ERROR, || Ok::<_, String>(42))),
        Ok(42)
    );
    // Error strings are the IPC contract with the frontend: passed through verbatim.
    assert_eq!(
        block_on(run_blocking(CLEAN_INTERNAL_ERROR, || {
            Err::<(), _>("Choose an output folder first.".to_string())
        })),
        Err("Choose an output folder first.".to_string())
    );
}

#[test]
fn run_blocking_runs_on_the_blocking_pool() {
    let caller = thread::current().id();
    let worker = block_on(run_blocking(CLEAN_INTERNAL_ERROR, || {
        // Panics on the runtime's core workers and inside `block_on`, which
        // would come back as the internal error. Succeeding here means the work
        // ran neither inline nor on a core worker.
        tauri::async_runtime::handle().block_on(async {});
        Ok(thread::current().id())
    }))
    .expect("the work did not run on the blocking pool");
    assert_ne!(worker, caller, "the work ran inline on the calling thread");

    // The probe really tells the two apart: on a core worker it fails.
    let on_core_worker = block_on(tauri::async_runtime::spawn(async {
        tauri::async_runtime::handle().block_on(async {});
    }));
    assert!(on_core_worker.is_err());
}

#[test]
fn a_panic_becomes_the_fixed_internal_error_and_the_app_carries_on() {
    for internal_error in [SCAN_INTERNAL_ERROR, CLEAN_INTERNAL_ERROR] {
        // The panic message below is printed by the default hook; that is expected.
        let result = block_on(run_blocking(internal_error, || -> Result<(), String> {
            panic!("could not read C:\\Users\\someone\\secret-clip.mp4")
        }));
        let message = result.unwrap_err();
        assert_eq!(message, internal_error);
        assert!(
            !message.contains("secret-clip") && !message.contains('\\'),
            "the panic payload leaked: {message}"
        );
    }

    // Nothing is poisoned: the next piece of work runs normally.
    assert_eq!(
        block_on(run_blocking(SCAN_INTERNAL_ERROR, || Ok::<_, String>(7))),
        Ok(7)
    );
}

/// Serialised the way the frontend receives it.
fn json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap()
}

fn strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_scan_on_the_worker_matches_a_direct_scan_file_for_file() {
    let dir = scratch("offthread-scan");
    let good = sample_video(&dir, "good.mp4");
    let unsupported = dir.join("clip.flv");
    std::fs::write(&unsupported, b"rejected by extension, never probed").unwrap();
    let paths = strings(&[good, dir.join("missing.mp4"), unsupported]);
    let listing_before = listing(&dir);

    let direct = scan_request(&paths, |_, _, _| {}).unwrap();

    let (tx, rx) = mpsc::channel();
    let worker_paths = paths.clone();
    let on_worker = block_on(run_blocking(SCAN_INTERNAL_ERROR, move || {
        scan_request(&worker_paths, |index, total, view| {
            tx.send((index, total, json(view))).unwrap();
        })
    }))
    .unwrap();
    let reported: Vec<(usize, usize, Value)> = rx.into_iter().collect();

    // Classifier and inspection output are unchanged by where they ran.
    assert_eq!(json(&on_worker), json(&direct));

    // One report per file, in input order, each identical to the returned view.
    assert_eq!(reported.len(), paths.len());
    for (position, (index, total, view)) in reported.iter().enumerate() {
        assert_eq!((*index, *total), (position, paths.len()));
        assert_eq!(view, &json(&on_worker[position]));
        assert_eq!(view["path"], paths[position].as_str());
    }
    let ok: Vec<bool> = on_worker.iter().map(|v| v.ok).collect();
    assert_eq!(ok, [true, false, false]);
    assert_eq!(
        on_worker[1].error.as_deref(),
        Some("File is no longer available")
    );

    // A scan writes nothing.
    assert_eq!(listing(&dir), listing_before);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn listing(dir: &Path) -> Vec<(String, u64)> {
    let mut entries: Vec<(String, u64)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let len = e.metadata().map(|m| m.len()).unwrap_or(0);
            (e.file_name().to_string_lossy().into_owned(), len)
        })
        .collect();
    entries.sort();
    entries
}

/// A batch folder with its own app data dir and output folder, as the app has.
struct BatchDirs {
    root: PathBuf,
    app: PathBuf,
    out: PathBuf,
}

fn batch_dirs(name: &str, prefix: &str) -> BatchDirs {
    let root = scratch(name);
    let app = root.join("app");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    write_settings(
        &app,
        &Settings {
            prefix: prefix.into(),
            output_directory: out.to_string_lossy().into_owned(),
        },
    )
    .unwrap();
    BatchDirs { root, app, out }
}

/// What the frontend can observe of one progress event, plus the thread that
/// emitted it and what the output folder held at that moment.
#[derive(Debug)]
struct Seen {
    index: usize,
    total: usize,
    status: &'static str,
    output_name: Option<String>,
    verified: Option<bool>,
    thread: ThreadId,
    on_disk: Vec<String>,
}

/// The output of a summary with the parts that differ between two runs of the
/// same input (random IDs, output folder) blanked out.
fn comparable(summary: &Value) -> Value {
    let mut summary = summary.clone();
    summary["outputDir"] = Value::Null;
    for result in summary["results"].as_array_mut().unwrap() {
        if !result["outputName"].is_null() {
            result["outputName"] = Value::from("<id>");
        }
    }
    summary
}

#[test]
fn a_clean_on_the_worker_is_sequential_ordered_verified_and_unchanged() {
    let direct_dirs = batch_dirs("offthread-clean-direct", "CLIP");
    let worker_dirs = batch_dirs("offthread-clean-worker", "CLIP");
    let inputs_dir = direct_dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs_dir).unwrap();
    let broken = inputs_dir.join("broken.mp4");
    std::fs::write(&broken, b"this is not a video").unwrap();
    let inputs = vec![
        sample_video(&inputs_dir, "first.mp4"),
        broken,
        sample_video(&inputs_dir, "third.mp4"),
    ];
    let originals: Vec<Vec<u8>> = inputs.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let paths = strings(&inputs);

    // The same batch, once in the calling thread and once through the boundary.
    // A flag of its own: tests run in parallel and must not share the app's.
    static BUSY: AtomicBool = AtomicBool::new(false);
    let direct = clean_request(&BUSY, &direct_dirs.app, &paths, |_| {}).unwrap();

    let caller = thread::current().id();
    let (tx, rx) = mpsc::channel();
    let (app, worker_paths) = (worker_dirs.app.clone(), paths.clone());
    let worker_out = worker_dirs.out.clone();
    let on_worker = block_on(run_blocking(CLEAN_INTERNAL_ERROR, move || {
        clean_request(&BUSY, &app, &worker_paths, |progress| {
            tx.send(Seen {
                index: progress.index,
                total: progress.total,
                status: progress.status,
                output_name: progress.output_name.clone(),
                verified: progress.verification.as_ref().map(|v| v.verified),
                thread: thread::current().id(),
                on_disk: listing(&worker_out).into_iter().map(|(n, _)| n).collect(),
            })
            .unwrap();
        })
    }))
    .unwrap();
    let seen: Vec<Seen> = rx.into_iter().collect();

    // Same result, check for check: moving threads changed nothing it reports.
    assert_eq!(comparable(&json(&on_worker)), comparable(&json(&direct)));
    assert_eq!(
        (on_worker.completed, on_worker.errors, on_worker.verified),
        (2, 1, 2)
    );
    assert_eq!(on_worker.verification_failures, 0);

    // Exactly the events the batch always emitted, one file at a time: a file
    // starts only after the previous one has finished, and the bad file in the
    // middle does not stop the one after it.
    let order: Vec<(usize, &str)> = seen.iter().map(|s| (s.index, s.status)).collect();
    assert_eq!(
        order,
        [
            (0, "processing"),
            (0, "completed"),
            (1, "processing"),
            (1, "error"),
            (2, "processing"),
            (2, "completed"),
        ]
    );
    for event in &seen {
        assert_eq!(event.total, 3);
        match event.status {
            "completed" => assert!(event.output_name.is_some() && event.verified == Some(true)),
            _ => assert!(event.output_name.is_none() && event.verified.is_none()),
        }
    }

    // Each event went out while the batch stood exactly where it says: nothing
    // of a file exists at its "processing", its finished output exists by its
    // "completed", and nothing is ever half-written in between. Events gathered
    // up and sent after the loop, or files cleaned side by side, would not line
    // up with the folder like this.
    let mut finished: Vec<String> = Vec::new();
    for event in &seen {
        finished.extend(event.output_name.clone());
        let mut expected = finished.clone();
        expected.sort();
        assert_eq!(
            event.on_disk,
            expected,
            "output folder at {:?}",
            (event.index, event.status)
        );
    }
    // Every terminal event agrees with the summary row for the same file.
    let terminal: Vec<&str> = seen
        .iter()
        .filter(|s| s.status != "processing")
        .map(|s| s.status)
        .collect();
    let results: Vec<&str> = on_worker.results.iter().map(|r| r.status).collect();
    assert_eq!(terminal, results);

    // One worker thread ran the whole batch, and it was not the caller's.
    assert!(seen.iter().all(|s| s.thread == seen[0].thread));
    assert_ne!(seen[0].thread, caller);

    // Two finished files, nothing half-written, originals byte-identical.
    for dirs in [&direct_dirs, &worker_dirs] {
        let names: Vec<String> = listing(&dirs.out).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|n| n.starts_with("CLIP_")), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with(TEMP_PREFIX)));
    }
    for (path, original) in inputs.iter().zip(&originals) {
        assert_eq!(&std::fs::read(path).unwrap(), original, "{path:?} changed");
    }

    std::fs::remove_dir_all(&direct_dirs.root).unwrap();
    std::fs::remove_dir_all(&worker_dirs.root).unwrap();
}

#[test]
fn clean_request_without_an_output_folder_touches_nothing() {
    static BUSY: AtomicBool = AtomicBool::new(false);
    let root = scratch("offthread-no-folder");
    let mut called = false;
    let error = clean_request(&BUSY, &root, &["whatever.mp4".to_string()], |_| {
        called = true
    })
    .expect_err("a batch ran with no output folder");
    assert_eq!(error, "Choose an output folder first.");
    assert!(
        !called,
        "progress was reported for a batch that never started"
    );
    assert!(!root.join("used-ids.txt").exists());
    // A batch that ends in an error still lets the next one start.
    assert!(BatchGuard::try_acquire(&BUSY).is_some());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_batch_guard_is_exclusive_reusable_and_released_by_a_panic() {
    static BUSY: AtomicBool = AtomicBool::new(false);

    let first = BatchGuard::try_acquire(&BUSY).expect("a free guard was refused");
    assert!(
        BatchGuard::try_acquire(&BUSY).is_none(),
        "two holders at once"
    );
    drop(first);
    drop(BatchGuard::try_acquire(&BUSY).expect("not released when dropped"));

    // A batch that panics on the worker still frees the guard on its way out.
    let result = block_on(run_blocking(
        CLEAN_INTERNAL_ERROR,
        || -> Result<(), String> {
            let _batch = BatchGuard::try_acquire(&BUSY).unwrap();
            panic!("mid-batch");
        },
    ));
    assert_eq!(result, Err(CLEAN_INTERNAL_ERROR.to_string()));
    assert!(
        BatchGuard::try_acquire(&BUSY).is_some(),
        "a panic left the guard set"
    );
}

#[test]
fn a_panic_inside_a_clean_batch_still_frees_the_guard() {
    static BUSY: AtomicBool = AtomicBool::new(false);
    let dirs = batch_dirs("offthread-guard-panic", "CLIP");
    // The file is never read: the panic comes from its "processing" event.
    let paths = vec![dirs
        .root
        .join("never-read.mp4")
        .to_string_lossy()
        .into_owned()];
    let app = dirs.app.clone();

    let result = block_on(run_blocking(CLEAN_INTERNAL_ERROR, move || {
        clean_request(&BUSY, &app, &paths, |_| panic!("mid-batch"))
    }));

    assert_eq!(result.unwrap_err(), CLEAN_INTERNAL_ERROR);
    assert!(
        BatchGuard::try_acquire(&BUSY).is_some(),
        "one panic left every later Clean refused until a restart"
    );
    std::fs::remove_dir_all(&dirs.root).unwrap();
}

/// The reload scenario: the page forgets a batch that is still running and asks
/// for another one in the same folder. The second request must be refused before
/// it touches anything, the first must finish verified, and the next batch after
/// it must run normally and never reuse an ID.
#[test]
fn a_second_clean_while_one_runs_is_refused_and_touches_nothing() {
    static BUSY: AtomicBool = AtomicBool::new(false);
    let dirs = batch_dirs("offthread-guard", "CLIP");
    let inputs_dir = dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs_dir).unwrap();
    let first_batch = strings(&[
        sample_video(&inputs_dir, "a.mp4"),
        sample_video(&inputs_dir, "b.mp4"),
    ]);
    let later_batch = strings(&[
        sample_video(&inputs_dir, "c.mp4"),
        sample_video(&inputs_dir, "d.mp4"),
    ]);

    // Start the first batch on the worker and hold it inside its first progress
    // event, i.e. after its own stale-temp sweep and before its first file.
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let (app, paths) = (dirs.app.clone(), first_batch.clone());
    let first = tauri::async_runtime::spawn(run_blocking(CLEAN_INTERNAL_ERROR, move || {
        let mut paused = false;
        clean_request(&BUSY, &app, &paths, |_| {
            if !paused {
                paused = true;
                started_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            }
        })
    }));
    started_rx.recv().expect("the first batch never started");

    // Really paused before its first file: its registry is open, but nothing
    // has been written and no ID has been reserved yet.
    let registry = dirs.app.join("used-ids.txt");
    assert!(listing(&dirs.out).is_empty());
    assert!(std::fs::read(&registry).unwrap().is_empty());

    // Stand-in for the first batch's half-written output, which is exactly what
    // a second batch's sweep would go after.
    let in_flight = dirs.out.join(format!("{TEMP_PREFIX}0000000001.mp4"));
    std::fs::write(&in_flight, b"being written by the first batch").unwrap();
    let registry_before = std::fs::read(&registry).unwrap();
    let out_before = listing(&dirs.out);

    let mut second_reported = false;
    let refused = clean_request(&BUSY, &dirs.app, &later_batch, |_| second_reported = true);
    assert_eq!(refused.unwrap_err(), CLEAN_BUSY);
    assert!(!second_reported, "the refused batch reported progress");
    assert!(
        in_flight.exists(),
        "the refused batch swept the output folder"
    );
    assert_eq!(
        std::fs::read(&registry).unwrap(),
        registry_before,
        "the refused batch reserved an ID"
    );
    assert_eq!(listing(&dirs.out), out_before);

    // Let the first batch finish. Its verification scans the folder for temp
    // files, so the stand-in has to go first.
    std::fs::remove_file(&in_flight).unwrap();
    resume_tx.send(()).unwrap();
    let first = block_on(first).unwrap().unwrap();
    assert_eq!((first.completed, first.verified, first.errors), (2, 2, 0));

    // Released on success: the next batch runs, in the same folder and registry.
    let later = clean_request(&BUSY, &dirs.app, &later_batch, |_| {}).unwrap();
    assert_eq!((later.completed, later.verified, later.errors), (2, 2, 0));

    let names: Vec<&String> = first
        .results
        .iter()
        .chain(&later.results)
        .map(|r| r.output_name.as_ref().unwrap())
        .collect();
    let unique: std::collections::HashSet<&&String> = names.iter().collect();
    assert_eq!(unique.len(), 4, "an ID was handed out twice: {names:?}");
    let recorded = std::fs::read_to_string(&registry).unwrap();
    for name in &names {
        let id = &name["CLIP_".len().."CLIP_".len() + 10];
        assert!(
            recorded.lines().any(|l| l.trim() == id),
            "{id} not recorded"
        );
    }
    let produced: Vec<String> = listing(&dirs.out).into_iter().map(|(n, _)| n).collect();
    assert_eq!(produced.len(), 4, "{produced:?}");
    assert!(!produced.iter().any(|n| n.starts_with(TEMP_PREFIX)));

    std::fs::remove_dir_all(&dirs.root).unwrap();
}
