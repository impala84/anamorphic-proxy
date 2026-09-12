export function derivedWidth(sourceWidth: number, sourceHeight: number, squeeze: number, targetHeight: number) {
  const raw = (sourceWidth / sourceHeight) * squeeze * targetHeight;
  return Math.round(raw / 2) * 2;
}

export function proxyPath(sourceDir: string, folderName = "Proxy") {
  const separator = sourceDir.includes("\\") ? "\\" : "/";
  return `${sourceDir.replace(/[\\/]$/, "")}${separator}${folderName || "Proxy"}`;
}

export function formatElapsed(seconds: number) {
  const total = Math.max(0, Math.round(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const remainder = total % 60;
  if (hours) return `${hours}h ${minutes}m ${remainder}s`;
  if (minutes) return `${minutes}m ${remainder}s`;
  return `${remainder}s`;
}

export function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / 1024 ** index;
  return `${value >= 10 || index === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[index]}`;
}

export function reductionPercent(sourceBytes: number, outputBytes: number) {
  if (sourceBytes <= 0 || outputBytes >= sourceBytes) return 0;
  return Math.max(0, Math.min(100, (1 - outputBytes / sourceBytes) * 100));
}

export function recommendedBitrate(height: number, codec: "hevc-main10" | "hevc" | "h264" | "prores-proxy") {
  const baseline = codec === "h264" ? 8 : 6;
  return Math.max(1, Math.round(baseline * (height / 1080) ** 2));
}

export function latestFileEvents(events: BatchProgress[]) {
  const byFile = new Map<string, BatchProgress>();
  const terminalKinds = ["completed", "failed", "skipped", "cancelled"];
  events.filter((event) => event.file && !["batch-complete", "log"].includes(event.kind)).forEach((event) => {
    const previous = byFile.get(event.file);
    const terminal = previous && terminalKinds.includes(previous.kind);
    if (!terminal || terminalKinds.includes(event.kind)) byFile.set(event.file, event);
  });
  return [...byFile.values()];
}

export function overallProgress(latest: BatchProgress | undefined, files: BatchProgress[]) {
  if (!latest?.totalFiles) return 0;
  const finished = latest.kind === "batch-complete" && latest.completedFiles + latest.skippedFiles + latest.failedFiles === latest.totalFiles;
  if (finished) return 100;
  return Math.round(files.reduce((sum, event) => ["completed", "failed", "skipped"].includes(event.kind) ? sum + 100 : sum + event.fileProgress, 0) / latest.totalFiles);
}

export function basename(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}
import type { BatchProgress } from "./types";
