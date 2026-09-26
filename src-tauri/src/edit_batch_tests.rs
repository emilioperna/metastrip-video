//! The Edit batch against real files and the pinned FFmpeg: what a file costs
//! at each stage, what reaches the output folder, and what the page is told.
//!
//! The planner's own matrix lives in `edit_plan_tests.rs`; these tests take a
//! representative file for each way a file can end and follow it through the
//! production runner. FFmpeg failures a real file cannot produce on demand are
//! reached through `edit_one`'s launcher argument, which in each case still
//! runs the real FFmpeg on the planner's real arguments.

use super::*;
use std::collections::BTreeMap;
use std::time::SystemTime;

use serde_json::Value;

use crate::edit::{EditRequest, EditableField, MetadataEdit};
use crate::edit_plan::EditRefusal;
use crate::inspect::MetadataScope;
use crate::testkit::{
    assert_ffmpeg_can_read, cover_jpeg, sample_for_format, sample_plain_avi, sample_video,
    sample_with_cover, scratch, stream_payload_hash,
};
use crate::{format_profile, remove_stale_temp_files, TEMP_PREFIX};

// ---------------------------------------------------------------- Helpers ---

/// Values no fixture carries, so finding one anywhere is proof of where it
/// came from.
const SET_CANARY: &str = "EDIT_SET_CANARY_7Q";
const FILL_CANARY: &str = "EDIT_FILL_CANARY_9Z";

fn set(field: EditableField, value: &str) -> MetadataEdit {
    MetadataEdit {
        field,
        operation: EditOperation::Set(value.into()),
    }
}

fn fill(field: EditableField, value: &str) -> MetadataEdit {
    MetadataEdit {
        field,
        operation: EditOperation::FillIfMissing(value.into()),
    }
}

fn remove(field: EditableField) -> MetadataEdit {
    MetadataEdit {
        field,
        operation: EditOperation::Remove,
    }
}

fn request(paths: &[&Path], edits: Vec<MetadataEdit>) -> ValidEditRequest {
    EditRequest {
        paths: paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        edits,
    }
    .validate()
    .unwrap()
}

/// Title set, genre filled where missing, comment removed: every operation.
fn the_edit() -> Vec<MetadataEdit> {
    vec![
        set(EditableField::Title, SET_CANARY),
        remove(EditableField::Comment),
        fill(EditableField::Genre, FILL_CANARY),
    ]
}

/// How many IDs the registry at `path` has ever handed out.
fn ids_spent(registry: &Path) -> usize {
    std::fs::read_to_string(registry)
        .map(|text| text.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn temp_files(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|name| name.starts_with(TEMP_PREFIX))
        .collect()
}

/// Container-level metadata as the inspector reads it back.
fn globals(path: &Path) -> BTreeMap<String, String> {
    inspect::inspect(path)
        .unwrap()
        .fields
        .into_iter()
        .filter(|f| f.scope == MetadataScope::Format)
        .map(|f| (f.key, f.value))
        .collect()
}

/// Bytes, size and modification time: everything an edit must leave alone.
struct Original {
    path: PathBuf,
    bytes: Vec<u8>,
    modified: SystemTime,
}

impl Original {
    fn of(path: &Path) -> Self {
        Original {
            path: path.to_path_buf(),
            bytes: std::fs::read(path).unwrap(),
            modified: std::fs::metadata(path).unwrap().modified().unwrap(),
        }
    }

    fn assert_untouched(&self) {
        let now = std::fs::read(&self.path).unwrap();
        assert_eq!(now.len(), self.bytes.len(), "{:?} changed size", self.path);
        assert!(now == self.bytes, "{:?} changed", self.path);
        assert_eq!(
            std::fs::metadata(&self.path).unwrap().modified().unwrap(),
            self.modified,
            "{:?} was written to",
            self.path
        );
    }
}

/// Every string anywhere in a JSON value.
fn strings_in(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().flat_map(strings_in).collect(),
        Value::Object(map) => map
            .iter()
            .flat_map(|(k, v)| std::iter::once(k.clone()).chain(strings_in(v)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Nothing the page receives may carry a path, a requested value or a claim
/// of verification.
fn assert_safe_for_the_page(value: &Value, root: &Path) {
    let root = root.to_string_lossy();
    for text in strings_in(value) {
        assert!(!text.contains(root.as_ref()), "a path leaked: {text}");
        assert!(!text.contains('\\'), "a path leaked: {text}");
        assert!(!text.contains(TEMP_PREFIX), "a temp name leaked: {text}");
        for canary in [SET_CANARY, FILL_CANARY] {
            assert!(!text.contains(canary), "a requested value leaked: {text}");
        }
        assert!(
            !text.to_ascii_lowercase().contains("verif"),
            "Task 3 claimed verification: {text}"
        );
    }
}

// --------------------------------------------------------------- Fixtures ---

/// An iPhone-style MP4: `mdta` keys the MP4 muxer would silently drop.
fn mdta_mp4(dir: &Path) -> PathBuf {
    build(
        dir.join("iphone.mp4"),
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=1",
            "-c:v",
            "mpeg4",
            "-metadata",
            "title=T",
            "-metadata",
            "com.apple.quicktime.make=Apple",
            "-movflags",
            "use_metadata_tags",
            "-f",
            "mp4",
        ],
    )
}

/// Two camera angles and a timecode: FFmpeg writes a `tmcd` track per video
/// stream, and the edit would rebuild only one.
fn two_angle_timecode_mp4(dir: &Path) -> PathBuf {
    build(
        dir.join("timecode.mp4"),
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10:duration=1",
            "-map",
            "0:v",
            "-map",
            "1:v",
            "-c:v",
            "mpeg4",
            "-timecode",
            "01:00:00:00",
            "-f",
            "mp4",
        ],
    )
}

/// An MP4 whose track name, written back on every edit, is too long for a
/// Windows command line together with the rest of the command.
fn long_track_name_mp4(dir: &Path) -> PathBuf {
    let name = format!("title={}", "N".repeat(16_000));
    build(
        dir.join("long-name.mp4"),
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=1",
            "-c:v",
            "mpeg4",
            "-metadata:s:v:0",
            &name,
            "-f",
            "mp4",
        ],
    )
}

/// A valid Matroska file of MPEG-4 and AAC under a `.webm` name: it inspects
/// and plans, and the WebM muxer then refuses its codecs. A real FFmpeg
/// failure after the ID is reserved, and one that must never be "fixed" by
/// re-encoding.
fn incompatible_webm(dir: &Path) -> PathBuf {
    let base = sample_video(dir, "base-for-webm.mp4");
    let path = dir.join("incompatible.webm");
    let built = ffmpeg()
        .args(["-y", "-i"])
        .arg(&base)
        .args(["-c", "copy", "-f", "matroska"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}

fn build(path: PathBuf, args: &[&str]) -> PathBuf {
    let built = ffmpeg().arg("-y").args(args).arg(&path).output().unwrap();
    assert!(
        built.status.success(),
        "could not build {path:?}: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}

/// Each fixture in a folder of its own, so their helper files never collide.
fn fixture_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// -------------------------------------------------------- Pre-ID refusals ---

/// Every way a file can be turned away before it costs anything: each one is
/// skipped with its own safe reason, the registry does not grow, and nothing
/// reaches the output folder.
#[test]
fn a_file_refused_before_reservation_is_skipped_and_spends_no_id() {
    let root = scratch("edit-batch-refusals");
    let out = fixture_dir(&root, "out");
    let app = fixture_dir(&root, "app");

    let missing = root.join("gone.mp4");
    let unsupported = root.join("notes.txt");
    std::fs::write(&unsupported, b"not a video").unwrap();
    let broken = root.join("broken.mp4");
    std::fs::write(&broken, b"this is not a video").unwrap();
    let mdta = mdta_mp4(&fixture_dir(&root, "mdta"));
    let cover_dir = fixture_dir(&root, "cover");
    let mov_cover = sample_with_cover(&cover_dir, "mov", &cover_jpeg(&cover_dir, false));
    let avi = sample_for_format(&fixture_dir(&root, "avi"), "avi");
    let timecode = two_angle_timecode_mp4(&fixture_dir(&root, "timecode"));
    let long_name = long_track_name_mp4(&fixture_dir(&root, "long"));

    let inputs = [
        &missing,
        &unsupported,
        &broken,
        &mdta,
        &mov_cover,
        &avi,
        &timecode,
        &long_name,
    ];
    let originals: Vec<Original> = inputs
        .iter()
        .filter(|p| p.exists())
        .map(|p| Original::of(p))
        .collect();
    let paths: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();

    let summary =
        run_edit_batch(&request(&paths, the_edit()), "CLIP", &out, &app, 3, |_| {}).unwrap();

    let messages: Vec<(&str, Option<&str>)> = summary
        .results
        .iter()
        .map(|r| (r.status, r.message.as_deref()))
        .collect();
    let unpreserved = EditRefusal::UnpreservedMetadata(vec!["com.apple.quicktime.make".into()]);
    let tmcd = EditRefusal::DataTrack {
        codec_tag: Some("tmcd".into()),
    };
    assert_eq!(
        messages,
        [
            ("skipped", Some("File is no longer available")),
            (
                "skipped",
                Some("Unsupported file type. Supported: MP4, MOV, M4V, MKV, WebM, AVI.")
            ),
            ("skipped", Some(EDIT_NOT_INSPECTED)),
            ("skipped", Some(unpreserved.message().as_str())),
            ("skipped", Some(EditRefusal::CoverArt.message().as_str())),
            (
                "skipped",
                Some(EditRefusal::UnknownStream.message().as_str())
            ),
            ("skipped", Some(tmcd.message().as_str())),
            (
                "skipped",
                Some(EditRefusal::CommandLineTooLong.message().as_str())
            ),
        ]
    );
    assert_eq!(
        (summary.completed, summary.skipped, summary.errors),
        (0, 8, 0)
    );
    assert!(summary.results.iter().all(|r| r.output_name.is_none()));

    assert_eq!(
        ids_spent(&app.join("used-ids.txt")),
        0,
        "a refusal spent an ID"
    );
    assert!(listing(&out).is_empty(), "{:?}", listing(&out));
    for original in &originals {
        original.assert_untouched();
    }
    assert_safe_for_the_page(&serde_json::to_value(&summary.results).unwrap(), &root);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A requested field the file holds twice with different values. No FFmpeg
/// writes such a file on demand, so its inspection is the real one with the
/// conflicting value added; the path is the real file.
#[test]
fn an_ambiguous_requested_field_is_skipped_and_spends_no_id() {
    let root = scratch("edit-batch-ambiguous");
    let out = fixture_dir(&root, "out");
    let input = sample_for_format(&root, "mp4");
    let original = Original::of(&input);

    let mut report = inspect::inspect(&input).unwrap();
    let mut conflicting = report
        .fields
        .iter()
        .find(|f| f.scope == MetadataScope::Format && f.key == "title")
        .expect("the fixture has a title")
        .clone();
    conflicting.value = "A SECOND TITLE".into();
    report.fields.push(conflicting);
    let fresh = FreshInspection::from_report(&input, report);

    let registry_path = root.join("used-ids.txt");
    let mut registry = IdRegistry::open(&registry_path).unwrap();
    let mut launched = false;
    let outcome = edit_inspected(
        &fresh,
        format_profile("mp4").unwrap(),
        &request(&[&input], vec![set(EditableField::Title, SET_CANARY)]),
        &out,
        "CLIP",
        &mut registry,
        |_: &[String]| {
            launched = true;
            run_ffmpeg(&[])
        },
    );

    assert_eq!(
        outcome,
        EditOutcome::Skipped(EditRefusal::AmbiguousField(EditableField::Title).message())
    );
    assert!(!launched, "FFmpeg ran for a refused file");
    assert!(registry.used.is_empty());
    assert_eq!(ids_spent(&registry_path), 0);
    assert!(listing(&out).is_empty());
    original.assert_untouched();
    std::fs::remove_dir_all(&root).unwrap();
}

// ---------------------------------------------------------- Real execution ---

/// The production runner on an MP4, an MKV, an AVI and an upper-case `.MOV`:
/// one ID and one output each, the source extension spelled as it was, the
/// operations visible in the output, the streams copied, the originals
/// untouched and no temporary file left.
#[test]
fn real_edits_publish_one_output_per_file() {
    let root = scratch("edit-batch-success");
    let out = fixture_dir(&root, "out");
    let app = fixture_dir(&root, "app");

    let mp4 = sample_for_format(&fixture_dir(&root, "mp4"), "mp4");
    let mkv = sample_for_format(&fixture_dir(&root, "mkv"), "mkv");
    let avi = sample_plain_avi(&fixture_dir(&root, "avi"));
    let mov_dir = fixture_dir(&root, "mov");
    let mov = mov_dir.join("clip.MOV");
    std::fs::rename(sample_for_format(&mov_dir, "mov"), &mov).unwrap();

    let inputs = [&mp4, &mkv, &avi, &mov];
    let originals: Vec<Original> = inputs.iter().map(|p| Original::of(p)).collect();
    let paths: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();

    let summary =
        run_edit_batch(&request(&paths, the_edit()), "CLIP", &out, &app, 1, |_| {}).unwrap();

    assert_eq!(
        (summary.completed, summary.skipped, summary.errors),
        (4, 0, 0),
        "{:?}",
        summary.results
    );
    assert_eq!(ids_spent(&app.join("used-ids.txt")), 4);
    assert!(temp_files(&out).is_empty(), "{:?}", listing(&out));
    assert_eq!(listing(&out).len(), 4, "{:?}", listing(&out));

    for (input, result) in inputs.iter().zip(&summary.results) {
        assert_eq!(result.status, "completed");
        assert_eq!(result.message, None);
        let name = result.output_name.as_ref().unwrap();
        let extension = input.extension().unwrap().to_str().unwrap();
        let id = name
            .strip_prefix("CLIP_")
            .and_then(|rest| rest.strip_suffix(&format!(".{extension}")))
            .unwrap_or_else(|| panic!("{name} does not keep .{extension}"));
        assert!(id.len() == 10 && id.bytes().all(|b| b.is_ascii_digit()));

        let output = out.join(name);
        assert_ffmpeg_can_read(&output);
        let written = globals(&output);
        assert_eq!(written.get("title").map(String::as_str), Some(SET_CANARY));
        assert_eq!(written.get("genre").map(String::as_str), Some(FILL_CANARY));
        assert!(!written.contains_key("comment"), "{name}: {written:?}");
        for stream in ["0:v:0", "0:a:0"] {
            assert_eq!(
                stream_payload_hash(input, stream),
                stream_payload_hash(&output, stream),
                "{name}: {stream} was not copied"
            );
        }
    }
    assert!(summary.results[3]
        .output_name
        .as_ref()
        .unwrap()
        .ends_with(".MOV"));
    for original in &originals {
        original.assert_untouched();
    }
    std::fs::remove_dir_all(&root).unwrap();
}

/// Every operation resolves to Keep here: the title is present, so Fill keeps
/// it, and there is no genre to remove. The file is still written -- a new
/// copy, completed, one ID -- and not skipped.
#[test]
fn a_no_op_edit_still_writes_a_new_copy() {
    let root = scratch("edit-batch-noop");
    let out = fixture_dir(&root, "out");
    let app = fixture_dir(&root, "app");
    let input = sample_for_format(&root, "mp4");
    let original = Original::of(&input);
    let before = globals(&input);

    let summary = run_edit_batch(
        &request(
            &[&input],
            vec![
                fill(EditableField::Title, FILL_CANARY),
                remove(EditableField::Genre),
            ],
        ),
        "CLIP",
        &out,
        &app,
        1,
        |_| {},
    )
    .unwrap();

    assert_eq!((summary.completed, summary.skipped), (1, 0));
    let result = &summary.results[0];
    assert_eq!(
        (result.status, result.message.as_deref()),
        ("completed", None)
    );
    let output = out.join(result.output_name.as_ref().unwrap());
    assert_ffmpeg_can_read(&output);
    assert_eq!(
        globals(&output).get("title"),
        before.get("title"),
        "Fill replaced a title that was there"
    );
    assert!(!globals(&output).contains_key("genre"));
    assert_eq!(ids_spent(&app.join("used-ids.txt")), 1);
    assert!(temp_files(&out).is_empty());
    original.assert_untouched();
    std::fs::remove_dir_all(&root).unwrap();
}

// -------------------------------------------------- Attempts and failures ---

/// A run that dies part-way through: it leaves a partial file at the
/// temporary path and exits with a real failure status from the real FFmpeg,
/// with `stderr` standing in for what it printed.
fn dies_part_way(args: &[String], stderr: String) -> std::io::Result<Output> {
    std::fs::write(args.last().unwrap(), b"partial output").unwrap();
    let mut failed = run_ffmpeg(&["-no-such-option".to_string()])?;
    assert!(!failed.status.success());
    failed.stderr = stderr.into_bytes();
    Ok(failed)
}

/// The faststart attempt dies part-way. The partial output is gone before
/// the standard attempt runs, the standard attempt -- the real FFmpeg on the
/// planner's real arguments -- writes the same temporary path under the same
/// ID, and the file completes having spent one ID.
#[test]
fn a_failed_faststart_falls_back_under_the_same_id() {
    let root = scratch("edit-batch-fallback");
    let out = fixture_dir(&root, "out");
    let input = sample_for_format(&root, "mp4");
    let original = Original::of(&input);
    let registry_path = root.join("used-ids.txt");
    let mut registry = IdRegistry::open(&registry_path).unwrap();

    let mut attempts: Vec<Vec<String>> = Vec::new();
    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |args: &[String]| {
            let temp = PathBuf::from(args.last().unwrap());
            assert!(
                !temp.exists(),
                "attempt {} found the previous attempt's partial output",
                attempts.len() + 1
            );
            attempts.push(args.to_vec());
            if attempts.len() == 1 {
                dies_part_way(args, "Error writing trailer: I/O error\n".into())
            } else {
                run_ffmpeg(args)
            }
        },
    );

    let EditOutcome::Completed(name) = outcome else {
        panic!("the standard attempt did not complete the file: {outcome:?}");
    };
    assert_eq!(attempts.len(), 2);
    assert!(attempts[0].iter().any(|a| a == "+faststart"));
    assert!(!attempts[1].iter().any(|a| a == "+faststart"));
    assert_eq!(
        attempts[0].last(),
        attempts[1].last(),
        "the retry wrote somewhere else"
    );
    let id = &name["CLIP_".len().."CLIP_".len() + 10];
    assert_eq!(
        Path::new(attempts[1].last().unwrap()).file_name().unwrap(),
        format!("{TEMP_PREFIX}{id}.mp4").as_str(),
        "the retry used another ID"
    );
    assert_eq!(ids_spent(&registry_path), 1, "the retry spent a second ID");
    assert_eq!(listing(&out), std::slice::from_ref(&name));
    let output = out.join(&name);
    assert_ffmpeg_can_read(&output);
    assert_eq!(
        globals(&output).get("title").map(String::as_str),
        Some(SET_CANARY)
    );
    original.assert_untouched();
    std::fs::remove_dir_all(&root).unwrap();
}

/// Both attempts die part-way. The file is an error, its ID stays spent,
/// nothing is published under any name, no partial output remains and the
/// message says what failed without saying where.
#[test]
fn a_failed_execution_burns_one_id_and_publishes_nothing() {
    let root = scratch("edit-batch-failure");
    let out = fixture_dir(&root, "out");
    let input = sample_for_format(&root, "mp4");
    let original = Original::of(&input);
    let registry_path = root.join("used-ids.txt");
    let mut registry = IdRegistry::open(&registry_path).unwrap();

    let mut attempts = 0;
    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |args: &[String]| {
            attempts += 1;
            assert!(!Path::new(args.last().unwrap()).exists());
            dies_part_way(
                args,
                format!(
                    "[out#0/mp4 @ 000001d2c3] Error writing trailer of {}: No space left on device\nConversion failed!\n",
                    args.last().unwrap()
                ),
            )
        },
    );

    let EditOutcome::Failed(message) = outcome else {
        panic!("a failed run was not an error: {outcome:?}");
    };
    assert_eq!(attempts, 2, "ISO-BMFF gets exactly two attempts");
    assert_eq!(
        message,
        "Stream copy edit failed for this MP4 file: Error writing trailer of [output file]: No space left on device. No video or audio re-encoding was attempted."
    );
    assert_eq!(ids_spent(&registry_path), 1);
    assert!(listing(&out).is_empty(), "{:?}", listing(&out));
    original.assert_untouched();
    std::fs::remove_dir_all(&root).unwrap();
}

/// The pinned FFmpeg answers `-n` on an existing file by exiting with status
/// 0 without writing anything. So a partial output that cannot be removed --
/// held open by a scanner, say -- must stop the file: were the standard
/// attempt to run over it, its "success" would publish the partial file.
#[cfg(windows)]
#[test]
fn a_partial_output_that_cannot_be_removed_stops_the_file() {
    use std::os::windows::fs::OpenOptionsExt;

    // First, the FFmpeg behaviour this guards against, pinned.
    let root = scratch("edit-batch-held-partial");
    let occupied = root.join("occupied.mp4");
    std::fs::write(&occupied, b"partial output").unwrap();
    let input = sample_for_format(&root, "mp4");
    let refused = run_ffmpeg(&[
        "-n".into(),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-c".into(),
        "copy".into(),
        occupied.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert!(
        refused.status.success(),
        "FFmpeg now fails on -n; revisit this guard"
    );
    assert_eq!(std::fs::read(&occupied).unwrap(), b"partial output");

    let out = fixture_dir(&root, "out");
    let registry_path = root.join("used-ids.txt");
    let mut registry = IdRegistry::open(&registry_path).unwrap();
    let mut held = None;
    let mut runs = 0;
    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |args: &[String]| {
            runs += 1;
            let result = dies_part_way(args, "Error writing trailer\n".into());
            // Open without sharing delete, so the partial cannot be removed.
            held = Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(args.last().unwrap())
                    .unwrap(),
            );
            result
        },
    );

    assert_eq!(outcome, EditOutcome::Failed(EDIT_TEMP_IN_THE_WAY.into()));
    assert_eq!(runs, 1, "the standard attempt ran over a partial output");
    drop(held);
    assert_eq!(ids_spent(&registry_path), 1);
    // Only the partial is there, under its temporary name, and the next
    // batch's sweep clears it. Nothing was published.
    let left = listing(&out);
    assert_eq!(left.len(), 1);
    assert!(left[0].starts_with(TEMP_PREFIX), "{left:?}");
    remove_stale_temp_files(&out);
    assert!(listing(&out).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

/// FFmpeg that cannot be started at all fails the file after its ID, with the
/// app's own message.
#[test]
fn ffmpeg_that_cannot_start_is_an_error_after_the_id() {
    let root = scratch("edit-batch-no-start");
    let out = fixture_dir(&root, "out");
    let input = sample_for_format(&root, "mkv");
    let registry_path = root.join("used-ids.txt");
    let mut registry = IdRegistry::open(&registry_path).unwrap();

    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |_: &[String]| Err(std::io::ErrorKind::NotFound.into()),
    );

    assert_eq!(outcome, EditOutcome::Failed(FFMPEG_MISSING.into()));
    assert_eq!(ids_spent(&registry_path), 1);
    assert!(listing(&out).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

/// Something takes the final name while FFmpeg runs. The finished edit is not
/// renamed over it: that file keeps its bytes, the temporary output goes and
/// the file is an error.
#[test]
fn an_output_name_taken_mid_edit_is_never_overwritten() {
    let root = scratch("edit-batch-collision");
    let out = fixture_dir(&root, "out");
    let input = sample_for_format(&root, "mkv");
    let mut registry = IdRegistry::open(&root.join("used-ids.txt")).unwrap();

    let mut squatter = None;
    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |args: &[String]| {
            let result = run_ffmpeg(args);
            let temp = Path::new(args.last().unwrap());
            let id = &temp.file_name().unwrap().to_str().unwrap()[TEMP_PREFIX.len()..][..10];
            let taken = out.join(format!("CLIP_{id}.mkv"));
            std::fs::write(&taken, b"someone else's file").unwrap();
            squatter = Some(taken);
            result
        },
    );

    let EditOutcome::Failed(message) = outcome else {
        panic!("published over an existing file: {outcome:?}");
    };
    assert!(message.contains("Nothing was overwritten"), "{message}");
    let squatter = squatter.unwrap();
    assert_eq!(std::fs::read(&squatter).unwrap(), b"someone else's file");
    assert_eq!(
        listing(&out),
        [squatter.file_name().unwrap().to_string_lossy().into_owned()]
    );
    std::fs::remove_dir_all(&root).unwrap();
}

// ------------------------------------------------------------- Redaction ---

/// FFmpeg echoes its arguments. Whatever line is chosen, the paths and every
/// value on the command line come out as what they are, never as themselves.
#[test]
fn edit_errors_never_show_paths_or_values() {
    let input = Path::new(r"C:\Users\someone\Holiday\beach.mp4");
    let out_dir = Path::new(r"D:\Exports\Private");
    let temp = out_dir.join(format!("{TEMP_PREFIX}0123456789.mp4"));
    let stderr = format!(
        "[out#0/mp4 @ 000001d2c3] Error writing {} from {} into {} (title={SET_CANARY}, genre={FILL_CANARY}, name {}): Invalid argument\nConversion failed!\n",
        temp.display(),
        input.display(),
        out_dir.display(),
        temp.file_name().unwrap().to_string_lossy(),
    );

    let message = edit_ffmpeg_error(
        stderr.as_bytes(),
        input,
        &temp,
        out_dir,
        &[SET_CANARY, FILL_CANARY],
    );

    assert_eq!(
        message,
        "Error writing [output file] from [input file] into [output folder] (title=[value], genre=[value], name [output file]): Invalid argument"
    );
}

/// A value that is also a word of the message, or part of a placeholder, is
/// still replaced once and never inside a replacement.
#[test]
fn redaction_never_rewrites_its_own_placeholders() {
    let message = edit_ffmpeg_error(
        b"Error opening output file value: output\n",
        Path::new(r"C:\in.mp4"),
        Path::new(r"C:\out\t.mp4"),
        Path::new(r"C:\out"),
        &["output", "value", "file"],
    );
    assert_eq!(
        message, "Error opening [value] [value] [value]: [value]",
        "a placeholder was rewritten"
    );
    // Nothing to report is still not an empty message.
    assert_eq!(
        edit_ffmpeg_error(b"", Path::new("a"), Path::new("b"), Path::new("c"), &[]),
        "FFmpeg failed"
    );
}

/// The same guarantee through the runner: an FFmpeg failure that repeats
/// the paths, the requested values and a track name read from the file
/// reaches the result redacted.
#[test]
fn an_edit_failure_reaches_the_result_redacted() {
    let root = scratch("edit-batch-redaction");
    let out = fixture_dir(&root, "out");
    // The shared MP4 fixture carries a track name, written back on every edit.
    let input = sample_for_format(&root, "mp4");
    let mut registry = IdRegistry::open(&root.join("used-ids.txt")).unwrap();

    let outcome = edit_one(
        &input,
        &request(&[&input], the_edit()),
        &out,
        "CLIP",
        &mut registry,
        |args: &[String]| {
            // A real failed exit status, with the stderr FFmpeg might write.
            let mut failed = run_ffmpeg(&["-no-such-option".to_string()])?;
            assert!(!failed.status.success());
            failed.stderr = format!(
                "Could not write {} for {}: {}\n",
                args.last().unwrap(),
                input.display(),
                args.join(" ")
            )
            .into_bytes();
            Ok(failed)
        },
    );

    let EditOutcome::Failed(message) = outcome else {
        panic!("{outcome:?}");
    };
    for leaked in [
        root.to_string_lossy().as_ref(),
        TEMP_PREFIX,
        SET_CANARY,
        FILL_CANARY,
        "STREAM_SECRET",
    ] {
        assert!(!message.contains(leaked), "{leaked} leaked: {message}");
    }
    assert!(message.contains("title=[value]"), "{message}");
    assert!(message.contains("Could not write [output file] for [input file]"));
    assert!(listing(&out).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

// ------------------------------------------------------------------ Batch ---

/// The scenario the app runs: a batch where one file completes, one is
/// refused, one fails in FFmpeg and one completes. Files run one after
/// another, each one's events bracket its work, no file stops the next, and
/// only the accepted files spend IDs.
#[test]
fn a_mixed_batch_reports_in_order_and_isolates_each_file() {
    let root = scratch("edit-batch-mixed");
    let out = fixture_dir(&root, "out");
    let app = fixture_dir(&root, "app");
    let mp4 = sample_for_format(&fixture_dir(&root, "mp4"), "mp4");
    let cover_dir = fixture_dir(&root, "cover");
    let refused = sample_with_cover(&cover_dir, "mov", &cover_jpeg(&cover_dir, false));
    let failing = incompatible_webm(&fixture_dir(&root, "webm"));
    let mkv = sample_for_format(&fixture_dir(&root, "mkv"), "mkv");
    let inputs = [&mp4, &refused, &failing, &mkv];
    let originals: Vec<Original> = inputs.iter().map(|p| Original::of(p)).collect();
    let paths: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();

    // A stale temporary file from a batch that was killed: cleared once, at
    // the start. A file without the exact prefix is the user's and stays.
    std::fs::write(out.join(format!("{TEMP_PREFIX}0000000001.mp4")), b"stale").unwrap();
    std::fs::write(out.join("mine.mp4"), b"user file").unwrap();

    let mut events: Vec<EditProgress> = Vec::new();
    let summary = run_edit_batch(
        &request(&paths, the_edit()),
        "CLIP",
        &out,
        &app,
        42,
        |progress| {
            // The file this event is about has, at "processing", not been
            // started: nothing of it is in the folder yet.
            if progress.status == "processing" {
                assert_eq!(
                    listing(&out).len(),
                    1 + events.iter().filter(|e| e.status == "completed").count(),
                    "work began before its processing event"
                );
            }
            events.push(progress);
        },
    )
    .unwrap();

    let order: Vec<(usize, &str)> = events.iter().map(|e| (e.index, e.status)).collect();
    assert_eq!(
        order,
        [
            (0, "processing"),
            (0, "completed"),
            (1, "processing"),
            (1, "skipped"),
            (2, "processing"),
            (2, "error"),
            (3, "processing"),
            (3, "completed"),
        ]
    );
    let names = [
        "fixture.mp4",
        "fixture-cover.mov",
        "incompatible.webm",
        "fixture.mkv",
    ];
    for event in &events {
        assert_eq!((event.batch_id, event.total), (42, 4));
        assert_eq!(event.input_name, names[event.index]);
        match event.status {
            "processing" => {
                assert_eq!((&event.output_name, &event.message), (&None, &None))
            }
            "completed" => assert!(event.output_name.is_some() && event.message.is_none()),
            _ => assert!(event.output_name.is_none() && event.message.is_some()),
        }
    }
    // The final events are the results, one for one.
    let finals: Vec<&EditProgress> = events.iter().filter(|e| e.status != "processing").collect();
    for (event, result) in finals.iter().zip(&summary.results) {
        assert_eq!(
            (
                &event.input_name,
                &event.output_name,
                event.status,
                &event.message
            ),
            (
                &result.input_name,
                &result.output_name,
                result.status,
                &result.message
            )
        );
    }

    assert_eq!(summary.batch_id, 42);
    assert_eq!(summary.output_dir, out.to_string_lossy());
    assert_eq!(
        (summary.completed, summary.skipped, summary.errors),
        (2, 1, 1)
    );
    assert_eq!(
        summary.results[1].message.as_deref(),
        Some(EditRefusal::CoverArt.message().as_str())
    );
    let error = summary.results[2].message.as_deref().unwrap();
    assert!(
        error.starts_with("Stream copy edit failed for this WebM file: ")
            && error.contains("No video or audio re-encoding was attempted"),
        "{error}"
    );

    // Two completed and one failed: three IDs. The refusal spent none.
    assert_eq!(ids_spent(&app.join("used-ids.txt")), 3);
    let mut expected: Vec<String> = [&summary.results[0], &summary.results[3]]
        .iter()
        .map(|r| r.output_name.clone().unwrap())
        .chain(["mine.mp4".to_string()])
        .collect();
    expected.sort();
    assert_eq!(
        listing(&out),
        expected,
        "stale temp, partial or extra output"
    );
    for original in &originals {
        original.assert_untouched();
    }

    for event in &events {
        assert_safe_for_the_page(&serde_json::to_value(event).unwrap(), &root);
    }
    assert_safe_for_the_page(&serde_json::to_value(&summary.results).unwrap(), &root);
    std::fs::remove_dir_all(&root).unwrap();
}

/// What goes on the wire: exactly these fields, and no verification.
#[test]
fn the_progress_and_summary_payloads_are_compact() {
    let progress = serde_json::to_value(EditProgress {
        batch_id: 1,
        index: 0,
        total: 1,
        input_name: "a.mp4".into(),
        output_name: None,
        status: "processing",
        message: None,
    })
    .unwrap();
    let keys =
        |value: &Value| -> Vec<String> { value.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&progress),
        [
            "batchId",
            "index",
            "inputName",
            "message",
            "outputName",
            "status",
            "total"
        ]
    );

    let summary = serde_json::to_value(EditSummary {
        batch_id: 1,
        output_dir: "out".into(),
        completed: 1,
        skipped: 0,
        errors: 0,
        results: vec![EditFileResult {
            input_name: "a.mp4".into(),
            output_name: Some("CLIP_0000000001.mp4".into()),
            status: "completed",
            message: None,
        }],
    })
    .unwrap();
    assert_eq!(
        keys(&summary),
        [
            "batchId",
            "completed",
            "errors",
            "outputDir",
            "results",
            "skipped"
        ]
    );
    assert_eq!(
        keys(&summary["results"][0]),
        ["inputName", "message", "outputName", "status"]
    );
}

/// Batch-level preconditions fail the whole batch before any file is touched
/// or any ID spent.
#[test]
fn a_batch_without_its_output_folder_touches_nothing() {
    let root = scratch("edit-batch-no-folder");
    let app = fixture_dir(&root, "app");
    let input = sample_video(&root, "a.mp4");
    let original = Original::of(&input);
    let mut reported = false;

    let error = run_edit_batch(
        &request(&[&input], the_edit()),
        "CLIP",
        &root.join("deleted"),
        &app,
        1,
        |_| reported = true,
    )
    .unwrap_err();

    assert_eq!(
        error,
        "The output folder no longer exists. Choose a new one."
    );
    assert!(!reported);
    assert!(!app.join("used-ids.txt").exists());
    original.assert_untouched();
    std::fs::remove_dir_all(&root).unwrap();
}
