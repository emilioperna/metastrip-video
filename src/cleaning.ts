// The one cleaning choice the user can make, and everything the page says about
// cleaning. Tauri-free so it can be unit tested.
//
// The privacy floor -- metadata, chapters, non-media tracks and detected cover
// art -- is not a choice and has no switch here. The backend enforces it and
// its verifier checks it on every file.

import type { ScanState, ScanView } from "./privacy";

/** Mirrors `CleaningOptions` in `plan.rs`. */
export type CleaningOptions = {
  removeSubtitles: boolean;
};

export const DEFAULT_CLEANING_OPTIONS: Readonly<CleaningOptions> = Object.freeze({
  removeSubtitles: false,
});

export const ALWAYS_REMOVED_NOTE =
  "Always removed: metadata, chapters, non-media tracks and detected cover art.";

export const REMOVE_SUBTITLES_LABEL = "Also remove subtitle tracks";

export const REMOVE_SUBTITLES_HELP =
  "Deletes separate subtitle tracks from the cleaned copy. Captions burned into the picture or carried inside the video stream are not affected.";

/**
 * The options the backend reported, read defensively: anything but an explicit
 * `true` is the default. The backend already normalises what it stores; this
 * keeps a surprise from ever turning into a removal the user did not ask for.
 */
export function readCleaningOptions(value: unknown): CleaningOptions {
  const removeSubtitles =
    typeof value === "object" &&
    value !== null &&
    (value as { removeSubtitles?: unknown }).removeSubtitles === true;
  return { removeSubtitles };
}

/** The options after the checkbox changes. */
export function withRemoveSubtitles(
  current: CleaningOptions,
  removeSubtitles: boolean,
): CleaningOptions {
  return { ...current, removeSubtitles };
}

/**
 * One batch's options, fixed at the click. A frozen copy: nothing the page
 * does to its own state afterwards can reach the value that was sent.
 */
export function snapshotOptions(options: CleaningOptions): Readonly<CleaningOptions> {
  return Object.freeze({ removeSubtitles: options.removeSubtitles === true });
}

/** The `clean_videos` arguments: the queue and the options, both as of now. */
export function cleanRequest(
  paths: readonly string[],
  options: CleaningOptions,
): { paths: string[]; options: Readonly<CleaningOptions> } {
  return { paths: [...paths], options: snapshotOptions(options) };
}

/** The `save_cleaning_options` arguments. */
export function saveOptionsRequest(options: CleaningOptions): {
  options: Readonly<CleaningOptions>;
} {
  return { options: snapshotOptions(options) };
}

/**
 * Whether the cleaning control is locked: while the pipeline is busy --
 * cleaning, or installing an update -- and until the stored choice has loaded,
 * so a click cannot race the value arriving. Scanning does not lock it: what
 * the scan finds does not depend on the options.
 */
export function cleaningControlLocked(busy: boolean, settingsLoaded: boolean): boolean {
  return busy || !settingsLoaded;
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/**
 * What the Clean button is about to remove beyond metadata, from what the scan
 * has found so far. Counts only ever come from files that were scanned, and
 * nothing here claims a file has no cover art or no subtitles: when some files
 * are still scanning or could not be scanned, the counts say they are partial.
 */
export function preCleanWarnings(
  files: readonly { scanState: ScanState; scan?: ScanView }[],
  options: CleaningOptions,
): string[] {
  const scanned = files.flatMap((file) =>
    file.scanState === "done" && file.scan?.ok ? [file.scan] : [],
  );
  const warnings: string[] = [];

  const withCover = scanned.filter((scan) => scan.coverImages > 0).length;
  if (withCover > 0) {
    warnings.push(
      `Detected cover art will be removed from ${plural(withCover, "video", "videos")}.`,
    );
  }

  if (options.removeSubtitles) {
    const withSubtitles = scanned.filter((scan) => scan.subtitleStreams > 0);
    const tracks = withSubtitles.reduce((sum, scan) => sum + scan.subtitleStreams, 0);
    if (tracks > 0) {
      warnings.push(
        withSubtitles.length === 1
          ? `${plural(tracks, "subtitle track", "subtitle tracks")} will be removed from 1 video.`
          : `${plural(tracks, "subtitle track", "subtitle tracks")} across ${withSubtitles.length} videos will be removed.`,
      );
    }
  }

  const unscanned = files.length - scanned.length;
  if (warnings.length > 0 && unscanned > 0) {
    warnings.push(
      `Based on ${scanned.length} of ${plural(files.length, "video", "videos")} scanned so far.`,
    );
  }
  return warnings;
}

/**
 * Tooltip for the "Verified cleaning" badge: exactly what the checks cover and
 * no more. Runtime verification compares stream parameters; packet identity is
 * proven by the test suite only, and nothing inside a kept stream is inspected.
 */
export function verifiedCleaningDetail(options: CleaningOptions): string {
  const removed = options.removeSubtitles
    ? "Metadata fields, chapters, non-media tracks, detected cover art and subtitle tracks were removed."
    : "Metadata fields, chapters, non-media tracks and detected cover art were removed.";
  const kept = options.removeSubtitles
    ? "Kept video and audio streams"
    : "Kept video, audio and subtitle streams";
  return `Every check passed. ${removed} ${kept} use stream copy with matching codec parameters, and their contents are not inspected. The original was not modified and no temporary files were left.`;
}

/** The counts behind the completion card's stats line. */
export type RemovalCounts = {
  privacyFieldsRemoved: number;
  technicalFieldsRemoved: number;
  chaptersRemoved: number;
  dataStreamsRemoved: number;
  coverArtStreamsRemoved: number;
  subtitleStreamsRemoved: number;
};

/**
 * The completion card's stats, privacy first. Every figure is measured by the
 * verifier, and each noun is the one its count can justify.
 */
export function completionStats(summary: RemovalCounts): string[] {
  const stats: string[] = [];
  if (summary.privacyFieldsRemoved > 0) {
    stats.push(
      `${plural(summary.privacyFieldsRemoved, "privacy field", "privacy fields")} removed`,
    );
  }
  if (summary.coverArtStreamsRemoved > 0) {
    stats.push(
      `${plural(summary.coverArtStreamsRemoved, "detected cover art stream", "detected cover art streams")} removed`,
    );
  }
  if (summary.dataStreamsRemoved > 0) {
    // "other", for the same reason the scan line says it: this counter is every
    // non-media track removed, and for a Matroska that is typically an embedded
    // font rather than a data track.
    stats.push(`${plural(summary.dataStreamsRemoved, "other track", "other tracks")} removed`);
  }
  if (summary.subtitleStreamsRemoved > 0) {
    stats.push(
      `${plural(summary.subtitleStreamsRemoved, "subtitle track", "subtitle tracks")} removed`,
    );
  }
  if (summary.chaptersRemoved > 0) {
    stats.push(`${plural(summary.chaptersRemoved, "chapter marker", "chapter markers")} removed`);
  }
  if (summary.technicalFieldsRemoved > 0) {
    stats.push(
      `${plural(summary.technicalFieldsRemoved, "technical field", "technical fields")} removed`,
    );
  }
  return stats;
}
