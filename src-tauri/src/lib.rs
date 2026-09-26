use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

mod edit;
mod edit_batch;
mod edit_plan;
mod inspect;
mod plan;
mod privacy;
mod sidecar;
mod verify;

#[cfg(test)]
mod testkit;

#[cfg(test)]
#[path = "bench_tests.rs"]
mod bench_tests;

#[cfg(test)]
#[path = "offthread_tests.rs"]
mod offthread_tests;

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

use edit::EditRequest;
use edit_batch::{edit_with, EditSummary};
use plan::CleaningOptions;
use privacy::{PrivacyFinding, PrivacySummary};
use sidecar::{ffmpeg, ffmpeg_available, FFMPEG_MISSING, FFPROBE_MISSING};
use verify::{OriginalFingerprint, VerificationReport};

// Functional defaults. Deliberately brand-neutral: nothing here encodes who ships
// the app, so a fork can keep every value as-is. Product identity lives in
// `tauri.conf.json` (product name, identifier, publisher) and nowhere else.

/// Starting file-name prefix. The user can change it and the choice is persisted.
const DEFAULT_PREFIX: &str = "VIDEO";
/// Marks an output that FFmpeg is still writing. Renamed into place on success.
const TEMP_PREFIX: &str = ".video-cleaner-processing-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainerProfile {
    IsoBmff,
    Matroska,
    WebM,
    Avi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MuxAttempt {
    FastStart,
    Standard,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatProfile {
    extension: &'static str,
    label: &'static str,
    pub(crate) container: ContainerProfile,
    /// Explicit rather than inferred from the temporary file name. In particular,
    /// FFmpeg maps `.m4v` to its raw MPEG-4 video muxer unless we select MP4 here.
    output_muxer: &'static str,
}

const ISO_BMFF_ATTEMPTS: [MuxAttempt; 2] = [MuxAttempt::FastStart, MuxAttempt::Standard];
const STANDARD_ATTEMPTS: [MuxAttempt; 1] = [MuxAttempt::Standard];

impl FormatProfile {
    fn mux_attempts(self) -> &'static [MuxAttempt] {
        match self.container {
            ContainerProfile::IsoBmff => &ISO_BMFF_ATTEMPTS,
            ContainerProfile::Matroska | ContainerProfile::WebM | ContainerProfile::Avi => {
                &STANDARD_ATTEMPTS
            }
        }
    }
}

/// The product support contract. FFmpeg may understand many more extensions, but
/// only profiles represented here are accepted by either side of the application.
const FORMAT_PROFILES: [FormatProfile; 6] = [
    FormatProfile {
        extension: "mp4",
        label: "MP4",
        container: ContainerProfile::IsoBmff,
        output_muxer: "mp4",
    },
    FormatProfile {
        extension: "mov",
        label: "MOV",
        container: ContainerProfile::IsoBmff,
        output_muxer: "mov",
    },
    FormatProfile {
        extension: "m4v",
        label: "M4V",
        container: ContainerProfile::IsoBmff,
        output_muxer: "mp4",
    },
    FormatProfile {
        extension: "mkv",
        label: "MKV",
        container: ContainerProfile::Matroska,
        output_muxer: "matroska",
    },
    FormatProfile {
        extension: "webm",
        label: "WebM",
        container: ContainerProfile::WebM,
        output_muxer: "webm",
    },
    FormatProfile {
        extension: "avi",
        label: "AVI",
        container: ContainerProfile::Avi,
        output_muxer: "avi",
    },
];

fn format_profile(extension: &str) -> Option<FormatProfile> {
    FORMAT_PROFILES
        .iter()
        .copied()
        .find(|profile| profile.extension == extension)
}

fn supported_format_labels() -> String {
    FORMAT_PROFILES
        .iter()
        .map(|profile| profile.label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `.m4v` is ambiguous in FFmpeg: it may be an ISO-BMFF file or a raw MPEG-4
/// elementary stream. Tier 1 deliberately covers the former only. Walk the first
/// top-level boxes instead of looking for an arbitrary `ftyp` byte sequence inside
/// media payload.
fn is_iso_bmff(path: &Path) -> Result<bool, String> {
    const MAX_HEADER_SCAN: u64 = 1024 * 1024;

    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Could not inspect the M4V container: {e}"))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("Could not inspect the M4V container: {e}"))?
        .len();
    let scan_limit = file_len.min(MAX_HEADER_SCAN);
    let mut offset = 0u64;

    while offset + 8 <= scan_limit {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Could not inspect the M4V container: {e}"))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)
            .map_err(|e| format!("Could not inspect the M4V container: {e}"))?;

        let size32 = u32::from_be_bytes(header[0..4].try_into().unwrap()) as u64;
        let box_type = &header[4..8];
        let (box_size, header_size) = if size32 == 1 {
            if offset + 16 > scan_limit {
                return Ok(false);
            }
            let mut extended = [0u8; 8];
            file.read_exact(&mut extended)
                .map_err(|e| format!("Could not inspect the M4V container: {e}"))?;
            (u64::from_be_bytes(extended), 16u64)
        } else if size32 == 0 {
            (file_len - offset, 8u64)
        } else {
            (size32, 8u64)
        };

        let Some(next_offset) = offset.checked_add(box_size) else {
            return Ok(false);
        };
        if box_size < header_size || next_offset > file_len {
            return Ok(false);
        }
        if box_type == b"ftyp" {
            return Ok(true);
        }
        if size32 == 0 || next_offset > scan_limit {
            return Ok(false);
        }
        offset = next_offset;
    }

    Ok(false)
}

const MAX_PREFIX_LEN: usize = 64;
const MAX_BATCH: usize = 100;

// -------------------------------------------------------------- Settings ---

/// What `settings.json` holds.
///
/// Every v0.5.0 install already has one of these on disk, written before
/// `cleaning` existed, and an update must not cost anyone their prefix or
/// output folder. Two things make sure of it:
///
/// * `default` on the struct: a key that is missing takes its default instead
///   of failing the whole document.
/// * `lenient` on `cleaning`: a value this version cannot read -- `null`, a
///   string, a number, an object of the wrong shape, one written by some later
///   version -- becomes the default options instead of failing the whole
///   document, which would throw away the prefix and folder beside it.
///
/// Unknown keys are ignored, so a downgrade reads what it understands.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    prefix: String,
    /// Empty until the user picks one.
    output_directory: String,
    /// The user's standing choice. A batch never reads it from here: the page
    /// sends the options with the Clean request, so what runs is what the user
    /// saw when they clicked, not whatever the file says by then.
    #[serde(deserialize_with = "lenient")]
    cleaning: CleaningOptions,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            prefix: DEFAULT_PREFIX.to_string(),
            output_directory: String::new(),
            cleaning: CleaningOptions::default(),
        }
    }
}

/// Reads a value, or its default when the value is there but unusable.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// What the UI needs: the stored values plus whether the folder is still usable.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    prefix: String,
    output_directory: String,
    output_directory_valid: bool,
    cleaning: CleaningOptions,
}

impl From<Settings> for SettingsView {
    fn from(s: Settings) -> Self {
        let valid = !s.output_directory.is_empty() && Path::new(&s.output_directory).is_dir();
        SettingsView {
            prefix: s.prefix,
            output_directory: s.output_directory,
            output_directory_valid: valid,
            cleaning: s.cleaning,
        }
    }
}

fn settings_path(app_dir: &Path) -> PathBuf {
    app_dir.join("settings.json")
}

/// Never fails: a missing or corrupt file just means "use the defaults", which is
/// better than refusing to start over a preferences file.
fn load_settings(app_dir: &Path) -> Settings {
    std::fs::read_to_string(settings_path(app_dir))
        .ok()
        .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
        .map(|mut s| {
            s.prefix = sanitize_prefix(&s.prefix).unwrap_or_else(|_| DEFAULT_PREFIX.to_string());
            s
        })
        .unwrap_or_default()
}

/// Written next to `settings.json` and renamed over it once complete.
const SETTINGS_TEMP: &str = "settings.json.tmp";

/// Writes the whole file to a temporary sibling, flushes it to disk, then
/// renames it over the real one. A crash part-way leaves the previous
/// `settings.json` intact rather than a truncated one, which `load_settings`
/// would read as corrupt and replace with defaults. On Windows the rename
/// replaces the existing file in one step.
fn write_settings(app_dir: &Path, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(app_dir)
        .map_err(|e| format!("Could not create the settings folder: {e}"))?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let temp = app_dir.join(SETTINGS_TEMP);
    let written = std::fs::File::create(&temp)
        .and_then(|mut file| {
            file.write_all(json.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temp, settings_path(app_dir)));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Could not save settings: {e}")
    })
}

/// Held for each read-modify-write of `settings.json`, so two saves in quick
/// succession cannot each start from the same old file and lose the other's
/// change.
static SETTINGS_WRITE: Mutex<()> = Mutex::new(());

/// Change the stored settings through `edit`, keeping everything it does not
/// touch. Every save goes through here: rebuilding the file from only the
/// fields one command knows about is how a new setting gets silently erased.
fn update_settings(app_dir: &Path, edit: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    let _guard = SETTINGS_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut settings = load_settings(app_dir);
    edit(&mut settings);
    write_settings(app_dir, &settings)?;
    Ok(settings)
}

/// Keeps the prefix to something that can only ever be part of a file name:
/// no separators, no drive letters, no `..`.
fn sanitize_prefix(raw: &str) -> Result<String, String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '.'
            )
        })
        .filter(|c| !c.is_control())
        .take(MAX_PREFIX_LEN)
        .collect();
    let cleaned = cleaned.trim().to_string();

    if cleaned.is_empty() {
        return Err("The file name prefix cannot be empty.".into());
    }
    Ok(cleaned)
}

// ------------------------------------------------------------ ID registry ---

/// Append-only list of every ID ever handed out, so a name is never reused even
/// after old outputs are moved or deleted.
struct IdRegistry {
    file: std::fs::File,
    used: HashSet<u64>,
}

const ID_MODULO: u64 = 10_000_000_000; // exactly 10 digits, leading zeros kept

fn format_id(id: u64) -> String {
    format!("{id:010}")
}

impl IdRegistry {
    fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create the app data folder: {e}"))?;
        }
        let used = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .filter(|id| *id < ID_MODULO)
            .collect();

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Could not open the ID registry: {e}"))?;

        Ok(IdRegistry { file, used })
    }

    /// Two guards: the registry of everything ever handed out, and the output
    /// folder itself, which may already hold that name from somewhere else.
    fn is_free(&self, output_dir: &Path, prefix: &str, ext: &str, id: u64) -> bool {
        !self.used.contains(&id)
            && !output_dir
                .join(format!("{prefix}_{}.{ext}", format_id(id)))
                .exists()
    }

    /// Reserves an ID and persists it *before* returning, so a crash mid-batch can
    /// never hand the same number out twice. A failed conversion simply burns its
    /// ID; there are ten billion of them.
    fn reserve(&mut self, output_dir: &Path, prefix: &str, ext: &str) -> Result<u64, String> {
        for _ in 0..1000 {
            let id = rand::random_range(0..ID_MODULO);
            if !self.is_free(output_dir, prefix, ext, id) {
                // Remember it either way so we do not re-test the same name.
                self.used.insert(id);
                continue;
            }

            writeln!(self.file, "{}", format_id(id))
                .and_then(|()| self.file.flush())
                .and_then(|()| self.file.sync_all())
                .map_err(|e| format!("Could not record the output ID: {e}"))?;
            self.used.insert(id);
            return Ok(id);
        }
        Err("Could not find a free output ID".into())
    }
}

// --------------------------------------------------------------- Cleaning ---

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    /// Which batch produced this event. A page applies an event only when it
    /// belongs to the batch it started, so one that outlived a reload cannot
    /// write into rows it has never seen.
    batch_id: u64,
    index: usize,
    total: usize,
    input_name: String,
    output_name: Option<String>,
    status: &'static str,
    message: Option<String>,
    /// Present once a file has been cleaned and checked. `None` while processing.
    verification: Option<VerificationReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileResult {
    input_name: String,
    output_name: Option<String>,
    status: &'static str,
    message: Option<String>,
    verification: Option<VerificationReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanSummary {
    /// The batch these results came from, the same id its progress carried.
    batch_id: u64,
    output_dir: String,
    completed: usize,
    errors: usize,
    /// Cleaned files whose every verification check passed.
    verified: usize,
    /// Cleaned files that were written but failed at least one check. These are
    /// deliberately counted apart from `errors`: the output exists, but the
    /// product will not call it verified.
    verification_failures: usize,
    fields_removed: usize,
    /// Privacy and technical (structural) fields removed, reported apart.
    privacy_fields_removed: usize,
    technical_fields_removed: usize,
    chapters_removed: usize,
    data_streams_removed: usize,
    /// Streams the containers marked as attached cover art, removed.
    cover_art_streams_removed: usize,
    subtitle_streams_removed: usize,
    /// The options this batch actually ran with, as received at the click.
    options: CleaningOptions,
    results: Vec<FileResult>,
}

fn ffmpeg_args(
    input: &Path,
    output: &Path,
    format: FormatProfile,
    attempt: MuxAttempt,
    options: CleaningOptions,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        // -n, not -y: we already guarantee the target is free, so an existing file
        // means something is wrong and must not be overwritten.
        "-n".into(),
    ];
    if format.container == ContainerProfile::Avi {
        // AVI can carry streams that FFmpeg demuxes as `Unknown: none`. `-dn`
        // cannot classify those, so allow the explicit `-map 0` to skip them
        // rather than failing or copying their payload.
        args.push("-ignore_unknown".into());
    }
    args.extend([
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-map".into(),
        "0".into(),
        // `-map 0` selects attachment streams as well, and `-dn` does not reach
        // them: it drops data streams only. Matroska is the one supported
        // container that can carry an attachment -- an embedded subtitle font,
        // typically -- and the metadata stripping below removes the `filename`
        // and `mimetype` tags its muxer requires for one, so leaving an
        // attachment mapped fails the whole file at header write. Excluding it
        // removes the embedded payload, which is what the scan already tells
        // the user happens. The `?` keeps the negative map inert on every
        // container that cannot carry one.
        //
        // A Matroska cover under a mimetype FFmpeg cannot map to an image codec
        // really is an attachment and goes with the rest, which is the right
        // outcome for an opaque embedded file.
        "-map".into(),
        "-0:t?".into(),
        // Cover art FFmpeg reports as a video stream marked `attached_pic` is
        // not an attachment, so the map above never selects it. It is an image
        // copied byte for byte, EXIF and all -- a phone photo used as a cover
        // carries its camera and location -- so it goes too. Selected by that
        // mark and nothing else: never by index, which could name a real
        // second camera angle, and never by codec or frame count, which cannot
        // tell a still from footage. An image the container does not mark is
        // kept as the video it claims to be. A negative map that matches
        // nothing is inert, so no `?` is needed.
        "-map".into(),
        "-0:disp:attached_pic".into(),
        "-c".into(),
        "copy".into(),
        // Global metadata, per-stream metadata and chapters all dropped.
        "-map_metadata".into(),
        "-1".into(),
        "-map_metadata:s".into(),
        "-1".into(),
        "-map_chapters".into(),
        "-1".into(),
        // `-map 0` copies data tracks too, and those carry metadata of their own:
        // GoPro `gpmd` telemetry, iPhone `mebx`, chapter text. Dropped, payload
        // and all. Data streams only -- attachments are handled by the negative
        // map above.
        "-dn".into(),
    ]);
    if options.remove_subtitles {
        // Only on request: subtitles are media, kept by default.
        args.push("-sn".into());
    }
    args.extend(["-fflags".into(), "+bitexact".into()]);
    if attempt == MuxAttempt::FastStart {
        debug_assert_eq!(format.container, ContainerProfile::IsoBmff);
        args.push("-movflags".into());
        args.push("+faststart".into());
    }
    args.push("-f".into());
    args.push(format.output_muxer.into());
    args.push(output.to_string_lossy().into_owned());
    args
}

/// FFmpeg's sign-off when a run fails. It says only that something went wrong.
const FFMPEG_TERMINATOR: &str = "Conversion failed!";

/// Lines FFmpeg prints on its way out that describe the run rather than what
/// went wrong: the sign-off, the progress counter, the size summary and the
/// repeat marker.
///
/// A failure before or at header write puts the error last. A failure after
/// muxing has started does not: the errors come first, then this tail. Taking
/// one line back from the sign-off would then hand the user `frame= 76 fps=0.0
/// ... speed= 321x`, which reads like a success report. These patterns are
/// deliberately narrow -- they must never match a line that begins with
/// `Error`, which is the diagnostic we are walking back to find.
fn is_ffmpeg_run_summary(line: &str) -> bool {
    line == FFMPEG_TERMINATOR
        || line.starts_with("frame=")
        || line.starts_with("size=")
        || line.starts_with("Last message repeated")
        || line.contains("muxing overhead:")
}

/// Drops an `[out#0/matroska @ 0000017db62c0ac0] ` style prefix. The component
/// name means nothing to a user and the value after the `@` is a heap address,
/// which has no business on screen.
fn strip_component_prefix(line: &str) -> &str {
    let Some(rest) = line.strip_prefix('[') else {
        return line;
    };
    let Some(end) = rest.find("] ") else {
        return line;
    };
    if !rest[..end].contains(" @ ") {
        return line;
    }
    rest[end + 2..].trim_start()
}

/// The one line of FFmpeg's stderr that names the problem, without its
/// component prefix and before any redaction. `None` when it wrote nothing.
fn ffmpeg_diagnostic(text: &str) -> Option<&str> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    // Walk back over the closing summary rather than assuming it is one line.
    // If every line is summary, its last line still beats saying nothing.
    lines
        .iter()
        .rev()
        .find(|line| !is_ffmpeg_run_summary(line))
        .or_else(|| lines.last())
        .map(|line| strip_component_prefix(line))
}

/// The most useful single line FFmpeg wrote, made safe to show.
///
/// One line, never the whole of stderr: the rest is the banner, the input
/// report and the stream mapping, none of which means anything here. The output
/// paths -- the temporary file, its name and its folder -- are replaced by what
/// they are, because the message is shown beside the file's own name and an
/// absolute path adds nothing to it.
fn last_ffmpeg_error(stderr: &[u8], output: &Path, output_dir: &Path) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut message = ffmpeg_diagnostic(&text)
        .unwrap_or("FFmpeg failed")
        .to_string();
    // Longest first: the folder is a prefix of the file. Bracketed so the
    // result reads as a redaction rather than as a strange file name.
    for (path, replacement) in [(output, "[output file]"), (output_dir, "[output folder]")] {
        let raw = path.to_string_lossy();
        if !raw.is_empty() {
            message = message.replace(raw.as_ref(), replacement);
        }
    }
    if let Some(name) = output.file_name().and_then(|name| name.to_str()) {
        message = message.replace(name, "[output file]");
    }

    if message.is_empty() {
        "FFmpeg failed".into()
    } else {
        message
    }
}

/// Leftovers from a batch that was killed mid-run. Only our own marker is touched.
fn remove_stale_temp_files(output_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(output_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(TEMP_PREFIX) && entry.path().is_file() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Everything that decides whether this app will touch a file at all: it exists,
/// its extension is in the support matrix, and an `.m4v` is a real ISO-BMFF file
/// rather than a raw elementary stream.
///
/// Shared by scanning and cleaning so a file can never pass the scan and then be
/// rejected by the cleaner with a different message.
fn validate_input(input: &Path) -> Result<FormatProfile, String> {
    if !input.is_file() {
        return Err("File is no longer available".into());
    }
    let normalized_ext = input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let format = format_profile(&normalized_ext).ok_or_else(|| {
        format!(
            "Unsupported file type. Supported: {}.",
            supported_format_labels()
        )
    })?;
    if normalized_ext == "m4v" && !is_iso_bmff(input)? {
        return Err(
            "Unsupported M4V variant. Supported M4V files must use an ISO-BMFF container; raw MPEG-4 video streams are not accepted."
                .into(),
        );
    }
    Ok(format)
}

/// Cleans one video. Returns the final file name on success.
fn clean_one(
    input: &Path,
    output_dir: &Path,
    prefix: &str,
    options: CleaningOptions,
    registry: &mut IdRegistry,
) -> Result<String, String> {
    let format = validate_input(input)?;
    let original_ext = input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    // Preserve the source spelling too (`.MOV` stays `.MOV`), while lookup above is
    // case-insensitive. Every accepted value is one of the safe ASCII extensions in
    // FORMAT_PROFILES.
    let id = format_id(registry.reserve(output_dir, prefix, original_ext)?);
    let final_name = format!("{prefix}_{id}.{original_ext}");
    let final_path = output_dir.join(&final_name);
    let temp_path = output_dir.join(format!("{TEMP_PREFIX}{id}.{original_ext}"));

    let run = |attempt: MuxAttempt| {
        ffmpeg()
            .args(ffmpeg_args(input, &temp_path, format, attempt, options))
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    FFMPEG_MISSING.to_string()
                } else {
                    format!("Could not start FFmpeg: {e}")
                }
            })
    };

    let mut last_error = "FFmpeg failed".to_string();
    for attempt in format.mux_attempts() {
        let result = match run(*attempt) {
            Ok(result) => result,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
        };

        if result.status.success() {
            // The finished name only appears once the bytes are all there.
            std::fs::rename(&temp_path, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&temp_path);
                format!("Could not move the cleaned file into place: {e}")
            })?;
            return Ok(final_name);
        }

        last_error = last_ffmpeg_error(&result.stderr, &temp_path, output_dir);
        // ISO-BMFF gets one retry without faststart. Other profiles have exactly
        // one attempt. Removing the partial output is required because `-n` must
        // keep protecting every invocation from overwrites.
        let _ = std::fs::remove_file(&temp_path);
    }

    Err(format!(
        "Stream copy failed for this {} file: {last_error}. No video or audio re-encoding was attempted.",
        format.label
    ))
}

// ----------------------------------------------------------- Privacy scan ---

/// One inspected file as the UI sees it. Normalised in Rust: the frontend never
/// receives ffprobe's JSON, only this.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanView {
    path: String,
    name: String,
    /// False when the file could not be inspected at all. The batch continues
    /// either way; a file that cannot be scanned can still be cleaned.
    ok: bool,
    error: Option<String>,
    summary: PrivacySummary,
    findings: Vec<PrivacyFinding>,
    container: Option<String>,
    duration_seconds: Option<f64>,
    /// Video streams that are footage: detected cover art is counted apart.
    video_streams: usize,
    audio_streams: usize,
    subtitle_streams: usize,
    /// Streams the container marks as attached cover art, and only those. Not
    /// a count of every picture the file might hold.
    cover_images: usize,
    other_streams: usize,
    chapter_count: usize,
    field_count: usize,
}

fn failed_scan(path: &str, name: String, error: String) -> ScanView {
    ScanView {
        path: path.to_string(),
        name,
        ok: false,
        error: Some(error),
        summary: PrivacySummary::default(),
        findings: Vec::new(),
        container: None,
        duration_seconds: None,
        video_streams: 0,
        audio_streams: 0,
        subtitle_streams: 0,
        cover_images: 0,
        other_streams: 0,
        chapter_count: 0,
        field_count: 0,
    }
}

/// Inspect and classify one file. Never returns an error: a failure becomes a
/// `ScanView` marked `ok: false`, so one bad file cannot stop a batch.
fn scan_one(raw: &str) -> ScanView {
    let path = PathBuf::from(raw);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());

    if let Err(error) = validate_input(&path) {
        return failed_scan(raw, name, error);
    }
    let report = match inspect::inspect(&path) {
        Ok(report) => report,
        Err(error) => return failed_scan(raw, name, error),
    };

    let findings = privacy::classify(&report);
    let counts = privacy::stream_counts(&report);

    ScanView {
        path: raw.to_string(),
        name,
        ok: true,
        error: None,
        summary: PrivacySummary::of(&findings),
        findings,
        container: report.container.clone(),
        duration_seconds: report.duration_seconds,
        video_streams: counts.video,
        audio_streams: counts.audio,
        subtitle_streams: counts.subtitle,
        cover_images: counts.cover_art,
        other_streams: counts.other,
        chapter_count: report.chapters.len(),
        field_count: report.fields.len(),
    }
}

/// Scan a whole batch, reporting each result as it lands so the UI can fill rows
/// progressively instead of waiting for the slowest file.
fn run_scan(paths: &[String], mut on_result: impl FnMut(usize, usize, &ScanView)) -> Vec<ScanView> {
    let total = paths.len();
    let mut views = Vec::with_capacity(total);
    for (index, raw) in paths.iter().enumerate() {
        let view = scan_one(raw);
        on_result(index, total, &view);
        views.push(view);
    }
    views
}

// --------------------------------------------------------------- Cleaning ---

/// Inspect, clean and verify one file.
///
/// The before-report is taken fresh here rather than reused from the scan: the
/// file may have changed since the user added it, and verification has to
/// compare the output against what was actually on disk when it was cleaned.
///
/// A file that cannot be inspected is still cleaned. Inspection is what makes
/// verification possible, not what makes cleaning safe, and refusing to clean a
/// file the v0.4 pipeline handles fine would be a regression.
fn clean_and_verify(
    input: &Path,
    output_dir: &Path,
    prefix: &str,
    options: CleaningOptions,
    registry: &mut IdRegistry,
) -> (Result<String, String>, Option<VerificationReport>) {
    let fingerprint = OriginalFingerprint::capture(input);
    let before = inspect::inspect(input).ok();
    let before_findings = before.as_ref().map(privacy::classify);

    // The options drive FFmpeg directly, so this runs whether or not the file
    // could be inspected. The plan below exists only to check the result.
    let cleaned = clean_one(input, output_dir, prefix, options, registry);
    let Ok(output_name) = &cleaned else {
        return (cleaned, None);
    };

    let (Some(before), Some(before_findings)) = (before.as_ref(), before_findings.as_ref()) else {
        // Cleaned, but there is no baseline to check it against. Reported as
        // unverified rather than quietly treated as verified.
        return (cleaned, None);
    };
    let plan = plan::plan_for(before, options);
    let report = verify::verify(
        input,
        &output_dir.join(output_name),
        before,
        before_findings,
        &plan,
        fingerprint,
        output_dir,
        TEMP_PREFIX,
    );
    (cleaned, Some(report))
}

/// The whole batch, with progress pushed through a callback so this stays
/// independent of Tauri and testable on its own.
#[allow(clippy::too_many_arguments)]
fn run_batch(
    paths: &[String],
    prefix: &str,
    output_dir: &Path,
    app_dir: &Path,
    batch_id: u64,
    options: CleaningOptions,
    mut on_progress: impl FnMut(Progress),
) -> Result<CleanSummary, String> {
    if paths.is_empty() {
        return Err("No videos selected".into());
    }
    if paths.len() > MAX_BATCH {
        return Err(format!(
            "Too many videos: {} selected, the limit is {MAX_BATCH}.",
            paths.len()
        ));
    }
    if !output_dir.is_dir() {
        return Err("The output folder no longer exists. Choose a new one.".into());
    }
    if !ffmpeg_available() {
        return Err(FFMPEG_MISSING.into());
    }

    remove_stale_temp_files(output_dir);
    let mut registry = IdRegistry::open(&app_dir.join("used-ids.txt"))?;

    let total = paths.len();
    let mut results = Vec::with_capacity(total);
    let (mut completed, mut errors) = (0usize, 0usize);
    let (mut verified, mut verification_failures) = (0usize, 0usize);
    let (mut fields_removed, mut chapters_removed, mut data_streams_removed) =
        (0usize, 0usize, 0usize);
    let (mut privacy_fields_removed, mut technical_fields_removed) = (0usize, 0usize);
    let (mut cover_art_streams_removed, mut subtitle_streams_removed) = (0usize, 0usize);

    for (index, raw) in paths.iter().enumerate() {
        let input = PathBuf::from(raw);
        let input_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());

        on_progress(Progress {
            batch_id,
            index,
            total,
            input_name: input_name.clone(),
            output_name: None,
            status: "processing",
            message: None,
            verification: None,
        });

        let (outcome, verification) =
            clean_and_verify(&input, output_dir, prefix, options, &mut registry);

        if let Some(report) = &verification {
            if report.verified {
                verified += 1;
            } else {
                verification_failures += 1;
            }
            fields_removed += report.fields_removed;
            privacy_fields_removed += report.privacy_fields_removed;
            technical_fields_removed += report.technical_fields_removed;
            chapters_removed += report.chapters_removed;
            data_streams_removed += report.data_streams_removed;
            cover_art_streams_removed += report.cover_art_streams_removed;
            subtitle_streams_removed += report.subtitle_streams_removed;
        }

        let (status, output_name, message) = match outcome {
            Ok(name) => {
                completed += 1;
                let note = match &verification {
                    Some(report) if !report.verified => Some(failed_check_summary(report)),
                    // Cleaned, but with no baseline to verify against.
                    None => Some(
                        "Cleaned, but this file could not be inspected, so the result was not verified."
                            .to_string(),
                    ),
                    Some(_) => None,
                };
                ("completed", Some(name), note)
            }
            Err(e) => {
                errors += 1;
                ("error", None, Some(e))
            }
        };

        on_progress(Progress {
            batch_id,
            index,
            total,
            input_name: input_name.clone(),
            output_name: output_name.clone(),
            status,
            message: message.clone(),
            verification: verification.clone(),
        });
        results.push(FileResult {
            input_name,
            output_name,
            status,
            message,
            verification,
        });
    }

    Ok(CleanSummary {
        batch_id,
        output_dir: output_dir.to_string_lossy().into_owned(),
        completed,
        errors,
        verified,
        verification_failures,
        fields_removed,
        privacy_fields_removed,
        technical_fields_removed,
        chapters_removed,
        data_streams_removed,
        cover_art_streams_removed,
        subtitle_streams_removed,
        options,
        results,
    })
}

/// A short, safe description of why a file is not verified. Only the details of
/// the checks that failed, all of which are generated by this app rather than
/// copied out of the media file.
///
/// The details, not the names: every check is named for the thing it asserts
/// ("Sensitive metadata removed"), so listing names after "Verification failed"
/// states the opposite of what happened. Each detail is already written in the
/// failing direction ("3 sensitive field(s) survived").
fn failed_check_summary(report: &VerificationReport) -> String {
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.detail.as_str())
        .collect();
    if failed.is_empty() {
        return "Verification did not pass.".to_string();
    }
    format!("Verification failed: {}.", failed.join(", "))
}

// --------------------------------------------------------------- Commands ---

/// The single place that decides where `settings.json` and `used-ids.txt` live.
/// Resolved by Tauri from the bundle identifier in `tauri.conf.json`, so renaming
/// or forking the product moves the folder without touching this code.
fn app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Could not locate the application data folder: {e}"))
}

/// Legacy config location used only to migrate installations <= v0.2.0.
const LEGACY_CONFIG_DIR: &str = "com.aurevm.videocleaner";

fn legacy_app_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .config_dir()
        .ok()
        .map(|base| base.join(LEGACY_CONFIG_DIR))
}

/// One-time, best-effort copy of the pre-rebrand settings into the new config
/// folder. Per file rather than per folder, so a half-populated new folder still
/// picks up what it is missing, and anything already present always wins.
fn migrate_legacy_config(new_dir: &Path, legacy_dir: &Path) -> Result<(), String> {
    if !legacy_dir.is_dir() {
        return Ok(());
    }
    for name in ["settings.json", "used-ids.txt"] {
        let from = legacy_dir.join(name);
        let to = new_dir.join(name);
        if !from.is_file() || to.exists() {
            continue;
        }
        std::fs::create_dir_all(new_dir)
            .map_err(|e| format!("Could not create the config folder: {e}"))?;
        std::fs::copy(&from, &to).map_err(|e| format!("Could not migrate {name}: {e}"))?;
    }
    // The old folder is left in place on purpose: copying is reversible, moving is not.
    Ok(())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SupportedFormatView {
    extension: String,
    label: String,
}

/// Runtime single source of truth for the file picker, drag/drop validation and
/// user-facing supported-format copy in the React frontend.
#[tauri::command]
fn get_supported_formats() -> Vec<SupportedFormatView> {
    FORMAT_PROFILES
        .iter()
        .map(|profile| SupportedFormatView {
            extension: profile.extension.to_string(),
            label: profile.label.to_string(),
        })
        .collect()
}

/// `null` when both bundled tools are usable, otherwise the message to show.
/// Returning the text from here keeps the wording in one place, since it differs
/// between a development build (PATH) and a packaged one (bundled sidecar).
///
/// FFprobe is checked alongside FFmpeg because the privacy scan depends on it;
/// one clear message at startup beats the same failure on every file.
///
/// Async for the same reason the media commands are: each check starts a real
/// process and waits for it, and this one runs while the window is drawing for
/// the first time. On the window's own thread that is a visible stall before
/// anything appears, and adding the probe made it two.
#[tauri::command]
async fn check_ffmpeg() -> Result<Option<&'static str>, String> {
    run_blocking(TOOLS_INTERNAL_ERROR, || {
        if !ffmpeg_available() {
            return Ok(Some(FFMPEG_MISSING));
        }
        if !inspect::available() {
            return Ok(Some(FFPROBE_MISSING));
        }
        Ok(None)
    })
    .await
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<SettingsView, String> {
    Ok(load_settings(&app_dir(&app)?).into())
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    prefix: String,
    output_directory: String,
) -> Result<SettingsView, String> {
    Ok(store_output_settings(&app_dir(&app)?, &prefix, &output_directory)?.into())
}

/// Store the user's standing cleaning choice, leaving prefix and folder alone.
/// Separate from `save_settings` so neither control has to send the other's
/// value to avoid wiping it.
#[tauri::command]
fn save_cleaning_options(app: AppHandle, options: CleaningOptions) -> Result<SettingsView, String> {
    Ok(store_cleaning_options(&app_dir(&app)?, options)?.into())
}

/// Everything `save_settings` does, minus Tauri. Keeps the cleaning options.
fn store_output_settings(
    app_dir: &Path,
    prefix: &str,
    output_directory: &str,
) -> Result<Settings, String> {
    let prefix = sanitize_prefix(prefix)?;

    let output_directory = output_directory.trim().to_string();
    if !output_directory.is_empty() {
        let path = Path::new(&output_directory);
        if path.exists() && !path.is_dir() {
            return Err("That path is a file, not a folder.".into());
        }
        // The picker only returns existing folders; this covers one the user
        // typed or one that was deleted and is being re-selected.
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Could not use that output folder: {e}"))?;
    }

    update_settings(app_dir, |settings| {
        settings.prefix = prefix;
        settings.output_directory = output_directory;
    })
}

/// Everything `save_cleaning_options` does, minus Tauri. Keeps prefix and folder.
fn store_cleaning_options(app_dir: &Path, options: CleaningOptions) -> Result<Settings, String> {
    update_settings(app_dir, |settings| {
        settings.cleaning = options;
    })
}

// Shown instead of a panic message, which can carry file paths.
const TOOLS_INTERNAL_ERROR: &str = "The bundled tools could not be checked.";
const SCAN_INTERNAL_ERROR: &str = "The privacy scan stopped because of an internal error.";
const CLEAN_INTERNAL_ERROR: &str =
    "Cleaning stopped because of an internal error. Files finished before it are in the output folder.";
const EDIT_INTERNAL_ERROR: &str =
    "Editing stopped because of an internal error. Files finished before it are in the output folder.";

/// Runs `work` on Tauri's blocking thread pool and waits for it to finish.
///
/// A synchronous command runs on the window's own thread, so every ffprobe and
/// FFmpeg run would stop the window from repainting and hold every progress
/// event back until the batch ended. The body of an `async` command is no place
/// for it either: that runs on one of the runtime's few core workers, which
/// Tauri's own IPC needs. Long synchronous work belongs on the blocking pool.
///
/// The work is awaited, never detached, so a command still answers only once
/// its batch is over. A panic becomes `internal_error`; its text is never
/// forwarded to the UI.
async fn run_blocking<T, F>(internal_error: &'static str, work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .unwrap_or_else(|_| Err(internal_error.to_string()))
}

/// Everything `scan_videos` does, minus Tauri, so it can be tested as is.
fn scan_request(
    paths: &[String],
    on_result: impl FnMut(usize, usize, &ScanView),
) -> Result<Vec<ScanView>, String> {
    if paths.len() > MAX_BATCH {
        return Err(format!(
            "Too many videos: {} selected, the limit is {MAX_BATCH}.",
            paths.len()
        ));
    }
    if !inspect::available() {
        return Err(FFPROBE_MISSING.into());
    }
    Ok(run_scan(paths, on_result))
}

/// Inspect and classify a batch without touching anything on disk. Emits one
/// `scan-progress` event per file so rows can fill in as results arrive.
#[tauri::command]
async fn scan_videos(app: AppHandle, paths: Vec<String>) -> Result<Vec<ScanView>, String> {
    run_blocking(SCAN_INTERNAL_ERROR, move || {
        scan_request(&paths, |index, total, view| {
            let _ = app.emit(
                "scan-progress",
                ScanProgress {
                    index,
                    total,
                    view: view.clone(),
                },
            );
        })
    })
    .await
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    index: usize,
    total: usize,
    view: ScanView,
}

// Both refuse a Clean and an Edit alike: the two share one batch state, so
// the busy message names neither.
const CLEAN_BUSY: &str = "Another media operation is already running. Wait for it to finish.";
const CLEAN_UPDATING: &str = "An update is being installed. Try again once that has finished.";

/// Set while a Clean or an Edit batch runs. There is one of these for both, so
/// the two are mutually exclusive with each other and with an update install.
///
/// When the commands ran on the window's thread, that thread kept batches
/// apart for free. Off it, a page reload mid-batch leaves the old batch running
/// and lets a new one start in the same folder, where its stale-temp sweep and
/// the "No temporary files left" check would trip over the first batch's file.
/// This keeps the batches' disk work apart; it does not reconnect a reloaded
/// page to the batch still running.
///
/// It is also the only thing that knows whether cleaning is happening: a
/// reloaded page has forgotten, and the updater and the close button both have
/// to ask something that has not.
static CLEAN_STATE: BatchState = BatchState::new();

/// Whether a batch is running, whether an update install has been reserved,
/// and which batch it is, in a single word.
///
/// The three have to be read and changed together. A caller that learned
/// "running" and "which" separately could mix two batches; a caller that
/// checked "not cleaning" and then reserved an install could have a batch start
/// in between. So they share one atomic and every transition is one
/// compare-and-swap: bit 0 is "a batch is running", bit 1 is "an update install
/// is reserved", and the rest is the batch counter.
///
/// Cleaning and installing are mutually exclusive, and each transition refuses
/// if either bit is set. Starting batch `n + 1` from idle is `+5` (`+4` for the
/// counter, `+1` for the bit) and finishing it is `-1`; reserving an install is
/// `+2` and releasing it is `-2`.
///
/// The counter is the batch identity. A plain integer is enough: it has to tell
/// this process's batches apart for as long as the process lives, nothing more,
/// so there is no reason to take a UUID dependency for it. It is internal to
/// the app and carries nothing from the files being cleaned.
pub(crate) struct BatchState(AtomicU64);

/// A batch is running.
const CLEANING: u64 = 0b01;
/// An update install has been reserved and may be under way.
const INSTALLING: u64 = 0b10;
/// One batch on the counter, which starts above both flags.
const ONE_BATCH: u64 = 0b100;
/// Set while either exclusive transition is held.
const EXCLUSIVE: u64 = CLEANING | INSTALLING;

/// What the frontend is allowed to know about the pipeline: whether it is busy,
/// why, and which batch that is. Deliberately nothing else -- no paths, no file
/// names, no counts, nothing read out of a video.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessingState {
    /// A media batch -- Clean or Edit -- is running. Named for Clean because
    /// the page already reads it under this name; kept until the page learns
    /// about Edit.
    cleaning: bool,
    /// An update install is reserved. Nothing may start cleaning until it is
    /// released, which after a successful install means never, because the
    /// installer ends this process.
    installing: bool,
    /// The newest batch this process has started, `0` before the first one.
    /// While `cleaning` it is the batch that is running.
    batch_id: u64,
}

impl BatchState {
    pub(crate) const fn new() -> Self {
        BatchState(AtomicU64::new(0))
    }

    /// One load, so the flags and the id can never disagree.
    pub(crate) fn snapshot(&self) -> ProcessingState {
        let word = self.0.load(Ordering::Acquire);
        ProcessingState {
            cleaning: word & CLEANING != 0,
            installing: word & INSTALLING != 0,
            batch_id: word / ONE_BATCH,
        }
    }

    /// The running batch id, or `None` when nothing is running.
    fn active_id(&self) -> Option<u64> {
        let state = self.snapshot();
        state.cleaning.then_some(state.batch_id)
    }

    /// Claim the app for an update install, or refuse.
    ///
    /// The check and the claim are the same compare-and-swap, so a Clean cannot
    /// slip between them: once this returns `true` no batch can start until the
    /// reservation is released, and it returns `false` if one already has.
    ///
    /// Not RAII, because the installer is driven by the signed updater plugin
    /// from the page, so the reservation has to outlive the command that takes
    /// it. It is released deterministically in three ways: the page releases it
    /// when `install()` fails to start the installer, the page releases any it
    /// finds on load, and a successful install ends the process.
    fn try_reserve_install(&self) -> bool {
        let mut word = self.0.load(Ordering::Relaxed);
        loop {
            if word & EXCLUSIVE != 0 {
                return false;
            }
            match self.0.compare_exchange_weak(
                word,
                word + INSTALLING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => word = actual,
            }
        }
    }

    /// Give the app back. Doing this when nothing is reserved is a no-op rather
    /// than an underflow, so the page may call it on load without knowing.
    fn release_install(&self) {
        let mut word = self.0.load(Ordering::Relaxed);
        loop {
            if word & INSTALLING == 0 {
                return;
            }
            match self.0.compare_exchange_weak(
                word,
                word - INSTALLING,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => word = actual,
            }
        }
    }
}

/// Holds the state for as long as it lives. Released by `Drop`, so a batch that
/// ends in an error or a panic frees it too.
///
/// Nothing can reach the cleaning or editing pipeline without one: `clean_with`
/// and `edit_with` take it by value, so "the app is marked as busy" is not a
/// step that could be taken late or skipped, it is the ticket to the work.
struct BatchGuard {
    state: &'static BatchState,
    id: u64,
}

impl BatchGuard {
    /// Never waits: a second batch is refused, not queued, and so is one asked
    /// for while an update install holds the app. A refused attempt consumes no
    /// id, so the id reported by `snapshot` is always the batch that is really
    /// running. The reason comes from the word the swap actually saw, not from
    /// a second look afterwards.
    fn try_acquire(state: &'static BatchState) -> Result<Self, &'static str> {
        let mut word = state.0.load(Ordering::Relaxed);
        loop {
            if word & CLEANING != 0 {
                return Err(CLEAN_BUSY);
            }
            if word & INSTALLING != 0 {
                return Err(CLEAN_UPDATING);
            }
            match state.0.compare_exchange_weak(
                word,
                word + ONE_BATCH + CLEANING,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok(BatchGuard {
                        state,
                        id: word / ONE_BATCH + 1,
                    })
                }
                Err(actual) => word = actual,
            }
        }
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        // Clears the running bit and leaves the counter where it is, so the
        // next batch is a different one and this one is never handed out again.
        self.state.0.fetch_sub(CLEANING, Ordering::Release);
    }
}

/// The two steps `clean_videos` takes, run inline: claim the app, then clean
/// with what was claimed. The command keeps them apart on purpose, because only
/// the second belongs on the blocking pool; here they are together so a test
/// can drive a whole batch against its own state without a live `AppHandle`.
#[cfg(test)]
fn clean_request(
    state: &'static BatchState,
    app_dir: &Path,
    paths: &[String],
    options: CleaningOptions,
    on_progress: impl FnMut(Progress),
) -> Result<CleanSummary, String> {
    let batch = BatchGuard::try_acquire(state).map_err(str::to_string)?;
    clean_with(batch, app_dir, paths, options, on_progress)
}

/// Everything `clean_videos` does once it holds the app and knows the app data
/// folder. The guard comes in by value and is held until the last file is
/// verified, so nothing here can run while the app looks idle to the close
/// button, the updater or another Clean.
///
/// `options` is the value the page sent with the click and is used for every
/// file. The stored preference in `settings.json` is not consulted: it may
/// have been changed, or be mid-save, by the time the batch runs.
fn clean_with(
    batch: BatchGuard,
    app_dir: &Path,
    paths: &[String],
    options: CleaningOptions,
    on_progress: impl FnMut(Progress),
) -> Result<CleanSummary, String> {
    let settings = load_settings(app_dir);
    if settings.output_directory.is_empty() {
        return Err("Choose an output folder first.".into());
    }
    let output_dir = PathBuf::from(&settings.output_directory);

    run_batch(
        paths,
        &settings.prefix,
        &output_dir,
        app_dir,
        batch.id,
        options,
        on_progress,
    )
}

/// Clean, verify and report a batch, one file after another, emitting
/// `clean-progress` as each file starts and as each one finishes.
///
/// The app is claimed here, before the work is handed to the blocking pool.
/// Taking it inside that closure instead would leave a gap between accepting
/// the request and the state saying so, and in that gap the close button would
/// read the app as idle and let the window go while the worker was starting
/// FFmpeg. Only the claim is early: every heavy step still runs on the pool.
#[tauri::command]
async fn clean_videos(
    app: AppHandle,
    paths: Vec<String>,
    options: CleaningOptions,
) -> Result<CleanSummary, String> {
    let batch = BatchGuard::try_acquire(&CLEAN_STATE).map_err(str::to_string)?;
    run_blocking(CLEAN_INTERNAL_ERROR, move || {
        let dir = app_dir(&app)?;
        clean_with(batch, &dir, &paths, options, |progress| {
            let _ = app.emit("clean-progress", progress);
        })
    })
    .await
}

/// `edit_videos` run inline, the way `clean_request` runs a Clean: validate,
/// claim the app, edit. A request that fails validation never claims it.
#[cfg(test)]
fn edit_request(
    state: &'static BatchState,
    app_dir: &Path,
    request: EditRequest,
    on_progress: impl FnMut(edit_batch::EditProgress),
) -> Result<EditSummary, String> {
    let request = request.validate()?;
    let batch = BatchGuard::try_acquire(state).map_err(str::to_string)?;
    edit_with(batch, app_dir, &request, on_progress)
}

/// Edit a batch's metadata, one file after another, emitting `edit-progress`
/// as each file starts and as each one finishes.
///
/// The request is validated first, so one the backend would refuse never
/// claims the app or a batch id. Then the app is claimed here, before the work
/// goes to the blocking pool, for the reason `clean_videos` gives: Clean and
/// Edit share one state, so no gap may exist in which an accepted Edit looks
/// idle to the close button, the updater or another batch.
#[tauri::command]
async fn edit_videos(app: AppHandle, request: EditRequest) -> Result<EditSummary, String> {
    let request = request.validate()?;
    let batch = BatchGuard::try_acquire(&CLEAN_STATE).map_err(str::to_string)?;
    run_blocking(EDIT_INTERNAL_ERROR, move || {
        let dir = app_dir(&app)?;
        edit_with(batch, &dir, &request, |progress| {
            let _ = app.emit("edit-progress", progress);
        })
    })
    .await
}

/// Whether cleaning is running, straight from the pipeline own state.
///
/// The page that started a batch forgets it on reload; this does not. Anything
/// whose correctness depends on a batch being over -- installing an update,
/// closing the window, starting another batch -- asks here rather than trusting
/// what React last remembered.
#[tauri::command]
fn get_processing_state() -> ProcessingState {
    CLEAN_STATE.snapshot()
}

/// Claim the app for an update install. `false` means a batch holds it, or an
/// install already does, and nothing may be installed.
///
/// This replaces asking `get_processing_state` and then installing: those are
/// two round trips, and a Clean accepted between them would have been cleaning
/// by the time the installer ran. Claiming is one transition, and it shuts out
/// Clean for as long as it is held.
#[tauri::command]
fn reserve_update_install() -> bool {
    CLEAN_STATE.try_reserve_install()
}

/// Give the app back after an install that never started, or on page load,
/// where anything still reserved was left by a page that no longer exists.
/// Releasing when nothing is reserved does nothing.
#[tauri::command]
fn release_update_install() {
    CLEAN_STATE.release_install()
}

/// Whether a close request has to be refused. The window asks the pipeline, not
/// the page: a reloaded page believes nothing is running.
fn cleaning_blocks_close(state: &BatchState) -> bool {
    state.active_id().is_some()
}

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Never fatal: if this fails the app simply starts on its defaults.
            if let (Ok(new_dir), Some(legacy_dir)) =
                (app_dir(app.handle()), legacy_app_dir(app.handle()))
            {
                if let Err(e) = migrate_legacy_config(&new_dir, &legacy_dir) {
                    eprintln!("settings migration skipped: {e}");
                }
            }
            Ok(())
        })
        // A responsive window is a window whose close button works mid-batch.
        // FFmpeg would carry on in its own process and leave a half-written
        // temporary file behind, so a normal close is refused while a batch is
        // running and the window says why. Only this one: nothing here tries to
        // survive Task Manager, a shutdown or a crash.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if cleaning_blocks_close(&CLEAN_STATE) {
                    api.prevent_close();
                    // No payload: the window already knows what to say, and a
                    // lifecycle event is no place for anything about the files.
                    let _ = window.emit("close-blocked", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            check_ffmpeg,
            get_supported_formats,
            get_settings,
            save_settings,
            save_cleaning_options,
            get_processing_state,
            reserve_update_install,
            release_update_install,
            scan_videos,
            clean_videos,
            edit_videos,
            open_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ------------------------------------------------------------------ Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    // ------------------------------------------ What FFmpeg is allowed to say ---

    /// The message a user gets is one line, and it has to be the line that
    /// names the problem. FFmpeg signs off with a generic "Conversion failed!",
    /// so a mux failure would otherwise report nothing at all.
    #[test]
    fn a_mux_failure_reports_the_line_that_names_the_problem() {
        let stderr = b"  Stream #0:3 -> #0:3 (copy)\n\
[matroska @ 0000017db633bf00] Attachment stream 3 has no filename tag.\n\
[out#0/matroska @ 0000017db62c0ac0] Could not write header (incorrect codec parameters ?): Invalid argument\n\
Conversion failed!\n";
        assert_eq!(
            last_ffmpeg_error(stderr, Path::new(r"C:\out\clip.mkv"), Path::new(r"C:\out")),
            "Could not write header (incorrect codec parameters ?): Invalid argument"
        );
    }

    /// The value after the `@` is a heap address. It is noise at best and an
    /// internal detail at worst, and neither belongs on screen.
    #[test]
    fn an_ffmpeg_component_prefix_never_reaches_the_user() {
        let stderr = b"[mov,mp4 @ 000002bd58e45280] moov atom not found\n";
        let message =
            last_ffmpeg_error(stderr, Path::new(r"C:\out\clip.mp4"), Path::new(r"C:\out"));
        assert_eq!(message, "moov atom not found");
        assert!(!message.contains('@'));
    }

    /// FFmpeg echoes the path it was handed. That path is this app's temporary
    /// file inside the user's own folder, and the row already names the file.
    #[test]
    fn the_output_path_is_redacted_out_of_the_message() {
        let dir = Path::new(r"C:\Users\someone\Holiday");
        let output = dir.join(format!("{TEMP_PREFIX}0000000001.mp4"));
        let stderr = format!("Error opening output file {}.\n", output.display());

        let message = last_ffmpeg_error(stderr.as_bytes(), &output, dir);

        assert_eq!(message, "Error opening output file [output file].");
        assert!(
            !message.contains("someone"),
            "a local path leaked: {message}"
        );
        assert!(!message.contains(TEMP_PREFIX));
    }

    /// The terminator is only stepped over. A single useful line stays put.
    #[test]
    fn an_input_failure_keeps_the_diagnostic_that_explains_it() {
        let stderr = b"Error opening input files: Invalid data found when processing input\n";
        assert_eq!(
            last_ffmpeg_error(stderr, Path::new(r"C:\out\clip.mp4"), Path::new(r"C:\out")),
            "Error opening input files: Invalid data found when processing input"
        );
    }

    /// A failure after muxing has started prints its errors first and its
    /// closing summary last. Stepping back exactly one line from the sign-off
    /// would hand the user a progress counter that reads like a success report.
    #[test]
    fn a_failure_after_muxing_starts_still_reports_an_error() {
        let stderr = b"[vost#0:0/copy @ 000002703a7ca080] Error submitting a packet to the muxer: Invalid argument\n\
    Last message repeated 1 times\n\
[out#0/matroska @ 000002703a77ec40] Error muxing a packet\n\
[out#0/matroska @ 000002703a77ec40] Error writing trailer: Invalid argument\n\
[out#0/matroska @ 000002703a77ec40] Error closing file: Invalid argument\n\
video:88KiB audio:5KiB subtitle:0KiB other streams:0KiB global headers:0KiB muxing overhead: 1.234567%\n\
frame=   76 fps=0.0 q=-1.0 Lsize=      94KiB time=00:00:03.12 bitrate= 248.1kbits/s speed= 321x\n\
Conversion failed!\n";
        let message =
            last_ffmpeg_error(stderr, Path::new(r"C:\out\clip.mkv"), Path::new(r"C:\out"));
        assert_eq!(message, "Error closing file: Invalid argument");
        assert!(
            !message.starts_with("frame=") && !message.contains("speed="),
            "a progress line was reported as the error: {message}"
        );
    }

    /// Nothing to report is still not an empty message.
    #[test]
    fn silent_ffmpeg_output_still_produces_a_message() {
        assert_eq!(
            last_ffmpeg_error(b"", Path::new(r"C:\out\clip.mp4"), Path::new(r"C:\out")),
            "FFmpeg failed"
        );
        assert_eq!(
            last_ffmpeg_error(
                b"Conversion failed!\n",
                Path::new(r"C:\out\clip.mp4"),
                Path::new(r"C:\out")
            ),
            "Conversion failed!"
        );
    }

    fn temp_files(output_dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(output_dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .map(|entry| entry.path())
            .collect()
    }

    /// The whole v0.4 contract for one container, re-run on every supported
    /// format: the fixture really is sensitive, the output is readable, the
    /// metadata and chapters and data tracks are gone, the encoded payload is
    /// byte-identical, the source is untouched and nothing is left behind.
    fn assert_format_cleaning(extension: &str) {
        let dir = scratch(&format!("format-{extension}"));
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = sample_for_format(&dir, extension);
        let capabilities = fixture_capabilities(extension);
        let before = std::fs::read(&input).unwrap();

        // A/B: the fixture is valid and demonstrably contains sensitive metadata.
        if extension == "m4v" {
            assert!(is_iso_bmff(&input).unwrap(), "M4V fixture is not ISO-BMFF");
        }
        assert_ffmpeg_can_read(&input);
        let input_metadata = ffmetadata(&input);
        assert!(
            input_metadata.contains("global_secret"),
            "{extension} fixture lacks its metadata: {input_metadata}"
        );
        if capabilities.chapters {
            assert!(
                input_metadata.contains("[chapter]") && input_metadata.contains("chapter_secret"),
                "{extension} fixture lacks chapters: {input_metadata}"
            );
        }
        let input_kinds = stream_kinds(&input);
        assert!(input_kinds.contains(&"Video".to_string()));
        assert!(input_kinds.contains(&"Audio".to_string()));
        if let Some(kind) = capabilities.non_media_stream_kind {
            assert!(
                input_kinds.contains(&kind.to_string()),
                "{extension} fixture lacks its expected non-media stream: {input_kinds:?}"
            );
        }
        if capabilities.subtitle_codec.is_some() {
            assert!(
                input_kinds.contains(&"Subtitle".to_string()),
                "{extension} fixture lacks its subtitle stream: {input_kinds:?}"
            );
        }
        if extension == "avi" {
            assert!(
                contains(&before, DATA_CANARY),
                "AVI fixture lacks its data payload canary"
            );
        }
        if capabilities.attaches() {
            assert_eq!(
                input_kinds
                    .iter()
                    .filter(|kind| *kind == "Attachment")
                    .count(),
                2,
                "{extension} fixture lacks its two attachment streams: {input_kinds:?}"
            );
            for canary in [ATTACHMENT_CANARY_ALPHA, ATTACHMENT_CANARY_BETA] {
                assert!(
                    contains(&before, canary),
                    "{extension} fixture lacks an attachment payload canary"
                );
            }
        }

        let source_video_hash = stream_payload_hash(&input, "0:v:0");
        let source_audio_hash = stream_payload_hash(&input, "0:a:0");
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        let output_name = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap();
        let cleaned = out.join(&output_name);

        // C/D/J: a finished, readable output exists with the same extension.
        assert!(cleaned.is_file(), "{extension} output was not created");
        assert_eq!(
            cleaned.extension().and_then(|value| value.to_str()),
            input.extension().and_then(|value| value.to_str())
        );
        assert_ffmpeg_can_read(&cleaned);
        if extension == "m4v" {
            assert!(is_iso_bmff(&cleaned).unwrap(), "M4V output is not ISO-BMFF");
        }

        // E/F/G: sensitive global metadata, chapters and data streams are gone.
        let output_metadata = ffmetadata(&cleaned);
        assert!(
            !output_metadata.contains("global_secret")
                && !output_metadata.contains("sensitive_comment"),
            "{extension} metadata survived: {output_metadata}"
        );
        if capabilities.chapters {
            assert!(
                !output_metadata.contains("[chapter]")
                    && !output_metadata.contains("chapter_secret"),
                "{extension} chapters survived: {output_metadata}"
            );
        }
        let output_kinds = stream_kinds(&cleaned);
        // A whitelist, not a list of the two kinds that went wrong once. It
        // mirrors `StreamKind::is_media`, which is what decides whether the
        // cleaner keeps a stream, so any other kind surviving fails here
        // whatever it is -- an attachment included.
        assert!(
            output_kinds
                .iter()
                .all(|kind| matches!(kind.as_str(), "Video" | "Audio" | "Subtitle")),
            "{extension} output kept a non-media stream: {output_kinds:?}"
        );
        // And the media the user came for is still there.
        assert!(output_kinds.contains(&"Video".to_string()));
        assert!(output_kinds.contains(&"Audio".to_string()));
        if capabilities.subtitle_codec.is_some() {
            assert!(
                output_kinds.contains(&"Subtitle".to_string()),
                "{extension} lost its subtitle track: {output_kinds:?}"
            );
        }
        let cleaned_bytes = std::fs::read(&cleaned).unwrap();
        // Subtitles are kept, so the proof is symmetrical with the attachment
        // one below: the stream is still there AND its payload came through. A
        // remux that kept an empty subtitle track would pass the kind check.
        if capabilities.subtitle_codec.is_some() {
            assert!(
                contains(&cleaned_bytes, SUBTITLE_BODY.as_bytes()),
                "{extension} lost its subtitle payload"
            );
        }
        if extension == "avi" {
            assert!(
                !contains(&cleaned_bytes, DATA_CANARY),
                "AVI data payload survived"
            );
        }
        if capabilities.attaches() {
            for canary in [ATTACHMENT_CANARY_ALPHA, ATTACHMENT_CANARY_BETA] {
                assert!(
                    !contains(&cleaned_bytes, canary),
                    "{extension} attachment payload survived"
                );
            }
            for name in [ATTACHMENT_NAME_ALPHA, ATTACHMENT_NAME_BETA] {
                assert!(
                    !contains(&cleaned_bytes, name.as_bytes()),
                    "{extension} attachment filename survived"
                );
            }
        }

        // H: the encoded video and audio packet payloads are byte-identical.
        assert_eq!(
            source_video_hash,
            stream_payload_hash(&cleaned, "0:v:0"),
            "{extension} video payload changed"
        );
        assert_eq!(
            source_audio_hash,
            stream_payload_hash(&cleaned, "0:a:0"),
            "{extension} audio payload changed"
        );

        // I/K: production never touched the source and published no temp artifact.
        assert_eq!(
            std::fs::read(&input).unwrap(),
            before,
            "{extension} source changed"
        );
        assert!(temp_files(&out).is_empty(), "{extension} left a temp file");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn support_matrix_has_explicit_unique_profiles() {
        let expected = [
            ("mp4", "MP4", ContainerProfile::IsoBmff, "mp4", 2),
            ("mov", "MOV", ContainerProfile::IsoBmff, "mov", 2),
            ("m4v", "M4V", ContainerProfile::IsoBmff, "mp4", 2),
            ("mkv", "MKV", ContainerProfile::Matroska, "matroska", 1),
            ("webm", "WebM", ContainerProfile::WebM, "webm", 1),
            ("avi", "AVI", ContainerProfile::Avi, "avi", 1),
        ];
        let mut extensions = HashSet::new();
        for (extension, label, container, muxer, attempts) in expected {
            let profile = format_profile(extension).unwrap();
            assert!(extensions.insert(profile.extension));
            assert_eq!(profile.label, label);
            assert_eq!(profile.container, container);
            assert_eq!(profile.output_muxer, muxer);
            assert_eq!(profile.mux_attempts().len(), attempts);
        }
        assert_eq!(extensions.len(), FORMAT_PROFILES.len());
        assert_eq!(
            get_supported_formats(),
            FORMAT_PROFILES
                .iter()
                .map(|profile| SupportedFormatView {
                    extension: profile.extension.to_string(),
                    label: profile.label.to_string(),
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn ffmpeg_options_are_container_specific_and_never_transcode() {
        let every_option = [
            CleaningOptions::default(),
            CleaningOptions {
                remove_subtitles: true,
            },
        ];
        for profile in FORMAT_PROFILES {
            for (attempt, options) in profile
                .mux_attempts()
                .iter()
                .flat_map(|attempt| every_option.map(|options| (attempt, options)))
            {
                let args = ffmpeg_args(
                    Path::new("input.video"),
                    Path::new("output.video"),
                    profile,
                    *attempt,
                    options,
                );
                // Detected cover art goes on every profile and with every
                // option; subtitles only when asked. Neither is ever selected
                // by index or by a broad video remap.
                assert_eq!(
                    args.windows(2)
                        .filter(|pair| pair[0] == "-map" && pair[1] == "-0:disp:attached_pic")
                        .count(),
                    1,
                    "{}: detected cover art is not excluded exactly once",
                    profile.extension
                );
                assert_eq!(
                    args.iter().any(|arg| arg == "-sn"),
                    options.remove_subtitles,
                    "{}: subtitle removal does not follow the option",
                    profile.extension
                );
                let maps: Vec<&String> = args
                    .windows(2)
                    .filter(|pair| pair[0] == "-map")
                    .map(|pair| &pair[1])
                    .collect();
                assert_eq!(
                    maps,
                    ["0", "-0:t?", "-0:disp:attached_pic"],
                    "{}: unexpected stream selection",
                    profile.extension
                );
                let movflags = args.iter().position(|arg| arg == "-movflags");
                assert_eq!(
                    movflags.is_some(),
                    *attempt == MuxAttempt::FastStart,
                    "unexpected MOV flags for {} {attempt:?}",
                    profile.extension
                );
                assert_eq!(
                    args.iter().any(|arg| arg == "-ignore_unknown"),
                    profile.container == ContainerProfile::Avi,
                    "unexpected unknown-stream policy for {}",
                    profile.extension
                );
                // Attachments are excluded on every profile, not only on the
                // one container that can carry them: a negative map is inert
                // where there is nothing to match, and making it conditional
                // would be a second rule to keep in step with the muxers.
                let pair_at = |first: &str, second: &str| {
                    args.windows(2)
                        .position(|pair| pair[0] == first && pair[1] == second)
                };
                let map_all = pair_at("-map", "0")
                    .unwrap_or_else(|| panic!("{} maps no streams", profile.extension));
                let drop_attachments = pair_at("-map", "-0:t?").unwrap_or_else(|| {
                    panic!("{} does not exclude attachment streams", profile.extension)
                });
                assert!(
                    drop_attachments > map_all,
                    "{}: the attachment exclusion must follow `-map 0`",
                    profile.extension
                );

                let codec = args.iter().position(|arg| arg == "-c").unwrap();
                assert_eq!(args[codec + 1], "copy");
                // `-c copy` is the only codec operation: no per-stream codec,
                // no second `-c`, nothing an encoder could hide behind.
                assert_eq!(
                    args.iter()
                        .filter(|arg| arg.starts_with("-c") || arg.ends_with("codec"))
                        .count(),
                    1,
                    "{}: more than one codec argument",
                    profile.extension
                );
                let muxer = args.iter().position(|arg| arg == "-f").unwrap();
                assert_eq!(args[muxer + 1], profile.output_muxer);
                assert!(!args.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "libx264" | "libx265" | "libvpx" | "libopus" | "aac"
                    )
                }));
            }
        }
        assert_eq!(format_profile("m4v").unwrap().output_muxer, "mp4");
    }

    /// The upgrade contract, literally: the default argument list is exactly
    /// v0.5.0's plus the one pair that removes detected cover art, placed after
    /// the attachment exclusion. Anything else changing here changes what every
    /// existing user's files come out as.
    #[test]
    fn the_default_argv_is_v050_plus_detected_cover_art_removal() {
        let v050 = |format: &str, avi: bool, faststart: bool| {
            let mut args: Vec<&str> = vec!["-n"];
            if avi {
                args.push("-ignore_unknown");
            }
            args.extend([
                "-i",
                "input.video",
                "-map",
                "0",
                "-map",
                "-0:t?",
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
            ]);
            if faststart {
                args.extend(["-movflags", "+faststart"]);
            }
            args.extend(["-f", format, "output.video"]);
            args.into_iter().map(str::to_string).collect::<Vec<_>>()
        };
        let with_cover_removal = |mut args: Vec<String>| {
            let at = args.iter().position(|arg| arg == "-0:t?").unwrap() + 1;
            args.insert(at, "-0:disp:attached_pic".into());
            args.insert(at, "-map".into());
            args
        };

        for profile in FORMAT_PROFILES {
            for attempt in profile.mux_attempts() {
                let expected = with_cover_removal(v050(
                    profile.output_muxer,
                    profile.container == ContainerProfile::Avi,
                    *attempt == MuxAttempt::FastStart,
                ));
                let args = ffmpeg_args(
                    Path::new("input.video"),
                    Path::new("output.video"),
                    profile,
                    *attempt,
                    CleaningOptions::default(),
                );
                assert_eq!(args, expected, "{} {attempt:?}", profile.extension);

                // Subtitle removal adds `-sn` and nothing else.
                let mut with_subtitles_removed = ffmpeg_args(
                    Path::new("input.video"),
                    Path::new("output.video"),
                    profile,
                    *attempt,
                    CleaningOptions {
                        remove_subtitles: true,
                    },
                );
                let sn = with_subtitles_removed
                    .iter()
                    .position(|arg| arg == "-sn")
                    .unwrap();
                with_subtitles_removed.remove(sn);
                assert_eq!(with_subtitles_removed, expected, "{}", profile.extension);
            }
        }
    }

    #[test]
    fn unsupported_error_is_generated_from_the_real_matrix() {
        let dir = scratch("unsupported");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = dir.join("clip.flv");
        std::fs::write(&input, b"not inspected because the extension is rejected").unwrap();
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();

        let error = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap_err();

        assert_eq!(
            error,
            "Unsupported file type. Supported: MP4, MOV, M4V, MKV, WebM, AVI."
        );
        assert!(std::fs::read_dir(&out).unwrap().next().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn incompatible_stream_copy_fails_without_transcoding_or_partial_output() {
        let dir = scratch("incompatible-webm");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        // A valid Matroska file carrying MPEG-4/AAC, deliberately given a .webm
        // extension. FFmpeg can read it, but the WebM muxer must reject those
        // codecs. The production path must report that incompatibility, never
        // make it pass by encoding VP8/VP9 + Opus/Vorbis.
        let matroska = sample_for_format(&dir, "mkv");
        let input = dir.join("incompatible.webm");
        std::fs::copy(&matroska, &input).unwrap();
        assert_ffmpeg_can_read(&input);
        let before = std::fs::read(&input).unwrap();

        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        let error = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap_err();

        assert!(
            error.contains("Stream copy failed for this WebM file"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("No video or audio re-encoding was attempted"),
            "no no-transcode guarantee in error: {error}"
        );
        assert_eq!(std::fs::read(&input).unwrap(), before);
        assert!(std::fs::read_dir(&out).unwrap().next().is_none());
        assert!(temp_files(&out).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn raw_mpeg4_m4v_is_not_misrepresented_as_iso_bmff() {
        let dir = scratch("raw-m4v");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = dir.join("raw.m4v");
        let built = ffmpeg()
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=10:duration=1",
                "-an",
                "-c:v",
                "mpeg4",
                "-f",
                "m4v",
            ])
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "could not build raw M4V: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        assert_ffmpeg_can_read(&input);
        assert!(!is_iso_bmff(&input).unwrap());
        let before = std::fs::read(&input).unwrap();
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();

        let error = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap_err();

        assert!(
            error.contains("Unsupported M4V variant"),
            "unexpected error: {error}"
        );
        assert!(error.contains("raw MPEG-4 video streams are not accepted"));
        assert_eq!(std::fs::read(&input).unwrap(), before);
        assert!(std::fs::read_dir(&out).unwrap().next().is_none());
        assert!(registry.used.is_empty(), "invalid M4V burned an output ID");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mp4_format_regression() {
        assert_format_cleaning("mp4");
    }

    #[test]
    fn mov_format_regression() {
        assert_format_cleaning("mov");
    }

    #[test]
    fn m4v_iso_bmff_format_regression() {
        assert_format_cleaning("m4v");
    }

    #[test]
    fn mkv_format_regression() {
        assert_format_cleaning("mkv");
    }

    #[test]
    fn webm_format_regression() {
        assert_format_cleaning("webm");
    }

    #[test]
    fn avi_format_regression() {
        assert_format_cleaning("avi");
    }

    #[test]
    fn prefix_is_sanitized() {
        assert_eq!(sanitize_prefix("  CLIP  ").unwrap(), "CLIP");
        // The shipped default must survive its own sanitiser.
        assert_eq!(sanitize_prefix(DEFAULT_PREFIX).unwrap(), DEFAULT_PREFIX);
        assert_eq!(sanitize_prefix("my reels").unwrap(), "my reels");
        // Separators and dots are stripped, so a prefix can never walk the path.
        assert_eq!(sanitize_prefix("..\\..\\evil").unwrap(), "evil");
        assert_eq!(sanitize_prefix("C:/x").unwrap(), "Cx");
        assert!(sanitize_prefix("   ").is_err());
        assert!(sanitize_prefix("/\\..").is_err());
        assert_eq!(
            sanitize_prefix(&"A".repeat(200)).unwrap().len(),
            MAX_PREFIX_LEN
        );
    }

    #[test]
    fn ids_are_always_ten_digits() {
        assert_eq!(format_id(0), "0000000000");
        assert_eq!(format_id(17_293_845), "0017293845");
        assert_eq!(format_id(ID_MODULO - 1), "9999999999");
        for _ in 0..2000 {
            let id = format_id(rand::random_range(0..ID_MODULO));
            assert_eq!(id.len(), 10);
            assert!(id.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn a_recorded_id_is_never_handed_out_again() {
        let dir = scratch("reuse");
        let registry_path = dir.join("used-ids.txt");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let mut first = IdRegistry::open(&registry_path).unwrap();
        let mut issued = HashSet::new();
        for _ in 0..200 {
            assert!(issued.insert(first.reserve(&out, "CLIP", "mp4").unwrap()));
        }
        drop(first);

        // A fresh registry reads the file back and still avoids every earlier ID.
        let mut reopened = IdRegistry::open(&registry_path).unwrap();
        assert_eq!(reopened.used.len(), 200);
        for _ in 0..200 {
            let id = reopened.reserve(&out, "CLIP", "mp4").unwrap();
            assert!(!issued.contains(&id), "reused id {id} after restart");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_id_whose_file_already_exists_is_rejected() {
        let dir = scratch("collision");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();

        // Same name already on disk but unknown to the registry: still refused.
        std::fs::write(out.join("CLIP_0000000007.mp4"), b"x").unwrap();
        assert!(!registry.is_free(&out, "CLIP", "mp4", 7));
        assert!(registry.is_free(&out, "CLIP", "mp4", 8));

        // A different extension is a different file name, so it stays free.
        assert!(registry.is_free(&out, "CLIP", "mov", 7));
        // ...and a different prefix likewise.
        assert!(registry.is_free(&out, "REELS", "mp4", 7));

        // Known to the registry but absent from disk: also refused.
        let issued = registry.reserve(&out, "CLIP", "mp4").unwrap();
        std::fs::write(out.join(format!("CLIP_{}.mp4", format_id(issued))), b"x").unwrap();
        std::fs::remove_file(out.join(format!("CLIP_{}.mp4", format_id(issued)))).unwrap();
        assert!(!registry.is_free(&out, "CLIP", "mp4", issued));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn batch_size_is_enforced_by_the_backend() {
        let dir = scratch("limit");
        let over: Vec<String> = (0..MAX_BATCH + 1).map(|i| format!("v{i}.mp4")).collect();
        let err = run_batch(
            &over,
            "CLIP",
            &dir,
            &dir,
            1,
            CleaningOptions::default(),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("101"), "unexpected message: {err}");
        assert!(err.contains(&MAX_BATCH.to_string()));

        let err = run_batch(
            &[],
            "CLIP",
            &dir,
            &dir,
            1,
            CleaningOptions::default(),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, "No videos selected");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn success_renames_the_temp_file_and_leaves_the_original_alone() {
        let dir = scratch("success");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = sample_video(&dir, "reel.mp4");
        let before = std::fs::read(&input).unwrap();

        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        let name = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap();

        assert!(name.starts_with("CLIP_") && name.ends_with(".mp4"));

        // The shipped default produces the documented shape too.
        let defaulted = clean_one(
            &input,
            &out,
            DEFAULT_PREFIX,
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap();
        assert!(defaulted.starts_with("VIDEO_") && defaulted.ends_with(".mp4"));
        assert_eq!(defaulted.len(), "VIDEO_".len() + 10 + ".mp4".len());
        assert!(out.join(&defaulted).is_file());

        let digits = &name["CLIP_".len()..name.len() - 4];
        assert_eq!(digits.len(), 10);
        assert!(out.join(&name).is_file());

        // Nothing half-written left behind, and the source is byte-identical.
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temp file survived a success");
        assert_eq!(std::fs::read(&input).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Regression: `-map 0` copies data tracks as well, so timed-metadata streams
    /// (GoPro `gpmd`, iPhone `mebx`, chapter text tracks) used to reach the output
    /// with their payload intact even though every tag around them was stripped.
    /// `-dn` drops them. Remove `-dn` from `ffmpeg_args` and this test fails on the
    /// canary.
    #[test]
    fn data_streams_are_dropped_while_video_and_audio_survive() {
        let dir = scratch("data-streams");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = sample_video_with_data_track(&dir, "reel.mp4");
        let before = std::fs::read(&input).unwrap();

        // The fixture is worth nothing unless it really carries a data stream whose
        // payload is findable in the bytes.
        let kinds = stream_kinds(&input);
        assert_eq!(
            kinds,
            ["Video", "Audio", "Data"],
            "unexpected fixture: {kinds:?}"
        );
        assert!(
            contains(&before, DATA_CANARY),
            "the fixture lost its data payload; the test would prove nothing"
        );

        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        let name = clean_one(
            &input,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry,
        )
        .unwrap();
        let cleaned = out.join(&name);

        // The data track is gone; video and audio are not.
        let kinds = stream_kinds(&cleaned);
        assert_eq!(kinds, ["Video", "Audio"], "data stream survived: {kinds:?}");

        // And so is what it carried.
        assert!(
            !contains(&std::fs::read(&cleaned).unwrap(), DATA_CANARY),
            "the data payload is still recoverable from the cleaned file"
        );

        // Still lossless: the streams that remain are byte-identical to the source.
        let hash = |path: &Path, stream: &str| -> String {
            let probe = ffmpeg()
                .args(["-v", "error", "-i"])
                .arg(path)
                .args(["-map", stream, "-c", "copy", "-f", "md5", "-"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&probe.stdout).trim().to_string()
        };
        let video = hash(&input, "0:v:0");
        let audio = hash(&input, "0:a:0");
        assert!(
            !video.is_empty() && !audio.is_empty(),
            "could not hash the source"
        );
        assert_eq!(
            video,
            hash(&cleaned, "0:v:0"),
            "video was not copied verbatim"
        );
        assert_eq!(
            audio,
            hash(&cleaned, "0:a:0"),
            "audio was not copied verbatim"
        );

        // Tags and chapters are still stripped.
        let probe = ffmpeg()
            .args(["-v", "error", "-i"])
            .arg(&cleaned)
            .args(["-f", "ffmetadata", "-"])
            .output()
            .unwrap();
        let meta = String::from_utf8_lossy(&probe.stdout).to_lowercase();
        assert!(!meta.contains("secret"), "metadata survived: {meta}");
        assert!(!meta.contains("[chapter]"), "chapters survived: {meta}");

        // The original is untouched and nothing half-written is left behind.
        assert_eq!(
            std::fs::read(&input).unwrap(),
            before,
            "the original changed"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temp file survived a success");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failure_removes_the_temp_file_and_publishes_nothing() {
        let dir = scratch("failure");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let broken = dir.join("broken.mp4");
        std::fs::write(&broken, b"this is not a video").unwrap();

        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        assert!(clean_one(
            &broken,
            &out,
            "CLIP",
            CleaningOptions::default(),
            &mut registry
        )
        .is_err());

        assert_eq!(
            std::fs::read_dir(&out).unwrap().count(),
            0,
            "a failed conversion left files in the output folder"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_temp_files_are_cleared_but_real_outputs_are_kept() {
        let dir = scratch("stale");
        std::fs::write(dir.join(format!("{TEMP_PREFIX}0000000001.mp4")), b"partial").unwrap();
        std::fs::write(dir.join("CLIP_0000000002.mp4"), b"finished").unwrap();
        std::fs::write(dir.join("something-else.mp4"), b"user file").unwrap();

        remove_stale_temp_files(&dir);

        assert!(!dir.join(format!("{TEMP_PREFIX}0000000001.mp4")).exists());
        assert!(dir.join("CLIP_0000000002.mp4").exists());
        assert!(dir.join("something-else.mp4").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The scenario the app actually runs: a mixed batch where one file is junk.
    #[test]
    fn a_mixed_batch_renames_every_good_file_and_isolates_the_bad_one() {
        let dir = scratch("batch");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let mut inputs = Vec::new();
        for i in 1..=5 {
            inputs.push(sample_video(&dir, &format!("reel{i}.mp4")));
        }
        // A MOV, remuxed from one of the MP4s so both containers are covered.
        let mov = dir.join("clip.mov");
        assert!(ffmpeg()
            .args(["-y", "-i"])
            .arg(&inputs[0])
            .args(["-c", "copy"])
            .arg(&mov)
            .output()
            .unwrap()
            .status
            .success());
        inputs.push(mov);

        let broken = dir.join("broken.mp4");
        std::fs::write(&broken, b"this is not a video").unwrap();
        inputs.push(broken.clone());

        let before: Vec<Vec<u8>> = inputs.iter().map(|p| std::fs::read(p).unwrap()).collect();
        let paths: Vec<String> = inputs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        let mut seen_processing = 0;
        let summary = run_batch(
            &paths,
            "CLIP",
            &out,
            &dir,
            1,
            CleaningOptions::default(),
            |p| {
                if p.status == "processing" {
                    seen_processing += 1;
                }
            },
        )
        .unwrap();

        assert_eq!(summary.completed, 6, "{:?}", summary.results);
        assert_eq!(summary.errors, 1);
        assert_eq!(seen_processing, 7, "every file should report a start");

        // Naming, uniqueness and extension preservation.
        let mut ids = HashSet::new();
        for result in &summary.results {
            match result.status {
                "completed" => {
                    let name = result.output_name.as_ref().unwrap();
                    let stem = name.rsplit_once('.').unwrap();
                    let id = stem.0.strip_prefix("CLIP_").expect("prefix missing");
                    assert_eq!(id.len(), 10, "{name}");
                    assert!(id.chars().all(|c| c.is_ascii_digit()), "{name}");
                    assert!(
                        ids.insert(id.to_string()),
                        "duplicate id in one batch: {name}"
                    );
                    assert!(out.join(name).is_file());
                    let expected_ext = if result.input_name.ends_with(".mov") {
                        "mov"
                    } else {
                        "mp4"
                    };
                    assert_eq!(stem.1, expected_ext, "{name}");
                }
                "error" => {
                    assert_eq!(result.input_name, "broken.mp4");
                    assert!(result.output_name.is_none());
                }
                other => panic!("unexpected status {other}"),
            }
        }

        // The output folder holds exactly the six finished files: no temp leftovers,
        // nothing from the failed conversion.
        let produced: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(produced.len(), 6, "{produced:?}");
        assert!(
            produced.iter().all(|n| n.starts_with("CLIP_")),
            "{produced:?}"
        );

        // Originals are untouched, byte for byte.
        for (path, original) in inputs.iter().zip(&before) {
            assert_eq!(&std::fs::read(path).unwrap(), original, "{path:?} changed");
        }

        // Every reserved ID landed in the registry file.
        let registry = std::fs::read_to_string(dir.join("used-ids.txt")).unwrap();
        let recorded: HashSet<&str> = registry.lines().map(str::trim).collect();
        assert!(recorded.len() >= ids.len());
        for id in &ids {
            assert!(recorded.contains(id.as_str()), "id {id} not persisted");
        }

        // Lossless: the copied streams hash identically to the source.
        let hash = |p: &Path| -> String {
            let out = ffmpeg()
                .args(["-v", "error", "-i"])
                .arg(p)
                .args([
                    "-map",
                    "0",
                    "-c",
                    "copy",
                    "-f",
                    "streamhash",
                    "-hash",
                    "md5",
                    "-",
                ])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let cleaned_first = summary.results[0].output_name.as_ref().unwrap();
        assert_eq!(hash(&inputs[0]), hash(&out.join(cleaned_first)));

        // And the metadata really is gone.
        let probe = ffmpeg()
            .args(["-v", "error", "-i"])
            .arg(out.join(cleaned_first))
            .args(["-f", "ffmetadata", "-"])
            .output()
            .unwrap();
        let meta = String::from_utf8_lossy(&probe.stdout).to_lowercase();
        assert!(!meta.contains("secret"), "metadata survived: {meta}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_settings_migrate_once_without_clobbering_newer_ones() {
        let root = scratch("migration");
        let legacy = root.join("com.aurevm.videocleaner");
        let current = root.join("com.metastrip.video");
        std::fs::create_dir_all(&legacy).unwrap();

        let stored = Settings {
            prefix: "REEL".into(),
            output_directory: root.to_string_lossy().into_owned(),
            ..Settings::default()
        };
        write_settings(&legacy, &stored).unwrap();
        std::fs::write(
            legacy.join("used-ids.txt"),
            "2267423415
0000000007
",
        )
        .unwrap();

        // Nothing on the new side yet: everything comes across.
        migrate_legacy_config(&current, &legacy).unwrap();
        let migrated = load_settings(&current);
        assert_eq!(migrated.prefix, "REEL");
        assert_eq!(migrated.output_directory, stored.output_directory);

        let registry = IdRegistry::open(&current.join("used-ids.txt")).unwrap();
        assert!(
            registry.used.contains(&2_267_423_415),
            "migrated id missing"
        );
        assert!(registry.used.contains(&7), "migrated id missing");
        assert!(!registry.is_free(&current, "REEL", "mp4", 2_267_423_415));
        drop(registry);

        // The originals stay put; migration copies, it does not move.
        assert!(legacy.join("settings.json").is_file());
        assert!(legacy.join("used-ids.txt").is_file());

        // Repeating it is a no-op and must not overwrite what is now current.
        let newer = Settings {
            prefix: "CLIP".into(),
            output_directory: root.to_string_lossy().into_owned(),
            ..Settings::default()
        };
        write_settings(&current, &newer).unwrap();
        std::fs::write(
            current.join("used-ids.txt"),
            "0000000042
",
        )
        .unwrap();

        migrate_legacy_config(&current, &legacy).unwrap();
        migrate_legacy_config(&current, &legacy).unwrap();
        assert_eq!(load_settings(&current).prefix, "CLIP");
        assert_eq!(
            std::fs::read_to_string(current.join("used-ids.txt"))
                .unwrap()
                .trim(),
            "0000000042"
        );

        // No legacy folder at all is fine.
        let untouched = root.join("com.example.fresh");
        migrate_legacy_config(&untouched, &root.join("does-not-exist")).unwrap();
        assert!(
            !untouched.exists(),
            "migration created a folder with nothing to migrate"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settings_round_trip_and_fall_back_when_missing() {
        let dir = scratch("settings");
        // Nothing stored yet.
        let fresh = load_settings(&dir);
        assert_eq!(fresh.prefix, DEFAULT_PREFIX);
        // Product requirement: the out-of-the-box prefix is brand-neutral.
        assert_eq!(DEFAULT_PREFIX, "VIDEO");
        assert!(fresh.output_directory.is_empty());

        let stored = Settings {
            prefix: "REELS".into(),
            output_directory: dir.to_string_lossy().into_owned(),
            ..Settings::default()
        };
        write_settings(&dir, &stored).unwrap();
        let read_back = load_settings(&dir);
        assert_eq!(read_back.prefix, "REELS");
        assert_eq!(read_back.output_directory, stored.output_directory);
        assert!(SettingsView::from(read_back).output_directory_valid);

        // A folder that has since disappeared is reported, not fatal.
        let gone = Settings {
            prefix: "REELS".into(),
            output_directory: dir.join("nope").to_string_lossy().into_owned(),
            ..Settings::default()
        };
        write_settings(&dir, &gone).unwrap();
        assert!(!SettingsView::from(load_settings(&dir)).output_directory_valid);

        // Corrupt file: defaults rather than a crash.
        std::fs::write(settings_path(&dir), "{ not json").unwrap();
        assert_eq!(load_settings(&dir).prefix, DEFAULT_PREFIX);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ------------------------------------------------ v0.5.1 settings ---

    /// Exactly what a v0.5.0 install has on disk: `to_string_pretty` of the old
    /// two-field struct.
    const V050_SETTINGS: &str =
        "{\n  \"prefix\": \"VIDEO\",\n  \"outputDirectory\": \"D:\\\\Clean videos\"\n}";

    #[test]
    fn a_literal_v050_settings_file_keeps_its_prefix_and_folder() {
        let dir = scratch("settings-v050");
        std::fs::write(settings_path(&dir), V050_SETTINGS).unwrap();

        let loaded = load_settings(&dir);

        assert_eq!(loaded.prefix, "VIDEO");
        assert_eq!(loaded.output_directory, r"D:\Clean videos");
        assert!(!loaded.cleaning.remove_subtitles);
        assert_eq!(loaded.cleaning, CleaningOptions::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_cleaning_value_never_costs_the_prefix_or_folder() {
        let dir = scratch("settings-bad-cleaning");
        for cleaning in [
            "null",
            "\"x\"",
            "7",
            "true",
            "[]",
            "{\"removeSubtitles\": \"yes\"}",
            "{\"removeSubtitles\": 1}",
            "{\"removeSubtitles\": null}",
        ] {
            std::fs::write(
                settings_path(&dir),
                format!(
                    "{{\"prefix\": \"REELS\", \"outputDirectory\": \"D:\\\\Out\", \"cleaning\": {cleaning}}}"
                ),
            )
            .unwrap();

            let loaded = load_settings(&dir);

            assert_eq!(loaded.prefix, "REELS", "cleaning = {cleaning}");
            assert_eq!(loaded.output_directory, r"D:\Out", "cleaning = {cleaning}");
            assert_eq!(
                loaded.cleaning,
                CleaningOptions::default(),
                "cleaning = {cleaning}"
            );
        }

        // A value from a later version: the part this one understands is
        // kept, and so is the rest of the file, unknown keys included.
        std::fs::write(
            settings_path(&dir),
            r#"{"prefix":"REELS","outputDirectory":"D:\\Out","cleaning":{"removeSubtitles":true,"futureChoice":3},"futureSetting":"x"}"#,
        )
        .unwrap();
        let loaded = load_settings(&dir);
        assert_eq!(loaded.prefix, "REELS");
        assert!(loaded.cleaning.remove_subtitles);

        // A document that is not JSON at all still falls back whole, as v0.5.0 did.
        std::fs::write(settings_path(&dir), "{ not json").unwrap();
        assert_eq!(load_settings(&dir).prefix, DEFAULT_PREFIX);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_save_keeps_what_it_does_not_change() {
        let root = scratch("settings-preserve");
        let app = root.join("app");
        let folder = root.join("out");
        let other_folder = root.join("elsewhere");
        let remove_subtitles = CleaningOptions {
            remove_subtitles: true,
        };

        store_output_settings(&app, "REELS", &folder.to_string_lossy()).unwrap();
        store_cleaning_options(&app, remove_subtitles).unwrap();

        // Changing the prefix keeps the cleaning choice and the folder.
        let saved = store_output_settings(&app, "CLIPS", &folder.to_string_lossy()).unwrap();
        assert!(saved.cleaning.remove_subtitles);
        let loaded = load_settings(&app);
        assert_eq!(loaded.prefix, "CLIPS");
        assert!(
            loaded.cleaning.remove_subtitles,
            "a prefix save erased the option"
        );

        // Changing the folder keeps it too.
        store_output_settings(&app, "CLIPS", &other_folder.to_string_lossy()).unwrap();
        let loaded = load_settings(&app);
        assert_eq!(loaded.output_directory, other_folder.to_string_lossy());
        assert!(
            loaded.cleaning.remove_subtitles,
            "a folder save erased the option"
        );

        // Changing the cleaning choice keeps prefix and folder.
        store_cleaning_options(&app, CleaningOptions::default()).unwrap();
        let loaded = load_settings(&app);
        assert_eq!(loaded.prefix, "CLIPS");
        assert_eq!(loaded.output_directory, other_folder.to_string_lossy());
        assert!(!loaded.cleaning.remove_subtitles);

        // What the page is shown carries the choice as well.
        let view = SettingsView::from(loaded);
        assert!(view.output_directory_valid);
        assert!(!view.cleaning.remove_subtitles);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settings_are_replaced_whole_and_leave_no_temporary_file() {
        let dir = scratch("settings-atomic");
        // A leftover from a write that died part-way must not get in the way.
        std::fs::write(dir.join(SETTINGS_TEMP), "{ half a fi").unwrap();
        std::fs::write(settings_path(&dir), V050_SETTINGS).unwrap();

        let saved = store_cleaning_options(
            &dir,
            CleaningOptions {
                remove_subtitles: true,
            },
        )
        .unwrap();

        assert!(
            !dir.join(SETTINGS_TEMP).exists(),
            "the temporary file was left"
        );
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path(&dir)).unwrap()).unwrap();
        assert_eq!(on_disk["prefix"], "VIDEO");
        assert_eq!(on_disk["outputDirectory"], r"D:\Clean videos");
        assert_eq!(on_disk["cleaning"]["removeSubtitles"], true);
        assert_eq!(load_settings(&dir).output_directory, saved.output_directory);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The counts the page builds its warnings from: detected cover art apart
    /// from footage, subtitles counted, and no execution plan in the scan.
    #[test]
    fn the_scan_counts_subtitles_and_detected_cover_art() {
        let dir = scratch("scan-counts");
        let cover = cover_jpeg(&dir, false);
        let input = sample_with_cover(&dir, "mp4", &cover);

        let view = scan_one(&input.to_string_lossy());

        assert!(view.ok, "{:?}", view.error);
        assert_eq!(view.video_streams, 1, "the cover was counted as footage");
        assert_eq!(view.cover_images, 1);
        assert_eq!(view.subtitle_streams, 1);
        assert_eq!(view.audio_streams, 1);
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["coverImages"], 1);
        assert_eq!(json["subtitleStreams"], 1);
        assert!(json.get("plan").is_none(), "the scan still sends a plan");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
