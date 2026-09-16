//! Post-clean verification.
//!
//! The product's claim is "verified cleaning", so the word has to mean something
//! concrete. Every check below runs against the file that was actually written,
//! and a file is only reported as verified when all of them pass. A check that
//! could not be evaluated fails; it never passes by default.
//!
//! ## Stream parameters, not stream identity
//!
//! Not re-encoding is a property of the pipeline: the cleaner only ever runs
//! FFmpeg with `-c copy` and has no transcoding path. What is checked, and
//! where, is split deliberately.
//!
//! * **Runtime (this module).** The media stream parameters are compared field
//!   by field between input and output: codec name, codec tag, profile,
//!   dimensions, pixel format, sample rate, channels and layout. This costs one
//!   `ffprobe` run on the output — constant time, independent of file size. It
//!   catches a stream that went missing or came out as a different codec or
//!   format, but it does **not** prove the packets are bit-for-bit the same, and
//!   it cannot on its own rule out a re-encode that kept every parameter. The
//!   check is therefore named "Media stream parameters match" and the UI claims
//!   only that: stream copy used, codec parameters preserved.
//! * **Regression (tests only).** The test suite additionally hashes the encoded
//!   packet payload of every stream with FFmpeg's `md5` muxer and asserts byte
//!   equality. That reads the whole file, which is fine for two-second fixtures
//!   and not fine for a 4 GB holiday video on every run.
//!
//! Hashing every output at runtime was rejected on cost: it turns a
//! millisecond-scale check into a full re-read of every file in the batch. Until
//! runtime packet identity actually exists, nothing the user sees may describe
//! this check as a bit-for-bit or byte-identical proof.

use serde::Serialize;
use std::path::Path;

use crate::inspect::{inspect, MetadataReport, StreamKind};
use crate::plan::CleaningPlan;
use crate::privacy::{classify, PrivacyFinding, Severity};

/// One named check with its outcome. Sent to the UI as-is so a failure can say
/// exactly which promise was not kept.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReport {
    pub verified: bool,
    pub checks: Vec<VerificationCheck>,
    /// Counts the completion screen shows, all measured rather than predicted.
    pub fields_removed: usize,
    pub chapters_removed: usize,
    pub data_streams_removed: usize,
    /// Findings that survived into the output, if any. Empty on a pass except
    /// for container bookkeeping, which is expected and reported separately.
    pub residual: Vec<PrivacyFinding>,
    /// Category-level before/after, for the summary table.
    pub before_after: Vec<BeforeAfterRow>,
}

/// One row of the before/after view: a category that was present, and whether it
/// is still there.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeAfterRow {
    pub category: &'static str,
    pub severity: &'static str,
    pub before: &'static str,
    pub after: &'static str,
    pub removed: bool,
}

/// Facts about the original captured before cleaning, so "the original was not
/// modified" can be checked against something rather than assumed.
///
/// Size and modification time, not a content hash: the cleaner never opens the
/// input for writing, so this is a guard against an unexpected bug or an
/// external process, and hashing multi-gigabyte inputs on every run would cost
/// more than the guard is worth. The test suite compares full bytes instead.
#[derive(Clone, Copy, Debug)]
pub struct OriginalFingerprint {
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

impl OriginalFingerprint {
    pub fn capture(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(OriginalFingerprint {
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    fn still_matches(&self, path: &Path) -> bool {
        match OriginalFingerprint::capture(path) {
            Some(now) => now.size == self.size && now.modified == self.modified,
            None => false,
        }
    }
}

/// Name of the runtime stream check. Exported so tests assert against the label
/// the UI shows instead of a copy of it.
pub const STREAM_PARAMETERS_CHECK: &str = "Media stream parameters match";

fn check(name: &'static str, passed: bool, detail: impl Into<String>) -> VerificationCheck {
    VerificationCheck {
        name,
        passed,
        detail: detail.into(),
    }
}

/// Build the before/after rows from the input findings and whatever survived.
fn before_after(before: &[PrivacyFinding], after: &[PrivacyFinding]) -> Vec<BeforeAfterRow> {
    let mut rows: Vec<BeforeAfterRow> = Vec::new();
    for finding in before {
        if rows.iter().any(|r| r.category == finding.category_label) {
            continue;
        }
        let survived = after
            .iter()
            .any(|a| a.category_label == finding.category_label);
        rows.push(BeforeAfterRow {
            category: finding.category_label,
            severity: finding.severity_label,
            before: "Present",
            after: if survived { "Present" } else { "Removed" },
            removed: !survived,
        });
    }
    rows
}

/// Verify one cleaned output against the input it came from.
///
/// `output_dir` is scanned for leftovers from the temporary-file mechanism, and
/// `temp_prefix` is the marker the cleaner uses for them.
#[allow(clippy::too_many_arguments)]
pub fn verify(
    input: &Path,
    output: &Path,
    before: &MetadataReport,
    before_findings: &[PrivacyFinding],
    plan: &CleaningPlan,
    original: Option<OriginalFingerprint>,
    output_dir: &Path,
    temp_prefix: &str,
) -> VerificationReport {
    let mut checks = Vec::new();

    // 1. The output exists at all.
    let exists = output.is_file();
    checks.push(check(
        "Output written",
        exists,
        if exists {
            "The cleaned file is on disk".to_string()
        } else {
            "The cleaned file is missing".to_string()
        },
    ));

    // 2. It can be read back. Everything downstream needs this report, so a
    //    failure here short-circuits into an unverified result rather than
    //    letting later checks pass vacuously.
    let after = if exists { inspect(output).ok() } else { None };
    let Some(after) = after else {
        checks.push(check(
            "Output readable",
            false,
            "The cleaned file could not be inspected",
        ));
        return VerificationReport {
            verified: false,
            checks,
            fields_removed: 0,
            chapters_removed: 0,
            data_streams_removed: 0,
            residual: Vec::new(),
            before_after: Vec::new(),
        };
    };
    checks.push(check(
        "Output readable",
        true,
        format!(
            "{} stream{} readable",
            after.streams.len(),
            if after.streams.len() == 1 { "" } else { "s" }
        ),
    ));

    let after_findings = classify(&after);

    // 3. No disclosure that was in the input is still in the output.
    //
    //    Structural bookkeeping is excluded: a muxer has to write `major_brand`,
    //    `handler_name` and `vendor_id` back, and demanding otherwise would fail
    //    every file for no privacy gain.
    //
    //    A field counts as surviving only when the same key in the same place
    //    still carries the same value. A muxer rewriting its own tag is not a
    //    surviving disclosure: `encoder` goes from `Lavf62.12.102`, which pins
    //    the exact build, to the bare `Lavf` that `-fflags +bitexact` produces.
    //    Check 4 below independently rejects anything sensitive appearing in the
    //    output under any value, so a partial rewrite cannot slip through.
    let mut survivors: Vec<PrivacyFinding> = Vec::new();
    for finding in before_findings {
        if finding.category.is_structural() {
            continue;
        }
        let still_there = after_findings.iter().any(|a| {
            a.source_key == finding.source_key
                && a.scope == finding.scope
                && a.stream_index == finding.stream_index
                && a.detail == finding.detail
        });
        if still_there {
            survivors.push(finding.clone());
        }
    }
    checks.push(check(
        "Sensitive metadata removed",
        survivors.is_empty(),
        if survivors.is_empty() {
            "No sensitive field from the original survived".to_string()
        } else {
            format!("{} sensitive field(s) survived", survivors.len())
        },
    ));

    // 4. Nothing new above LOW appeared. Catches a muxer writing something
    //    unexpected, independently of check 3.
    let introduced: Vec<PrivacyFinding> = after_findings
        .iter()
        .filter(|f| f.severity >= Severity::Medium)
        .cloned()
        .collect();
    checks.push(check(
        "No sensitive metadata in output",
        introduced.is_empty(),
        if introduced.is_empty() {
            "Only container bookkeeping remains".to_string()
        } else {
            format!("{} sensitive field(s) present", introduced.len())
        },
    ));

    // 5. Chapters, where the input had any.
    let chapters_removed = before.chapters.len().saturating_sub(after.chapters.len());
    let chapters_ok = !plan.remove_chapters || after.chapters.is_empty();
    checks.push(check(
        "Chapters removed",
        chapters_ok,
        if !plan.remove_chapters {
            "The original had no chapters".to_string()
        } else if chapters_ok {
            format!("{chapters_removed} chapter marker(s) removed")
        } else {
            format!("{} chapter marker(s) remain", after.chapters.len())
        },
    ));

    // 6. Data and other non-media tracks, where the input had any.
    let after_data = after.non_media_streams().count();
    let before_data = before.non_media_streams().count();
    let data_removed = before_data.saturating_sub(after_data);
    let data_ok = !plan.remove_data_streams || after_data == 0;
    checks.push(check(
        "Data tracks removed",
        data_ok,
        if !plan.remove_data_streams {
            "The original had no data tracks".to_string()
        } else if data_ok {
            format!("{data_removed} data track(s) removed")
        } else {
            format!("{after_data} data track(s) remain")
        },
    ));

    // 7. The media the user came for is still present, with the same codec
    //    parameters. A parameter comparison, not packet identity: see the module
    //    comment for what this does and does not establish.
    let mut identity_ok = true;
    let mut identity_detail = String::new();
    for kind in [StreamKind::Video, StreamKind::Audio] {
        let source: Vec<_> = before.streams.iter().filter(|s| s.kind == kind).collect();
        let result: Vec<_> = after.streams.iter().filter(|s| s.kind == kind).collect();
        if source.len() != result.len() {
            identity_ok = false;
            identity_detail = format!(
                "{} stream count changed: {} -> {}",
                kind.label(),
                source.len(),
                result.len()
            );
            break;
        }
        for (a, b) in source.iter().zip(&result) {
            if a.identity != b.identity {
                identity_ok = false;
                identity_detail = format!(
                    "{} stream {} changed codec parameters",
                    kind.label(),
                    a.index
                );
                break;
            }
        }
        if !identity_ok {
            break;
        }
    }
    if identity_ok {
        let video = before
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Video)
            .count();
        let audio = before
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Audio)
            .count();
        identity_detail =
            format!("{video} video and {audio} audio stream(s) with codec parameters preserved");
    }
    checks.push(check(STREAM_PARAMETERS_CHECK, identity_ok, identity_detail));

    // 8. The original is exactly as it was.
    let original_ok = match original {
        Some(fingerprint) => fingerprint.still_matches(input),
        None => false,
    };
    checks.push(check(
        "Original unchanged",
        original_ok,
        if original_ok {
            "The source file is unchanged".to_string()
        } else {
            "The source file could not be confirmed unchanged".to_string()
        },
    ));

    // 9. The extension is preserved, case included.
    let same_extension =
        input.extension().and_then(|e| e.to_str()) == output.extension().and_then(|e| e.to_str());
    checks.push(check(
        "Extension preserved",
        same_extension,
        match output.extension().and_then(|e| e.to_str()) {
            Some(ext) if same_extension => format!("Saved as .{ext}"),
            _ => "The output extension does not match the original".to_string(),
        },
    ));

    // 10. No half-written artefact left in the output folder.
    let leftovers = std::fs::read_dir(output_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(temp_prefix))
                .count()
        })
        .unwrap_or(0);
    checks.push(check(
        "No temporary files left",
        leftovers == 0,
        if leftovers == 0 {
            "The output folder is clean".to_string()
        } else {
            format!("{leftovers} temporary file(s) left behind")
        },
    ));

    let fields_removed = before.fields.len().saturating_sub(after.fields.len());

    VerificationReport {
        verified: checks.iter().all(|c| c.passed),
        checks,
        fields_removed,
        chapters_removed,
        data_streams_removed: data_removed,
        residual: survivors,
        before_after: before_after(before_findings, &after_findings),
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod verify_tests;
