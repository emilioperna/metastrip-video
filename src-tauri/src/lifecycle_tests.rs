//! What the app is allowed to do while a batch is running.
//!
//! Making the window responsive took away the thing that used to keep these
//! straight: with the work off the window's thread, the page can reload, the
//! updater can fire and the close button can be pressed with FFmpeg still
//! running. None of those may look at React state to decide, because a reloaded
//! page has forgotten the batch it started. They look here.
//!
//! These tests pin three things: the pipeline's own state is accurate at every
//! point of a batch, including the ones that end badly; every event of a batch
//! carries that batch's identity and no other's; and a normal close is refused
//! for exactly as long as a batch is running.
//!
//! No test here measures time. A batch is held still with a channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use tauri::async_runtime::block_on;

use crate::plan::CleaningOptions;
use crate::testkit::{sample_for_format, sample_video, scratch, stream_kinds};
use crate::{
    clean_request, clean_with, cleaning_blocks_close, run_blocking, store_cleaning_options,
    write_settings, BatchGuard, BatchState, CleanSummary, ProcessingState, Progress, Settings,
    CLEAN_BUSY, CLEAN_INTERNAL_ERROR, CLEAN_UPDATING,
};

/// A batch folder with its own app data dir and output folder, as the app has.
struct BatchDirs {
    root: PathBuf,
    app: PathBuf,
}

fn batch_dirs(name: &str) -> BatchDirs {
    let root = scratch(name);
    let app = root.join("app");
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    write_settings(
        &app,
        &Settings {
            prefix: "CLIP".into(),
            output_directory: out.to_string_lossy().into_owned(),
            ..Settings::default()
        },
    )
    .unwrap();
    BatchDirs { root, app }
}

fn idle(batch_id: u64) -> ProcessingState {
    ProcessingState {
        cleaning: false,
        installing: false,
        batch_id,
    }
}

fn running(batch_id: u64) -> ProcessingState {
    ProcessingState {
        cleaning: true,
        installing: false,
        batch_id,
    }
}

fn reserved(batch_id: u64) -> ProcessingState {
    ProcessingState {
        cleaning: false,
        installing: true,
        batch_id,
    }
}

fn paths_of(dir: &std::path::Path, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .map(|name| sample_video(dir, name).to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_fresh_process_is_idle_and_has_no_batch_to_name() {
    static STATE: BatchState = BatchState::new();
    assert_eq!(STATE.snapshot(), idle(0));
    assert!(!cleaning_blocks_close(&STATE));
}

#[test]
fn the_state_is_running_for_exactly_as_long_as_the_guard_is_held() {
    static STATE: BatchState = BatchState::new();

    let first = BatchGuard::try_acquire(&STATE).expect("a free pipeline was refused");
    assert_eq!(first.id, 1);
    assert_eq!(STATE.snapshot(), running(1));

    drop(first);
    // Released, and the counter stays put: batch 1 is spent, not reusable.
    assert_eq!(STATE.snapshot(), idle(1));

    let second = BatchGuard::try_acquire(&STATE).expect("not released when dropped");
    assert_eq!(
        second.id, 2,
        "a second batch reused the first one's identity"
    );
    assert_eq!(STATE.snapshot(), running(2));
    drop(second);
    assert_eq!(STATE.snapshot(), idle(2));
}

#[test]
fn a_refused_batch_leaves_the_running_one_named_correctly() {
    static STATE: BatchState = BatchState::new();
    let held = BatchGuard::try_acquire(&STATE).unwrap();

    // Several attempts while it runs. Each is refused, and none of them may
    // move the id on: a page reading the state has to be told the batch that is
    // really running, not one that never started.
    for _ in 0..3 {
        assert!(
            BatchGuard::try_acquire(&STATE).is_err(),
            "two batches at once"
        );
        assert_eq!(STATE.snapshot(), running(held.id));
    }

    drop(held);
    let next = BatchGuard::try_acquire(&STATE).unwrap();
    assert_eq!(next.id, 2, "refused attempts consumed identities");
}

#[test]
fn the_state_is_released_after_an_error_and_after_a_panic() {
    static AFTER_ERROR: BatchState = BatchState::new();
    let root = scratch("lifecycle-error");
    // No output folder configured, so the batch fails right after taking the
    // state and before it touches the disk.
    let error = clean_request(
        &AFTER_ERROR,
        &root,
        &["whatever.mp4".to_string()],
        CleaningOptions::default(),
        |_| {},
    )
    .expect_err("a batch ran with no output folder");
    assert_eq!(error, "Choose an output folder first.");
    assert_eq!(AFTER_ERROR.snapshot(), idle(1));
    assert!(!cleaning_blocks_close(&AFTER_ERROR));
    std::fs::remove_dir_all(&root).unwrap();

    static AFTER_PANIC: BatchState = BatchState::new();
    let dirs = batch_dirs("lifecycle-panic");
    let app = dirs.app.clone();
    // The file is never read: the panic comes from its "processing" event.
    let paths = vec![dirs
        .root
        .join("never-read.mp4")
        .to_string_lossy()
        .into_owned()];
    let result = block_on(run_blocking(CLEAN_INTERNAL_ERROR, move || {
        clean_request(
            &AFTER_PANIC,
            &app,
            &paths,
            CleaningOptions::default(),
            |_| panic!("mid-batch"),
        )
    }));

    assert_eq!(result.unwrap_err(), CLEAN_INTERNAL_ERROR);
    assert_eq!(
        AFTER_PANIC.snapshot(),
        idle(1),
        "one panic left the app looking busy for the rest of the session"
    );
    // Which is what the close button and the updater would have been told.
    assert!(!cleaning_blocks_close(&AFTER_PANIC));
    assert!(BatchGuard::try_acquire(&AFTER_PANIC).is_ok());
    std::fs::remove_dir_all(&dirs.root).unwrap();
}

/// The whole of one batch, watched from outside it: what the state says before,
/// at every event, and after; what the close button would do at each of those
/// points; and which batch every event claims to be.
#[test]
fn a_batch_names_itself_the_same_way_from_its_first_event_to_its_summary() {
    static STATE: BatchState = BatchState::new();
    let dirs = batch_dirs("lifecycle-identity");
    let inputs = dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let paths = paths_of(&inputs, &["a.mp4", "b.mp4"]);

    assert_eq!(STATE.snapshot(), idle(0));
    assert!(!cleaning_blocks_close(&STATE));

    /// One progress event, plus what the rest of the app would have been told
    /// at the moment it went out.
    struct Seen {
        batch_id: u64,
        index: usize,
        status: &'static str,
        state: ProcessingState,
        close_blocked: bool,
    }

    let mut seen: Vec<Seen> = Vec::new();
    let first = clean_request(
        &STATE,
        &dirs.app,
        &paths,
        CleaningOptions::default(),
        |progress: Progress| {
            seen.push(Seen {
                batch_id: progress.batch_id,
                index: progress.index,
                status: progress.status,
                state: STATE.snapshot(),
                close_blocked: cleaning_blocks_close(&STATE),
            });
        },
    )
    .unwrap();

    // Four events, in the order the pipeline has always emitted them, and every
    // one of them stamped with the batch the summary came back as.
    assert_eq!(first.batch_id, 1);
    let order: Vec<(usize, &str)> = seen.iter().map(|e| (e.index, e.status)).collect();
    assert_eq!(
        order,
        [
            (0, "processing"),
            (0, "completed"),
            (1, "processing"),
            (1, "completed"),
        ]
    );
    for event in &seen {
        assert_eq!(
            event.batch_id, first.batch_id,
            "an event named another batch"
        );
        // Mid-batch, the pipeline says so and the window will not close.
        assert_eq!(event.state, running(first.batch_id));
        assert!(event.close_blocked);
    }

    // And once it is over, both of those flip back.
    assert_eq!(STATE.snapshot(), idle(first.batch_id));
    assert!(!cleaning_blocks_close(&STATE));

    // A second batch in the same process is a different batch, and says so in
    // every event. This is what stops a page that reloaded mid-batch from
    // taking the old batch's events for its new one.
    let mut later_ids: Vec<u64> = Vec::new();
    let second = clean_request(
        &STATE,
        &dirs.app,
        &paths,
        CleaningOptions::default(),
        |progress: Progress| later_ids.push(progress.batch_id),
    )
    .unwrap();
    assert_eq!(second.batch_id, 2);
    assert_ne!(second.batch_id, first.batch_id);
    assert!(later_ids.iter().all(|id| *id == second.batch_id));

    std::fs::remove_dir_all(&dirs.root).unwrap();
}

/// The reload scenario end to end, from the point of view of everything that
/// has to ask the backend: a batch is running, a page that has forgotten it
/// asks for another one, the window is closed, and then the batch finishes.
#[test]
fn while_a_batch_runs_a_second_clean_is_refused_and_a_normal_close_waits() {
    static STATE: BatchState = BatchState::new();
    let dirs = batch_dirs("lifecycle-reload");
    let inputs = dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let first_batch = paths_of(&inputs, &["a.mp4"]);
    let later_batch = paths_of(&inputs, &["b.mp4"]);

    // Hold the first batch still inside its first progress event.
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let (app, paths) = (dirs.app.clone(), first_batch.clone());
    let running_batch =
        tauri::async_runtime::spawn(run_blocking(CLEAN_INTERNAL_ERROR, move || {
            let mut paused = false;
            clean_request(
                &STATE,
                &app,
                &paths,
                CleaningOptions::default(),
                |progress| {
                    if !paused {
                        paused = true;
                        started_tx.send(progress.batch_id).unwrap();
                        resume_rx.recv().unwrap();
                    }
                },
            )
        }));
    let live_id = started_rx.recv().expect("the first batch never started");

    // What a reloaded page, the updater and the close button each see. None of
    // them knows a batch was ever started from this process.
    assert_eq!(STATE.snapshot(), running(live_id));
    assert!(
        cleaning_blocks_close(&STATE),
        "the window would have closed on FFmpeg"
    );

    // A page that forgot the batch and asked for another one is refused, and
    // the refusal does not disturb what the running batch reports.
    let mut reported = false;
    let refused = clean_request(
        &STATE,
        &dirs.app,
        &later_batch,
        CleaningOptions::default(),
        |_| reported = true,
    );
    assert_eq!(refused.unwrap_err(), CLEAN_BUSY);
    assert!(!reported, "the refused batch reported progress");
    assert_eq!(STATE.snapshot(), running(live_id));

    resume_tx.send(()).unwrap();
    let finished = block_on(running_batch).unwrap().unwrap();
    assert_eq!(finished.batch_id, live_id);
    assert_eq!((finished.completed, finished.verified), (1, 1));

    // Finished, so the window closes and the next batch runs.
    assert_eq!(STATE.snapshot(), idle(live_id));
    assert!(!cleaning_blocks_close(&STATE));
    let later = clean_request(
        &STATE,
        &dirs.app,
        &later_batch,
        CleaningOptions::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(later.batch_id, live_id + 1);
    assert_eq!((later.completed, later.verified), (1, 1));

    std::fs::remove_dir_all(&dirs.root).unwrap();
}

/// What `get_processing_state` puts on the wire. The page needs three facts to
/// decide whether it may clean, install an update or close; anything more would
/// be something about the user files travelling for no reason.
#[test]
fn the_reported_state_is_three_internal_facts_and_nothing_else() {
    static STATE: BatchState = BatchState::new();
    let guard = BatchGuard::try_acquire(&STATE).unwrap();
    let wire = serde_json::to_value(STATE.snapshot()).unwrap();

    let fields: Vec<&String> = wire.as_object().unwrap().keys().collect();
    assert_eq!(fields, ["batchId", "cleaning", "installing"]);
    assert_eq!(
        wire,
        serde_json::json!({ "cleaning": true, "installing": false, "batchId": 1 })
    );

    drop(guard);
    assert!(STATE.try_reserve_install());
    assert_eq!(
        serde_json::to_value(STATE.snapshot()).unwrap(),
        serde_json::json!({ "cleaning": false, "installing": true, "batchId": 1 })
    );

    STATE.release_install();
    assert_eq!(
        serde_json::to_value(STATE.snapshot()).unwrap(),
        serde_json::json!({ "cleaning": false, "installing": false, "batchId": 1 })
    );
}

// --------------------------------- Cleaning against update installation ---
//
// Asking whether a batch is running and then installing is two steps, and a
// Clean accepted between them would have been cleaning by the time the
// installer ran. So installing claims the app through the same word cleaning
// does, and the claim is the check.

/// The updater finds the app free and takes it; a Clean asked for afterwards is
/// refused, and told why rather than being told a batch is running.
#[test]
fn an_update_that_has_claimed_the_app_shuts_cleaning_out() {
    static STATE: BatchState = BatchState::new();
    assert_eq!(STATE.snapshot(), idle(0));

    assert!(STATE.try_reserve_install(), "a free app was refused");
    assert_eq!(STATE.snapshot(), reserved(0));

    assert_eq!(BatchGuard::try_acquire(&STATE).err(), Some(CLEAN_UPDATING));
    // And the refusal spent nothing: the app is exactly as the updater left it.
    assert_eq!(STATE.snapshot(), reserved(0));

    // Nor may a second install claim what the first one holds.
    assert!(!STATE.try_reserve_install(), "two installers at once");
}

/// A batch is running, so the updater is refused and the installer is never
/// handed over. This is the reload case: the page believes nothing is running.
#[test]
fn an_update_cannot_claim_the_app_while_a_batch_runs() {
    static STATE: BatchState = BatchState::new();
    let batch = BatchGuard::try_acquire(&STATE).unwrap();

    for _ in 0..3 {
        assert!(!STATE.try_reserve_install(), "installed over a live batch");
        assert_eq!(STATE.snapshot(), running(batch.id));
    }

    drop(batch);
    // Once the batch is over the same request goes through.
    assert!(STATE.try_reserve_install());
    assert_eq!(STATE.snapshot(), reserved(1));
}

/// The interleaving the reservation exists for, run in the order that used to
/// break: the updater looks, a Clean starts, and only then does the updater
/// try to install. Under a plain re-check the install would already have been
/// approved; here the approval is the claim, so the Clean either loses or the
/// install does, never both.
#[test]
fn a_clean_that_starts_after_the_updater_looked_still_stops_the_install() {
    static STATE: BatchState = BatchState::new();

    // T0/T1: the updater looks and sees an idle app.
    assert!(!STATE.snapshot().cleaning);

    // T2/T3: a Clean is accepted and claims the app.
    let batch = BatchGuard::try_acquire(&STATE).unwrap();

    // T4: the updater goes to install. Under the old check-then-install it had
    // nothing left to consult; now the claim is what it asks for, and it fails.
    assert!(
        !STATE.try_reserve_install(),
        "the installer ran while a batch was cleaning"
    );

    drop(batch);
    assert!(STATE.try_reserve_install());
}

/// An install that never started gives the app back, and one released twice, or
/// released when nothing is reserved, leaves the counter alone.
#[test]
fn releasing_an_install_is_safe_however_often_it_happens() {
    static STATE: BatchState = BatchState::new();
    drop(BatchGuard::try_acquire(&STATE).unwrap());
    assert_eq!(STATE.snapshot(), idle(1));

    // The page calls this on load without knowing whether anything is reserved.
    STATE.release_install();
    assert_eq!(STATE.snapshot(), idle(1), "a release invented a change");

    assert!(STATE.try_reserve_install());
    STATE.release_install();
    STATE.release_install();
    assert_eq!(STATE.snapshot(), idle(1), "a second release took a batch");

    // And cleaning works normally afterwards.
    let batch = BatchGuard::try_acquire(&STATE).unwrap();
    assert_eq!(batch.id, 2);
}

/// A reservation is not a reason to keep the window open: the installer
/// restarts the app, and blocking close would be blocking that restart.
#[test]
fn a_reserved_install_does_not_hold_the_window_open() {
    static STATE: BatchState = BatchState::new();
    assert!(STATE.try_reserve_install());
    assert!(!cleaning_blocks_close(&STATE));

    STATE.release_install();
    assert!(!cleaning_blocks_close(&STATE));
}

// ------------------------------------ Cleaning against the close button ---

/// The close button reads the app as busy from the moment the Clean command
/// claims it, which is before any of the work is queued.
///
/// The command claims the app on the thread that runs it and moves the claim
/// into the closure it hands to the blocking pool. So the window between "this
/// Clean was accepted" and "the app says it is cleaning" contains no scheduling
/// at all. Here the claim is taken first and the work is then held still before
/// its first file, which is where the pool would have taken it under the old
/// order.
#[test]
fn the_close_button_sees_a_clean_before_its_work_is_queued() {
    static STATE: BatchState = BatchState::new();
    let dirs = batch_dirs("lifecycle-close-before-queue");
    let inputs = dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let paths = paths_of(&inputs, &["a.mp4"]);

    assert!(!cleaning_blocks_close(&STATE));

    // What the command does first, on its own thread.
    let batch = BatchGuard::try_acquire(&STATE).unwrap();

    // Already true, with nothing queued and no worker thread in existence.
    assert!(
        cleaning_blocks_close(&STATE),
        "a close between accepting a Clean and queueing it would have gone through"
    );
    assert_eq!(STATE.snapshot(), running(batch.id));
    // Nothing has been written, so there was nothing a close could have orphaned.
    assert!(!dirs.app.join("used-ids.txt").exists());

    // Now the work goes to the pool, exactly as the command hands it over.
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let app = dirs.app.clone();
    let running_batch =
        tauri::async_runtime::spawn(run_blocking(CLEAN_INTERNAL_ERROR, move || {
            let mut paused = false;
            clean_with(batch, &app, &paths, CleaningOptions::default(), |_| {
                if !paused {
                    paused = true;
                    started_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                }
            })
        }));
    started_rx.recv().expect("the work never reached the pool");
    assert!(cleaning_blocks_close(&STATE));

    resume_tx.send(()).unwrap();
    let finished = block_on(running_batch).unwrap().unwrap();
    assert_eq!((finished.batch_id, finished.completed), (1, 1));

    // Over, so the window closes immediately.
    assert!(!cleaning_blocks_close(&STATE));
    assert_eq!(STATE.snapshot(), idle(1));

    std::fs::remove_dir_all(&dirs.root).unwrap();
}

/// The pipeline cannot be entered without the claim: `clean_with` takes the
/// guard by value, so there is no call that starts FFmpeg with the app still
/// reading as idle. This compiles only while that stays true.
#[test]
fn the_pipeline_can_only_be_entered_with_the_app_claimed() {
    fn assert_takes_the_guard<F>(_: F)
    where
        F: FnOnce(
            BatchGuard,
            &Path,
            &[String],
            CleaningOptions,
            fn(Progress),
        ) -> Result<CleanSummary, String>,
    {
    }
    assert_takes_the_guard(clean_with);
}

/// The options are the ones the click carried, for every file of the batch.
/// The stored preference is changed -- both before the batch and while its
/// first file is being cleaned -- and neither change reaches the batch.
#[test]
fn a_batch_runs_with_the_options_it_was_handed_from_first_file_to_last() {
    static STATE: BatchState = BatchState::new();
    let dirs = batch_dirs("lifecycle-options");
    let inputs = dirs.root.join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let first_dir = inputs.join("first");
    let second_dir = inputs.join("second");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    let paths: Vec<String> = [&first_dir, &second_dir]
        .iter()
        .map(|dir| sample_for_format(dir, "mkv").to_string_lossy().into_owned())
        .collect();

    // Stored: keep subtitles. Clicked: remove them.
    store_cleaning_options(&dirs.app, CleaningOptions::default()).unwrap();
    let clicked = CleaningOptions {
        remove_subtitles: true,
    };
    let mut flipped = false;
    let summary = clean_request(&STATE, &dirs.app, &paths, clicked, |progress: Progress| {
        if !flipped && progress.status == "processing" {
            // The user flips the stored preference mid-batch.
            store_cleaning_options(&dirs.app, CleaningOptions::default()).unwrap();
            flipped = true;
        }
    })
    .unwrap();

    assert!(flipped);
    assert_eq!(
        summary.options, clicked,
        "the summary does not echo what ran"
    );
    assert_eq!(summary.completed, 2);
    assert_eq!(summary.verified, 2, "{:?}", summary.results);
    assert_eq!(summary.subtitle_streams_removed, 2);
    for result in &summary.results {
        let output = dirs
            .root
            .join("out")
            .join(result.output_name.as_ref().unwrap());
        let kinds = stream_kinds(&output);
        assert!(
            !kinds.iter().any(|kind| kind == "Subtitle"),
            "{} kept its subtitle: {kinds:?}",
            result.input_name
        );
    }
    std::fs::remove_dir_all(&dirs.root).unwrap();
}
