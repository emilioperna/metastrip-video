import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CompletionSummary,
  DropZone,
  FileQueue,
  InlineMessage,
  OutputSettings,
  ProductHeader,
  type Phase,
  type Settings,
  type Status,
  type Summary,
  type VideoFile,
} from "./components/Workflow";
import {
  fileExtension,
  partitionSupported,
  supportedFormatLabels,
  type SupportedFormat,
} from "./formats";
import {
  CLEANING_ELSEWHERE_MESSAGE,
  CLOSE_BLOCKED_MESSAGE,
  IDLE_PROCESSING,
  PROCESSING_POLL_MS,
  UNKNOWN_PROCESSING,
  applyProgress,
  attach,
  cleaningElsewhere,
  pipelineBusy,
  type BatchAttachment,
  type ProcessingState,
} from "./lifecycle";
import {
  EMPTY_SUMMARY,
  STREAM_COPY_NOTE,
  type ScanView,
  type VerificationReport,
} from "./privacy";
import { statusText } from "./updater";
import { useUpdater } from "./useUpdater";

const MAX_FILES = 100;

type Progress = {
  batchId: number;
  index: number;
  total: number;
  inputName: string;
  outputName: string | null;
  status: Status;
  message: string | null;
  verification: VerificationReport | null;
};

type ScanProgress = {
  index: number;
  total: number;
  view: ScanView;
};

/** A placeholder result for a file whose scan never produced one. */
function emptyScan(path: string, name: string): ScanView {
  return {
    path,
    name,
    ok: false,
    error: null,
    summary: { ...EMPTY_SUMMARY },
    findings: [],
    container: null,
    durationSeconds: null,
    videoStreams: 0,
    audioStreams: 0,
    otherStreams: 0,
    chapterCount: 0,
    fieldCount: 0,
    plan: null,
  };
}

function baseName(path: string) {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

// Illustration only. The backend reserves the real ID when processing starts.
function demoId() {
  return String(Math.floor(Math.random() * 10_000_000_000)).padStart(10, "0");
}

export default function App() {
  const [files, setFiles] = useState<VideoFile[]>([]);
  const [phase, setPhase] = useState<Phase>("idle");
  const [dragging, setDragging] = useState(false);
  const [done, setDone] = useState(0);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [supportedFormats, setSupportedFormats] = useState<SupportedFormat[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [prefixDraft, setPrefixDraft] = useState("");
  const [previewId] = useState(demoId);
  // At most one row is open at a time. With a hundred files, letting every row
  // expand at once is what turns a usable list into an unreadable wall.
  const [expanded, setExpanded] = useState<string | null>(null);
  // What the backend last said about the pipeline. The phase above is this
  // page's own story and a reload rewrites it; this one survives, because it is
  // not stored here.
  const [processing, setProcessing] = useState<ProcessingState>(IDLE_PROCESSING);
  const [closeBlocked, setCloseBlocked] = useState(false);

  // Native listeners are registered once and read changing values through refs.
  const phaseRef = useRef<Phase>("idle");
  phaseRef.current = phase;
  // The batch whose progress this page may apply. Null until it starts one, so
  // a page that has just loaded applies nothing.
  const attachment = useRef<BatchAttachment | null>(null);
  // Held from the click until the batch is over. `phase` cannot do this job any
  // more: asking the backend takes a round trip, so there is now a gap between
  // the click and `phase` becoming "running" in which a second click would be
  // taken as a second batch.
  const cleanInFlight = useRef(false);
  const supportedFormatsRef = useRef<SupportedFormat[]>([]);
  // Paths already sent to the scanner, so re-adding a file does not re-probe it.
  const scannedPaths = useRef<Set<string>>(new Set());

  /**
   * Ask the pipeline whether it is busy. Everything whose correctness depends
   * on a batch being over goes through here rather than through `phase`, which
   * a reload resets while the batch it described carries on in the backend.
   */
  const readProcessing = useCallback(async (): Promise<ProcessingState> => {
    try {
      const state = await invoke<ProcessingState>("get_processing_state");
      setProcessing(state);
      return state;
    } catch (reason) {
      // Nothing else would be working either: this command only reads a flag.
      // The caller is told "busy", which is the answer that keeps the three
      // things that must not happen mid-batch -- another Clean, an update
      // install, a close -- shut.
      console.warn("[lifecycle] could not read the processing state:", reason);
      return UNKNOWN_PROCESSING;
    }
  }, []);

  function addPaths(paths: string[]) {
    if (phaseRef.current === "running") return;
    const formats = supportedFormatsRef.current;
    if (formats.length === 0) {
      setNotice("Supported formats are still loading. Try again in a moment.");
      return;
    }

    const { accepted, rejected } = partitionSupported(paths, formats);
    setFiles((previous) => {
      const known = new Set(previous.map((file) => file.path));
      const fresh = accepted
        .filter((path) => !known.has(path))
        .map((path) => ({
          path,
          name: baseName(path),
          status: "ready" as Status,
          scanState: "pending" as const,
        }));
      // Re-adding clears any previous run's result but keeps the scan already
      // paid for: the file on disk has not changed just because the queue did.
      const reset = previous.map((file) => ({
        path: file.path,
        name: file.name,
        status: "ready" as Status,
        scanState: file.scanState,
        scan: file.scan,
      }));
      const merged = [...reset, ...fresh];
      const capped = merged.slice(0, MAX_FILES);
      const overLimit = merged.length - capped.length;
      const messages: string[] = [];

      if (rejected.length > 0) {
        const noun = rejected.length === 1 ? "file was" : "files were";
        messages.push(
          `${rejected.length} unsupported ${noun} skipped. Supported: ${supportedFormatLabels(formats)}.`,
        );
      }
      if (overLimit > 0) {
        const noun = overLimit === 1 ? "file was" : "files were";
        messages.push(`${overLimit} ${noun} skipped. Batches are limited to ${MAX_FILES} videos.`);
      }
      setNotice(messages.length > 0 ? messages.join(" ") : null);
      return capped;
    });

    setPhase("idle");
    setDone(0);
    setSummary(null);
    setError(null);
    setExpanded(null);

    const fresh = accepted.filter((path) => !scannedPaths.current.has(path));
    if (fresh.length > 0) void scanPaths(fresh.slice(0, MAX_FILES));
  }

  /**
   * Inspect newly added files. Results stream back through `scan-progress`, so
   * rows fill in as they land rather than all at the end.
   *
   * A scan never blocks cleaning: a file that cannot be inspected is marked and
   * the batch carries on.
   */
  async function scanPaths(paths: string[]) {
    for (const path of paths) scannedPaths.current.add(path);
    setFiles((previous) =>
      previous.map((file) =>
        paths.includes(file.path) ? { ...file, scanState: "scanning" as const } : file,
      ),
    );
    try {
      await invoke<ScanView[]>("scan_videos", { paths });
    } catch (reason) {
      // The whole scan failed (no ffprobe, batch too large). Mark exactly the
      // files this call owned, so an earlier successful scan is not discarded.
      const message = String(reason);
      for (const path of paths) scannedPaths.current.delete(path);
      setFiles((previous) =>
        previous.map((file) =>
          paths.includes(file.path)
            ? {
                ...file,
                scanState: "failed" as const,
                scan: { ...emptyScan(file.path, file.name), error: message },
              }
            : file,
        ),
      );
    }
  }

  useEffect(() => {
    let unlistenDrag: (() => void) | undefined;
    let unlistenProgress: (() => void) | undefined;
    let unlistenScan: (() => void) | undefined;
    let unlistenClose: (() => void) | undefined;

    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "over") {
          setDragging(true);
        } else if (event.payload.type === "drop") {
          setDragging(false);
          addPaths(event.payload.paths);
        } else {
          setDragging(false);
        }
      })
      .then((unlisten) => {
        unlistenDrag = unlisten;
      });

    listen<Progress>("clean-progress", ({ payload }) => {
      // An event from a batch this page did not start -- one that outlived a
      // reload, or one that finished before the current batch began -- names a
      // different batch, and rows here have nothing to do with its files.
      const claim = applyProgress(attachment.current, payload.batchId);
      attachment.current = claim.attachment;
      if (!claim.accepted) return;

      setFiles((previous) =>
        previous.map((file, index) =>
          index === payload.index
            ? {
                ...file,
                status: payload.status,
                outputName: payload.outputName ?? undefined,
                message: payload.message ?? undefined,
                verification: payload.verification ?? undefined,
              }
            : file,
        ),
      );
      if (payload.status !== "processing") setDone(payload.index + 1);
    }).then((unlisten) => {
      unlistenProgress = unlisten;
    });

    listen<ScanProgress>("scan-progress", ({ payload }) => {
      // Matched by path, not index: the queue can change while a scan runs.
      setFiles((previous) =>
        previous.map((file) =>
          file.path === payload.view.path
            ? {
                ...file,
                scanState: payload.view.ok ? ("done" as const) : ("failed" as const),
                scan: payload.view,
              }
            : file,
        ),
      );
    }).then((unlisten) => {
      unlistenScan = unlisten;
    });

    // Refused by the backend because a batch is running. The window stays open
    // and says so rather than leaving FFmpeg writing into a folder nobody is
    // watching any more.
    listen("close-blocked", () => setCloseBlocked(true)).then((unlisten) => {
      unlistenClose = unlisten;
    });

    // What a reload cannot tell this page: a batch started before it may still
    // be running -- and an update install claimed before the reload is this
    // page's to give back, because the page that took it no longer exists and a
    // claim that succeeded would have ended the process instead of reloading it.
    void invoke("release_update_install")
      .catch((reason) => console.warn("[lifecycle] stale install claim kept:", reason))
      .finally(() => void readProcessing());

    invoke<string | null>("check_ffmpeg")
      .then((problem) => {
        if (problem) setError(problem);
      })
      .catch((reason) => setError(String(reason)));

    invoke<SupportedFormat[]>("get_supported_formats")
      .then((formats) => {
        if (formats.length === 0) throw new Error("The backend returned no supported formats.");
        supportedFormatsRef.current = formats;
        setSupportedFormats(formats);
      })
      .catch((reason) => setError(`Could not load the supported formats: ${String(reason)}`));

    invoke<Settings>("get_settings")
      .then((loaded) => {
        setSettings(loaded);
        setPrefixDraft(loaded.prefix);
      })
      .catch((reason) => setError(String(reason)));

    return () => {
      unlistenDrag?.();
      unlistenProgress?.();
      unlistenScan?.();
      unlistenClose?.();
    };
  }, []);

  // A batch this page is not showing -- one it started before a reload, or one
  // it was refused a turn behind -- is watched until it ends, because nothing
  // else will say so. The page's own batch reports through its Clean call.
  useEffect(() => {
    if (!processing.cleaning || phase === "running") return;
    const timer = setInterval(() => void readProcessing(), PROCESSING_POLL_MS);
    return () => clearInterval(timer);
  }, [processing.cleaning, phase, readProcessing]);

  async function persist(prefix: string, outputDirectory: string) {
    try {
      const saved = await invoke<Settings>("save_settings", { prefix, outputDirectory });
      setSettings(saved);
      setPrefixDraft(saved.prefix);
      setError(null);
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function chooseOutputFolder() {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string") {
      const prefix = prefixDraft.trim() || settings?.prefix || "";
      await persist(prefix, picked);
    }
  }

  function commitPrefix() {
    if (!settings || prefixDraft.trim() === settings.prefix) return;
    void persist(prefixDraft, settings.outputDirectory);
  }

  async function selectVideos() {
    if (supportedFormats.length === 0) return;
    const picked = await open({
      multiple: true,
      filters: [
        {
          name: "Videos",
          extensions: supportedFormats.map((format) => format.extension),
        },
      ],
    });
    if (Array.isArray(picked)) addPaths(picked);
    else if (typeof picked === "string") addPaths([picked]);
  }

  async function cleanVideos() {
    if (cleanInFlight.current) return;
    cleanInFlight.current = true;
    try {
      await runBatch();
    } finally {
      cleanInFlight.current = false;
    }
  }

  async function runBatch() {
    // The backend decides, not this page: a batch it started before a reload is
    // still running even though nothing here remembers it. Asking also fixes
    // the batch this page is about to own -- anything newer than what the
    // backend reports here is the batch this call is about to start, and
    // nothing older can be.
    const before = await readProcessing();
    if (pipelineBusy(before)) return;
    attachment.current = attach(before);

    setPhase("running");
    setDone(0);
    setError(null);
    setNotice(null);
    setSummary(null);
    setExpanded(null);
    setCloseBlocked(false);
    setFiles((previous) =>
      previous.map((file) => ({
        path: file.path,
        name: file.name,
        status: "ready" as Status,
        // The scan is still valid; only the previous run's outcome is dropped.
        scanState: file.scanState,
        scan: file.scan,
      })),
    );

    try {
      const result = await invoke<Summary>("clean_videos", {
        paths: files.map((file) => file.path),
      });
      setSummary(result);
      setPhase("done");
      // Deliberately still attached: the last file's event and this answer
      // race, and dropping the claim now would leave its row mid-flight.
    } catch (reason) {
      // Nothing was started, or what was started is over: either way no further
      // event can belong to this page, so it stops claiming one.
      attachment.current = null;
      setError(String(reason));
      setPhase("idle");
      invoke<Settings>("get_settings").then(setSettings).catch(() => undefined);
    }
    void readProcessing();
  }

  function reset() {
    setFiles([]);
    setPhase("idle");
    setDragging(false);
    setDone(0);
    setSummary(null);
    setError(null);
    setNotice(null);
    setExpanded(null);
    setCloseBlocked(false);
    scannedPaths.current.clear();
  }

  const running = phase === "running";
  // A batch is running that this page is not showing: it started one and then
  // reloaded, or it was refused a turn behind. Cleaning is sequential, so there
  // is nothing to do but say so and wait.
  const elsewhere = cleaningElsewhere(processing, attachment.current);
  // Anything that must not happen mid-batch asks this, never `running` alone.
  const busy = running || pipelineBusy(processing);
  const updateStatus = useUpdater(busy, readProcessing);
  const updateText = statusText(updateStatus, busy);
  const total = files.length;
  const prefixValid = prefixDraft.trim().length > 0;
  const folderReady = settings?.outputDirectoryValid === true;
  const canClean = total > 0 && !busy && folderReady && prefixValid;
  const selectedExtension = total > 0 ? fileExtension(files[0].name) : null;
  const previewName = !prefixValid
    ? "Enter a prefix"
    : selectedExtension
      ? `${prefixDraft.trim()}_${previewId}.${selectedExtension}`
      : `${prefixDraft.trim()}_${previewId}`;
  const actionNote = !folderReady
    ? "Choose an available output folder to continue."
    : !prefixValid
      ? "Enter a file name prefix to continue."
      : STREAM_COPY_NOTE;
  // Shown only while the batch it refers to is still running; a close that was
  // refused a batch ago is not news.
  const closeWarning = closeBlocked && busy;

  return (
    <main className={`app-shell app-shell--${phase}`} aria-busy={busy}>
      <ProductHeader />

      <div className={`workflow-stage${total === 0 ? " workflow-stage--empty" : ""}`}>
        {total === 0 ? (
          <DropZone dragging={dragging} formats={supportedFormats} onChoose={selectVideos} />
        ) : (
          <FileQueue
            files={files}
            phase={phase}
            done={done}
            dragging={dragging}
            expanded={expanded}
            onToggle={(path) => setExpanded((current) => (current === path ? null : path))}
            onAdd={selectVideos}
            onClear={reset}
          />
        )}
      </div>

      {(elsewhere || closeWarning || notice || error) && (
        <div className="message-stack">
          {elsewhere ? (
            <InlineMessage tone="notice">{CLEANING_ELSEWHERE_MESSAGE}</InlineMessage>
          ) : null}
          {closeWarning ? <InlineMessage tone="error">{CLOSE_BLOCKED_MESSAGE}</InlineMessage> : null}
          {notice ? <InlineMessage tone="notice">{notice}</InlineMessage> : null}
          {error ? <InlineMessage tone="error">{error}</InlineMessage> : null}
        </div>
      )}

      {summary && phase === "done" ? (
        <CompletionSummary
          summary={summary}
          onOpenFolder={() => {
            void invoke("open_folder", { path: summary.outputDir }).catch((reason) =>
              setError(String(reason)),
            );
          }}
          onReset={reset}
        />
      ) : (
        <>
          <OutputSettings
            settings={settings}
            prefixDraft={prefixDraft}
            previewName={previewName}
            prefixValid={prefixValid}
            running={busy}
            onChooseFolder={chooseOutputFolder}
            onPrefixChange={setPrefixDraft}
            onPrefixCommit={commitPrefix}
          />

          {total > 0 && !busy ? (
            <div className="primary-action-row">
              <p className={canClean ? "action-note" : "action-note action-note--warning"}>
                {actionNote}
              </p>
              <button
                type="button"
                className="primary-button"
                onClick={cleanVideos}
                disabled={!canClean}
              >
                {`Clean ${total} ${total === 1 ? "video" : "videos"}`}
              </button>
            </div>
          ) : null}
        </>
      )}

      {updateText ? (
        <p className="update-status" role="status">
          <span className="update-dot" aria-hidden="true" />
          {updateText}
        </p>
      ) : null}
    </main>
  );
}
