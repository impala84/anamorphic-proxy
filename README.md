# DeProxy

A small macOS desktop utility for making lightweight, Resolve-linkable, de-squeezed editing proxies with Apple silicon's hardware HEVC encoder. It has a React interface, a minimal Tauri/Rust backend, and runs FFmpeg entirely on your Mac—no uploads, accounts, or cloud processing.

## What v1 does

- Picks source and output folders (default: `Source/Proxy`)
- Accepts a source folder—or any video inside it—by drag and drop
- Supports 1.33×, 1.5×, 1.6×, 1.8×, 2×, or a custom anamorphic squeeze
- Outputs at 720, 1080, or a custom height
- Derives width as `source raster aspect ratio × squeeze × target height`, rounded to an even pixel count
- Encodes H.265 Main10 through Apple VideoToolbox with an `hvc1` tag
- Uses 6 Mbps video and AAC 160 kbps audio by default
- Preserves source timecode when FFprobe exposes it
- Runs two encodes in parallel by default (configurable from 1–8)
- Skips existing proxies, writes hidden temporary files, and only renames after success
- Cancels active work and removes partial outputs
- Shows per-clip progress, overall progress, concise diagnostics, and a batch summary
- Handles Canon-style MP4 edit lists using `-ignore_editlist 1`
- Trims negative-timestamp edit-list preroll so proxies open on the first real picture rather than a generated black frame
- Offers one-click FFmpeg installation through Homebrew when the tools are missing

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
3. Keep the automatically selected `Proxy` output folder or choose another destination.
4. For 1.5× C50 footage, choose **1.5×**, **1080p**, **6 Mbps**, and **2 jobs**.
5. Leave timecode, AAC audio, and skip-existing enabled, then select **Generate proxies**.
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
- HEVC Main10 VideoToolbox is fixed; ProRes and software fallbacks are future work.
- The bitrate control uses average bitrate targeting. VideoToolbox output can vary slightly by source.
- Timecode is preserved when present in the selected video stream or format metadata. Unusual cameras that expose timecode only through a separate proprietary/data stream may need a camera-specific fallback in a later release.
- Color-space interpretation is inherited through source metadata; v1 does not provide LUT or color-management controls.
- Sleep prevention, pause/resume, recursive input folders, and signed/notarized release automation are not included yet.

## Safety and privacy

All media stays local. A failed or cancelled encode removes its temporary file; a completed destination is never exposed until FFmpeg exits successfully. Existing files are left untouched when **Skip existing outputs** is enabled.

## License

[MIT](LICENSE)
