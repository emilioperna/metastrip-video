//! Running an Edit batch: one file after another, each inspected afresh and
//! planned before it may cost anything, then written to a temporary name and
//! published by rename.
//!
//! Per file, in this order and no other:
//!
//! 1. the input is still there and in the support matrix ([`validate_input`]);
//! 2. it is inspected now ([`FreshInspection::take`]);
//! 3. it is planned ([`plan_edit`]), which refuses anything the edit would lose;
//! 4. only then is an output ID reserved and persisted;
//! 5. FFmpeg writes the temporary file, with the planner's own arguments;
//! 6. a finished temporary file is renamed to its final name.
//!
//! A file stopped at 1-3 is *skipped*: nothing was written and no ID was
//! spent. A file stopped at 4-6 is an *error*: its ID stays spent, and no
//! output under its final name exists. A file that reaches the end is
//! *completed* -- written, not verified. Verification is a separate step that
//! this module does not claim.
//!
//! Clean has its own runner in the crate root and shares nothing here beyond
//! the ID registry, the temporary-file convention and the batch state.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde::Serialize;

use crate::edit::{EditOperation, ValidEditRequest};
use crate::edit_plan::{edit_temp_path, plan_edit, FreshInspection};
use crate::sidecar::{ffmpeg, ffmpeg_available, FFMPEG_MISSING, FFPROBE_MISSING};
use crate::{
    ffmpeg_diagnostic, format_id, inspect, load_settings, remove_stale_temp_files, validate_input,
    BatchGuard, FormatProfile, IdRegistry,
};

/// Shown for a file ffprobe could not read. Edit cannot go ahead without a
/// fresh baseline, and ffprobe's own words may carry the path.
pub(crate) const EDIT_NOT_INSPECTED: &str =
    "This file could not be inspected, so it was not edited.";

/// Shown when a partial output could not be cleared before an FFmpeg attempt.
pub(crate) const EDIT_TEMP_IN_THE_WAY: &str =
    "A partial output could not be removed, so editing stopped for this file. Nothing was published.";

/// One event of an Edit batch as the page receives it on `edit-progress`:
/// once when a file starts, once when its outcome is known. File names only,
/// never a path, a metadata value or anything FFmpeg printed unredacted.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditProgress {
    pub(crate) batch_id: u64,
    pub(crate) index: usize,
    pub(crate) total: usize,
    pub(crate) input_name: String,
    pub(crate) output_name: Option<String>,
    /// `processing`, then one of `completed`, `skipped` or `error`.
    pub(crate) status: &'static str,
    pub(crate) message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditFileResult {
    pub(crate) input_name: String,
    pub(crate) output_name: Option<String>,
    pub(crate) status: &'static str,
    pub(crate) message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditSummary {
    pub(crate) batch_id: u64,
    pub(crate) output_dir: String,
    /// Outputs written and in place. Not a claim that they were verified.
    pub(crate) completed: usize,
    /// Refused before an ID was reserved; nothing written.
    pub(crate) skipped: usize,
    /// Accepted, but no output was published.
    pub(crate) errors: usize,
    pub(crate) results: Vec<EditFileResult>,
}

/// How one file ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditOutcome {
    /// The output is in place under this name.
    Completed(String),
    /// Refused before an output ID was reserved.
    Skipped(String),
    /// Refused or failed after its ID was reserved. The ID stays spent.
    Failed(String),
}

impl EditOutcome {
    fn status(&self) -> &'static str {
        match self {
            EditOutcome::Completed(_) => "completed",
            EditOutcome::Skipped(_) => "skipped",
            EditOutcome::Failed(_) => "error",
        }
    }

    /// Status, output name and message, as a result row carries them.
    fn into_row(self) -> (&'static str, Option<String>, Option<String>) {
        let status = self.status();
        match self {
            EditOutcome::Completed(name) => (status, Some(name), None),
            EditOutcome::Skipped(message) | EditOutcome::Failed(message) => {
                (status, None, Some(message))
            }
        }
    }
}

/// Start the bundled FFmpeg with exactly these arguments and wait for it. The
/// one launcher production uses; tests hand `edit_one` their own to reach
/// failures a real file cannot produce on demand.
pub(crate) fn run_ffmpeg(args: &[String]) -> std::io::Result<Output> {
    ffmpeg().args(args).output()
}

/// Edit one file: validate, inspect, plan -- and only then reserve, write and
/// publish. See the module documentation for the order and what each outcome
/// means.
pub(crate) fn edit_one(
    input: &Path,
    request: &ValidEditRequest,
    output_dir: &Path,
    prefix: &str,
    registry: &mut IdRegistry,
    run: impl FnMut(&[String]) -> std::io::Result<Output>,
) -> EditOutcome {
    let format = match validate_input(input) {
        Ok(format) => format,
        Err(message) => return EditOutcome::Skipped(message),
    };
    let fresh = match FreshInspection::take(input) {
        Ok(fresh) => fresh,
        Err(_) => return EditOutcome::Skipped(EDIT_NOT_INSPECTED.into()),
    };
    edit_inspected(&fresh, format, request, output_dir, prefix, registry, run)
}

/// Everything after the fresh inspection: plan, reserve, write, publish. The
/// file edited is the one that was inspected.
pub(crate) fn edit_inspected(
    fresh: &FreshInspection,
    format: FormatProfile,
    request: &ValidEditRequest,
    output_dir: &Path,
    prefix: &str,
    registry: &mut IdRegistry,
    mut run: impl FnMut(&[String]) -> std::io::Result<Output>,
) -> EditOutcome {
    let plan = match plan_edit(request, format, fresh, output_dir) {
        Ok(plan) => plan,
        Err(refusal) => return EditOutcome::Skipped(refusal.message()),
    };

    // Accepted: from here on the file costs an ID, whatever happens to it.
    let input = fresh.path();
    // The source spelling (`.MOV` stays `.MOV`), as Clean keeps it. Always one
    // of the ASCII extensions in the support matrix, or validation failed.
    let extension = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let id = match registry.reserve(output_dir, prefix, extension) {
        Ok(id) => format_id(id),
        Err(message) => return EditOutcome::Failed(message),
    };
    let final_name = format!("{prefix}_{id}.{extension}");
    let final_path = output_dir.join(&final_name);
    let temp_path = edit_temp_path(output_dir, &id, input);

    // Everything an FFmpeg message must not show: the user's own values and
    // the track names written back from the file, beside the paths.
    let values: Vec<&str> = request
        .edits()
        .iter()
        .filter_map(|edit| match &edit.operation {
            EditOperation::Set(value) | EditOperation::FillIfMissing(value) => Some(value.as_str()),
            EditOperation::Remove => None,
        })
        .chain(plan.restores().iter().map(|restore| restore.title.as_str()))
        .collect();

    // The same ID and temporary path for every attempt: a retry is the same
    // output, not another one.
    let mut last_error = "FFmpeg failed".to_string();
    for attempt in plan.mux_attempts() {
        // `-n` keeps FFmpeg from overwriting a file already at the temporary
        // path, but the pinned FFmpeg then exits with status 0. A success
        // proves this run wrote the file only if nothing was there before it,
        // so a partial output that could not be removed stops the file here
        // rather than being published by the next attempt's "success".
        if temp_path.symlink_metadata().is_ok() {
            return EditOutcome::Failed(EDIT_TEMP_IN_THE_WAY.into());
        }
        let args = match plan.ffmpeg_args(input, &temp_path, *attempt) {
            Ok(args) => args,
            // Planning measured this command against a path of the same
            // length, so this cannot happen; if it did, nothing runs.
            Err(refusal) => return EditOutcome::Failed(refusal.message()),
        };
        let result = match run(&args) {
            Ok(result) => result,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                return EditOutcome::Failed(if error.kind() == std::io::ErrorKind::NotFound {
                    FFMPEG_MISSING.to_string()
                } else {
                    format!("Could not start FFmpeg: {error}")
                });
            }
        };
        if result.status.success() {
            return publish(&temp_path, &final_path, final_name);
        }
        last_error = edit_ffmpeg_error(&result.stderr, input, &temp_path, output_dir, &values);
        // The partial output goes before the next attempt, which must start
        // from an empty path (see above).
        let _ = std::fs::remove_file(&temp_path);
    }

    EditOutcome::Failed(format!(
        "Stream copy edit failed for this {} file: {last_error}. No video or audio re-encoding was attempted.",
        format.label
    ))
}

/// Give a finished temporary file its final name, and never over a file that
/// is already there: the name was free when it was reserved, but a rename on
/// Windows replaces an existing file, so it is checked again at the last step.
fn publish(temp: &Path, final_path: &Path, final_name: String) -> EditOutcome {
    if final_path.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(temp);
        return EditOutcome::Failed(
            "The output name was taken while this file was being edited. Nothing was overwritten."
                .into(),
        );
    }
    match std::fs::rename(temp, final_path) {
        Ok(()) => EditOutcome::Completed(final_name),
        Err(error) => {
            let _ = std::fs::remove_file(temp);
            EditOutcome::Failed(format!(
                "Could not move the edited file into place: {error}"
            ))
        }
    }
}

/// The line FFmpeg's failure is best described by, with nothing in it that
/// identifies the user's files or repeats what they typed.
///
/// FFmpeg echoes its arguments, and an Edit's arguments carry the input path,
/// the output path and every value being written. Each of those is replaced
/// by what it is; values become `[value]`.
pub(crate) fn edit_ffmpeg_error(
    stderr: &[u8],
    input: &Path,
    output: &Path,
    output_dir: &Path,
    values: &[&str],
) -> String {
    let text = String::from_utf8_lossy(stderr);
    let Some(line) = ffmpeg_diagnostic(&text) else {
        return "FFmpeg failed".into();
    };

    let input = input.to_string_lossy();
    let output_path = output.to_string_lossy();
    let folder = output_dir.to_string_lossy();
    let temp_name = output
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut needles: Vec<(&str, &str)> = vec![
        (input.as_ref(), "[input file]"),
        (output_path.as_ref(), "[output file]"),
        (folder.as_ref(), "[output folder]"),
        (temp_name.as_ref(), "[output file]"),
    ];
    needles.extend(values.iter().map(|value| (*value, "[value]")));

    let message = redact(line, &needles);
    if message.trim().is_empty() {
        "FFmpeg failed".into()
    } else {
        message
    }
}

/// Replace every occurrence of every needle in one pass from left to right,
/// the longest needle first wherever several match. A replacement is never
/// searched again, so one value can never be found inside another's
/// placeholder, and a path is replaced whole before its folder could be.
fn redact(text: &str, needles: &[(&str, &str)]) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(next) = rest.chars().next() {
        let hit = needles
            .iter()
            .filter(|(needle, _)| !needle.is_empty() && rest.starts_with(needle))
            .max_by_key(|(needle, _)| needle.len());
        match hit {
            Some((needle, replacement)) => {
                redacted.push_str(replacement);
                rest = &rest[needle.len()..];
            }
            None => {
                redacted.push(next);
                rest = &rest[next.len_utf8()..];
            }
        }
    }
    redacted
}

/// The whole batch, sequentially, with progress pushed through a callback so
/// it stays independent of Tauri. One file's refusal or failure never stops
/// the files after it.
pub(crate) fn run_edit_batch(
    request: &ValidEditRequest,
    prefix: &str,
    output_dir: &Path,
    app_dir: &Path,
    batch_id: u64,
    mut on_progress: impl FnMut(EditProgress),
) -> Result<EditSummary, String> {
    if !output_dir.is_dir() {
        return Err("The output folder no longer exists. Choose a new one.".into());
    }
    if !ffmpeg_available() {
        return Err(FFMPEG_MISSING.into());
    }
    // Unlike Clean, Edit has no way forward without an inspection.
    if !inspect::available() {
        return Err(FFPROBE_MISSING.into());
    }

    // Safe once, here: the batch state admits one batch at a time, Clean or
    // Edit, so no other batch has a temporary file in flight.
    remove_stale_temp_files(output_dir);
    let mut registry = IdRegistry::open(&app_dir.join("used-ids.txt"))?;

    let paths = request.paths();
    let total = paths.len();
    let mut results = Vec::with_capacity(total);
    let (mut completed, mut skipped, mut errors) = (0usize, 0usize, 0usize);

    for (index, raw) in paths.iter().enumerate() {
        let input = PathBuf::from(raw);
        let input_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());

        on_progress(EditProgress {
            batch_id,
            index,
            total,
            input_name: input_name.clone(),
            output_name: None,
            status: "processing",
            message: None,
        });

        let outcome = edit_one(
            &input,
            request,
            output_dir,
            prefix,
            &mut registry,
            run_ffmpeg,
        );
        match outcome {
            EditOutcome::Completed(_) => completed += 1,
            EditOutcome::Skipped(_) => skipped += 1,
            EditOutcome::Failed(_) => errors += 1,
        }
        let (status, output_name, message) = outcome.into_row();

        on_progress(EditProgress {
            batch_id,
            index,
            total,
            input_name: input_name.clone(),
            output_name: output_name.clone(),
            status,
            message: message.clone(),
        });
        results.push(EditFileResult {
            input_name,
            output_name,
            status,
            message,
        });
    }

    Ok(EditSummary {
        batch_id,
        output_dir: output_dir.to_string_lossy().into_owned(),
        completed,
        skipped,
        errors,
        results,
    })
}

/// Everything `edit_videos` does once the request is valid and the app is
/// claimed. The guard comes in by value and is held until the last file is
/// done. Prefix and output folder come from the settings; nothing else there
/// applies to Edit, and the edits come only from the request.
pub(crate) fn edit_with(
    batch: BatchGuard,
    app_dir: &Path,
    request: &ValidEditRequest,
    on_progress: impl FnMut(EditProgress),
) -> Result<EditSummary, String> {
    let settings = load_settings(app_dir);
    if settings.output_directory.is_empty() {
        return Err("Choose an output folder first.".into());
    }
    let output_dir = PathBuf::from(&settings.output_directory);

    run_edit_batch(
        request,
        &settings.prefix,
        &output_dir,
        app_dir,
        batch.id,
        on_progress,
    )
}

#[cfg(test)]
#[path = "edit_batch_tests.rs"]
mod edit_batch_tests;
