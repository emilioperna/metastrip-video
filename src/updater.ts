// Update state, kept free of Tauri imports so it can be unit-tested on its own.
// The rule this file exists to enforce: an update never interrupts a batch.

export type UpdaterStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "downloading"; version: string }
  | { kind: "ready"; version: string }
  | { kind: "installing"; version: string };

export const IDLE: UpdaterStatus = { kind: "idle" };

/** How often to look again while the app stays open. */
export const CHECK_INTERVAL_MS = 60 * 60 * 1000;

/** The slice of the plugin's `Update` this module needs. */
export type UpdateHandle = {
  version: string;
  download: () => Promise<void>;
};

/**
 * An update that finished downloading may only be installed once nothing is
 * being processed. Installing on Windows closes the app, so doing it mid-batch
 * would abandon the remaining videos.
 */
export function canInstall(status: UpdaterStatus, batchRunning: boolean): boolean {
  return status.kind === "ready" && !batchRunning;
}

/**
 * Whether a fresh check is worth starting. One is skipped while another is in
 * flight, and once an update is downloaded there is nothing left to look for.
 */
export function shouldCheck(status: UpdaterStatus): boolean {
  return status.kind === "idle";
}

/**
 * One check-and-download cycle. Returns the update to install later, or null.
 *
 * Never throws. GitHub unreachable, a malformed latest.json, a signature that
 * does not verify, a download that dies halfway: all of them end the same way,
 * back on idle with the app untouched. An update that cannot be fetched is not
 * a reason to bother anyone.
 *
 * `check` is injected rather than imported so the failure paths are testable
 * without standing up the plugin.
 */
export async function checkAndDownload(
  check: () => Promise<UpdateHandle | null>,
  onStatus: (status: UpdaterStatus) => void,
  onError: (e: unknown) => void = () => {},
): Promise<UpdateHandle | null> {
  onStatus({ kind: "checking" });
  try {
    const update = await check();
    if (!update) {
      // Already current. The version comparison is the plugin's, and it only
      // ever moves forward.
      onStatus(IDLE);
      return null;
    }
    onStatus({ kind: "downloading", version: update.version });
    // Signature verification happens in here; a bad one throws.
    await update.download();
    onStatus({ kind: "ready", version: update.version });
    return update;
  } catch (e) {
    onError(e);
    onStatus(IDLE);
    return null;
  }
}

/**
 * The one line the window shows. `null` means show nothing: no update, or a
 * check that failed because GitHub was unreachable.
 */
export function statusText(status: UpdaterStatus, batchRunning: boolean): string | null {
  switch (status.kind) {
    case "idle":
      return null;
    case "checking":
      return "Checking for updates…";
    case "downloading":
      return "Downloading MetaStrip Video v" + status.version + "…";
    case "ready":
      return batchRunning
        ? "Update v" + status.version + " ready — it will install after the current batch."
        : "Update v" + status.version + " ready — installing…";
    case "installing":
      return "Installing MetaStrip Video v" + status.version + "…";
  }
}
