export type ToolStatus = {
  ffmpeg: boolean;
  ffprobe: boolean;
  ffmpegVersion?: string;
};

export type BatchConfig = {
  sourceDir: string;
  outputDir: string;
  filenameSuffix: string;
  codec: "hevc-main10" | "hevc" | "h264" | "prores-proxy";
  proresProfile: "proxy" | "lt" | "standard" | "hq";
  squeeze: number;
  outputHeight: number;
  bitrateMbps: number;
  parallelJobs: number;
  preserveTimecode: boolean;
  includeAudio: boolean;
  skipExisting: boolean;
};

export type BatchProgress = {
  batchId: string;
  file: string;
  fileIndex: number;
  totalFiles: number;
  fileProgress: number;
  completedFiles: number;
  failedFiles: number;
  skippedFiles: number;
  sourceBytes: number;
  outputBytes: number;
  message?: string;
  kind: "started" | "progress" | "completed" | "failed" | "skipped" | "cancelled" | "log" | "batch-complete";
};
