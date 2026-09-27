//! Post-edit verification: what "Verified edit" means.
//!
//! Edit is not Clean, and this is not Clean's verifier. Clean promises that
//! metadata is gone; Edit promises that the fields the user named came out as
//! planned and that nothing else meaningful changed. Every check below reads
//! the file that was actually published, through one ffprobe run, and compares
//! it with the [`EditPlan`] that produced it -- the same plan, built from the
//! same fresh inspection, that gave FFmpeg its arguments. Nothing is inspected
//! or planned again from the input.
//!
//! A file is verified only when every check passes. A fact that could not be
//! established -- an output that cannot be read, an original whose size and
//! time were never captured -- fails its check; nothing passes by default.
//!
//! ## What this proves, and what it does not
//!
//! It proves that the output exists and reads back; that each requested field
//! resolved exactly as planned; that the container-level metadata is exactly
//! the plan's, no more and no less; that the planned streams are there, in
//! order, with the same codec parameters, cover art flag and tags; that the
//! chapters are the same; that the only data tracks are the ISO-BMFF tracks the
//! muxer rebuilds; that the original is as it was, the extension is kept and
//! this output's temporary file is gone.
//!
//! It does not prove packet identity -- the arguments are always a stream copy,
//! and the test suite hashes packets; hashing every output here would re-read
//! whole files -- nor byte identity, nor that any other program shows every tag
//! the same way. And it proves nothing about privacy: Edit keeps the file's
//! metadata on purpose. What privacy metadata remains is reported beside the
//! checks as information, and never decides `verified`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::edit::{effective, EditableField, FieldChange, NormalizedGlobals};
use crate::edit_plan::{
    is_chapter_track, is_timecode_track, stream_tag, stream_tags, EditPlan, RebuiltTrack,
};
use crate::inspect::{self, MetadataReport, MetadataScope, StreamKind, StreamSummary};
use crate::privacy::{classify, Severity};
use crate::verify::{OriginalFingerprint, VerificationCheck};

/// The checks, by the names the page shows, in the order they run. Exported
/// so the tests assert against these rather than copies of them.
pub const OUTPUT_WRITTEN: &str = "Output written";
pub const OUTPUT_READABLE: &str = "Output readable";
pub const REQUESTED_CHANGES_APPLIED: &str = "Requested metadata changes applied";
pub const OTHER_METADATA_PRESERVED: &str = "Other metadata preserved";
pub const STREAM_STRUCTURE_PRESERVED: &str = "Stream structure preserved";
pub const STREAM_METADATA_PRESERVED: &str = "Stream metadata preserved";
pub const CHAPTERS_PRESERVED: &str = "Chapters preserved";
pub const STRUCTURAL_TRACKS_PRESERVED: &str = "Structural tracks preserved";
pub const ORIGINAL_UNCHANGED: &str = "Original unchanged";
pub const EXTENSION_PRESERVED: &str = "Extension preserved";
pub const NO_TEMPORARY_OUTPUT: &str = "No temporary output left";

/// Every report carries all of these, in this order, whether or not the
/// output could be read.
pub const EDIT_CHECKS: [&str; 11] = [
    OUTPUT_WRITTEN,
    OUTPUT_READABLE,
    REQUESTED_CHANGES_APPLIED,
    OTHER_METADATA_PRESERVED,
    STREAM_STRUCTURE_PRESERVED,
    STREAM_METADATA_PRESERVED,
    CHAPTERS_PRESERVED,
    STRUCTURAL_TRACKS_PRESERVED,
    ORIGINAL_UNCHANGED,
    EXTENSION_PRESERVED,
    NO_TEMPORARY_OUTPUT,
];

/// The detail of a check that needs the output's inspection when there is
/// none. It fails: not having looked is not a pass.
const NOT_CHECKED: &str = "Not checked: the edited file could not be read";

/// One requested field, before and after, for the page's table. Values are the
/// normalised ones the checks compare -- trimmed, as the inspector reads them;
/// `None` is a field the file does not carry, never a placeholder.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChangeRow {
    pub field: EditableField,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Whether the field's effective value -- missing and empty being the same
    /// -- differs between the two files.
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditVerificationReport {
    /// Every check passed. Nothing else feeds this.
    pub verified: bool,
    pub checks: Vec<VerificationCheck>,
    /// One row per requested field, in field order. Empty when the output
    /// could not be read: there is no "after" to show.
    pub changes: Vec<FieldChangeRow>,
    /// Rows whose `changed` is true.
    pub fields_changed: usize,
    /// HIGH and MEDIUM privacy findings still in the output, other than those
    /// this edit wrote on purpose. Information only; see [`remaining_privacy`].
    pub remaining_privacy_count: usize,
    /// Their category labels, once each, most severe first.
    pub remaining_privacy_categories: Vec<&'static str>,
}

impl EditVerificationReport {
    /// A short, safe line for a file whose edit did not verify: the details of
    /// the failed checks, each already written in the failing direction and
    /// generated here from field labels, stream kinds and counts -- never a
    /// value out of the file or the request. `None` for a verified file.
    ///
    /// Checks that were not run for want of a readable output are left out:
    /// the readability failure already says why.
    pub fn failure_message(&self) -> Option<String> {
        if self.verified {
            return None;
        }
        let mut details: Vec<&str> = Vec::new();
        for check in self
            .checks
            .iter()
            .filter(|c| !c.passed && c.detail != NOT_CHECKED)
        {
            if !details.contains(&check.detail.as_str()) {
                details.push(&check.detail);
            }
        }
        Some(if details.is_empty() {
            "Verification did not pass.".to_string()
        } else {
            format!("Verification failed: {}.", details.join(", "))
        })
    }
}

/// Everything the verifier is given, all of it from the one execution that
/// wrote the output: the fresh inspection the plan was built from, that plan,
/// the original's size and time taken before FFmpeg ran, and the temporary
/// path this output was written under.
pub struct EditVerification<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub temp: &'a Path,
    pub before: &'a MetadataReport,
    pub plan: &'a EditPlan,
    pub original: Option<OriginalFingerprint>,
}

fn check(name: &'static str, passed: bool, detail: impl Into<String>) -> VerificationCheck {
    VerificationCheck {
        name,
        passed,
        detail: detail.into(),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl EditVerification<'_> {
    /// Inspect the output once and check it.
    pub fn run(&self) -> EditVerificationReport {
        let written = std::fs::symlink_metadata(self.output).is_ok_and(|m| m.file_type().is_file());
        let after = if written {
            inspect::inspect(self.output).ok()
        } else {
            None
        };
        self.evaluate(written, after.as_ref())
    }

    /// The checks against an output that was, or was not, written and read
    /// back as `after`. Split from [`Self::run`] so the tests can hand in an
    /// inspection a real container cannot be made to produce.
    pub(crate) fn evaluate(
        &self,
        written: bool,
        after: Option<&MetadataReport>,
    ) -> EditVerificationReport {
        let mut checks = vec![check(
            OUTPUT_WRITTEN,
            written,
            if written {
                "The edited file is on disk"
            } else {
                "The edited file is missing"
            },
        )];

        // Nothing is read from a file that is not there.
        let Some(after) = after.filter(|_| written) else {
            checks.push(check(
                OUTPUT_READABLE,
                false,
                if written {
                    "The edited file could not be inspected"
                } else {
                    "There is no edited file to inspect"
                },
            ));
            for name in &EDIT_CHECKS[2..8] {
                checks.push(check(name, false, NOT_CHECKED));
            }
            // These three do not need the output's inspection, so they are
            // still real answers.
            checks.push(self.original_unchanged());
            checks.push(self.extension_preserved());
            checks.push(self.no_temporary_output());
            return report(checks, Vec::new(), (0, Vec::new()));
        };
        checks.push(check(
            OUTPUT_READABLE,
            true,
            format!(
                "{} readable",
                plural(after.streams.len(), "stream", "streams")
            ),
        ));

        // The planner's own normalisation, for the muxer the plan wrote with,
        // on both sides.
        let muxer = self.plan.muxer();
        let before_globals = NormalizedGlobals::from_report(self.before, muxer);
        let after_globals = NormalizedGlobals::from_report(after, muxer);

        checks.push(requested_changes_applied(
            self.plan,
            &before_globals,
            &after_globals,
        ));
        checks.push(other_metadata_preserved(self.plan, &after_globals));
        let kept = ordinary_streams(after);
        checks.push(stream_structure_preserved(self.plan, &kept));
        checks.push(stream_metadata_preserved(self.plan, after, &kept));
        checks.push(chapters_preserved(self.plan, after));
        checks.push(structural_tracks_preserved(self.plan, after));
        checks.push(self.original_unchanged());
        checks.push(self.extension_preserved());
        checks.push(self.no_temporary_output());

        report(
            checks,
            change_rows(self.plan, &before_globals, &after_globals),
            remaining_privacy(self.plan, after),
        )
    }

    /// The original has the size and modification time it had before FFmpeg
    /// ran. Size and time, not a hash, for the reason Clean gives; without a
    /// fingerprint there is nothing to compare, and the check fails.
    fn original_unchanged(&self) -> VerificationCheck {
        let passed = self
            .original
            .is_some_and(|fingerprint| fingerprint.still_matches(self.input));
        check(
            ORIGINAL_UNCHANGED,
            passed,
            if passed {
                "The source file is unchanged"
            } else {
                "The source file could not be confirmed unchanged"
            },
        )
    }

    /// The same extension, spelled the same way: `.MOV` stays `.MOV`.
    fn extension_preserved(&self) -> VerificationCheck {
        let output = self.output.extension();
        let passed = output.is_some() && output == self.input.extension();
        check(
            EXTENSION_PRESERVED,
            passed,
            match output.and_then(|e| e.to_str()) {
                Some(extension) if passed => format!("Saved as .{extension}"),
                _ => "The output extension does not match the original".to_string(),
            },
        )
    }

    /// This output's own temporary file is gone. Only that path: a leftover
    /// from another file of the batch is that file's business, and the next
    /// batch's sweep clears it.
    fn no_temporary_output(&self) -> VerificationCheck {
        let (passed, detail) = match self.temp.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (true, "No temporary file was left for this output")
            }
            Ok(_) => (false, "A temporary file was left for this output"),
            Err(_) => (
                false,
                "The temporary file for this output could not be confirmed gone",
            ),
        };
        check(NO_TEMPORARY_OUTPUT, passed, detail)
    }
}

fn report(
    checks: Vec<VerificationCheck>,
    changes: Vec<FieldChangeRow>,
    (remaining_privacy_count, remaining_privacy_categories): (usize, Vec<&'static str>),
) -> EditVerificationReport {
    EditVerificationReport {
        verified: checks.iter().all(|c| c.passed),
        fields_changed: changes.iter().filter(|row| row.changed).count(),
        checks,
        changes,
        remaining_privacy_count,
        remaining_privacy_categories,
    }
}

// ------------------------------------------------------------ Global checks ---

/// What each planned action requires of the output's field: the written value,
/// absence, or the value the fresh inspection found. A field the output holds
/// twice with different values has no one value, so it never matches.
pub(crate) fn requested_changes_applied(
    plan: &EditPlan,
    before: &NormalizedGlobals,
    after: &NormalizedGlobals,
) -> VerificationCheck {
    let (mut written, mut deleted, mut kept) = (0usize, 0usize, 0usize);
    let mut wrong: Vec<&'static str> = Vec::new();
    for action in plan.actions() {
        let key = action.field.key();
        let expected = match &action.change {
            FieldChange::Write(value) => {
                written += 1;
                Some(value.as_str())
            }
            FieldChange::Delete => {
                deleted += 1;
                None
            }
            FieldChange::Keep => {
                kept += 1;
                before.get(key)
            }
        };
        if after.ambiguous().contains(key) || after.get(key) != expected {
            wrong.push(action.field.label());
        }
    }
    if wrong.is_empty() {
        check(
            REQUESTED_CHANGES_APPLIED,
            true,
            format!(
                "{} as planned: {written} written, {deleted} removed, {kept} kept",
                plural(plan.actions().len(), "requested field", "requested fields")
            ),
        )
    } else {
        check(
            REQUESTED_CHANGES_APPLIED,
            false,
            format!("Requested change not applied to {}", wrong.join(", ")),
        )
    }
}

/// The output's container-level metadata is exactly the plan's: every key,
/// every value, nothing missing, nothing added, and no key reported twice with
/// two values.
pub(crate) fn other_metadata_preserved(
    plan: &EditPlan,
    after: &NormalizedGlobals,
) -> VerificationCheck {
    let expected = plan.expected_globals();
    let actual = after.fields();
    let passed = actual == expected && after.ambiguous().is_empty();
    if passed {
        return check(
            OTHER_METADATA_PRESERVED,
            true,
            format!(
                "{} exactly as planned",
                plural(expected.len(), "container field", "container fields")
            ),
        );
    }

    let missing = expected.keys().filter(|k| !actual.contains_key(*k)).count();
    let changed = expected
        .iter()
        .filter(|(k, v)| actual.get(*k).is_some_and(|value| value != *v))
        .count();
    let added = actual.keys().filter(|k| !expected.contains_key(*k)).count();
    let conflicting = after.ambiguous().len();
    let parts: Vec<String> = [
        (missing, "missing"),
        (changed, "changed"),
        (added, "unexpected"),
        (conflicting, "with conflicting values"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect();
    check(
        OTHER_METADATA_PRESERVED,
        false,
        format!(
            "Container metadata differs from the plan ({})",
            parts.join(", ")
        ),
    )
}

// ------------------------------------------------------------ Stream checks ---

/// The output's streams the plan lists, which is every stream but data
/// tracks: those are the tracks the muxer rebuilds, and they are checked
/// apart. Paired with the plan by position in this list, never by raw index,
/// because a rebuilt track can land anywhere among them (an MP4's chapter
/// track is written before its cover).
fn ordinary_streams(report: &MetadataReport) -> Vec<&StreamSummary> {
    report
        .streams
        .iter()
        .filter(|s| s.kind != StreamKind::Data)
        .collect()
}

/// Same number, same order, same kind, same codec parameters and the same
/// cover art flag, stream by stream. A parameter comparison, not packet or
/// byte identity.
pub(crate) fn stream_structure_preserved(
    plan: &EditPlan,
    kept: &[&StreamSummary],
) -> VerificationCheck {
    let expected = plan.expected_streams();
    if kept.len() != expected.len() {
        return check(
            STREAM_STRUCTURE_PRESERVED,
            false,
            format!(
                "Stream count changed: {} expected, {} found",
                expected.len(),
                kept.len()
            ),
        );
    }
    // Same length, so `zip` examines every stream.
    for (want, got) in expected.iter().zip(kept) {
        let problem = if want.kind != got.kind {
            format!(
                "Stream order changed: {} found where {} stream {} was expected",
                got.kind.label(),
                want.kind.label(),
                want.input_index
            )
        } else if want.identity != got.identity {
            format!(
                "{} stream {} changed codec parameters",
                want.kind.label(),
                want.input_index
            )
        } else if want.attached_pic != got.attached_pic {
            format!(
                "{} stream {} changed its cover art flag",
                want.kind.label(),
                want.input_index
            )
        } else {
            continue;
        };
        return check(STREAM_STRUCTURE_PRESERVED, false, problem);
    }
    check(
        STREAM_STRUCTURE_PRESERVED,
        true,
        format!(
            "{} kept in order with codec parameters preserved",
            plural(expected.len(), "stream", "streams")
        ),
    )
}

/// Each planned stream's tags, normalised exactly as the planner normalised
/// them -- the muxer's own keys left out, nothing else -- equal the plan's. An
/// ISO-BMFF track name the edit wrote back is compared as ffprobe reads it
/// after the remux, as `name`.
pub(crate) fn stream_metadata_preserved(
    plan: &EditPlan,
    after: &MetadataReport,
    kept: &[&StreamSummary],
) -> VerificationCheck {
    let expected = plan.expected_streams();
    if kept.len() != expected.len() {
        // Without the same streams there is nothing to pair tags with.
        return check(
            STREAM_METADATA_PRESERVED,
            false,
            "Stream metadata could not be compared: the stream count changed",
        );
    }
    let mut problems: Vec<String> = Vec::new();
    for (want, got) in expected.iter().zip(kept) {
        let stream = format!("{} stream {}", want.kind.label(), want.input_index);
        match stream_tags(after, got, plan.muxer()) {
            Err(_) => problems.push(format!("{stream} has conflicting metadata")),
            Ok(tags) if tags != want.tags => problems.push(format!(
                "{stream} metadata changed ({})",
                tag_difference(&want.tags, &tags)
            )),
            Ok(_) => {}
        }
    }
    if problems.is_empty() {
        let tags: usize = expected.iter().map(|s| s.tags.len()).sum();
        check(
            STREAM_METADATA_PRESERVED,
            true,
            format!(
                "{} on {} preserved",
                plural(tags, "stream tag", "stream tags"),
                plural(expected.len(), "stream", "streams")
            ),
        )
    } else {
        check(STREAM_METADATA_PRESERVED, false, problems.join(", "))
    }
}

/// How two tag sets differ, in counts: key names and values stay out of it.
fn tag_difference(
    expected: &BTreeMap<String, String>,
    actual: &BTreeMap<String, String>,
) -> String {
    let missing = expected.keys().filter(|k| !actual.contains_key(*k)).count();
    let changed = expected
        .iter()
        .filter(|(k, v)| actual.get(*k).is_some_and(|value| value != *v))
        .count();
    let added = actual.keys().filter(|k| !expected.contains_key(*k)).count();
    [
        (missing, "missing"),
        (changed, "changed"),
        (added, "unexpected"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect::<Vec<_>>()
    .join(", ")
}

// ----------------------------------------------------------------- Chapters ---

/// The same chapters: count, and each one's start, end and title, in order.
/// The position ffprobe numbers them by is not compared; it is only their
/// order, which the pairing already is.
pub(crate) fn chapters_preserved(plan: &EditPlan, after: &MetadataReport) -> VerificationCheck {
    let expected = plan.expected_chapters();
    let actual = &after.chapters;
    if actual.len() != expected.len() {
        return check(
            CHAPTERS_PRESERVED,
            false,
            format!(
                "Chapter count changed: {} expected, {} found",
                expected.len(),
                actual.len()
            ),
        );
    }
    for (position, (want, got)) in expected.iter().zip(actual).enumerate() {
        let number = position + 1;
        if want.start_time != got.start_time || want.end_time != got.end_time {
            return check(
                CHAPTERS_PRESERVED,
                false,
                format!("Chapter {number} timing changed"),
            );
        }
        if want.title != got.title {
            return check(
                CHAPTERS_PRESERVED,
                false,
                format!("Chapter {number} title changed"),
            );
        }
    }
    check(
        CHAPTERS_PRESERVED,
        true,
        if expected.is_empty() {
            "No chapters, as in the original".to_string()
        } else {
            format!(
                "{} preserved",
                plural(expected.len(), "chapter", "chapters")
            )
        },
    )
}

// --------------------------------------------------------- Rebuilt tracks ---

/// The output's data tracks are exactly the tracks the plan says the muxer
/// rebuilds, recognised the way the planner recognises them, and nothing else:
/// no telemetry, no unrecognised data, and no rebuilt track the plan did not
/// expect -- which, outside ISO-BMFF, is any data track at all. A rebuilt
/// timecode track must carry the timecode of the one video stream it was
/// rebuilt from.
///
/// A rebuilt track's own tags are not compared: the muxer writes them itself
/// (a rebuilt `tmcd` track names its handler after the video track).
pub(crate) fn structural_tracks_preserved(
    plan: &EditPlan,
    after: &MetadataReport,
) -> VerificationCheck {
    let mut rebuilt: Vec<RebuiltTrack> = Vec::new();
    let mut unrecognised = 0usize;
    for stream in after.streams.iter().filter(|s| s.kind == StreamKind::Data) {
        if is_chapter_track(stream) {
            rebuilt.push(RebuiltTrack::Chapters);
        } else if is_timecode_track(stream) {
            rebuilt.push(RebuiltTrack::Timecode);
        } else {
            unrecognised += 1;
        }
    }
    rebuilt.sort();

    let mut problems: Vec<String> = Vec::new();
    if unrecognised > 0 {
        problems.push(format!(
            "{} found",
            plural(
                unrecognised,
                "unexpected data track",
                "unexpected data tracks"
            )
        ));
    }
    let expected = plan.rebuilt_tracks();
    for (track, label) in [
        (RebuiltTrack::Chapters, "chapter track"),
        (RebuiltTrack::Timecode, "timecode track"),
    ] {
        let want = expected.iter().filter(|t| **t == track).count();
        let got = rebuilt.iter().filter(|t| **t == track).count();
        if got < want {
            problems.push(format!("Rebuilt {label} missing"));
        } else if got > want {
            problems.push(format!("Unexpected {label} found"));
        }
    }

    if expected.contains(&RebuiltTrack::Timecode) {
        // The planner rebuilds a timecode track only beside exactly one video
        // stream, whose `timecode` tag it matched against the track's.
        let source = plan
            .expected_streams()
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .and_then(|s| s.tags.get("timecode"))
            .map(String::as_str);
        let mismatched = after
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Data && is_timecode_track(s))
            .any(|s| source.is_none() || stream_tag(after, s.index, "timecode") != source);
        if mismatched {
            problems.push("Rebuilt timecode track does not match the video's timecode".into());
        }
    }

    if problems.is_empty() {
        let describe = |track: &RebuiltTrack| match track {
            RebuiltTrack::Chapters => "chapter track",
            RebuiltTrack::Timecode => "timecode track",
        };
        check(
            STRUCTURAL_TRACKS_PRESERVED,
            true,
            if expected.is_empty() {
                "No data tracks, as planned".to_string()
            } else {
                format!(
                    "Only the rebuilt {} present",
                    expected
                        .iter()
                        .map(describe)
                        .collect::<Vec<_>>()
                        .join(" and ")
                )
            },
        )
    } else {
        check(STRUCTURAL_TRACKS_PRESERVED, false, problems.join(", "))
    }
}

// ------------------------------------------------------------ Field rows ---

/// One row per requested field, from the pre-edit inspection and the output's.
pub(crate) fn change_rows(
    plan: &EditPlan,
    before: &NormalizedGlobals,
    after: &NormalizedGlobals,
) -> Vec<FieldChangeRow> {
    plan.actions()
        .iter()
        .map(|action| {
            let key = action.field.key();
            let (was, now) = (before.get(key), after.get(key));
            FieldChangeRow {
                field: action.field,
                before: was.map(str::to_string),
                after: now.map(str::to_string),
                changed: effective(was) != effective(now),
            }
        })
        .collect()
}

// ------------------------------------------------------ Remaining privacy ---

/// HIGH and MEDIUM privacy findings in the output, as the scan's classifier
/// sees them, minus what this edit put there on purpose: a container-level
/// field the plan wrote. An Artist the user just set is their choice, not a
/// leftover; an Artist the file already had, kept by Fill if missing, is a
/// leftover like any other. Structural findings and LOW ones are not a warning.
///
/// Counts and category labels only, never a value. Edit keeps metadata by
/// design, so none of this bears on whether the edit verified.
pub(crate) fn remaining_privacy(
    plan: &EditPlan,
    after: &MetadataReport,
) -> (usize, Vec<&'static str>) {
    let written: BTreeSet<&str> = plan
        .actions()
        .iter()
        .filter(|action| matches!(action.change, FieldChange::Write(_)))
        .map(|action| action.field.key())
        .collect();
    let mut categories: Vec<&'static str> = Vec::new();
    let mut count = 0usize;
    // Already most severe first.
    for finding in classify(after) {
        if finding.severity < Severity::Medium || finding.category.is_structural() {
            continue;
        }
        if finding.scope == MetadataScope::Format && written.contains(finding.source_key.as_str()) {
            continue;
        }
        count += 1;
        if !categories.contains(&finding.category_label) {
            categories.push(finding.category_label);
        }
    }
    (count, categories)
}

#[cfg(test)]
#[path = "edit_verify_tests.rs"]
mod edit_verify_tests;
