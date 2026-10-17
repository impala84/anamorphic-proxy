use crate::encode::{tool_path, BatchConfig};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const BRIDGE: &str = include_str!("../scripts/resolve_raw.py");

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("crm"))
}

fn python() -> Result<PathBuf, String> {
    if !cfg!(target_os = "macos") {
        return Err("Canon RAW mode currently requires macOS and Resolve Studio".into());
    }
    [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|p| {
        p.to_str() != Some("/usr/bin/python3")
            || Path::new("/Library/Developer/CommandLineTools/usr/bin/python3").is_file()
            || Path::new("/Applications/Xcode.app/Contents/Developer/usr/bin/python3").is_file()
    })
    .find(|p| p.is_file())
    .ok_or("Install Python 3 to use Resolve RAW mode".into())
}

pub struct Workspace {
    pub path: PathBuf,
}
impl Workspace {
    pub fn new(parent: &Path) -> Result<Self, String> {
        let path = parent.join(format!(".deproxy-raw-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        Ok(Self { path })
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn run(
    mut request: Value,
    cancel: &AtomicBool,
    workspace: &Path,
    mut progress: impl FnMut(f64),
) -> Result<Value, String> {
    let action = request["action"].as_str().unwrap_or("").to_owned();
    let started = Instant::now();
    let mut connected = false;
    let marker = workspace.join("cancel");
    request["cancelPath"] = json!(marker);
    request["ffprobe"] = json!(tool_path("ffprobe").ok_or("FFprobe was not found")?);
    request["ffmpeg"] = json!(tool_path("ffmpeg").ok_or("FFmpeg was not found")?);
    let mut child = Command::new(python()?)
        .args(["-u", "-c", BRIDGE, &request.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start Python 3: {e}"))?;
    let stdout = child.stdout.take().ok_or("No RAW bridge output")?;
    let mut stderr = child.stderr.take().ok_or("No RAW bridge diagnostics")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let diagnostics = thread::spawn(move || {
        let mut data = String::new();
        let _ = stderr.read_to_string(&mut data);
        data
    });
    let mut result = None;
    let mut error = None;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = fs::write(&marker, b"cancel");
        }
        for line in rx.try_iter() {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value["connected"].as_bool() == Some(true) {
                    connected = true;
                }
                if let Some(pct) = value["progress"].as_f64() {
                    progress(pct);
                }
                if let Some(message) = value["error"].as_str() {
                    error = Some(message.to_owned());
                }
                if let Some(value) = value.get("result") {
                    result = Some(value.clone());
                }
            }
        }
        if started.elapsed() > Duration::from_secs(60)
            && (matches!(action.as_str(), "inspect" | "status")
                || (action == "decode" && !connected))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err("RAW preparation timed out. Check that the source drive is accessible, Resolve Studio is open with local external scripting enabled, and Python is compatible with Resolve.".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            // The reader may still be delivering its final JSON after process exit.
            for line in rx.iter() {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if let Some(message) = value["error"].as_str() {
                        error = Some(message.to_owned());
                    }
                    if let Some(value) = value.get("result") {
                        result = Some(value.clone());
                    }
                }
            }
            let diagnostics = diagnostics.join().unwrap_or_default();
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".into());
            }
            if !status.success() || error.is_some() {
                return Err(
                    error.unwrap_or_else(|| format!("Resolve bridge failed: {diagnostics}"))
                );
            }
            return result.ok_or("Resolve bridge returned no result".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// A single bridge process owns Resolve until the batch is finished.
pub struct Session {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    workspace: Workspace,
    closed: bool,
    connected: bool,
}
impl Session {
    pub fn start(
        config: &BatchConfig,
        sources: &[PathBuf],
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let workspace = Workspace::new(Path::new(&config.output_dir))?;
        let request = json!({"action":"serve","sources":sources,"outputHeight":config.output_height,"intermediateQuality":config.raw_intermediate_quality,"cancelPath":workspace.path.join("cancel")});
        let mut child = Command::new(python()?)
            .args(["-u", "-c", BRIDGE, &request.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().ok_or("No Resolve bridge output")?;
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let mut session = Self {
            child,
            input,
            lines,
            workspace,
            closed: false,
            connected: false,
        };
        session.receive(cancel, None, |_| {})?;
        Ok(session)
    }
    fn receive(
        &mut self,
        cancel: &AtomicBool,
        marker: Option<&Path>,
        mut progress: impl FnMut(f64),
    ) -> Result<Value, String> {
        let started = Instant::now();
        loop {
            if !self.connected && started.elapsed() > Duration::from_secs(60) {
                self.closed = true;
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err("Resolve did not respond. Open Resolve Studio and enable local external scripting.".into());
            }
            if cancel.load(Ordering::SeqCst) {
                let _ = fs::write(
                    marker.unwrap_or(&self.workspace.path.join("cancel")),
                    b"cancel",
                );
            }
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        if value["connected"].as_bool() == Some(true) {
                            self.send(&json!({"action":"begin"}))?;
                            self.connected = true;
                        }
                        if let Some(pct) = value["progress"].as_f64() {
                            progress(pct);
                        }
                        if let Some(error) = value["error"].as_str() {
                            return Err(error.into());
                        }
                        if let Some(result) = value.get("result") {
                            return Ok(result.clone());
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("Resolve bridge closed unexpectedly".into()),
            }
        }
    }
    pub fn decode(
        &mut self,
        mut request: Value,
        cancel: &AtomicBool,
        workspace: &Path,
        progress: impl FnMut(f64),
    ) -> Result<Value, String> {
        let marker = workspace.join("cancel");
        request["cancelPath"] = json!(marker);
        request["ffprobe"] = json!(tool_path("ffprobe").ok_or("FFprobe was not found")?);
        request["ffmpeg"] = json!(tool_path("ffmpeg").ok_or("FFmpeg was not found")?);
        self.send(&request)?;
        self.receive(cancel, Some(&marker), progress)
    }
    fn send(&mut self, value: &Value) -> Result<(), String> {
        use std::io::Write;
        let input = self.input.as_mut().ok_or("Resolve bridge is closed")?;
        writeln!(input, "{value}")
            .and_then(|_| input.flush())
            .map_err(|e| e.to_string())
    }
    pub fn finish(&mut self) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // EOF also triggers the Python context manager, including during unwinding.
        let sent = self.send(&json!({"action":"close"}));
        self.input.take();
        let result = if sent.is_ok() {
            self.receive(&AtomicBool::new(false), None, |_| {})
                .map(|_| ())
        } else {
            sent
        };
        let _ = self.child.wait();
        result
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

pub fn inspect(config: &BatchConfig) -> Result<Value, String> {
    let workspace = Workspace::new(&std::env::temp_dir())?;
    run(
        json!({"action":"inspect", "sourceDir":config.source_dir, "squeeze":config.squeeze, "useProjectAspect":config.raw_project_aspect}),
        &AtomicBool::new(false),
        &workspace.path,
        |_| {},
    )
}

pub fn geometry(width: u32, height: u32, squeeze: f64, output_height: u32) -> (u32, u32) {
    let width = crate::encode::derive_width(width, height, squeeze, output_height);
    let cap = ((output_height as f64 * 4096.0 / 2160.0 / 2.0).round() * 2.0) as u32;
    if width > cap {
        (
            cap,
            ((output_height as f64 * cap as f64 / width as f64 / 2.0).round() * 2.0) as u32,
        )
    } else {
        (width, output_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anamorphic_raw_is_desqueezed_once_within_4k_width() {
        assert_eq!(geometry(6000, 3164, 1.5, 2160), (4096, 1440));
        assert_eq!(geometry(6000, 3164, 1.0, 2160), (4096, 2160));
    }
}
