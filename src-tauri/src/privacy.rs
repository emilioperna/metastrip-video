//! Deterministic privacy classification.
//!
//! No machine learning, no network, no heuristics that vary between runs: the
//! same file always produces the same findings. Everything lives in one rule
//! table so the product's idea of "sensitive" is reviewable in one place rather
//! than scattered across `contains()` calls.
//!
//! Matching is layered instead of plain substring search, because substrings
//! misfire in both directions: `model` appears inside `modification_time`, and a
//! namespaced key like `com.apple.quicktime.make` never equals `make`. Rules are
//! tried most specific first — exact key, then namespace prefix, then the last
//! dotted segment, then substring as a deliberate last resort.

use serde::Serialize;

use crate::inspect::{MetadataField, MetadataReport, MetadataScope, StreamKind, StreamSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Low => "LOW",
            Severity::Medium => "MEDIUM",
            Severity::High => "HIGH",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrivacyCategory {
    Location,
    Device,
    Timestamp,
    Software,
    CreatorIdentity,
    Telemetry,
    Identifier,
    Copyright,
    Structural,
    Unknown,
}

impl PrivacyCategory {
    pub fn label(self) -> &'static str {
        match self {
            PrivacyCategory::Location => "Location",
            PrivacyCategory::Device => "Device",
            PrivacyCategory::Timestamp => "Timestamp",
            PrivacyCategory::Software => "Software",
            PrivacyCategory::CreatorIdentity => "Creator",
            PrivacyCategory::Telemetry => "Telemetry",
            PrivacyCategory::Identifier => "Identifier",
            PrivacyCategory::Copyright => "Copyright",
            PrivacyCategory::Structural => "Structural",
            PrivacyCategory::Unknown => "Unknown",
        }
    }

    /// Categories that describe the container rather than the person or the
    /// place. The verifier tolerates these surviving, because a muxer has to
    /// write some of them back; everything else must be gone.
    ///
    /// Structural findings are technical metadata, not privacy findings: they
    /// stay in the finding list so the user can see them, but they never count
    /// toward a privacy total or a HIGH/MEDIUM/LOW summary.
    pub fn is_structural(self) -> bool {
        self == PrivacyCategory::Structural
    }
}

/// How a rule recognises a key. Ordered by specificity; see the module comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyMatch {
    /// The whole normalised key, e.g. `creation_time`.
    Exact(&'static str),
    /// A namespace, e.g. `com.apple.quicktime.location` covers `.iso6709`,
    /// `.accuracy.horizontal`, `.name` and the rest of that family.
    Prefix(&'static str),
    /// The final dotted segment, so `com.android.version` matches a `version`
    /// rule without `version` matching `driver_version`.
    Suffix(&'static str),
    /// Last resort, for vendor keys with no stable shape.
    Contains(&'static str),
}

impl KeyMatch {
    fn specificity(self) -> u8 {
        match self {
            KeyMatch::Exact(_) => 0,
            KeyMatch::Prefix(_) => 1,
            KeyMatch::Suffix(_) => 2,
            KeyMatch::Contains(_) => 3,
        }
    }

    fn matches(self, key: &str) -> bool {
        match self {
            KeyMatch::Exact(want) => key == want,
            KeyMatch::Prefix(want) => key == want || key.starts_with(&format!("{want}.")),
            KeyMatch::Suffix(want) => key.rsplit('.').next().is_some_and(|last| last == want),
            KeyMatch::Contains(want) => key.contains(want),
        }
    }
}

struct Rule {
    key: KeyMatch,
    category: PrivacyCategory,
    severity: Severity,
}

/// Explanations are static strings keyed by category: the same category always
/// explains itself the same way, so nothing user-specific can leak into the copy
/// and nothing has to be generated at runtime.
pub fn explanation(category: PrivacyCategory) -> &'static str {
    match category {
        PrivacyCategory::Location => "This metadata may reveal where the media was recorded.",
        PrivacyCategory::Device => "This field identifies the recording device.",
        PrivacyCategory::Timestamp => "This field records when the media was created or changed.",
        PrivacyCategory::Software => "This field may reveal which software processed the file.",
        PrivacyCategory::CreatorIdentity => "This field names a person credited with the media.",
        PrivacyCategory::Telemetry => {
            "This track carries sensor data recorded alongside the video, which can include a GPS trace."
        }
        PrivacyCategory::Identifier => {
            "This field carries an identifier that can link this file to a device or to other files."
        }
        PrivacyCategory::Copyright => "This field carries ownership or licensing text.",
        PrivacyCategory::Structural => {
            "These fields describe technical properties of the file and are not usually privacy-sensitive."
        }
        PrivacyCategory::Unknown => {
            "This field is not recognised, so its contents cannot be described in advance."
        }
    }
}

/// Free-text fields get their own explanation: they are recognised, but what is
/// inside them is not predictable.
const FREE_TEXT_EXPLANATION: &str =
    "This free-text field may contain anything the camera, the editing software or a person wrote.";

/// Keys whose contents are arbitrary prose. Classified as `Unknown` because the
/// category list has no "user content" bucket, but at MEDIUM rather than the LOW
/// an unrecognised key gets: a comment field is a common place for a name, a
/// place or a note to end up.
const FREE_TEXT_KEYS: [&str; 7] = [
    "title",
    "comment",
    "description",
    "synopsis",
    "keywords",
    "lyrics",
    "notes",
];

/// The rule table. Order does not matter — the most specific match wins — so
/// rules can be grouped by category for review.
const RULES: &[Rule] = &[
    // ---- Location -------------------------------------------------------
    Rule {
        key: KeyMatch::Exact("location"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Exact("location-eng"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Exact("coordinates"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.location"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Suffix("gpslatitude"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Suffix("gpslongitude"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Suffix("gpsaltitude"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("gps"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("geotag"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("latitude"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("longitude"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("iso6709"),
        category: PrivacyCategory::Location,
        severity: Severity::High,
    },
    // ---- Telemetry ------------------------------------------------------
    Rule {
        key: KeyMatch::Contains("telemetry"),
        category: PrivacyCategory::Telemetry,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("gyro"),
        category: PrivacyCategory::Telemetry,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Contains("accelerometer"),
        category: PrivacyCategory::Telemetry,
        severity: Severity::High,
    },
    // ---- Device ---------------------------------------------------------
    Rule {
        key: KeyMatch::Exact("make"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("model"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("device"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.make"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.model"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.camera"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.android.manufacturer"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.android.model"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Suffix("manufacturer"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Suffix("firmware"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Contains("lens"),
        category: PrivacyCategory::Device,
        severity: Severity::Medium,
    },
    // ---- Timestamp ------------------------------------------------------
    Rule {
        key: KeyMatch::Exact("creation_time"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("date"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("date-eng"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("modification_time"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.creationdate"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Suffix("datetimeoriginal"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Suffix("creationdate"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Medium,
    },
    // Timecode often encodes wall-clock time of day on camera originals.
    Rule {
        key: KeyMatch::Exact("timecode"),
        category: PrivacyCategory::Timestamp,
        severity: Severity::Low,
    },
    // ---- Software -------------------------------------------------------
    Rule {
        key: KeyMatch::Exact("encoder"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("encoded_by"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("software"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("writing_application"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("muxing_app"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.software"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Suffix("application"),
        category: PrivacyCategory::Software,
        severity: Severity::Low,
    },
    // ---- Creator identity ----------------------------------------------
    Rule {
        key: KeyMatch::Exact("artist"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("author"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("creator"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("album_artist"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("composer"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("performer"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("director"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("producer"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.author"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.artist"),
        category: PrivacyCategory::CreatorIdentity,
        severity: Severity::Medium,
    },
    // ---- Identifier -----------------------------------------------------
    Rule {
        key: KeyMatch::Exact("serial"),
        category: PrivacyCategory::Identifier,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Exact("serial_number"),
        category: PrivacyCategory::Identifier,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Suffix("serialnumber"),
        category: PrivacyCategory::Identifier,
        severity: Severity::High,
    },
    Rule {
        key: KeyMatch::Exact("host_computer"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("uid"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("unique_id"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("guid"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Exact("uuid"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.content.identifier"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    Rule {
        key: KeyMatch::Suffix("identifier"),
        category: PrivacyCategory::Identifier,
        severity: Severity::Medium,
    },
    // ---- Copyright ------------------------------------------------------
    Rule {
        key: KeyMatch::Exact("copyright"),
        category: PrivacyCategory::Copyright,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("license"),
        category: PrivacyCategory::Copyright,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("rights"),
        category: PrivacyCategory::Copyright,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Prefix("com.apple.quicktime.copyright"),
        category: PrivacyCategory::Copyright,
        severity: Severity::Low,
    },
    // ---- Structural -----------------------------------------------------
    Rule {
        key: KeyMatch::Exact("major_brand"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("minor_version"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("compatible_brands"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("handler_name"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("language"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("track"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    Rule {
        key: KeyMatch::Exact("disc"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    // Written by the muxer into the sample description, and it names the muxer
    // (`FFMP`, `[0][0][0][0]`), not the camera. `-map_metadata -1` cannot remove
    // it because the muxer writes it again on the way out.
    Rule {
        key: KeyMatch::Exact("vendor_id"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
    // Matroska and WebM carry a per-stream DURATION tag that the muxer regenerates.
    Rule {
        key: KeyMatch::Exact("duration"),
        category: PrivacyCategory::Structural,
        severity: Severity::Low,
    },
];

/// Codec tags of tracks that exist to carry sensor data. These are the payloads
/// `-dn` drops; without the drop, stripping tags alone would leave a full GPS
/// trace inside the file.
const TELEMETRY_CODEC_TAGS: [&str; 4] = ["gpmd", "mebx", "fdsc", "rtmd"];

/// Handler names that announce a telemetry track even when the codec tag is
/// generic. Matched case-insensitively as a prefix of the handler.
const TELEMETRY_HANDLERS: [&str; 3] = ["gopro met", "gopro tcd", "meta"];

/// A value shaped like ISO 6709 (`+45.4642+009.1900/`), which is how ISO-BMFF
/// stores a fix. Checked in addition to the key rules so coordinates hiding in a
/// generically-named field are still caught.
fn looks_like_iso6709(value: &str) -> bool {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.len() < 7 || !trimmed.starts_with(['+', '-']) {
        return false;
    }
    // At least two signed numeric groups, and nothing outside the grammar.
    let signs = trimmed.matches(['+', '-']).count();
    let valid = trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.'));
    signs >= 2 && valid
}

fn classify_key(key: &str) -> Option<(PrivacyCategory, Severity)> {
    RULES
        .iter()
        .filter(|rule| rule.key.matches(key))
        .min_by_key(|rule| rule.key.specificity())
        .map(|rule| (rule.category, rule.severity))
}

/// The classification of a single field, before it becomes a finding.
pub fn classify_field(field: &MetadataField) -> (PrivacyCategory, Severity, &'static str) {
    if looks_like_iso6709(&field.value) {
        return (
            PrivacyCategory::Location,
            Severity::High,
            explanation(PrivacyCategory::Location),
        );
    }
    if FREE_TEXT_KEYS.contains(&field.key.as_str()) {
        return (
            PrivacyCategory::Unknown,
            Severity::Medium,
            FREE_TEXT_EXPLANATION,
        );
    }
    match classify_key(&field.key) {
        Some((category, severity)) => (category, severity, explanation(category)),
        // An unrecognised key is never assumed to be the worst case. It is still
        // removed by the cleaner; it is simply not described as high risk.
        None => (
            PrivacyCategory::Unknown,
            Severity::Low,
            explanation(PrivacyCategory::Unknown),
        ),
    }
}

fn classify_stream(stream: &StreamSummary) -> Option<(PrivacyCategory, Severity, String)> {
    if stream.kind.is_media() {
        return None;
    }
    let tag = stream
        .identity
        .codec_tag
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let handler = stream
        .handler_name
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();

    let telemetry = TELEMETRY_CODEC_TAGS.contains(&tag.as_str())
        || TELEMETRY_HANDLERS.iter().any(|h| handler.starts_with(h));

    if telemetry {
        return Some((
            PrivacyCategory::Telemetry,
            Severity::High,
            format!(
                "{} track carrying sensor data ({})",
                stream.kind.label(),
                stream
                    .identity
                    .codec_tag
                    .as_deref()
                    .unwrap_or("unknown format")
            ),
        ));
    }
    Some((
        PrivacyCategory::Unknown,
        Severity::Medium,
        format!(
            "{} track that is not video or audio ({})",
            stream.kind.label(),
            stream
                .identity
                .codec_name
                .as_deref()
                .or(stream.identity.codec_tag.as_deref())
                .unwrap_or("unknown format")
        ),
    ))
}

/// What the file discloses, in one entry per disclosure.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyFinding {
    pub category: PrivacyCategory,
    pub category_label: &'static str,
    pub severity: Severity,
    pub severity_label: &'static str,
    /// What was found, already shortened for display.
    pub detail: String,
    pub explanation: &'static str,
    pub scope: MetadataScope,
    pub scope_label: &'static str,
    pub stream_index: Option<usize>,
    /// Normalised key, or a synthetic one for stream and chapter findings. Used
    /// by the verifier to tell whether a specific disclosure survived.
    pub source_key: String,
}

/// Values are truncated before they ever reach the UI. Nothing is sent anywhere,
/// but a metadata field can hold kilobytes and a row has one line.
fn preview(value: &str) -> String {
    const LIMIT: usize = 120;
    let cleaned = value.replace(['\n', '\r', '\t'], " ");
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= LIMIT {
        return cleaned.to_string();
    }
    let short: String = cleaned.chars().take(LIMIT).collect();
    format!("{short}…")
}

fn finding_from_field(field: &MetadataField) -> PrivacyFinding {
    let (category, severity, explanation) = classify_field(field);
    let detail = if field.value.is_empty() {
        field.display_key.clone()
    } else {
        format!("{}: {}", field.display_key, preview(&field.value))
    };
    PrivacyFinding {
        category,
        category_label: category.label(),
        severity,
        severity_label: severity.label(),
        detail,
        explanation,
        scope: field.scope,
        scope_label: field.scope.label(),
        stream_index: field.stream_index,
        source_key: field.key.clone(),
    }
}

/// Classify a whole report: every metadata field, every non-media track, and the
/// chapter list as a single entry.
pub fn classify(report: &MetadataReport) -> Vec<PrivacyFinding> {
    let mut findings: Vec<PrivacyFinding> = report.fields.iter().map(finding_from_field).collect();

    for stream in report.non_media_streams() {
        if let Some((category, severity, detail)) = classify_stream(stream) {
            findings.push(PrivacyFinding {
                category,
                category_label: category.label(),
                severity,
                severity_label: severity.label(),
                detail,
                explanation: if category == PrivacyCategory::Telemetry {
                    explanation(PrivacyCategory::Telemetry)
                } else {
                    "This track is not video or audio. Its contents cannot be described in advance and it is removed whole."
                },
                scope: MetadataScope::Stream,
                scope_label: MetadataScope::Stream.label(),
                stream_index: Some(stream.index),
                source_key: format!("stream:{}", stream.index),
            });
        }
    }

    if !report.chapters.is_empty() {
        let count = report.chapters.len();
        findings.push(PrivacyFinding {
            category: PrivacyCategory::Structural,
            category_label: PrivacyCategory::Structural.label(),
            severity: Severity::Low,
            severity_label: Severity::Low.label(),
            detail: format!(
                "{count} chapter {}",
                if count == 1 { "marker" } else { "markers" }
            ),
            // Chapter markers are Structural, so they share its description; a
            // group's explanation must not depend on which member sorts first.
            explanation: explanation(PrivacyCategory::Structural),
            scope: MetadataScope::Chapter,
            scope_label: MetadataScope::Chapter.label(),
            stream_index: None,
            source_key: "chapters".into(),
        });
    }

    // Highest severity first, then by category, so the row summary and the
    // expanded list always agree on what matters most.
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.category_label.cmp(b.category_label))
            .then_with(|| a.source_key.cmp(&b.source_key))
    });
    findings
}

/// True when a metadata field is technical (structural) rather than a privacy
/// disclosure. Used by the verifier to report the two kinds of removal apart.
pub fn is_technical_field(field: &MetadataField) -> bool {
    classify_field(field).0.is_structural()
}

/// Counts for the queue row and the batch header, so the UI never has to walk
/// thousands of findings to render a summary.
///
/// `total`, `high`, `medium` and `low` count privacy findings only. Structural
/// findings are counted in `technical` and nowhere else, so container
/// bookkeeping such as `major_brand` or `handler_name` cannot inflate the
/// privacy picture.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacySummary {
    pub total: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub technical: usize,
}

impl PrivacySummary {
    pub fn of(findings: &[PrivacyFinding]) -> Self {
        let mut summary = PrivacySummary::default();
        for finding in findings {
            if finding.category.is_structural() {
                summary.technical += 1;
                continue;
            }
            summary.total += 1;
            match finding.severity {
                Severity::High => summary.high += 1,
                Severity::Medium => summary.medium += 1,
                Severity::Low => summary.low += 1,
            }
        }
        summary
    }
}

/// How many streams of each kind a file carries, as the scan reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamCounts {
    /// Footage: video streams not marked as attached cover art.
    pub video: usize,
    pub audio: usize,
    pub subtitle: usize,
    /// Streams the container marks as attached cover art, and only those.
    pub cover_art: usize,
    /// Data, attachment and unknown tracks.
    pub other: usize,
}

/// Which streams a container actually exposed. Kept here rather than in the UI
/// so the counts have one definition.
pub fn stream_counts(report: &MetadataReport) -> StreamCounts {
    let count = |keep: &dyn Fn(&crate::inspect::StreamSummary) -> bool| {
        report.streams.iter().filter(|s| keep(s)).count()
    };
    StreamCounts {
        video: count(&|s| s.kind == StreamKind::Video && !s.attached_pic),
        audio: count(&|s| s.kind == StreamKind::Audio),
        subtitle: count(&|s| s.kind == StreamKind::Subtitle),
        cover_art: count(&|s| s.attached_pic),
        other: report.non_media_streams().count(),
    }
}

#[cfg(test)]
#[path = "privacy_tests.rs"]
mod privacy_tests;
