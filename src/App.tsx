import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";

const MAX_FILES = 100;

type Status = "ready" | "processing" | "completed" | "error";

type VideoFile = {
  path: string;
  name: string;
  status: Status;
  outputName?: string;
  message?: string;
};

type Progress = {
  index: number;
  total: number;
  inputName: string;
  outputName: string | null;
  status: Status;
  message: string | null;
};

type Summary = {
  outputDir: string;
  completed: number;
  errors: number;
};

type Settings = {
  prefix: string;
  outputDirectory: string;
  outputDirectoryValid: boolean;
};

type Phase = "idle" | "running" | "done";

function isSupported(path: string) {
  const lower = path.toLowerCase();
  return lower.endsWith(".mp4") || lower.endsWith(".mov");
}

function baseName(path: string) {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/// Illustration only — the real ID is assigned by the backend at processing time.
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

  const [settings, setSettings] = useState<Settings | null>(null);
  const [prefixDraft, setPrefixDraft] = useState("");
  const [previewId] = useState(demoId);

  // The drop handler is registered once, so it reads the phase through a ref
  // instead of a stale closure value.
  const phaseRef = useRef<Phase>("idle");
  phaseRef.current = phase;

  function addPaths(paths: string[]) {
    if (phaseRef.current === "running") return;
    const accepted = paths.filter(isSupported);
    const rejected = paths.length - accepted.length;

    setFiles((prev) => {
      const known = new Set(prev.map((f) => f.path));
      const fresh = accepted
        .filter((p) => !known.has(p))
        .map((p) => ({ path: p, name: baseName(p), status: "ready" as Status }));
      const merged = [...prev, ...fresh].map((f) => ({
        ...f,
        status: "ready" as Status,
        outputName: undefined,
        message: undefined,
      }));
      const capped = merged.slice(0, MAX_FILES);
      const dropped = merged.length - capped.length;
      const messages = [];
      if (rejected > 0) messages.push(rejected + " file(s) skipped - MP4 or MOV only.");
      if (dropped > 0) messages.push(dropped + " file(s) skipped - limit is " + MAX_FILES + ".");
      setNotice(messages.length > 0 ? messages.join(" ") : null);
      return capped;
    });
    setPhase("idle");
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
      .then((un) => {
        unlistenDrag = un;
      });

    listen<Progress>("clean-progress", ({ payload }) => {
      setFiles((prev) =>
        prev.map((f, i) =>
          i === payload.index
            ? {
                ...f,
                status: payload.status,
                outputName: payload.outputName ?? undefined,
                message: payload.message ?? undefined,
              }
            : f,
        ),
      );
      if (payload.status !== "processing") {
        setDone(payload.index + 1);
      }
    }).then((un) => {
      unlistenProgress = un;
    });

    invoke<string | null>("check_ffmpeg").then((problem) => {
      if (problem) setError(problem);
    });

    invoke<Settings>("get_settings").then((loaded) => {
      setSettings(loaded);
      setPrefixDraft(loaded.prefix);
    });

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
    } catch (e) {
      setError(String(e));
    }
  }

  async function chooseOutputFolder() {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string") {
      // Fall back to the saved prefix so an empty text box cannot make picking a
      // folder fail.
      const prefix = prefixDraft.trim() || settings?.prefix || "";
      await persist(prefix, picked);
    }
  }

  function commitPrefix() {
    if (!settings) return;
    if (prefixDraft.trim() === settings.prefix) return;
    persist(prefixDraft, settings.outputDirectory);
  }

  async function selectVideos() {
    const picked = await open({
      multiple: true,
      filters: [{ name: "Videos", extensions: ["mp4", "mov"] }],
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
    setFiles((prev) =>
      prev.map((f) => ({ ...f, status: "ready", outputName: undefined, message: undefined })),
    );
    try {
      const result = await invoke<Summary>("clean_videos", {
        paths: files.map((f) => f.path),
      });
      setSummary(result);
      setPhase("done");
    } catch (e) {
      setError(String(e));
      setPhase("idle");
      // The folder may have vanished since startup; re-read so the UI reflects it.
      invoke<Settings>("get_settings").then(setSettings);
    }
  }

  function reset() {
    setFiles([]);
    setPhase("idle");
    setDone(0);
    setSummary(null);
    setError(null);
    setNotice(null);
  }

  const running = phase === "running";
  const total = files.length;
  const percent = total > 0 ? (done / total) * 100 : 0;
  const prefixValid = prefixDraft.trim().length > 0;
  const folderReady = settings?.outputDirectoryValid === true;
  const canClean = total > 0 && !running && folderReady && prefixValid;

  return (
    <main className="app">
      <header className="header">
        <h1>MetaStrip Video</h1>
        <p className="subtitle">Video Metadata Remover</p>
        <p>Clean metadata from your videos locally without re-encoding.</p>
      </header>

      <section className="config">
        <div className="field">
          <label className="field-label" htmlFor="prefix">
            FILE NAME
          </label>
          <input
            id="prefix"
            className="prefix-input"
            value={prefixDraft}
            disabled={running}
            spellCheck={false}
            onChange={(e) => setPrefixDraft(e.target.value)}
            onBlur={commitPrefix}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
          />
          <p className="field-hint">
            {prefixValid ? (
              <>
                Preview: {prefixDraft.trim()}_{previewId}.mp4
              </>
            ) : (
              <span className="bad">The file name prefix cannot be empty.</span>
            )}
          </p>
        </div>

        <div className="field">
          <span className="field-label">OUTPUT FOLDER</span>
          <div className="folder-row">
            {settings?.outputDirectory ? (
              // Right-to-left keeps the tail of a long path visible; it is only
              // safe on a real path, since it would also reorder trailing
              // punctuation in ordinary prose.
              <span className={folderReady ? "folder-path" : "folder-path bad"}>
                {settings.outputDirectory}
              </span>
            ) : (
              <span className="folder-empty">No folder chosen yet.</span>
            )}
            <button className="btn secondary small" onClick={chooseOutputFolder} disabled={running}>
              Change
            </button>
          </div>
          {settings && !folderReady && settings.outputDirectory && (
            <p className="field-hint bad">
              This folder no longer exists. Choose a new one.
            </p>
          )}
        </div>
      </section>

      {total === 0 ? (
        <section className={dragging ? "dropzone dragging" : "dropzone"}>
          <p className="drop-title">DROP VIDEOS HERE</p>
          <p className="drop-sub">MP4 or MOV</p>
          <p className="drop-sub">Up to {MAX_FILES} videos</p>
          <button className="btn secondary" onClick={selectVideos}>
            Select videos
          </button>
        </section>
      ) : (
        <section className={dragging ? "panel dragging" : "panel"}>
          <div className="panel-head">
            <span className="count">
              {phase === "done"
                ? "Cleaning complete"
                : running
                  ? "Cleaning videos..."
                  : total + (total === 1 ? " video selected" : " videos selected")}
            </span>
            {!running && (
              <div className="panel-actions">
                <button className="link" onClick={selectVideos}>
                  Add more
                </button>
                <button className="link" onClick={reset}>
                  Clear
                </button>
              </div>
            )}
          </div>

          {running && (
            <div className="progress">
              <div className="bar">
                <div className="fill" style={{ width: percent + "%" }} />
              </div>
              <span className="progress-text">
                {done} / {total}
              </span>
            </div>
          )}

          <ul className="list">
            {files.map((f) => (
              <li key={f.path} className={"row " + f.status}>
                <span className="name" title={f.path}>
                  {f.name}
                  {f.outputName && <span className="renamed"> → {f.outputName}</span>}
                </span>
                <span className="status" title={f.message ?? ""}>
                  {f.status === "ready" && "Ready"}
                  {f.status === "processing" && "Processing..."}
                  {f.status === "completed" && "✓"}
                  {f.status === "error" && (f.message ?? "Error")}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {notice && <p className="notice">{notice}</p>}
      {error && <p className="error-box">{error}</p>}

      {summary && phase === "done" && (
        <div className="summary">
          <p className="ok">{summary.completed} completed</p>
          <p className={summary.errors > 0 ? "bad" : "muted"}>
            {summary.errors} {summary.errors === 1 ? "error" : "errors"}
          </p>
          <div className="summary-actions">
            <button
              className="btn secondary"
              onClick={() => invoke("open_folder", { path: summary.outputDir })}
            >
              Open output folder
            </button>
            <button className="link" onClick={reset}>
              Start over
            </button>
          </div>
        </div>
      )}

      {total > 0 && phase !== "done" && (
        <button className="btn primary" onClick={cleanVideos} disabled={!canClean}>
          {running
            ? "Cleaning " + done + " / " + total
            : "Clean " + total + (total === 1 ? " video" : " videos")}
        </button>
      )}
    </main>
  );
}
