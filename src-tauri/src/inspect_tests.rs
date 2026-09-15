//! Inspector tests.
//!
//! The parsing half runs against fixed JSON documents with no process involved,
//! so a malformed or surprising ffprobe response is a test case rather than a
//! field report. The end-to-end half runs the real ffprobe over real fixtures.

use super::*;
use crate::testkit::{sample_for_format, scratch};

const SAMPLE: &str = r#"{
  "streams": [
    {
      "index": 0, "codec_name": "h264", "profile": "High", "codec_type": "video",
      "codec_tag_string": "avc1", "width": 1920, "height": 1080, "pix_fmt": "yuv420p",
      "tags": { "language": "und", "handler_name": "VideoHandler", "title": "STREAM_SECRET" }
    },
    {
      "index": 1, "codec_name": "aac", "codec_type": "audio", "codec_tag_string": "mp4a",
      "sample_rate": "44100", "channels": 2, "channel_layout": "stereo",
      "tags": { "language": "eng" }
    },
    {
      "index": 2, "codec_name": "bin_data", "codec_type": "data",
      "codec_tag_string": "gpmd", "tags": { "handler_name": "GoPro MET" }
    }
  ],
  "chapters": [
    { "id": 0, "start_time": "0.000000", "end_time": "1.200000", "tags": { "title": "CHAPTER_SECRET" } }
  ],
  "format": {
    "filename": "x.mp4", "nb_streams": 3, "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
    "duration": "2.000000", "size": "31199",
    "tags": { "major_brand": "isom", "title": "GLOBAL_SECRET", "com.apple.quicktime.make": "Apple" }
  }
}"#;

#[test]
fn every_scope_of_metadata_is_captured_with_its_origin() {
    let report = parse_ffprobe_json("x.mp4", SAMPLE).unwrap();

    assert_eq!(report.file_name, "x.mp4");
    assert_eq!(report.file_size, Some(31199));
    assert_eq!(report.duration_seconds, Some(2.0));
    assert_eq!(report.container.as_deref(), Some("mov,mp4,m4a,3gp,3g2,mj2"));

    let find = |key: &str| report.fields.iter().find(|f| f.key == key).unwrap();

    // Container scope.
    let title = find("title");
    assert_eq!(title.scope, MetadataScope::Format);
    assert_eq!(title.value, "GLOBAL_SECRET");
    assert_eq!(title.stream_index, None);

    // Per-stream scope, which the FFMETADATA route cannot see at all. This is
    // the capability ffprobe was bundled for.
    let stream_title = report
        .fields
        .iter()
        .find(|f| f.scope == MetadataScope::Stream && f.value == "STREAM_SECRET")
        .expect("per-stream metadata was not captured");
    assert_eq!(stream_title.stream_index, Some(0));
    assert_eq!(stream_title.stream_kind, Some(StreamKind::Video));

    // Chapter scope.
    let chapter_title = report
        .fields
        .iter()
        .find(|f| f.scope == MetadataScope::Chapter)
        .unwrap();
    assert_eq!(chapter_title.value, "CHAPTER_SECRET");

    // Keys are normalised for matching but keep their original spelling.
    let make = find("com.apple.quicktime.make");
    assert_eq!(make.display_key, "com.apple.quicktime.make");
}

#[test]
fn streams_are_classified_and_codec_identity_is_captured() {
    let report = parse_ffprobe_json("x.mp4", SAMPLE).unwrap();
    assert_eq!(report.streams.len(), 3);

    let video = &report.streams[0];
    assert_eq!(video.kind, StreamKind::Video);
    assert_eq!(video.identity.codec_name.as_deref(), Some("h264"));
    assert_eq!(video.identity.width, Some(1920));
    assert_eq!(video.identity.pix_fmt.as_deref(), Some("yuv420p"));

    let audio = &report.streams[1];
    assert_eq!(audio.kind, StreamKind::Audio);
    // Reported by ffprobe as a string; kept as one so before/after compare exactly.
    assert_eq!(audio.identity.sample_rate.as_deref(), Some("44100"));
    assert_eq!(audio.identity.channels, Some(2));

    let data = &report.streams[2];
    assert_eq!(data.kind, StreamKind::Data);
    assert_eq!(data.identity.codec_tag.as_deref(), Some("gpmd"));
    assert_eq!(data.handler_name.as_deref(), Some("GoPro MET"));

    assert_eq!(report.media_streams().count(), 2);
    assert_eq!(report.non_media_streams().count(), 1);
    assert_eq!(report.chapters.len(), 1);
}

#[test]
fn numbers_are_accepted_whether_ffprobe_quotes_them_or_not() {
    // ffprobe is inconsistent about this between fields and versions.
    let quoted = r#"{"format":{"size":"123","duration":"4.5"},"streams":[{"index":0,"codec_type":"video","width":"640"}]}"#;
    let bare = r#"{"format":{"size":123,"duration":4.5},"streams":[{"index":0,"codec_type":"video","width":640}]}"#;

    for raw in [quoted, bare] {
        let report = parse_ffprobe_json("x.mp4", raw).unwrap();
        assert_eq!(report.file_size, Some(123));
        assert_eq!(report.duration_seconds, Some(4.5));
        assert_eq!(report.streams[0].identity.width, Some(640));
    }
}

#[test]
fn a_sparse_or_unusual_document_degrades_instead_of_panicking() {
    // No format, no streams, no chapters.
    let empty = parse_ffprobe_json("x.mp4", "{}").unwrap();
    assert!(empty.fields.is_empty());
    assert!(empty.streams.is_empty());
    assert!(empty.chapters.is_empty());
    assert_eq!(empty.file_size, None);

    // An unrecognised codec_type is Unknown, not a crash, and still counts as a
    // non-media track so the cleaner's `-dn` is still described correctly.
    let odd = parse_ffprobe_json(
        "x.mp4",
        r#"{"streams":[{"index":0,"codec_type":"quantum"},{"index":1}]}"#,
    )
    .unwrap();
    assert_eq!(odd.streams[0].kind, StreamKind::Unknown);
    assert_eq!(odd.streams[1].kind, StreamKind::Unknown);
    assert_eq!(odd.non_media_streams().count(), 2);

    // A non-string tag value is stringified rather than lost.
    let numeric = parse_ffprobe_json("x.mp4", r#"{"format":{"tags":{"track":7}}}"#).unwrap();
    assert_eq!(numeric.fields[0].value, "7");
}

#[test]
fn malformed_output_is_an_error_not_a_panic() {
    assert!(parse_ffprobe_json("x.mp4", "not json at all").is_err());
    assert!(parse_ffprobe_json("x.mp4", "").is_err());
    // Valid JSON, wrong shape.
    assert!(parse_ffprobe_json("x.mp4", "[1,2,3]").is_err());
    assert!(parse_ffprobe_json("x.mp4", "\"a string\"").is_err());
}

#[test]
fn inspecting_a_missing_or_unreadable_file_reports_instead_of_failing() {
    let dir = scratch("inspect-bad");
    let missing = dir.join("nope.mp4");
    assert!(inspect(&missing).is_err());

    // A real file that is not media at all.
    let junk = dir.join("junk.mp4");
    std::fs::write(&junk, b"this is not a video").unwrap();
    let error = inspect(&junk).unwrap_err();
    assert!(
        error.starts_with("This file could not be inspected"),
        "unexpected error: {error}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// End to end against the real bundled ffprobe, so a schema change upstream
/// shows up here rather than in the UI.
#[test]
fn a_real_fixture_is_inspected_through_the_bundled_ffprobe() {
    let dir = scratch("inspect-real");
    let input = sample_for_format(&dir, "mp4");

    let report = inspect(&input).unwrap();
    assert_eq!(report.file_name, "fixture.mp4");
    assert!(report.file_size.unwrap() > 0);
    assert!(report.duration_seconds.unwrap() > 1.0);
    assert!(report.container.as_deref().unwrap().contains("mp4"));

    // The planted canaries really are visible to the inspector.
    let values: Vec<&str> = report.fields.iter().map(|f| f.value.as_str()).collect();
    assert!(
        values.contains(&"GLOBAL_SECRET"),
        "container metadata missing: {values:?}"
    );
    assert!(
        report
            .fields
            .iter()
            .any(|f| f.scope == MetadataScope::Stream),
        "no per-stream metadata was found"
    );
    assert_eq!(report.media_streams().count(), 2);

    std::fs::remove_dir_all(&dir).unwrap();
}
