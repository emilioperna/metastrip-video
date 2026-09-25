use super::*;
use crate::inspect::parse_ffprobe_json;
use crate::{format_profile, FORMAT_PROFILES};

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

fn request(paths: usize, edits: Vec<MetadataEdit>) -> EditRequest {
    EditRequest {
        paths: (0..paths)
            .map(|i| format!("C:\\videos\\clip{i}.mp4"))
            .collect(),
        edits,
    }
}

fn one_edit(edit: MetadataEdit) -> Result<ValidEditRequest, String> {
    request(1, vec![edit]).validate()
}

/// The single validated value of a one-edit request.
fn validated(field: EditableField, value: &str) -> Result<String, String> {
    one_edit(set(field, value)).map(|valid| match &valid.edits()[0].operation {
        EditOperation::Set(value) => value.clone(),
        other => panic!("expected a Set, got {other:?}"),
    })
}

/// A report with these container-level tags, plus one stream tag that must
/// never reach the normalised globals.
fn report(tags: &[(&str, &str)]) -> MetadataReport {
    let tags: serde_json::Map<String, serde_json::Value> = tags
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect();
    let json = serde_json::json!({
        "format": { "format_name": "mov,mp4,m4a,3gp,3g2,mj2", "tags": tags },
        "streams": [{ "index": 0, "codec_type": "video", "tags": { "title": "Stream title" } }]
    });
    parse_ffprobe_json("x.mp4", &json.to_string()).unwrap()
}

fn keys(globals: &NormalizedGlobals) -> Vec<&str> {
    globals.fields().keys().map(String::as_str).collect()
}

fn muxer_for(extension: &str) -> EditMuxer {
    edit_muxer(format_profile(extension).unwrap()).unwrap()
}

// ---------------------------------------------------------- EditableField ---

#[test]
fn there_are_exactly_six_editable_fields_with_their_canonical_keys() {
    let keys: Vec<&str> = EditableField::ALL.iter().map(|f| f.key()).collect();
    assert_eq!(
        keys,
        ["title", "artist", "comment", "copyright", "genre", "date"]
    );
    let labels: Vec<&str> = EditableField::ALL.iter().map(|f| f.label()).collect();
    assert_eq!(
        labels,
        ["Title", "Artist", "Comment", "Copyright", "Genre", "Date"]
    );
    assert_eq!(MAX_EDITS, 6);
    for field in EditableField::ALL {
        assert_eq!(EditableField::from_key(field.key()), Some(field));
    }
}

#[test]
fn only_date_is_validated_as_a_date() {
    for field in EditableField::ALL {
        let want = if field == EditableField::Date {
            ValueKind::Date
        } else {
            ValueKind::Text
        };
        assert_eq!(field.kind(), want, "{field:?}");
    }
}

#[test]
fn fields_that_are_not_editable_have_no_variant() {
    for key in [
        "album",
        "description",
        "creation_time",
        "location",
        "make",
        "model",
        "encoder",
        "Title",
    ] {
        assert_eq!(EditableField::from_key(key), None, "{key}");
        let json = format!("\"{key}\"");
        assert!(
            serde_json::from_str::<EditableField>(&json).is_err(),
            "{key} must not deserialise into a field"
        );
    }
}

#[test]
fn fields_serialise_as_their_canonical_keys() {
    for field in EditableField::ALL {
        let json = serde_json::to_string(&field).unwrap();
        assert_eq!(json, format!("\"{}\"", field.key()));
        assert_eq!(serde_json::from_str::<EditableField>(&json).unwrap(), field);
    }
}

#[test]
fn operations_and_requests_use_the_camel_case_ipc_shape() {
    let edits = vec![
        set(EditableField::Title, "A"),
        fill(EditableField::Copyright, "2026"),
        remove(EditableField::Comment),
    ];
    let json = serde_json::to_value(request(1, edits.clone())).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "paths": ["C:\\videos\\clip0.mp4"],
            "edits": [
                { "field": "title", "operation": { "kind": "set", "value": "A" } },
                { "field": "copyright", "operation": { "kind": "fillIfMissing", "value": "2026" } },
                { "field": "comment", "operation": { "kind": "remove" } }
            ]
        })
    );
    let back: EditRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back.edits, edits);
}

#[test]
fn a_malformed_operation_does_not_deserialise() {
    for operation in [
        r#"{"kind":"set"}"#,
        r#"{"kind":"replace","value":"x"}"#,
        r#"{"kind":"fillIfMissing","value":3}"#,
        r#"{"value":"x"}"#,
    ] {
        assert!(
            serde_json::from_str::<EditOperation>(operation).is_err(),
            "{operation}"
        );
    }
}

// ----------------------------------------------------- Request validation ---

#[test]
fn a_request_needs_between_one_and_a_hundred_paths() {
    let edits = || vec![set(EditableField::Title, "A")];
    assert_eq!(
        request(0, edits()).validate().unwrap_err(),
        "No videos selected"
    );
    assert!(request(1, edits()).validate().is_ok());
    assert!(request(100, edits()).validate().is_ok());
    assert_eq!(
        request(101, edits()).validate().unwrap_err(),
        "Too many videos: 101 selected, the limit is 100."
    );
}

#[test]
fn an_empty_path_is_rejected_without_touching_the_disk() {
    for blank in ["", "   "] {
        let mut req = request(3, vec![set(EditableField::Title, "A")]);
        req.paths[1] = blank.into();
        assert_eq!(req.validate().unwrap_err(), "A selected video has no path.");
    }
    // A path that does not exist is still well formed here; whether it is
    // there is checked when the file is processed.
    let req = EditRequest {
        paths: vec!["Z:\\nowhere\\missing.mkv".into()],
        edits: vec![set(EditableField::Title, "A")],
    };
    assert!(req.validate().is_ok());
}

#[test]
fn a_request_needs_at_least_one_edit() {
    assert_eq!(
        request(1, vec![]).validate().unwrap_err(),
        "Choose at least one field to edit."
    );
}

#[test]
fn a_field_may_be_edited_only_once() {
    for second in [
        set(EditableField::Artist, "B"),
        fill(EditableField::Artist, "B"),
        remove(EditableField::Artist),
    ] {
        let err = request(1, vec![set(EditableField::Artist, "A"), second])
            .validate()
            .unwrap_err();
        assert_eq!(
            err,
            "Artist is edited more than once. Choose one operation per field."
        );
    }
}

#[test]
fn all_six_fields_at_once_are_accepted_and_ordered() {
    // Listed in reverse: validation puts them in the one fixed order.
    let edits = vec![
        set(EditableField::Date, "2026-09-25"),
        remove(EditableField::Genre),
        fill(EditableField::Copyright, "2026 Owner"),
        set(EditableField::Comment, "note"),
        set(EditableField::Artist, "Emilio"),
        set(EditableField::Title, "Holiday"),
    ];
    let valid = request(2, edits).validate().unwrap();
    let fields: Vec<EditableField> = valid.edits().iter().map(|e| e.field).collect();
    assert_eq!(fields, EditableField::ALL);
    assert_eq!(valid.paths().len(), 2);
}

#[test]
fn a_seventh_edit_is_rejected() {
    let mut edits: Vec<MetadataEdit> = EditableField::ALL
        .iter()
        .map(|&field| set(field, "2026"))
        .collect();
    edits.push(remove(EditableField::Title));
    assert_eq!(
        request(1, edits).validate().unwrap_err(),
        "Too many edits: 7, the limit is 6, one per field."
    );
}

// ------------------------------------------------------------ Text values ---

#[test]
fn values_are_trimmed_before_they_are_checked_or_stored() {
    assert_eq!(
        validated(EditableField::Title, "  Holiday  ").unwrap(),
        "Holiday"
    );
    assert_eq!(validated(EditableField::Date, " 2026 ").unwrap(), "2026");
    // Inner spacing is the user's and is kept.
    assert_eq!(validated(EditableField::Title, "a  b").unwrap(), "a  b");
    let valid = one_edit(fill(EditableField::Genre, "\u{3000}Jazz ")).unwrap();
    assert_eq!(
        valid.edits()[0].operation,
        EditOperation::FillIfMissing("Jazz".into())
    );
}

#[test]
fn a_value_that_is_empty_after_trimming_is_rejected() {
    for blank in ["", "   ", "\u{00a0}"] {
        assert_eq!(
            validated(EditableField::Comment, blank).unwrap_err(),
            "Comment cannot be empty. Use Remove to delete it."
        );
        assert!(one_edit(fill(EditableField::Comment, blank)).is_err());
    }
}

#[test]
fn unicode_is_accepted_as_written() {
    let value = "Città – 東京 – 🎬 – Ümlaut – 👨‍👩‍👧 – עברית";
    assert_eq!(validated(EditableField::Title, value).unwrap(), value);
}

#[test]
fn the_length_limit_counts_characters_not_bytes() {
    // Three bytes a character: 3,000 bytes, and still exactly at the limit.
    let at_limit = "東".repeat(MAX_VALUE_CHARS);
    assert_eq!(MAX_VALUE_CHARS, 1000);
    assert_eq!(
        validated(EditableField::Comment, &at_limit).unwrap(),
        at_limit
    );

    let over = "a".repeat(MAX_VALUE_CHARS + 1);
    assert_eq!(
        validated(EditableField::Comment, &over).unwrap_err(),
        "Comment is too long: 1001 characters, the limit is 1000."
    );
    // Rejected, never cut down to fit.
    assert!(one_edit(fill(EditableField::Comment, &over)).is_err());
}

#[test]
fn line_breaks_and_control_characters_are_rejected() {
    for bad in [
        "line\rbreak",
        "line\nbreak",
        "line\r\nbreak",
        "tab\there",
        "nul\0here",
        "bell\u{7}",
        "del\u{7f}",
        "c1\u{85}next-line",
        "line\u{2028}separator",
        "para\u{2029}separator",
    ] {
        assert_eq!(
            validated(EditableField::Title, bad).unwrap_err(),
            "Title must be a single line of text without control characters.",
            "{bad:?}"
        );
    }
}

#[test]
fn error_messages_never_repeat_the_value() {
    let secret = "Private Name\nwith a line break";
    let err = validated(EditableField::Artist, secret).unwrap_err();
    assert!(!err.contains("Private"), "{err}");
    let err = validated(EditableField::Date, "Private 2026").unwrap_err();
    assert!(!err.contains("Private"), "{err}");
}

#[test]
fn remove_carries_no_value() {
    let valid = one_edit(remove(EditableField::Copyright)).unwrap();
    assert_eq!(valid.edits()[0].operation, EditOperation::Remove);
}

// ------------------------------------------------------------------- Date ---

#[test]
fn a_year_a_month_or_a_day_is_a_valid_date() {
    for date in [
        "2026",
        "2026-09",
        "2026-09-25",
        "2000-02-29",
        "2024-02-29",
        "0001",
        "9999-12-31",
    ] {
        assert_eq!(validated(EditableField::Date, date).unwrap(), date);
    }
}

#[test]
fn anything_else_is_not_a_date() {
    const SHAPE: &str = "Date must be a year (YYYY), a month (YYYY-MM) or a day (YYYY-MM-DD).";
    const CALENDAR: &str = "Date is not a real calendar date.";
    for (date, why) in [
        ("26", SHAPE),
        ("20", SHAPE),
        ("02026", SHAPE),
        ("2026-0", SHAPE),
        ("2026-9", SHAPE),
        ("2026-09-5", SHAPE),
        ("2026/09/25", SHAPE),
        ("2026.09.25", SHAPE),
        ("2026-09-25T10:00:00", SHAPE),
        ("2026-09-25 10:00", SHAPE),
        ("2026-09-25-01", SHAPE),
        ("-2026", SHAPE),
        ("+2026", SHAPE),
        ("２０２６", SHAPE),
        ("0000", CALENDAR),
        ("2026-00", CALENDAR),
        ("2026-13", CALENDAR),
        ("2026-09-00", CALENDAR),
        ("2026-02-29", CALENDAR),
        ("1900-02-29", CALENDAR),
        ("2026-02-30", CALENDAR),
        ("2026-04-31", CALENDAR),
        ("2026-09-31", CALENDAR),
        ("2026-12-32", CALENDAR),
    ] {
        assert_eq!(
            validated(EditableField::Date, date).unwrap_err(),
            why,
            "{date:?}"
        );
    }
    assert_eq!(
        validated(EditableField::Date, "").unwrap_err(),
        "Date cannot be empty. Use Remove to delete it."
    );
}

#[test]
fn date_rules_apply_to_fill_as_well_and_to_no_other_field() {
    assert!(one_edit(fill(EditableField::Date, "2026-02-30")).is_err());
    // "2026-02-30" is ordinary text anywhere else.
    assert!(validated(EditableField::Comment, "2026-02-30").is_ok());
}

// ------------------------------------------------------------- Resolution ---

#[test]
fn set_writes_whatever_was_there() {
    let op = EditOperation::Set("New".into());
    for current in [None, Some(""), Some("Old"), Some("New")] {
        assert_eq!(op.resolve(current), FieldChange::Write("New".into()));
        assert_eq!(op.expected_value(current).as_deref(), Some("New"));
    }
}

#[test]
fn fill_writes_only_a_missing_or_empty_field() {
    let op = EditOperation::FillIfMissing("2026".into());
    assert_eq!(op.resolve(None), FieldChange::Write("2026".into()));
    assert_eq!(op.resolve(Some("")), FieldChange::Write("2026".into()));
    assert_eq!(op.resolve(Some("   ")), FieldChange::Write("2026".into()));
    assert_eq!(op.expected_value(None).as_deref(), Some("2026"));
    assert_eq!(op.expected_value(Some("")).as_deref(), Some("2026"));

    assert_eq!(op.resolve(Some("1999 Owner")), FieldChange::Keep);
    assert_eq!(
        op.expected_value(Some("1999 Owner")).as_deref(),
        Some("1999 Owner")
    );
}

#[test]
fn remove_expects_the_field_absent() {
    let op = EditOperation::Remove;
    assert_eq!(op.resolve(Some("Old")), FieldChange::Delete);
    assert_eq!(op.resolve(Some("")), FieldChange::Delete);
    assert_eq!(op.resolve(None), FieldChange::Keep);
    for current in [None, Some(""), Some("Old")] {
        assert_eq!(op.expected_value(current), None);
    }
}

// ---------------------------------------------------------- Normalisation ---

#[test]
fn only_container_level_fields_are_normalised() {
    let globals =
        NormalizedGlobals::from_report(&report(&[("title", "Container")]), EditMuxer::Mp4);
    assert_eq!(globals.get("title"), Some("Container"));
    assert_eq!(
        globals.fields().len(),
        1,
        "the stream title must not appear"
    );
}

#[test]
fn keys_are_lowercased_and_values_trimmed() {
    let globals = NormalizedGlobals::from_report(
        &report(&[("ARTIST", "  Orig Artist  "), ("Comment", "\tNote\t")]),
        EditMuxer::Matroska,
    );
    assert_eq!(keys(&globals), ["artist", "comment"]);
    assert_eq!(globals.current(EditableField::Artist), Some("Orig Artist"));
    assert_eq!(globals.current(EditableField::Comment), Some("Note"));
    assert_eq!(globals.current(EditableField::Title), None);
}

#[test]
fn normalisation_is_deterministic_whatever_the_tag_order() {
    let a = report(&[("title", "T"), ("genre", "G"), ("artist", "A")]);
    let b = report(&[("artist", "A"), ("title", "T"), ("genre", "G")]);
    let (a, b) = (
        NormalizedGlobals::from_report(&a, EditMuxer::Mov),
        NormalizedGlobals::from_report(&b, EditMuxer::Mov),
    );
    assert_eq!(a, b);
    assert_eq!(keys(&a), ["artist", "genre", "title"]);
}

#[test]
fn muxer_owned_keys_are_exactly_these_and_are_left_out() {
    assert_eq!(
        EditMuxer::Mp4.muxer_owned_keys(),
        [
            "compatible_brands",
            "encoder",
            "major_brand",
            "minor_version"
        ]
    );
    assert_eq!(
        EditMuxer::Mov.muxer_owned_keys(),
        EditMuxer::Mp4.muxer_owned_keys()
    );
    assert_eq!(EditMuxer::Matroska.muxer_owned_keys(), ["encoder"]);
    assert_eq!(EditMuxer::WebM.muxer_owned_keys(), ["encoder"]);
    assert_eq!(EditMuxer::Avi.muxer_owned_keys(), ["software"]);

    let iso = report(&[
        ("major_brand", "M4V "),
        ("minor_version", "512"),
        ("compatible_brands", "M4V iso2mp41"),
        ("encoder", "Lavf62.12.102"),
        ("title", "T"),
    ]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&iso, EditMuxer::Mp4)),
        ["title"]
    );

    // Owned by the muxer that writes the file, not by every muxer: AVI's
    // muxer does not write `encoder`, so there it is an ordinary key.
    let avi = report(&[("software", "Lavf62.12.102"), ("encoder", "Other")]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&avi, EditMuxer::Avi)),
        ["encoder"]
    );
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&avi, EditMuxer::Matroska)),
        ["software"]
    );
}

#[test]
fn muxer_owned_keys_match_the_privacy_scan_semantics() {
    use crate::inspect::{MetadataField, StreamKind};
    use crate::privacy::{classify_field, PrivacyCategory};

    let category = |key: &str| {
        classify_field(&MetadataField {
            key: key.into(),
            display_key: key.into(),
            value: "x".into(),
            scope: MetadataScope::Format,
            stream_index: None,
            stream_kind: None::<StreamKind>,
        })
        .0
    };
    // The brand fields are already technical metadata in the scan.
    for key in ["major_brand", "minor_version", "compatible_brands"] {
        assert_eq!(category(key), PrivacyCategory::Structural, "{key}");
    }
    // `encoder` and `software` stay privacy findings; Edit only declines to
    // promise them, because the muxer rewrites them regardless.
    for key in ["encoder", "software"] {
        assert_eq!(category(key), PrivacyCategory::Software, "{key}");
    }
    // Structural keys the muxer does not regenerate at container level are
    // not muxer-owned: a track number is the user's.
    for key in ["track", "disc", "language"] {
        assert_eq!(category(key), PrivacyCategory::Structural);
        assert!(!EditMuxer::Mp4.is_muxer_owned(key), "{key}");
    }
}

#[test]
fn an_iso_location_alias_with_the_same_value_collapses() {
    let fix = "+45.4642+009.1900/";
    let input = report(&[("location", fix), ("location-eng", fix), ("title", "T")]);
    for muxer in [EditMuxer::Mp4, EditMuxer::Mov] {
        let globals = NormalizedGlobals::from_report(&input, muxer);
        assert_eq!(keys(&globals), ["location", "title"], "{muxer:?}");
        assert_eq!(globals.get("location"), Some(fix));
    }
    // What an Edit that kept the location produces normalises the same.
    let output = report(&[("location-eng", fix), ("location", fix), ("title", "New")]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&output, EditMuxer::Mp4)),
        ["location", "title"]
    );
}

#[test]
fn a_location_alias_that_disagrees_or_is_not_iso_is_kept() {
    let input = report(&[("location", "+1+2/"), ("location-eng", "+3+4/")]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&input, EditMuxer::Mp4)),
        ["location", "location-eng"]
    );
    // Matroska tags with a language suffix are separate tags there, and
    // collapsing them would hide the loss of one.
    let input = report(&[("location", "+1+2/"), ("location-eng", "+1+2/")]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&input, EditMuxer::Matroska)),
        ["location", "location-eng"]
    );
    // Only a three-letter language code makes an alias.
    let input = report(&[
        ("location", "+1+2/"),
        ("location-english", "+1+2/"),
        ("location-", "+1+2/"),
    ]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&input, EditMuxer::Mov)),
        ["location", "location-", "location-english"]
    );
    // Only `location` is aliased: nothing else was observed to be.
    let input = report(&[("title", "T"), ("title-eng", "T")]);
    assert_eq!(
        keys(&NormalizedGlobals::from_report(&input, EditMuxer::Mp4)),
        ["title", "title-eng"]
    );
}

#[test]
fn privacy_metadata_is_never_discarded_by_normalisation() {
    let input = report(&[
        ("location", "+45.4642+009.1900/"),
        (
            "com.apple.quicktime.location.ISO6709",
            "+45.4642+009.1900+120.000/",
        ),
        ("com.apple.quicktime.make", "Apple"),
        ("make", "TestCam"),
        ("model", "QA-1"),
        ("artist", "Someone"),
        ("creation_time", "2024-05-01T10:00:00.000000Z"),
        ("serial_number", "123"),
        ("x_custom", "custom"),
    ]);
    for muxer in [
        EditMuxer::Mp4,
        EditMuxer::Mov,
        EditMuxer::Matroska,
        EditMuxer::WebM,
        EditMuxer::Avi,
    ] {
        let globals = NormalizedGlobals::from_report(&input, muxer);
        assert_eq!(globals.fields().len(), 9, "{muxer:?}");
        assert_eq!(
            globals.get("com.apple.quicktime.location.iso6709"),
            Some("+45.4642+009.1900+120.000/")
        );
    }
}

#[test]
fn a_key_reported_twice_with_different_values_is_ambiguous() {
    // The inspector lowercases keys, so two spellings can land on one key.
    let input = report(&[
        ("TITLE", "One"),
        ("title", "Two"),
        ("genre", "G"),
        ("GENRE", "G"),
    ]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Matroska);
    assert_eq!(
        globals
            .ambiguous()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["title"]
    );
    // A repeat that agrees is simply the same field.
    assert_eq!(globals.get("genre"), Some("G"));
}

// ----------------------------------------------------------- Capabilities ---

#[test]
fn every_supported_format_has_a_known_edit_muxer() {
    let muxers: Vec<(&str, EditMuxer)> = FORMAT_PROFILES
        .iter()
        .map(|profile| (profile.extension, edit_muxer(*profile).unwrap()))
        .collect();
    assert_eq!(
        muxers,
        [
            ("mp4", EditMuxer::Mp4),
            ("mov", EditMuxer::Mov),
            ("m4v", EditMuxer::Mp4),
            ("mkv", EditMuxer::Matroska),
            ("webm", EditMuxer::WebM),
            ("avi", EditMuxer::Avi),
        ]
    );
}

#[test]
fn every_muxer_supports_every_editable_field() {
    for muxer in [
        EditMuxer::Mp4,
        EditMuxer::Mov,
        EditMuxer::Matroska,
        EditMuxer::WebM,
        EditMuxer::Avi,
    ] {
        for field in EditableField::ALL {
            assert!(muxer.supports_field(field), "{muxer:?} {field:?}");
        }
    }
}

/// The exact allowlists. A key added here without evidence from the pinned
/// FFmpeg would let an edit publish a file missing that key, so any change
/// to these lists has to be made on purpose, here.
#[test]
fn the_iso_and_avi_allowlists_are_pinned() {
    assert_eq!(
        MP4_PRESERVED,
        [
            "album",
            "album_artist",
            "artist",
            "comment",
            "composer",
            "copyright",
            "creation_time",
            "date",
            "description",
            "disc",
            "episode_id",
            "genre",
            "grouping",
            "keywords",
            "location",
            "lyrics",
            "network",
            "show",
            "synopsis",
            "title",
            "track",
        ]
    );
    assert_eq!(
        MOV_PRESERVED,
        [
            "album",
            "artist",
            "comment",
            "copyright",
            "creation_time",
            "date",
            "genre",
            "location",
            "make",
            "model",
            "title",
        ]
    );
    assert_eq!(
        AVI_PRESERVED,
        [
            "artist",
            "comment",
            "copyright",
            "date",
            "encoded_by",
            "genre",
            "language",
            "title",
            "track",
        ]
    );
    for list in [MP4_PRESERVED, MOV_PRESERVED, AVI_PRESERVED] {
        assert!(list.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
    }
}

#[test]
fn mp4_and_m4v_keep_itunes_fields_and_drop_mdta_device_and_custom_keys() {
    for ext in ["mp4", "m4v"] {
        let muxer = muxer_for(ext);
        for kept in [
            "title",
            "artist",
            "description",
            "location",
            "creation_time",
            "track",
        ] {
            assert!(muxer.preserves_global_key(kept), "{ext} {kept}");
        }
        for lost in [
            "make",
            "model",
            "com.apple.quicktime.make",
            "com.apple.quicktime.location.iso6709",
            "com.apple.quicktime.software",
            "x_custom",
            "performer",
            "author",
            "year",
            "location-eng",
        ] {
            assert!(!muxer.preserves_global_key(lost), "{ext} {lost}");
        }
    }
}

#[test]
fn mov_keeps_make_and_model_but_not_mdta_or_itunes_only_keys() {
    let mov = muxer_for("mov");
    for kept in ["make", "model", "location", "title", "creation_time"] {
        assert!(mov.preserves_global_key(kept), "{kept}");
    }
    for lost in [
        "com.apple.quicktime.make",
        "com.apple.quicktime.model",
        "com.apple.quicktime.location.iso6709",
        "x_custom",
        "description",
        "composer",
        "author",
        "album_artist",
    ] {
        assert!(!mov.preserves_global_key(lost), "{lost}");
    }
}

#[test]
fn matroska_and_webm_keep_any_key_made_of_safe_characters() {
    for ext in ["mkv", "webm"] {
        let muxer = muxer_for(ext);
        for kept in [
            "title",
            "x_custom",
            "metastrip_qa",
            "make",
            "creation_time",
            "date_released",
            "part2",
        ] {
            assert!(muxer.preserves_global_key(kept), "{ext} {kept}");
        }
        for unsure in [
            "",
            "my key",
            "com.apple.quicktime.make",
            "location-eng",
            "Title",
            "clé",
        ] {
            assert!(!muxer.preserves_global_key(unsure), "{ext} {unsure:?}");
        }
    }
}

#[test]
fn avi_keeps_only_its_info_fields() {
    let avi = muxer_for("avi");
    for kept in ["title", "artist", "comment", "copyright", "genre", "date"] {
        assert!(avi.preserves_global_key(kept), "{kept}");
    }
    for lost in [
        "album",
        "product",
        "creation_time",
        "location",
        "make",
        "x_custom",
        "description",
    ] {
        assert!(!avi.preserves_global_key(lost), "{lost}");
    }
}

// ------------------------------------------------------- Unpreserved keys ---

#[test]
fn an_iphone_style_mp4_would_lose_its_mdta_keys() {
    let input = report(&[
        ("major_brand", "isom"),
        ("title", "Orig"),
        ("location", "+45.4642+009.1900/"),
        ("location-eng", "+45.4642+009.1900/"),
        ("com.apple.quicktime.make", "Apple"),
        (
            "com.apple.quicktime.location.ISO6709",
            "+45.4642+009.1900+120.000/",
        ),
        ("encoder", "Lavf62.12.102"),
    ]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Mp4);
    let lost = unpreserved_keys(
        &globals,
        EditMuxer::Mp4,
        &[set(EditableField::Title, "New")],
    );
    assert_eq!(
        lost.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "com.apple.quicktime.location.iso6709",
            "com.apple.quicktime.make"
        ]
    );
}

#[test]
fn a_file_with_only_preserved_keys_loses_nothing() {
    let input = report(&[
        ("title", "Orig"),
        ("artist", "A"),
        ("comment", "C"),
        ("creation_time", "2024-05-01T10:00:00.000000Z"),
        ("location", "+1.0000+2.0000/"),
        ("location-eng", "+1.0000+2.0000/"),
        ("major_brand", "qt  "),
    ]);
    let edits = [remove(EditableField::Comment)];
    for muxer in [EditMuxer::Mp4, EditMuxer::Mov] {
        let globals = NormalizedGlobals::from_report(&input, muxer);
        assert!(
            unpreserved_keys(&globals, muxer, &edits).is_empty(),
            "{muxer:?}"
        );
    }
    let mkv = report(&[("title", "T"), ("x_custom", "v"), ("encoder", "Lavf62")]);
    let globals = NormalizedGlobals::from_report(&mkv, EditMuxer::Matroska);
    assert!(unpreserved_keys(&globals, EditMuxer::Matroska, &edits).is_empty());
}

#[test]
fn edited_fields_are_never_counted_as_lost() {
    // A muxer that could not keep an editable field would make this moot,
    // but the rule stands on its own: changing a field on purpose is not
    // losing it.
    let input = report(&[("title", "T"), ("album", "Kept?")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Avi);
    let lost = unpreserved_keys(&globals, EditMuxer::Avi, &[remove(EditableField::Title)]);
    assert_eq!(
        lost.iter().map(String::as_str).collect::<Vec<_>>(),
        ["album"]
    );
}

#[test]
fn an_ambiguous_key_is_never_claimed_to_survive() {
    let input = report(&[("TITLE", "One"), ("title", "Two"), ("genre", "G")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Matroska);
    let edit = [set(EditableField::Genre, "New")];
    assert_eq!(
        unpreserved_keys(&globals, EditMuxer::Matroska, &edit)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["title"]
    );
    // Editing the ambiguous field itself settles it.
    assert!(unpreserved_keys(
        &globals,
        EditMuxer::Matroska,
        &[set(EditableField::Title, "New")]
    )
    .is_empty());
}

// ------------------------------------- Only a real change exempts a field ---

fn lost(globals: &NormalizedGlobals, muxer: EditMuxer, edits: &[MetadataEdit]) -> Vec<String> {
    unpreserved_keys(globals, muxer, edits)
        .into_iter()
        .collect()
}

/// Title reported twice with two different values.
fn ambiguous_title() -> NormalizedGlobals {
    let input = report(&[("TITLE", "One"), ("title", "Two")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Matroska);
    assert!(globals.ambiguous().contains("title"));
    globals
}

#[test]
fn set_on_an_ambiguous_title_settles_it() {
    let globals = ambiguous_title();
    let edits = [set(EditableField::Title, "New")];
    assert_eq!(
        changed_fields(&globals, &edits),
        [EditableField::Title].into()
    );
    assert!(lost(&globals, EditMuxer::Matroska, &edits).is_empty());
}

#[test]
fn remove_on_an_ambiguous_title_settles_it() {
    let globals = ambiguous_title();
    let edits = [remove(EditableField::Title)];
    assert_eq!(
        changed_fields(&globals, &edits),
        [EditableField::Title].into()
    );
    assert!(lost(&globals, EditMuxer::Matroska, &edits).is_empty());
}

#[test]
fn fill_on_a_present_ambiguous_title_leaves_it_unpreserved() {
    // Present, so Fill keeps it: the field is untouched, and which of its two
    // values a remux would keep is still unknown.
    let globals = ambiguous_title();
    let edits = [fill(EditableField::Title, "New")];
    assert!(changed_fields(&globals, &edits).is_empty());
    assert_eq!(lost(&globals, EditMuxer::Matroska, &edits), ["title"]);
}

#[test]
fn fill_on_a_present_title_does_not_hide_another_lost_key() {
    let input = report(&[("title", "Orig"), ("make", "TestCam")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Mp4);
    let edits = [fill(EditableField::Title, "New")];
    assert!(changed_fields(&globals, &edits).is_empty());
    assert_eq!(lost(&globals, EditMuxer::Mp4, &edits), ["make"]);
}

#[test]
fn fill_on_a_missing_title_is_a_change() {
    let input = report(&[("make", "TestCam")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Mp4);
    let edits = [fill(EditableField::Title, "New")];
    assert_eq!(
        changed_fields(&globals, &edits),
        [EditableField::Title].into()
    );
    assert_eq!(lost(&globals, EditMuxer::Mp4, &edits), ["make"]);
}

#[test]
fn fill_on_an_empty_title_is_a_change() {
    let input = report(&[("title", "   "), ("comment", "Kept")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Avi);
    let edits = [fill(EditableField::Title, "New")];
    assert_eq!(
        changed_fields(&globals, &edits),
        [EditableField::Title].into()
    );
    assert!(lost(&globals, EditMuxer::Avi, &edits).is_empty());
}

#[test]
fn a_remove_of_an_absent_field_changes_nothing() {
    let input = report(&[("title", "T")]);
    let globals = NormalizedGlobals::from_report(&input, EditMuxer::Mov);
    assert!(changed_fields(&globals, &[remove(EditableField::Comment)]).is_empty());
}
