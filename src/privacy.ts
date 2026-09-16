/**
 * Privacy-scan types and the aggregation the UI needs.
 *
 * These mirror the typed model the Rust backend sends. Nothing here parses
 * ffprobe output — normalisation happens in Rust, and the frontend only ever
 * sees this shape.
 *
 * The functions are pure and Tauri-free so they can be unit tested, which is
 * what keeps the batch counters honest without driving the whole app.
 */

export type Severity = "high" | "medium" | "low";

export type PrivacyFinding = {
  category: string;
  categoryLabel: string;
  severity: Severity;
  severityLabel: string;
  detail: string;
  explanation: string;
  scopeLabel: string;
  streamIndex: number | null;
  sourceKey: string;
};

/**
 * `total`, `high`, `medium` and `low` count privacy findings only. Structural
 * findings (container bookkeeping such as `major_brand` or `handler_name`) are
 * technical metadata and are counted in `technical`, never in the privacy
 * figures. The backend computes this; the frontend only adds it up.
 */
export type PrivacySummary = {
  total: number;
  high: number;
  medium: number;
  low: number;
  technical: number;
};

export type CleaningPlan = {
  removeFormatMetadata: boolean;
  removeStreamMetadata: boolean;
  removeChapters: boolean;
  removeDataStreams: boolean;
  containerStrategy: string;
  expectedRemovedFields: number;
  sensitiveFindings: number;
  expectedRemovedChapters: number;
  expectedRemovedDataStreams: number;
  guarantees: string[];
};

export type ScanView = {
  path: string;
  name: string;
  ok: boolean;
  error: string | null;
  summary: PrivacySummary;
  findings: PrivacyFinding[];
  container: string | null;
  durationSeconds: number | null;
  videoStreams: number;
  audioStreams: number;
  otherStreams: number;
  chapterCount: number;
  fieldCount: number;
  plan: CleaningPlan | null;
};

export type VerificationCheck = {
  name: string;
  passed: boolean;
  detail: string;
};

export type BeforeAfterRow = {
  category: string;
  severity: string;
  before: string;
  after: string;
  removed: boolean;
  technical: boolean;
};

export type VerificationReport = {
  verified: boolean;
  checks: VerificationCheck[];
  fieldsRemoved: number;
  privacyFieldsRemoved: number;
  technicalFieldsRemoved: number;
  chaptersRemoved: number;
  dataStreamsRemoved: number;
  residual: PrivacyFinding[];
  beforeAfter: BeforeAfterRow[];
};

/** Scan state of one queued file, tracked separately from cleaning status. */
export type ScanState = "pending" | "scanning" | "done" | "failed";

export const EMPTY_SUMMARY: PrivacySummary = {
  total: 0,
  high: 0,
  medium: 0,
  low: 0,
  technical: 0,
};

/** The backend's serialised name for the one technical category. */
export const TECHNICAL_CATEGORY = "structural";

/** Structural metadata is technical, not a privacy finding. */
export function isTechnical(finding: Pick<PrivacyFinding, "category">): boolean {
  return finding.category === TECHNICAL_CATEGORY;
}

/** Splits a finding list for the expanded view, preserving order. */
export function partitionFindings(findings: PrivacyFinding[]): {
  privacy: PrivacyFinding[];
  technical: PrivacyFinding[];
} {
  const privacy: PrivacyFinding[] = [];
  const technical: PrivacyFinding[] = [];
  for (const finding of findings) {
    if (isTechnical(finding)) technical.push(finding);
    else privacy.push(finding);
  }
  return { privacy, technical };
}

/**
 * Batch totals. Only successfully scanned files contribute; a file that could
 * not be inspected is counted apart so the header never implies it was found
 * clean.
 */
export type BatchSummary = PrivacySummary & {
  scanned: number;
  failed: number;
  filesWithHigh: number;
};

export function summarise(scans: Iterable<ScanView>): BatchSummary {
  const batch: BatchSummary = {
    ...EMPTY_SUMMARY,
    scanned: 0,
    failed: 0,
    filesWithHigh: 0,
  };
  for (const scan of scans) {
    if (!scan.ok) {
      batch.failed += 1;
      continue;
    }
    batch.scanned += 1;
    batch.total += scan.summary.total;
    batch.high += scan.summary.high;
    batch.medium += scan.summary.medium;
    batch.low += scan.summary.low;
    batch.technical += scan.summary.technical;
    if (scan.summary.high > 0) batch.filesWithHigh += 1;
  }
  return batch;
}

/**
 * The highest severity present, or null when there are no findings at all.
 * Drives the single chip shown on a collapsed queue row.
 */
export function topSeverity(summary: PrivacySummary): Severity | null {
  if (summary.high > 0) return "high";
  if (summary.medium > 0) return "medium";
  if (summary.low > 0) return "low";
  return null;
}

/**
 * `4 privacy findings` / `1 privacy finding`, `No privacy findings` when only
 * technical metadata is present, and `No metadata found` when there is nothing.
 */
export function findingsLabel(summary: PrivacySummary): string {
  if (summary.total === 0) {
    return summary.technical > 0 ? "No privacy findings" : "No metadata found";
  }
  return `${summary.total} privacy ${summary.total === 1 ? "finding" : "findings"}`;
}

/** `7 technical fields`, or null when there are none. Secondary copy only. */
export function technicalLabel(count: number): string | null {
  if (count <= 0) return null;
  return `${count} technical ${count === 1 ? "field" : "fields"}`;
}

/**
 * Findings grouped by category, highest severity first, for the expanded view.
 *
 * Grouping matters for large files: a camera original can carry dozens of
 * `com.apple.quicktime.*` keys that are all one disclosure to a reader.
 */
export type FindingGroup = {
  category: string;
  severity: Severity;
  severityLabel: string;
  explanation: string;
  items: PrivacyFinding[];
};

const SEVERITY_ORDER: Record<Severity, number> = { high: 0, medium: 1, low: 2 };

export function groupFindings(findings: PrivacyFinding[]): FindingGroup[] {
  const groups = new Map<string, FindingGroup>();

  for (const finding of findings) {
    const existing = groups.get(finding.categoryLabel);
    if (!existing) {
      groups.set(finding.categoryLabel, {
        category: finding.categoryLabel,
        severity: finding.severity,
        severityLabel: finding.severityLabel,
        explanation: finding.explanation,
        items: [finding],
      });
      continue;
    }
    existing.items.push(finding);
    // A group takes the severity of its most severe member.
    if (SEVERITY_ORDER[finding.severity] < SEVERITY_ORDER[existing.severity]) {
      existing.severity = finding.severity;
      existing.severityLabel = finding.severityLabel;
      existing.explanation = finding.explanation;
    }
  }

  return [...groups.values()].sort(
    (a, b) =>
      SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity] ||
      a.category.localeCompare(b.category),
  );
}

/**
 * What the app may say about the audio and video streams.
 *
 * Cleaning always uses FFmpeg stream copy and has no transcoding path. The
 * runtime verifier compares codec parameters between input and output; it does
 * not compare packets, so nothing here may call the streams bit-for-bit or
 * byte-identical. Packet identity is proven by the Rust regression tests only.
 */
export const STREAM_COPY_NOTE =
  "Metadata is removed locally. Video and audio use stream copy, with no transcoding.";

/** Tooltip for the "Verified cleaning" badge: exactly what the checks cover. */
export const VERIFIED_CLEANING_DETAIL =
  "Every check passed: sensitive metadata removed, stream copy used and media stream parameters match, original not modified, no temporary files left.";

/**
 * Completion headline. Deliberately refuses the word "verified" unless every
 * cleaned file actually passed, and names the shortfall when it did not.
 */
export function completionTitle(summary: {
  completed: number;
  errors: number;
  verified: number;
  verificationFailures: number;
}): string {
  const noun = summary.completed === 1 ? "video" : "videos";
  if (summary.completed === 0) return "No videos were cleaned";
  if (summary.errors > 0) {
    return `${summary.completed} cleaned · ${summary.errors} failed`;
  }
  if (summary.verificationFailures > 0) {
    return `${summary.completed} ${noun} cleaned · ${summary.verificationFailures} not verified`;
  }
  return `${summary.completed} ${noun} cleaned`;
}

/**
 * True only when every file that produced an output passed every check.
 *
 * Files that failed outright are not considered: they were never cleaned, so
 * they cannot be unverified. The completion headline reports those separately,
 * which is why this can be true in a batch that also had errors.
 */
export function allVerified(summary: {
  completed: number;
  verified: number;
  verificationFailures: number;
}): boolean {
  return (
    summary.completed > 0 &&
    summary.verificationFailures === 0 &&
    summary.verified === summary.completed
  );
}

/** `1.2 s`, `48 s`, `3:20`. Durations are shown only when ffprobe reported one. */
export function formatDuration(seconds: number | null): string | null {
  if (seconds === null || !Number.isFinite(seconds) || seconds < 0) return null;
  if (seconds < 60) {
    return seconds < 10 ? `${seconds.toFixed(1)} s` : `${Math.round(seconds)} s`;
  }
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  // 59.6 s rounds to 60, which must not render as `3:60`.
  if (rest === 60) return `${minutes + 1}:00`;
  return `${minutes}:${String(rest).padStart(2, "0")}`;
}

/**
 * The secondary facts line of an expanded file: duration, streams, chapters.
 *
 * Deliberately no generic metadata field total: the row already states privacy
 * findings and technical fields separately, and a combined count would blur
 * that distinction again.
 */
export function detailFacts(
  scan: Pick<ScanView, "durationSeconds" | "videoStreams" | "audioStreams" | "otherStreams" | "chapterCount">,
): string[] {
  const facts: string[] = [];
  const duration = formatDuration(scan.durationSeconds);
  if (duration) facts.push(duration);
  facts.push(
    `${scan.videoStreams} video · ${scan.audioStreams} audio` +
      (scan.otherStreams > 0 ? ` · ${scan.otherStreams} data` : ""),
  );
  if (scan.chapterCount > 0) facts.push(`${scan.chapterCount} chapters`);
  return facts;
}
