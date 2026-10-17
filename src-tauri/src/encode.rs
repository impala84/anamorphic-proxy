use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
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
    #[serde(default)]
    pub raw_mode: bool,
    #[serde(default)]
    pub raw_project_aspect: bool,
    #[serde(default = "default_raw_quality")]
    pub raw_intermediate_quality: String,
    #[serde(default)]
    pub raw_aspects: HashMap<String, f64>,
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
    #[serde(default = "default_cpu_limit")]
    pub cpu_limit_percent: u8,
}

fn default_raw_quality() -> String {
    "standard".into()
}

fn default_cpu_limit() -> u8 {
    100
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

struct BatchControl {
    cancel: Arc<AtomicBool>,
    cpu_limit: Arc<AtomicU8>,
}

#[derive(Default)]
pub struct BatchManager {
    batches: Mutex<HashMap<String, BatchControl>>,
}

#[derive(Default)]
struct Counts {
    throttle_epoch: OnceLock<Instant>,
    file_progress: Mutex<HashMap<PathBuf, f64>>,
    complete: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
    source_bytes: AtomicU64,
    output_bytes: AtomicU64,
}

impl BatchManager {
    pub fn insert(&self, id: String, flag: Arc<AtomicBool>, cpu_limit: Arc<AtomicU8>) {
        self.batches.lock().expect("batch state poisoned").insert(
            id,
            BatchControl {
                cancel: flag,
                cpu_limit,
            },
        );
    }

    pub fn set_cpu_limit(&self, percent: u8) {
        for control in self.batches.lock().expect("batch state poisoned").values() {
            control.cpu_limit.store(percent, Ordering::Relaxed);
        }
    }

    pub fn cancel(&self, id: &str) -> bool {
        if let Some(control) = self.batches.lock().expect("batch state poisoned").get(id) {
            control.cancel.store(true, Ordering::SeqCst);
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
                        "mp4" | "mov" | "mxf" | "m4v" | "crm"
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
    if config.raw_mode {
        return 1;
    }
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
    Ok(
        if config.codec == "prores-proxy" || crate::raw::is_raw(source) {
            path.with_extension("mov")
        } else {
            path
        },
    )
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

fn raw_ffmpeg_args(
    source: &Path,
    temporary: &Path,
    probe: &ProbeInfo,
    config: &BatchConfig,
    decoded: &Path,
) -> Vec<String> {
    let mut args = ffmpeg_args(source, temporary, probe, config);
    let input = args.iter().position(|v| v == "-i").unwrap();
    args[input + 1] = decoded.to_string_lossy().into_owned();
    args.splice(
        input + 2..input + 2,
        ["-i".into(), source.to_string_lossy().into_owned()],
    );
    for value in &mut args {
        if value == "0:a?" {
            *value = "1:a?".into();
        }
    }
    let metadata = args.iter().position(|v| v == "-map_metadata").unwrap();
    args[metadata + 1] = "1".into();
    let (width, height) = crate::raw::geometry(
        probe.width,
        probe.height,
        config.squeeze,
        config.output_height,
    );
    let filter = args.iter().position(|v| v == "-vf").unwrap();
    args[filter + 1] =
        format!("trim=start=0,scale={width}:{height}:flags=lanczos,setsar=1,setpts=PTS-STARTPTS");
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
    // Update native indicators from the worker, even with a minimized webview.
    if kind == "batch-complete" {
        let _ = crate::set_encoding_progress(app.clone(), None);
    } else if total > 0 && ["started", "progress", "completed", "failed", "skipped"].contains(&kind)
    {
        let mut files = counts
            .file_progress
            .lock()
            .expect("progress state poisoned");
        files.insert(
            file.to_path_buf(),
            if ["completed", "failed", "skipped"].contains(&kind) {
                100.0
            } else {
                progress
            },
        );
        let percent = (files.values().sum::<f64>() / total as f64).round() as u64;
        let _ = crate::set_encoding_progress(app.clone(), Some(percent));
    }
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
    cpu_limit: &AtomicU8,
    counts: &Counts,
    raw_session: &mut Option<crate::raw::Session>,
    raw_sources: &[PathBuf],
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
    let raw_source = crate::raw::is_raw(source);
    let mut workspace = None;
    let mut decoded = None;
    let mut effective = config.clone();
    if raw_source {
        if !config.raw_mode {
            return Err("Canon RAW Light requires Resolve Studio mode. Enable it and review the RAW clips before starting.".into());
        }
        let ratio = config
            .raw_aspects
            .get(&source.to_string_lossy().to_string())
            .copied()
            .ok_or("Review the RAW clip aspect ratios before starting")?;
        if !ratio.is_finite() || !(1.0..=2.0).contains(&ratio) {
            return Err("Invalid RAW aspect ratio".into());
        }
        effective.squeeze = ratio;
        let work = crate::raw::Workspace::new(Path::new(&config.output_dir))?;
        emit(
            app,
            batch_id,
            source,
            index,
            total,
            0.0,
            counts,
            "started",
            Some("Decoding Canon RAW in Resolve Studio…".into()),
        );
        if raw_session.is_none() {
            *raw_session = Some(crate::raw::Session::start(config, raw_sources, cancel)?);
        }
        let result = raw_session.as_mut().ok_or("RAW session unavailable")?.decode(
            serde_json::json!({"action":"decode", "source":source, "directory":work.path, "outputHeight":config.output_height}),
            cancel,
            &work.path,
            |pct| {
                emit(
                    app,
                    batch_id,
                    source,
                    index,
                    total,
                    pct * 0.4,
                    counts,
                    "progress",
                    Some("Decoding RAW".into()),
                );
            },
        )?;
        decoded = Some(result);
        workspace = Some(work);
    }
    let args = if let Some(raw) = &decoded {
        raw_ffmpeg_args(
            source,
            &temporary,
            &probe,
            &effective,
            Path::new(raw["input"].as_str().ok_or("Missing RAW intermediate")?),
        )
    } else {
        ffmpeg_args(source, &temporary, &probe, &effective)
    };

    emit(
        app,
        batch_id,
        source,
        index,
        total,
        if raw_source { 40.0 } else { 0.0 },
        counts,
        "started",
        None,
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

    let epoch = *counts.throttle_epoch.get_or_init(Instant::now);
    let mut paused = false;
    loop {
        if cancel.load(Ordering::SeqCst) {
            terminate_encoder(&mut child, &temporary);
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
                        app,
                        batch_id,
                        source,
                        index,
                        total,
                        if raw_source { 40.0 + pct * 0.5 } else { pct },
                        counts,
                        "progress",
                        None,
                    );
                }
            }
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                terminate_encoder(&mut child, &temporary);
                return Err(format!("Could not monitor ffmpeg: {error}"));
            }
        };
        if let Some(status) = status {
            let diagnostics = stderr_thread.join().unwrap_or_default();
            let useful = diagnostics
                .lines()
                .filter(|line| !harmless_audio_warning(line))
                .collect::<Vec<_>>()
                .join("\n");
            if status.success() {
                if let (Some(raw), Some(work)) = (&decoded, &workspace) {
                    emit(
                        app,
                        batch_id,
                        source,
                        index,
                        total,
                        90.0,
                        counts,
                        "progress",
                        Some("Verifying RAW frames, timecode and audio…".into()),
                    );
                    if let Err(error) = crate::raw::run(
                        serde_json::json!({"action":"verify", "source":source, "output":temporary, "frames":raw["frames"], "timecode":raw["timecode"], "preserveTimecode":config.preserve_timecode, "audioMode":config.audio_mode}),
                        cancel,
                        &work.path,
                        |_| {},
                    ) {
                        let _ = fs::remove_file(&temporary);
                        return Err(error);
                    }
                }
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
        // All workers share the same 200ms duty cycle so their combined CPU
        // time stays within the selected fraction of total machine capacity.
        let limit = cpu_limit.load(Ordering::Relaxed);
        #[cfg(unix)]
        {
            let should_pause = limit < 100 && throttle_paused(epoch.elapsed(), limit);
            if let Err(error) = apply_throttle(&child, &mut paused, should_pause) {
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                terminate_encoder(&mut child, &temporary);
                return Err(format!("Could not apply CPU limit: {error}"));
            }
        }
        thread::sleep(Duration::from_millis(if limit < 100 { 5 } else { 100 }));
    }
}

#[cfg(unix)]
fn apply_throttle(child: &Child, paused: &mut bool, should_pause: bool) -> std::io::Result<()> {
    if should_pause != *paused {
        let signal = if should_pause {
            libc::SIGSTOP
        } else {
            libc::SIGCONT
        };
        // SAFETY: the caller owns this live child process.
        if unsafe { libc::kill(child.id() as libc::pid_t, signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        *paused = should_pause;
    }
    Ok(())
}

fn terminate_encoder(child: &mut Child, temporary: &Path) {
    // SIGKILL also terminates a worker stopped by the CPU limiter.
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(temporary);
}

fn throttle_paused(elapsed: Duration, percent: u8) -> bool {
    elapsed.as_millis() % 200 >= u128::from(percent) * 2
}

pub fn run_batch(
    app: AppHandle,
    manager: Arc<BatchManager>,
    id: String,
    config: BatchConfig,
    cancel: Arc<AtomicBool>,
    cpu_limit: Arc<AtomicU8>,
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
    let raw_sources = Arc::new(
        files
            .iter()
            .filter(|p| crate::raw::is_raw(p))
            .filter(|p| {
                !config.skip_existing
                    || output_path_for_config(p, &config).is_ok_and(|out| !out.exists())
            })
            .cloned()
            .collect::<Vec<_>>(),
    );
    let files = Arc::new(files);
    let mut workers = Vec::new();
    for _ in 0..effective_parallel_jobs(&config) {
        let (app, id, config, cancel, cpu_limit, counts, next, files) = (
            app.clone(),
            id.clone(),
            config.clone(),
            cancel.clone(),
            cpu_limit.clone(),
            counts.clone(),
            next.clone(),
            files.clone(),
        );
        let raw_sources = raw_sources.clone();
        workers.push(thread::spawn(move || {
            let mut raw_session = None;
            loop {
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
                    &cpu_limit,
                    &counts,
                    &mut raw_session,
                    &raw_sources,
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
            }
            if let Some(mut session) = raw_session {
                if let Err(error) = session.finish() {
                    counts.failed.fetch_add(1, Ordering::SeqCst);
                    emit(
                        &app,
                        &id,
                        Path::new(&config.source_dir),
                        total,
                        total,
                        0.0,
                        &counts,
                        "log",
                        Some(format!("Resolve cleanup: {error}")),
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

    #[test]
    fn raw_args_use_original_audio_and_bake_reviewed_aspect_once() {
        let mut settings = config();
        settings.output_height = 2160;
        settings.audio_mode = "preserve".into();
        let probe = ProbeInfo {
            width: 6000,
            height: 3164,
            duration_seconds: 8.008,
            timecode: Some("17:09:39:21".into()),
        };
        let args = raw_ffmpeg_args(
            Path::new("source.CRM"),
            Path::new("proxy.mov"),
            &probe,
            &settings,
            Path::new("decode.mov"),
        );
        assert!(args.windows(2).any(|v| v == ["-map", "1:a?"]));
        assert!(args.windows(2).any(|v| v == ["-c:a", "copy"]));
        assert!(args
            .iter()
            .any(|v| v.contains("scale=4096:1440:flags=lanczos,setsar=1")));
        assert!(!args.iter().any(|v| v == "0:a?"));
    }

    #[test]
    #[ignore = "requires a running Resolve Studio and DEPROXY_RAW_SOURCE/DEPROXY_RAW_OUTPUT"]
    fn real_resolve_raw_smoke() {
        let source = PathBuf::from(std::env::var("DEPROXY_RAW_SOURCE").unwrap());
        let output = PathBuf::from(std::env::var("DEPROXY_RAW_OUTPUT").unwrap());
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        let mut settings = config();
        settings.output_dir = output.parent().unwrap().to_string_lossy().into_owned();
        settings.raw_mode = true;
        settings.raw_project_aspect = true;
        settings.source_dir = source.parent().unwrap().to_string_lossy().into_owned();
        settings.output_height = 2160;
        settings.audio_mode = "preserve".into();
        let before = crate::raw::inspect(&settings).unwrap();
        let aspect = before["clips"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["file"].as_str() == source.to_str())
            .unwrap()["squeeze"]
            .as_f64()
            .unwrap();
        assert_eq!(aspect, 1.5);
        settings.squeeze = aspect;
        let workspace = crate::raw::Workspace::new(output.parent().unwrap()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut sources = vec![source.clone()];
        if let Ok(second) = std::env::var("DEPROXY_RAW_SECOND") {
            sources.push(PathBuf::from(second));
        }
        let mut session = crate::raw::Session::start(&settings, &sources, &cancel).unwrap();
        let decoded = session.decode(serde_json::json!({"action":"decode","source":source,"directory":workspace.path,"outputHeight":2160}), &cancel, &workspace.path, |_| {}).unwrap();
        let args = raw_ffmpeg_args(
            &source,
            &output,
            &probe_file(&source).unwrap(),
            &settings,
            Path::new(decoded["input"].as_str().unwrap()),
        );
        let encoded = Command::new(tool_path("ffmpeg").unwrap())
            .args(args)
            .output()
            .unwrap();
        assert!(
            encoded.status.success(),
            "{}",
            String::from_utf8_lossy(&encoded.stderr)
        );
        crate::raw::run(serde_json::json!({"action":"verify","source":source,"output":output,"frames":decoded["frames"],"timecode":decoded["timecode"],"preserveTimecode":true,"audioMode":"preserve"}),&cancel,&workspace.path,|_| {}).unwrap();
        if let Some(second) = sources.get(1) {
            let second_work = crate::raw::Workspace::new(output.parent().unwrap()).unwrap();
            let second_decoded = session.decode(serde_json::json!({"action":"decode","source":second,"directory":second_work.path,"outputHeight":2160}), &cancel, &second_work.path, |_| {}).unwrap();
            assert_eq!(decoded["project"], second_decoded["project"]);
            assert_eq!(second_decoded["frames"], 30);
        }
        session.finish().unwrap();
        let after = crate::raw::run(
            serde_json::json!({"action":"status"}),
            &cancel,
            &workspace.path,
            |_| {},
        )
        .unwrap();
        assert_eq!(before["project"], after["project"]);
        let proxy = probe_file(&output).unwrap();
        assert_eq!((proxy.width, proxy.height), (4096, 1440));
    }

    #[test]
    fn changing_limit_updates_all_running_batches_without_affecting_cancellation() {
        let manager = BatchManager::default();
        let first = Arc::new(AtomicU8::new(100));
        let second = Arc::new(AtomicU8::new(50));
        manager.insert(
            "first".into(),
            Arc::new(AtomicBool::new(false)),
            first.clone(),
        );
        manager.insert(
            "second".into(),
            Arc::new(AtomicBool::new(false)),
            second.clone(),
        );
        manager.set_cpu_limit(10);
        assert_eq!(first.load(Ordering::Relaxed), 10);
        assert_eq!(second.load(Ordering::Relaxed), 10);
        manager.set_cpu_limit(100);
        assert_eq!(first.load(Ordering::Relaxed), 100);
        assert_eq!(second.load(Ordering::Relaxed), 100);
        assert!(manager.cancel("first"));
        manager.remove("first");
        manager.set_cpu_limit(25);
        assert_eq!(second.load(Ordering::Relaxed), 25);
        assert_eq!(first.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn cpu_duty_cycle_is_shared_and_unlimited_by_default() {
        assert_eq!(default_cpu_limit(), 100);
        assert!(!throttle_paused(Duration::from_millis(99), 50));
        assert!(throttle_paused(Duration::from_millis(100), 50));
        assert!(!throttle_paused(Duration::from_millis(200), 50));
        assert!(!throttle_paused(Duration::from_millis(199), 100));
    }

    #[cfg(unix)]
    #[test]
    fn removing_limit_resumes_a_stopped_process() {
        let mut child = Command::new("/bin/sleep").arg("0.1").spawn().unwrap();
        let mut paused = false;
        apply_throttle(&child, &mut paused, true).unwrap();
        assert!(paused);
        thread::sleep(Duration::from_millis(150));
        assert!(child.try_wait().unwrap().is_none());
        apply_throttle(&child, &mut paused, false).unwrap();
        assert!(!paused);
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("encoder did not resume");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_reaps_a_paused_encoder_and_removes_partial_output() {
        let path = std::env::temp_dir().join(format!("deproxy-cancel-{}", Uuid::new_v4()));
        fs::write(&path, b"partial proxy").unwrap();
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        // SAFETY: this test owns the live child process.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGSTOP) },
            0
        );
        terminate_encoder(&mut child, &path);
        assert!(child.try_wait().unwrap().is_some());
        assert!(!path.exists());
    }

    fn config() -> BatchConfig {
        BatchConfig {
            raw_intermediate_quality: "standard".into(),
            raw_mode: false,
            raw_project_aspect: false,
            raw_aspects: HashMap::new(),
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
            cpu_limit_percent: 100,
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
