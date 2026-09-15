//! Structured media inspection.
//!
//! One `ffprobe -print_format json` run per file, normalised into the typed model
//! below. Nothing downstream — classifier, plan, verifier, UI — ever sees raw
//! ffprobe JSON: the shape of that JSON is ffprobe's business, and pinning the
//! rest of the app to it would make every schema change a breaking change.
//!
//! Human-readable FFmpeg output is never parsed. Where a fact is not in the JSON
//! it is reported as absent rather than guessed.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use crate::sidecar::{ffprobe, ffprobe_available, FFPROBE_MISSING};

/// Where a metadata field was found. Containers differ in what they can carry, so
/// the scope is part of the identity of a field: a `title` on the container and a
/// `title` on a stream are two different facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataScope {
    Format,
    Stream,
    Chapter,
}

impl MetadataScope {
    pub fn label(self) -> &'static str {
        match self {
            MetadataScope::Format => "Container",
            MetadataScope::Stream => "Stream",
            MetadataScope::Chapter => "Chapter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

impl StreamKind {
    fn from_codec_type(value: Option<&str>) -> Self {
        match value {
            Some("video") => StreamKind::Video,
            Some("audio") => StreamKind::Audio,
            Some("subtitle") => StreamKind::Subtitle,
            Some("data") => StreamKind::Data,
            Some("attachment") => StreamKind::Attachment,
            _ => StreamKind::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StreamKind::Video => "Video",
            StreamKind::Audio => "Audio",
            StreamKind::Subtitle => "Subtitle",
            StreamKind::Data => "Data",
            StreamKind::Attachment => "Attachment",
            StreamKind::Unknown => "Unknown",
        }
    }

    /// Streams the cleaner keeps. Everything else is dropped by `-dn`, payload and
    /// all, which is why data tracks are a privacy concern and not a bookkeeping
    /// detail.
    pub fn is_media(self) -> bool {
        matches!(
            self,
            StreamKind::Video | StreamKind::Audio | StreamKind::Subtitle
        )
    }
}

/// One metadata field, already normalised. `key` is lowercased for matching;
/// `display_key` keeps what ffprobe actually reported so the UI can show it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataField {
    pub key: String,
    pub display_key: String,
    pub value: String,
    pub scope: MetadataScope,
    pub stream_index: Option<usize>,
    pub stream_kind: Option<StreamKind>,
}

/// The codec facts that must not change when a stream is copied. Compared
/// wholesale by the verifier; any difference means something re-encoded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamIdentity {
    pub codec_name: Option<String>,
    pub codec_tag: Option<String>,
    pub profile: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub pix_fmt: Option<String>,
    pub sample_rate: Option<String>,
    pub channels: Option<u64>,
    pub channel_layout: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSummary {
    pub index: usize,
    pub kind: StreamKind,
    pub identity: StreamIdentity,
    /// ffprobe's `handler_name`, lifted out of the tags because it is how
    /// telemetry tracks (`GoPro MET`) announce themselves.
    pub handler_name: Option<String>,
    pub tag_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterSummary {
    pub index: usize,
    pub title: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

/// Everything the rest of the pipeline knows about one media file.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataReport {
    pub file_name: String,
    pub file_size: Option<u64>,
    /// ffprobe's `format_name`, e.g. `mov,mp4,m4a,3gp,3g2,mj2`.
    pub container: Option<String>,
    pub duration_seconds: Option<f64>,
    pub fields: Vec<MetadataField>,
    pub streams: Vec<StreamSummary>,
    pub chapters: Vec<ChapterSummary>,
}

impl MetadataReport {
    /// Data, attachment and unknown tracks: everything the cleaner drops whole.
    pub fn non_media_streams(&self) -> impl Iterator<Item = &StreamSummary> {
        self.streams.iter().filter(|s| !s.kind.is_media())
    }

    pub fn media_streams(&self) -> impl Iterator<Item = &StreamSummary> {
        self.streams.iter().filter(|s| s.kind.is_media())
    }
}

// ------------------------------------------------------------ JSON helpers ---

fn as_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// ffprobe reports some numbers as JSON numbers and others as strings (`"1920"`,
/// `"44100"`). Accept both rather than losing a field to its encoding.
fn as_u64(value: &Value, key: &str) -> Option<u64> {
    let found = value.get(key)?;
    found
        .as_u64()
        .or_else(|| found.as_str().and_then(|s| s.parse().ok()))
}

fn as_f64(value: &Value, key: &str) -> Option<f64> {
    let found = value.get(key)?;
    found
        .as_f64()
        .or_else(|| found.as_str().and_then(|s| s.parse().ok()))
}

/// A number that keeps its textual form, because the exact spelling is what gets
/// compared before and after (`"44100"`).
fn as_numeric_string(value: &Value, key: &str) -> Option<String> {
    let found = value.get(key)?;
    match found {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Pull a `tags` object into normalised fields. Non-string tag values are
/// stringified rather than dropped: an unexpected shape is still a disclosure.
fn collect_tags(
    container: &Value,
    scope: MetadataScope,
    stream_index: Option<usize>,
    stream_kind: Option<StreamKind>,
    into: &mut Vec<MetadataField>,
) {
    let Some(tags) = container.get("tags").and_then(Value::as_object) else {
        return;
    };
    for (key, value) in tags {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        into.push(MetadataField {
            key: key.trim().to_ascii_lowercase(),
            display_key: key.trim().to_string(),
            value: text.trim().to_string(),
            scope,
            stream_index,
            stream_kind,
        });
    }
}

fn parse_report(file_name: String, root: &Value) -> MetadataReport {
    let mut fields = Vec::new();

    let format = root.get("format");
    if let Some(format) = format {
        collect_tags(format, MetadataScope::Format, None, None, &mut fields);
    }

    let mut streams = Vec::new();
    if let Some(list) = root.get("streams").and_then(Value::as_array) {
        for (position, stream) in list.iter().enumerate() {
            // ffprobe always reports `index`; falling back to the position keeps
            // two streams from collapsing onto the same identity if it ever does not.
            let index = as_u64(stream, "index")
                .map(|i| i as usize)
                .unwrap_or(position);
            let kind =
                StreamKind::from_codec_type(stream.get("codec_type").and_then(Value::as_str));
            let before = fields.len();
            collect_tags(
                stream,
                MetadataScope::Stream,
                Some(index),
                Some(kind),
                &mut fields,
            );

            streams.push(StreamSummary {
                index,
                kind,
                identity: StreamIdentity {
                    codec_name: as_str(stream, "codec_name"),
                    codec_tag: as_str(stream, "codec_tag_string"),
                    profile: as_str(stream, "profile"),
                    width: as_u64(stream, "width"),
                    height: as_u64(stream, "height"),
                    pix_fmt: as_str(stream, "pix_fmt"),
                    sample_rate: as_numeric_string(stream, "sample_rate"),
                    channels: as_u64(stream, "channels"),
                    channel_layout: as_str(stream, "channel_layout"),
                },
                handler_name: stream
                    .get("tags")
                    .and_then(|t| t.get("handler_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tag_count: fields.len() - before,
            });
        }
    }

    let mut chapters = Vec::new();
    if let Some(list) = root.get("chapters").and_then(Value::as_array) {
        for (index, chapter) in list.iter().enumerate() {
            collect_tags(chapter, MetadataScope::Chapter, None, None, &mut fields);
            chapters.push(ChapterSummary {
                index,
                title: chapter
                    .get("tags")
                    .and_then(|t| t.get("title"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                start_time: as_numeric_string(chapter, "start_time"),
                end_time: as_numeric_string(chapter, "end_time"),
            });
        }
    }

    MetadataReport {
        file_name,
        file_size: format.and_then(|f| as_u64(f, "size")),
        container: format.and_then(|f| as_str(f, "format_name")),
        duration_seconds: format.and_then(|f| as_f64(f, "duration")),
        fields,
        streams,
        chapters,
    }
}

/// Parse a complete ffprobe JSON document. Split out from [`inspect`] so the
/// normalisation can be tested against fixed documents with no process involved.
pub fn parse_ffprobe_json(file_name: &str, raw: &str) -> Result<MetadataReport, String> {
    let root: Value = serde_json::from_str(raw)
        .map_err(|e| format!("Could not read the media inspection output: {e}"))?;
    if !root.is_object() {
        return Err("The media inspection output was not a JSON object.".into());
    }
    Ok(parse_report(file_name.to_string(), &root))
}

const PROBE_ARGS: [&str; 7] = [
    "-v",
    "error",
    "-print_format",
    "json",
    "-show_format",
    "-show_streams",
    "-show_chapters",
];

/// Inspect one file. Errors carry a message fit to show the user; a file that
/// cannot be inspected is reported as such and never takes the batch down.
pub fn inspect(path: &Path) -> Result<MetadataReport, String> {
    if !path.is_file() {
        return Err("File is no longer available".into());
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let output = ffprobe().args(PROBE_ARGS).arg(path).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FFPROBE_MISSING.to_string()
        } else {
            format!("Could not start FFprobe: {e}")
        }
    })?;

    if !output.status.success() {
        // stderr is the only diagnostic ffprobe offers on failure. It is surfaced
        // as an opaque message and never parsed for facts.
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("FFprobe failed")
            .to_string();
        return Err(format!("This file could not be inspected: {detail}"));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    parse_ffprobe_json(&file_name, &raw)
}

/// Checked once before a batch, so a missing probe tool is reported as one clear
/// error instead of the same failure repeated on every file.
pub fn available() -> bool {
    ffprobe_available()
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod inspect_tests;
