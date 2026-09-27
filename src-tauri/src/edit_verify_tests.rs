//! The Edit verifier, against real edits and against outputs that are wrong.
//!
//! Three kinds of test:
//!
//! * real edits through the production path (`edit_one`: fresh inspection,
//!   plan, fingerprint, ID, the pinned FFmpeg, rename, verify) for every
//!   format and every structure Edit accepts, each of which must verify;
//! * the same path with the planner's arguments altered on the way to FFmpeg,
//!   so the real muxer writes an output that breaks exactly one promise, which
//!   the matching check must catch;
//! * synthetic inspections, for wrong outputs no container can be made to
//!   produce on demand (a key reported twice, a telemetry track, a cover that
//!   lost its flag).

use super::*;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::edit::{EditOperation, EditRequest, MetadataEdit, ValidEditRequest};
use crate::edit_batch::{edit_one, run_ffmpeg, EditOutcome};
use crate::edit_plan::{edit_temp_path, plan_edit, FreshInspection};
use crate::inspect::parse_ffprobe_json;
use crate::privacy::PrivacyCategory;
use crate::testkit::{
    contains, cover_jpeg, sample_for_format, sample_plain_avi, sample_video, sample_with_cover,
    sample_with_timecode, scratch, stream_payload_hash, ATTACHMENT_CANARY_ALPHA,
    ATTACHMENT_CANARY_BETA, SUBTITLE_BODY,
};
use crate::{format_id, format_profile, IdRegistry, MuxAttempt};

// ---------------------------------------------------------------- Helpers ---

/// Values no fixture carries, so finding one is proof of where it came from.
const SET_CANARY: &str = "VERIFY_SET_CANARY_4K";
const FILL_CANARY: &str = "VERIFY_FILL_CANARY_8M";

const ALL_EXTENSIONS: [&str; 6] = ["mp4", "mov", "m4v", "mkv", "webm", "avi"];

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

fn request(path: &Path, edits: Vec<MetadataEdit>) -> ValidEditRequest {
    EditRequest {
        paths: vec![path.to_string_lossy().into_owned()],
        edits,
    }
    .validate()
    .unwrap()
}

/// Every operation, and both ways Fill resolves against the shared fixtures:
/// Title set, Comment removed, Genre (missing) filled, Copyright (present)
/// kept.
fn the_edit() -> Vec<MetadataEdit> {
    vec![
        set(EditableField::Title, SET_CANARY),
        remove(EditableField::Comment),
        fill(EditableField::Genre, FILL_CANARY),
        fill(EditableField::Copyright, FILL_CANARY),
    ]
}

/// The output of one real edit of `input`, run through the production path
/// with `run` as the FFmpeg launcher, and its verification.
fn edit_file(
    input: &Path,
    edits: Vec<MetadataEdit>,
    run: impl FnMut(&[String]) -> std::io::Result<Output>,
) -> (PathBuf, EditVerificationReport) {
    let dir = input.parent().unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
    match edit_one(
        input,
        &request(input, edits),
        &out,
        "CLIP",
        &mut registry,
        run,
    ) {
        EditOutcome::Completed(name, report) => (out.join(name), report),
        other => panic!("{input:?} did not complete: {other:?}"),
    }
}

fn failed(report: &EditVerificationReport) -> Vec<&'static str> {
    report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
        .collect()
}

fn check_of<'a>(report: &'a EditVerificationReport, name: &str) -> &'a VerificationCheck {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} check"))
}

/// All eleven checks, in order, every one passed.
fn assert_verified(report: &EditVerificationReport, what: &str) {
    let names: Vec<&str> = report.checks.iter().map(|c| c.name).collect();
    assert_eq!(names, EDIT_CHECKS, "{what}");
    assert!(
        report.verified && failed(report).is_empty(),
        "{what}: {:?}",
        report.checks
    );
    assert_eq!(report.failure_message(), None);
}

/// Not verified, and `name` is among the checks that said so.
fn assert_caught(report: &EditVerificationReport, name: &str) {
    assert!(!report.verified, "{name}: the output verified");
    assert!(
        failed(report).contains(&name),
        "{name} did not fail: {:?}",
        report.checks
    );
    let names: Vec<&str> = report.checks.iter().map(|c| c.name).collect();
    assert_eq!(names, EDIT_CHECKS);
}

fn assert_passed(report: &EditVerificationReport, name: &str) {
    assert!(
        check_of(report, name).passed,
        "{:?}",
        check_of(report, name)
    );
}

/// The launcher that runs the real FFmpeg on the planner's arguments after
/// `change` has altered them.
fn tampered(change: impl Fn(&mut Vec<String>)) -> impl FnMut(&[String]) -> std::io::Result<Output> {
    move |args: &[String]| {
        let mut args = args.to_vec();
        change(&mut args);
        run_ffmpeg(&args)
    }
}

/// Remove one adjacent pair of arguments, which must be there.
fn without(args: &mut Vec<String>, pair: [&str; 2]) {
    let at = args
        .windows(2)
        .position(|w| w[0] == pair[0] && w[1] == pair[1])
        .unwrap_or_else(|| panic!("{pair:?} not in {args:?}"));
    args.drain(at..at + 2);
}

/// Replace one adjacent pair of arguments, which must be there, with `with`.
fn replacing(args: &mut Vec<String>, pair: [&str; 2], with: &[&str]) {
    let at = args
        .windows(2)
        .position(|w| w[0] == pair[0] && w[1] == pair[1])
        .unwrap_or_else(|| panic!("{pair:?} not in {args:?}"));
    args.splice(at..at + 2, with.iter().map(|s| s.to_string()));
}

/// Add output options just before `-f <muxer> <output>`, where they apply to
/// the one output.
fn with_output_options(args: &mut Vec<String>, extra: &[&str]) {
    let at = args.len() - 3;
    assert_eq!(args[at], "-f", "{args:?}");
    args.splice(at..at, extra.iter().map(|s| s.to_string()));
}

/// Add a second input, after the file being edited, so it is input 1.
fn with_input(args: &mut Vec<String>, input: &[&str]) {
    let at = args.iter().position(|a| a == "-i").unwrap() + 2;
    args.splice(at..at, input.iter().map(|s| s.to_string()));
}

/// An ffmetadata file holding these chapters, each `(start ms, end ms,
/// title)`.
fn chapters_file(dir: &Path, chapters: &[(u32, u32, &str)]) -> PathBuf {
    let mut text = String::from(";FFMETADATA1\n");
    for (start, end, title) in chapters {
        text.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={start}\nEND={end}\ntitle={title}\n"
        ));
    }
    let path = dir.join("tamper-chapters.ffmetadata");
    std::fs::write(&path, text).unwrap();
    path
}

/// Bytes and modification time.
fn snapshot(path: &Path) -> (Vec<u8>, SystemTime) {
    (
        std::fs::read(path).unwrap(),
        std::fs::metadata(path).unwrap().modified().unwrap(),
    )
}

fn row(report: &EditVerificationReport, field: EditableField) -> &FieldChangeRow {
    report
        .changes
        .iter()
        .find(|r| r.field == field)
        .unwrap_or_else(|| panic!("no row for {field:?}"))
}

fn some(value: &str) -> Option<String> {
    Some(value.to_string())
}

/// The MKV fixture edited with `the_edit`, through FFmpeg arguments altered by
/// `change`: the common shape of the tamper tests.
fn tampered_mkv(name: &str, change: impl Fn(&mut Vec<String>)) -> EditVerificationReport {
    let root = scratch(name);
    let input = sample_for_format(&root, "mkv");
    let (_, report) = edit_file(&input, the_edit(), tampered(change));
    // No value, requested or read from the file, reaches the message.
    let message = report.failure_message().unwrap_or_default();
    for value in [
        SET_CANARY,
        FILL_CANARY,
        "GLOBAL_SECRET",
        "SENSITIVE_COMMENT",
        "CREATOR_SECRET",
        "STREAM_SECRET",
        "CHAPTER_SECRET",
        "TAMPER",
    ] {
        assert!(!message.contains(value), "{value} in {message}");
    }
    std::fs::remove_dir_all(&root).unwrap();
    report
}

// ------------------------------------------------------ Real, verified edits ---

/// Every supported format, edited for real: verified, the four rows as the
/// operations resolved, the streams copied (packet hashes, test-only), the
/// original byte for byte as it was, the extension kept.
#[test]
fn a_real_edit_verifies_in_every_format() {
    for ext in ALL_EXTENSIONS {
        let root = scratch(&format!("edit-verify-{ext}"));
        let input = if ext == "avi" {
            sample_plain_avi(&root)
        } else {
            sample_for_format(&root, ext)
        };
        let before = snapshot(&input);

        let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);

        assert_verified(&report, ext);
        assert_eq!(output.extension(), input.extension(), "{ext}");
        let fields: Vec<EditableField> = report.changes.iter().map(|r| r.field).collect();
        assert_eq!(
            fields,
            [
                EditableField::Title,
                EditableField::Comment,
                EditableField::Copyright,
                EditableField::Genre
            ],
            "{ext}"
        );
        let title = row(&report, EditableField::Title);
        assert_eq!(
            (&title.before, &title.after, title.changed),
            (&some("GLOBAL_SECRET"), &some(SET_CANARY), true),
            "{ext}"
        );
        let comment = row(&report, EditableField::Comment);
        assert_eq!(
            (&comment.before, &comment.after, comment.changed),
            (&some("SENSITIVE_COMMENT"), &None, true),
            "{ext}"
        );
        let genre = row(&report, EditableField::Genre);
        assert_eq!(
            (&genre.before, &genre.after, genre.changed),
            (&None, &some(FILL_CANARY), true),
            "{ext}"
        );
        let copyright = row(&report, EditableField::Copyright);
        assert_eq!(
            (&copyright.before, &copyright.after, copyright.changed),
            (&some("COPYRIGHT_SECRET"), &some("COPYRIGHT_SECRET"), false),
            "{ext}"
        );
        assert_eq!(report.fields_changed, 3, "{ext}");

        for stream in ["0:v:0", "0:a:0"] {
            assert_eq!(
                stream_payload_hash(&input, stream),
                stream_payload_hash(&output, stream),
                "{ext}: {stream} was not copied"
            );
        }
        assert!(snapshot(&input) == before, "{ext}: the original changed");
        assert!(output.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Every action resolves to Keep: the title is there so Fill keeps it, there
/// is no genre to remove, the copyright is there. A new copy is still
/// written, and it verifies with nothing changed.
#[test]
fn a_no_op_edit_verifies_with_nothing_changed() {
    let root = scratch("edit-verify-noop");
    let input = sample_for_format(&root, "mkv");
    let (_, report) = edit_file(
        &input,
        vec![
            fill(EditableField::Title, FILL_CANARY),
            remove(EditableField::Genre),
            fill(EditableField::Copyright, FILL_CANARY),
        ],
        run_ffmpeg,
    );
    assert_verified(&report, "no-op");
    assert_eq!(report.fields_changed, 0);
    assert!(report.changes.iter().all(|r| !r.changed));
    assert_eq!(row(&report, EditableField::Genre).before, None);
    assert_eq!(row(&report, EditableField::Genre).after, None);
    assert_eq!(
        row(&report, EditableField::Title).after,
        some("GLOBAL_SECRET")
    );
    assert!(check_of(&report, REQUESTED_CHANGES_APPLIED)
        .detail
        .ends_with("0 written, 0 removed, 3 kept"));
    std::fs::remove_dir_all(&root).unwrap();
}

/// Subtitles are media Edit keeps: the stream is expected, paired and copied.
#[test]
fn subtitles_survive_a_verified_edit() {
    for ext in ["mp4", "mov", "m4v", "mkv", "webm"] {
        let root = scratch(&format!("edit-verify-subs-{ext}"));
        let input = sample_for_format(&root, ext);
        let fresh = FreshInspection::take(&input).unwrap();
        assert_eq!(fresh.report().subtitle_streams().count(), 1, "{ext}");
        let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
        assert_verified(&report, ext);
        assert_eq!(
            stream_payload_hash(&input, "0:s:0"),
            stream_payload_hash(&output, "0:s:0"),
            "{ext}: the subtitle was not copied"
        );
        assert!(contains(
            &std::fs::read(&output).unwrap(),
            SUBTITLE_BODY.as_bytes()
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// MP4 and M4V cover art: kept with its flag, paired by position among the
/// ordinary streams although the rebuilt chapter track is written before it.
#[test]
fn mp4_cover_art_survives_a_verified_edit() {
    for ext in ["mp4", "m4v"] {
        let root = scratch(&format!("edit-verify-cover-{ext}"));
        let input = sample_with_cover(&root, ext, &cover_jpeg(&root, true));
        let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
        assert_verified(&report, ext);

        let before = inspect::inspect(&input).unwrap();
        let after = inspect::inspect(&output).unwrap();
        let cover = |r: &MetadataReport| r.attached_pictures().next().unwrap().index;
        assert_eq!(after.attached_pictures().count(), 1, "{ext}");
        assert_eq!(
            stream_payload_hash(&input, &format!("0:{}", cover(&before))),
            stream_payload_hash(&output, &format!("0:{}", cover(&after))),
            "{ext}: the cover was not copied"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Matroska attachments (two fonts) are ordinary expected streams, with
/// their tags, and their payloads are in the output.
#[test]
fn mkv_attachments_survive_a_verified_edit() {
    let root = scratch("edit-verify-attachments");
    let input = sample_for_format(&root, "mkv");
    let fresh = FreshInspection::take(&input).unwrap();
    let plan = plan_edit(
        &request(&input, the_edit()),
        format_profile("mkv").unwrap(),
        &fresh,
        &root,
    )
    .unwrap();
    let attachments = plan
        .expected_streams()
        .iter()
        .filter(|s| s.kind == StreamKind::Attachment)
        .count();
    assert_eq!(attachments, 2);

    let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
    assert_verified(&report, "mkv attachments");
    let bytes = std::fs::read(&output).unwrap();
    assert!(contains(&bytes, ATTACHMENT_CANARY_ALPHA));
    assert!(contains(&bytes, ATTACHMENT_CANARY_BETA));
    std::fs::remove_dir_all(&root).unwrap();
}

/// Chapters come through with their times and titles, in every container that
/// has them.
#[test]
fn chapters_survive_a_verified_edit() {
    for ext in ["mp4", "mov", "m4v", "mkv", "webm"] {
        let root = scratch(&format!("edit-verify-chapters-{ext}"));
        let input = sample_for_format(&root, ext);
        let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
        assert_verified(&report, ext);
        assert_eq!(
            check_of(&report, CHAPTERS_PRESERVED).detail,
            "1 chapter preserved",
            "{ext}"
        );
        let after = inspect::inspect(&output).unwrap();
        assert_eq!(after.chapters[0].title.as_deref(), Some("CHAPTER_SECRET"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The ISO-BMFF timecode and chapter tracks `-dn` drops are rebuilt by the
/// muxer, recognised as such, and nothing else is there.
#[test]
fn rebuilt_timecode_and_chapter_tracks_verify() {
    for ext in ["mov", "mp4"] {
        let root = scratch(&format!("edit-verify-timecode-{ext}"));
        let input = sample_with_timecode(&root, ext);
        let edits = vec![set(EditableField::Title, SET_CANARY)];
        let fresh = FreshInspection::take(&input).unwrap();
        let plan = plan_edit(
            &request(&input, edits.clone()),
            format_profile(ext).unwrap(),
            &fresh,
            &root,
        )
        .unwrap_or_else(|r| panic!("{ext}: {}", r.message()));
        assert_eq!(
            plan.rebuilt_tracks(),
            [RebuiltTrack::Chapters, RebuiltTrack::Timecode],
            "{ext}"
        );

        let (output, report) = edit_file(&input, edits, run_ffmpeg);
        assert_verified(&report, ext);
        assert_eq!(
            check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
            "Only the rebuilt chapter track and timecode track present"
        );
        let tmcd_timecode = |report: &MetadataReport| -> String {
            let tmcd = report
                .streams
                .iter()
                .find(|s| is_timecode_track(s))
                .unwrap();
            stream_tag(report, tmcd.index, "timecode")
                .unwrap()
                .to_string()
        };
        let timecode = tmcd_timecode(fresh.report());
        assert!(timecode.starts_with("01:00:00:"), "{timecode}");
        assert_eq!(tmcd_timecode(&inspect::inspect(&output).unwrap()), timecode);
        assert_eq!(
            stream_payload_hash(&input, "0:v:0"),
            stream_payload_hash(&output, "0:v:0")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// An ISO-BMFF track name is lost by a remux unless written back; the plan
/// writes it back, and the verifier reads it back as ffprobe reports it.
#[test]
fn iso_track_names_are_restored_and_verified() {
    for ext in ["mp4", "mov", "m4v"] {
        let root = scratch(&format!("edit-verify-names-{ext}"));
        let input = sample_for_format(&root, ext);
        let fresh = FreshInspection::take(&input).unwrap();
        let plan = plan_edit(
            &request(&input, the_edit()),
            format_profile(ext).unwrap(),
            &fresh,
            &root,
        )
        .unwrap();
        assert_eq!(plan.restores()[0].title, "STREAM_SECRET", "{ext}");

        let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
        assert_verified(&report, ext);
        let after = inspect::inspect(&output).unwrap();
        let video = after
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .unwrap();
        assert_eq!(
            stream_tag(&after, video.index, "name"),
            Some("STREAM_SECRET")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ------------------------------------------------- Tampered real outputs ---

#[test]
fn a_set_that_did_not_happen_fails() {
    let report = tampered_mkv("edit-verify-set-missing", |args| {
        without(args, ["-metadata", &format!("title={SET_CANARY}")])
    });
    assert_caught(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, REQUESTED_CHANGES_APPLIED).detail,
        "Requested change not applied to Title"
    );
    let title = row(&report, EditableField::Title);
    assert_eq!(
        (&title.after, title.changed),
        (&some("GLOBAL_SECRET"), false)
    );
}

#[test]
fn a_set_to_the_wrong_value_fails() {
    let report = tampered_mkv("edit-verify-set-wrong", |args| {
        replacing(
            args,
            ["-metadata", &format!("title={SET_CANARY}")],
            &["-metadata", "title=TAMPER_WRONG"],
        )
    });
    assert_caught(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        row(&report, EditableField::Title).after,
        some("TAMPER_WRONG")
    );
}

#[test]
fn a_fill_that_should_have_written_fails() {
    let report = tampered_mkv("edit-verify-fill-missing", |args| {
        without(args, ["-metadata", &format!("genre={FILL_CANARY}")])
    });
    assert_caught(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, REQUESTED_CHANGES_APPLIED).detail,
        "Requested change not applied to Genre"
    );
}

#[test]
fn a_fill_that_should_have_kept_fails_when_the_value_changed() {
    let report = tampered_mkv("edit-verify-fill-keep", |args| {
        with_output_options(args, &["-metadata", "copyright=TAMPER_COPYRIGHT"])
    });
    assert_caught(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, REQUESTED_CHANGES_APPLIED).detail,
        "Requested change not applied to Copyright"
    );
    assert!(row(&report, EditableField::Copyright).changed);
}

#[test]
fn a_remove_that_left_the_field_fails() {
    let report = tampered_mkv("edit-verify-remove", |args| {
        without(args, ["-metadata", "comment="])
    });
    assert_caught(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, REQUESTED_CHANGES_APPLIED).detail,
        "Requested change not applied to Comment"
    );
}

#[test]
fn an_unrelated_field_removed_fails_other_metadata() {
    let report = tampered_mkv("edit-verify-global-removed", |args| {
        with_output_options(args, &["-metadata", "artist="])
    });
    assert_caught(&report, OTHER_METADATA_PRESERVED);
    assert_passed(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, OTHER_METADATA_PRESERVED).detail,
        "Container metadata differs from the plan (1 missing)"
    );
}

#[test]
fn an_unrelated_field_changed_fails_other_metadata() {
    let report = tampered_mkv("edit-verify-global-changed", |args| {
        with_output_options(args, &["-metadata", "artist=TAMPER_ARTIST"])
    });
    assert_caught(&report, OTHER_METADATA_PRESERVED);
    assert_passed(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, OTHER_METADATA_PRESERVED).detail,
        "Container metadata differs from the plan (1 changed)"
    );
}

#[test]
fn an_unexpected_global_field_fails_other_metadata() {
    let report = tampered_mkv("edit-verify-global-added", |args| {
        with_output_options(args, &["-metadata", "album=TAMPER_ALBUM"])
    });
    assert_caught(&report, OTHER_METADATA_PRESERVED);
    assert_passed(&report, REQUESTED_CHANGES_APPLIED);
    assert_eq!(
        check_of(&report, OTHER_METADATA_PRESERVED).detail,
        "Container metadata differs from the plan (1 unexpected)"
    );
}

#[test]
fn a_missing_media_stream_fails_stream_structure() {
    let report = tampered_mkv("edit-verify-stream-missing", |args| {
        with_output_options(args, &["-map", "-0:a"])
    });
    assert_caught(&report, STREAM_STRUCTURE_PRESERVED);
    assert_caught(&report, STREAM_METADATA_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_STRUCTURE_PRESERVED).detail,
        "Stream count changed: 5 expected, 4 found"
    );
}

#[test]
fn an_unexpected_media_stream_fails_stream_structure() {
    let report = tampered_mkv("edit-verify-stream-added", |args| {
        with_output_options(args, &["-map", "0:a"])
    });
    assert_caught(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_STRUCTURE_PRESERVED).detail,
        "Stream count changed: 5 expected, 6 found"
    );
}

#[test]
fn a_changed_stream_order_fails_stream_structure() {
    let report = tampered_mkv("edit-verify-stream-order", |args| {
        replacing(
            args,
            ["-map", "0"],
            &["-map", "0:a", "-map", "0:v", "-map", "0:s", "-map", "0:t"],
        )
    });
    assert_caught(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_STRUCTURE_PRESERVED).detail,
        "Stream order changed: Audio found where Video stream 0 was expected"
    );
}

#[test]
fn a_re_encoded_stream_fails_stream_structure() {
    let report = tampered_mkv("edit-verify-stream-identity", |args| {
        with_output_options(args, &["-c:a", "pcm_s16le"])
    });
    assert_caught(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_STRUCTURE_PRESERVED).detail,
        "Audio stream 1 changed codec parameters"
    );
}

#[test]
fn stream_metadata_removed_fails() {
    let report = tampered_mkv("edit-verify-stream-tag-removed", |args| {
        with_output_options(args, &["-metadata:s:v:0", "title="])
    });
    assert_caught(&report, STREAM_METADATA_PRESERVED);
    assert_passed(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_METADATA_PRESERVED).detail,
        "Video stream 0 metadata changed (1 missing)"
    );
}

#[test]
fn stream_metadata_changed_fails() {
    let report = tampered_mkv("edit-verify-stream-tag-changed", |args| {
        with_output_options(args, &["-metadata:s:v:0", "title=TAMPER_NAME"])
    });
    assert_caught(&report, STREAM_METADATA_PRESERVED);
    assert_passed(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_METADATA_PRESERVED).detail,
        "Video stream 0 metadata changed (1 changed)"
    );
}

#[test]
fn unexpected_stream_metadata_fails() {
    let report = tampered_mkv("edit-verify-stream-tag-added", |args| {
        with_output_options(args, &["-metadata:s:a:0", "extra=TAMPER_EXTRA"])
    });
    assert_caught(&report, STREAM_METADATA_PRESERVED);
    assert_passed(&report, STREAM_STRUCTURE_PRESERVED);
    assert_eq!(
        check_of(&report, STREAM_METADATA_PRESERVED).detail,
        "Audio stream 1 metadata changed (1 unexpected)"
    );
}

#[test]
fn a_removed_chapter_fails() {
    let report = tampered_mkv("edit-verify-chapter-removed", |args| {
        replacing(args, ["-map_chapters", "0"], &["-map_chapters", "-1"])
    });
    assert_caught(&report, CHAPTERS_PRESERVED);
    assert_passed(&report, OTHER_METADATA_PRESERVED);
    assert_eq!(
        check_of(&report, CHAPTERS_PRESERVED).detail,
        "Chapter count changed: 1 expected, 0 found"
    );
}

/// Chapters taken from a second input instead of the file's own.
fn with_chapters(args: &mut Vec<String>, file: &Path) {
    let file = file.to_string_lossy().into_owned();
    with_input(args, &["-f", "ffmetadata", "-i", &file]);
    replacing(args, ["-map_chapters", "0"], &["-map_chapters", "1"]);
}

fn tampered_chapters(name: &str, chapters: &[(u32, u32, &str)]) -> EditVerificationReport {
    let dir = scratch(&format!("{name}-chapters"));
    let file = chapters_file(&dir, chapters);
    let report = tampered_mkv(name, |args| with_chapters(args, &file));
    std::fs::remove_dir_all(&dir).unwrap();
    report
}

#[test]
fn a_chapter_with_other_timing_fails() {
    let report = tampered_chapters("edit-verify-chapter-timing", &[(0, 900, "CHAPTER_SECRET")]);
    assert_caught(&report, CHAPTERS_PRESERVED);
    assert_eq!(
        check_of(&report, CHAPTERS_PRESERVED).detail,
        "Chapter 1 timing changed"
    );
}

#[test]
fn a_chapter_with_another_title_fails() {
    let report = tampered_chapters("edit-verify-chapter-title", &[(0, 1200, "TAMPER_TITLE")]);
    assert_caught(&report, CHAPTERS_PRESERVED);
    assert_eq!(
        check_of(&report, CHAPTERS_PRESERVED).detail,
        "Chapter 1 title changed"
    );
}

#[test]
fn an_unexpected_chapter_fails() {
    let report = tampered_chapters(
        "edit-verify-chapter-added",
        &[(0, 1200, "CHAPTER_SECRET"), (1200, 2000, "TAMPER_EXTRA")],
    );
    assert_caught(&report, CHAPTERS_PRESERVED);
    assert_eq!(
        check_of(&report, CHAPTERS_PRESERVED).detail,
        "Chapter count changed: 1 expected, 2 found"
    );
}

/// With the video's timecode tag deleted on the way out, the muxer has
/// nothing to rebuild the timecode track from, and it is missing.
#[test]
fn a_rebuilt_timecode_track_that_is_missing_fails() {
    let root = scratch("edit-verify-tmcd-missing");
    let input = sample_with_timecode(&root, "mov");
    let (output, report) = edit_file(
        &input,
        vec![set(EditableField::Title, SET_CANARY)],
        tampered(|args| with_output_options(args, &["-metadata:s:v:0", "timecode="])),
    );
    assert!(inspect::inspect(&output)
        .unwrap()
        .streams
        .iter()
        .all(|s| !is_timecode_track(s)));
    assert_caught(&report, STRUCTURAL_TRACKS_PRESERVED);
    assert_eq!(
        check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
        "Rebuilt timecode track missing"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// An MP4 with no chapters, given chapters on the way out: the muxer writes a
/// chapter track the plan did not expect.
#[test]
fn an_unexpected_rebuilt_track_fails() {
    let root = scratch("edit-verify-unexpected-track");
    let input = sample_video(&root, "plain.mp4");
    let file = chapters_file(&root, &[(0, 500, "TAMPER_CHAPTER")]);
    let (_, report) = edit_file(
        &input,
        vec![set(EditableField::Title, SET_CANARY)],
        tampered(|args| with_chapters(args, &file)),
    );
    assert_caught(&report, STRUCTURAL_TRACKS_PRESERVED);
    assert_caught(&report, CHAPTERS_PRESERVED);
    assert_eq!(
        check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
        "Unexpected chapter track found"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

/// The original is touched while FFmpeg runs: same size, same bytes, a new
/// modification time. The fingerprint taken before FFmpeg catches it.
#[test]
fn an_original_changed_during_the_edit_fails() {
    let root = scratch("edit-verify-original");
    let input = sample_for_format(&root, "mkv");
    let target = input.clone();
    let (_, report) = edit_file(&input, the_edit(), move |args: &[String]| {
        let result = run_ffmpeg(args);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap();
        let then = file.metadata().unwrap().modified().unwrap();
        file.set_modified(then + Duration::from_secs(60)).unwrap();
        result
    });
    assert_caught(&report, ORIGINAL_UNCHANGED);
    assert_eq!(failed(&report), [ORIGINAL_UNCHANGED]);
    std::fs::remove_dir_all(&root).unwrap();
}

// ------------------------------------------- The file itself, checked apart ---

/// A real edit made the way the runner makes it, kept apart so its output and
/// temporary path can be tampered with before verification.
struct Direct {
    root: PathBuf,
    input: PathBuf,
    fresh: FreshInspection,
    plan: EditPlan,
    output: PathBuf,
    temp: PathBuf,
    original: Option<OriginalFingerprint>,
}

impl Direct {
    fn new(name: &str) -> Self {
        let root = scratch(name);
        let input = sample_for_format(&root, "mkv");
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let fresh = FreshInspection::take(&input).unwrap();
        let plan = plan_edit(
            &request(&input, the_edit()),
            format_profile("mkv").unwrap(),
            &fresh,
            &out,
        )
        .unwrap();
        let original = OriginalFingerprint::capture(&input);
        let temp = edit_temp_path(&out, &format_id(1), &input);
        let output = out.join(format!("CLIP_{}.mkv", format_id(1)));
        let args = plan
            .ffmpeg_args(&input, &temp, MuxAttempt::Standard)
            .unwrap();
        assert!(run_ffmpeg(&args).unwrap().status.success());
        std::fs::rename(&temp, &output).unwrap();
        Direct {
            root,
            input,
            fresh,
            plan,
            output,
            temp,
            original,
        }
    }

    fn verify_as(&self, output: &Path) -> EditVerificationReport {
        EditVerification {
            input: &self.input,
            output,
            temp: &self.temp,
            before: self.fresh.report(),
            plan: &self.plan,
            original: self.original,
        }
        .run()
    }

    fn verify(&self) -> EditVerificationReport {
        self.verify_as(&self.output)
    }
}

impl Drop for Direct {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_direct_edit_verifies_untouched() {
    let direct = Direct::new("edit-verify-direct");
    assert_verified(&direct.verify(), "direct");
}

/// The same file under another extension, or another spelling of it: `.MKV`
/// is not `.mkv`. (Another base name too, since NTFS would take a name that
/// differs only in case for the same file.)
#[test]
fn a_changed_extension_fails() {
    let direct = Direct::new("edit-verify-extension");
    for name in ["RENAMED_0000000001.MKV", "RENAMED_0000000001.webm"] {
        let renamed = direct.output.with_file_name(name);
        std::fs::copy(&direct.output, &renamed).unwrap();
        let report = direct.verify_as(&renamed);
        assert_caught(&report, EXTENSION_PRESERVED);
        assert_eq!(failed(&report), [EXTENSION_PRESERVED], "{name}");
    }
}

/// This output's own temporary path must be empty; another file's leftover
/// is not this output's failure.
#[test]
fn only_this_outputs_temporary_file_counts() {
    let direct = Direct::new("edit-verify-temp");
    let other = edit_temp_path(
        direct.output.parent().unwrap(),
        &format_id(2),
        &direct.input,
    );
    std::fs::write(&other, b"another file's partial output").unwrap();
    assert_verified(&direct.verify(), "another temporary file");

    std::fs::write(&direct.temp, b"this file's partial output").unwrap();
    let report = direct.verify();
    assert_caught(&report, NO_TEMPORARY_OUTPUT);
    assert_eq!(failed(&report), [NO_TEMPORARY_OUTPUT]);
}

/// No output at all: nothing downstream passes by default, the three checks
/// that do not need the output still give real answers, and there are no
/// rows.
#[test]
fn a_missing_output_fails_everything_that_needs_it() {
    let direct = Direct::new("edit-verify-missing");
    std::fs::remove_file(&direct.output).unwrap();
    let report = direct.verify();
    assert_eq!(failed(&report), EDIT_CHECKS[..8]);
    for name in &EDIT_CHECKS[2..8] {
        assert_eq!(check_of(&report, name).detail, NOT_CHECKED);
    }
    assert!(report.changes.is_empty());
    assert_eq!(report.fields_changed, 0);
    assert_eq!(
        report.failure_message().unwrap(),
        "Verification failed: The edited file is missing, There is no edited file to inspect."
    );
}

/// An output that is there but is not a video: written, not readable, and
/// nothing about its contents is claimed.
#[test]
fn an_unreadable_output_fails_without_panicking() {
    let direct = Direct::new("edit-verify-unreadable");
    std::fs::write(&direct.output, b"not a video").unwrap();
    let report = direct.verify();
    assert_eq!(failed(&report), EDIT_CHECKS[1..8]);
    assert!(report.changes.is_empty());
    assert_eq!(report.remaining_privacy_count, 0);
    assert_eq!(
        report.failure_message().unwrap(),
        "Verification failed: The edited file could not be inspected."
    );
    assert!(direct.output.is_file(), "the verifier deleted the output");
}

// -------------------------------------------------- Synthetic inspections ---

fn tags(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect(),
    )
}

fn stream(kind: &str, codec: &str, tag: &str, stream_tags: &[(&str, &str)]) -> Value {
    json!({
        "codec_type": kind,
        "codec_name": codec,
        "codec_tag_string": tag,
        "disposition": { "attached_pic": 0 },
        "tags": tags(stream_tags),
    })
}

fn video(stream_tags: &[(&str, &str)]) -> Value {
    stream("video", "mpeg4", "mp4v", stream_tags)
}

fn audio(stream_tags: &[(&str, &str)]) -> Value {
    stream("audio", "aac", "mp4a", stream_tags)
}

fn cover() -> Value {
    let mut cover = stream("video", "mjpeg", "[0][0][0][0]", &[]);
    cover["disposition"]["attached_pic"] = json!(1);
    cover
}

fn chapter_track() -> Value {
    stream(
        "data",
        "bin_data",
        "text",
        &[("handler_name", "SubtitleHandler")],
    )
}

fn timecode_track(timecode: &str) -> Value {
    stream(
        "data",
        "unknown",
        "tmcd",
        &[("handler_name", "TimeCodeHandler"), ("timecode", timecode)],
    )
}

/// An inspection with these globals, these streams (indexed in order) and
/// these chapters, each `(start, end, title)`.
fn probe(
    globals: &[(&str, &str)],
    streams: Vec<Value>,
    chapters: &[(&str, &str, &str)],
) -> MetadataReport {
    let streams: Vec<Value> = streams
        .into_iter()
        .enumerate()
        .map(|(index, mut stream)| {
            stream["index"] = json!(index);
            stream
        })
        .collect();
    let chapters: Vec<Value> = chapters
        .iter()
        .map(|(start, end, title)| {
            json!({ "start_time": start, "end_time": end, "tags": { "title": title } })
        })
        .collect();
    let json = json!({
        "format": { "format_name": "test", "tags": tags(globals) },
        "streams": streams,
        "chapters": chapters,
    });
    parse_ffprobe_json("x", &json.to_string()).unwrap()
}

fn synthetic_plan(ext: &str, edits: Vec<MetadataEdit>, before: &MetadataReport) -> EditPlan {
    let input = PathBuf::from(format!(r"C:\videos\clip.{ext}"));
    plan_edit(
        &request(&input, edits),
        format_profile(ext).unwrap(),
        &FreshInspection::from_report(&input, before.clone()),
        Path::new(r"C:\out"),
    )
    .unwrap()
}

/// The checks against a synthetic output. The paths do not exist, so there is
/// no fingerprint and no temporary file; the tests look at the other checks.
fn verify_synthetic(
    ext: &str,
    plan: &EditPlan,
    before: &MetadataReport,
    after: &MetadataReport,
) -> EditVerificationReport {
    let input = PathBuf::from(format!(r"C:\videos\clip.{ext}"));
    let output = PathBuf::from(format!(r"C:\out\CLIP_0000000001.{ext}"));
    let temp = PathBuf::from(format!(
        r"C:\out\.video-cleaner-processing-0000000001.{ext}"
    ));
    EditVerification {
        input: &input,
        output: &output,
        temp: &temp,
        before,
        plan,
        original: None,
    }
    .evaluate(true, Some(after))
}

/// Everything that reads the output, which is all a synthetic test controls.
const OUTPUT_CHECKS: [&str; 7] = [
    OUTPUT_READABLE,
    REQUESTED_CHANGES_APPLIED,
    OTHER_METADATA_PRESERVED,
    STREAM_STRUCTURE_PRESERVED,
    STREAM_METADATA_PRESERVED,
    CHAPTERS_PRESERVED,
    STRUCTURAL_TRACKS_PRESERVED,
];

fn failed_output_checks(report: &EditVerificationReport) -> Vec<&'static str> {
    failed(report)
        .into_iter()
        .filter(|name| OUTPUT_CHECKS.contains(name))
        .collect()
}

/// A synthetic output that is exactly what the plan says passes every check
/// it controls, so a failure in the tests below is the tampering's doing.
#[test]
fn a_synthetic_output_matching_the_plan_passes() {
    let before = probe(
        &[("title", "Old"), ("artist", "A")],
        vec![
            video(&[("timecode", "01:00:00:00")]),
            audio(&[]),
            chapter_track(),
            timecode_track("01:00:00:00"),
        ],
        &[("0.000000", "1.000000", "One")],
    );
    let plan = synthetic_plan("mov", vec![set(EditableField::Title, "New")], &before);
    let after = probe(
        &[("title", "New"), ("artist", "A"), ("major_brand", "qt  ")],
        vec![
            video(&[("timecode", "01:00:00:00")]),
            audio(&[]),
            timecode_track("01:00:00:00"),
            chapter_track(),
        ],
        &[("0.000000", "1.000000", "One")],
    );
    let report = verify_synthetic("mov", &plan, &before, &after);
    assert!(
        failed_output_checks(&report).is_empty(),
        "{:?}",
        report.checks
    );
    // The one check a synthetic test cannot satisfy: there is no original.
    assert_eq!(failed(&report), [ORIGINAL_UNCHANGED]);
}

#[test]
fn a_requested_field_reported_twice_fails() {
    let before = probe(
        &[("title", "Old"), ("artist", "A")],
        vec![video(&[]), audio(&[])],
        &[],
    );
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "New")], &before);
    let after = probe(
        &[("title", "New"), ("TITLE", "Other"), ("artist", "A")],
        vec![video(&[]), audio(&[])],
        &[],
    );
    let report = verify_synthetic("mkv", &plan, &before, &after);
    assert_eq!(
        failed_output_checks(&report),
        [REQUESTED_CHANGES_APPLIED, OTHER_METADATA_PRESERVED]
    );
    // ffprobe's object sorts `TITLE` first, so the first value seen for the
    // key is also the wrong one.
    assert_eq!(
        check_of(&report, OTHER_METADATA_PRESERVED).detail,
        "Container metadata differs from the plan (1 changed, 1 with conflicting values)"
    );
}

#[test]
fn an_unrelated_field_reported_twice_fails() {
    let before = probe(
        &[("title", "Old"), ("artist", "A")],
        vec![video(&[]), audio(&[])],
        &[],
    );
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "New")], &before);
    let after = probe(
        &[("title", "New"), ("artist", "A"), ("ARTIST", "B")],
        vec![video(&[]), audio(&[])],
        &[],
    );
    let report = verify_synthetic("mkv", &plan, &before, &after);
    assert_eq!(failed_output_checks(&report), [OTHER_METADATA_PRESERVED]);
}

/// Remove of a field that is not there resolves to Keep; the output gaining
/// the field is a change nobody asked for.
#[test]
fn a_no_op_field_that_changed_fails() {
    let before = probe(&[("title", "T")], vec![video(&[]), audio(&[])], &[]);
    let plan = synthetic_plan("mkv", vec![remove(EditableField::Genre)], &before);
    let after = probe(
        &[("title", "T"), ("genre", "G")],
        vec![video(&[]), audio(&[])],
        &[],
    );
    let report = verify_synthetic("mkv", &plan, &before, &after);
    assert_eq!(
        failed_output_checks(&report),
        [REQUESTED_CHANGES_APPLIED, OTHER_METADATA_PRESERVED]
    );
}

#[test]
fn a_cover_that_lost_its_flag_fails() {
    let before = probe(&[], vec![video(&[]), audio(&[]), cover()], &[]);
    let plan = synthetic_plan("mp4", vec![set(EditableField::Title, "N")], &before);
    let mut plain = cover();
    plain["disposition"]["attached_pic"] = json!(0);
    let after = probe(&[("title", "N")], vec![video(&[]), audio(&[]), plain], &[]);
    let report = verify_synthetic("mp4", &plan, &before, &after);
    assert_eq!(failed_output_checks(&report), [STREAM_STRUCTURE_PRESERVED]);
    assert_eq!(
        check_of(&report, STREAM_STRUCTURE_PRESERVED).detail,
        "Video stream 2 changed its cover art flag"
    );
}

#[test]
fn a_stream_with_conflicting_tags_fails() {
    let before = probe(&[], vec![video(&[("language", "eng")]), audio(&[])], &[]);
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "N")], &before);
    let after = probe(
        &[("title", "N")],
        vec![
            video(&[("language", "eng"), ("LANGUAGE", "ita")]),
            audio(&[]),
        ],
        &[],
    );
    let report = verify_synthetic("mkv", &plan, &before, &after);
    assert_eq!(failed_output_checks(&report), [STREAM_METADATA_PRESERVED]);
    assert_eq!(
        check_of(&report, STREAM_METADATA_PRESERVED).detail,
        "Video stream 0 has conflicting metadata"
    );
}

/// The ordinary streams are paired by their order among themselves, not by
/// raw index: a rebuilt track ahead of them shifts every index and is fine.
#[test]
fn ordinary_streams_are_paired_around_rebuilt_tracks() {
    let before = probe(
        &[],
        vec![video(&[]), audio(&[("name", "Mic")]), chapter_track()],
        &[("0.000000", "1.000000", "One")],
    );
    let plan = synthetic_plan("mp4", vec![set(EditableField::Title, "N")], &before);
    let after = probe(
        &[("title", "N")],
        vec![chapter_track(), video(&[]), audio(&[("name", "Mic")])],
        &[("0.000000", "1.000000", "One")],
    );
    let report = verify_synthetic("mp4", &plan, &before, &after);
    assert!(
        failed_output_checks(&report).is_empty(),
        "{:?}",
        report.checks
    );
}

fn timecode_plan() -> (MetadataReport, EditPlan) {
    let before = probe(
        &[],
        vec![
            video(&[("timecode", "01:00:00:00")]),
            audio(&[]),
            chapter_track(),
            timecode_track("01:00:00:00"),
        ],
        &[("0.000000", "1.000000", "One")],
    );
    let plan = synthetic_plan("mov", vec![set(EditableField::Title, "N")], &before);
    assert_eq!(
        plan.rebuilt_tracks(),
        [RebuiltTrack::Chapters, RebuiltTrack::Timecode]
    );
    (before, plan)
}

fn with_data_tracks(tracks: Vec<Value>) -> MetadataReport {
    let mut streams = vec![video(&[("timecode", "01:00:00:00")]), audio(&[])];
    streams.extend(tracks);
    probe(
        &[("title", "N")],
        streams,
        &[("0.000000", "1.000000", "One")],
    )
}

#[test]
fn rebuilt_tracks_must_all_be_there() {
    let (before, plan) = timecode_plan();
    for (tracks, detail) in [
        (vec![chapter_track()], "Rebuilt timecode track missing"),
        (
            vec![timecode_track("01:00:00:00")],
            "Rebuilt chapter track missing",
        ),
        (
            vec![],
            "Rebuilt chapter track missing, Rebuilt timecode track missing",
        ),
    ] {
        let report = verify_synthetic("mov", &plan, &before, &with_data_tracks(tracks));
        assert_eq!(failed_output_checks(&report), [STRUCTURAL_TRACKS_PRESERVED]);
        assert_eq!(
            check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
            detail
        );
    }
}

#[test]
fn a_rebuilt_timecode_track_must_match_the_video() {
    let (before, plan) = timecode_plan();
    let after = with_data_tracks(vec![chapter_track(), timecode_track("02:00:00:00")]);
    let report = verify_synthetic("mov", &plan, &before, &after);
    assert_eq!(failed_output_checks(&report), [STRUCTURAL_TRACKS_PRESERVED]);
    assert_eq!(
        check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
        "Rebuilt timecode track does not match the video's timecode"
    );
}

/// Telemetry, arbitrary data, a second rebuilt track: none of it is in the
/// plan, whatever the track calls itself.
#[test]
fn an_unexpected_data_track_fails() {
    let (before, plan) = timecode_plan();
    for (extra, detail) in [
        (
            stream("data", "bin_data", "gpmd", &[("handler_name", "GoPro MET")]),
            "1 unexpected data track found",
        ),
        (
            stream("data", "bin_data", "mebx", &[]),
            "1 unexpected data track found",
        ),
        (
            timecode_track("01:00:00:00"),
            "Unexpected timecode track found",
        ),
    ] {
        let after = with_data_tracks(vec![chapter_track(), timecode_track("01:00:00:00"), extra]);
        let report = verify_synthetic("mov", &plan, &before, &after);
        assert_eq!(failed_output_checks(&report), [STRUCTURAL_TRACKS_PRESERVED]);
        assert_eq!(
            check_of(&report, STRUCTURAL_TRACKS_PRESERVED).detail,
            detail
        );
    }

    // Outside ISO-BMFF no data track is ever expected.
    let before = probe(&[], vec![video(&[]), audio(&[])], &[]);
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "N")], &before);
    let after = probe(
        &[("title", "N")],
        vec![video(&[]), audio(&[]), chapter_track()],
        &[],
    );
    let report = verify_synthetic("mkv", &plan, &before, &after);
    assert_eq!(failed_output_checks(&report), [STRUCTURAL_TRACKS_PRESERVED]);
}

/// A chapter the output lost, or gained, or whose end moved, when the
/// original had them; and chapters out of nowhere when it had none.
#[test]
fn chapters_are_compared_in_full() {
    let chapters = [
        ("0.000000", "1.000000", "One"),
        ("1.000000", "2.000000", "Two"),
    ];
    let before = probe(&[], vec![video(&[]), audio(&[])], &chapters);
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "N")], &before);
    for (after_chapters, detail) in [
        (
            vec![chapters[0]],
            "Chapter count changed: 2 expected, 1 found",
        ),
        (
            vec![chapters[0], ("1.000000", "2.500000", "Two")],
            "Chapter 2 timing changed",
        ),
        (
            vec![chapters[0], ("1.000000", "2.000000", "Deux")],
            "Chapter 2 title changed",
        ),
    ] {
        let after = probe(
            &[("title", "N")],
            vec![video(&[]), audio(&[])],
            &after_chapters,
        );
        let report = verify_synthetic("mkv", &plan, &before, &after);
        assert_eq!(failed_output_checks(&report), [CHAPTERS_PRESERVED]);
        assert_eq!(check_of(&report, CHAPTERS_PRESERVED).detail, detail);
    }

    let none = probe(&[], vec![video(&[]), audio(&[])], &[]);
    let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "N")], &none);
    let after = probe(
        &[("title", "N")],
        vec![video(&[]), audio(&[])],
        &[chapters[0]],
    );
    let report = verify_synthetic("mkv", &plan, &none, &after);
    assert_eq!(failed_output_checks(&report), [CHAPTERS_PRESERVED]);
}

/// No fingerprint is no proof: the check fails rather than passing blind.
#[test]
fn an_original_without_a_fingerprint_is_not_confirmed() {
    let direct = Direct::new("edit-verify-no-fingerprint");
    let report = EditVerification {
        input: &direct.input,
        output: &direct.output,
        temp: &direct.temp,
        before: direct.fresh.report(),
        plan: &direct.plan,
        original: None,
    }
    .run();
    assert_eq!(failed(&report), [ORIGINAL_UNCHANGED]);
}

// ---------------------------------------------------------- After an edit ---

fn categories(report: &EditVerificationReport) -> &[&'static str] {
    &report.remaining_privacy_categories
}

/// GPS the file already had is kept by an edit that did not touch it. That is
/// Edit working, so the edit verifies, and the location is reported as still
/// there.
#[test]
fn retained_gps_is_reported_and_does_not_fail_verification() {
    let root = scratch("edit-verify-privacy-gps");
    let input = sample_for_format(&root, "mp4");
    let (_, report) = edit_file(
        &input,
        vec![set(EditableField::Title, SET_CANARY)],
        run_ffmpeg,
    );
    assert_verified(&report, "gps");
    assert!(categories(&report).contains(&"Location"), "{report:?}");
    assert!(report.remaining_privacy_count > 0);
    // Category labels only: no coordinate anywhere in the warning.
    let warning = serde_json::to_string(categories(&report)).unwrap();
    assert!(!warning.contains("45.46"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn retained_device_metadata_is_reported() {
    let root = scratch("edit-verify-privacy-device");
    let input = sample_for_format(&root, "mov");
    let (_, report) = edit_file(
        &input,
        vec![set(EditableField::Title, SET_CANARY)],
        run_ffmpeg,
    );
    assert_verified(&report, "device");
    assert!(categories(&report).contains(&"Device"), "{report:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

/// An Artist the user just set is their own choice, not a leftover.
#[test]
fn an_artist_the_edit_wrote_is_not_a_warning() {
    let root = scratch("edit-verify-privacy-artist-set");
    let input = sample_for_format(&root, "mkv");
    let (output, report) = edit_file(
        &input,
        vec![set(EditableField::Artist, "Emilio")],
        run_ffmpeg,
    );
    assert_verified(&report, "artist set");
    // The classifier does see it...
    let findings = classify(&inspect::inspect(&output).unwrap());
    assert!(findings
        .iter()
        .any(|f| f.category == PrivacyCategory::CreatorIdentity));
    // ...and it is the only creator field, so the warning has no Creator row.
    assert!(!categories(&report).contains(&"Creator"), "{report:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

/// An Artist the file already had, kept by Fill if missing, was not written
/// by this edit: it is a leftover like any other.
#[test]
fn an_artist_kept_by_fill_is_a_warning() {
    let root = scratch("edit-verify-privacy-artist-kept");
    let input = sample_for_format(&root, "mkv");
    let (_, report) = edit_file(
        &input,
        vec![fill(EditableField::Artist, "Emilio")],
        run_ffmpeg,
    );
    assert_verified(&report, "artist kept");
    assert!(!row(&report, EditableField::Artist).changed);
    assert!(categories(&report).contains(&"Creator"), "{report:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

/// LOW findings (the Matroska muxer's `encoder`, a copyright) and Structural
/// ones (handler names, languages, chapters) are in the output and are not a
/// warning.
#[test]
fn low_and_structural_findings_are_not_a_warning() {
    let root = scratch("edit-verify-privacy-low");
    let input = sample_for_format(&root, "mkv");
    let (output, report) = edit_file(&input, the_edit(), run_ffmpeg);
    assert_verified(&report, "low");
    let findings = classify(&inspect::inspect(&output).unwrap());
    for category in [
        PrivacyCategory::Software,
        PrivacyCategory::Copyright,
        PrivacyCategory::Structural,
    ] {
        assert!(
            findings.iter().any(|f| f.category == category),
            "the fixture carries no {category:?} finding"
        );
    }
    for label in ["Software", "Copyright", "Structural"] {
        assert!(!categories(&report).contains(&label), "{report:?}");
    }
    let expected = findings
        .iter()
        .filter(|f| f.severity >= Severity::Medium && !f.category.is_structural())
        .filter(|f| {
            !(f.scope == MetadataScope::Format
                && ["title", "genre"].contains(&f.source_key.as_str()))
        })
        .count();
    assert_eq!(report.remaining_privacy_count, expected);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Whatever privacy metadata remains, `verified` is the checks and only the
/// checks: the same output with and without a location verifies the same.
#[test]
fn remaining_privacy_never_decides_verified() {
    for location in [None, Some("+45.4642+009.1900/")] {
        let mut globals = vec![("title", "Old")];
        globals.extend(location.map(|l| ("location", l)));
        let before = probe(&globals, vec![video(&[]), audio(&[])], &[]);
        let plan = synthetic_plan("mkv", vec![set(EditableField::Title, "New")], &before);
        globals[0] = ("title", "New");
        let after = probe(&globals, vec![video(&[]), audio(&[])], &[]);
        let report = verify_synthetic("mkv", &plan, &before, &after);
        assert!(failed_output_checks(&report).is_empty());
        assert_eq!(
            categories(&report).contains(&"Location"),
            location.is_some()
        );
        assert_eq!(failed(&report), [ORIGINAL_UNCHANGED]);
    }
}
