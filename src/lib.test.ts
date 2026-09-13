import { describe, expect, it } from "vitest";
import { derivedWidth, formatBytes, formatElapsed, latestFileEvents, overallProgress, proxyPath, recommendedBitrate, reductionPercent } from "./lib";
import type { BatchProgress } from "./types";

describe("proxy dimensions", () => {
  it("produces the established Canon C50 1.5x raster", () => {
    expect(derivedWidth(6960, 4640, 1.5, 1080)).toBe(2430);
  });

  it("always returns an encoder-safe even width", () => {
    expect(derivedWidth(4096, 2160, 1.33, 720) % 2).toBe(0);
  });

  it("produces the C50 1.5x 4K-height raster", () => {
    expect(derivedWidth(6960, 4640, 1.5, 2160)).toBe(4860);
  });

  it("keeps ordinary footage at its normal aspect ratio with no squeeze", () => {
    expect(derivedWidth(1920, 1080, 1, 1080)).toBe(1920);
  });
});

describe("default output folder", () => {
  it("uses a Proxy directory under the source", () => {
    expect(proxyPath("/Volumes/Footage/Day 01")).toBe("/Volumes/Footage/Day 01/Proxy");
  });

  it("uses a custom proxy folder name", () => {
    expect(proxyPath("/Volumes/Footage/Day 01", "Editorial")).toBe("/Volumes/Footage/Day 01/Editorial");
  });
});

describe("elapsed time", () => {
  it("formats a compact batch duration", () => expect(formatElapsed(125)).toBe("2m 5s"));
});

describe("batch sizes", () => {
  it("formats file sizes cleanly", () => expect(formatBytes(22 * 1024 ** 3)).toBe("22 GB"));
  it("calculates the reduction", () => expect(reductionPercent(1000, 250)).toBe(75));
});

describe("recommended bitrate", () => {
  it("scales HEVC with pixel count", () => {
    expect(recommendedBitrate(720, "hevc-main10")).toBe(3);
    expect(recommendedBitrate(1080, "hevc-main10")).toBe(6);
    expect(recommendedBitrate(2160, "hevc-main10")).toBe(24);
  });

  it("gives H.264 additional headroom", () => {
    expect(recommendedBitrate(1080, "h264")).toBe(8);
    expect(recommendedBitrate(2160, "h264")).toBe(32);
  });
});

describe("batch progress", () => {
  const event = (kind: BatchProgress["kind"], progress: number): BatchProgress => ({ batchId: "1", file: "/clip.mp4", fileIndex: 1, totalFiles: 1, fileProgress: progress, completedFiles: kind === "completed" || kind === "batch-complete" ? 1 : 0, failedFiles: 0, skippedFiles: 0, sourceBytes: 1000, outputBytes: 250, kind });

  it("does not let a late progress event replace a completed file", () => {
    const files = latestFileEvents([event("completed", 100), event("progress", 73)]);
    expect(files[0].kind).toBe("completed");
  });

  it("pins a completed batch at 100 percent", () => {
    expect(overallProgress(event("batch-complete", 100), [event("progress", 73)])).toBe(100);
  });

  it("uses cumulative counts when old completion events have been discarded", () => {
    const latest: BatchProgress = {
      ...event("progress", 56),
      file: "/clip-68.mp4",
      fileIndex: 68,
      totalFiles: 100,
      completedFiles: 67,
    };
    const active = [
      { ...latest, file: "/clip-68.mp4", fileProgress: 97 },
      { ...latest, file: "/clip-69.mp4", fileProgress: 56 },
    ];

    expect(overallProgress(latest, active)).toBe(69);
  });

  it("does not double-count retained terminal events", () => {
    const latest: BatchProgress = {
      ...event("progress", 50),
      file: "/clip-3.mp4",
      fileIndex: 3,
      totalFiles: 4,
      completedFiles: 2,
    };
    const files = [
      { ...latest, file: "/clip-1.mp4", kind: "completed" as const, fileProgress: 100 },
      { ...latest, file: "/clip-2.mp4", kind: "completed" as const, fileProgress: 100 },
      { ...latest, file: "/clip-3.mp4", fileProgress: 50 },
    ];

    expect(overallProgress(latest, files)).toBe(63);
  });
});
