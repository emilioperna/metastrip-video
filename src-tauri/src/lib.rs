use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

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

use plan::CleaningPlan;
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    prefix: String,
    /// Empty until the user picks one.
    output_directory: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            prefix: DEFAULT_PREFIX.to_string(),
            output_directory: String::new(),
        }
    }
}

/// What the UI needs: the stored values plus whether the folder is still usable.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    prefix: String,
    output_directory: String,
    output_directory_valid: bool,
}

impl From<Settings> for SettingsView {
    fn from(s: Settings) -> Self {
        let valid = !s.output_directory.is_empty() && Path::new(&s.output_directory).is_dir();
        SettingsView {
            prefix: s.prefix,
            output_directory: s.output_directory,
            output_directory_valid: valid,
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

fn write_settings(app_dir: &Path, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(app_dir)
        .map_err(|e| format!("Could not create the settings folder: {e}"))?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(app_dir), json)
        .map_err(|e| format!("Could not save settings: {e}"))
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
    results: Vec<FileResult>,
}

fn ffmpeg_args(
    input: &Path,
    output: &Path,
    format: FormatProfile,
    attempt: MuxAttempt,
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
        // and all.
        "-dn".into(),
        "-fflags".into(),
        "+bitexact".into(),
    ]);
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

fn last_ffmpeg_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let message = text
        .lines()
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    if message.is_empty() {
        "FFmpeg failed".into()
    } else {
        message.to_string()
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
            .args(ffmpeg_args(input, &temp_path, format, attempt))
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

        last_error = last_ffmpeg_error(&result.stderr);
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
    video_streams: usize,
    audio_streams: usize,
    other_streams: usize,
    chapter_count: usize,
    field_count: usize,
    plan: Option<CleaningPlan>,
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
        other_streams: 0,
        chapter_count: 0,
        field_count: 0,
        plan: None,
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

    let format = match validate_input(&path) {
        Ok(format) => format,
        Err(error) => return failed_scan(raw, name, error),
    };
    let report = match inspect::inspect(&path) {
        Ok(report) => report,
        Err(error) => return failed_scan(raw, name, error),
    };

    let findings = privacy::classify(&report);
    let (video, audio, other) = privacy::stream_kind_counts(&report);
    let plan = plan::plan_for(&report, &findings, format);

    ScanView {
        path: raw.to_string(),
        name,
        ok: true,
        error: None,
        summary: PrivacySummary::of(&findings),
        findings,
        container: report.container.clone(),
        duration_seconds: report.duration_seconds,
        video_streams: video,
        audio_streams: audio,
        other_streams: other,
        chapter_count: report.chapters.len(),
        field_count: report.fields.len(),
        plan: Some(plan),
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
    registry: &mut IdRegistry,
) -> (Result<String, String>, Option<VerificationReport>) {
    let fingerprint = OriginalFingerprint::capture(input);
    let before = inspect::inspect(input).ok();
    let before_findings = before.as_ref().map(privacy::classify);

    let cleaned = clean_one(input, output_dir, prefix, registry);
    let Ok(output_name) = &cleaned else {
        return (cleaned, None);
    };

    let (Some(before), Some(before_findings)) = (before.as_ref(), before_findings.as_ref()) else {
        // Cleaned, but there is no baseline to check it against. Reported as
        // unverified rather than quietly treated as verified.
        return (cleaned, None);
    };
    let Ok(format) = validate_input(input) else {
        return (cleaned, None);
    };

    let plan = plan::plan_for(before, before_findings, format);
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
fn run_batch(
    paths: &[String],
    prefix: &str,
    output_dir: &Path,
    app_dir: &Path,
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

    for (index, raw) in paths.iter().enumerate() {
        let input = PathBuf::from(raw);
        let input_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());

        on_progress(Progress {
            index,
            total,
            input_name: input_name.clone(),
            output_name: None,
            status: "processing",
            message: None,
            verification: None,
        });

        let (outcome, verification) = clean_and_verify(&input, output_dir, prefix, &mut registry);

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
        results,
    })
}

/// A short, safe description of why a file is not verified. Only the names of
/// the checks that failed and their details, all of which are generated by this
/// app rather than copied out of the media file.
fn failed_check_summary(report: &VerificationReport) -> String {
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
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
#[tauri::command]
fn check_ffmpeg() -> Option<&'static str> {
    if !ffmpeg_available() {
        return Some(FFMPEG_MISSING);
    }
    if !inspect::available() {
        return Some(FFPROBE_MISSING);
    }
    None
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
    let dir = app_dir(&app)?;
    let prefix = sanitize_prefix(&prefix)?;

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

    let settings = Settings {
        prefix,
        output_directory,
    };
    write_settings(&dir, &settings)?;
    Ok(settings.into())
}

// Shown instead of a panic message, which can carry file paths.
const SCAN_INTERNAL_ERROR: &str = "The privacy scan stopped because of an internal error.";
const CLEAN_INTERNAL_ERROR: &str =
    "Cleaning stopped because of an internal error. Files finished before it are in the output folder.";

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

const CLEAN_BUSY: &str = "A cleaning batch is already running. Wait for it to finish.";

/// Set while a Clean batch runs.
///
/// When the commands ran on the window's thread, that thread kept batches
/// apart for free. Off it, a page reload mid-batch leaves the old batch running
/// and lets a new one start in the same folder, where its stale-temp sweep and
/// the "No temporary files left" check would trip over the first batch's file.
/// This keeps the batches' disk work apart; it does not reconnect a reloaded
/// page to the batch still running.
static CLEAN_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Holds a flag for as long as it lives. Released by `Drop`, so a batch that
/// ends in an error or a panic frees it too.
struct BatchGuard(&'static AtomicBool);

impl BatchGuard {
    /// Never waits: a second batch is refused, not queued.
    fn try_acquire(flag: &'static AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| BatchGuard(flag))
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Everything `clean_videos` does once the app data folder is known, minus
/// Tauri, so it can be tested as is. `busy` is taken before anything touches
/// the disk and held until the last file is verified.
fn clean_request(
    busy: &'static AtomicBool,
    app_dir: &Path,
    paths: &[String],
    on_progress: impl FnMut(Progress),
) -> Result<CleanSummary, String> {
    let _batch = BatchGuard::try_acquire(busy).ok_or_else(|| CLEAN_BUSY.to_string())?;
    let settings = load_settings(app_dir);
    if settings.output_directory.is_empty() {
        return Err("Choose an output folder first.".into());
    }
    let output_dir = PathBuf::from(&settings.output_directory);

    run_batch(paths, &settings.prefix, &output_dir, app_dir, on_progress)
}

/// Clean, verify and report a batch, one file after another, emitting
/// `clean-progress` as each file starts and as each one finishes.
#[tauri::command]
async fn clean_videos(app: AppHandle, paths: Vec<String>) -> Result<CleanSummary, String> {
    run_blocking(CLEAN_INTERNAL_ERROR, move || {
        let dir = app_dir(&app)?;
        clean_request(&CLEAN_IN_PROGRESS, &dir, &paths, |progress| {
            let _ = app.emit("clean-progress", progress);
        })
    })
    .await
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
        .invoke_handler(tauri::generate_handler![
            check_ffmpeg,
            get_supported_formats,
            get_settings,
            save_settings,
            scan_videos,
            clean_videos,
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
        if let Some(data_stream_kind) = capabilities.data_stream_kind {
            assert!(
                input_kinds.contains(&data_stream_kind.to_string()),
                "{extension} fixture lacks its expected data stream: {input_kinds:?}"
            );
        }
        if extension == "avi" {
            assert!(
                contains(&before, DATA_CANARY),
                "AVI fixture lacks its data payload canary"
            );
        }

        let source_video_hash = stream_payload_hash(&input, "0:v:0");
        let source_audio_hash = stream_payload_hash(&input, "0:a:0");
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();
        let output_name = clean_one(&input, &out, "CLIP", &mut registry).unwrap();
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
        assert!(!output_kinds.contains(&"Data".to_string()));
        assert!(
            !output_kinds.contains(&"Unknown".to_string()),
            "{extension} output still has an unknown/data stream"
        );
        if extension == "avi" {
            assert!(
                !contains(&std::fs::read(&cleaned).unwrap(), DATA_CANARY),
                "AVI data payload survived"
            );
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
        for profile in FORMAT_PROFILES {
            for attempt in profile.mux_attempts() {
                let args = ffmpeg_args(
                    Path::new("input.video"),
                    Path::new("output.video"),
                    profile,
                    *attempt,
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
                let codec = args.iter().position(|arg| arg == "-c").unwrap();
                assert_eq!(args[codec + 1], "copy");
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

    #[test]
    fn unsupported_error_is_generated_from_the_real_matrix() {
        let dir = scratch("unsupported");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let input = dir.join("clip.flv");
        std::fs::write(&input, b"not inspected because the extension is rejected").unwrap();
        let mut registry = IdRegistry::open(&dir.join("used-ids.txt")).unwrap();

        let error = clean_one(&input, &out, "CLIP", &mut registry).unwrap_err();

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
        let error = clean_one(&input, &out, "CLIP", &mut registry).unwrap_err();

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

        let error = clean_one(&input, &out, "CLIP", &mut registry).unwrap_err();

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
        let err = run_batch(&over, "CLIP", &dir, &dir, |_| {}).unwrap_err();
        assert!(err.contains("101"), "unexpected message: {err}");
        assert!(err.contains(&MAX_BATCH.to_string()));

        let err = run_batch(&[], "CLIP", &dir, &dir, |_| {}).unwrap_err();
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
        let name = clean_one(&input, &out, "CLIP", &mut registry).unwrap();

        assert!(name.starts_with("CLIP_") && name.ends_with(".mp4"));

        // The shipped default produces the documented shape too.
        let defaulted = clean_one(&input, &out, DEFAULT_PREFIX, &mut registry).unwrap();
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
        let name = clean_one(&input, &out, "CLIP", &mut registry).unwrap();
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
        assert!(clean_one(&broken, &out, "CLIP", &mut registry).is_err());

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
        let summary = run_batch(&paths, "CLIP", &out, &dir, |p| {
            if p.status == "processing" {
                seen_processing += 1;
            }
        })
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
        };
        write_settings(&dir, &gone).unwrap();
        assert!(!SettingsView::from(load_settings(&dir)).output_directory_valid);

        // Corrupt file: defaults rather than a crash.
        std::fs::write(settings_path(&dir), "{ not json").unwrap();
        assert_eq!(load_settings(&dir).prefix, DEFAULT_PREFIX);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
