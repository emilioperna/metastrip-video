import { describe, expect, it } from "vitest";
import {
  ALWAYS_REMOVED_NOTE,
  DEFAULT_CLEANING_OPTIONS,
  REMOVE_SUBTITLES_HELP,
  REMOVE_SUBTITLES_LABEL,
  cleanRequest,
  cleaningControlLocked,
  completionStats,
  preCleanWarnings,
  readCleaningOptions,
  saveOptionsRequest,
  snapshotOptions,
  verifiedCleaningDetail,
  withRemoveSubtitles,
  type CleaningOptions,
} from "./cleaning";
import { EMPTY_SUMMARY, type ScanState, type ScanView } from "./privacy";

function scan(overrides: Partial<ScanView> = {}): ScanView {
  return {
    path: "C:/v.mp4",
    name: "v.mp4",
    ok: true,
    error: null,
    summary: { ...EMPTY_SUMMARY },
    findings: [],
    container: "mov,mp4",
    durationSeconds: 2,
    videoStreams: 1,
    audioStreams: 1,
    subtitleStreams: 0,
    coverImages: 0,
    otherStreams: 0,
    chapterCount: 0,
    fieldCount: 0,
    ...overrides,
  };
}

function file(scanState: ScanState, overrides: Partial<ScanView> = {}) {
  return scanState === "done" ? { scanState, scan: scan(overrides) } : { scanState };
}

const KEEP: CleaningOptions = { removeSubtitles: false };
const REMOVE: CleaningOptions = { removeSubtitles: true };

describe("cleaning options", () => {
  it("keeps subtitles by default", () => {
    expect(DEFAULT_CLEANING_OPTIONS.removeSubtitles).toBe(false);
    expect(Object.isFrozen(DEFAULT_CLEANING_OPTIONS)).toBe(true);
  });

  it("loads a stored choice, and anything unexpected as the default", () => {
    expect(readCleaningOptions({ removeSubtitles: true })).toEqual(REMOVE);
    expect(readCleaningOptions({ removeSubtitles: false })).toEqual(KEEP);
    for (const odd of [
      undefined,
      null,
      7,
      "x",
      [],
      {},
      { removeSubtitles: "yes" },
      { removeSubtitles: 1 },
    ]) {
      expect(readCleaningOptions(odd)).toEqual(KEEP);
    }
  });

  it("saves exactly the value the checkbox now shows", () => {
    const checked = withRemoveSubtitles(KEEP, true);
    expect(checked).toEqual(REMOVE);
    expect(saveOptionsRequest(checked)).toEqual({ options: { removeSubtitles: true } });
    const unchecked = withRemoveSubtitles(checked, false);
    expect(saveOptionsRequest(unchecked)).toEqual({ options: { removeSubtitles: false } });
    // Nothing else rides along with a cleaning save.
    expect(Object.keys(saveOptionsRequest(unchecked))).toEqual(["options"]);
  });
});

describe("the Clean request", () => {
  it("carries a copy of the options taken at the click", () => {
    const live: CleaningOptions = { removeSubtitles: true };
    const paths = ["C:/a.mp4", "C:/b.mkv"];
    const request = cleanRequest(paths, live);

    // The page changing its own state afterwards cannot reach what was sent.
    live.removeSubtitles = false;
    paths.push("C:/late.mov");

    expect(request).toEqual({
      paths: ["C:/a.mp4", "C:/b.mkv"],
      options: { removeSubtitles: true },
    });
    expect(Object.isFrozen(request.options)).toBe(true);
    expect(request.options).not.toBe(live);
  });

  it("snapshots only an explicit true as removal", () => {
    expect(snapshotOptions(REMOVE)).toEqual(REMOVE);
    expect(snapshotOptions({ removeSubtitles: "true" } as unknown as CleaningOptions)).toEqual(
      KEEP,
    );
  });
});

describe("cleaning control lock", () => {
  it("locks while cleaning or installing an update, and until settings load", () => {
    expect(cleaningControlLocked(true, true)).toBe(true);
    expect(cleaningControlLocked(false, false)).toBe(true);
    expect(cleaningControlLocked(false, true)).toBe(false);
  });
});

describe("preCleanWarnings", () => {
  it("names the videos whose detected cover art will go", () => {
    expect(
      preCleanWarnings(
        [file("done", { coverImages: 1 }), file("done", { coverImages: 1 }), file("done")],
        KEEP,
      ),
    ).toEqual(["Detected cover art will be removed from 2 videos."]);
    expect(preCleanWarnings([file("done", { coverImages: 1 })], KEEP)).toEqual([
      "Detected cover art will be removed from 1 video.",
    ]);
  });

  it("warns about subtitles only when their removal is on", () => {
    const files = [
      file("done", { subtitleStreams: 2 }),
      file("done", { subtitleStreams: 1 }),
      file("done"),
    ];
    expect(preCleanWarnings(files, KEEP)).toEqual([]);
    expect(preCleanWarnings(files, REMOVE)).toEqual([
      "3 subtitle tracks across 2 videos will be removed.",
    ]);
    expect(preCleanWarnings([file("done", { subtitleStreams: 1 })], REMOVE)).toEqual([
      "1 subtitle track will be removed from 1 video.",
    ]);
  });

  it("says its counts are partial while scans are pending or failed", () => {
    const failed = { scanState: "failed" as const, scan: scan({ ok: false, coverImages: 0 }) };
    expect(
      preCleanWarnings([file("done", { coverImages: 1 }), file("scanning"), failed], KEEP),
    ).toEqual([
      "Detected cover art will be removed from 1 video.",
      "Based on 1 of 3 videos scanned so far.",
    ]);
  });

  it("never claims a video has no cover art or no subtitles", () => {
    // Nothing found yet, some files unscanned: silence, not reassurance.
    const warnings = preCleanWarnings([file("done"), file("pending"), file("scanning")], REMOVE);
    expect(warnings).toEqual([]);
  });
});

describe("what the page says about cleaning", () => {
  const all = [
    ALWAYS_REMOVED_NOTE,
    REMOVE_SUBTITLES_LABEL,
    REMOVE_SUBTITLES_HELP,
    verifiedCleaningDetail(KEEP),
    verifiedCleaningDetail(REMOVE),
  ];

  it("does not overclaim", () => {
    for (const text of all) {
      expect(text).not.toMatch(/anonym/i);
      expect(text).not.toMatch(/all (sensitive|private|personal)/i);
      expect(text).not.toMatch(/all cover/i);
      expect(text).not.toMatch(/every (cover|image|picture)/i);
      expect(text).not.toMatch(/bit[- ]for[- ]bit|byte[- ]identical|identical|lossless/i);
      expect(text).not.toMatch(/-dn|-sn|-map|stream mapping|attached_pic/);
    }
  });

  it("calls cover art detected, and says kept contents are not inspected", () => {
    expect(ALWAYS_REMOVED_NOTE).toMatch(/detected cover art/);
    for (const options of [KEEP, REMOVE]) {
      const detail = verifiedCleaningDetail(options);
      expect(detail).toMatch(/detected cover art/);
      expect(detail).toMatch(/contents are not inspected/);
      expect(detail).toMatch(/stream copy with matching codec parameters/);
    }
  });

  it("follows the subtitle choice", () => {
    expect(verifiedCleaningDetail(KEEP)).toMatch(/Kept video, audio and subtitle streams/);
    expect(verifiedCleaningDetail(KEEP)).not.toMatch(/subtitle tracks were removed/);
    expect(verifiedCleaningDetail(REMOVE)).toMatch(/and subtitle tracks were removed/);
    expect(verifiedCleaningDetail(REMOVE)).toMatch(/Kept video and audio streams/);
  });

  it("reports removed cover art and subtitles by what was measured", () => {
    const stats = completionStats({
      privacyFieldsRemoved: 4,
      technicalFieldsRemoved: 2,
      chaptersRemoved: 1,
      dataStreamsRemoved: 0,
      coverArtStreamsRemoved: 2,
      subtitleStreamsRemoved: 1,
    });
    expect(stats).toEqual([
      "4 privacy fields removed",
      "2 detected cover art streams removed",
      "1 subtitle track removed",
      "1 chapter marker removed",
      "2 technical fields removed",
    ]);
    expect(
      completionStats({
        privacyFieldsRemoved: 0,
        technicalFieldsRemoved: 0,
        chaptersRemoved: 0,
        dataStreamsRemoved: 0,
        coverArtStreamsRemoved: 0,
        subtitleStreamsRemoved: 0,
      }),
    ).toEqual([]);
  });
});

describe("no stale cleaning plan", () => {
  it("the scan type carries no execution plan", () => {
    // Fails to build if `plan` comes back on ScanView.
    type HasPlan = "plan" extends keyof ScanView ? true : false;
    const hasPlan: HasPlan = false;
    expect(hasPlan).toBe(false);
  });

  it("no frontend source declares a CleaningPlan type", () => {
    const sources = import.meta.glob(["./**/*.ts", "./**/*.tsx", "!./**/*.test.ts"], {
      query: "?raw",
      import: "default",
      eager: true,
    }) as Record<string, string>;
    expect(Object.keys(sources).length).toBeGreaterThan(5);
    for (const [path, source] of Object.entries(sources)) {
      expect(source, path).not.toMatch(/\bCleaningPlan\b/);
    }
  });
});

describe("the Clean click uses the checkbox, not the stored preference", () => {
  // App.tsx has no component-test harness, so this pins the source contract the
  // way the CleaningPlan check above does. The invariant: the options a batch
  // runs with are the ones the checkbox shows when Clean is pressed -- never
  // rebuilt from the persisted `settings.cleaning`, which can lag behind a save
  // still in flight or a save that failed.
  const sources = import.meta.glob(["./App.tsx", "./components/Workflow.tsx"], {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>;
  const app = sources["./App.tsx"];
  const workflow = sources["./components/Workflow.tsx"];

  /** The source of one top-level function in App, braces matched. */
  function functionBody(source: string, name: string): string {
    const start = source.indexOf(`function ${name}(`);
    expect(start, `${name} not found`).toBeGreaterThanOrEqual(0);
    const open = source.indexOf("{", source.indexOf(")", start));
    let depth = 0;
    for (let i = open; i < source.length; i++) {
      if (source[i] === "{") depth++;
      if (source[i] === "}" && --depth === 0) return source.slice(open, i + 1);
    }
    throw new Error(`${name} has no closing brace`);
  }

  it("builds the request from the checkbox state at the click", () => {
    const click = functionBody(app, "cleanVideos");
    expect(click).toMatch(
      /const request = cleanRequest\(\s*files\.map\(\(file\) => file\.path\),\s*snapshotOptions\(cleaning\),?\s*\)/,
    );
    expect(click).not.toMatch(/settings/);
  });

  it("sends exactly that request, and nothing rebuilds it from settings", () => {
    const run = functionBody(app, "runBatch");
    expect(run).toMatch(/invoke<Summary>\("clean_videos", request\)/);
    expect(run).not.toMatch(/settings\??\.cleaning/);
    expect(app.match(/"clean_videos"/g)).toHaveLength(1);
    expect(app).not.toMatch(/snapshotOptions\([^)]*settings/);
    expect(app).not.toMatch(/cleanRequest\([^;]*settings/);
  });

  it("the checkbox renders the same state the click snapshots", () => {
    expect(app).toMatch(/const \[cleaning, setCleaning\] = useState<CleaningOptions>/);
    expect(app).toMatch(/cleaning=\{cleaning\}/);
    expect(workflow).toMatch(/checked=\{cleaning\.removeSubtitles\}/);
  });
});
