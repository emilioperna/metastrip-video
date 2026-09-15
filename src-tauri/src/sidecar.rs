//! Locating and running the bundled FFmpeg tools.
//!
//! `externalBin` in `tauri.conf.json` installs each sidecar next to the app
//! executable, so a packaged install never depends on the system PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[cfg(debug_assertions)]
pub const FFMPEG_MISSING: &str = "FFmpeg was not found. Install FFmpeg and restart the app.";
#[cfg(not(debug_assertions))]
pub const FFMPEG_MISSING: &str =
    "FFmpeg was not found in the application folder. Reinstall the application.";

#[cfg(debug_assertions)]
pub const FFPROBE_MISSING: &str =
    "FFprobe was not found. Install FFmpeg, which includes ffprobe, and restart the app.";
#[cfg(not(debug_assertions))]
pub const FFPROBE_MISSING: &str =
    "FFprobe was not found in the application folder. Reinstall the application.";

/// Resolve one sidecar by its stem (`ffmpeg`, `ffprobe`).
///
/// Installed layout first, then the target-triple name the Tauri CLI uses in the
/// target dir during development. A release build never falls back to the PATH:
/// a missing sidecar means a broken install, and failing loudly is better than
/// silently running some other build.
fn resolve(stem: &str) -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));

    if let Some(dir) = &dir {
        let names = [
            format!("{stem}.exe"),
            format!("{stem}-{}.exe", env!("TARGET_TRIPLE")),
        ];
        if let Some(found) = names.iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
            return found;
        }
    }

    #[cfg(debug_assertions)]
    {
        PathBuf::from(stem)
    }
    #[cfg(not(debug_assertions))]
    {
        dir.unwrap_or_default().join(format!("{stem}.exe"))
    }
}

pub fn ffmpeg_program() -> &'static PathBuf {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| resolve("ffmpeg"))
}

pub fn ffprobe_program() -> &'static PathBuf {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| resolve("ffprobe"))
}

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: keeps a console from flashing for every file.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Build an `ffmpeg` invocation. Arguments are always passed as a real argv,
/// never through a shell, so paths cannot be interpreted as commands.
pub fn ffmpeg() -> Command {
    command(ffmpeg_program())
}

/// Same contract as [`ffmpeg`], for the probe tool.
pub fn ffprobe() -> Command {
    command(ffprobe_program())
}

fn responds_to_version(mut command: Command) -> bool {
    command
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn ffmpeg_available() -> bool {
    responds_to_version(ffmpeg())
}

pub fn ffprobe_available() -> bool {
    responds_to_version(ffprobe())
}
