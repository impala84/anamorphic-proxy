use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

pub fn tool_path(name: &str) -> Option<PathBuf> {
    let mut candidates = std::env::var_os("PATH")
        .map(|value| {
            std::env::split_paths(&value)
                .map(|dir| dir.join(name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin").join(name),
        PathBuf::from("/usr/local/bin").join(name),
        PathBuf::from("/usr/bin").join(name),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchConfig {
    pub source_dir: String,
    pub output_dir: String,
    pub filename_suffix: String,
    pub codec: String,
    pub prores_profile: String,
    pub squeeze: f64,
    pub output_height: u32,
    pub bitrate_mbps: f64,
    pub parallel_jobs: usize,
    pub preserve_timecode: bool,
    pub audio_mode: String,
    pub skip_existing: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub width: u32,
    pub height: u32,
    pub duration_seconds: f64,
    pub timecode: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProgress {
    batch_id: String,
    file: String,
    file_index: usize,
    total_files: usize,
    file_progress: f64,
    completed_files: usize,
    failed_files: usize,
    skipped_files: usize,
    kind: String,
    message: Option<String>,
    source_bytes: u64,
    output_bytes: u64,
}

#[derive(Default)]
pub struct BatchManager {
    batches: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

#[derive(Default)]
struct Counts {
    complete: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
    source_bytes: AtomicU64,
    output_bytes: AtomicU64,
}

impl BatchManager {
    pub fn insert(&self, id: String, flag: Arc<AtomicBool>) {
        self.batches
            .lock()
            .expect("batch state poisoned")
            .insert(id, flag);
    }

    pub fn cancel(&self, id: &str) -> bool {
        if let Some(flag) = self.batches.lock().expect("batch state poisoned").get(id) {
            flag.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    pub fn remove(&self, id: &str) {
        self.batches
            .lock()
            .expect("batch state poisoned")
            .remove(id);
    }
}

pub fn discover_videos(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = fs::read_dir(folder)
        .map_err(|e| format!("Cannot read source folder: {e}"))?
        .filter_map(Result::ok)
        .map(|item| item.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "mp4" | "mov" | "mxf" | "m4v"
                    )
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

pub fn derive_width(
    source_width: u32,
    source_height: u32,
    squeeze: f64,
    output_height: u32,
) -> u32 {
    let raw = source_width as f64 / source_height as f64 * squeeze * output_height as f64;
    ((raw / 2.0).round() * 2.0) as u32
}

pub fn effective_parallel_jobs(config: &BatchConfig) -> usize {
    if config.codec == "prores-proxy" {
        config
            .parallel_jobs
            .min(if config.output_height >= 2160 { 1 } else { 2 })
            .max(1)
    } else {
        config.parallel_jobs.clamp(1, 8)
    }
}

pub fn probe_file(path: &Path) -> Result<ProbeInfo, String> {
    let ffprobe = tool_path("ffprobe").ok_or("FFprobe was not found")?;
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-ignore_editlist",
            "1",
            "-select_streams",
            "v:0",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("Could not run ffprobe: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_probe_json(&output.stdout)
}

fn parse_probe_json(bytes: &[u8]) -> Result<ProbeInfo, String> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid ffprobe response: {e}"))?;
    let stream = root["streams"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or("No video stream found")?;
    let width = stream["width"].as_u64().ok_or("Video width unavailable")? as u32;
    let height = stream["height"]
        .as_u64()
        .ok_or("Video height unavailable")? as u32;
    let duration_seconds = stream["duration"]
        .as_str()
        .or_else(|| root["format"]["duration"].as_str())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let timecode = stream["tags"]["timecode"]
        .as_str()
        .or_else(|| root["format"]["tags"]["timecode"].as_str())
        .map(ToOwned::to_owned);
    Ok(ProbeInfo {
        width,
        height,
        duration_seconds,
        timecode,
    })
}

pub fn output_path(source: &Path, output_dir: &Path, suffix: &str) -> Result<PathBuf, String> {
    let stem = source
        .file_stem()
        .and_then(|v| v.to_str())
        .ok_or("Invalid source filename")?;
    Ok(output_dir.join(format!("{stem}{suffix}.mp4")))
}

fn output_path_for_config(source: &Path, config: &BatchConfig) -> Result<PathBuf, String> {
    let path = output_path(
        source,
        Path::new(&config.output_dir),
        &config.filename_suffix,
    )?;
    Ok(if config.codec == "prores-proxy" {
        path.with_extension("mov")
    } else {
        path
    })
}

pub fn ffmpeg_args(
    source: &Path,
    temporary: &Path,
    probe: &ProbeInfo,
    config: &BatchConfig,
) -> Vec<String> {
    let width = derive_width(
        probe.width,
        probe.height,
        config.squeeze,
        config.output_height,
    );
    let bitrate = format!("{}k", (config.bitrate_mbps * 1000.0).round() as u64);
    let buffer = format!("{}k", (config.bitrate_mbps * 2000.0).round() as u64);
    let mut args = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-ignore_editlist".into(),
        "1".into(),
        "-i".into(),
        source.to_string_lossy().into_owned(),
        "-map".into(),
        "0:v:0".into(),
    ];
    if config.audio_mode != "none" {
        args.extend(["-map".into(), "0:a?".into()]);
    }
    args.extend([
        "-map_metadata".into(),
        "0".into(),
        "-vf".into(),
        format!(
            "trim=start=0,scale={width}:{}:flags=lanczos,setsar=1,setpts=PTS-STARTPTS",
            config.output_height
        ),
    ]);
    match config.codec.as_str() {
        "hevc" => args.extend([
            "-c:v".into(),
            "hevc_videotoolbox".into(),
            "-profile:v".into(),
            "main".into(),
            "-pix_fmt".into(),
            "nv12".into(),
            "-tag:v".into(),
            "hvc1".into(),
            "-b:v".into(),
            bitrate.clone(),
            "-maxrate".into(),
            bitrate,
            "-bufsize".into(),
            buffer,
        ]),
        "h264" => args.extend([
            "-c:v".into(),
            "h264_videotoolbox".into(),
            "-profile:v".into(),
            "high".into(),
            "-pix_fmt".into(),
            "nv12".into(),
            "-tag:v".into(),
            "avc1".into(),
            "-b:v".into(),
            bitrate.clone(),
            "-maxrate".into(),
            bitrate,
            "-bufsize".into(),
            buffer,
        ]),
        "prores-proxy" => args.extend([
            "-c:v".into(),
            "prores_ks".into(),
            "-profile:v".into(),
            match config.prores_profile.as_str() {
                "lt" => "1",
                "standard" => "2",
                "hq" => "3",
                _ => "0",
            }
            .into(),
            "-pix_fmt".into(),
            "yuv422p10le".into(),
            "-vendor".into(),
            "apl0".into(),
        ]),
        _ => args.extend([
            "-c:v".into(),
            "hevc_videotoolbox".into(),
            "-profile:v".into(),
            "main10".into(),
            "-pix_fmt".into(),
            "p010le".into(),
            "-tag:v".into(),
            "hvc1".into(),
            "-b:v".into(),
            bitrate.clone(),
            "-maxrate".into(),
            bitrate,
            "-bufsize".into(),
            buffer,
        ]),
    }
    match config.audio_mode.as_str() {
        "preserve" => args.extend(["-c:a".into(), "copy".into()]),
        "aac" => args.extend([
            "-af".into(),
            "atrim=start=0,asetpts=PTS-STARTPTS".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "160k".into(),
        ]),
        _ => args.push("-an".into()),
    }
    if config.preserve_timecode {
        if let Some(timecode) = &probe.timecode {
            args.extend(["-timecode".into(), timecode.clone()]);
        }
    }
    args.extend([
        "-movflags".into(),
        "+faststart".into(),
        "-avoid_negative_ts".into(),
        "disabled".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-y".into(),
        temporary.to_string_lossy().into_owned(),
    ]);
    args
}

#[allow(clippy::too_many_arguments)]
fn emit(
    app: &AppHandle,
    batch_id: &str,
    file: &Path,
    index: usize,
    total: usize,
    progress: f64,
    counts: &Counts,
    kind: &str,
    message: Option<String>,
) {
    let _ = app.emit(
        "batch-progress",
        BatchProgress {
            batch_id: batch_id.into(),
            file: file.to_string_lossy().into_owned(),
            file_index: index,
            total_files: total,
            file_progress: progress,
            completed_files: counts.complete.load(Ordering::SeqCst),
            failed_files: counts.failed.load(Ordering::SeqCst),
            skipped_files: counts.skipped.load(Ordering::SeqCst),
            kind: kind.into(),
            message,
            source_bytes: counts.source_bytes.load(Ordering::SeqCst),
            output_bytes: counts.output_bytes.load(Ordering::SeqCst),
        },
    );
}

fn harmless_audio_warning(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("channel layout")
        && lower.contains("mono")
        && (lower.contains("guessed") || lower.contains("unspecified"))
}

#[allow(clippy::too_many_arguments)]
fn encode_one(
    app: &AppHandle,
    batch_id: &str,
    source: &Path,
    index: usize,
    total: usize,
    config: &BatchConfig,
    cancel: &AtomicBool,
    counts: &Counts,
) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("Cancelled".into());
    }
    let probe = probe_file(source)?;
    let output = output_path_for_config(source, config)?;
    if config.skip_existing && output.exists() {
        counts.skipped.fetch_add(1, Ordering::SeqCst);
        emit(
            app,
            batch_id,
            source,
            index,
            total,
            100.0,
            counts,
            "skipped",
            Some("Output already exists".into()),
        );
        return Ok(());
    }
    let extension = output.extension().and_then(|v| v.to_str()).unwrap_or("mp4");
    let temporary = output.with_file_name(format!(
        ".{}.{}.part.{extension}",
        output
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("proxy"),
        Uuid::new_v4()
    ));
    let args = ffmpeg_args(source, &temporary, &probe, config);
    emit(
        app, batch_id, source, index, total, 0.0, counts, "started", None,
    );
    let ffmpeg = tool_path("ffmpeg").ok_or("FFmpeg was not found")?;
    let mut child = Command::new(ffmpeg)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start ffmpeg: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Could not read FFmpeg progress")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Could not read FFmpeg diagnostics")?;
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let stderr_thread = thread::spawn(move || {
        let mut value = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut value);
        value
    });

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&temporary);
            emit(
                app,
                batch_id,
                source,
                index,
                total,
                0.0,
                counts,
                "cancelled",
                Some("Cancelled; partial output removed".into()),
            );
            return Err("Cancelled".into());
        }
        while let Ok(line) = rx.try_recv() {
            if let Some(raw) = line.strip_prefix("out_time_us=") {
                if let Ok(micros) = raw.parse::<f64>() {
                    let pct = if probe.duration_seconds > 0.0 {
                        (micros / 1_000_000.0 / probe.duration_seconds * 100.0).clamp(0.0, 99.9)
                    } else {
                        0.0
                    };
                    emit(
                        app, batch_id, source, index, total, pct, counts, "progress", None,
                    );
                }
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("Could not monitor ffmpeg: {e}"))?
        {
            let diagnostics = stderr_thread.join().unwrap_or_default();
            let useful = diagnostics
                .lines()
                .filter(|line| !harmless_audio_warning(line))
                .collect::<Vec<_>>()
                .join("\n");
            if status.success() {
                fs::rename(&temporary, &output).map_err(|e| {
                    let _ = fs::remove_file(&temporary);
                    format!("Could not finalize output: {e}")
                })?;
                if let Ok(metadata) = fs::metadata(source) {
                    counts
                        .source_bytes
                        .fetch_add(metadata.len(), Ordering::SeqCst);
                }
                if let Ok(metadata) = fs::metadata(&output) {
                    counts
                        .output_bytes
                        .fetch_add(metadata.len(), Ordering::SeqCst);
                }
                counts.complete.fetch_add(1, Ordering::SeqCst);
                emit(
                    app,
                    batch_id,
                    source,
                    index,
                    total,
                    100.0,
                    counts,
                    "completed",
                    None,
                );
                return Ok(());
            }
            let _ = fs::remove_file(&temporary);
            return Err(if useful.is_empty() {
                format!("FFmpeg exited with {status}")
            } else {
                useful
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub fn run_batch(
    app: AppHandle,
    manager: Arc<BatchManager>,
    id: String,
    config: BatchConfig,
    cancel: Arc<AtomicBool>,
) {
    let source_dir = PathBuf::from(&config.source_dir);
    let output_dir = PathBuf::from(&config.output_dir);
    let files = match discover_videos(&source_dir) {
        Ok(files) => files,
        Err(error) => {
            let counts = Counts::default();
            emit(
                &app,
                &id,
                &source_dir,
                0,
                0,
                0.0,
                &counts,
                "batch-complete",
                Some(error),
            );
            manager.remove(&id);
            return;
        }
    };
    if let Err(error) = fs::create_dir_all(&output_dir) {
        let counts = Counts::default();
        emit(
            &app,
            &id,
            &output_dir,
            0,
            files.len(),
            0.0,
            &counts,
            "batch-complete",
            Some(format!("Cannot create output folder: {error}")),
        );
        manager.remove(&id);
        return;
    }
    let total = files.len();
    let counts = Arc::new(Counts::default());
    let next = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    let mut workers = Vec::new();
    for _ in 0..effective_parallel_jobs(&config) {
        let (app, id, config, cancel, counts, next, files) = (
            app.clone(),
            id.clone(),
            config.clone(),
            cancel.clone(),
            counts.clone(),
            next.clone(),
            files.clone(),
        );
        workers.push(thread::spawn(move || loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let index = next.fetch_add(1, Ordering::SeqCst);
            let Some(source) = files.get(index) else {
                break;
            };
            if let Err(error) = encode_one(
                &app,
                &id,
                source,
                index + 1,
                total,
                &config,
                &cancel,
                &counts,
            ) {
                if error != "Cancelled" {
                    counts.failed.fetch_add(1, Ordering::SeqCst);
                    emit(
                        &app,
                        &id,
                        source,
                        index + 1,
                        total,
                        0.0,
                        &counts,
                        "failed",
                        Some(error),
                    );
                }
            }
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    let message = if total == 0 {
        Some("No supported video files found".into())
    } else if cancel.load(Ordering::SeqCst) {
        Some("Batch cancelled".into())
    } else {
        Some(format!(
            "Batch finished: {} complete, {} skipped, {} failed",
            counts.complete.load(Ordering::SeqCst),
            counts.skipped.load(Ordering::SeqCst),
            counts.failed.load(Ordering::SeqCst)
        ))
    };
    emit(
        &app,
        &id,
        &source_dir,
        total,
        total,
        100.0,
        &counts,
        "batch-complete",
        message,
    );
    manager.remove(&id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BatchConfig {
        BatchConfig {
            source_dir: "/source".into(),
            output_dir: "/output".into(),
            filename_suffix: "_proxy".into(),
            codec: "hevc-main10".into(),
            prores_profile: "proxy".into(),
            squeeze: 1.5,
            output_height: 1080,
            bitrate_mbps: 6.0,
            parallel_jobs: 2,
            preserve_timecode: true,
            audio_mode: "preserve".into(),
            skip_existing: true,
        }
    }

    #[test]
    fn c50_anamorphic_width_is_2430() {
        assert_eq!(derive_width(6960, 4640, 1.5, 1080), 2430);
    }

    #[test]
    fn command_matches_established_proxy_settings() {
        let probe = ProbeInfo {
            width: 6960,
            height: 4640,
            duration_seconds: 10.0,
            timecode: Some("01:02:03:04".into()),
        };
        let args = ffmpeg_args(
            Path::new("clip.mp4"),
            Path::new("clip.part.mp4"),
            &probe,
            &config(),
        );
        let joined = args.join(" ");
        assert!(joined.contains("-ignore_editlist 1"));
        assert!(joined
            .contains("trim=start=0,scale=2430:1080:flags=lanczos,setsar=1,setpts=PTS-STARTPTS"));
        assert!(
            joined.contains("-c:v hevc_videotoolbox -profile:v main10 -pix_fmt p010le -tag:v hvc1")
        );
        assert!(joined.contains("-b:v 6000k"));
        assert!(joined.contains("-timecode 01:02:03:04"));
        assert!(joined.contains("-map 0:a?"));
        assert!(joined.contains("-c:a copy"));
        assert!(!joined.contains("-af"));
        assert!(joined.contains("-avoid_negative_ts disabled"));
        assert!(joined.contains("-progress pipe:1"));
    }

    #[test]
    fn aac_mode_reencodes_each_mapped_audio_track() {
        let mut config = config();
        config.audio_mode = "aac".into();
        let probe = ProbeInfo {
            width: 1920,
            height: 1080,
            duration_seconds: 1.0,
            timecode: None,
        };
        let args = ffmpeg_args(
            Path::new("clip.mp4"),
            Path::new("clip.part.mp4"),
            &probe,
            &config,
        )
        .join(" ");
        assert!(args.contains("-map 0:a?"));
        assert!(args.contains("-c:a aac -b:a 160k"));
        assert!(args.contains("-af atrim=start=0,asetpts=PTS-STARTPTS"));
    }

    #[test]
    fn no_audio_mode_disables_audio() {
        let mut config = config();
        config.audio_mode = "none".into();
        let probe = ProbeInfo {
            width: 1920,
            height: 1080,
            duration_seconds: 1.0,
            timecode: None,
        };
        let args = ffmpeg_args(
            Path::new("clip.mp4"),
            Path::new("clip.part.mp4"),
            &probe,
            &config,
        )
        .join(" ");
        assert!(!args.contains("-map 0:a?"));
        assert!(args.contains("-an"));
    }

    #[test]
    fn only_known_mono_layout_noise_is_suppressed() {
        assert!(harmless_audio_warning(
            "Guessed Channel Layout for Input Stream #0.1 : mono"
        ));
        assert!(!harmless_audio_warning("Error while opening encoder"));
    }

    #[test]
    fn prores_proxy_uses_ten_bit_mov_output() {
        let mut config = config();
        config.codec = "prores-proxy".into();
        let probe = ProbeInfo {
            width: 6960,
            height: 4640,
            duration_seconds: 10.0,
            timecode: None,
        };
        let args = ffmpeg_args(
            Path::new("clip.mp4"),
            Path::new("clip.part.mov"),
            &probe,
            &config,
        )
        .join(" ");
        assert!(args.contains("-c:v prores_ks -profile:v 0 -pix_fmt yuv422p10le -vendor apl0"));
        assert_eq!(
            output_path_for_config(Path::new("clip.mp4"), &config).unwrap(),
            PathBuf::from("/output/clip_proxy.mov")
        );
    }

    #[test]
    fn prores_lt_selects_profile_one() {
        let mut config = config();
        config.codec = "prores-proxy".into();
        config.prores_profile = "lt".into();
        let probe = ProbeInfo {
            width: 1920,
            height: 1080,
            duration_seconds: 1.0,
            timecode: None,
        };
        let args = ffmpeg_args(
            Path::new("clip.mp4"),
            Path::new("clip.part.mov"),
            &probe,
            &config,
        )
        .join(" ");
        assert!(args.contains("-c:v prores_ks -profile:v 1"));
    }

    #[test]
    fn four_k_prores_is_limited_to_one_worker() {
        let mut config = config();
        config.codec = "prores-proxy".into();
        config.output_height = 2160;
        config.parallel_jobs = 8;
        assert_eq!(effective_parallel_jobs(&config), 1);
    }
}
