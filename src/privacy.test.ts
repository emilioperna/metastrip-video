import { describe, expect, it } from "vitest";
import {
  allVerified,
  completionTitle,
  detailFacts,
  STREAM_COPY_NOTE,
  findingsLabel,
  formatDuration,
  groupFindings,
  isTechnical,
  partitionFindings,
  summarise,
  technicalLabel,
  topSeverity,
  type PrivacyFinding,
  type ScanView,
  type Severity,
} from "./privacy";
import { verifiedCleaningDetail } from "./cleaning";

function finding(
  categoryLabel: string,
  severity: Severity,
  detail = "x",
  sourceKey = detail,
): PrivacyFinding {
  return {
    category: categoryLabel.toLowerCase(),
    categoryLabel,
    severity,
    severityLabel: severity.toUpperCase(),
    detail,
    explanation: `About ${categoryLabel}.`,
    scopeLabel: "Container",
    streamIndex: null,
    sourceKey,
  };
}

/** The summary the backend sends: `PrivacySummary::of` in `privacy.rs`. */
function backendSummary(findings: PrivacyFinding[]) {
  const privacy = findings.filter((f) => f.category !== "structural");
  return {
    total: privacy.length,
    high: privacy.filter((f) => f.severity === "high").length,
    medium: privacy.filter((f) => f.severity === "medium").length,
    low: privacy.filter((f) => f.severity === "low").length,
    technical: findings.length - privacy.length,
  };
}

function scan(overrides: Partial<ScanView> = {}): ScanView {
  const findings = overrides.findings ?? [];
  return {
    path: "C:/v.mp4",
    name: "v.mp4",
    ok: true,
    error: null,
    summary: backendSummary(findings),
    findings,
    container: "mov,mp4",
    durationSeconds: 12,
    videoStreams: 1,
    audioStreams: 1,
    subtitleStreams: 0,
    coverImages: 0,
    otherStreams: 0,
    chapterCount: 0,
    fieldCount: findings.length,
    ...overrides,
  };
}

describe("summarise", () => {
  it("adds up findings across a batch", () => {
    const batch = summarise([
      scan({ findings: [finding("Location", "high"), finding("Device", "medium")] }),
      scan({ findings: [finding("Software", "low")] }),
      scan({ findings: [finding("Location", "high"), finding("Creator", "medium")] }),
    ]);

    expect(batch.scanned).toBe(3);
    expect(batch.failed).toBe(0);
    expect(batch.total).toBe(5);
    expect(batch.high).toBe(2);
    expect(batch.medium).toBe(2);
    expect(batch.low).toBe(1);
    expect(batch.filesWithHigh).toBe(2);
  });

  it("counts an unscannable file apart instead of treating it as clean", () => {
    const batch = summarise([
      scan({ findings: [finding("Location", "high")] }),
      scan({ ok: false, error: "This file could not be inspected", findings: [] }),
    ]);

    expect(batch.scanned).toBe(1);
    expect(batch.failed).toBe(1);
    // The failed file contributes nothing to the totals.
    expect(batch.total).toBe(1);
    expect(batch.high).toBe(1);
  });

  it("handles an empty batch", () => {
    const batch = summarise([]);
    expect(batch).toEqual({
      total: 0,
      high: 0,
      medium: 0,
      low: 0,
      technical: 0,
      scanned: 0,
      failed: 0,
      filesWithHigh: 0,
    });
  });

  it("keeps structural fields out of the batch privacy totals", () => {
    const structural = (key: string) => finding("Structural", "low", key, key);
    const batch = summarise([
      scan({
        findings: [
          finding("Timestamp", "medium", "creation_time"),
          finding("Software", "low", "encoder"),
          structural("major_brand"),
          structural("minor_version"),
          structural("handler_name"),
          structural("language"),
        ],
      }),
      scan({ findings: [structural("major_brand"), structural("handler_name")] }),
    ]);

    expect(batch.total).toBe(2);
    expect(batch.medium).toBe(1);
    expect(batch.low).toBe(1);
    expect(batch.high + batch.medium + batch.low).toBe(batch.total);
    expect(batch.technical).toBe(6);
  });
});

describe("technical metadata", () => {
  const structural = (key: string) => finding("Structural", "low", key, key);

  it("recognises only the structural category as technical", () => {
    expect(isTechnical(structural("major_brand"))).toBe(true);
    for (const label of [
      "Location",
      "Device",
      "Timestamp",
      "Software",
      "CreatorIdentity",
      "Telemetry",
      "Identifier",
      "Copyright",
      "Unknown",
    ]) {
      expect(isTechnical(finding(label, "low"))).toBe(false);
    }
  });

  it("excludes structural fields from the single-file privacy summary", () => {
    const view = scan({
      findings: [
        finding("Timestamp", "medium", "creation_time"),
        finding("Device", "medium", "make"),
        finding("Identifier", "medium", "uuid"),
        finding("Software", "low", "encoder"),
        structural("handler_name"),
        structural("language"),
        structural("major_brand"),
        structural("minor_version"),
        structural("compatible_brands"),
        structural("vendor_id"),
        structural("duration"),
      ],
    });

    expect(view.summary).toEqual({ total: 4, high: 0, medium: 3, low: 1, technical: 7 });
    expect(findingsLabel(view.summary)).toBe("4 privacy findings");
    expect(technicalLabel(view.summary.technical)).toBe("7 technical fields");
    expect(topSeverity(view.summary)).toBe("medium");
  });

  it("shows zero privacy findings for a file with only structural metadata", () => {
    const view = scan({ findings: [structural("major_brand"), structural("handler_name")] });

    expect(view.summary.total).toBe(0);
    expect(topSeverity(view.summary)).toBeNull();
    expect(findingsLabel(view.summary)).toBe("No privacy findings");
    expect(technicalLabel(view.summary.technical)).toBe("2 technical fields");
  });

  it("keeps structural metadata visible in the expanded details", () => {
    const findings = [
      finding("Location", "high", "location"),
      structural("handler_name"),
      finding("Software", "low", "encoder"),
      structural("language"),
    ];
    const { privacy, technical } = partitionFindings(findings);

    expect(privacy.map((f) => f.categoryLabel)).toEqual(["Location", "Software"]);
    expect(technical.map((f) => f.sourceKey)).toEqual(["handler_name", "language"]);
    expect(privacy.length + technical.length).toBe(findings.length);
    const groups = groupFindings(technical);
    expect(groups).toHaveLength(1);
    expect(groups[0].items).toHaveLength(2);
  });

  it("labels technical counts only when there are some", () => {
    expect(technicalLabel(0)).toBeNull();
    expect(technicalLabel(1)).toBe("1 technical field");
  });
});

describe("topSeverity", () => {
  it("reports the worst severity present", () => {
    expect(topSeverity({ total: 3, high: 1, medium: 1, low: 1, technical: 0 })).toBe("high");
    expect(topSeverity({ total: 2, high: 0, medium: 1, low: 1, technical: 0 })).toBe("medium");
    expect(topSeverity({ total: 1, high: 0, medium: 0, low: 1, technical: 0 })).toBe("low");
    expect(topSeverity({ total: 0, high: 0, medium: 0, low: 0, technical: 0 })).toBeNull();
    // Technical fields never raise the severity.
    expect(topSeverity({ total: 0, high: 0, medium: 0, low: 0, technical: 9 })).toBeNull();
  });
});

describe("findingsLabel", () => {
  it("is singular, plural or explicit about finding nothing", () => {
    const empty = { total: 0, high: 0, medium: 0, low: 0, technical: 0 };
    expect(findingsLabel(empty)).toBe("No metadata found");
    expect(findingsLabel({ ...empty, technical: 3 })).toBe("No privacy findings");
    expect(findingsLabel({ ...empty, total: 1, high: 1 })).toBe("1 privacy finding");
    expect(findingsLabel({ ...empty, total: 6, high: 1, medium: 2, low: 3 })).toBe(
      "6 privacy findings",
    );
  });
});

describe("groupFindings", () => {
  it("groups by category, worst first, and keeps every item", () => {
    const groups = groupFindings([
      finding("Software", "low", "encoder", "encoder"),
      finding("Location", "high", "location", "location"),
      finding("Device", "medium", "make", "make"),
      finding("Device", "medium", "model", "model"),
    ]);

    expect(groups.map((g) => g.category)).toEqual(["Location", "Device", "Software"]);
    expect(groups[1].items).toHaveLength(2);
    expect(groups.flatMap((g) => g.items)).toHaveLength(4);
  });

  it("gives a group the severity of its most severe member", () => {
    // A camera can put a low-risk and a high-risk key in the same category.
    const groups = groupFindings([
      finding("Location", "low", "location_name", "location_name"),
      finding("Location", "high", "gps", "gps"),
    ]);

    expect(groups).toHaveLength(1);
    expect(groups[0].severity).toBe("high");
    expect(groups[0].severityLabel).toBe("HIGH");
    expect(groups[0].items).toHaveLength(2);
  });

  it("returns nothing for no findings", () => {
    expect(groupFindings([])).toEqual([]);
  });
});

describe("completionTitle and allVerified", () => {
  it("says cleaned when everything verified", () => {
    const summary = { completed: 6, errors: 0, verified: 6, verificationFailures: 0 };
    expect(completionTitle(summary)).toBe("6 videos cleaned");
    expect(allVerified(summary)).toBe(true);
  });

  it("is singular for one file", () => {
    expect(
      completionTitle({ completed: 1, errors: 0, verified: 1, verificationFailures: 0 }),
    ).toBe("1 video cleaned");
  });

  it("never claims verified when a check failed", () => {
    const summary = { completed: 6, errors: 0, verified: 5, verificationFailures: 1 };
    expect(completionTitle(summary)).toBe("6 videos cleaned · 1 not verified");
    expect(allVerified(summary)).toBe(false);
  });

  it("reports outright failures ahead of verification", () => {
    const summary = { completed: 4, errors: 2, verified: 4, verificationFailures: 0 };
    expect(completionTitle(summary)).toBe("4 cleaned · 2 failed");
    // The two failures were never cleaned, so the four that were are still
    // legitimately verified. The headline is what reports the failures.
    expect(allVerified(summary)).toBe(true);
  });

  it("does not claim verified when a cleaned file has no report at all", () => {
    // Cleaned but unverifiable: verified < completed with no explicit failure.
    const summary = { completed: 3, errors: 0, verified: 2, verificationFailures: 0 };
    expect(allVerified(summary)).toBe(false);
  });

  it("handles a batch where nothing was cleaned", () => {
    const summary = { completed: 0, errors: 3, verified: 0, verificationFailures: 0 };
    expect(completionTitle(summary)).toBe("No videos were cleaned");
    expect(allVerified(summary)).toBe(false);
  });
});

describe("formatDuration", () => {
  it("formats seconds and minutes readably", () => {
    expect(formatDuration(1.24)).toBe("1.2 s");
    expect(formatDuration(48)).toBe("48 s");
    expect(formatDuration(200)).toBe("3:20");
    expect(formatDuration(60)).toBe("1:00");
  });

  it("does not round into an impossible clock value", () => {
    // 59.6 s would render as 3:60 with naive rounding.
    expect(formatDuration(239.6)).toBe("4:00");
  });

  it("returns null when there is no usable duration", () => {
    expect(formatDuration(null)).toBeNull();
    expect(formatDuration(Number.NaN)).toBeNull();
    expect(formatDuration(Number.POSITIVE_INFINITY)).toBeNull();
    expect(formatDuration(-1)).toBeNull();
  });
});

describe("stream claims", () => {
  // Runtime verification compares codec parameters, not packets. The copy must
  // not promise a bit-for-bit result that only the regression tests establish.
  const overclaims = [/bit[- ]for[- ]bit/i, /identical/i, /unchanged/i, /lossless/i, /proven/i];

  it("does not overclaim what runtime verification checks", () => {
    const verified = [
      verifiedCleaningDetail({ removeSubtitles: false }),
      verifiedCleaningDetail({ removeSubtitles: true }),
    ];
    for (const text of [STREAM_COPY_NOTE, ...verified]) {
      for (const pattern of overclaims) {
        expect(text).not.toMatch(pattern);
      }
    }
  });

  it("states what is actually true", () => {
    expect(STREAM_COPY_NOTE).toMatch(/stream copy/);
    expect(verifiedCleaningDetail({ removeSubtitles: false })).toMatch(
      /stream copy with matching codec parameters/,
    );
  });
});

describe("detailFacts", () => {
  it("shows duration and streams without a generic metadata field total", () => {
    const facts = detailFacts(
      scan({
        durationSeconds: 41,
        fieldCount: 11,
        findings: [finding("Timestamp", "medium"), finding("Structural", "low", "major_brand")],
      }),
    );

    expect(facts).toEqual(["41 s", "1 video · 1 audio"]);
    expect(facts.join(" · ")).not.toMatch(/metadata|field/i);
  });

  it("lists subtitles and detected cover art apart from footage", () => {
    expect(
      detailFacts(scan({ durationSeconds: null, subtitleStreams: 2, coverImages: 1 })),
    ).toEqual(["1 video · 1 audio · 2 subtitle", "1 detected cover art"]);
  });

  it("still lists data tracks and chapters when present", () => {
    expect(detailFacts(scan({ durationSeconds: null, otherStreams: 1, chapterCount: 3 }))).toEqual([
      "1 video · 1 audio · 1 other",
      "3 chapters",
    ]);
  });
});
