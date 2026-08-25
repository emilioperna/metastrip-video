use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

#[cfg(debug_assertions)]
const FFMPEG_MISSING: &str = "FFmpeg was not found. Install FFmpeg and restart the app.";
#[cfg(not(debug_assertions))]
const FFMPEG_MISSING: &str =
    "FFmpeg was not found in the application folder. Reinstall the application.";

// Functional defaults. Deliberately brand-neutral: nothing here encodes who ships
// the app, so a fork can keep every value as-is. Product identity lives in
// `tauri.conf.json` (product name, identifier, publisher) and nowhere else.

/// Starting file-name prefix. The user can change it and the choice is persisted.
const DEFAULT_PREFIX: &str = "VIDEO";
/// Marks an output that FFmpeg is still writing. Renamed into place on success.
const TEMP_PREFIX: &str = ".video-cleaner-processing-";

const SUPPORTED_EXTENSIONS: [&str; 2] = ["mp4", "mov"];
const MAX_PREFIX_LEN: usize = 64;
const MAX_BATCH: usize = 100;

// ---------------------------------------------------------------- FFmpeg ---

/// Locate the bundled FFmpeg. `externalBin` installs the sidecar next to the app
/// executable, so a packaged install never depends on the system PATH.
fn ffmpeg_program() -> &'static PathBuf {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));

        if let Some(dir) = &dir {
            // Installed layout first, then the target-triple name the Tauri CLI
            // uses in the target dir during development.
            let names = ["ffmpeg.exe", concat!("ffmpeg-", env!("TARGET_TRIPLE"), ".exe")];
            if let Some(found) = names.iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
                return found;
            }
        }

        // Dev convenience only: fall back to whatever is on PATH. A release build
        // stays pinned to its own folder so a broken install fails loudly instead
        // of silently picking up some other FFmpeg.
        #[cfg(debug_assertions)]
        {
            PathBuf::from("ffmpeg")
        }
        #[cfg(not(debug_assertions))]
        {
            dir.unwrap_or_default().join("ffmpeg.exe")
        }
    })
}

/// Build an `ffmpeg` invocation. Arguments are always passed as a real argv,
/// never through a shell, so paths cannot be interpreted as commands.
fn ffmpeg() -> Command {
    let mut command = Command::new(ffmpeg_program());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: keeps a console from flashing for every file.
        command.creation_flags(0x0800_0000);
    }
    command
}

fn ffmpeg_available() -> bool {
    ffmpeg()
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

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
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '.'))
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileResult {
    input_name: String,
    output_name: Option<String>,
    status: &'static str,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanSummary {
    output_dir: String,
    completed: usize,
    errors: usize,
    results: Vec<FileResult>,
}

fn ffmpeg_args(input: &Path, output: &Path, faststart: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        // -n, not -y: we already guarantee the target is free, so an existing file
        // means something is wrong and must not be overwritten.
        "-n".into(),
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
    ];
    if faststart {
        args.push("-movflags".into());
        args.push("+faststart".into());
    }
    args.push(output.to_string_lossy().into_owned());
    args
}

fn last_ffmpeg_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let message = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .next_back()
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

/// Cleans one video. Returns the final file name on success.
fn clean_one(
    input: &Path,
    output_dir: &Path,
    prefix: &str,
    registry: &mut IdRegistry,
) -> Result<String, String> {
    if !input.is_file() {
        return Err("File is no longer available".into());
    }
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return Err("Unsupported file type (MP4 or MOV only)".into());
    }

    let id = format_id(registry.reserve(output_dir, prefix, &ext)?);
    let final_name = format!("{prefix}_{id}.{ext}");
    let final_path = output_dir.join(&final_name);
    let temp_path = output_dir.join(format!("{TEMP_PREFIX}{id}.{ext}"));

    let run = |faststart: bool| {
        ffmpeg()
            .args(ffmpeg_args(input, &temp_path, faststart))
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    FFMPEG_MISSING.to_string()
                } else {
                    format!("Could not start FFmpeg: {e}")
                }
            })
    };

    // faststart is the preferred path; if this container rejects it, retry without
    // it. The temp file must go first or `-n` would refuse to write.
    let mut result = run(true)?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&temp_path);
        result = run(false)?;
    }
    if !result.status.success() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(last_ffmpeg_error(&result.stderr));
    }

    // The finished name only appears once the bytes are all there.
    std::fs::rename(&temp_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        format!("Could not move the cleaned file into place: {e}")
    })?;

    Ok(final_name)
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
        });

        let (status, output_name, message) = match clean_one(&input, output_dir, prefix, &mut registry) {
            Ok(name) => {
                completed += 1;
                ("completed", Some(name), None)
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
        });
        results.push(FileResult {
            input_name,
            output_name,
            status,
            message,
        });
    }

    Ok(CleanSummary {
        output_dir: output_dir.to_string_lossy().into_owned(),
        completed,
        errors,
        results,
    })
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

/// `null` when FFmpeg is usable, otherwise the message to show. Returning the
/// text from here keeps the wording in one place, since it differs between a
/// development build (PATH) and a packaged one (bundled sidecar).
#[tauri::command]
fn check_ffmpeg() -> Option<&'static str> {
    if ffmpeg_available() {
        None
    } else {
        Some(FFMPEG_MISSING)
    }
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

#[tauri::command]
fn clean_videos(app: AppHandle, paths: Vec<String>) -> Result<CleanSummary, String> {
    let dir = app_dir(&app)?;
    let settings = load_settings(&dir);
    if settings.output_directory.is_empty() {
        return Err("Choose an output folder first.".into());
    }
    let output_dir = PathBuf::from(&settings.output_directory);

    run_batch(&paths, &settings.prefix, &output_dir, &dir, |progress| {
        let _ = app.emit("clean-progress", progress);
    })
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
            get_settings,
            save_settings,
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

    /// A throwaway directory under the OS temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("video-cleaner-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tiny real MP4, so the FFmpeg path is exercised for real.
    fn sample_video(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let status = ffmpeg()
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc=size=64x64:rate=10:duration=1",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
                "-c:v", "mpeg4", "-pix_fmt", "yuv420p", "-c:a", "aac",
                "-metadata", "title=SECRET",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be available for these tests");
        assert!(
            status.status.success(),
            "could not build the sample video: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        path
    }

    /// Payload planted in the fixture's data track. Its absence from an output is
    /// the proof that the track itself is gone, not merely its tags.
    const DATA_CANARY: &[u8] = b"DATA_TRACK_CANARY";

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// A sample that really carries a data stream, which is what `-dn` has to drop.
    ///
    /// The mp4 muxer refuses a bare `bin_data` track, so the only way to get one out
    /// of a stock LGPL FFmpeg is to let `-map_chapters` build a chapter text track.
    /// The demuxer hands those samples back only when the chapters stop short of the
    /// media, hence a 7 s clip whose chapters end at 7000 ms: flush against the end
    /// the track reads back empty and the fixture would prove nothing. The test
    /// asserts the payload is present in the input for exactly that reason.
    fn sample_video_with_data_track(dir: &Path, name: &str) -> PathBuf {
        let base = dir.join("base-for-data-track.mp4");
        let built = ffmpeg()
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc=size=64x64:rate=10:duration=7",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=7",
                "-c:v", "mpeg4", "-pix_fmt", "yuv420p", "-c:a", "aac",
            ])
            .arg(&base)
            .output()
            .expect("ffmpeg must be available for these tests");
        assert!(
            built.status.success(),
            "could not build the base clip: {}",
            String::from_utf8_lossy(&built.stderr)
        );

        let chapters = dir.join("chapters.ffmetadata");
        let script = [
            ";FFMETADATA1",
            "[CHAPTER]",
            "TIMEBASE=1/1000",
            "START=0",
            "END=3000",
            "title=opening",
            "[CHAPTER]",
            "TIMEBASE=1/1000",
            "START=3000",
            "END=7000",
            "title=DATA_TRACK_CANARY",
        ]
        .join("\n");
        std::fs::write(&chapters, script).unwrap();

        let path = dir.join(name);
        let built = ffmpeg()
            .args(["-y", "-i"])
            .arg(&base)
            .arg("-i")
            .arg(&chapters)
            .args([
                "-map", "0", "-map_chapters", "1", "-c", "copy",
                "-metadata", "title=SECRET",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg must be available for these tests");
        assert!(
            built.status.success(),
            "could not build the fixture: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        path
    }

    /// The stream kinds FFmpeg reports for a file, in order, e.g. `["Video", "Audio"]`.
    fn stream_kinds(path: &Path) -> Vec<String> {
        // No output file, so FFmpeg exits non-zero after printing the input report.
        let probe = ffmpeg()
            .arg("-i")
            .arg(path)
            .output()
            .expect("ffmpeg must be available for these tests");
        String::from_utf8_lossy(&probe.stderr)
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("Stream #0:"))
            .filter_map(|line| {
                ["Video", "Audio", "Data", "Subtitle", "Attachment"]
                    .into_iter()
                    .find(|kind| line.contains(&format!(": {kind}: ")))
                    .map(str::to_string)
            })
            .collect()
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
        assert_eq!(sanitize_prefix(&"A".repeat(200)).unwrap().len(), MAX_PREFIX_LEN);
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
        assert_eq!(kinds, ["Video", "Audio", "Data"], "unexpected fixture: {kinds:?}");
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
        assert!(!video.is_empty() && !audio.is_empty(), "could not hash the source");
        assert_eq!(video, hash(&cleaned, "0:v:0"), "video was not copied verbatim");
        assert_eq!(audio, hash(&cleaned, "0:a:0"), "audio was not copied verbatim");

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
        assert_eq!(std::fs::read(&input).unwrap(), before, "the original changed");
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
                    assert!(ids.insert(id.to_string()), "duplicate id in one batch: {name}");
                    assert!(out.join(name).is_file());
                    let expected_ext =
                        if result.input_name.ends_with(".mov") { "mov" } else { "mp4" };
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
        assert!(produced.iter().all(|n| n.starts_with("CLIP_")), "{produced:?}");

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
                .args(["-map", "0", "-c", "copy", "-f", "streamhash", "-hash", "md5", "-"])
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
        std::fs::write(legacy.join("used-ids.txt"), "2267423415
0000000007
").unwrap();

        // Nothing on the new side yet: everything comes across.
        migrate_legacy_config(&current, &legacy).unwrap();
        let migrated = load_settings(&current);
        assert_eq!(migrated.prefix, "REEL");
        assert_eq!(migrated.output_directory, stored.output_directory);

        let registry = IdRegistry::open(&current.join("used-ids.txt")).unwrap();
        assert!(registry.used.contains(&2_267_423_415), "migrated id missing");
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
        std::fs::write(current.join("used-ids.txt"), "0000000042
").unwrap();

        migrate_legacy_config(&current, &legacy).unwrap();
        migrate_legacy_config(&current, &legacy).unwrap();
        assert_eq!(load_settings(&current).prefix, "CLIP");
        assert_eq!(
            std::fs::read_to_string(current.join("used-ids.txt")).unwrap().trim(),
            "0000000042"
        );

        // No legacy folder at all is fine.
        let untouched = root.join("com.example.fresh");
        migrate_legacy_config(&untouched, &root.join("does-not-exist")).unwrap();
        assert!(!untouched.exists(), "migration created a folder with nothing to migrate");

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

