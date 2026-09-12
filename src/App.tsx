import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, ChevronDown, ChevronUp, CircleAlert, Film, FolderOpen, OctagonX, Play, RotateCcw } from "lucide-react";
import { basename, proxyPath } from "./lib";
import type { BatchConfig, BatchProgress, ToolStatus } from "./types";

const squeezes = [1.33, 1.5, 1.6, 1.8, 2];
const heights = [720, 1080];

const initialConfig: BatchConfig = {
  sourceDir: "",
  outputDir: "",
  filenameSuffix: "_proxy",
  squeeze: 1.5,
  outputHeight: 1080,
  bitrateMbps: 6,
  parallelJobs: 2,
  preserveTimecode: true,
  includeAudio: true,
  skipExisting: true
};

function Choice({ label, values, value, onChange, suffix = "" }: { label: string; values: number[]; value: number; onChange: (value: number) => void; suffix?: string }) {
  const [custom, setCustom] = useState(!values.includes(value));
  return (
    <label className="field">
      <span>{label}</span>
      <div className="select-wrap">
        {custom ? (
          <input autoFocus min="1" step="0.01" type="number" value={value} onChange={(e) => onChange(Number(e.target.value))} />
        ) : (
          <select value={value} onChange={(e) => e.target.value === "custom" ? setCustom(true) : onChange(Number(e.target.value))}>
            {values.map((item) => <option key={item} value={item}>{item}{suffix}</option>)}
            <option value="custom">Custom…</option>
          </select>
        )}
        {!custom && <ChevronDown size={15} />}
        {custom && <button className="inline-reset" onClick={() => { setCustom(false); onChange(values[0]); }} type="button">Reset</button>}
      </div>
    </label>
  );
}

function NumericStepper({ label, value, min, max, unit, onChange }: { label: string; value: number; min: number; max: number; unit: string; onChange: (value: number) => void }) {
  const set = (next: number) => onChange(Math.min(max, Math.max(min, Math.round(next))));
  return (
    <label className="field">
      <span>{label}</span>
      <div className="stepper">
        <input type="number" min={min} max={max} value={value} onChange={(e) => set(Number(e.target.value))} />
        <i>{unit}</i>
        <div className="stepper-buttons">
          <button type="button" aria-label={`Increase ${label.toLowerCase()}`} onClick={() => set(value + 1)}><ChevronUp size={13} /></button>
          <button type="button" aria-label={`Decrease ${label.toLowerCase()}`} onClick={() => set(value - 1)}><ChevronDown size={13} /></button>
        </div>
      </div>
    </label>
  );
}

function App() {
  const [config, setConfig] = useState(initialConfig);
  const [tools, setTools] = useState<ToolStatus | null>(null);
  const [batchId, setBatchId] = useState<string | null>(null);
  const [events, setEvents] = useState<BatchProgress[]>([]);
  const [starting, setStarting] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [toolMessage, setToolMessage] = useState("");

  const update = <K extends keyof BatchConfig>(key: K, value: BatchConfig[K]) => setConfig((old) => ({ ...old, [key]: value }));
  const running = Boolean(batchId);
  const latest = events.at(-1);
  const fileEvents = useMemo(() => {
    const byFile = new Map<string, BatchProgress>();
    events.filter((event) => event.file && !["batch-complete", "log"].includes(event.kind)).forEach((event) => byFile.set(event.file, event));
    return [...byFile.values()];
  }, [events]);
  const overall = latest?.totalFiles ? Math.round(fileEvents.reduce((sum, event) => {
    if (["completed", "failed", "skipped"].includes(event.kind)) return sum + 100;
    return sum + event.fileProgress;
  }, 0) / latest.totalFiles) : 0;

  useEffect(() => {
    invoke<ToolStatus>("check_tools").then(setTools).catch(() => setTools({ ffmpeg: false, ffprobe: false }));
    const unlisten = listen<BatchProgress>("batch-progress", ({ payload }) => {
      setEvents((old) => [...old.slice(-499), payload]);
      if (payload.kind === "batch-complete") setBatchId(null);
    });
    const unlistenInstall = listen<{ success: boolean; message: string }>("tool-install-status", ({ payload }) => {
      setInstalling(false);
      setToolMessage(payload.message);
      if (payload.success) invoke<ToolStatus>("check_tools").then(setTools);
    });
    const unlistenDrop = getCurrentWebviewWindow().onDragDropEvent(async (event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") setDragging(true);
      if (event.payload.type === "leave") setDragging(false);
      if (event.payload.type === "drop") {
        setDragging(false);
        const first = event.payload.paths[0];
        if (first && !running) {
          try {
            const sourceDir = await invoke<string>("resolve_source_drop", { path: first });
            setConfig((old) => ({ ...old, sourceDir, outputDir: proxyPath(sourceDir) }));
          } catch (error) {
            setToolMessage(String(error));
          }
        }
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
      void unlistenInstall.then((fn) => fn());
      void unlistenDrop.then((fn) => fn());
    };
  }, [running]);

  async function installTools() {
    setInstalling(true);
    setToolMessage("Installing FFmpeg with Homebrew…");
    try {
      await invoke("install_ffmpeg");
    } catch (error) {
      setInstalling(false);
      setToolMessage(String(error));
    }
  }

  async function chooseSource() {
    const selected = await open({ directory: true, multiple: false, title: "Choose source footage" });
    if (typeof selected === "string") setConfig((old) => ({ ...old, sourceDir: selected, outputDir: proxyPath(selected) }));
  }

  async function chooseOutput() {
    const selected = await open({ directory: true, multiple: false, title: "Choose proxy destination" });
    if (typeof selected === "string") update("outputDir", selected);
  }

  async function start() {
    setStarting(true);
    setEvents([]);
    try {
      const id = await invoke<string>("start_batch", { config });
      setBatchId(id);
    } catch (error) {
      setEvents([{ batchId: "", file: "", fileIndex: 0, totalFiles: 0, fileProgress: 0, completedFiles: 0, failedFiles: 1, skippedFiles: 0, kind: "log", message: String(error) }]);
    } finally {
      setStarting(false);
    }
  }

  async function cancel() {
    if (batchId) await invoke("cancel_batch", { batchId });
  }

  const ready = tools?.ffmpeg && tools?.ffprobe && config.sourceDir && config.outputDir && config.squeeze > 0 && config.outputHeight > 0;

  return (
    <main>
      <header>
        <div className="mark"><img src="/deproxy-logo.svg" alt="" /></div>
        <div><h1>DeProxy</h1><p>Hardware-accelerated anamorphic proxies</p></div>
        <div className={`status ${tools?.ffmpeg && tools?.ffprobe ? "ok" : "bad"}`}>
          {tools === null ? "Checking tools…" : tools.ffmpeg && tools.ffprobe ? <><Check size={13} /> FFmpeg ready</> : <><CircleAlert size={13} /> FFmpeg missing</>}
        </div>
      </header>

      {tools && (!tools.ffmpeg || !tools.ffprobe) && (
        <div className="notice"><CircleAlert size={18} /><div><strong>FFmpeg and FFprobe are required</strong><span>{toolMessage || "Install them automatically with Homebrew."}</span></div><button type="button" onClick={installTools} disabled={installing}>{installing ? "Installing…" : "Install FFmpeg"}</button></div>
      )}

      <section className={`paths ${dragging ? "dragging" : ""}`}>
        <button className={`path-card ${config.sourceDir ? "selected" : ""}`} onClick={chooseSource} disabled={running}>
          <FolderOpen size={20} /><span><small>Source folder</small><strong>{dragging ? "Drop folder here" : config.sourceDir ? basename(config.sourceDir) : "Choose or drop footage…"}</strong><em>{config.sourceDir || "MP4, MOV, MXF and M4V"}</em></span>
        </button>
        <button className={`path-card ${config.outputDir ? "selected" : ""}`} onClick={chooseOutput} disabled={running || !config.sourceDir}>
          <FolderOpen size={20} /><span><small>Output folder</small><strong>{config.outputDir ? basename(config.outputDir) : "Source/Proxy"}</strong><em>{config.outputDir || "Created automatically"}</em></span>
        </button>
      </section>

      <section className="panel">
        <div className="panel-title"><h2>Proxy settings</h2><span>H.265 Main10 · VideoToolbox · hvc1</span></div>
        <div className="settings-grid">
          <Choice label="Anamorphic squeeze" values={squeezes} value={config.squeeze} suffix="×" onChange={(v) => update("squeeze", v)} />
          <Choice label="Output height" values={heights} value={config.outputHeight} suffix="p" onChange={(v) => update("outputHeight", Math.round(v))} />
          <NumericStepper label="Bitrate" value={config.bitrateMbps} min={1} max={100} unit="Mbps" onChange={(v) => update("bitrateMbps", v)} />
          <NumericStepper label="Parallel jobs" value={config.parallelJobs} min={1} max={8} unit="jobs" onChange={(v) => update("parallelJobs", v)} />
          <label className="field wide"><span>Filename suffix</span><input value={config.filenameSuffix} onChange={(e) => update("filenameSuffix", e.target.value)} placeholder="_proxy" /></label>
        </div>
        <div className="toggles">
          {([
            ["preserveTimecode", "Preserve source timecode"],
            ["includeAudio", "AAC audio · 160 kbps"],
            ["skipExisting", "Skip existing outputs"]
          ] as const).map(([key, label]) => (
            <label key={key}><input type="checkbox" checked={config[key]} onChange={(e) => update(key, e.target.checked)} /><span className="switch" />{label}</label>
          ))}
        </div>
      </section>

      {(running || events.length > 0) && (
        <section className="panel progress-panel">
          <div className="panel-title"><h2>{running ? "Encoding" : "Batch summary"}</h2><strong>{overall}%</strong></div>
          <div className="progress-track"><i style={{ width: `${overall}%` }} /></div>
          <div className="summary"><span>{latest?.completedFiles ?? 0} complete</span><span>{latest?.skippedFiles ?? 0} skipped</span><span>{latest?.failedFiles ?? 0} failed</span><span>{latest?.totalFiles ?? 0} total</span></div>
          <div className="file-list">
            {fileEvents.slice(-5).map((event) => (
              <div key={event.file}><span>{event.kind === "failed" ? <CircleAlert size={14} /> : event.kind === "completed" ? <Check size={14} /> : <Film size={14} />}{basename(event.file)}</span><em>{event.kind === "progress" ? `${Math.round(event.fileProgress)}%` : event.kind}</em></div>
            ))}
          </div>
          <div className="log">{events.filter((e) => e.message && ["failed", "log", "batch-complete", "cancelled"].includes(e.kind)).slice(-4).map((e, i) => <div key={i}>{e.message}</div>)}</div>
        </section>
      )}

      <footer>
        {!running && events.length > 0 && <button className="secondary" onClick={() => setEvents([])}><RotateCcw size={16} /> Reset</button>}
        {running ? <button className="danger" onClick={cancel}><OctagonX size={17} /> Cancel batch</button> : <button className="primary" disabled={!ready || starting} onClick={start}><Play size={17} fill="currentColor" />{starting ? "Starting…" : "Generate proxies"}</button>}
      </footer>
    </main>
  );
}

export default App;
