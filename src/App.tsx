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
  message?: string;
};

type Progress = {
  index: number;
  total: number;
  name: string;
  status: Status;
  message: string | null;
};

type Summary = {
  outputDir: string;
  completed: number;
  errors: number;
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

export default function App() {
  const [files, setFiles] = useState<VideoFile[]>([]);
  const [phase, setPhase] = useState<Phase>("idle");
  const [dragging, setDragging] = useState(false);
  const [done, setDone] = useState(0);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

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
            ? { ...f, status: payload.status, message: payload.message ?? undefined }
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

    return () => {
      unlistenDrag?.();
      unlistenProgress?.();
    };
  }, []);

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
    setFiles((prev) => prev.map((f) => ({ ...f, status: "ready", message: undefined })));
    try {
      const result = await invoke<Summary>("clean_videos", {
        paths: files.map((f) => f.path),
      });
      setSummary(result);
      setPhase("done");
    } catch (e) {
      setError(String(e));
      setPhase("idle");
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

  return (
    <main className="app">
      <header className="header">
        <h1>Aurevm Video Cleaner</h1>
        <p>Clean metadata from your videos locally.</p>
      </header>

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
        <button className="btn primary" onClick={cleanVideos} disabled={running}>
          {running
            ? "Cleaning " + done + " / " + total
            : "Clean " + total + (total === 1 ? " video" : " videos")}
        </button>
      )}
    </main>
  );
}
