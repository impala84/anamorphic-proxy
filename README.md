# DeProxy

A small macOS desktop utility for making lightweight, Resolve-linkable, de-squeezed editing proxies with Apple silicon's hardware HEVC encoder. It has a React interface, a minimal Tauri/Rust backend, and runs FFmpeg entirely on your Mac—no uploads, accounts, or cloud processing.

## Why DeProxy exists

High-resolution anamorphic camera originals are beautiful, but they are unnecessarily heavy for editing. Generic proxy tools often preserve the squeezed raster, lose or shift timecode, expose a wall of FFmpeg settings, or create a black first frame from camera edit-list preroll. That makes an otherwise routine proxy job surprisingly fragile—especially when the results must relink cleanly in DaVinci Resolve.

DeProxy turns the known-good FFmpeg workflow into one focused utility. Pick the footage, choose its squeeze and editing resolution, and let the Mac's local hardware do the work. The resulting files are de-squeezed, square-pixel, compact, correctly tagged for QuickTime, and designed to retain the source naming and timecode needed for reliable relinking. Nothing is uploaded, and advanced codec choices remain available without overwhelming the default workflow.

## What's new in 0.3.2

- Adds an optional **Preserve source audio** mode that retains every source stream's codec, channel count, track order, sample rate, bit depth, and timing.
- Avoids AAC encoder priming and skip-sample metadata that can make multi-track proxy audio conform differently inside an NLE.
- Adds explicit **Preserve source audio**, **AAC**, and **No audio** choices under Advanced encoding.
- Migrates existing saved presets: the old AAC toggle becomes AAC or No audio, while new presets use the selected audio mode.

## What v1 does

- Picks source and output folders (default: `Source/Proxy`) and lets you rename the proxy folder
- Accepts a source folder—or any video inside it—by drag and drop
- Supports no squeeze (1:1), 1.33×, 1.5×, 1.6×, 1.8×, 2×, or a custom anamorphic squeeze
- Outputs at 720p, 1080p (default), 2160p, or a custom height
- Derives width as `source raster aspect ratio × squeeze × target height`, rounded to an even pixel count
- Defaults to H.265 Main10 through Apple VideoToolbox with an `hvc1` tag
- Offers H.265 8-bit, H.264 8-bit, and 10-bit 4:2:2 ProRes Proxy/LT/422/HQ in a tucked-away advanced panel
- Uses 6 Mbps HEVC at 1080p by default and scales recommendations with pixel count (3 Mbps at 720p, 24 Mbps at 2160p); H.264 uses 4/8/32 Mbps
- Preserves source timecode when FFprobe exposes it
- Encodes every source audio track as AAC by default; exact source-audio preservation and video-only output remain optional
- Runs two encodes in parallel by default (configurable from 1–8)
- Limits ProRes to two parallel jobs, and 2160p ProRes to one, to avoid excessive memory pressure
- Skips existing proxies, writes hidden temporary files, and only renames after success
- Cancels active work and removes partial outputs
- Shows per-clip progress, monotonic overall progress, concise diagnostics, and a persistent completion summary with elapsed time and source-to-proxy size reduction
- Handles Canon-style MP4 edit lists using `-ignore_editlist 1`
- Trims negative-timestamp edit-list preroll so proxies open on the first real picture rather than a generated black frame
- Offers one-click FFmpeg installation through Homebrew when the tools are missing
- Saves and instantly reapplies named local presets without storing source or output paths

The established Canon C50-style example is covered by frontend and Rust tests:

```text
6960 × 4640 source, 1.5× squeeze, 1080 target height
(6960 / 4640) × 1.5 × 1080 = 2430
Output: 2430 × 1080, square-pixel HEVC Main10, 6 Mbps by default, hvc1
```

## Prerequisites

- macOS 12 or newer on Apple silicon
- [Homebrew](https://brew.sh/) (recommended)
- FFmpeg and FFprobe: `brew install ffmpeg`
- Node.js 20 or newer
- Rust stable and Apple's Command Line Tools

Install the development prerequisites:

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
brew install ffmpeg
```

The packaged app does not bundle FFmpeg in v1. At launch it checks the shell path plus the standard Apple silicon and Intel Homebrew locations (`/opt/homebrew/bin` and `/usr/local/bin`) and reports clearly if either tool is missing.

## Install and run

```sh
git clone https://github.com/impala84/anamorphic-proxy.git
cd anamorphic-proxy
npm install
npm run tauri dev
```

Run tests and the web-interface production build:

```sh
npm test
npm run build
cd src-tauri && cargo test
```

## Build the macOS app

```sh
npm run tauri build
```

Tauri writes the `.app` and `.dmg` to `src-tauri/target/release/bundle/`. Local unsigned builds may require Control-click → Open the first time. Public distribution requires an Apple Developer certificate and notarization, which are intentionally outside v1.

## Example workflow

1. Open DeProxy and confirm the green **FFmpeg ready** indicator.
2. Choose a folder containing the camera originals.
3. Keep the automatically selected `Proxy` output folder, rename it, or choose another destination.
4. For 1.5× C50 footage, choose **1.5×**, **1080p**, **6 Mbps**, and **2 jobs**.
5. Leave timecode, AAC audio, and skip-existing enabled for compact proxies. Choose **Preserve source audio** under Advanced encoding when exact production-audio matching is important.
6. Import or relink the resulting `_proxy.mp4` clips in Resolve using their matching names and timecode.

## Architecture

```text
React utility UI
  └─ Tauri commands and progress events
      ├─ FFprobe: raster, duration, and timecode metadata
      └─ bounded Rust worker pool
          └─ FFmpeg → hidden .part.mp4 → atomic rename on success
```

The backend passes paths as process arguments rather than building a shell command, so spaces and special characters in filenames are safe. Successful encodes stay quiet; non-zero exits and actionable FFmpeg diagnostics remain visible as failures.

## Limitations

- v1 scans only the selected folder, not nested folders, and accepts MP4, MOV, MXF, and M4V files.
- H.265 Main10 remains the recommended default. H.265 8-bit and H.264 use VideoToolbox; the ProRes profiles use FFmpeg's CPU-based `prores_ks` encoder and create larger `.mov` files.
- VideoToolbox failures are reported per clip; DeProxy does not silently switch to a slower software encoder.
- The bitrate control uses average bitrate targeting. VideoToolbox output can vary slightly by source.
- Preserved audio uses more space than AAC. Four mono 48 kHz/24-bit PCM tracks add roughly 35 MB per minute, but match the camera originals without encoder delay.
- Timecode is preserved when present in the selected video stream or format metadata. Unusual cameras that expose timecode only through a separate proprietary/data stream may need a camera-specific fallback in a later release.
- Color-space interpretation is inherited through source metadata; v1 does not provide LUT or color-management controls.
- Sleep prevention, pause/resume, recursive input folders, and signed/notarized release automation are not included yet.

## Safety and privacy

All media stays local. A failed or cancelled encode removes its temporary file; a completed destination is never exposed until FFmpeg exits successfully. Existing files are left untouched when **Skip existing outputs** is enabled.

## License

[MIT](LICENSE)
