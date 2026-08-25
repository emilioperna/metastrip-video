import { describe, expect, it } from "vitest";
import {
  CHECK_INTERVAL_MS,
  IDLE,
  canInstall,
  checkAndDownload,
  shouldCheck,
  statusText,
  type UpdaterStatus,
} from "./updater";

const ready = { kind: "ready", version: "0.3.1" } as const;

describe("install gating", () => {
  it("defers an install while a batch is running", () => {
    expect(canInstall(ready, true)).toBe(false);
  });

  it("allows the deferred install once the batch is finished", () => {
    // The same pending update, after the only thing that changed is the batch
    // ending. This is the transition the whole feature turns on.
    expect(canInstall(ready, true)).toBe(false);
    expect(canInstall(ready, false)).toBe(true);
  });

  it("never installs before the download has finished", () => {
    expect(canInstall(IDLE, false)).toBe(false);
    expect(canInstall({ kind: "checking" }, false)).toBe(false);
    expect(canInstall({ kind: "downloading", version: "0.3.1" }, false)).toBe(false);
  });

  it("does not start a second install while one is under way", () => {
    expect(canInstall({ kind: "installing", version: "0.3.1" }, false)).toBe(false);
  });
});

describe("checking", () => {
  it("only starts a check when nothing else is in flight", () => {
    expect(shouldCheck(IDLE)).toBe(true);
    expect(shouldCheck({ kind: "checking" })).toBe(false);
    expect(shouldCheck({ kind: "downloading", version: "0.3.1" })).toBe(false);
    expect(shouldCheck(ready)).toBe(false);
    expect(shouldCheck({ kind: "installing", version: "0.3.1" })).toBe(false);
  });

  it("returns to a checkable state after a failure, so the next hour retries", () => {
    // A failed check leaves the app on IDLE, which is checkable again.
    expect(shouldCheck(IDLE)).toBe(true);
  });

  it("polls hourly, not aggressively", () => {
    expect(CHECK_INTERVAL_MS).toBe(3_600_000);
  });
});

describe("status line", () => {
  it("shows nothing when there is no update", () => {
    // Includes the case where the check failed because GitHub was unreachable:
    // the hook drops back to idle, and idle is silent.
    expect(statusText(IDLE, false)).toBeNull();
    expect(statusText(IDLE, true)).toBeNull();
  });

  it("says an update is waiting for the batch, rather than interrupting it", () => {
    expect(statusText(ready, true)).toBe(
      "Update v0.3.1 ready — it will install after the current batch.",
    );
  });

  it("names the version at each step", () => {
    expect(statusText({ kind: "checking" }, false)).toBe("Checking for updates…");
    expect(statusText({ kind: "downloading", version: "0.3.1" }, false)).toBe(
      "Downloading MetaStrip Video v0.3.1…",
    );
    expect(statusText(ready, false)).toBe("Update v0.3.1 ready — installing…");
    expect(statusText({ kind: "installing", version: "0.3.1" }, false)).toBe(
      "Installing MetaStrip Video v0.3.1…",
    );
  });
});

describe("check and download", () => {
  /** Collects everything the UI would have been told, in order. */
  function recorder() {
    const seen: UpdaterStatus[] = [];
    return { seen, onStatus: (s: UpdaterStatus) => seen.push(s) };
  }

  it("downloads an update it finds and hands it back for later", async () => {
    const { seen, onStatus } = recorder();
    let downloaded = false;
    const update = {
      version: "0.3.1",
      download: async () => {
        downloaded = true;
      },
    };

    const result = await checkAndDownload(async () => update, onStatus);

    expect(downloaded).toBe(true);
    expect(result).toBe(update);
    expect(seen.map((s) => s.kind)).toEqual(["checking", "downloading", "ready"]);
  });

  it("goes quiet when there is nothing newer", async () => {
    const { seen, onStatus } = recorder();
    const result = await checkAndDownload(async () => null, onStatus);

    expect(result).toBeNull();
    expect(seen.map((s) => s.kind)).toEqual(["checking", "idle"]);
    expect(statusText(seen[seen.length - 1], false)).toBeNull();
  });

  it("survives an unreachable GitHub without throwing", async () => {
    const { seen, onStatus } = recorder();
    const errors: unknown[] = [];

    const result = await checkAndDownload(
      async () => {
        throw new Error("network error: getaddrinfo ENOTFOUND github.com");
      },
      onStatus,
      (e) => errors.push(e),
    );

    // The app is left exactly as it was, and says nothing.
    expect(result).toBeNull();
    expect(seen[seen.length - 1]).toEqual(IDLE);
    expect(statusText(seen[seen.length - 1], false)).toBeNull();
    expect(errors).toHaveLength(1);
  });

  it("installs nothing when the signature does not verify", async () => {
    const { seen, onStatus } = recorder();
    const errors: unknown[] = [];

    const result = await checkAndDownload(
      async () => ({
        version: "6.6.6",
        download: async () => {
          throw new Error("signature verification failed");
        },
      }),
      onStatus,
      (e) => errors.push(e),
    );

    // Nothing is handed back, so there is nothing the app could install.
    expect(result).toBeNull();
    expect(canInstall(seen[seen.length - 1], false)).toBe(false);
    expect(errors).toHaveLength(1);
  });

  it("leaves the state checkable again after a failure", async () => {
    const { seen, onStatus } = recorder();
    await checkAndDownload(
      async () => {
        throw new Error("invalid latest.json");
      },
      onStatus,
      () => {},
    );
    // So the next hourly check is not blocked by the failed one.
    expect(shouldCheck(seen[seen.length - 1])).toBe(true);
  });
});
