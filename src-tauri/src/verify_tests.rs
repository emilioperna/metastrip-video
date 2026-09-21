//! Verification tests.
//!
//! The point of these is adversarial: it is easy to write a verifier that always
//! says yes. Each test below either proves a real clean passes every check, or
//! forces one check to fail and asserts that `verified` goes false.

use super::*;
use crate::testkit::{
    assert_ffmpeg_can_read, expected_categories, sample_for_format, scratch, stream_payload_hash,
    ATTACHMENT_CANARY_ALPHA, ATTACHMENT_CANARY_BETA, DATA_CANARY,
};
use crate::{clean_and_verify, validate_input, IdRegistry, TEMP_PREFIX};

fn clean(dir: &std::path::Path, input: &std::path::Path) -> (String, VerificationReport) {
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
    let (outcome, report) = clean_and_verify(input, &out, "CLIP", &mut registry);
    (outcome.unwrap(), report.expect("no verification report"))
}

fn failed_names(report: &VerificationReport) -> Vec<&str> {
    report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
        .collect()
}

/// Every container, cleaned and verified for real.
fn assert_verified_clean(extension: &str) {
    let dir = scratch(&format!("verify-{extension}"));
    let input = sample_for_format(&dir, extension);
    let (name, report) = clean(&dir, &input);
    let output = dir.join("out").join(&name);

    assert!(
        report.verified,
        "{extension} did not verify: {:?}",
        failed_names(&report)
    );
    assert!(
        report.checks.len() >= 9,
        "{extension} ran only {} checks",
        report.checks.len()
    );
    assert!(report.checks.iter().all(|c| c.passed));
    assert!(report.residual.is_empty(), "{extension} left residue");

    // The counts shown to the user are real measurements.
    assert!(
        report.fields_removed > 0,
        "{extension} reported no fields removed"
    );
    // Privacy and technical removals are measured apart, and structural
    // bookkeeping is never reported as privacy metadata removed.
    assert!(
        report.privacy_fields_removed > 0,
        "{extension} reported no privacy fields removed"
    );
    for row in &report.before_after {
        assert_eq!(
            row.technical,
            row.category == "Structural",
            "{extension}: {} has the wrong technical flag",
            row.category
        );
    }

    // The before/after table describes what was actually there.
    assert!(
        !report.before_after.is_empty(),
        "{extension} produced no before/after rows"
    );
    // Every privacy category that was present must read as removed. Container
    // bookkeeping is the one exception, and it is shown apart as technical.
    //
    // `Software` used to be a second exception here, for Matroska and WebM:
    // those muxers regenerate an `encoder` tag, and the table compared category
    // labels, so it called the category present again on a file every check had
    // just passed. The table now answers with the same rule check 3 uses, so
    // the exception is gone.
    //
    // What this is and is not: since the table reads from check 3's survivors,
    // `report.verified` above already implies these rows. It is a regression
    // guard -- revert the table to comparing category labels and it fires --
    // not an independent measurement. The independent one is
    // `the_planted_canaries_are_found_before_and_gone_after`, which re-inspects
    // and re-classifies the output rather than trusting the report.
    for row in &report.before_after {
        assert_eq!(row.before, "Present");
        if row.technical {
            continue;
        }
        assert_eq!(row.after, "Removed", "{extension} kept {}", row.category);
        assert!(row.removed);
    }

    assert_ffmpeg_can_read(&output);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mp4_verifies() {
    assert_verified_clean("mp4");
}
#[test]
fn mov_verifies() {
    assert_verified_clean("mov");
}
#[test]
fn m4v_verifies() {
    assert_verified_clean("m4v");
}
#[test]
fn mkv_verifies() {
    assert_verified_clean("mkv");
}
#[test]
fn webm_verifies() {
    assert_verified_clean("webm");
}
#[test]
fn avi_verifies() {
    assert_verified_clean("avi");
}

/// Every container: the canaries that container can actually carry are found
/// before cleaning, and none of them is there afterwards.
#[test]
fn the_planted_canaries_are_found_before_and_gone_after() {
    for extension in ["mp4", "mov", "m4v", "mkv", "webm", "avi"] {
        let dir = scratch(&format!("canaries-{extension}"));
        let input = sample_for_format(&dir, extension);

        let before = inspect(&input).unwrap();
        let findings = classify(&before);

        // The fixture is worth nothing unless it really carries what we claim.
        let expected = expected_categories(extension);
        for category in &expected {
            assert!(
                findings.iter().any(|f| &f.category_label == category),
                "{extension} fixture lost its {category} canary"
            );
        }
        if expected.contains(&"Location") {
            assert!(
                findings.iter().any(|f| f.severity == Severity::High),
                "{extension} fixture has no high-severity finding"
            );
        }

        let (name, report) = clean(&dir, &input);
        assert!(report.verified, "{extension}: {:?}", failed_names(&report));

        // Nothing above LOW survives, and specifically none of the canaries.
        let after = classify(&inspect(&dir.join("out").join(&name)).unwrap());
        assert!(
            after.iter().all(|f| f.severity == Severity::Low),
            "{extension} kept sensitive findings: {:?}",
            after
                .iter()
                .filter(|f| f.severity != Severity::Low)
                .map(|f| &f.detail)
                .collect::<Vec<_>>()
        );
        for category in &expected {
            assert!(
                !after.iter().any(|f| &f.category_label == category),
                "{extension}: {category} survived cleaning"
            );
        }
        // Whatever is left is container bookkeeping and nothing else.
        assert!(
            after
                .iter()
                .all(|f| f.category.is_structural() || f.category_label == "Software"),
            "{extension} kept something unexpected: {:?}",
            after.iter().map(|f| &f.detail).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Packet identity, run only in tests: the encoded packet payloads are
/// byte-identical, not merely same-codec. The runtime verifier compares stream
/// parameters only; see the module comment in `verify.rs` for why.
#[test]
fn deep_regression_encoded_payloads_are_byte_identical() {
    for extension in ["mp4", "mov", "m4v", "mkv", "webm", "avi"] {
        let dir = scratch(&format!("deep-{extension}"));
        let input = sample_for_format(&dir, extension);
        let source_video = stream_payload_hash(&input, "0:v:0");
        let source_audio = stream_payload_hash(&input, "0:a:0");

        let (name, report) = clean(&dir, &input);
        let output = dir.join("out").join(&name);

        assert!(report.verified, "{extension}: {:?}", failed_names(&report));
        assert_eq!(
            source_video,
            stream_payload_hash(&output, "0:v:0"),
            "{extension} video payload changed"
        );
        assert_eq!(
            source_audio,
            stream_payload_hash(&output, "0:a:0"),
            "{extension} audio payload changed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_data_track_payload_is_gone_and_verification_says_so() {
    let dir = scratch("verify-data");
    let input = sample_for_format(&dir, "avi");
    assert!(
        crate::testkit::contains(&std::fs::read(&input).unwrap(), DATA_CANARY),
        "the AVI fixture lost its payload canary; the test would prove nothing"
    );

    let (name, report) = clean(&dir, &input);
    let output = dir.join("out").join(&name);

    assert!(report.verified, "{:?}", failed_names(&report));
    assert!(report.data_streams_removed > 0);
    assert!(
        !crate::testkit::contains(&std::fs::read(&output).unwrap(), DATA_CANARY),
        "the data payload is still recoverable"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn chapters_are_counted_as_removed_only_where_the_container_had_them() {
    // MP4 carries chapters; AVI does not, and must not claim a removal.
    let dir = scratch("verify-chapters");
    let mp4 = sample_for_format(&dir, "mp4");
    let (_, report) = clean(&dir, &mp4);
    assert!(report.chapters_removed > 0, "MP4 chapters were not counted");
    let chapter_check = report
        .checks
        .iter()
        .find(|c| c.name == "Chapters removed")
        .unwrap();
    assert!(chapter_check.passed);
    assert!(chapter_check.detail.contains("removed"));
    std::fs::remove_dir_all(&dir).unwrap();

    let dir = scratch("verify-chapters-avi");
    let avi = sample_for_format(&dir, "avi");
    let (_, report) = clean(&dir, &avi);
    let chapter_check = report
        .checks
        .iter()
        .find(|c| c.name == "Chapters removed")
        .unwrap();
    assert!(chapter_check.passed);
    assert_eq!(chapter_check.detail, "The original had no chapters");
    assert_eq!(report.chapters_removed, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_output_fails_verification_instead_of_passing_vacuously() {
    let dir = scratch("verify-missing");
    let input = sample_for_format(&dir, "mp4");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let before = inspect(&input).unwrap();
    let findings = classify(&before);
    let plan = crate::plan::plan_for(&before, &findings, validate_input(&input).unwrap());

    let report = verify(
        &input,
        &out.join("never-written.mp4"),
        &before,
        &findings,
        &plan,
        OriginalFingerprint::capture(&input),
        &out,
        TEMP_PREFIX,
    );

    assert!(
        !report.verified,
        "a missing output was reported as verified"
    );
    assert!(failed_names(&report).contains(&"Output written"));
    // It must not go on to claim the later checks passed.
    assert!(report.checks.iter().all(|c| c.name != "Original unchanged"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_output_fails_verification() {
    let dir = scratch("verify-unreadable");
    let input = sample_for_format(&dir, "mp4");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // A file that exists but is not media.
    let broken = out.join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();

    let before = inspect(&input).unwrap();
    let findings = classify(&before);
    let plan = crate::plan::plan_for(&before, &findings, validate_input(&input).unwrap());

    let report = verify(
        &input,
        &broken,
        &before,
        &findings,
        &plan,
        OriginalFingerprint::capture(&input),
        &out,
        TEMP_PREFIX,
    );

    assert!(!report.verified);
    assert!(failed_names(&report).contains(&"Output readable"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_changed_original_fails_verification() {
    let dir = scratch("verify-original");
    let input = sample_for_format(&dir, "mp4");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let before = inspect(&input).unwrap();
    let findings = classify(&before);
    let plan = crate::plan::plan_for(&before, &findings, validate_input(&input).unwrap());
    let fingerprint = OriginalFingerprint::capture(&input);

    // Clean normally, then tamper with the source behind the verifier's back.
    let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
    let name = crate::clean_one(&input, &out, "CLIP", &mut registry).unwrap();
    std::fs::write(&input, b"the original was replaced").unwrap();

    let report = verify(
        &input,
        &out.join(&name),
        &before,
        &findings,
        &plan,
        fingerprint,
        &out,
        TEMP_PREFIX,
    );

    assert!(
        !report.verified,
        "a modified original was reported as verified"
    );
    assert!(failed_names(&report).contains(&"Original unchanged"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_leftover_temp_file_fails_verification() {
    let dir = scratch("verify-temp");
    let input = sample_for_format(&dir, "mp4");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    let before = inspect(&input).unwrap();
    let findings = classify(&before);
    let plan = crate::plan::plan_for(&before, &findings, validate_input(&input).unwrap());
    let fingerprint = OriginalFingerprint::capture(&input);

    let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
    let name = crate::clean_one(&input, &out, "CLIP", &mut registry).unwrap();
    std::fs::write(out.join(format!("{TEMP_PREFIX}0000000001.mp4")), b"partial").unwrap();

    let report = verify(
        &input,
        &out.join(&name),
        &before,
        &findings,
        &plan,
        fingerprint,
        &out,
        TEMP_PREFIX,
    );

    assert!(!report.verified);
    assert!(failed_names(&report).contains(&"No temporary files left"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A re-encoded output is exactly what the pipeline is built never to produce.
/// Feeding one that changes codec to the verifier proves the parameter
/// comparison actually bites. It is not evidence that the comparison would catch
/// a re-encode that kept every parameter; the packet-identity test above is.
#[test]
fn a_transcoded_output_fails_the_stream_parameters_check() {
    let dir = scratch("verify-reencode");
    let input = sample_for_format(&dir, "mp4");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // Deliberately transcode to a different codec. Production never does this.
    let transcoded = out.join("transcoded.mp4");
    let built = crate::sidecar::ffmpeg()
        .args(["-y", "-i"])
        .arg(&input)
        // The subtitle is carried through so the video codec is the ONLY
        // difference from the input. Dropping it here would make check 7 fail
        // on a stream count as well, and the codec comparison this test exists
        // to pin would no longer be the reason it fires.
        .args([
            "-map", "0:v:0", "-map", "0:a:0", "-map", "0:s:0", "-c:v", "mjpeg", "-c:a", "aac",
            "-c:s", "copy", "-t", "2",
        ])
        .arg(&transcoded)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "could not build the transcoded control: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let before = inspect(&input).unwrap();
    let findings = classify(&before);
    let plan = crate::plan::plan_for(&before, &findings, validate_input(&input).unwrap());

    let report = verify(
        &input,
        &transcoded,
        &before,
        &findings,
        &plan,
        OriginalFingerprint::capture(&input),
        &out,
        TEMP_PREFIX,
    );

    assert!(
        !report.verified,
        "a re-encoded output was reported as verified"
    );
    assert!(
        failed_names(&report).contains(&STREAM_PARAMETERS_CHECK),
        "the stream parameters check did not fire: {:?}",
        failed_names(&report)
    );
    // And it fired for the right reason. Without this the test would still pass
    // if the codec comparison were deleted and the check failed on a count.
    let stream_check = report
        .checks
        .iter()
        .find(|c| c.name == STREAM_PARAMETERS_CHECK)
        .unwrap();
    assert!(
        stream_check.detail.contains("changed codec parameters"),
        "the failure was not attributed to the codec comparison: {}",
        stream_check.detail
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_attachment_payload_is_gone_and_verification_says_so() {
    // Matroska is the one supported container that can carry an attachment, so
    // this is the only fixture that can prove the payload really leaves.
    let dir = scratch("verify-attachment");
    let input = sample_for_format(&dir, "mkv");
    let raw = std::fs::read(&input).unwrap();
    for canary in [ATTACHMENT_CANARY_ALPHA, ATTACHMENT_CANARY_BETA] {
        assert!(
            crate::testkit::contains(&raw, canary),
            "the MKV fixture lost an attachment canary; the test would prove nothing"
        );
    }

    let (name, report) = clean(&dir, &input);
    let output = dir.join("out").join(&name);

    assert!(report.verified, "{:?}", failed_names(&report));
    assert!(
        report.data_streams_removed >= 2,
        "both attachments should be counted as removed, got {}",
        report.data_streams_removed
    );
    let cleaned = std::fs::read(&output).unwrap();
    for canary in [ATTACHMENT_CANARY_ALPHA, ATTACHMENT_CANARY_BETA] {
        assert!(
            !crate::testkit::contains(&cleaned, canary),
            "an attachment payload is still recoverable"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cleaning_an_already_cleaned_file_still_verifies() {
    // Matroska and WebM write `encoder=Lavf` back on every remux, so a second
    // pass meets the same key, scope and value the first pass wrote. Counting
    // this app's own bitexact stamp as a surviving disclosure would report a
    // failure for a file that is in fact clean.
    for extension in ["mkv", "webm"] {
        let dir = scratch(&format!("verify-reclean-{extension}"));
        let input = sample_for_format(&dir, extension);

        let (first, first_report) = clean(&dir, &input);
        assert!(
            first_report.verified,
            "{extension} first pass: {:?}",
            failed_names(&first_report)
        );

        let once_cleaned = dir.join("out").join(&first);
        let (_, second_report) = clean(&dir, &once_cleaned);
        assert!(
            second_report.verified,
            "{extension} second pass: {:?}",
            failed_names(&second_report)
        );
        assert!(
            second_report.residual.is_empty(),
            "{extension} reported its own stamp as a survivor: {:?}",
            second_report.residual
        );
        // And the table does not claim a removal the output disproves. On this
        // pass the only Software finding IS the stamp, and it is still there
        // byte for byte, so it earns no row either way -- neither "Present",
        // which would contradict the badge, nor "Removed", which would be false.
        assert!(
            !second_report
                .before_after
                .iter()
                .any(|row| row.category == "Software"),
            "{extension} claims it removed its own bitexact stamp: {:?}",
            second_report
                .before_after
                .iter()
                .map(|row| (row.category, row.after))
                .collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_verified_file_shows_no_privacy_category_as_still_present() {
    // The before/after table and the "Verified" badge share a screen, so they
    // must not disagree. Nothing may read "Present -> Present" in the privacy
    // section of a file every check just passed; technical rows are exempt,
    // because container bookkeeping really does come back.
    for extension in ["mp4", "mkv", "webm"] {
        let dir = scratch(&format!("verify-beforeafter-{extension}"));
        let input = sample_for_format(&dir, extension);
        let (_, report) = clean(&dir, &input);

        assert!(report.verified, "{extension}: {:?}", failed_names(&report));
        let contradictions: Vec<&str> = report
            .before_after
            .iter()
            .filter(|row| !row.technical && !row.removed)
            .map(|row| row.category)
            .collect();
        assert!(
            contradictions.is_empty(),
            "{extension} calls these privacy categories present on a verified file: {contradictions:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_dropped_subtitle_fails_the_stream_parameters_check() {
    // Subtitles are media: the cleaner keeps them, and nothing else in the
    // report would notice one going missing. `non_media_streams` excludes them,
    // so check 6 never counts them either.
    let dir = scratch("verify-subtitle-lost");
    let input = sample_for_format(&dir, "mkv");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // Everything the real cleaner produces, minus the subtitle track.
    let stripped = out.join("stripped.mkv");
    let built = crate::sidecar::ffmpeg()
        .args(["-y", "-i"])
        .arg(&input)
        .args([
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-map_metadata:s",
            "-1",
            "-map_chapters",
            "-1",
            "-dn",
            "-fflags",
            "+bitexact",
            "-f",
            "matroska",
        ])
        .arg(&stripped)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the subtitle-less output: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let before = crate::inspect::inspect(&input).unwrap();
    let before_findings = crate::privacy::classify(&before);
    let format = validate_input(&input).unwrap();
    let plan = crate::plan::plan_for(&before, &before_findings, format);
    let report = verify(
        &input,
        &stripped,
        &before,
        &before_findings,
        &plan,
        OriginalFingerprint::capture(&input),
        &out,
        TEMP_PREFIX,
    );

    assert!(!report.verified, "a lost subtitle track went unnoticed");
    assert!(
        failed_names(&report).contains(&STREAM_PARAMETERS_CHECK),
        "the loss was not attributed to the stream check: {:?}",
        failed_names(&report)
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_this_apps_own_bitexact_stamp_is_exempt_from_the_survivor_check() {
    // The exemption is a hole in check 3, the central privacy check, so its
    // edge is what matters: widen it and a real `encoder` disclosure surviving
    // unchanged into the output would be reported as verified. Check 4 cannot
    // catch that -- it only looks at MEDIUM and above, and `encoder` is LOW.
    let finding_for = |key: &str, value: &str| {
        let json = format!(r#"{{"format":{{"tags":{{"{key}":"{value}"}}}},"streams":[]}}"#);
        let report = crate::inspect::parse_ffprobe_json("x.mkv", &json).unwrap();
        let findings = classify(&report);
        assert_eq!(findings.len(), 1, "expected one finding for {key}={value}");
        findings.into_iter().next().unwrap()
    };

    assert!(
        is_bitexact_encoder_stamp(&finding_for("encoder", "Lavf")),
        "the bare stamp must be exempt, or re-cleaning an output reports a false failure"
    );

    for (key, value) in [
        // A version pins the exact build: a real disclosure.
        ("encoder", "Lavf62.12.102"),
        ("encoder", "HandBrake 1.7.3"),
        ("encoder", "Lavf-modified"),
        // Same value, different field: the exemption is about one key only.
        ("writing_application", "Lavf"),
        ("software", "Lavf"),
    ] {
        assert!(
            !is_bitexact_encoder_stamp(&finding_for(key, value)),
            "{key}={value} must still be checked as a survivor"
        );
    }
}
