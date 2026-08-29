import { useEffect, useRef, useState } from "react";
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
import { statusText } from "./updater";
import { useUpdater } from "./useUpdater";

const MAX_FILES = 100;

type Progress = {
  index: number;
  total: number;
  inputName: string;
  outputName: string | null;
  status: Status;
  message: string | null;
};

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

  // Native listeners are registered once and read changing values through refs.
  const phaseRef = useRef<Phase>("idle");
  phaseRef.current = phase;
  const supportedFormatsRef = useRef<SupportedFormat[]>([]);

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
        .map((path) => ({ path, name: baseName(path), status: "ready" as Status }));
      const reset = previous.map((file) => ({
        path: file.path,
        name: file.name,
        status: "ready" as Status,
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
  }

  useEffect(() => {
    let unlistenDrag: (() => void) | undefined;
    let unlistenProgress: (() => void) | undefined;

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
      setFiles((previous) =>
        previous.map((file, index) =>
          index === payload.index
            ? {
                ...file,
                status: payload.status,
                outputName: payload.outputName ?? undefined,
                message: payload.message ?? undefined,
              }
            : file,
        ),
      );
      if (payload.status !== "processing") setDone(payload.index + 1);
    }).then((unlisten) => {
      unlistenProgress = unlisten;
    });

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
    };
  }, []);

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
    setPhase("running");
    setDone(0);
    setError(null);
    setNotice(null);
    setSummary(null);
    setFiles((previous) =>
      previous.map((file) => ({
        path: file.path,
        name: file.name,
        status: "ready",
      })),
    );

    try {
      const result = await invoke<Summary>("clean_videos", {
        paths: files.map((file) => file.path),
      });
      setSummary(result);
      setPhase("done");
    } catch (reason) {
      setError(String(reason));
      setPhase("idle");
      invoke<Settings>("get_settings").then(setSettings).catch(() => undefined);
    }
  }

  function reset() {
    setFiles([]);
    setPhase("idle");
    setDragging(false);
    setDone(0);
    setSummary(null);
    setError(null);
    setNotice(null);
  }

  const running = phase === "running";
  const updateStatus = useUpdater(running);
  const updateText = statusText(updateStatus, running);
  const total = files.length;
  const prefixValid = prefixDraft.trim().length > 0;
  const folderReady = settings?.outputDirectoryValid === true;
  const canClean = total > 0 && !running && folderReady && prefixValid;
  const selectedExtension = total > 0 ? fileExtension(files[0].name) : null;
  const previewName = prefixValid
    ? `${prefixDraft.trim()}_${previewId}.${selectedExtension ?? "[format]"}`
    : "Enter a prefix";
  const processingIndex = files.findIndex((file) => file.status === "processing");
  const processingPosition = processingIndex >= 0 ? processingIndex + 1 : Math.min(done + 1, total);
  const actionNote = !folderReady
    ? "Choose an available output folder to continue."
    : !prefixValid
      ? "Enter a file name prefix to continue."
      : "Metadata is removed locally. Video and audio streams are copied unchanged.";

  return (
    <main className={`app-shell app-shell--${phase}`} aria-busy={running}>
      <ProductHeader />

      <div className="workflow-stage">
        {total === 0 ? (
          <DropZone dragging={dragging} formats={supportedFormats} onChoose={selectVideos} />
        ) : (
          <FileQueue
            files={files}
            phase={phase}
            done={done}
            dragging={dragging}
            onAdd={selectVideos}
            onClear={reset}
          />
        )}
      </div>

      {(notice || error) && (
        <div className="message-stack">
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
            running={running}
            onChooseFolder={chooseOutputFolder}
            onPrefixChange={setPrefixDraft}
            onPrefixCommit={commitPrefix}
          />

          {total > 0 ? (
            <div className="primary-action-row">
              <p className={canClean || running ? "action-note" : "action-note action-note--warning"}>
                {actionNote}
              </p>
              <button
                type="button"
                className="primary-button"
                onClick={cleanVideos}
                disabled={!canClean}
              >
                {running
                  ? `Cleaning ${processingPosition} of ${total}`
                  : `Clean ${total} ${total === 1 ? "video" : "videos"}`}
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
