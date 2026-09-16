//! Classifier tests.
//!
//! These are pure: no FFmpeg, no filesystem. A rule table is only worth having
//! if its behaviour is pinned down, and these assertions are the pin.

use super::*;
use crate::inspect::{MetadataField, MetadataScope, StreamIdentity, StreamKind, StreamSummary};

fn field(key: &str, value: &str) -> MetadataField {
    MetadataField {
        key: key.to_ascii_lowercase(),
        display_key: key.to_string(),
        value: value.to_string(),
        scope: MetadataScope::Format,
        stream_index: None,
        stream_kind: None,
    }
}

fn category_of(key: &str) -> PrivacyCategory {
    classify_field(&field(key, "x")).0
}

fn severity_of(key: &str) -> Severity {
    classify_field(&field(key, "x")).1
}

#[test]
fn location_keys_are_high_across_every_spelling() {
    for key in [
        "location",
        "location-eng",
        "com.apple.quicktime.location.ISO6709",
        "com.apple.quicktime.location.accuracy.horizontal",
        "GPSLatitude",
        "GPSLongitude",
        "gps_coordinates",
        "geotag_lat",
    ] {
        assert_eq!(category_of(key), PrivacyCategory::Location, "{key}");
        assert_eq!(severity_of(key), Severity::High, "{key}");
    }
}

#[test]
fn device_keys_are_medium_and_do_not_swallow_similar_words() {
    for key in [
        "make",
        "model",
        "com.apple.quicktime.make",
        "com.apple.quicktime.model",
        "com.android.manufacturer",
    ] {
        assert_eq!(category_of(key), PrivacyCategory::Device, "{key}");
        assert_eq!(severity_of(key), Severity::Medium, "{key}");
    }

    // The substring trap: `model` is inside `modification_time`, and `make` is
    // inside `maker_note`-style keys. Exact and suffix rules must win.
    assert_eq!(category_of("modification_time"), PrivacyCategory::Timestamp);
    assert_ne!(category_of("modification_time"), PrivacyCategory::Device);
}

#[test]
fn timestamps_software_creator_and_copyright_land_where_documented() {
    assert_eq!(category_of("creation_time"), PrivacyCategory::Timestamp);
    assert_eq!(severity_of("creation_time"), Severity::Medium);

    for key in ["encoder", "software", "writing_application", "encoded_by"] {
        assert_eq!(category_of(key), PrivacyCategory::Software, "{key}");
        assert_eq!(severity_of(key), Severity::Low, "{key}");
    }

    for key in ["artist", "author", "creator", "composer"] {
        assert_eq!(category_of(key), PrivacyCategory::CreatorIdentity, "{key}");
        assert_eq!(severity_of(key), Severity::Medium, "{key}");
    }

    assert_eq!(category_of("copyright"), PrivacyCategory::Copyright);
    assert_eq!(severity_of("copyright"), Severity::Low);
}

#[test]
fn serial_numbers_outrank_generic_identifiers() {
    assert_eq!(category_of("serial_number"), PrivacyCategory::Identifier);
    assert_eq!(severity_of("serial_number"), Severity::High);
    assert_eq!(category_of("uuid"), PrivacyCategory::Identifier);
    assert_eq!(severity_of("uuid"), Severity::Medium);
}

#[test]
fn an_unknown_key_is_unknown_and_never_automatically_high() {
    for key in ["zzz_vendor_blob", "some_random_key", "x-private-9"] {
        assert_eq!(category_of(key), PrivacyCategory::Unknown, "{key}");
        assert_eq!(severity_of(key), Severity::Low, "{key}");
    }
}

#[test]
fn free_text_fields_are_unknown_but_not_dismissed_as_low() {
    for key in ["title", "comment", "description"] {
        let (category, severity, explanation) = classify_field(&field(key, "anything"));
        assert_eq!(category, PrivacyCategory::Unknown, "{key}");
        assert_eq!(severity, Severity::Medium, "{key}");
        assert_eq!(explanation, FREE_TEXT_EXPLANATION, "{key}");
    }
}

#[test]
fn structural_container_bookkeeping_stays_low_and_is_marked_structural() {
    for key in [
        "major_brand",
        "minor_version",
        "compatible_brands",
        "handler_name",
        "language",
    ] {
        assert_eq!(category_of(key), PrivacyCategory::Structural, "{key}");
        assert!(category_of(key).is_structural(), "{key}");
        assert_eq!(severity_of(key), Severity::Low, "{key}");
    }
}

#[test]
fn coordinates_are_caught_by_value_even_under_an_innocuous_key() {
    // The key alone says nothing; the ISO 6709 shape of the value is the signal.
    let (category, severity, _) = classify_field(&field("vendor_blob", "+45.4642+009.1900/"));
    assert_eq!(category, PrivacyCategory::Location);
    assert_eq!(severity, Severity::High);

    // And it does not fire on ordinary text, version numbers or plain numbers.
    for value in [
        "a comment",
        "1.2.3",
        "-42",
        "Lavf62.12.102",
        "2026-08-31T13:27:00.000000Z",
        "",
    ] {
        assert_ne!(
            classify_field(&field("comment_like", value)).0,
            PrivacyCategory::Location,
            "false positive on {value:?}"
        );
    }
}

#[test]
fn every_category_has_a_distinct_non_empty_explanation() {
    let categories = [
        PrivacyCategory::Location,
        PrivacyCategory::Device,
        PrivacyCategory::Timestamp,
        PrivacyCategory::Software,
        PrivacyCategory::CreatorIdentity,
        PrivacyCategory::Telemetry,
        PrivacyCategory::Identifier,
        PrivacyCategory::Copyright,
        PrivacyCategory::Structural,
        PrivacyCategory::Unknown,
    ];
    let mut seen = std::collections::HashSet::new();
    for category in categories {
        let text = explanation(category);
        assert!(!text.is_empty(), "{category:?} has no explanation");
        assert!(
            text.ends_with('.'),
            "{category:?} explanation is not a sentence"
        );
        assert!(seen.insert(text), "{category:?} reuses another explanation");
        assert!(!category.label().is_empty());
    }
}

fn stream(kind: StreamKind, codec_tag: Option<&str>, handler: Option<&str>) -> StreamSummary {
    StreamSummary {
        index: 2,
        kind,
        identity: StreamIdentity {
            codec_name: Some("bin_data".into()),
            codec_tag: codec_tag.map(str::to_string),
            profile: None,
            width: None,
            height: None,
            pix_fmt: None,
            sample_rate: None,
            channels: None,
            channel_layout: None,
        },
        handler_name: handler.map(str::to_string),
        tag_count: 0,
    }
}

#[test]
fn telemetry_tracks_are_high_and_other_data_tracks_are_medium() {
    for tag in ["gpmd", "mebx", "fdsc", "rtmd"] {
        let (category, severity, _) =
            classify_stream(&stream(StreamKind::Data, Some(tag), None)).unwrap();
        assert_eq!(category, PrivacyCategory::Telemetry, "{tag}");
        assert_eq!(severity, Severity::High, "{tag}");
    }

    // Handler name is the fallback signal when the codec tag is generic.
    let (category, _, _) =
        classify_stream(&stream(StreamKind::Data, Some("text"), Some("GoPro MET"))).unwrap();
    assert_eq!(category, PrivacyCategory::Telemetry);

    // An ordinary data track is still a disclosure, just not a described one.
    let (category, severity, _) = classify_stream(&stream(
        StreamKind::Data,
        Some("text"),
        Some("SubtitleHandler"),
    ))
    .unwrap();
    assert_eq!(category, PrivacyCategory::Unknown);
    assert_eq!(severity, Severity::Medium);

    // Video and audio are never findings.
    assert!(classify_stream(&stream(StreamKind::Video, None, None)).is_none());
    assert!(classify_stream(&stream(StreamKind::Audio, None, None)).is_none());
}

#[test]
fn long_values_are_truncated_before_they_reach_a_finding() {
    let long = "A".repeat(500);
    let finding = finding_from_field(&field("comment", &long));
    assert!(
        finding.detail.chars().count() < 200,
        "value was not truncated: {} chars",
        finding.detail.chars().count()
    );
    assert!(finding.detail.ends_with('…'));

    // Newlines cannot break the row layout.
    let finding = finding_from_field(&field("comment", "one\ntwo\r\nthree"));
    assert!(!finding.detail.contains('\n') && !finding.detail.contains('\r'));
}

#[test]
fn summary_counts_match_the_findings_they_describe() {
    let findings = vec![
        finding_from_field(&field("location", "+45.4+009.1/")),
        finding_from_field(&field("make", "CanaryPhone")),
        finding_from_field(&field("encoder", "Lavf")),
        finding_from_field(&field("artist", "Someone")),
    ];
    let summary = PrivacySummary::of(&findings);
    assert_eq!(summary.total, 4);
    assert_eq!(summary.high, 1);
    assert_eq!(summary.medium, 2);
    assert_eq!(summary.low, 1);
    assert_eq!(summary.technical, 0);
    assert_eq!(summary.high + summary.medium + summary.low, summary.total);
}

#[test]
fn structural_fields_are_technical_and_never_privacy_findings() {
    let findings = vec![
        finding_from_field(&field("location", "+45.4+009.1/")),
        finding_from_field(&field("make", "CanaryPhone")),
        finding_from_field(&field("creation_time", "2024-01-01T00:00:00Z")),
        finding_from_field(&field("encoder", "Lavf")),
        finding_from_field(&field("major_brand", "isom")),
        finding_from_field(&field("minor_version", "512")),
        finding_from_field(&field("compatible_brands", "isomiso2avc1mp41")),
        finding_from_field(&field("handler_name", "VideoHandler")),
        finding_from_field(&field("language", "und")),
        finding_from_field(&field("vendor_id", "[0][0][0][0]")),
    ];
    let summary = PrivacySummary::of(&findings);

    // Privacy counts see only the four real disclosures, and severities add up.
    assert_eq!(summary.total, 4);
    assert_eq!(summary.high, 1);
    assert_eq!(summary.medium + summary.low, 3);
    assert_eq!(summary.high + summary.medium + summary.low, summary.total);
    // The six structural fields are counted apart, and are still in the list.
    assert_eq!(summary.technical, 6);
    assert_eq!(
        findings
            .iter()
            .filter(|f| f.category.is_structural())
            .count(),
        6
    );
}

#[test]
fn a_file_with_only_structural_metadata_has_zero_privacy_findings() {
    let findings: Vec<_> = ["major_brand", "minor_version", "handler_name", "language"]
        .iter()
        .map(|key| finding_from_field(&field(key, "x")))
        .collect();
    let summary = PrivacySummary::of(&findings);
    assert_eq!(summary.total, 0);
    assert_eq!((summary.high, summary.medium, summary.low), (0, 0, 0));
    assert_eq!(summary.technical, 4);
    assert!(findings
        .iter()
        .all(|f| is_technical_field(&field(&f.source_key, "x"))));
}
