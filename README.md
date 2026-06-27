# Speedy — Video Processing Tool

A fast command-line video processing tool built in Rust on top of the FFmpeg
CLI. Speedy handles speed changes, multi-clip stitching, LUT-based color
grading, log-profile conversion, and a full set of color-enhancement filters —
all driven by a single `speedy` binary.

## Project Structure

This project is a Rust workspace with two crates:

- **`speedy-core`** — the core library: an FFmpeg command builder, the video
  processing pipeline, color/log-profile handling, and the built-in presets.
- **`speedy-cli`** — the command-line interface (`speedy` binary) that parses
  arguments and drives `speedy-core`.

## Features

- **Speed adjustment** — speed up or slow down footage. The retimed stream is
  resampled back to a sane frame rate (the source fps by default, or `--output-fps`),
  so a speed-up drops frames into a shorter clip instead of inflating the frame
  rate — a 10× speed-up of 30 fps footage stays 30 fps rather than becoming
  ~300 fps with every source frame re-encoded. Audio is retimed with pitch
  correction (`atempo`), automatically chaining filters for speeds beyond the
  0.5×–2.0× range. Speed changes on video-only clips skip the audio path.
- **Multi-clip stitching** — pass several inputs (or a directory) to concatenate
  them into one output, in order. Clips of differing resolution or orientation
  are normalized to a common frame (scaled to fit and padded), so mixed 4K/6K
  and portrait/landscape footage can be combined. Any color grading is applied
  once over the joined timeline.
- **LUT color grading** — apply a `.cube` 3D LUT with `--lut`.
- **Log-profile support** — declare the source profile (`--profile`) for D-Log,
  S-Log, C-Log, V-Log, or F-Log footage. When a matching conversion LUT is
  present under `luts/`, it is applied automatically; if it's missing the
  conversion is skipped with a warning so other adjustments still run.
- **Color enhancement filters**:
  - Contrast and saturation
  - Vibrance (intelligent saturation that protects skin tones)
  - Dehaze (`--dehaze`) — removes atmospheric haze by pulling the black point,
    adding contrast, and restoring saturation/vibrance (a DaVinci-style "dehaze"
    approximated with `curves`/`eq`/`vibrance`)
  - Color curves (presets or custom curve definitions)
  - Color balance across shadows, midtones, and highlights
  - Selective color adjustments
  - Hue shifting
- **Enhancement & cleanup** — video stabilization (`deshake`), denoising
  (`nlmeans`), and sharpening (`unsharp`).
- **Encoding control** — codec (H.264, H.265/HEVC, VP9, AV1, ProRes), CRF
  quality, target bitrate, thread count, and output scaling.
- **Hardware acceleration** — optional, using the best method per platform
  (VAAPI on Linux, VideoToolbox on macOS, DXVA2 on Windows).
- **Auto-rotation** — honors rotation metadata by default; disable with
  `--no-auto-rotate`.
- **Smart presets** — ready-made settings for common cameras and platforms.
- **Progress reporting** — a live progress bar while FFmpeg runs.

> Note: stitched output is currently video-only — audio tracks from the input
> clips are not concatenated (a warning is logged when audio is present).

## Installation

### Prerequisites

- **Rust** 1.88 or later (the workspace uses the 2024 edition and let-chains)
- **FFmpeg** with `ffmpeg` and `ffprobe` on your `PATH`. Use a build that
  includes the encoders for the codecs you intend to use (x264, x265, libvpx,
  libaom, ProRes). FFmpeg 4.3+ covers all the filters used here; FFmpeg 7 is
  what the Nix dev shell ships.

Install FFmpeg:

```bash
# Ubuntu/Debian
sudo apt install ffmpeg
# macOS
brew install ffmpeg
# Windows: https://ffmpeg.org/download.html
```

### Building from Source

```bash
git clone https://github.com/douglaz/speedy.git
cd speedy
cargo build --release
```

The binary is produced at `target/release/speedy`.

### Building with Nix

The repository ships a Nix flake that builds a statically linked (musl) binary
and provides a development shell with FFmpeg and tooling preinstalled:

```bash
# Build the static binary (result/bin/speedy)
nix build

# Run it directly
nix run . -- -i input.mp4 -o output.mp4 --speed 2.0

# Enter the dev shell (FFmpeg, Rust toolchain, git hooks, etc.)
nix develop
```

When using Nix for development, prefix cargo commands with `nix develop -c` so
the FFmpeg environment is available, e.g. `nix develop -c cargo test`.

## Usage

### Basic Usage

```bash
# Speed up a video 2x
speedy -i input.mp4 -o output.mp4 --speed 2.0

# Apply a LUT file
speedy -i input.mp4 -o output.mp4 --lut color_grade.cube

# Use a preset for DJI Mavic 4 Pro D-Log footage
speedy -i drone_footage.mp4 -o processed.mp4 --preset mavic4pro-dlog

# Treat the source as S-Log footage (applies the S-Log LUT if available)
speedy -i clip.mov -o graded.mp4 --profile s-log
```

### Stitching Multiple Clips

Pass several inputs (or a directory) to stitch them into a single output, in
order. A directory is expanded to its video files (`.mp4`, `.mov`, `.m4v`,
`.mkv`, `.avi`, `.webm`) sorted by filename. Clips of different resolution or
orientation are normalized to a common frame.

```bash
# Stitch specific clips, in the given order
speedy -i clip1.mp4 clip2.mp4 clip3.mp4 -o combined.mp4

# Stitch every video in a folder (sorted by filename) and grade from D-Log
speedy -i /path/to/DCIM/DJI_001 --preset mavic4pro-dlog -o combined.mp4

# Stitch a folder of DJI D-Log clips into a 10× hyperlapse. The speed-up
# decimates frames back to the source fps, so the output is a short,
# normal-frame-rate clip (not a ~300 fps file). `--profile d-log` auto-applies
# the bundled D-Log LUT when one is present under luts/ (and is skipped with a
# warning otherwise); or grade with your own via `--lut /path/to/your.cube`.
speedy -i /path/to/DCIM/DJI_001 \
  --profile d-log --speed 10 --codec h265 -o combined_10x.mp4
```

### Advanced Color Grading

```bash
# Vibrance plus a lighter curve
speedy -i input.mp4 -o output.mp4 --vibrance 0.5 --curves "preset=lighter"

# Remove atmospheric haze from flat/weather-affected footage (0.5 = medium)
speedy -i hazy.mp4 -o clear.mp4 --dehaze 0.5

# Cinematic teal and orange look
speedy -i input.mp4 -o output.mp4 --preset cinematic

# Custom color balance (shadows,midtones,highlights as r:g:b, each -1..1)
speedy -i input.mp4 -o output.mp4 --color-balance "0.1:-0.1:0,0:0:0,-0.1:0:0.1"

# Hue shift and selective color
speedy -i input.mp4 -o output.mp4 --hue-shift 10 \
  --selective-color "reds=0.1:0:-0.1:0,blues=-0.1:0:0.1:0"
```

### Enhancement, Scaling, and Encoding

```bash
# Stabilize, denoise, and sharpen
speedy -i shaky.mp4 -o clean.mp4 --stabilize --denoise 4 --sharpen 0.6

# Downscale to 1080p (keep aspect ratio with -1 height)
speedy -i input.mp4 -o output.mp4 --scale "1920:-1"

# Encode H.265 at a higher quality (lower CRF) with hardware acceleration
speedy -i input.mp4 -o output.mp4 --codec h265 --quality 18 --hw-accel
```

### Options Reference

| Option | Description | Default |
| --- | --- | --- |
| `-i, --input <PATH>...` | Input file(s) or a directory (multiple = stitch) | — |
| `-o, --output <PATH>` | Output video file | — |
| `--preset <NAME>` | Apply a preset (see below) | — |
| `-s, --speed <X>` | Speed multiplier (e.g. `2.0`) | `1.0` |
| `--output-fps <FPS>` | Output frame rate for speed changes (e.g. `30`, `30000/1001`) | source fps |
| `-l, --lut <FILE>` | `.cube` LUT for color grading | — |
| `-p, --profile <PROFILE>` | Source profile: `standard`, `d-log`, `s-log`, `c-log`, `v-log`, `f-log` | `standard` |
| `-c, --contrast <V>` | Contrast (0.0–2.0) | `1.0` |
| `-S, --saturation <V>` | Saturation (0.0–2.0) | `1.0` |
| `--codec <CODEC>` | `h264`, `h265`/`hevc`, `vp9`, `av1`, `prores` | `h264` |
| `-b, --bitrate <MBPS>` | Target video bitrate in Mbps | — |
| `-q, --quality <CRF>` | CRF quality (0–51, lower is better) | `23` |
| `--hw-accel` | Enable hardware acceleration if available | off |
| `-t, --threads <N>` | Number of encoding threads | auto |
| `--stabilize` | Enable video stabilization | off |
| `--no-auto-rotate` | Disable auto-rotation from metadata | off |
| `--denoise <1-10>` | Denoising strength | — |
| `--sharpen <0.1-2.0>` | Sharpening strength | — |
| `--vibrance <-2.0..2.0>` | Vibrance (protects skin tones) | — |
| `--dehaze <STRENGTH>` | Remove atmospheric haze (~`0.5` medium, `1.0` strong) | — |
| `--curves <SPEC>` | Color curves, e.g. `preset=lighter` | — |
| `--hue-shift <-180..180>` | Hue shift in degrees | — |
| `--color-balance <SPEC>` | `shadows,midtones,highlights` as `r:g:b` | — |
| `--selective-color <SPEC>` | Per-color-range adjustments | — |
| `--scale <SPEC>` | Resolution, e.g. `1920x1080` or `1920:-1` | — |
| `--list-presets` | List available presets and exit | — |
| `-v, --verbose` | Verbose (debug) logging | off |

When a preset is used, explicitly passed flags override the preset's values,
while flags left at their defaults do not clobber what the preset sets.

Run `speedy --help` for the authoritative, always-current list.

### Available Presets

List them at any time with `speedy --list-presets`.

| Preset | Aliases | Description |
| --- | --- | --- |
| `mavic4pro-dlog` | `mavic4pro_dlog`, `mavic-4-pro-dlog` | DJI Mavic 4 Pro footage with D-Log profile |
| `dji` | `dji-standard` | DJI drone footage, standard profile |
| `gopro` | | GoPro action camera footage |
| `sony-slog` | `slog` | Sony footage with S-Log profile |
| `canon-clog` | `clog` | Canon footage with C-Log profile |
| `instagram` | `ig` | Optimized for Instagram |
| `youtube` | `yt` | Optimized for YouTube |
| `tiktok` | `tt` | Optimized for TikTok |
| `cinema4k` | `cinema`, `4k` | Cinema 4K export (ProRes, maximum quality) |
| `preview` | `fast` | Fast preview (lower quality, faster) |
| `archive` | `archival` | High-quality archival (H.265, low CRF) |
| `natural` | `natural-enhance` | Natural color enhancement using vibrance |
| `cinematic` | `teal-orange` | Cinematic teal and orange look |
| `portrait` | | Portrait mode with skin-tone protection |

## Development

This project uses Git hooks for code quality. When you enter the Nix dev shell
the hooks are configured automatically:

```bash
$ nix develop
📎 Setting up Git hooks for code quality checks...
✅ Git hooks configured automatically!
   • pre-commit: Checks code formatting
   • pre-push: Runs formatting and clippy checks
```

### Git Hooks

1. **pre-commit** — ensures code is formatted (`cargo fmt --check`).
2. **pre-push** — runs formatting and `cargo clippy --workspace -- -D warnings`.

To configure them manually:

```bash
git config core.hooksPath .githooks
```

To disable them temporarily:

```bash
git config --unset core.hooksPath
```

### Running Checks Manually

```bash
# Tests
nix develop -c cargo test

# Formatting
nix develop -c cargo fmt --check   # check
nix develop -c cargo fmt           # fix

# Clippy
nix develop -c cargo clippy --workspace -- -D warnings

# Everything
nix develop -c cargo fmt --check && nix develop -c cargo clippy --workspace -- -D warnings
```

### Workspace Layout

```
speedy/
├── Cargo.toml            # Workspace configuration
├── flake.nix             # Nix flake (static build + dev shell)
├── speedy-core/          # Core library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            # Public API, ColorProfile
│       ├── ffmpeg_wrapper.rs # FFmpeg command builder + ffprobe
│       ├── video_processor.rs# Processing pipeline / stitching
│       └── presets.rs        # Built-in presets
└── speedy-cli/           # CLI application (`speedy` binary)
    ├── Cargo.toml
    └── src/
        └── main.rs
```

### Using speedy-core as a Library

Add to your `Cargo.toml`:

```toml
[dependencies]
speedy-core = { git = "https://github.com/douglaz/speedy.git" }
```

Example usage:

```rust
use speedy_core::{ColorProfile, VideoProcessor};

fn main() -> anyhow::Result<()> {
    let processor = VideoProcessor::new("input.mp4", "output.mp4")
        .speed(2.0)
        .profile(ColorProfile::DLog)
        .vibrance(0.5)
        .quality(20);

    processor.process()?;
    Ok(())
}
```

To stitch multiple clips, build the processor with `VideoProcessor::new_multi`:

```rust
use std::path::PathBuf;
use speedy_core::VideoProcessor;

let clips = vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")];
VideoProcessor::new_multi(clips, "combined.mp4").process()?;
```

## License

Licensed under either of MIT or Apache-2.0, at your option (see the `license`
field in `Cargo.toml`).

## Contributing

Contributions are welcome — please feel free to open an issue or a pull request.
