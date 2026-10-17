import { useEffect, useMemo, useRef, useState, type MouseEvent } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, ChevronDown, ChevronUp, CircleAlert, Film, FolderOpen, OctagonX, Play, RotateCcw, Settings, X } from "lucide-react";
import { basename, formatBytes, formatElapsed, latestFileEvents, overallProgress, proxyPath, recommendedBitrate, reductionPercent } from "./lib";
import type { BatchConfig, BatchProgress, ToolStatus } from "./types";
import { version as appVersion } from "../package.json";

type PresetSettings = Omit<BatchConfig, "sourceDir" | "outputDir"> & { proxyFolderName: string };
type SavedPreset = { name: string; settings: PresetSettings };
const presetStorageKey = "deproxy-presets-v1";

const squeezes = [1, 1.33, 1.5, 1.6, 1.8, 2];
const heights = [720, 1080, 2160];

const initialConfig: BatchConfig = {
  rawMode: false,
  rawProjectAspect: true,
  rawIntermediateQuality: "standard",
  sourceDir: "",
  outputDir: "",
  filenameSuffix: "_proxy",
  codec: "hevc-main10",
  proresProfile: "proxy",
  squeeze: 1.5,
  outputHeight: 1080,
  bitrateMbps: 6,
  parallelJobs: 2,
  preserveTimecode: true,
  audioMode: "aac",
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
            {values.map((item) => <option key={item} value={item}>{suffix === "×" && item === 1 ? "None (1:1)" : `${item}${suffix}`}</option>)}
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

function dragWindow(event: MouseEvent<HTMLElement>) {
  if (isTauri() && event.button === 0 && !(event.target as HTMLElement).closest("button")) {
    event.preventDefault();
    void getCurrentWebviewWindow().startDragging().catch(console.error);
  }
}

function App() {
  const [config, setConfig] = useState(initialConfig);
  const [sourceInfo, setSourceInfo] = useState<{ totalFiles: number; rawClips: number } | null>(null);
  const [sourceChecking, setSourceChecking] = useState(false);
  const usesRaw = (sourceInfo?.rawClips ?? 0) > 0;
  useEffect(() => {
    let stale = false;
    setSourceInfo(null);
    if (!config.sourceDir || !isTauri()) return;
    setSourceChecking(true);
    void invoke<{ totalFiles: number; rawClips: number }>("inspect_source", { sourceDir: config.sourceDir }).then((info) => {
      if (stale) return;
      setSourceInfo(info);
      setConfig((old) => ({ ...old, rawMode: info.rawClips > 0, audioMode: info.rawClips > 0 ? "preserve" : old.audioMode }));
    }).catch((error) => { if (!stale) setToolMessage(String(error)); }).finally(() => { if (!stale) setSourceChecking(false); });
    return () => { stale = true; };
  }, [config.sourceDir]);
  const [rawPlan, setRawPlan] = useState<{ project: string; clips: { file: string; squeeze: number; fromProject: boolean }[] } | null>(null);
  const [rawReviewOpen, setRawReviewOpen] = useState(false);
  const rawDialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { if (rawReviewOpen) rawDialog.current?.showModal(); else rawDialog.current?.close(); }, [rawReviewOpen]);
  const [tools, setTools] = useState<ToolStatus | null>(null);
  const [batchId, setBatchId] = useState<string | null>(null);
  const [events, setEvents] = useState<BatchProgress[]>([]);
  const [starting, setStarting] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [toolMessage, setToolMessage] = useState("");
  const [proxyFolderName, setProxyFolderName] = useState("Proxy");
  const [elapsedSeconds, setElapsedSeconds] = useState<number | null>(null);
  const [presets, setPresets] = useState<SavedPreset[]>(() => {
    try { return JSON.parse(localStorage.getItem(presetStorageKey) || "[]") as SavedPreset[]; } catch { return []; }
  });
  const [selectedPreset, setSelectedPreset] = useState("");
  const [savingPreset, setSavingPreset] = useState(false);
  const [presetName, setPresetName] = useState("");
  const [preferencesOpen, setPreferencesOpen] = useState(false);
  const [cpuLimit, setCpuLimit] = useState(() => {
    try {
      const saved = Number(localStorage.getItem("deproxy-cpu-limit-v1") ?? 100);
      return Number.isFinite(saved) ? Math.min(100, Math.max(10, Math.round(saved))) : 100;
    } catch { return 100; }
  });
  const cpuLimitRef = useRef(cpuLimit);
  const preferencesDialog = useRef<HTMLDialogElement>(null);
  const completedBatchId = useRef<string | null>(null);
  const startedAt = useRef<number | null>(null);

  const update = <K extends keyof BatchConfig>(key: K, value: BatchConfig[K]) => setConfig((old) => ({ ...old, [key]: value }));
  const running = Boolean(batchId);
  const latest = events.at(-1);
  const batchResult = [...events].reverse().find((event) => event.kind === "batch-complete");
  const statusEvent = batchResult ?? latest;
  const fileEvents = useMemo(() => {
    return latestFileEvents(events);
  }, [events]);
  const batchFinished = batchResult?.kind === "batch-complete" && (batchResult.totalFiles === 0 || batchResult.completedFiles + batchResult.skippedFiles + batchResult.failedFiles === batchResult.totalFiles);
  const overall = overallProgress(statusEvent, fileEvents);

  useEffect(() => {
    invoke<ToolStatus>("check_tools").then(setTools).catch(() => setTools({ ffmpeg: false, ffprobe: false }));
    const unlisten = isTauri() ? listen<BatchProgress>("batch-progress", ({ payload }) => {
      setEvents((old) => [...old.slice(-499), payload]);
      if (payload.kind === "batch-complete") {
        completedBatchId.current = payload.batchId;
        setBatchId(null);
        if (startedAt.current) setElapsedSeconds((Date.now() - startedAt.current) / 1000);
      }
    }) : Promise.resolve(() => {});
    const unlistenInstall = isTauri() ? listen<{ success: boolean; message: string }>("tool-install-status", ({ payload }) => {
      setInstalling(false);
      setToolMessage(payload.message);
      if (payload.success) invoke<ToolStatus>("check_tools").then(setTools);
    }) : Promise.resolve(() => {});
    const unlistenDrop = isTauri() ? getCurrentWebviewWindow().onDragDropEvent(async (event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") setDragging(true);
      if (event.payload.type === "leave") setDragging(false);
      if (event.payload.type === "drop") {
        setDragging(false);
        const first = event.payload.paths[0];
        if (first && !running) {
          try {
            const sourceDir = await invoke<string>("resolve_source_drop", { path: first });
            setConfig((old) => ({ ...old, sourceDir, outputDir: proxyPath(sourceDir, proxyFolderName) }));
          } catch (error) {
            setToolMessage(String(error));
          }
        }
      }
    }) : Promise.resolve(() => {});
    return () => {
      void unlisten.then((fn) => fn());
      void unlistenInstall.then((fn) => fn());
      void unlistenDrop.then((fn) => fn());
    };
  }, [running, proxyFolderName]);

  useEffect(() => {
    const dialog = preferencesDialog.current;
    if (preferencesOpen) dialog?.showModal();
    else dialog?.close();
  }, [preferencesOpen]);

  function saveCpuLimit(value: number) {
    cpuLimitRef.current = value;
    setCpuLimit(value);
    if (isTauri()) void invoke("set_cpu_limit", { percent: value }).catch((error) => setToolMessage(`Could not apply CPU limit: ${String(error)}`));
    try { localStorage.setItem("deproxy-cpu-limit-v1", String(value)); }
    catch { setToolMessage("Could not save the CPU preference. It will apply for this session."); }
  }

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
    if (typeof selected === "string") setConfig((old) => ({ ...old, sourceDir: selected, outputDir: proxyPath(selected, proxyFolderName) }));
  }

  async function chooseOutput() {
    const selected = await open({ directory: true, multiple: false, title: "Choose proxy destination" });
    if (typeof selected === "string") update("outputDir", selected);
  }

  async function start(confirmed = false) {
    const batchConfig = { ...config, rawMode: usesRaw };
    if (!confirmed) {
      setStarting(true);
      try {
        const info = await invoke<{ rawClips: number; totalFiles: number }>("inspect_source", { sourceDir: config.sourceDir });
        setSourceInfo(info);
        batchConfig.rawMode = info.rawClips > 0;
      } catch (error) { setToolMessage(String(error)); setStarting(false); return; }
    }
    if (batchConfig.rawMode && !confirmed) {
      setStarting(true);
      try {
        const plan = await invoke<NonNullable<typeof rawPlan>>("inspect_raw", { config: batchConfig });
        if (!plan.clips.length) throw new Error("No Canon RAW Light (.CRM) clips found in this folder. Turn off RAW mode for regular video files.");
        setRawPlan(plan);
        setRawReviewOpen(true);
      } catch (error) { setToolMessage(String(error)); }
      finally { setStarting(false); }
      return;
    }
    setRawReviewOpen(false);
    setStarting(true);
    completedBatchId.current = null;
    setEvents([]);
    setElapsedSeconds(null);
    startedAt.current = Date.now();
    try {
      const id = await invoke<string>("start_batch", { config: { ...batchConfig, rawAspects: batchConfig.rawMode ? Object.fromEntries(rawPlan?.clips.map((clip) => [clip.file, clip.squeeze]) ?? []) : {}, cpuLimitPercent: cpuLimitRef.current } });
      if (completedBatchId.current !== id) setBatchId(id);
      void invoke("set_cpu_limit", { percent: cpuLimitRef.current }).catch((error) => setToolMessage(`Could not apply CPU limit: ${String(error)}`));
    } catch (error) {
      setEvents([{ batchId: "", file: "", fileIndex: 0, totalFiles: 0, fileProgress: 0, completedFiles: 0, failedFiles: 1, skippedFiles: 0, sourceBytes: 0, outputBytes: 0, kind: "log", message: String(error) }]);
    } finally {
      setStarting(false);
    }
  }

  function renameProxyFolder(value: string) {
    const safe = value.replace(/[\\/:]/g, "");
    setProxyFolderName(safe);
    if (config.sourceDir) update("outputDir", proxyPath(config.sourceDir, safe || "Proxy"));
  }

  function selectCodec(codec: BatchConfig["codec"]) {
    setConfig((old) => ({
      ...old,
      codec,
      bitrateMbps: codec === "prores-proxy" ? old.bitrateMbps : recommendedBitrate(old.outputHeight, codec),
      parallelJobs: codec === "prores-proxy" ? Math.min(old.parallelJobs, old.outputHeight >= 2160 ? 1 : 2) : old.parallelJobs
    }));
  }

  function selectOutputHeight(outputHeight: number) {
    setConfig((old) => ({
      ...old,
      outputHeight: Math.round(outputHeight),
      bitrateMbps: old.codec === "prores-proxy" ? old.bitrateMbps : recommendedBitrate(outputHeight, old.codec),
      parallelJobs: old.codec === "prores-proxy" && outputHeight >= 2160 ? 1 : old.parallelJobs
    }));
  }

  function applyPreset(name: string) {
    setSelectedPreset(name);
    const preset = presets.find((item) => item.name === name);
    if (!preset) return;
    const { proxyFolderName: folderName, ...settings } = preset.settings;
    const compatibleSettings = { ...settings } as Partial<BatchConfig> & { includeAudio?: boolean };
    if (!compatibleSettings.audioMode && "includeAudio" in compatibleSettings) {
      compatibleSettings.audioMode = compatibleSettings.includeAudio ? "aac" : "none";
    }
    delete compatibleSettings.includeAudio;
    setProxyFolderName(folderName);
    setConfig((old) => ({
      ...old,
      ...compatibleSettings,
      outputDir: old.sourceDir ? proxyPath(old.sourceDir, folderName) : old.outputDir
    }));
  }

  function savePreset() {
    const name = presetName.trim();
    if (!name) return;
    const { sourceDir: _sourceDir, outputDir: _outputDir, ...settings } = config;
    const preset: SavedPreset = { name, settings: { ...settings, proxyFolderName } };
    const next = [...presets.filter((item) => item.name.toLocaleLowerCase() !== name.toLocaleLowerCase()), preset].sort((a, b) => a.name.localeCompare(b.name));
    setPresets(next);
    localStorage.setItem(presetStorageKey, JSON.stringify(next));
    setSelectedPreset(name);
    setPresetName("");
    setSavingPreset(false);
  }

  const codecLabels: Record<BatchConfig["codec"], string> = {
    "hevc-main10": "H.265 Main10 · VideoToolbox · hvc1",
    hevc: "H.265 8-bit · VideoToolbox · hvc1",
    h264: "H.264 8-bit · VideoToolbox · avc1",
    "prores-proxy": "ProRes Proxy · 10-bit 4:2:2 · CPU"
  };

  async function cancel() {
    if (batchId) await invoke("cancel_batch", { batchId });
  }

  const ready = tools?.ffmpeg && tools?.ffprobe && config.sourceDir && config.outputDir && config.squeeze > 0 && config.outputHeight > 0;

  return (
    <>
    <div className="window-drag-strip" aria-hidden="true" onMouseDown={dragWindow} />
    <main>
      <header onMouseDown={dragWindow}>
        <div className="mark"><img src="/deproxy-logo.svg" alt="" /></div>
        <p><span className="app-version">v{appVersion}</span> Hardware-accelerated anamorphic proxies</p>
        <div className={`status ${tools?.ffmpeg && tools?.ffprobe ? "ok" : "bad"}`}>
          {tools === null ? "Checking tools…" : tools.ffmpeg && tools.ffprobe ? <><Check size={13} /> FFmpeg ready</> : <><CircleAlert size={13} /> FFmpeg missing</>}
        </div>
        <button type="button" className="preferences-button" aria-label="Preferences" title="Preferences" onClick={() => setPreferencesOpen(true)}><Settings size={18} /></button>
      </header>

      <dialog ref={preferencesDialog} className="preferences" aria-labelledby="preferences-title" onCancel={() => setPreferencesOpen(false)} onClose={() => setPreferencesOpen(false)}>
        <div className="panel-title"><h2 id="preferences-title">Preferences</h2><button type="button" className="preferences-button" aria-label="Close preferences" onClick={() => setPreferencesOpen(false)}><X size={18} /></button></div>
        <label className="field"><span>Maximum CPU use · {cpuLimit}%</span><input aria-label="Maximum CPU use" type="range" min="10" max="100" step="5" value={cpuLimit} onChange={(event) => saveCpuLimit(Number(event.target.value))} /></label>
        <p>100% uses all available CPU capacity. Lower limits reduce average encoding CPU use across all jobs and may slow encoding. Short peaks are possible; hardware video encoding is separate.</p>
        <p>Saved automatically. Changes apply immediately, including during a batch.</p>
      </dialog>
      {toolMessage && tools?.ffmpeg && tools?.ffprobe && <div role="status" className="notice">{toolMessage}</div>}

      {tools && (!tools.ffmpeg || !tools.ffprobe) && (
        <div className="notice"><CircleAlert size={18} /><div><strong>FFmpeg and FFprobe are required</strong><span>{toolMessage || "Install them automatically with Homebrew."}</span></div><button type="button" onClick={installTools} disabled={installing}>{installing ? "Installing…" : "Install FFmpeg"}</button></div>
      )}

      <section className={`paths ${dragging ? "dragging" : ""}`}>
        <button className={`path-card ${config.sourceDir ? "selected" : ""}`} onClick={chooseSource} disabled={running || starting}>
          <FolderOpen size={20} /><span><small>Source folder</small><strong>{dragging ? "Drop folder here" : config.sourceDir ? basename(config.sourceDir) : "Choose or drop footage…"}</strong><em>{config.sourceDir || "MP4, MOV, MXF, M4V and Canon CRM"}</em></span>
        </button>
        <button className={`path-card ${config.outputDir ? "selected" : ""}`} onClick={chooseOutput} disabled={running || starting || !config.sourceDir}>
          <FolderOpen size={20} /><span><small>Output folder</small><strong>{config.outputDir ? basename(config.outputDir) : "Source/Proxy"}</strong><em>{config.outputDir || "Created automatically"}</em></span>
        </button>
      </section>

      {sourceChecking && <p className="source-detection" role="status">Checking source files…</p>}
      {usesRaw && <details className="raw-options"><summary><Film size={14} /> {sourceInfo?.rawClips} Canon RAW Light clips detected <span>Resolve Studio</span></summary><div className="raw-help"><p>Resolve Studio decodes RAW automatically. Keep Resolve open with external scripting set to Local. DeProxy uses one temporary project for the whole batch, then restores your project when finished.</p><label className="field raw-quality"><span>RAW decoding quality</span><div className="select-wrap"><select disabled={running || starting} value={config.rawIntermediateQuality} onChange={(e) => update("rawIntermediateQuality", e.target.value as BatchConfig["rawIntermediateQuality"])}><option value="standard">High · recommended</option><option value="hq">Highest · larger temporary files</option><option value="lt">Balanced · smaller temporary files</option><option value="proxy">Compact · lowest temporary quality</option></select><ChevronDown size={15} /></div></label><p>This controls Resolve’s temporary 10-bit ProRes file. High uses ProRes 422; Highest uses 422 HQ, Balanced uses 422 LT, and Compact uses ProRes Proxy. The temporary file is deleted after encoding.</p><label><input type="checkbox" checked={config.rawProjectAspect} disabled={running || starting} onChange={(e) => update("rawProjectAspect", e.target.checked)} /> Use clip aspect ratios from the open Resolve project</label><p>Unmatched clips use your selected squeeze. Corrections are baked into square pixels, up to the selected DCI size. RAW development uses Resolve’s defaults without film grades. Temporary files need extra space; the CPU limit applies to FFmpeg.</p></div></details>}
      <section className="panel">
        <div className="panel-title"><h2>Proxy settings</h2><span>{codecLabels[config.codec]}</span></div>
        {usesRaw && <div className="raw-pipeline"><strong>Canon RAW <span>→</span> {config.codec === "prores-proxy" ? "ProRes" : config.codec === "h264" ? "H.264" : "H.265"} proxy</strong><p>Resolve decodes the source. The encoder, output height and {config.codec === "prores-proxy" ? "ProRes quality" : "bitrate"} below control your finished proxy. RAW clips run one at a time.</p></div>}
        <div className="settings-grid">
          <Choice label={usesRaw ? "Fallback squeeze" : "Anamorphic squeeze"} values={squeezes} value={config.squeeze} suffix="×" onChange={(v) => update("squeeze", v)} />
          <Choice label="Output height" values={heights} value={config.outputHeight} suffix="p" onChange={selectOutputHeight} />
          {config.codec === "prores-proxy" ? <label className="field"><span>ProRes quality</span><div className="select-wrap"><select value={config.proresProfile} onChange={(e) => update("proresProfile", e.target.value as BatchConfig["proresProfile"])}><option value="proxy">Proxy</option><option value="lt">422 LT</option><option value="standard">422</option><option value="hq">422 HQ</option></select><ChevronDown size={15} /></div></label> : <NumericStepper label={usesRaw ? "Final bitrate" : "Bitrate"} value={config.bitrateMbps} min={1} max={100} unit="Mbps" onChange={(v) => update("bitrateMbps", v)} />}
          {usesRaw ? <label className="field"><span>Parallel jobs</span><div className="readonly-setting raw-locked" aria-label="Parallel jobs locked to one for RAW">1 job <small>Locked for RAW</small></div></label> : <NumericStepper label="Parallel jobs" value={config.parallelJobs} min={1} max={config.codec === "prores-proxy" ? (config.outputHeight >= 2160 ? 1 : 2) : 8} unit="jobs" onChange={(v) => update("parallelJobs", v)} />}
          <label className="field wide"><span>Filename suffix</span><input value={config.filenameSuffix} onChange={(e) => update("filenameSuffix", e.target.value)} placeholder="_proxy" /></label>
          <label className="field wide"><span>Proxy folder name</span><input value={proxyFolderName} onChange={(e) => renameProxyFolder(e.target.value)} placeholder="Proxy" /></label>
        </div>
        <details className="advanced">
          <summary>Advanced encoding <span>{config.codec === "hevc-main10" ? "Recommended" : "Custom"}</span></summary>
          <div className="advanced-content">
            <div className="advanced-fields">
              <label className="field"><span>Final encoder</span><div className="select-wrap"><select value={config.codec} onChange={(e) => selectCodec(e.target.value as BatchConfig["codec"])}><option value="hevc-main10">H.265 Main10 (VideoToolbox)</option><option value="hevc">H.265 8-bit (VideoToolbox)</option><option value="h264">H.264 8-bit (VideoToolbox)</option><option value="prores-proxy">Apple ProRes 422</option></select><ChevronDown size={15} /></div></label>
              <label className="field"><span>Audio</span><div className="select-wrap"><select value={config.audioMode} onChange={(e) => update("audioMode", e.target.value as BatchConfig["audioMode"])}><option value="aac">AAC · 160 kbps per track</option><option value="preserve">Preserve source audio</option><option value="none">No audio</option></select><ChevronDown size={15} /></div></label>
            </div>
            <p>{config.codec === "prores-proxy" ? `10-bit 4:2:2 editing codec. Proxy is smallest; LT, 422 and HQ progressively increase quality and size. ${config.outputHeight >= 2160 ? "4K ProRes runs one job at a time for stability." : "ProRes is limited to two parallel jobs for stability."}` : config.codec === "h264" ? "The broadest compatibility, hardware accelerated, but limited to 8-bit output." : config.codec === "hevc" ? "Smaller 8-bit files using Apple hardware acceleration." : "Recommended: compact 10-bit proxies encoded by Apple VideoToolbox."}</p>
          </div>
        </details>
        <div className="toggles">
          {([
            ["preserveTimecode", "Preserve source timecode"],
            ["skipExisting", "Skip existing outputs"]
          ] as const).map(([key, label]) => (
            <label key={key}><input type="checkbox" checked={config[key]} onChange={(e) => update(key, e.target.checked)} /><span className="switch" />{label}</label>
          ))}
        </div>
      </section>

      {(running || events.length > 0) && (
        <section className="panel progress-panel">
          <div className="panel-title"><h2>{running ? "Encoding proxies" : batchFinished ? "Batch complete" : "Batch summary"}</h2><strong>{overall}%</strong></div>
          <div className="progress-track"><i style={{ width: `${overall}%` }} /></div>
          {!running && batchFinished && <div className="completion"><div className="completion-icon"><Check size={22} /></div><div className="completion-stat time"><strong>Finished in {formatElapsed(elapsedSeconds ?? 0)}</strong><span>{statusEvent?.completedFiles ?? 0} {statusEvent?.completedFiles === 1 ? "proxy" : "proxies"} processed successfully</span></div><div className="completion-stat sizes"><strong>{formatBytes(statusEvent?.sourceBytes ?? 0)} <b>→</b> {formatBytes(statusEvent?.outputBytes ?? 0)}</strong><span>Source to proxy size</span></div><div className="completion-stat saved"><strong>{reductionPercent(statusEvent?.sourceBytes ?? 0, statusEvent?.outputBytes ?? 0).toFixed(1)}% smaller</strong><span>{formatBytes(Math.max(0, (statusEvent?.sourceBytes ?? 0) - (statusEvent?.outputBytes ?? 0)))} saved</span></div></div>}
          <div className="summary"><span>{statusEvent?.completedFiles ?? 0} complete</span><span>{statusEvent?.skippedFiles ?? 0} skipped</span><span>{statusEvent?.failedFiles ?? 0} failed</span><span>{statusEvent?.totalFiles ?? 0} total</span></div>
          <div className="file-list">
            {fileEvents.slice(-5).map((event) => (
              <div key={event.file}><span>{event.kind === "failed" ? <CircleAlert size={14} /> : event.kind === "completed" ? <Check size={14} /> : <Film size={14} />}{basename(event.file)}</span><em>{event.kind === "progress" ? `${event.message ? `${event.message} · ` : ""}${Math.round(event.fileProgress)}%` : event.kind}</em></div>
            ))}
          </div>
          <div className="log">{events.filter((e) => e.message && ["failed", "log", "batch-complete", "cancelled"].includes(e.kind)).slice(-4).map((e, i) => <div key={i}>{e.message}</div>)}</div>
        </section>
      )}

      <footer>
        <div className="footer-presets">
          <div className="preset-select select-wrap"><select aria-label="Saved presets" value={selectedPreset} onChange={(e) => applyPreset(e.target.value)}><option value="">Presets…</option>{presets.map((preset) => <option key={preset.name} value={preset.name}>{preset.name}</option>)}</select><ChevronDown size={15} /></div>
          {savingPreset ? <div className="preset-editor"><input autoFocus aria-label="Preset name" placeholder="Preset name" value={presetName} onChange={(e) => setPresetName(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") savePreset(); if (e.key === "Escape") setSavingPreset(false); }} /><button type="button" className="preset-save" disabled={!presetName.trim()} onClick={savePreset}>Save</button><button type="button" className="preset-cancel" onClick={() => setSavingPreset(false)}>Cancel</button></div> : <button type="button" className="save-preset" onClick={() => { setPresetName(selectedPreset); setSavingPreset(true); }} disabled={running}>Save preset</button>}
        </div>
        {!running && events.length > 0 && <button className="secondary" onClick={() => setEvents([])}><RotateCcw size={16} /> Reset</button>}
        {running ? <button className="danger" onClick={cancel}><OctagonX size={17} /> Cancel batch</button> : <button className="primary" disabled={!ready || starting || sourceChecking} onClick={() => void start()}><Play size={17} fill="currentColor" />{starting ? "Starting…" : "Generate proxies"}</button>}
      </footer>
    </main>
    <dialog ref={rawDialog} className="raw-review" aria-labelledby="raw-review-title" onCancel={() => setRawReviewOpen(false)}>
      <div className="raw-review-heading"><div><span className="eyebrow">Before you start</span><h2 id="raw-review-title">Review clip corrections</h2></div><button type="button" className="dialog-close" aria-label="Close review" onClick={() => setRawReviewOpen(false)}><X size={18} /></button></div><p>Aspect settings from {rawPlan?.project}. Check that spherical clips are 1× and anamorphic clips match the lens.</p>
      <div className="raw-clips">{rawPlan?.clips.map((clip) => <div key={clip.file}><span>{basename(clip.file)}</span><strong>{clip.squeeze}×</strong><small>{clip.fromProject ? "From Resolve" : "Fallback setting"}</small></div>)}</div>
      <p>Existing files are skipped when “Skip existing outputs” is enabled. Disable it to replace earlier proxies.</p>
      <div className="dialog-actions"><button type="button" className="secondary" onClick={() => setRawReviewOpen(false)}>Back</button><button type="button" className="primary" onClick={() => void start(true)}><Play size={15} fill="currentColor" /> Start batch</button></div>
    </dialog>
    </>
  );
}

export default App;
