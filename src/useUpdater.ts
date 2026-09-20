import { useEffect, useRef, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type { ProcessingState } from "./lifecycle";
import {
  CHECK_INTERVAL_MS,
  IDLE,
  canInstall,
  checkAndDownload,
  installApproved,
  shouldCheck,
  type UpdaterStatus,
} from "./updater";

/**
 * Check on start, then hourly. Downloading is deliberately separate from
 * installing: installing on Windows closes the app, so a batch in flight would
 * lose its remaining videos. The bytes are fetched as soon as they exist, the
 * installer runs only once nothing is being processed.
 *
 * `batchRunning` is the cheap gate and `readProcessing` is the authoritative
 * one: the page can be a reload behind the pipeline, so the backend is asked
 * again in the moment before the installer is handed over. `readProcessing` is
 * the window's own, so an install deferred here also tells the rest of the
 * window that a batch is running, and the answer that clears it comes back
 * through `batchRunning`.
 */
export function useUpdater(
  batchRunning: boolean,
  readProcessing: () => Promise<ProcessingState>,
): UpdaterStatus {
  const [status, setStatus] = useState<UpdaterStatus>(IDLE);

  // The downloaded update, held until a batch-free moment to install it.
  const pending = useRef<Update | null>(null);
  // Lets the interval callback read the live status without re-registering.
  const statusRef = useRef<UpdaterStatus>(IDLE);
  statusRef.current = status;
  const installing = useRef(false);

  useEffect(() => {
    let cancelled = false;
    const publish = (next: UpdaterStatus) => {
      if (!cancelled) setStatus(next);
    };

    async function cycle() {
      if (cancelled || !shouldCheck(statusRef.current)) return;
      const update = await checkAndDownload(check, publish, (e) =>
        console.warn("[updater] check/download skipped:", e),
      );
      if (update && !cancelled) pending.current = update as Update;
    }

    void cycle();
    const timer = setInterval(() => void cycle(), CHECK_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Runs again whenever a batch ends, which is what lets a deferred update
  // through.
  useEffect(() => {
    if (installing.current) return;
    if (!canInstall(status, batchRunning)) return;
    const update = pending.current;
    if (!update) return;

    installing.current = true;

    (async () => {
      // Asked here rather than trusting the render: a page that reloaded
      // mid-batch starts out believing nothing is running. Left on `ready`, so
      // the next answer that says the pipeline is free installs it.
      const backend = await readProcessing();
      if (!installApproved(status, batchRunning, backend)) {
        installing.current = false;
        return;
      }
      setStatus({ kind: "installing", version: update.version });
      try {
        // On Windows this hands the installer to the shell and ends the
        // process: nothing after it runs, and the NSIS `/R` flag that
        // `installMode: "passive"` sets is what starts the new version. So
        // there is deliberately no relaunch call here -- it would be dead code.
        await update.install();
      } catch (e) {
        // Only reachable if the installer never got started. A failed install
        // leaves a working app on the old version.
        console.warn("[updater] install skipped:", e);
        pending.current = null;
        installing.current = false;
        setStatus(IDLE);
      }
    })();
  }, [status, batchRunning, readProcessing]);

  return status;
}
