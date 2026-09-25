//! The per-file Edit plan: whether one file can be edited without losing
//! anything the user did not ask to change, and if so, exactly what the
//! output must contain and the FFmpeg arguments that produce it.
//!
//! Planning is the gate. A file either gets an [`EditPlan`] or an
//! [`EditRefusal`], and the refusal comes before an output ID is reserved or
//! FFmpeg runs. There is no third outcome in which the edit goes ahead and
//! something is quietly left behind: whatever the pinned FFmpeg was not seen
//! to carry over intact is refused.
//!
//! What may change, and nothing else:
//!
//! * the fields the request actually changes (see [`changed_fields`]);
//! * metadata the muxer writes itself on every remux ([`EditMuxer::muxer_owned_keys`]
//!   at container level, [`stream_muxer_owned_keys`] per stream);
//! * the ISO-BMFF chapter and timecode tracks, which `-dn` drops and the muxer
//!   rebuilds, from the chapter list and from the video stream's `timecode`
//!   tag respectively.
//!
//! Clean is untouched: it has its own options, plan, arguments and verifier,
//! and none of them read anything here.

// The batch runner that calls this lands in the next task.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::edit::{
    changed_fields, edit_muxer, unpreserved_keys, EditMuxer, EditableField, FieldChange,
    NormalizedGlobals, ValidEditRequest,
};
use crate::inspect::{
    self, ChapterSummary, MetadataReport, MetadataScope, StreamIdentity, StreamKind, StreamSummary,
};
use crate::sidecar::ffmpeg_program;
use crate::{format_id, ContainerProfile, FormatProfile, MuxAttempt, TEMP_PREFIX};

// -------------------------------------------------------- Fresh inspection ---

/// An inspection taken for this edit, immediately before it runs, together
/// with the file it inspected.
///
/// Planning takes nothing else. The privacy scan the page showed is
/// information for the user; by the time a batch reaches a file, the file may
/// have changed, and `FillIfMissing` in particular must decide against what
/// is on disk now. The only way to make one outside the tests is
/// [`FreshInspection::take`].
pub struct FreshInspection {
    path: PathBuf,
    report: MetadataReport,
}

impl FreshInspection {
    /// Run ffprobe on `path` now.
    pub fn take(path: &Path) -> Result<Self, String> {
        inspect::inspect(path).map(|report| FreshInspection {
            path: path.to_path_buf(),
            report,
        })
    }

    /// A report that stands in for a fresh inspection of `path` in tests.
    #[cfg(test)]
    pub fn from_report(path: impl Into<PathBuf>, report: MetadataReport) -> Self {
        FreshInspection {
            path: path.into(),
            report,
        }
    }

    /// The file that was inspected, which is the file FFmpeg will read.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn report(&self) -> &MetadataReport {
        &self.report
    }
}

/// Where FFmpeg writes an Edit output until it is complete: the same
/// temporary name Clean uses, in the output folder, with the input's own
/// extension spelling. The ID is always ten digits, so the length of this path
/// is known before an ID is reserved.
pub fn edit_temp_path(output_dir: &Path, id: &str, input: &Path) -> PathBuf {
    let extension = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    output_dir.join(format!("{TEMP_PREFIX}{id}.{extension}"))
}

// ----------------------------------------------------------------- Refusal ---

/// Why a file cannot be edited safely. Carries field names, metadata key names
/// and codec tags -- structure, never a value read out of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditRefusal {
    /// The format has no Edit muxer facts at all.
    UnsupportedFormat,
    /// A stream FFmpeg could not classify, which `-ignore_unknown` would drop.
    UnknownStream,
    /// Detected cover art this muxer does not carry over: MOV drops it and
    /// Matroska turns it into an ordinary one-frame video track.
    CoverArt,
    /// A data track that is not one the muxer is proven to rebuild: telemetry
    /// (`gpmd`), iPhone timed metadata (`mebx`) or anything else. The codec
    /// tag is kept for the message when ffprobe reported one.
    DataTrack { codec_tag: Option<String> },
    /// An attachment in a container Edit has not proven it survives in.
    Attachment,
    /// An ISO-BMFF stream carries a `title` tag. The muxer writes a stream
    /// title back as the track's `name`, so it would not come back as itself.
    StreamTitle,
    /// A stream reported the same tag twice with different values.
    ConflictingStreamMetadata,
    /// A field the request edits has conflicting values in the file, so which
    /// one is the field's value -- and whether it is "missing" -- is unknown.
    AmbiguousField(EditableField),
    /// Untouched metadata the muxer would drop. Key names only.
    UnpreservedMetadata(Vec<String>),
    /// The whole FFmpeg command line -- paths, flags, edits and restored track
    /// names -- would not fit the conservative Windows budget.
    CommandLineTooLong,
}

/// At most this many key names are listed in a message; the refusal itself
/// keeps them all.
const LISTED_KEYS: usize = 3;

impl EditRefusal {
    /// One sentence for the user. Never a value from the file.
    pub fn message(&self) -> String {
        match self {
            EditRefusal::UnsupportedFormat => "Edit does not support this format.".into(),
            EditRefusal::UnknownStream => {
                "This file has a track FFmpeg cannot identify, and editing would drop it.".into()
            }
            EditRefusal::CoverArt => {
                "Edit can't keep the detected cover art in this format safely.".into()
            }
            EditRefusal::DataTrack { codec_tag } => match codec_tag {
                Some(tag) => format!(
                    "This file has a data track ({tag}) that editing can't preserve, such as telemetry."
                ),
                None => {
                    "This file has a data track that editing can't preserve, such as telemetry."
                        .into()
                }
            },
            EditRefusal::Attachment => {
                "This file has an attachment that editing can't preserve in this format.".into()
            }
            EditRefusal::StreamTitle => {
                "A track title in this file would not survive editing in this format.".into()
            }
            EditRefusal::ConflictingStreamMetadata => {
                "A track in this file has conflicting metadata and can't be edited safely.".into()
            }
            EditRefusal::AmbiguousField(field) => format!(
                "{} has conflicting metadata values and can't be edited safely.",
                field.label()
            ),
            EditRefusal::UnpreservedMetadata(keys) => {
                let mut listed = keys
                    .iter()
                    .take(LISTED_KEYS)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                if keys.len() > LISTED_KEYS {
                    listed.push_str(&format!(" and {} more", keys.len() - LISTED_KEYS));
                }
                format!("Editing would lose metadata this format can't keep: {listed}.")
            }
            EditRefusal::CommandLineTooLong => {
                "This file's paths and metadata are too long to edit safely.".into()
            }
        }
    }
}

// -------------------------------------------------------------------- Plan ---

/// What one requested field does in this file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldAction {
    pub field: EditableField,
    pub change: FieldChange,
}

/// A track `-dn` removes and the ISO-BMFF muxer writes again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RebuiltTrack {
    /// The QuickTime chapter track, rebuilt from the chapter list.
    Chapters,
    /// The `tmcd` timecode track, rebuilt from the video stream's `timecode`.
    Timecode,
}

/// One stream the output must carry, in output order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedEditStream {
    pub input_index: usize,
    /// FFmpeg's output stream index, the `N` in `-metadata:s:N`. Data tracks
    /// removed by `-dn` shift everything after them, so this is not the input
    /// index. Nor is it the stream's position in the written file: the muxer
    /// places the tracks it rebuilds among these (an MP4's chapter track lands
    /// before its cover), so a written file is matched stream by stream with
    /// its data tracks left out, never by this number.
    pub output_index: usize,
    pub kind: StreamKind,
    pub identity: StreamIdentity,
    pub attached_pic: bool,
    /// Normalised tags the stream must still carry: lowercase keys, trimmed
    /// values, the muxer's own keys left out.
    pub tags: BTreeMap<String, String>,
}

/// A stream tag written back on purpose because the muxer would otherwise
/// lose it: ISO-BMFF reports a track name as `name` but writes one only from
/// `title`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamTitleRestore {
    pub output_index: usize,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct EditPlan {
    format: FormatProfile,
    muxer: EditMuxer,
    actions: Vec<FieldAction>,
    expected_globals: BTreeMap<String, String>,
    expected_streams: Vec<ExpectedEditStream>,
    rebuilt_tracks: Vec<RebuiltTrack>,
    expected_chapters: Vec<ChapterSummary>,
    restores: Vec<StreamTitleRestore>,
}

impl EditPlan {
    pub fn format(&self) -> FormatProfile {
        self.format
    }

    pub fn muxer(&self) -> EditMuxer {
        self.muxer
    }

    /// One entry per requested edit, in field order.
    pub fn actions(&self) -> &[FieldAction] {
        &self.actions
    }

    /// Whether any requested field changes. A plan where every edit resolves
    /// to `Keep` is still a plan: the file is copied and verified unchanged.
    pub fn changes_fields(&self) -> bool {
        self.actions.iter().any(|a| a.change != FieldChange::Keep)
    }

    /// The normalised container-level metadata the output must carry, exactly.
    pub fn expected_globals(&self) -> &BTreeMap<String, String> {
        &self.expected_globals
    }

    /// Every stream the output must carry except rebuilt tracks, in order.
    pub fn expected_streams(&self) -> &[ExpectedEditStream] {
        &self.expected_streams
    }

    pub fn rebuilt_tracks(&self) -> &[RebuiltTrack] {
        &self.rebuilt_tracks
    }

    pub fn expected_chapters(&self) -> &[ChapterSummary] {
        &self.expected_chapters
    }

    pub fn restores(&self) -> &[StreamTitleRestore] {
        &self.restores
    }

    /// The attempts to make, in order: faststart first, then without it, for
    /// ISO-BMFF; one plain attempt otherwise. The same convention as Clean.
    pub fn mux_attempts(&self) -> &'static [MuxAttempt] {
        self.format.mux_attempts()
    }

    /// The FFmpeg arguments for one attempt, refused if the command line they
    /// make would not fit [`COMMAND_LINE_BUDGET`]. The only way to get
    /// arguments to launch: planning calls this for every attempt against a
    /// stand-in output path of the real length, and the runner calls it again
    /// with the real one, so the check can never drift from what is launched.
    pub fn ffmpeg_args(
        &self,
        input: &Path,
        output: &Path,
        attempt: MuxAttempt,
    ) -> Result<Vec<String>, EditRefusal> {
        let args = self.build_args(input, output, attempt);
        let program = ffmpeg_program().to_string_lossy();
        if command_line_length(&program, &args) > COMMAND_LINE_BUDGET {
            return Err(EditRefusal::CommandLineTooLong);
        }
        Ok(args)
    }

    /// The arguments for one attempt, unchecked. Always a stream copy: there
    /// is no codec argument anywhere in this function, and no fallback.
    fn build_args(&self, input: &Path, output: &Path, attempt: MuxAttempt) -> Vec<String> {
        let iso = self.format.container == ContainerProfile::IsoBmff;
        // -n, not -y: the output name is reserved as free, so an existing file
        // means something is wrong and must not be overwritten.
        let mut args: Vec<String> = vec!["-n".into()];
        if self.format.container == ContainerProfile::Avi {
            // Inert: a file with an unknown stream is refused before this.
            args.push("-ignore_unknown".into());
        }
        args.extend([
            "-i".into(),
            input.to_string_lossy().into_owned(),
            // Everything, attachments and cover art included: Edit keeps the
            // file's structure. No negative maps.
            "-map".into(),
            "0".into(),
            "-c".into(),
            "copy".into(),
            // Keep the file's metadata and chapters; the edits below change
            // only what was asked.
            "-map_metadata".into(),
            "0".into(),
            "-map_chapters".into(),
            "0".into(),
        ]);
        if iso {
            // The ISO-BMFF chapter and timecode tracks cannot be copied as
            // data (MOV refuses the chapter track, MP4 the timecode track);
            // the muxer rebuilds both. Planning refused every other data track.
            args.push("-dn".into());
        }
        args.extend(self.metadata_args());
        args.extend(["-fflags".into(), "+bitexact".into()]);
        if attempt == MuxAttempt::FastStart {
            debug_assert!(iso);
            args.extend(["-movflags".into(), "+faststart".into()]);
        }
        args.extend([
            "-f".into(),
            self.format.output_muxer.into(),
            output.to_string_lossy().into_owned(),
        ]);
        args
    }

    /// The edits, then the stream title restores. Each is its own argument
    /// pair; nothing is quoted or escaped, because nothing passes a shell.
    fn metadata_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for action in &self.actions {
            let key = action.field.key();
            match &action.change {
                FieldChange::Write(value) => {
                    args.extend(["-metadata".into(), format!("{key}={value}")])
                }
                // `key=` is FFmpeg's deletion: an empty value is not stored.
                FieldChange::Delete => args.extend(["-metadata".into(), format!("{key}=")]),
                FieldChange::Keep => {}
            }
        }
        for restore in &self.restores {
            args.extend([
                format!("-metadata:s:{}", restore.output_index),
                format!("title={}", restore.title),
            ]);
        }
        args
    }
}

/// The most a whole FFmpeg command line may need, in UTF-16 units, program
/// included. Windows refuses to start a process whose command line exceeds
/// 32,767 (`WinError 206`); this is half of that, measured against an upper
/// bound, so a plan that passes cannot come near the real limit.
pub const COMMAND_LINE_BUDGET: usize = 16_384;

/// An upper bound on the characters Windows needs for `program` and `args`
/// on one command line, as the standard library builds it.
///
/// Every argument is counted as if it were quoted: its UTF-16 length, two
/// quotes, one separating space, and one extra character for every `"` and
/// `\` in it. Quoting escapes a `"` with a backslash and doubles a run of
/// backslashes only where it precedes a quote, so one per such character can
/// only overcount.
pub fn command_line_length(program: &str, args: &[String]) -> usize {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|arg| {
            let escapes = arg.chars().filter(|c| matches!(c, '"' | '\\')).count();
            arg.encode_utf16().count() + escapes + 3
        })
        .sum()
}

// ---------------------------------------------------------- Stream policy ---

/// Stream-level keys each muxer writes itself.
///
/// * ISO-BMFF: `vendor_id` is always the muxer's own (`FFMP`); a camera's
///   value does not survive.
/// * Matroska and WebM: `duration` is recomputed for every track.
/// * AVI: none observed.
pub fn stream_muxer_owned_keys(muxer: EditMuxer) -> &'static [&'static str] {
    match muxer {
        EditMuxer::Mp4 | EditMuxer::Mov => &["vendor_id"],
        EditMuxer::Matroska | EditMuxer::WebM => &["duration"],
        EditMuxer::Avi => &[],
    }
}

fn is_iso(muxer: EditMuxer) -> bool {
    matches!(muxer, EditMuxer::Mp4 | EditMuxer::Mov)
}

/// A stream's tags as the plan compares them. A key reported twice with two
/// values is refused rather than guessed at.
fn stream_tags(
    report: &MetadataReport,
    stream: &StreamSummary,
    muxer: EditMuxer,
) -> Result<BTreeMap<String, String>, EditRefusal> {
    let owned = stream_muxer_owned_keys(muxer);
    let mut tags = BTreeMap::new();
    for field in report
        .fields
        .iter()
        .filter(|f| f.scope == MetadataScope::Stream && f.stream_index == Some(stream.index))
    {
        let key = field.key.trim().to_ascii_lowercase();
        if owned.contains(&key.as_str()) {
            continue;
        }
        let value = field.value.trim().to_string();
        match tags.get(&key) {
            Some(existing) if *existing != value => {
                return Err(EditRefusal::ConflictingStreamMetadata)
            }
            Some(_) => {}
            None => {
                tags.insert(key, value);
            }
        }
    }
    Ok(tags)
}

/// A stream tag's value, as the planner reads it.
fn stream_tag<'a>(report: &'a MetadataReport, index: usize, key: &str) -> Option<&'a str> {
    report
        .fields
        .iter()
        .find(|f| f.scope == MetadataScope::Stream && f.stream_index == Some(index) && f.key == key)
        .map(|f| f.value.trim())
        .filter(|v| !v.is_empty())
}

fn is_chapter_track(stream: &StreamSummary) -> bool {
    stream.identity.codec_name.as_deref() == Some("bin_data")
        && stream.identity.codec_tag.as_deref() == Some("text")
}

fn is_timecode_track(stream: &StreamSummary) -> bool {
    stream.identity.codec_tag.as_deref() == Some("tmcd")
}

/// Which ISO-BMFF data tracks the muxer is proven to rebuild, or why not.
///
/// Only the shapes seen to come back identical with the pinned FFmpeg:
///
/// * one chapter track (`bin_data`, tag `text`), and only when the file has
///   chapters to rebuild it from;
/// * one `tmcd` track, and only in a file with exactly one video stream --
///   cover art included, since a cover got a timecode track of its own at
///   write time and none after an edit -- whose `timecode` tag matches the
///   track's.
///
/// Any other data track is refused, whatever its tag says.
fn rebuilt_iso_tracks(report: &MetadataReport) -> Result<Vec<RebuiltTrack>, EditRefusal> {
    let refuse = |stream: &StreamSummary| EditRefusal::DataTrack {
        codec_tag: stream.identity.codec_tag.clone(),
    };
    let video: Vec<&StreamSummary> = report
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .collect();

    let mut rebuilt = Vec::new();
    for stream in report.streams.iter().filter(|s| s.kind == StreamKind::Data) {
        let track = if is_chapter_track(stream) {
            if report.chapters.is_empty() {
                return Err(refuse(stream));
            }
            RebuiltTrack::Chapters
        } else if is_timecode_track(stream) {
            let own = stream_tag(report, stream.index, "timecode");
            let matches_video = match video.as_slice() {
                [only] => own.is_some() && stream_tag(report, only.index, "timecode") == own,
                _ => false,
            };
            if !matches_video {
                return Err(refuse(stream));
            }
            RebuiltTrack::Timecode
        } else {
            return Err(refuse(stream));
        };
        if rebuilt.contains(&track) {
            // Two of a kind: the muxer rebuilds one.
            return Err(refuse(stream));
        }
        rebuilt.push(track);
    }
    rebuilt.sort();
    Ok(rebuilt)
}

// ---------------------------------------------------------------- Planning ---

/// Plan one file, or refuse it.
///
/// The order of the checks is fixed so a file with several problems always
/// gets the same reason: structure first, then stream metadata, then the
/// requested fields, then everything else the edit would lose, and last
/// whether the command that does it fits on a command line. `output_dir` is
/// where the output will be written: its length is part of that command.
pub fn plan_edit(
    request: &ValidEditRequest,
    format: FormatProfile,
    fresh: &FreshInspection,
    output_dir: &Path,
) -> Result<EditPlan, EditRefusal> {
    let report = fresh.report();
    let muxer = edit_muxer(format).ok_or(EditRefusal::UnsupportedFormat)?;
    let edits = request.edits();

    // 1. Structure.
    for stream in &report.streams {
        match stream.kind {
            StreamKind::Unknown => return Err(EditRefusal::UnknownStream),
            StreamKind::Attachment if muxer != EditMuxer::Matroska => {
                return Err(EditRefusal::Attachment)
            }
            StreamKind::Data if !is_iso(muxer) => {
                return Err(EditRefusal::DataTrack {
                    codec_tag: stream.identity.codec_tag.clone(),
                })
            }
            _ => {}
        }
        if stream.attached_pic && muxer != EditMuxer::Mp4 {
            return Err(EditRefusal::CoverArt);
        }
    }
    let rebuilt_tracks = if is_iso(muxer) {
        rebuilt_iso_tracks(report)?
    } else {
        Vec::new()
    };

    // 2. Stream metadata, and the ISO-BMFF track names that must be written
    //    back. Data tracks are not in the output (they are rebuilt), so they
    //    take no output index.
    let mut expected_streams = Vec::new();
    let mut restores = Vec::new();
    for stream in report.streams.iter().filter(|s| s.kind != StreamKind::Data) {
        let tags = stream_tags(report, stream, muxer)?;
        let output_index = expected_streams.len();
        if is_iso(muxer) {
            if tags.contains_key("title") {
                return Err(EditRefusal::StreamTitle);
            }
            if let Some(name) = tags.get("name").filter(|name| !name.is_empty()) {
                restores.push(StreamTitleRestore {
                    output_index,
                    title: name.clone(),
                });
            }
        }
        expected_streams.push(ExpectedEditStream {
            input_index: stream.index,
            output_index,
            kind: stream.kind,
            identity: stream.identity.clone(),
            attached_pic: stream.attached_pic,
            tags,
        });
    }

    // 3. The requested fields, against this file as it is now.
    let globals = NormalizedGlobals::from_report(report, muxer);
    if let Some(edit) = edits
        .iter()
        .find(|edit| globals.ambiguous().contains(edit.field.key()))
    {
        return Err(EditRefusal::AmbiguousField(edit.field));
    }

    // 4. Everything untouched must survive.
    let lost = unpreserved_keys(&globals, muxer, edits);
    if !lost.is_empty() {
        return Err(EditRefusal::UnpreservedMetadata(lost.into_iter().collect()));
    }

    let actions: Vec<FieldAction> = edits
        .iter()
        .map(|edit| FieldAction {
            field: edit.field,
            change: edit.operation.resolve(globals.current(edit.field)),
        })
        .collect();
    debug_assert_eq!(
        changed_fields(&globals, edits),
        actions
            .iter()
            .filter(|a| a.change != FieldChange::Keep)
            .map(|a| a.field)
            .collect(),
        "the plan and the preservation check must agree on what changes"
    );

    let mut expected_globals = globals.fields().clone();
    for action in &actions {
        let key = action.field.key();
        match &action.change {
            FieldChange::Write(value) => {
                expected_globals.insert(key.to_string(), value.clone());
            }
            FieldChange::Delete => {
                expected_globals.remove(key);
            }
            FieldChange::Keep => {}
        }
    }

    let plan = EditPlan {
        format,
        muxer,
        actions,
        expected_globals,
        expected_streams,
        rebuilt_tracks,
        expected_chapters: report.chapters.clone(),
        restores,
    };

    // 5. The whole command, for every attempt it may take. The output path is
    //    not reserved yet, but its length is: the ID is always ten digits.
    let placeholder = edit_temp_path(output_dir, &format_id(0), fresh.path());
    for attempt in plan.mux_attempts() {
        plan.ffmpeg_args(fresh.path(), &placeholder, *attempt)?;
    }
    Ok(plan)
}

#[cfg(test)]
#[path = "edit_plan_tests.rs"]
mod edit_plan_tests;
