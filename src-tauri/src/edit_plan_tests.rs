use super::*;
use crate::edit::{EditOperation, EditRequest, MetadataEdit};
use crate::format_profile;
use crate::inspect::parse_ffprobe_json;
use crate::testkit::{
    cover_jpeg, sample_for_format, sample_plain_avi, sample_with_cover, scratch,
    stream_payload_hash,
};
use serde_json::{json, Value};
use std::path::PathBuf;

// ---------------------------------------------------------------- Helpers ---

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

fn valid(edits: Vec<MetadataEdit>) -> ValidEditRequest {
    EditRequest {
        paths: vec!["C:\\videos\\clip.mp4".into()],
        edits,
    }
    .validate()
    .unwrap()
}

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

fn subtitle(stream_tags: &[(&str, &str)]) -> Value {
    stream("subtitle", "mov_text", "tx3g", stream_tags)
}

fn cover(stream_tags: &[(&str, &str)]) -> Value {
    let mut cover = stream("video", "mjpeg", "[0][0][0][0]", stream_tags);
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

/// A fresh inspection of a file with these globals, streams (indexed in
/// order) and this many chapters.
fn inspection(globals: &[(&str, &str)], streams: Vec<Value>, chapters: usize) -> FreshInspection {
    let streams: Vec<Value> = streams
        .into_iter()
        .enumerate()
        .map(|(index, mut stream)| {
            stream["index"] = json!(index);
            stream
        })
        .collect();
    let chapters: Vec<Value> = (0..chapters)
        .map(|i| {
            json!({
                "start_time": format!("{i}.000000"),
                "end_time": format!("{}.000000", i + 1),
                "tags": { "title": format!("Chapter {}", i + 1) },
            })
        })
        .collect();
    let json = json!({
        "format": { "format_name": "test", "tags": tags(globals) },
        "streams": streams,
        "chapters": chapters,
    });
    FreshInspection::from_report(INPUT, parse_ffprobe_json("x", &json.to_string()).unwrap())
}

/// Video and audio, nothing else.
fn av(globals: &[(&str, &str)]) -> FreshInspection {
    inspection(globals, vec![video(&[]), audio(&[])], 0)
}

fn plan(
    ext: &str,
    edits: Vec<MetadataEdit>,
    fresh: &FreshInspection,
) -> Result<EditPlan, EditRefusal> {
    plan_edit(
        &valid(edits),
        format_profile(ext).unwrap(),
        fresh,
        Path::new(OUT_DIR),
    )
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Where a planned file is read from and written to, in the pure tests.
const INPUT: &str = r"C:\videos\clip.mp4";
const OUT_DIR: &str = r"C:\out";

const ALL_EXTENSIONS: [&str; 6] = ["mp4", "mov", "m4v", "mkv", "webm", "avi"];

// ------------------------------------------------------- Resolved actions ---

#[test]
fn every_resolution_state_is_distinguished() {
    let fresh = av(&[
        ("title", "Old title"),
        ("artist", "Old artist"),
        ("comment", "Old comment"),
        ("date", "   "),
    ]);
    let plan = plan(
        "mkv",
        vec![
            set(EditableField::Title, "New title"),
            fill(EditableField::Artist, "Ignored"),
            remove(EditableField::Comment),
            remove(EditableField::Copyright),
            fill(EditableField::Genre, "Jazz"),
            fill(EditableField::Date, "2026-09-25"),
        ],
        &fresh,
    )
    .unwrap();

    let actions: Vec<(EditableField, FieldChange)> = plan
        .actions()
        .iter()
        .map(|a| (a.field, a.change.clone()))
        .collect();
    assert_eq!(
        actions,
        [
            (EditableField::Title, FieldChange::Write("New title".into())),
            (EditableField::Artist, FieldChange::Keep),
            (EditableField::Comment, FieldChange::Delete),
            (EditableField::Copyright, FieldChange::Keep),
            (EditableField::Genre, FieldChange::Write("Jazz".into())),
            (EditableField::Date, FieldChange::Write("2026-09-25".into())),
        ]
    );
    assert!(plan.changes_fields());
    assert_eq!(
        plan.expected_globals(),
        &map(&[
            ("artist", "Old artist"),
            ("date", "2026-09-25"),
            ("genre", "Jazz"),
            ("title", "New title"),
        ])
    );
}

#[test]
fn a_plan_that_changes_nothing_is_still_a_plan() {
    let fresh = av(&[("title", "Kept"), ("x_custom", "v")]);
    let plan = plan(
        "mkv",
        vec![
            fill(EditableField::Title, "Other"),
            remove(EditableField::Genre),
        ],
        &fresh,
    )
    .unwrap();
    assert!(!plan.changes_fields());
    assert_eq!(
        plan.expected_globals(),
        &map(&[("title", "Kept"), ("x_custom", "v")])
    );
    let args = plan
        .ffmpeg_args(
            Path::new("in.mkv"),
            Path::new("out.mkv"),
            MuxAttempt::Standard,
        )
        .unwrap();
    assert!(!args.iter().any(|a| a.starts_with("-metadata")), "{args:?}");
}

#[test]
fn fill_decides_against_the_fresh_inspection() {
    // The same request against the file as the scan saw it, and as it is when
    // the batch reaches it: only the second one counts.
    let request = vec![fill(EditableField::Copyright, "2026 Me")];
    let at_scan = av(&[]);
    let now = av(&[("copyright", "1999 Someone")]);
    assert_eq!(
        plan("mkv", request.clone(), &at_scan).unwrap().actions()[0].change,
        FieldChange::Write("2026 Me".into())
    );
    let plan = plan("mkv", request, &now).unwrap();
    assert_eq!(plan.actions()[0].change, FieldChange::Keep);
    assert_eq!(
        plan.expected_globals(),
        &map(&[("copyright", "1999 Someone")])
    );
}

// -------------------------------------------------------- Expected globals ---

#[test]
fn expected_globals_leave_out_only_the_muxers_own_keys() {
    let fresh = av(&[
        ("major_brand", "qt  "),
        ("minor_version", "512"),
        ("compatible_brands", "qt  "),
        ("encoder", "Lavf62.12.102"),
        ("title", "T"),
        ("location", "+45.4642+009.1900/"),
        ("location-eng", "+45.4642+009.1900/"),
        ("make", "Cam"),
        ("model", "QA-1"),
        ("creation_time", "2024-05-01T10:00:00.000000Z"),
    ]);
    let plan = plan("mov", vec![set(EditableField::Artist, "A")], &fresh).unwrap();
    // Privacy metadata the container can keep is expected to be kept.
    assert_eq!(
        plan.expected_globals(),
        &map(&[
            ("artist", "A"),
            ("creation_time", "2024-05-01T10:00:00.000000Z"),
            ("location", "+45.4642+009.1900/"),
            ("make", "Cam"),
            ("model", "QA-1"),
            ("title", "T"),
        ])
    );
}

// --------------------------------------------------------------- Ambiguity ---

fn ambiguous_title() -> FreshInspection {
    av(&[
        ("TITLE", "One secret"),
        ("title", "Two secret"),
        ("genre", "G"),
    ])
}

#[test]
fn an_ambiguous_requested_field_is_refused_whatever_the_operation() {
    for edit in [
        set(EditableField::Title, "New"),
        remove(EditableField::Title),
        fill(EditableField::Title, "New"),
    ] {
        let refusal = plan("mkv", vec![edit.clone()], &ambiguous_title()).unwrap_err();
        assert_eq!(
            refusal,
            EditRefusal::AmbiguousField(EditableField::Title),
            "{edit:?}"
        );
        let message = refusal.message();
        assert_eq!(
            message,
            "Title has conflicting metadata values and can't be edited safely."
        );
        assert!(!message.contains("secret"));
    }
}

#[test]
fn an_ambiguous_untouched_field_is_unpreserved_metadata() {
    let refusal = plan(
        "mkv",
        vec![set(EditableField::Genre, "New")],
        &ambiguous_title(),
    )
    .unwrap_err();
    assert_eq!(
        refusal,
        EditRefusal::UnpreservedMetadata(vec!["title".into()])
    );
}

// ----------------------------------------------------- Unpreserved globals ---

fn iphone_style(title: &str) -> FreshInspection {
    av(&[
        ("title", title),
        ("artist", "Someone"),
        ("com.apple.quicktime.make", "Apple"),
        ("com.apple.quicktime.model", "iPhone SECRET"),
        (
            "com.apple.quicktime.location.ISO6709",
            "+45.4642+009.1900+120.000/",
        ),
        ("com.apple.quicktime.software", "17.0"),
    ])
}

#[test]
fn untouched_mdta_keys_refuse_an_mp4_edit() {
    let refusal = plan(
        "mp4",
        vec![set(EditableField::Title, "New")],
        &iphone_style("Old"),
    )
    .unwrap_err();
    assert_eq!(
        refusal,
        EditRefusal::UnpreservedMetadata(vec![
            "com.apple.quicktime.location.iso6709".into(),
            "com.apple.quicktime.make".into(),
            "com.apple.quicktime.model".into(),
            "com.apple.quicktime.software".into(),
        ])
    );
    let message = refusal.message();
    assert_eq!(
        message,
        "Editing would lose metadata this format can't keep: com.apple.quicktime.location.iso6709, com.apple.quicktime.make, com.apple.quicktime.model and 1 more."
    );
    assert!(!message.contains("SECRET") && !message.contains("+45"));
}

#[test]
fn make_and_model_survive_mov_but_refuse_mp4() {
    let fresh = av(&[("title", "T"), ("make", "Cam"), ("model", "M")]);
    assert!(plan("mov", vec![set(EditableField::Title, "N")], &fresh).is_ok());
    for ext in ["mp4", "m4v"] {
        assert_eq!(
            plan(ext, vec![set(EditableField::Title, "N")], &fresh).unwrap_err(),
            EditRefusal::UnpreservedMetadata(vec!["make".into(), "model".into()]),
            "{ext}"
        );
    }
}

#[test]
fn avi_refuses_what_its_info_chunks_cannot_hold() {
    let fresh = av(&[("title", "T"), ("product", "Album")]);
    assert_eq!(
        plan("avi", vec![set(EditableField::Title, "N")], &fresh).unwrap_err(),
        EditRefusal::UnpreservedMetadata(vec!["product".into()])
    );
}

// --------------------------------------------------------------- Cover art ---

#[test]
fn cover_art_is_kept_only_on_the_mp4_muxer_path() {
    let fresh = inspection(&[], vec![video(&[]), audio(&[]), cover(&[])], 0);
    for ext in ["mp4", "m4v"] {
        let plan = plan(ext, vec![set(EditableField::Title, "N")], &fresh).unwrap();
        let kept = &plan.expected_streams()[2];
        assert!(kept.attached_pic && kept.kind == StreamKind::Video, "{ext}");
        assert_eq!((kept.input_index, kept.output_index), (2, 2));
    }
    for ext in ["mov", "mkv", "webm", "avi"] {
        let refusal = plan(ext, vec![set(EditableField::Title, "N")], &fresh).unwrap_err();
        assert_eq!(refusal, EditRefusal::CoverArt, "{ext}");
        assert_eq!(
            refusal.message(),
            "Edit can't keep the detected cover art in this format safely."
        );
    }
}

// -------------------------------------------------------------- Data tracks ---

fn data_refusal(ext: &str, fresh: &FreshInspection) -> EditRefusal {
    plan(ext, vec![set(EditableField::Title, "N")], fresh).unwrap_err()
}

#[test]
fn telemetry_and_unknown_data_tracks_are_refused() {
    for (ext, tag) in [
        ("mp4", "gpmd"),
        ("mov", "mebx"),
        ("m4v", "fdsc"),
        ("mov", "rtmd"),
        ("mp4", "text"),
    ] {
        // `text` alone is not enough: a chapter track needs chapters, and
        // this file has none.
        let fresh = inspection(
            &[],
            vec![video(&[]), audio(&[]), stream("data", "bin_data", tag, &[])],
            0,
        );
        let refusal = data_refusal(ext, &fresh);
        assert_eq!(
            refusal,
            EditRefusal::DataTrack {
                codec_tag: Some(tag.into())
            },
            "{ext} {tag}"
        );
        assert!(refusal.message().contains(tag));
    }
}

#[test]
fn a_chapter_track_is_rebuilt_only_alongside_chapters() {
    let fresh = inspection(&[], vec![video(&[]), audio(&[]), chapter_track()], 2);
    for ext in ["mp4", "mov", "m4v"] {
        let plan = plan(ext, vec![set(EditableField::Title, "N")], &fresh).unwrap();
        assert_eq!(plan.rebuilt_tracks(), [RebuiltTrack::Chapters]);
        assert_eq!(
            plan.expected_streams().len(),
            2,
            "the data track is not copied"
        );
        assert_eq!(plan.expected_chapters().len(), 2);
    }
    let two = inspection(
        &[],
        vec![video(&[]), audio(&[]), chapter_track(), chapter_track()],
        2,
    );
    assert!(matches!(
        data_refusal("mp4", &two),
        EditRefusal::DataTrack { .. }
    ));
}

#[test]
fn a_timecode_track_is_rebuilt_only_in_the_proven_shape() {
    let tc = "01:02:03:04";
    let proven = inspection(
        &[],
        vec![
            video(&[("timecode", tc)]),
            audio(&[]),
            chapter_track(),
            timecode_track(tc),
        ],
        1,
    );
    for ext in ["mp4", "mov"] {
        let plan = plan(ext, vec![set(EditableField::Title, "N")], &proven).unwrap();
        assert_eq!(
            plan.rebuilt_tracks(),
            [RebuiltTrack::Chapters, RebuiltTrack::Timecode]
        );
    }

    let refused = |streams: Vec<Value>| data_refusal("mov", &inspection(&[], streams, 0));
    let tmcd = EditRefusal::DataTrack {
        codec_tag: Some("tmcd".into()),
    };
    // Timecodes that disagree.
    assert_eq!(
        refused(vec![
            video(&[("timecode", "00:00:00:00")]),
            timecode_track(tc)
        ]),
        tmcd
    );
    // No timecode on the video to rebuild from.
    assert_eq!(refused(vec![video(&[]), timecode_track(tc)]), tmcd);
    // Two video streams, or a cover beside the video.
    assert_eq!(
        refused(vec![
            video(&[("timecode", tc)]),
            video(&[("timecode", tc)]),
            timecode_track(tc)
        ]),
        tmcd
    );
    let with_cover = inspection(
        &[],
        vec![video(&[("timecode", tc)]), cover(&[]), timecode_track(tc)],
        0,
    );
    assert_eq!(data_refusal("mp4", &with_cover), tmcd);
    // Two timecode tracks.
    assert_eq!(
        refused(vec![
            video(&[("timecode", tc)]),
            timecode_track(tc),
            timecode_track(tc)
        ]),
        tmcd
    );
}

#[test]
fn data_tracks_outside_iso_bmff_are_refused() {
    let fresh = inspection(&[], vec![video(&[]), audio(&[]), chapter_track()], 1);
    for ext in ["mkv", "webm", "avi"] {
        assert!(
            matches!(data_refusal(ext, &fresh), EditRefusal::DataTrack { .. }),
            "{ext}"
        );
    }
}

#[test]
fn an_unknown_stream_is_refused_everywhere() {
    let fresh = inspection(&[], vec![video(&[]), json!({ "codec_type": "weird" })], 0);
    for ext in ALL_EXTENSIONS {
        assert_eq!(
            data_refusal(ext, &fresh),
            EditRefusal::UnknownStream,
            "{ext}"
        );
    }
}

// ------------------------------------------------ Attachments and subtitles ---

fn font() -> Value {
    stream(
        "attachment",
        "ttf",
        "[0][0][0][0]",
        &[
            ("filename", "font.ttf"),
            ("mimetype", "application/x-truetype-font"),
        ],
    )
}

#[test]
fn attachments_are_kept_in_matroska_and_refused_elsewhere() {
    let fresh = inspection(&[], vec![video(&[]), audio(&[]), font()], 0);
    let plan = plan("mkv", vec![set(EditableField::Title, "N")], &fresh).unwrap();
    let kept = &plan.expected_streams()[2];
    assert_eq!(kept.kind, StreamKind::Attachment);
    assert_eq!(
        kept.tags,
        map(&[
            ("filename", "font.ttf"),
            ("mimetype", "application/x-truetype-font")
        ])
    );
    for ext in ["mp4", "mov", "m4v", "webm", "avi"] {
        assert_eq!(data_refusal(ext, &fresh), EditRefusal::Attachment, "{ext}");
    }
}

#[test]
fn subtitles_are_always_expected_and_never_removed() {
    let fresh = inspection(
        &[],
        vec![video(&[]), audio(&[]), subtitle(&[]), subtitle(&[])],
        0,
    );
    for ext in ["mp4", "mov", "m4v", "mkv", "webm"] {
        let plan = plan(ext, vec![remove(EditableField::Comment)], &fresh).unwrap();
        let subtitles = plan
            .expected_streams()
            .iter()
            .filter(|s| s.kind == StreamKind::Subtitle)
            .count();
        assert_eq!(subtitles, 2, "{ext}");
        for attempt in plan.mux_attempts() {
            let args = plan
                .ffmpeg_args(Path::new("in"), Path::new("out"), *attempt)
                .unwrap();
            assert!(!args.contains(&"-sn".to_string()), "{ext}");
        }
    }
}

// -------------------------------------------------------- Stream metadata ---

#[test]
fn iso_track_names_are_restored_at_their_output_index() {
    // The chapter track sits between the subtitle and the cover. `-dn` drops
    // it, so the cover is output stream 3, not 4.
    let fresh = inspection(
        &[],
        vec![
            video(&[("name", "Cam A"), ("handler_name", "Core Media Video")]),
            audio(&[("name", "Mic")]),
            subtitle(&[("name", "Subs EN")]),
            chapter_track(),
            cover(&[("name", "Cover")]),
        ],
        1,
    );
    let plan = plan("mp4", vec![set(EditableField::Title, "N")], &fresh).unwrap();
    let restores: Vec<(usize, &str)> = plan
        .restores()
        .iter()
        .map(|r| (r.output_index, r.title.as_str()))
        .collect();
    assert_eq!(
        restores,
        [(0, "Cam A"), (1, "Mic"), (2, "Subs EN"), (3, "Cover")]
    );
    let indices: Vec<(usize, usize)> = plan
        .expected_streams()
        .iter()
        .map(|s| (s.input_index, s.output_index))
        .collect();
    assert_eq!(indices, [(0, 0), (1, 1), (2, 2), (4, 3)]);
    // The name is expected back as a name, and other tags as they were.
    assert_eq!(
        plan.expected_streams()[0].tags,
        map(&[("handler_name", "Core Media Video"), ("name", "Cam A")])
    );

    let args = plan
        .ffmpeg_args(
            Path::new("in.mp4"),
            Path::new("out.mp4"),
            MuxAttempt::Standard,
        )
        .unwrap();
    let tail: Vec<&str> = args
        .iter()
        .skip_while(|a| *a != "-dn")
        .skip(1)
        .take_while(|a| *a != "-fflags")
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "-metadata",
            "title=N",
            "-metadata:s:0",
            "title=Cam A",
            "-metadata:s:1",
            "title=Mic",
            "-metadata:s:2",
            "title=Subs EN",
            "-metadata:s:3",
            "title=Cover",
        ]
    );
}

#[test]
fn an_iso_stream_title_tag_is_refused_and_matroska_needs_no_restore() {
    let titled = inspection(&[], vec![video(&[("title", "Cam")]), audio(&[])], 0);
    for ext in ["mp4", "mov", "m4v"] {
        let refusal = data_refusal(ext, &titled);
        assert_eq!(refusal, EditRefusal::StreamTitle, "{ext}");
        assert!(!refusal.message().contains("Cam"));
    }
    for ext in ["mkv", "webm", "avi"] {
        let plan = plan(ext, vec![set(EditableField::Title, "N")], &titled).unwrap();
        assert!(plan.restores().is_empty(), "{ext}");
        assert_eq!(plan.expected_streams()[0].tags, map(&[("title", "Cam")]));
    }
}

#[test]
fn stream_keys_the_muxer_writes_are_not_expected() {
    assert_eq!(stream_muxer_owned_keys(EditMuxer::Mp4), ["vendor_id"]);
    assert_eq!(stream_muxer_owned_keys(EditMuxer::Mov), ["vendor_id"]);
    assert_eq!(stream_muxer_owned_keys(EditMuxer::Matroska), ["duration"]);
    assert_eq!(stream_muxer_owned_keys(EditMuxer::WebM), ["duration"]);
    assert!(stream_muxer_owned_keys(EditMuxer::Avi).is_empty());

    let fresh = inspection(
        &[],
        vec![
            video(&[
                ("vendor_id", "appl"),
                ("DURATION", "00:00:01.48"),
                ("BPS-eng", "123"),
            ]),
            audio(&[]),
        ],
        0,
    );
    let mov = plan("mov", vec![set(EditableField::Title, "N")], &fresh).unwrap();
    assert_eq!(
        mov.expected_streams()[0].tags,
        map(&[("bps-eng", "123"), ("duration", "00:00:01.48")])
    );
    let mkv = plan("mkv", vec![set(EditableField::Title, "N")], &fresh).unwrap();
    assert_eq!(
        mkv.expected_streams()[0].tags,
        map(&[("bps-eng", "123"), ("vendor_id", "appl")])
    );
}

#[test]
fn conflicting_stream_tags_are_refused() {
    let fresh = inspection(
        &[],
        vec![
            video(&[("LANGUAGE", "eng"), ("language", "ita")]),
            audio(&[]),
        ],
        0,
    );
    assert_eq!(
        data_refusal("mkv", &fresh),
        EditRefusal::ConflictingStreamMetadata
    );
}

// ------------------------------------------------------------------- Argv ---

fn expected_args(ext: &str, attempt: MuxAttempt) -> Vec<String> {
    let format = format_profile(ext).unwrap();
    let iso = format.container == ContainerProfile::IsoBmff;
    let mut args = vec!["-n"];
    if ext == "avi" {
        args.push("-ignore_unknown");
    }
    args.extend([
        "-i",
        "IN",
        "-map",
        "0",
        "-c",
        "copy",
        "-map_metadata",
        "0",
        "-map_chapters",
        "0",
    ]);
    if iso {
        args.push("-dn");
    }
    args.extend([
        "-metadata",
        "title=Città – 東京",
        "-metadata",
        "comment=",
        "-fflags",
        "+bitexact",
    ]);
    if attempt == MuxAttempt::FastStart {
        args.extend(["-movflags", "+faststart"]);
    }
    args.extend(["-f", format.output_muxer, "OUT"]);
    args.into_iter().map(String::from).collect()
}

#[test]
fn the_argv_contract_is_exact_for_every_format_and_attempt() {
    let fresh = av(&[("comment", "Old"), ("copyright", "Kept")]);
    for ext in ALL_EXTENSIONS {
        let plan = plan(
            ext,
            vec![
                set(EditableField::Title, "  Città – 東京  "),
                remove(EditableField::Comment),
                fill(EditableField::Copyright, "Ignored"),
            ],
            &fresh,
        )
        .unwrap();
        let attempts = plan.mux_attempts();
        assert_eq!(attempts, plan.format().mux_attempts(), "{ext}");
        let iso = matches!(ext, "mp4" | "mov" | "m4v");
        assert_eq!(
            attempts,
            if iso {
                &[MuxAttempt::FastStart, MuxAttempt::Standard][..]
            } else {
                &[MuxAttempt::Standard][..]
            },
            "{ext}"
        );
        for attempt in attempts {
            assert_eq!(
                plan.ffmpeg_args(Path::new("IN"), Path::new("OUT"), *attempt)
                    .unwrap(),
                expected_args(ext, *attempt),
                "{ext} {attempt:?}"
            );
        }
    }
}

#[test]
fn edit_argv_is_always_a_stream_copy_and_never_cleans() {
    let fresh = inspection(
        &[("title", "T")],
        vec![video(&[]), audio(&[]), subtitle(&[])],
        0,
    );
    for ext in ALL_EXTENSIONS {
        let plan = plan(ext, vec![set(EditableField::Title, "N")], &fresh).unwrap();
        for attempt in plan.mux_attempts() {
            let args = plan
                .ffmpeg_args(Path::new("IN"), Path::new("OUT"), *attempt)
                .unwrap();
            // Exactly one codec argument, and it is `-c copy`.
            let codec_args: Vec<(usize, &String)> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    a.starts_with("-c")
                        || a.starts_with("-codec")
                        || a.starts_with("-vcodec")
                        || a.starts_with("-acodec")
                        || a.starts_with("-scodec")
                })
                .collect();
            assert_eq!(codec_args.len(), 1, "{ext}: {args:?}");
            let (position, flag) = codec_args[0];
            assert_eq!((flag.as_str(), args[position + 1].as_str()), ("-c", "copy"));
            // Nothing that belongs to Clean, and no filter or encoder option.
            for forbidden in [
                "-sn",
                "-map_metadata:s",
                "-vf",
                "-af",
                "-filter_complex",
                "-crf",
                "-b:v",
                "-b:a",
                "-y",
            ] {
                assert!(!args.iter().any(|a| a == forbidden), "{ext}: {forbidden}");
            }
            for (flag, value) in [
                ("-map_metadata", "-1"),
                ("-map", "-0:t?"),
                ("-map", "-0:disp:attached_pic"),
                ("-map_chapters", "-1"),
            ] {
                assert!(
                    !args.windows(2).any(|w| w[0] == flag && w[1] == value),
                    "{ext}: {flag} {value}"
                );
            }
        }
    }
}

// ------------------------------------------------------ Command-line budget ---

/// The same inspection, read from another path.
fn at(fresh: &FreshInspection, path: &str) -> FreshInspection {
    FreshInspection::from_report(path, fresh.report().clone())
}

fn plan_in(
    ext: &str,
    edits: Vec<MetadataEdit>,
    fresh: &FreshInspection,
    out_dir: &str,
) -> Result<EditPlan, EditRefusal> {
    plan_edit(
        &valid(edits),
        format_profile(ext).unwrap(),
        fresh,
        Path::new(out_dir),
    )
}

/// The longest request the validator accepts: five 1,000-character values
/// and a date.
fn max_edits() -> Vec<MetadataEdit> {
    let max = "y".repeat(1000);
    [
        EditableField::Title,
        EditableField::Artist,
        EditableField::Comment,
        EditableField::Copyright,
        EditableField::Genre,
    ]
    .into_iter()
    .map(|field| set(field, &max))
    .chain([set(EditableField::Date, "2026-09-25")])
    .collect()
}

/// A path of about `len` characters, one long folder name deep.
fn long_path(len: usize, file: &str) -> String {
    format!(r"C:\{}\{file}", "d".repeat(len))
}

/// What the budget measures for one attempt: the full command, program
/// included, against the stand-in output path planning uses.
fn measured(plan: &EditPlan, input: &str, out_dir: &str, attempt: MuxAttempt) -> usize {
    let output = edit_temp_path(Path::new(out_dir), &format_id(0), Path::new(input));
    let args = plan.build_args(Path::new(input), &output, attempt);
    command_line_length(&ffmpeg_program().to_string_lossy(), &args)
}

#[test]
fn the_length_estimate_is_an_upper_bound_on_windows_quoting() {
    // Two quotes and a separator per argument, plus one per `"` or `\`.
    assert_eq!(command_line_length("ffmpeg", &[]), 6 + 3);
    assert_eq!(
        command_line_length("ffmpeg", &["-n".into(), r#"a"b\c"#.into()]),
        (6 + 3) + (2 + 3) + (5 + 2 + 3)
    );
    // UTF-16 units, not bytes and not characters: an emoji is two.
    assert_eq!(command_line_length("", &["🎬".into()]), 3 + 2 + 3);
}

#[test]
fn a_normal_edit_is_well_within_the_budget() {
    let plan = plan("mp4", vec![set(EditableField::Title, "Holiday")], &av(&[])).unwrap();
    for attempt in plan.mux_attempts() {
        assert!(plan
            .ffmpeg_args(Path::new(INPUT), Path::new(r"C:\out\x.mp4"), *attempt)
            .is_ok());
        assert!(measured(&plan, INPUT, OUT_DIR, *attempt) < COMMAND_LINE_BUDGET / 8);
    }
}

#[test]
fn six_maximum_length_values_fit_with_ordinary_paths() {
    let input = r"C:\Users\someone\Videos\Holiday 2026\clip.mp4";
    let out_dir = r"C:\Users\someone\Videos\Cleaned";
    for ext in ALL_EXTENSIONS {
        let plan = plan_in(ext, max_edits(), &at(&av(&[]), input), out_dir)
            .unwrap_or_else(|r| panic!("{ext}: {}", r.message()));
        for attempt in plan.mux_attempts() {
            let length = measured(&plan, input, out_dir, *attempt);
            assert!(length < COMMAND_LINE_BUDGET / 2, "{ext}: {length}");
        }
    }
}

#[test]
fn the_input_and_output_paths_count_toward_the_budget() {
    let request = || vec![set(EditableField::Title, "N")];
    let fresh = av(&[]);
    let long_input = long_path(COMMAND_LINE_BUDGET, "clip.mp4");
    assert_eq!(
        plan_in("mp4", request(), &at(&fresh, &long_input), OUT_DIR).unwrap_err(),
        EditRefusal::CommandLineTooLong
    );
    let long_out = long_path(COMMAND_LINE_BUDGET, "out");
    assert_eq!(
        plan_in("mp4", request(), &fresh, &long_out).unwrap_err(),
        EditRefusal::CommandLineTooLong
    );
    // Either one at a fifth of the budget is fine.
    let fifth = COMMAND_LINE_BUDGET / 5;
    assert!(plan_in(
        "mp4",
        request(),
        &at(&fresh, &long_path(fifth, "clip.mp4")),
        OUT_DIR
    )
    .is_ok());
    assert!(plan_in("mp4", request(), &fresh, &long_path(fifth, "out")).is_ok());

    // The runner's own call is checked the same way: a plan made for one
    // folder cannot be launched into a folder whose path does not fit.
    let plan = plan_in("mp4", request(), &fresh, OUT_DIR).unwrap();
    let too_far = PathBuf::from(long_path(COMMAND_LINE_BUDGET, "out.mp4"));
    for attempt in plan.mux_attempts() {
        assert_eq!(
            plan.ffmpeg_args(Path::new(INPUT), &too_far, *attempt)
                .unwrap_err(),
            EditRefusal::CommandLineTooLong
        );
    }
}

#[test]
fn a_restored_iso_track_name_counts_and_can_refuse() {
    let huge = "n".repeat(COMMAND_LINE_BUDGET);
    let named = inspection(&[], vec![video(&[("name", huge.as_str())]), audio(&[])], 0);
    for ext in ["mp4", "mov", "m4v"] {
        assert_eq!(
            plan(ext, vec![set(EditableField::Title, "N")], &named).unwrap_err(),
            EditRefusal::CommandLineTooLong,
            "{ext}"
        );
    }
    // Matroska needs no restore, so the same name adds nothing to its command.
    let titled = inspection(&[], vec![video(&[("title", huge.as_str())]), audio(&[])], 0);
    assert!(plan("mkv", vec![set(EditableField::Title, "N")], &titled).is_ok());
    // A long but ordinary name fits.
    let ok = "n".repeat(3_000);
    let named = inspection(&[], vec![video(&[("name", ok.as_str())]), audio(&[])], 0);
    assert!(plan("mp4", vec![set(EditableField::Title, "N")], &named).is_ok());
}

#[test]
fn the_total_command_line_decides_not_any_single_part() {
    let name = "n".repeat(1_500);
    let named = || {
        inspection(
            &[],
            vec![
                video(&[("name", name.as_str())]),
                audio(&[("name", name.as_str())]),
            ],
            0,
        )
    };
    let input = long_path(4_500, "clip.mp4");
    let out_dir = long_path(4_500, "out");
    let small = || vec![set(EditableField::Title, "N")];

    // Each part fits on its own, and so does any one of them with the rest
    // kept small...
    assert!(plan_in("mp4", max_edits(), &av(&[]), OUT_DIR).is_ok());
    assert!(plan_in("mp4", small(), &at(&av(&[]), &input), &out_dir).is_ok());
    assert!(plan_in("mp4", small(), &named(), OUT_DIR).is_ok());
    // ...metadata and track names together still fit, which is all a
    // metadata-only count would ever see...
    assert!(plan_in("mp4", max_edits(), &named(), OUT_DIR).is_ok());
    // ...but everything together does not.
    assert_eq!(
        plan_in("mp4", max_edits(), &at(&named(), &input), &out_dir).unwrap_err(),
        EditRefusal::CommandLineTooLong
    );
}

#[test]
fn every_attempt_must_fit_not_only_the_first() {
    let fresh = av(&[]);
    let request = || vec![set(EditableField::Title, "N")];
    let base = plan_in("mp4", request(), &fresh, OUT_DIR).unwrap();
    let fast = measured(&base, INPUT, OUT_DIR, MuxAttempt::FastStart);
    let standard = measured(&base, INPUT, OUT_DIR, MuxAttempt::Standard);
    assert!(fast > standard, "faststart adds -movflags +faststart");

    // Lengthen the input path until the standard attempt is exactly at the
    // budget: it fits, the faststart attempt does not, and the file is
    // refused rather than planned with only one of its attempts checked.
    let pad = |extra: usize| format!(r"C:\videos{}\clip.mp4", "p".repeat(extra));
    let at_limit = pad(COMMAND_LINE_BUDGET - standard);
    let plan_at_limit = plan_in("mp4", request(), &at(&fresh, &at_limit), OUT_DIR);
    assert_eq!(plan_at_limit.unwrap_err(), EditRefusal::CommandLineTooLong);
    let output = edit_temp_path(Path::new(OUT_DIR), &format_id(0), Path::new(&at_limit));
    assert!(base
        .ffmpeg_args(Path::new(&at_limit), &output, MuxAttempt::Standard)
        .is_ok());
    assert_eq!(
        base.ffmpeg_args(Path::new(&at_limit), &output, MuxAttempt::FastStart)
            .unwrap_err(),
        EditRefusal::CommandLineTooLong
    );

    // Short enough for faststart too, and the file is planned.
    let both_fit = pad(COMMAND_LINE_BUDGET - fast);
    assert!(plan_in("mp4", request(), &at(&fresh, &both_fit), OUT_DIR).is_ok());
}

#[test]
fn a_file_with_several_problems_always_gets_the_same_reason() {
    // Cover art in MOV and an ambiguous requested field: structure wins.
    let fresh = inspection(
        &[
            ("TITLE", "a"),
            ("title", "b"),
            ("com.apple.quicktime.make", "Apple"),
        ],
        vec![video(&[]), cover(&[])],
        0,
    );
    assert_eq!(data_refusal("mov", &fresh), EditRefusal::CoverArt);
    // Without the cover: the ambiguous field comes before the lost key.
    let fresh = inspection(
        &[
            ("TITLE", "a"),
            ("title", "b"),
            ("com.apple.quicktime.make", "Apple"),
        ],
        vec![video(&[])],
        0,
    );
    assert_eq!(
        data_refusal("mov", &fresh),
        EditRefusal::AmbiguousField(EditableField::Title)
    );
}

// ------------------------------------------------- Real FFmpeg, real files ---

/// Run a plan the way the batch runner will: each attempt in order, the
/// partial output removed after a failed one.
fn run_plan(plan: &EditPlan, input: &Path, output: &Path) {
    let mut last = String::new();
    for attempt in plan.mux_attempts() {
        let _ = std::fs::remove_file(output);
        let result = crate::sidecar::ffmpeg()
            .args(plan.ffmpeg_args(input, output, *attempt).unwrap())
            .output()
            .expect("ffmpeg must be available for these tests");
        if result.status.success() {
            return;
        }
        last = String::from_utf8_lossy(&result.stderr).into_owned();
    }
    panic!("every attempt failed for {input:?}: {last}");
}

/// The output carries exactly what the plan expects: globals, every stream in
/// order with its parameters and tags, the rebuilt tracks and the chapters.
/// A preview of what the Edit verifier will check, kept here so the planner's
/// arguments are proven against real FFmpeg now.
fn assert_output_matches(plan: &EditPlan, output: &Path) {
    let out = inspect::inspect(output).unwrap();
    assert_eq!(
        NormalizedGlobals::from_report(&out, plan.muxer()).fields(),
        plan.expected_globals(),
        "globals of {output:?}"
    );
    let kept: Vec<&StreamSummary> = out
        .streams
        .iter()
        .filter(|s| s.kind != StreamKind::Data)
        .collect();
    assert_eq!(
        kept.len(),
        plan.expected_streams().len(),
        "streams of {output:?}"
    );
    for (want, got) in plan.expected_streams().iter().zip(&kept) {
        assert_eq!(want.kind, got.kind);
        assert_eq!(want.identity, got.identity);
        assert_eq!(want.attached_pic, got.attached_pic);
        assert_eq!(
            want.tags,
            stream_tags(&out, got, plan.muxer()).unwrap(),
            "tags of output stream {}",
            want.output_index
        );
    }
    let mut rebuilt: Vec<RebuiltTrack> = out
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Data)
        .map(|s| {
            if is_chapter_track(s) {
                RebuiltTrack::Chapters
            } else if is_timecode_track(s) {
                RebuiltTrack::Timecode
            } else {
                panic!("unexpected data track in {output:?}")
            }
        })
        .collect();
    rebuilt.sort();
    assert_eq!(rebuilt, plan.rebuilt_tracks());
    let chapters =
        |report: &[ChapterSummary]| -> Vec<(Option<String>, Option<String>, Option<String>)> {
            report
                .iter()
                .map(|c| (c.title.clone(), c.start_time.clone(), c.end_time.clone()))
                .collect()
        };
    assert_eq!(chapters(&out.chapters), chapters(plan.expected_chapters()));
}

fn full_request() -> Vec<MetadataEdit> {
    vec![
        set(EditableField::Title, "Città – 東京 – 🎬"),
        set(EditableField::Artist, "Emilio"),
        remove(EditableField::Comment),
        fill(EditableField::Copyright, "Ignored because present"),
        fill(EditableField::Genre, "Jazz"),
        set(EditableField::Date, "2026-09-25"),
    ]
}

#[test]
fn the_planned_argv_edits_every_format_for_real() {
    for ext in ALL_EXTENSIONS {
        let dir = scratch(&format!("edit-plan-{ext}"));
        let input = if ext == "avi" {
            sample_plain_avi(&dir)
        } else {
            sample_for_format(&dir, ext)
        };
        let before = std::fs::read(&input).unwrap();
        let fresh = FreshInspection::take(&input).unwrap();
        let plan = plan_edit(
            &valid(full_request()),
            format_profile(ext).unwrap(),
            &fresh,
            &dir,
        )
        .unwrap_or_else(|refusal| panic!("{ext} refused: {}", refusal.message()));

        // The fixture's copyright is present, so Fill keeps it; its genre is
        // missing, so Fill writes it.
        let change = |field| {
            plan.actions()
                .iter()
                .find(|a| a.field == field)
                .unwrap()
                .change
                .clone()
        };
        assert_eq!(change(EditableField::Copyright), FieldChange::Keep, "{ext}");
        assert_eq!(
            change(EditableField::Genre),
            FieldChange::Write("Jazz".into()),
            "{ext}"
        );

        let output = dir.join(format!("edited.{ext}"));
        run_plan(&plan, &input, &output);
        assert_output_matches(&plan, &output);
        assert_eq!(
            std::fs::read(&input).unwrap(),
            before,
            "{ext}: the original changed"
        );
        for stream in ["0:v:0", "0:a:0"] {
            assert_eq!(
                stream_payload_hash(&input, stream),
                stream_payload_hash(&output, stream),
                "{ext} {stream} was not copied"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn iso_track_names_survive_a_real_edit() {
    let dir = scratch("edit-plan-names");
    let input = sample_for_format(&dir, "mp4");
    let fresh = FreshInspection::take(&input).unwrap();
    let plan = plan_edit(
        &valid(vec![set(EditableField::Title, "N")]),
        format_profile("mp4").unwrap(),
        &fresh,
        &dir,
    )
    .unwrap();
    assert_eq!(plan.restores()[0].title, "STREAM_SECRET");
    let output = dir.join("edited.mp4");
    run_plan(&plan, &input, &output);
    assert_output_matches(&plan, &output);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp4_cover_art_survives_a_real_edit() {
    for ext in ["mp4", "m4v"] {
        let dir = scratch(&format!("edit-plan-cover-{ext}"));
        let cover = cover_jpeg(&dir, true);
        let input = sample_with_cover(&dir, ext, &cover);
        let fresh = FreshInspection::take(&input).unwrap();
        assert_eq!(fresh.report().attached_pictures().count(), 1);
        let plan = plan_edit(
            &valid(full_request()),
            format_profile(ext).unwrap(),
            &fresh,
            &dir,
        )
        .unwrap();
        let output = dir.join(format!("edited.{ext}"));
        run_plan(&plan, &input, &output);
        assert_output_matches(&plan, &output);
        // Located by its mark in each file. Not by the plan's output index:
        // the muxer writes the rebuilt chapter track ahead of the cover.
        let written = inspect::inspect(&output).unwrap();
        let output_cover = written.attached_pictures().next().unwrap().index;
        let input_cover = fresh.report().attached_pictures().next().unwrap().index;
        assert_eq!(
            stream_payload_hash(&input, &format!("0:{input_cover}")),
            stream_payload_hash(&output, &format!("0:{output_cover}")),
            "{ext}: the cover was not copied"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn real_files_edit_cannot_keep_are_refused() {
    let dir = scratch("edit-plan-refusals");
    let request = valid(vec![set(EditableField::Title, "N")]);
    let refusal = |path: &Path, ext: &str| {
        plan_edit(
            &request,
            format_profile(ext).unwrap(),
            &FreshInspection::take(path).unwrap(),
            &dir,
        )
        .unwrap_err()
    };

    // The shared AVI fixture carries a data stream FFmpeg reads as Unknown.
    let avi = sample_for_format(&dir, "avi");
    assert_eq!(refusal(&avi, "avi"), EditRefusal::UnknownStream);

    // Cover art the MOV muxer would drop, and a Matroska cover that would
    // come out as an ordinary video track.
    let cover = cover_jpeg(&dir, false);
    for ext in ["mov", "mkv"] {
        let with_cover = sample_with_cover(&dir, ext, &cover);
        assert_eq!(refusal(&with_cover, ext), EditRefusal::CoverArt, "{ext}");
    }

    // An iPhone-style MP4 whose `mdta` keys the MP4 muxer would drop.
    let mdta = dir.join("mdta.mp4");
    let built = crate::sidecar::ffmpeg()
        .args([
            "-y",
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
            "-metadata",
            "com.apple.quicktime.location.ISO6709=+45.4642+009.1900+120.000/",
            "-movflags",
            "use_metadata_tags",
            "-f",
            "mp4",
        ])
        .arg(&mdta)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(
        refusal(&mdta, "mp4"),
        EditRefusal::UnpreservedMetadata(vec![
            "com.apple.quicktime.location.iso6709".into(),
            "com.apple.quicktime.make".into(),
        ])
    );
    let _ = std::fs::remove_dir_all(&dir);
}
