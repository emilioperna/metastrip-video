//! The Edit operation's model: what may be edited, how, and what a remux keeps.
//!
//! Edit is a separate operation from Clean and shares none of Clean's
//! contract. Clean drops all metadata and verifies that it is gone; Edit keeps
//! the file's metadata, changes the fields the user named, and must prove that
//! nothing else meaningful changed. Nothing here touches Clean.
//!
//! This module is pure: no FFmpeg, no file system, no Tauri. It holds
//!
//! * the request -- [`EditableField`], [`EditOperation`], [`MetadataEdit`],
//!   [`EditRequest`] -- and the one validation path that turns a request from
//!   the page into a [`ValidEditRequest`];
//! * [`EditOperation::resolve`], which decides per file what an operation does
//!   given the field's current value;
//! * [`NormalizedGlobals`], the container-level metadata of one inspected file
//!   in the form Edit compares before and after;
//! * [`EditMuxer`], the per-muxer facts about which existing keys survive an
//!   Edit remux, and [`unpreserved_keys`], which answers whether a file can be
//!   edited without losing something the user did not ask to change.
//!
//! ## Fail safe, not "declare and continue"
//!
//! FFmpeg's MP4 and MOV muxers silently drop `mdta` keys (every
//! `com.apple.quicktime.*` field, GPS included) and any key outside their
//! write lists, and the AVI muxer keeps only a handful of INFO fields. Edit
//! does not accept that loss on the user's behalf: a file whose untouched
//! metadata would not survive is to be skipped before it is written. The
//! capability facts below are therefore allowlists taken from what the pinned
//! FFmpeg was observed to round-trip, and anything not on them counts as
//! lost. Getting a key wrong in that direction skips a file that could have
//! been edited; getting it wrong the other way would publish a file that is
//! missing metadata, so every list errs toward the first.

// Parts of the model are read only by the Edit verifier, which lands in the
// next task.
#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::inspect::{MetadataReport, MetadataScope};
use crate::{FormatProfile, MAX_BATCH};

// ---------------------------------------------------------------- Request ---

/// A container-level field Edit may change. Deliberately closed: every field
/// here was observed to be written and read back unchanged by all six
/// supported containers, and nothing else may be edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditableField {
    Title,
    Artist,
    Comment,
    Copyright,
    Genre,
    Date,
}

/// How a field's value is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// One line of free text.
    Text,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, and a real calendar date.
    Date,
}

impl EditableField {
    /// Every field, in the order edits are applied and reported.
    pub const ALL: [EditableField; 6] = [
        EditableField::Title,
        EditableField::Artist,
        EditableField::Comment,
        EditableField::Copyright,
        EditableField::Genre,
        EditableField::Date,
    ];

    /// The metadata key as FFmpeg writes it and as the inspector reports it,
    /// lowercased.
    pub fn key(self) -> &'static str {
        match self {
            EditableField::Title => "title",
            EditableField::Artist => "artist",
            EditableField::Comment => "comment",
            EditableField::Copyright => "copyright",
            EditableField::Genre => "genre",
            EditableField::Date => "date",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EditableField::Title => "Title",
            EditableField::Artist => "Artist",
            EditableField::Comment => "Comment",
            EditableField::Copyright => "Copyright",
            EditableField::Genre => "Genre",
            EditableField::Date => "Date",
        }
    }

    /// `Date` is the descriptive date or year tag. It is not the recording
    /// timestamp (`creation_time`), not a file system date and carries no
    /// time zone.
    pub fn kind(self) -> ValueKind {
        match self {
            EditableField::Date => ValueKind::Date,
            _ => ValueKind::Text,
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.key() == key)
    }
}

/// What to do to one field.
///
/// Serialised as `{"kind": "set", "value": "..."}`,
/// `{"kind": "fillIfMissing", "value": "..."}` and `{"kind": "remove"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum EditOperation {
    /// The field ends up holding this value, whether it was there before or
    /// not: adding and replacing are the same operation.
    Set(String),
    /// Written only where the field is missing or empty; an existing value
    /// is kept as it is.
    FillIfMissing(String),
    /// The field ends up absent.
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataEdit {
    pub field: EditableField,
    pub operation: EditOperation,
}

/// An Edit batch as the page sends it: the queue and the edits, both fixed
/// at the click. Nothing may act on one before [`EditRequest::validate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRequest {
    pub paths: Vec<String>,
    pub edits: Vec<MetadataEdit>,
}

/// One edit per field at most, so a batch never holds more than this.
pub const MAX_EDITS: usize = EditableField::ALL.len();

/// Longest value accepted, in characters (Unicode scalar values), not bytes.
///
/// Values reach FFmpeg as command-line arguments, and Windows caps a whole
/// command line at 32,767 characters: a 40,000-character value fails to start
/// FFmpeg at all. Six values of this length stay far below that together with
/// two full paths.
pub const MAX_VALUE_CHARS: usize = 1000;

/// A request that passed validation. Only [`EditRequest::validate`] makes one,
/// so anything holding it holds trimmed, checked values, one edit per field,
/// in [`EditableField::ALL`] order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidEditRequest {
    paths: Vec<String>,
    edits: Vec<MetadataEdit>,
}

impl ValidEditRequest {
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn edits(&self) -> &[MetadataEdit] {
        &self.edits
    }
}

impl EditRequest {
    /// The only way a request becomes something that can run. The whole
    /// request is refused on the first problem: an edit the backend would not
    /// accept must not quietly apply to some files and not others.
    ///
    /// Messages name the field, never the value: a value is the user's own
    /// text and has no business in an error that may be shown or copied.
    pub fn validate(self) -> Result<ValidEditRequest, String> {
        if self.paths.is_empty() {
            return Err("No videos selected".into());
        }
        if self.paths.len() > MAX_BATCH {
            return Err(format!(
                "Too many videos: {} selected, the limit is {MAX_BATCH}.",
                self.paths.len()
            ));
        }
        if self.paths.iter().any(|path| path.trim().is_empty()) {
            return Err("A selected video has no path.".into());
        }

        if self.edits.is_empty() {
            return Err("Choose at least one field to edit.".into());
        }
        if self.edits.len() > MAX_EDITS {
            return Err(format!(
                "Too many edits: {}, the limit is {MAX_EDITS}, one per field.",
                self.edits.len()
            ));
        }

        let mut seen = BTreeSet::new();
        let mut edits = Vec::with_capacity(self.edits.len());
        for edit in self.edits {
            if !seen.insert(edit.field) {
                return Err(format!(
                    "{} is edited more than once. Choose one operation per field.",
                    edit.field.label()
                ));
            }
            let operation = match edit.operation {
                EditOperation::Set(value) => {
                    EditOperation::Set(validate_value(edit.field, &value)?)
                }
                EditOperation::FillIfMissing(value) => {
                    EditOperation::FillIfMissing(validate_value(edit.field, &value)?)
                }
                EditOperation::Remove => EditOperation::Remove,
            };
            edits.push(MetadataEdit {
                field: edit.field,
                operation,
            });
        }
        // Applied and reported in one fixed order, whatever order the page
        // listed them in, so the same request always plans the same way.
        edits.sort_by_key(|edit| edit.field);

        Ok(ValidEditRequest {
            paths: self.paths,
            edits,
        })
    }
}

/// A value as it will be written: outer whitespace trimmed first, because
/// the inspector trims what it reads back and an untrimmed value could never
/// be verified; then checked, never truncated.
pub fn validate_value(field: EditableField, raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!(
            "{} cannot be empty. Use Remove to delete it.",
            field.label()
        ));
    }
    let length = value.chars().count();
    if length > MAX_VALUE_CHARS {
        return Err(format!(
            "{} is too long: {length} characters, the limit is {MAX_VALUE_CHARS}.",
            field.label()
        ));
    }
    // Control characters (CR, LF, tab, NUL, DEL, the C1 range) and the two
    // Unicode line separators: a value is one line. Format characters such
    // as the zero-width joiner stay allowed, since emoji sequences and
    // right-to-left text need them.
    if value
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(format!(
            "{} must be a single line of text without control characters.",
            field.label()
        ));
    }
    if field.kind() == ValueKind::Date {
        validate_date(value)?;
    }
    Ok(value.to_string())
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` with ASCII digits, a year from 0001 to
/// 9999, a month from 01 to 12 and a day that exists in that month, leap
/// years included. No time, no zone, no other separator.
pub fn validate_date(value: &str) -> Result<(), String> {
    const SHAPE: &str = "Date must be a year (YYYY), a month (YYYY-MM) or a day (YYYY-MM-DD).";
    const CALENDAR: &str = "Date is not a real calendar date.";

    let parts: Vec<&str> = value.split('-').collect();
    let widths: &[usize] = match parts.len() {
        1 => &[4],
        2 => &[4, 2],
        3 => &[4, 2, 2],
        _ => return Err(SHAPE.into()),
    };
    let well_formed = parts
        .iter()
        .zip(widths)
        .all(|(part, width)| part.len() == *width && part.bytes().all(|b| b.is_ascii_digit()));
    if !well_formed {
        return Err(SHAPE.into());
    }

    // Digits only and at most four of them, so these cannot fail.
    let number = |part: &str| part.parse::<u32>().unwrap_or(0);
    let year = number(parts[0]);
    if year == 0 {
        return Err(CALENDAR.into());
    }
    let Some(month) = parts.get(1).map(|part| number(part)) else {
        return Ok(());
    };
    if !(1..=12).contains(&month) {
        return Err(CALENDAR.into());
    }
    let Some(day) = parts.get(2).map(|part| number(part)) else {
        return Ok(());
    };
    if day == 0 || day > days_in_month(year, month) {
        return Err(CALENDAR.into());
    }
    Ok(())
}

fn days_in_month(year: u32, month: u32) -> u32 {
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// ------------------------------------------------------------- Resolution ---

/// What one operation does to one file, decided from the field's value in a
/// fresh inspection of that file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldChange {
    /// Write this value.
    Write(String),
    /// Delete the field, which is present.
    Delete,
    /// Leave the field exactly as it is, present or not.
    Keep,
}

/// A value that is missing, or present but empty once trimmed. `FillIfMissing`
/// fills both.
fn effective(current: Option<&str>) -> Option<&str> {
    current.map(str::trim).filter(|value| !value.is_empty())
}

impl EditOperation {
    /// What this operation does to a field whose current value is `current`
    /// (`None` when the file has no such field).
    pub fn resolve(&self, current: Option<&str>) -> FieldChange {
        match self {
            EditOperation::Set(value) => FieldChange::Write(value.clone()),
            EditOperation::FillIfMissing(value) => match effective(current) {
                Some(_) => FieldChange::Keep,
                None => FieldChange::Write(value.clone()),
            },
            EditOperation::Remove => match current {
                Some(_) => FieldChange::Delete,
                None => FieldChange::Keep,
            },
        }
    }

    /// The value the output must carry for this field, `None` meaning the
    /// field must be absent. Compared against the output's normalised
    /// metadata, so the value is trimmed as the inspector trims.
    pub fn expected_value(&self, current: Option<&str>) -> Option<String> {
        match self.resolve(current) {
            FieldChange::Write(value) => Some(value),
            FieldChange::Delete => None,
            FieldChange::Keep => current.map(|value| value.trim().to_string()),
        }
    }
}

// ----------------------------------------------------------- Capabilities ---

/// The muxer an Edit writes with, which is what decides what survives. MP4
/// and M4V share one; MOV has its own and keeps a different set of keys, so
/// the container family alone is not enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMuxer {
    Mp4,
    Mov,
    Matroska,
    WebM,
    Avi,
}

/// The muxer behind a supported format profile. `None` for a profile this
/// module has no facts for, which a planner must treat as not editable.
pub fn edit_muxer(profile: FormatProfile) -> Option<EditMuxer> {
    match profile.output_muxer {
        "mp4" => Some(EditMuxer::Mp4),
        "mov" => Some(EditMuxer::Mov),
        "matroska" => Some(EditMuxer::Matroska),
        "webm" => Some(EditMuxer::WebM),
        "avi" => Some(EditMuxer::Avi),
        _ => None,
    }
}

/// Container-level keys each muxer writes itself on every remux, whatever
/// the input said. They are outside the Edit contract: comparing them would
/// fail every edit for a change nobody asked for and nobody can prevent.
///
/// * ISO-BMFF: the brand fields are regenerated (an `M4V ` brand comes out as
///   `isom`) and `encoder` is dropped under `-fflags +bitexact`.
/// * Matroska and WebM: `encoder` is rewritten to the bare `Lavf`.
/// * AVI: `software` (the INFO `ISFT` chunk) is dropped under bitexact.
///
/// `encoder` and `software` are privacy findings (Software, LOW) in the scan,
/// and they stay findings there: this list only says Edit does not promise to
/// keep them.
const ISO_MUXER_OWNED: &[&str] = &[
    "compatible_brands",
    "encoder",
    "major_brand",
    "minor_version",
];
const MATROSKA_MUXER_OWNED: &[&str] = &["encoder"];
const AVI_MUXER_OWNED: &[&str] = &["software"];

/// Keys the MP4 muxer writes and the demuxer reads back under the same name,
/// as observed with the pinned FFmpeg. Everything else -- `make`, `model`,
/// every `com.apple.quicktime.*` key, any custom key -- is dropped silently.
const MP4_PRESERVED: &[&str] = &[
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
];

/// The same for the MOV muxer, which writes QuickTime user data instead of
/// an iTunes list: it keeps `make` and `model`, which MP4 drops, and drops
/// `description`, `composer` and the other iTunes-only keys, which MP4 keeps.
const MOV_PRESERVED: &[&str] = &[
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
];

/// The INFO fields the AVI muxer writes back under the same name. `album` is
/// not among them: it is written as `IPRD`, read back as `product`, and
/// `product` is then dropped by the next remux.
const AVI_PRESERVED: &[&str] = &[
    "artist",
    "comment",
    "copyright",
    "date",
    "encoded_by",
    "genre",
    "language",
    "title",
    "track",
];

impl EditMuxer {
    /// Container-level keys this muxer regenerates; see [`ISO_MUXER_OWNED`].
    pub fn muxer_owned_keys(self) -> &'static [&'static str] {
        match self {
            EditMuxer::Mp4 | EditMuxer::Mov => ISO_MUXER_OWNED,
            EditMuxer::Matroska | EditMuxer::WebM => MATROSKA_MUXER_OWNED,
            EditMuxer::Avi => AVI_MUXER_OWNED,
        }
    }

    pub fn is_muxer_owned(self, key: &str) -> bool {
        self.muxer_owned_keys().contains(&key)
    }

    /// Whether an existing, meaningful container-level key -- normalised, so
    /// lowercase and not muxer-owned -- is expected to come out of an Edit
    /// remux with the same name and value.
    ///
    /// Matroska and WebM keep any key, uppercased on disk, which the
    /// lowercase comparison absorbs. They do rewrite some characters (a space
    /// becomes `_`), so only keys made of the characters seen to survive --
    /// ASCII lowercase letters, digits and `_` -- are claimed; anything else
    /// is treated as not preserved rather than guessed at.
    pub fn preserves_global_key(self, key: &str) -> bool {
        match self {
            EditMuxer::Mp4 => MP4_PRESERVED.contains(&key),
            EditMuxer::Mov => MOV_PRESERVED.contains(&key),
            EditMuxer::Avi => AVI_PRESERVED.contains(&key),
            EditMuxer::Matroska | EditMuxer::WebM => {
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }
        }
    }

    /// Whether this muxer can write and read back an editable field. True for
    /// every field and every muxer: that is what made them the editable set.
    pub fn supports_field(self, field: EditableField) -> bool {
        self.preserves_global_key(field.key())
    }
}

// ---------------------------------------------------------- Normalisation ---

/// One file's container-level metadata in the form Edit compares: lowercase
/// keys, trimmed values, one entry per effective field, muxer-owned keys left
/// out. Built from the inspector's report, never from raw ffprobe output.
///
/// Every other key is kept, privacy fields included: normalisation decides
/// what counts as the same field, not what matters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NormalizedGlobals {
    fields: BTreeMap<String, String>,
    /// Keys the report carried more than once with different values. Which
    /// one a remux would keep is not known, so a planner must not claim
    /// either survives.
    ambiguous: BTreeSet<String>,
}

/// ISO-BMFF stores a location once, in `©xyz`, and the demuxer reports it
/// twice: as `location` and again as `location-<language>`. Deleting
/// `location` deletes both; deleting only the alias does nothing.
const ISO_LANGUAGE_ALIASED: &[&str] = &["location"];

/// `location-eng` names `location` when the suffix is a three-letter
/// language code.
fn iso_alias_base(key: &str) -> Option<&'static str> {
    ISO_LANGUAGE_ALIASED.iter().copied().find(|base| {
        key.strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|lang| lang.len() == 3 && lang.bytes().all(|b| b.is_ascii_lowercase()))
    })
}

impl NormalizedGlobals {
    /// Normalise one report's container-level fields for the muxer the file
    /// is written with.
    pub fn from_report(report: &MetadataReport, muxer: EditMuxer) -> Self {
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();

        for field in report
            .fields
            .iter()
            .filter(|field| field.scope == MetadataScope::Format)
        {
            // The inspector already lowercases and trims; doing it again here
            // keeps this correct on its own terms.
            let key = field.key.trim().to_ascii_lowercase();
            if muxer.is_muxer_owned(&key) {
                continue;
            }
            let value = field.value.trim().to_string();
            match fields.get(&key) {
                Some(existing) if *existing != value => {
                    ambiguous.insert(key);
                }
                Some(_) => {}
                None => {
                    fields.insert(key, value);
                }
            }
        }

        if matches!(muxer, EditMuxer::Mp4 | EditMuxer::Mov) {
            // Only an alias that agrees with its base is the same field. One
            // that disagrees is a second fact and stays.
            let aliases: Vec<String> = fields
                .iter()
                .filter(|(key, value)| {
                    iso_alias_base(key)
                        .and_then(|base| fields.get(base))
                        .is_some_and(|base_value| base_value == *value)
                })
                .map(|(key, _)| key.clone())
                .collect();
            for alias in aliases {
                fields.remove(&alias);
            }
        }

        NormalizedGlobals { fields, ambiguous }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn fields(&self) -> &BTreeMap<String, String> {
        &self.fields
    }

    pub fn ambiguous(&self) -> &BTreeSet<String> {
        &self.ambiguous
    }

    /// The value an editable field has now, as [`EditOperation::resolve`]
    /// takes it.
    pub fn current(&self, field: EditableField) -> Option<&str> {
        self.get(field.key())
    }
}

/// The fields this file's edits actually change: those whose operation does
/// not resolve to [`FieldChange::Keep`] against the field's current value.
///
/// Being named in the request is not enough. `FillIfMissing` on a field that
/// already holds a value keeps it, so that field is untouched and must
/// survive the remux like any other.
pub fn changed_fields(
    globals: &NormalizedGlobals,
    edits: &[MetadataEdit],
) -> BTreeSet<EditableField> {
    edits
        .iter()
        .filter(|edit| edit.operation.resolve(globals.current(edit.field)) != FieldChange::Keep)
        .map(|edit| edit.field)
        .collect()
}

/// Untouched, meaningful container-level keys an Edit remux of this file
/// would lose or cannot be trusted to keep: every normalised key the muxer is
/// not known to preserve, plus every ambiguous key, minus the fields the edits
/// actually change (see [`changed_fields`]). A non-empty answer means the file
/// cannot be edited without losing something the user did not ask to change.
pub fn unpreserved_keys(
    globals: &NormalizedGlobals,
    muxer: EditMuxer,
    edits: &[MetadataEdit],
) -> BTreeSet<String> {
    let changed: BTreeSet<&str> = changed_fields(globals, edits)
        .into_iter()
        .map(EditableField::key)
        .collect();
    globals
        .fields
        .keys()
        .filter(|key| !muxer.preserves_global_key(key))
        .chain(globals.ambiguous.iter())
        .filter(|key| !changed.contains(key.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod edit_tests;
