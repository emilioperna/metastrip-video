import type { ReactNode } from "react";
import { fileExtension, type SupportedFormat } from "../formats";

export type Status = "ready" | "processing" | "completed" | "error";

export type VideoFile = {
  path: string;
  name: string;
  status: Status;
  outputName?: string;
  message?: string;
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
};

type IconName = "brand" | "video" | "folder" | "check" | "warning" | "info" | "failed";

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
        <p>Clean videos locally without re-encoding.</p>
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
    return (
      <span className="file-status file-status--completed" title="Video cleaned">
        <Icon name="check" size={15} />
        Cleaned
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

type FileQueueProps = {
  files: VideoFile[];
  phase: Phase;
  done: number;
  dragging: boolean;
  onAdd: () => void;
  onClear: () => void;
};

export function FileQueue({ files, phase, done, dragging, onAdd, onClear }: FileQueueProps) {
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
          const detail =
            file.status === "error"
              ? file.message ?? "This video could not be cleaned."
              : file.outputName
                ? `Saved as ${file.outputName}`
                : null;

          return (
            <li key={file.path} className={`file-row file-row--${file.status}`}>
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
              </span>
              <FileStatus file={file} />
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
  const title = hasErrors
    ? `${summary.completed} cleaned \u00b7 ${summary.errors} failed`
    : `${summary.completed} ${summary.completed === 1 ? "video" : "videos"} cleaned`;

  return (
    <section
      className={`completion-card${hasErrors ? " completion-card--warning" : ""}`}
      aria-live="polite"
    >
      <span className="completion-icon" aria-hidden="true">
        <Icon name={hasErrors ? "warning" : "check"} size={22} />
      </span>
      <div className="completion-copy">
        <h2>{title}</h2>
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
