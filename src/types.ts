export type ToolStatus = {
  ffmpeg: boolean;
  ffprobe: boolean;
  ffmpegVersion?: string;
};

export type BatchConfig = {
  sourceDir: string;
  outputDir: string;
  filenameSuffix: string;
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
  message?: string;
  kind: "started" | "progress" | "completed" | "failed" | "skipped" | "cancelled" | "log" | "batch-complete";
};

