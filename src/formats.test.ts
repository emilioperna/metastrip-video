import { describe, expect, it } from "vitest";
import {
  fileExtension,
  isSupported,
  partitionSupported,
  supportedFormatLabels,
  type SupportedFormat,
} from "./formats";

const formats: SupportedFormat[] = [
  { extension: "mp4", label: "MP4" },
  { extension: "mov", label: "MOV" },
  { extension: "m4v", label: "M4V" },
  { extension: "mkv", label: "MKV" },
  { extension: "webm", label: "WebM" },
  { extension: "avi", label: "AVI" },
];

describe("runtime format support", () => {
  it("matches extensions case-insensitively without suffix false positives", () => {
    expect(isSupported("C:\\clips\\REEL.MKV", formats)).toBe(true);
    expect(isSupported("/clips/reel.webm", formats)).toBe(true);
    expect(isSupported("/clips/reel.mp4.exe", formats)).toBe(false);
    expect(isSupported("/clips/mp4", formats)).toBe(false);
  });

  it("accepts supported files and reports every skipped path in a mixed drop", () => {
    const paths = [
      "one.mp4",
      "two.mov",
      "three.m4v",
      "four.mkv",
      "five.webm",
      "six.avi",
      "seven.MP4",
      "eight.WEBM",
      "notes.txt",
      "archive.mp4.zip",
    ];

    const result = partitionSupported(paths, formats);

    expect(result.accepted).toHaveLength(8);
    expect(result.rejected).toEqual(["notes.txt", "archive.mp4.zip"]);
  });

  it("uses backend-provided labels for user-facing copy", () => {
    expect(supportedFormatLabels(formats)).toBe("MP4, MOV, M4V, MKV, WebM, AVI");
  });

  it("preserves the selected file's extension spelling for previews", () => {
    expect(fileExtension("C:\\clips\\reel.WebM")).toBe("WebM");
    expect(fileExtension("no-extension")).toBeNull();
  });
});
