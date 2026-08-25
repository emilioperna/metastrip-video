import { useEffect, useRef, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  CHECK_INTERVAL_MS,
  IDLE,
  canInstall,
  checkAndDownload,
  shouldCheck,
  type UpdaterStatus,
} from "./updater";

/**
 * Check on start, then hourly. Downloading is separate from installing: the
 * bytes are fetched as soon as they exist, but the installer only runs when no
 * batch is in flight, because on Windows installing closes the app.
 */
export function useUpdater(batchRunning: boolean): UpdaterStatus {
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
    setStatus({ kind: "installing", version: update.version });

    (async () => {
      try {
        await update.install();
        await relaunch();
      } catch (e) {
        // A failed install leaves a working app on the old version.
        console.warn("[updater] install skipped:", e);
        pending.current = null;
        installing.current = false;
        setStatus(IDLE);
      }
    })();
  }, [status, batchRunning]);

  return status;
}
