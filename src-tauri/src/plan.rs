//! What the cleaner is about to do, stated before it does it.
//!
//! In v0.5 the plan is derived, never chosen: it is a description of the fixed
//! pipeline applied to one particular file, not a set of options. Two of its
//! fields carry weight today -- `remove_chapters` and `remove_data_streams`
//! gate verifier checks 5 and 6 -- and the rest describe the run. Nothing in
//! the UI reads the plan yet, so no wording here reaches a user.
//!
//! The cleaner's behaviour is unchanged by this module. If a plan ever
//! disagreed with `ffmpeg_args`, the plan would be the thing that is wrong.

use serde::Serialize;

use crate::inspect::MetadataReport;
use crate::privacy::PrivacyFinding;
use crate::{ContainerProfile, FormatProfile};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleaningPlan {
    /// Always true: `-map_metadata -1` drops container-level tags.
    pub remove_format_metadata: bool,
    /// Always true: `-map_metadata:s -1` drops per-stream tags.
    pub remove_stream_metadata: bool,
    /// `-map_chapters -1`. Reported as false when the input has no chapters, so
    /// the verifier does not claim to have removed something that never existed.
    pub remove_chapters: bool,
    /// `-dn` for data streams, `-map -0:t?` for attachments. Same reasoning as
    /// chapters: false when the input had no non-media track, so the verifier
    /// does not claim to have removed something that never existed.
    pub remove_data_streams: bool,
    /// Human-readable description of the muxing route for this container.
    pub container_strategy: &'static str,
    /// Stream indices that must survive, with their kind. Descriptive: the
    /// verifier works from the inspected reports rather than from this list,
    /// comparing every kind `is_media` keeps.
    pub preserved_streams: Vec<PreservedStream>,
    pub expected_removed_fields: usize,
    /// Findings that are not container bookkeeping: what the user is promised.
    pub sensitive_findings: usize,
    pub expected_removed_chapters: usize,
    pub expected_removed_data_streams: usize,
    /// What the product promises about this run. Every entry has a
    /// corresponding check in `verify`, though the verifier does not read this
    /// list: nothing is listed here that the run does not actually confirm.
    pub guarantees: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreservedStream {
    pub index: usize,
    pub kind: &'static str,
    pub codec_name: Option<String>,
}

fn container_strategy(profile: FormatProfile) -> &'static str {
    match profile.container {
        ContainerProfile::IsoBmff => {
            "ISO-BMFF remux, faststart first with one plain retry, streams copied"
        }
        ContainerProfile::Matroska => "Matroska remux, streams copied",
        ContainerProfile::WebM => "WebM remux, streams copied",
        ContainerProfile::Avi => "AVI remux with unknown streams skipped, streams copied",
    }
}

/// Build the plan for one inspected file.
pub fn plan_for(
    report: &MetadataReport,
    findings: &[PrivacyFinding],
    profile: FormatProfile,
) -> CleaningPlan {
    let data_streams = report.non_media_streams().count();
    let chapters = report.chapters.len();

    // `-map_metadata -1`, `-map_metadata:s -1` and `-map_chapters -1` between them
    // drop every field the inspector found, so the expected count is simply all of
    // them. The muxer then writes its own structural tags back (`major_brand`,
    // `handler_name`, `language`); the verifier measures what actually went and
    // reports that number rather than this estimate.
    let expected_removed_fields = report.fields.len();

    // What the user is actually being promised: the disclosures that are not just
    // container bookkeeping.
    let sensitive_findings = findings
        .iter()
        .filter(|f| !f.category.is_structural())
        .count();

    let mut guarantees = vec![
        "Video streams use stream copy; no transcoding path is configured",
        "Audio streams use stream copy; no transcoding path is configured",
        "The original file is left untouched",
        "The output keeps the original file extension",
    ];
    if data_streams > 0 {
        guarantees.push("Data tracks are removed with their payload");
    }
    if chapters > 0 {
        guarantees.push("Chapter markers are removed");
    }

    CleaningPlan {
        remove_format_metadata: true,
        remove_stream_metadata: true,
        remove_chapters: chapters > 0,
        remove_data_streams: data_streams > 0,
        container_strategy: container_strategy(profile),
        preserved_streams: report
            .media_streams()
            .map(|s| PreservedStream {
                index: s.index,
                kind: s.kind.label(),
                codec_name: s.identity.codec_name.clone(),
            })
            .collect(),
        expected_removed_fields,
        sensitive_findings,
        expected_removed_chapters: chapters,
        expected_removed_data_streams: data_streams,
        guarantees,
    }
}
