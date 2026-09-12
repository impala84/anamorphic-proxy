mod encode;

use encode::{run_batch, tool_path, BatchConfig, BatchManager};
use serde::Serialize;
use std::{
    path::PathBuf,
    process::Command,
    sync::{atomic::AtomicBool, Arc},
};
use tauri::{Emitter, State};
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolStatus {
    ffmpeg: bool,
    ffprobe: bool,
    ffmpeg_version: Option<String>,
}

#[derive(Clone, Serialize)]
struct ToolInstallStatus {
    success: bool,
    message: String,
}

fn available(command: &str) -> (bool, Option<String>) {
    let Some(path) = tool_path(command) else {
        return (false, None);
    };
    match Command::new(path).arg("-version").output() {
        Ok(output) if output.status.success() => {
            let line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map(ToOwned::to_owned);
            (true, line)
        }
        _ => (false, None),
    }
}

#[tauri::command]
fn check_tools() -> ToolStatus {
    let (ffmpeg, ffmpeg_version) = available("ffmpeg");
    let (ffprobe, _) = available("ffprobe");
    ToolStatus {
        ffmpeg,
        ffprobe,
        ffmpeg_version,
    }
}

#[tauri::command]
fn resolve_source_drop(path: String) -> Result<String, String> {
    let path = PathBuf::from(path);
    if path.is_dir() {
        return Ok(path.to_string_lossy().into_owned());
    }
    if path.is_file() {
        return path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .ok_or_else(|| "The dropped file has no containing folder".into());
    }
    Err("Drop a source folder or a video file".into())
}

#[tauri::command]
fn install_ffmpeg(app: tauri::AppHandle) -> Result<(), String> {
    let brew = tool_path("brew").ok_or(
        "Homebrew is required for automatic installation. Install it from brew.sh, then try again.",
    )?;
    std::thread::spawn(move || {
        let result = Command::new(brew).args(["install", "ffmpeg"]).output();
        let status = match result {
            Ok(output) if output.status.success() => ToolInstallStatus {
                success: true,
                message: "FFmpeg installed successfully".into(),
            },
            Ok(output) => {
                let details = String::from_utf8_lossy(&output.stderr);
                let concise = details.lines().rev().take(4).collect::<Vec<_>>();
                ToolInstallStatus {
                    success: false,
                    message: format!(
                        "FFmpeg installation failed: {}",
                        concise.into_iter().rev().collect::<Vec<_>>().join(" ")
                    ),
                }
            }
            Err(error) => ToolInstallStatus {
                success: false,
                message: format!("Could not start Homebrew: {error}"),
            },
        };
        let _ = app.emit("tool-install-status", status);
    });
    Ok(())
}

#[tauri::command]
fn start_batch(
    app: tauri::AppHandle,
    manager: State<'_, Arc<BatchManager>>,
    config: BatchConfig,
) -> Result<String, String> {
    if config.source_dir.is_empty() || config.output_dir.is_empty() {
        return Err("Choose source and output folders".into());
    }
    if config.squeeze <= 0.0 || config.output_height == 0 || config.bitrate_mbps <= 0.0 {
        return Err("Encoding settings must be greater than zero".into());
    }
    if !matches!(
        config.codec.as_str(),
        "hevc-main10" | "hevc" | "h264" | "prores-proxy"
    ) {
        return Err("Choose a supported encoder".into());
    }
    if config.codec == "prores-proxy"
        && !matches!(
            config.prores_profile.as_str(),
            "proxy" | "lt" | "standard" | "hq"
        )
    {
        return Err("Choose a supported ProRes quality".into());
    }
    let (ffmpeg, _) = available("ffmpeg");
    let (ffprobe, _) = available("ffprobe");
    if !ffmpeg || !ffprobe {
        return Err("FFmpeg and FFprobe must be installed and available in PATH".into());
    }
    let id = Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    manager.insert(id.clone(), cancel.clone());
    let manager = manager.inner().clone();
    let task_id = id.clone();
    std::thread::spawn(move || run_batch(app, manager, task_id, config, cancel));
    Ok(id)
}

#[tauri::command]
fn cancel_batch(manager: State<'_, Arc<BatchManager>>, batch_id: String) -> Result<(), String> {
    if manager.cancel(&batch_id) {
        Ok(())
    } else {
        Err("Batch is no longer running".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(BatchManager::default()))
        .invoke_handler(tauri::generate_handler![
            check_tools,
            resolve_source_drop,
            install_ffmpeg,
            start_batch,
            cancel_batch
        ])
        .run(tauri::generate_context!())
        .expect("error while running Anamorphic Proxy");
}
