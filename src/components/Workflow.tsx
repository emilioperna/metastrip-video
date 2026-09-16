import type { ReactNode } from "react";
import { fileExtension, type SupportedFormat } from "../formats";
import {
  allVerified,
  completionTitle,
  findingsLabel,
  formatDuration,
  groupFindings,
  partitionFindings,
  summarise,
  technicalLabel,
  topSeverity,
  type FindingGroup,
  type ScanState,
  type ScanView,
  type Severity,
  type VerificationReport,
  VERIFIED_CLEANING_DETAIL,
} from "../privacy";

export type Status = "ready" | "processing" | "completed" | "error";

export type VideoFile = {
  path: string;
  name: string;
  status: Status;
  outputName?: string;
  message?: string;
  /** Privacy scan, tracked separately from the cleaning status. */
  scanState: ScanState;
  scan?: ScanView;
  verification?: VerificationReport;
};

export type Phase = "idle" | "running" | "done";

export type Settings = {
  prefix: string;
  outputDirectory: string;
  outputDirectoryValid: boolean;
};

export type Summary = {
  outputDir: string;
  completed: number;
  errors: number;
  verified: number;
  verificationFailures: number;
  fieldsRemoved: number;
  privacyFieldsRemoved: number;
  technicalFieldsRemoved: number;
  chaptersRemoved: number;
  dataStreamsRemoved: number;
};

type IconName =
  | "brand"
  | "video"
  | "folder"
  | "check"
  | "warning"
  | "info"
  | "failed"
  | "shield"
  | "chevron";

const ICON_PATHS: Record<IconName, ReactNode> = {
  brand: (
    <>
      <rect x="3.5" y="4" width="17" height="16" rx="4" />
      <path d="m9 8 6 4-6 4V8Z" />
    </>
  ),
  video: (
    <>
      <rect x="3" y="5" width="14" height="14" rx="3" />
      <path d="m17 10 4-2v8l-4-2" />
      <path d="M7 9.5h4M7 13h6" />
    </>
  ),
  folder: (
    <path d="M3.5 7.5h6l1.7 2H20.5v7.75A2.75 2.75 0 0 1 17.75 20H6.25a2.75 2.75 0 0 1-2.75-2.75V7.5Zm0 0v-.75A2.75 2.75 0 0 1 6.25 4h2.6l1.8 2h7.1a2.75 2.75 0 0 1 2.75 2.75v.75" />
  ),
  check: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="m8 12.2 2.6 2.6L16.5 9" />
    </>
  ),
  warning: (
    <>
      <path d="M10.1 4.7 2.9 17.1A2 2 0 0 0 4.6 20h14.8a2 2 0 0 0 1.7-2.9L13.9 4.7a2.2 2.2 0 0 0-3.8 0Z" />
      <path d="M12 9v4.3M12 16.7h.01" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 10.8V17M12 7.3h.01" />
    </>
  ),
  failed: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="m9 9 6 6M15 9l-6 6" />
    </>
  ),
  shield: (
    <>
      <path d="M12 3.5l7 2.6v5.2c0 4.2-2.9 7.6-7 9.2-4.1-1.6-7-5-7-9.2V6.1l7-2.6Z" />
      <path d="m8.8 12.1 2.2 2.2 4.2-4.4" />
    </>
  ),
  chevron: <path d="m8 10 4 4 4-4" />,
};

function Icon({ name, size = 18 }: { name: IconName; size?: number }) {

  return (
    <svg
      aria-hidden="true"
      className="icon"
      fill="none"
      height={size}
      viewBox="0 0 24 24"
      width={size}
    >
      <g stroke="currentColor" strokeLinecap="round" strokeLinejoin="round" strokeWidth="1.65">
        {ICON_PATHS[name]}
      </g>
    </svg>
  );
}

export function ProductHeader() {
  return (
    <header className="product-header">
      <div className="product-bar">
        <div className="brand-lockup" aria-label="MetaStrip">
          <span className="brand-mark">
            <Icon name="brand" size={17} />
          </span>
          <span className="brand-name">MetaStrip</span>
        </div>
        <div className="local-signal" title="Your videos never leave this computer">
          <span className="local-dot" aria-hidden="true" />
          <span>Local processing</span>
        </div>
      </div>
      <div className="product-intro">
        <h1>Remove metadata. Keep the quality.</h1>
        <p>Clean videos locally with stream copy, no transcoding.</p>
      </div>
    </header>
  );
}

type DropZoneProps = {
  dragging: boolean;
  formats: SupportedFormat[];
  onChoose: () => void;
};

export function DropZone({ dragging, formats, onChoose }: DropZoneProps) {
  const loading = formats.length === 0;
  const labels = formats.map((format) => format.label).join(" \u00b7 ");

  return (
    <button
      type="button"
      className={`drop-zone${dragging ? " is-dragging" : ""}`}
      disabled={loading}
      onClick={onChoose}
      aria-describedby="supported-formats"
    >
      <span className="drop-icon" aria-hidden="true">
        <Icon name="video" size={26} />
      </span>
      <span className="drop-title">Drop videos here</span>
      <span className="drop-prompt">
        or <span className="drop-link">choose videos</span> from your PC
      </span>
      <span className="format-line" id="supported-formats">
        {loading ? "Loading supported formats..." : labels}
      </span>
    </button>
  );
}

function FileStatus({ file }: { file: VideoFile }) {
  if (file.status === "processing") {
    return (
      <span className="file-status file-status--processing" title="Cleaning this video">
        <span className="status-spinner" aria-hidden="true" />
        Cleaning...
      </span>
    );
  }
  if (file.status === "completed") {
    // "Cleaned" and "Verified" are different claims, and the UI keeps them
    // apart: the word verified appears only when every check actually passed.
    if (file.verification?.verified) {
      return (
        <span className="file-status file-status--completed" title="Cleaned and verified">
          <Icon name="check" size={15} />
          Verified
        </span>
      );
    }
    return (
      <span
        className="file-status file-status--unverified"
        title={file.message ?? "Cleaned, but not verified"}
      >
        <Icon name="warning" size={15} />
        {file.verification ? "Verification failed" : "Not verified"}
      </span>
    );
  }
  if (file.status === "error") {
    return (
      <span className="file-status file-status--error" title={file.message ?? "Cleaning failed"}>
        <Icon name="failed" size={15} />
        Failed
      </span>
    );
  }
  return (
    <span className="file-status file-status--ready">
      <span className="status-dot" aria-hidden="true" />
      Ready
    </span>
  );
}

function SeverityChip({ severity, count }: { severity: Severity; count: number }) {
  return (
    <span className={`sev-chip sev-chip--${severity}`}>
      <span className="sev-chip__count">{count}</span>
      {severity.toUpperCase()}
    </span>
  );
}

/**
 * The scan line on a collapsed row: severity chips and a total, nothing else.
 *
 * Counts only, never values. With a hundred files this is all that renders until
 * the reader opens something, which is what keeps a large batch usable.
 */
function ScanSummaryLine({ file }: { file: VideoFile }) {
  if (file.scanState === "pending" || file.scanState === "scanning") {
    return (
      <span className="scan-line scan-line--pending">
        <span className="status-spinner" aria-hidden="true" />
        Scanning...
      </span>
    );
  }
  if (file.scanState === "failed") {
    return (
      <span className="scan-line scan-line--failed" title={file.scan?.error ?? undefined}>
        <Icon name="warning" size={14} />
        Scan failed
      </span>
    );
  }
  const summary = file.scan?.summary;
  if (!summary) return null;
  const technical = technicalLabel(summary.technical);
  if (summary.total === 0) {
    return (
      <span className="scan-line scan-line--clean">
        {findingsLabel(summary)}
        {technical ? <span className="scan-technical">{technical}</span> : null}
      </span>
    );
  }
  return (
    <span className="scan-line">
      {summary.high > 0 ? <SeverityChip severity="high" count={summary.high} /> : null}
      {summary.medium > 0 ? <SeverityChip severity="medium" count={summary.medium} /> : null}
      {summary.low > 0 ? <SeverityChip severity="low" count={summary.low} /> : null}
      <span className="scan-total">{findingsLabel(summary)}</span>
      {technical ? <span className="scan-technical">{technical}</span> : null}
    </span>
  );
}

/**
 * One list of finding groups. Technical groups carry a neutral `TECHNICAL` tag
 * instead of a severity, so container bookkeeping never reads as a privacy
 * warning.
 */
function FindingGroupList({ groups, technical }: { groups: FindingGroup[]; technical: boolean }) {
  return (
    <ul className="finding-groups">
      {groups.map((group) => (
        <li
          key={group.category}
          className={`finding-group finding-group--${technical ? "technical" : group.severity}`}
        >
          <div className="finding-group__head">
            {technical ? (
              <span className="sev-tag sev-tag--technical">TECHNICAL</span>
            ) : (
              <span className={`sev-tag sev-tag--${group.severity}`}>{group.severityLabel}</span>
            )}
            <span className="finding-group__name">{group.category}</span>
            <span className="finding-group__count">{group.items.length}</span>
          </div>
          <p className="finding-group__why">{group.explanation}</p>
          <ul className="finding-items">
            {group.items.map((item) => (
              <li key={`${item.scopeLabel}:${item.streamIndex}:${item.sourceKey}`}>
                <span className="finding-scope">{item.scopeLabel}</span>
                <span className="finding-detail" title={item.detail}>
                  {item.detail}
                </span>
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ul>
  );
}

/** The expanded panel: grouped findings, or the before/after once cleaned. */
function FileDetail({ file }: { file: VideoFile }) {
  const scan = file.scan;

  if (file.scanState === "failed") {
    return (
      <div className="detail-panel">
        <p className="detail-note detail-note--warning">
          {scan?.error ?? "This file could not be inspected."}
        </p>
        <p className="detail-note">
          It can still be cleaned, but the result cannot be checked against a scan.
        </p>
      </div>
    );
  }
  if (!scan) return null;

  const verification = file.verification;
  const duration = formatDuration(scan.durationSeconds);
  const split = partitionFindings(scan.findings);
  const groups = groupFindings(split.privacy);
  const technicalGroups = groupFindings(split.technical);
  const privacyRows = verification?.beforeAfter.filter((row) => !row.technical) ?? [];
  const technicalRows = verification?.beforeAfter.filter((row) => row.technical) ?? [];

  return (
    <div className="detail-panel">
      <ul className="detail-facts">
        {duration ? <li>{duration}</li> : null}
        <li>
          {scan.videoStreams} video · {scan.audioStreams} audio
          {scan.otherStreams > 0 ? ` · ${scan.otherStreams} data` : ""}
        </li>
        {scan.chapterCount > 0 ? <li>{scan.chapterCount} chapters</li> : null}
        <li>{scan.fieldCount} metadata fields</li>
      </ul>

      {verification ? (
        <div className="before-after">
          <div className="before-after__head">
            <span>Category</span>
            <span>Before</span>
            <span>After</span>
          </div>
          {privacyRows.map((row) => (
            <div className="before-after__row" key={row.category}>
              <span className="ba-category">{row.category}</span>
              <span className="ba-before">{row.before}</span>
              <span className={row.removed ? "ba-after ba-after--removed" : "ba-after"}>
                {row.after}
              </span>
            </div>
          ))}
          {technicalRows.map((row) => (
            // Neutral colours: a container field the muxer writes back is
            // technical metadata, not a privacy failure.
            <div className="before-after__row before-after__row--technical" key={row.category}>
              <span className="ba-category">Technical metadata</span>
              <span className="ba-before">{row.before}</span>
              <span className="ba-after ba-after--technical">{row.after}</span>
            </div>
          ))}
          {!verification.verified ? (
            <ul className="check-list">
              {verification.checks
                .filter((check) => !check.passed)
                .map((check) => (
                  <li key={check.name} className="check-list__item">
                    <Icon name="warning" size={13} />
                    <span>
                      {check.name}: {check.detail}
                    </span>
                  </li>
                ))}
            </ul>
          ) : null}
        </div>
      ) : groups.length === 0 && technicalGroups.length === 0 ? (
        <p className="detail-note">This file carries no metadata to remove.</p>
      ) : (
        <div className="finding-sections">
          <p className="finding-section-title">Privacy findings</p>
          {groups.length === 0 ? (
            <p className="detail-note">No privacy findings in this file.</p>
          ) : (
            <FindingGroupList groups={groups} technical={false} />
          )}
          {technicalGroups.length > 0 ? (
            <>
              <p className="finding-section-title finding-section-title--technical">
                Technical metadata
              </p>
              <FindingGroupList groups={technicalGroups} technical />
            </>
          ) : null}
        </div>
      )}
    </div>
  );
}

type FileQueueProps = {
  files: VideoFile[];
  phase: Phase;
  done: number;
  dragging: boolean;
  expanded: string | null;
  onToggle: (path: string) => void;
  onAdd: () => void;
  onClear: () => void;
};

export function FileQueue({
  files,
  phase,
  done,
  dragging,
  expanded,
  onToggle,
  onAdd,
  onClear,
}: FileQueueProps) {
  const total = files.length;
  const running = phase === "running";
  const processingIndex = files.findIndex((file) => file.status === "processing");
  const currentPosition = processingIndex >= 0 ? processingIndex + 1 : Math.min(done + 1, total);
  const percent = total > 0 ? (done / total) * 100 : 0;
  const queueTitle = running
    ? `Cleaning ${currentPosition} of ${total}`
    : phase === "done"
      ? "Cleaning results"
      : `${total} ${total === 1 ? "video" : "videos"} ready`;

  const batch = summarise(files.flatMap((file) => (file.scan ? [file.scan] : [])));
  const batchTechnical = technicalLabel(batch.technical);
  const stillScanning = files.some(
    (file) => file.scanState === "pending" || file.scanState === "scanning",
  );
  const showBatchLine =
    phase !== "done" && (stillScanning || batch.total > 0 || batch.failed > 0);

  return (
    <section className={`queue-card${dragging ? " is-dragging" : ""}`} aria-label="Video queue">
      <div className="queue-header">
        <div className="queue-heading">
          <h2>{queueTitle}</h2>
          {!running && phase !== "done" ? <span>Up to 100 videos per batch</span> : null}
        </div>
        {!running && phase !== "done" ? (
          <div className="queue-actions">
            <button type="button" className="text-button" onClick={onAdd}>
              Add more
            </button>
            <span className="action-divider" aria-hidden="true" />
            <button type="button" className="text-button" onClick={onClear}>
              Clear
            </button>
          </div>
        ) : null}
      </div>

      {showBatchLine ? (
        <div className="batch-privacy" role="status">
          <span className="batch-privacy__label">
            <Icon name="shield" size={15} />
            Privacy scan
          </span>
          {stillScanning ? (
            <span className="batch-privacy__pending">
              Scanning {total} {total === 1 ? "video" : "videos"}...
            </span>
          ) : (
            <span className="batch-privacy__counts">
              {batch.high > 0 ? <SeverityChip severity="high" count={batch.high} /> : null}
              {batch.medium > 0 ? <SeverityChip severity="medium" count={batch.medium} /> : null}
              {batch.low > 0 ? <SeverityChip severity="low" count={batch.low} /> : null}
              <span className="scan-total">
                {batch.total === 0 ? "No privacy findings " : ""}
                across {batch.scanned} {batch.scanned === 1 ? "video" : "videos"}
              </span>
              {batchTechnical ? <span className="scan-technical">{batchTechnical}</span> : null}
              {batch.failed > 0 ? (
                <span className="batch-privacy__failed">{batch.failed} could not be scanned</span>
              ) : null}
            </span>
          )}
        </div>
      ) : null}

      {running ? (
        <div
          className="queue-progress"
          role="progressbar"
          aria-label={`${done} of ${total} videos finished`}
          aria-valuemax={total}
          aria-valuemin={0}
          aria-valuenow={done}
        >
          <span className="progress-track">
            <span className="progress-fill" style={{ width: `${percent}%` }} />
          </span>
          <span className="progress-count">{done} finished</span>
        </div>
      ) : null}

      <ul className="file-list" aria-label={`${total} selected videos`}>
        {files.map((file) => {
          const format = fileExtension(file.name)?.toUpperCase() ?? "FILE";
          const isOpen = expanded === file.path;
          const canExpand = file.scanState === "done" || file.scanState === "failed";
          // A quiet edge tint by worst finding, so a long list can be triaged
          // without opening anything.
          const worst = file.scan?.summary ? topSeverity(file.scan.summary) : null;
          const riskClass = worst ? ` file-row--risk-${worst}` : "";
          const detail =
            file.status === "error"
              ? (file.message ?? "This video could not be cleaned.")
              : file.outputName
                ? `Saved as ${file.outputName}`
                : null;

          return (
            <li
              key={file.path}
              className={`file-row file-row--${file.status}${riskClass}${isOpen ? " is-open" : ""}`}
            >
              <div className="file-row__main">
                <span className="format-indicator" aria-label={`${format} format`}>
                  {format}
                </span>
                <span className="file-copy">
                  <span className="file-name" title={file.path}>
                    {file.name}
                  </span>
                  {detail ? (
                    <span className="file-detail" title={detail}>
                      {detail}
                    </span>
                  ) : null}
                  <ScanSummaryLine file={file} />
                </span>
                <FileStatus file={file} />
                {canExpand ? (
                  <button
                    type="button"
                    className={`row-toggle${isOpen ? " is-open" : ""}`}
                    aria-expanded={isOpen}
                    aria-label={
                      isOpen ? `Hide details for ${file.name}` : `Show details for ${file.name}`
                    }
                    onClick={() => onToggle(file.path)}
                  >
                    <Icon name="chevron" size={16} />
                  </button>
                ) : (
                  <span className="row-toggle row-toggle--placeholder" aria-hidden="true" />
                )}
              </div>
              {isOpen ? <FileDetail file={file} /> : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}


type OutputSettingsProps = {
  settings: Settings | null;
  prefixDraft: string;
  previewName: string;
  prefixValid: boolean;
  running: boolean;
  onChooseFolder: () => void;
  onPrefixChange: (value: string) => void;
  onPrefixCommit: () => void;
};

export function OutputSettings({
  settings,
  prefixDraft,
  previewName,
  prefixValid,
  running,
  onChooseFolder,
  onPrefixChange,
  onPrefixCommit,
}: OutputSettingsProps) {
  const folderReady = settings?.outputDirectoryValid === true;
  const folderText = settings?.outputDirectory || "Choose an output folder";
  const showPrefixError = settings !== null && !prefixValid;

  return (
    <section className="output-settings" aria-label="Output options">
      <div className="setting-block setting-block--folder">
        <div className="setting-copy">
          <span className="setting-label">Output folder</span>
          <span className="folder-value" title={settings?.outputDirectory || folderText}>
            <Icon name="folder" size={16} />
            <span className={settings?.outputDirectory ? "folder-path" : "folder-placeholder"}>
              {folderText}
            </span>
          </span>
          {settings?.outputDirectory && !folderReady ? (
            <span className="setting-error" role="status">
              This folder is no longer available.
            </span>
          ) : null}
        </div>
        <button
          type="button"
          className="compact-button"
          onClick={onChooseFolder}
          disabled={running}
        >
          Change
        </button>
      </div>

      <div className="setting-block setting-block--naming">
        <div className="setting-copy">
          <label className="setting-label" htmlFor="prefix">
            File naming
          </label>
          <code className="naming-preview" title={previewName}>
            {previewName}
          </code>
          {showPrefixError ? (
            <span className="setting-error" id="prefix-error" role="status">
              Enter a file name prefix.
            </span>
          ) : null}
        </div>
        <input
          id="prefix"
          className="prefix-input"
          value={prefixDraft}
          aria-describedby={showPrefixError ? "prefix-error" : undefined}
          aria-invalid={showPrefixError}
          disabled={running}
          spellCheck={false}
          onBlur={onPrefixCommit}
          onChange={(event) => onPrefixChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
      </div>
    </section>
  );
}

export function InlineMessage({ tone, children }: { tone: "notice" | "error"; children: string }) {
  return (
    <p className={`inline-message inline-message--${tone}`} role={tone === "error" ? "alert" : "status"}>
      <Icon name={tone === "error" ? "warning" : "info"} size={15} />
      <span>{children}</span>
    </p>
  );
}

type CompletionSummaryProps = {
  summary: Summary;
  onOpenFolder: () => void;
  onReset: () => void;
};

export function CompletionSummary({ summary, onOpenFolder, onReset }: CompletionSummaryProps) {
  const hasErrors = summary.errors > 0;
  const verified = allVerified(summary);
  const title = completionTitle(summary);
  // The card only turns green on a clean sweep: any failure, or any file whose
  // checks did not pass, is a warning. "Completed" is never shown as success
  // when verification did not succeed.
  const isWarning = hasErrors || !verified;

  const stats: string[] = [];
  // Privacy metadata leads; technical container fields follow and are never
  // folded into the privacy figure.
  if (summary.privacyFieldsRemoved > 0) {
    stats.push(
      `${summary.privacyFieldsRemoved} privacy ${summary.privacyFieldsRemoved === 1 ? "field" : "fields"} removed`,
    );
  }
  if (summary.dataStreamsRemoved > 0) {
    stats.push(
      `${summary.dataStreamsRemoved} data ${summary.dataStreamsRemoved === 1 ? "track" : "tracks"} removed`,
    );
  }
  if (summary.chaptersRemoved > 0) {
    stats.push(
      `${summary.chaptersRemoved} chapter ${summary.chaptersRemoved === 1 ? "marker" : "markers"} removed`,
    );
  }
  if (summary.technicalFieldsRemoved > 0) {
    stats.push(
      `${summary.technicalFieldsRemoved} technical ${summary.technicalFieldsRemoved === 1 ? "field" : "fields"} removed`,
    );
  }

  return (
    <section
      className={`completion-card${isWarning ? " completion-card--warning" : ""}`}
      aria-live="polite"
    >
      <span className="completion-icon" aria-hidden="true">
        <Icon name={isWarning ? "warning" : "check"} size={22} />
      </span>
      <div className="completion-copy">
        <h2>{title}</h2>
        {verified ? (
          <p className="completion-verified" title={VERIFIED_CLEANING_DETAIL}>
            <Icon name="shield" size={14} />
            Verified cleaning
          </p>
        ) : summary.verificationFailures > 0 ? (
          <p className="completion-verified completion-verified--failed">
            <Icon name="warning" size={14} />
            Some files could not be verified. Open a row above to see which check failed.
          </p>
        ) : null}
        {stats.length > 0 ? <p className="completion-stats">{stats.join(" \u00b7 ")}</p> : null}
        <p>
          Your originals were left untouched.
          {hasErrors ? " Review the failed files above." : ""}
        </p>
      </div>
      <div className="completion-actions">
        <button type="button" className="primary-button primary-button--compact" onClick={onOpenFolder}>
          Open output folder
        </button>
        <button type="button" className="text-button" onClick={onReset}>
          Start over
        </button>
      </div>
    </section>
  );
}
