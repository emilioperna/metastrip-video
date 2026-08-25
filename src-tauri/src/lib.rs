use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;

#[cfg(debug_assertions)]
const FFMPEG_MISSING: &str = "FFmpeg was not found. Install FFmpeg and restart the app.";
#[cfg(not(debug_assertions))]
const FFMPEG_MISSING: &str =
    "FFmpeg was not found in the application folder. Reinstall Aurevm Video Cleaner.";

const SUPPORTED_EXTENSIONS: [&str; 2] = ["mp4", "mov"];

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
            // copies into the target dir during development.
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    index: usize,
    total: usize,
    name: String,
    status: &'static str,
    message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileResult {
    name: String,
    status: &'static str,
    message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanSummary {
    output_dir: String,
    completed: usize,
    errors: usize,
    results: Vec<FileResult>,
}

fn ffmpeg_available() -> bool {
    ffmpeg()
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
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

/// `name_cleaned.ext`, falling back to `name_cleaned_2.ext`, `_3`, ... when taken.
fn unique_output_path(dir: &Path, input: &Path) -> Result<PathBuf, String> {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid file name")?;
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("Invalid file extension")?;

    let first = dir.join(format!("{stem}_cleaned.{ext}"));
    if !first.exists() {
        return Ok(first);
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem}_cleaned_{n}.{ext}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("Could not find a free output file name".into())
}

fn ffmpeg_args(input: &Path, output: &Path, faststart: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-y".into(),
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

fn clean_one(input: &Path, output_dir: &Path) -> Result<(), String> {
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

    let output = unique_output_path(output_dir, input)?;

    let run = |faststart: bool| {
        ffmpeg()
            .args(ffmpeg_args(input, &output, faststart))
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    FFMPEG_MISSING.to_string()
                } else {
                    format!("Could not start FFmpeg: {e}")
                }
            })
    };

    // faststart is the preferred path; if this container rejects it, retry
    // without it rather than failing the file.
    let mut result = run(true)?;
    if !result.status.success() {
        let _ = std::fs::remove_file(&output);
        result = run(false)?;
    }
    if !result.status.success() {
        let _ = std::fs::remove_file(&output);
        return Err(last_ffmpeg_error(&result.stderr));
    }
    Ok(())
}

#[tauri::command]
fn clean_videos(app: AppHandle, paths: Vec<String>) -> Result<CleanSummary, String> {
    if paths.is_empty() {
        return Err("No videos selected".into());
    }
    if !ffmpeg_available() {
        return Err(FFMPEG_MISSING.into());
    }

    let first = PathBuf::from(&paths[0]);
    let base = first
        .parent()
        .ok_or("Could not determine the folder of the first video")?;
    let output_dir = base.join("cleaned");
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Could not create the output folder: {e}"))?;

    let total = paths.len();
    let mut results = Vec::with_capacity(total);
    let (mut completed, mut errors) = (0usize, 0usize);

    for (index, raw) in paths.iter().enumerate() {
        let input = PathBuf::from(raw);
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());

        let _ = app.emit(
            "clean-progress",
            Progress {
                index,
                total,
                name: name.clone(),
                status: "processing",
                message: None,
            },
        );

        let (status, message) = match clean_one(&input, &output_dir) {
            Ok(()) => {
                completed += 1;
                ("completed", None)
            }
            Err(e) => {
                errors += 1;
                ("error", Some(e))
            }
        };

        let _ = app.emit(
            "clean-progress",
            Progress {
                index,
                total,
                name: name.clone(),
                status,
                message: message.clone(),
            },
        );
        results.push(FileResult {
            name,
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

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            check_ffmpeg,
            clean_videos,
            open_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
