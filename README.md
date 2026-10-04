# Speedy — Video Processing Tool

A fast command-line video processing tool built in Rust on top of the FFmpeg
CLI. Speedy handles speed changes, multi-clip stitching, LUT-based color
grading, log-profile conversion, and a full set of color-enhancement filters —
all driven by a single `speedy` binary.

## Project Structure

This project is a Rust workspace with two crates:

- **`speedy-core`** — the core library: an FFmpeg command builder, the video
  processing pipeline, the color pipeline (Rec.709 and HDR), and the built-in
  presets.
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
- **HDR output for DJI D-Log** — D-Log/D-Gamut footage is delivered as
  Rec.2100 HLG (10-bit HEVC Main 10, ready for a YouTube HDR upload) through a
  scene-referred ACES pipeline: OpenColorIO converts D-Log/D-Gamut to ACES, the
  grade runs in ACEScg/ACEScct on 32-bit float RGB, and the ACES 2.0 output
  transform renders HLG at a 1000-nit reference. No Rec.709 LUT is involved.
  This is the default for D-Log input; see [HDR Output](#hdr-output-d-log--hlg).
- **LUT color grading** — apply a `.cube` 3D LUT with `--lut`.
- **Log-profile support** — declare the source with `--input-color` (or its
  older spelling `--profile`) for D-Log, S-Log, C-Log, V-Log, or F-Log footage.
  On the Rec.709 route, when a matching conversion LUT is present under
  `luts/` it is applied automatically; if it's missing the conversion is
  skipped with a warning so other adjustments still run.
- **Color enhancement filters** (Rec.709 output; HDR output takes only
  contrast, saturation, `--exposure` and an ACEScct LUT):
  - Contrast and saturation
  - Vibrance (intelligent saturation that protects skin tones)
  - Dehaze (`--dehaze`) — removes atmospheric haze by pulling the black point,
    adding contrast, and restoring saturation/vibrance (a DaVinci-style "dehaze"
    approximated with `curves`/`eq`/`vibrance`)
  - Color curves (presets or custom curve definitions)
  - Color balance across shadows, midtones, and highlights
  - Selective color adjustments
  - Hue shifting
- **Stabilization** (`--stabilize`) — two-pass `vidstab` (detect + transform),
  not the weaker single-pass `deshake`. Two refinements for real-world footage:
  - **Per-segment when stitching** — each clip is stabilized independently
    before concatenation, so smoothing never crosses a cut (no artificial pan at
    clip boundaries).
  - **Brightness-normalized detection** — motion is detected on a normalized
    copy, so a sudden exposure (EV) change isn't misread as camera motion (which
    would otherwise inject a shake the moment the exposure shifts).
  - Tune the glide with `--stabilize-smoothing <frames>`. Stabilized output is
    video-only, and 8-bit: the vidstab filters have no 10-bit mode, so ffmpeg
    converts the image down around them. Use `--no-stabilize` to switch off a
    preset's stabilization (`mavic4pro-dlog` and `dji` enable it) and keep an
    H.265 encode 10-bit end to end. Stabilization is refused with HDR output
    for the same reason.
- **Enhancement & cleanup** — denoising (`nlmeans`) and sharpening (`unsharp`).
- **Encoding control** — codec (H.264, H.265/HEVC, VP9, AV1, ProRes), CRF
  quality, target bitrate, thread count, and output scaling. `libx265` encodes
  10-bit (Main 10) and ProRes 10-bit 4:2:2, so a log source graded through a LUT
  keeps its gradients instead of banding in dark skies; the other codecs are
  8-bit.
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
  libaom, ProRes). FFmpeg 4.3+ covers all the filters used for Rec.709
  output.
- **For HDR output: FFmpeg with OpenColorIO.** The HDR route needs the `ocio`
  filter (FFmpeg 8+ configured with `--enable-libopencolorio`, OpenColorIO
  2.5+) and `zscale` (libzimg). Distribution builds normally lack `ocio`; the
  Nix dev shell ships FFmpeg 8 built with it, so run HDR jobs inside
  `nix develop`. speedy checks for the filter and stops with an explanation
  when it is missing.

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

# Enter the dev shell (FFmpeg with OpenColorIO, Rust toolchain, git hooks, etc.)
nix develop
```

The static binary calls whatever `ffmpeg` is on `PATH`, so HDR output from it
still needs the dev shell's FFmpeg: `nix develop -c ./result/bin/speedy ...`.

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
speedy -i clip.mov -o graded.mp4 --input-color s-log

# DJI D-Log footage to an HDR (Rec.2100 HLG) file for YouTube
speedy -i DJI_0001.MP4 -o hdr.mp4 --input-color dji-dlog
```

### HDR Output (D-Log → HLG)

When the input is DJI D-Log (`--input-color dji-dlog`, or `--profile d-log`),
no preset is used and `--output-color` is not given, the output is Rec.2100
HLG. The picture goes through ACES rather than a Rec.709 LUT:

```
D-Log/D-Gamut YUV → RGB float → ACES (ACEScg / ACEScct) → grade
  → ACES 2.0 output transform (HLG, 1000 nits) → BT.2020 10-bit HEVC Main 10
```

HDR needs the dev shell's FFmpeg + OpenColorIO build, and the dev shell does not
put `speedy` on `PATH`: build the binary once, then run it through the shell.

```bash
# Build the binary (result/bin/speedy)
nix build

# D-Log clip to HLG (libx265 Main 10, CRF 18, audio kept)
nix develop -c ./result/bin/speedy -i DJI_0001.MP4 -o hdr.mp4 --profile d-log

# Stitch a folder into a 10× HDR hyperlapse, downscaled to 4K
nix develop -c ./result/bin/speedy -i /path/to/DCIM/DJI_001 -o hyperlapse_hdr.mp4 \
  --input-color dji-dlog --speed 10 --scale 3840:-2

# Grade in ACES: 1 stop (0.3 over the default), a little contrast and saturation
nix develop -c ./result/bin/speedy -i DJI_0001.MP4 -o hdr.mp4 --input-color dji-dlog \
  --exposure 1.0 --contrast 1.1 --saturation 1.1

# A creative LUT that works on ACEScct values (in and out)
nix develop -c ./result/bin/speedy -i DJI_0001.MP4 -o hdr.mp4 --input-color dji-dlog \
  --lut look_acescct.cube --lut-space acescct

# The previous behaviour: Rec.709 through the D-Log LUT
nix develop -c ./result/bin/speedy -i DJI_0001.MP4 -o sdr.mp4 --profile d-log --output-color rec709
```

What HDR output does and allows:

- **Encode** — `libx265` Main 10, `yuv420p10le`, CRF 18 unless `--quality` is
  given. The stream is tagged BT.2020 / ARIB STD-B67 (HLG) / BT.2020
  non-constant-luminance / limited range in both the container and the HEVC
  bitstream, whatever the source was tagged. `.mp4` is the tested container.
  `--codec` may be `h265`, `hevc` or `libx265`; any other codec, including the
  hardware HEVC encoders, is an error.
- **Grade** — `--exposure <STOPS>` (−3 to 3, a linear gain in ACEScg; default
  0.7, which lifts 18% grey from the ~30% HLG signal the ACES 2.0 rendering
  gives it to the 38% HLG reference level; a value you pass replaces it),
  `--contrast` (0.3–2.0, around 18% grey in ACEScct), `--saturation` (0.0–2.0,
  in ACEScct), and `--lut` with `--lut-space acescct`.
- **Still available** — speed, `--output-fps`, stitching, `--scale` (applied
  before the color work), `--no-auto-rotate`, `--hw-accel` (decoding),
  `--bitrate`, `--threads`.
- **Refused, with an error** — stabilization (`vidstab` is 8-bit only; pass
  `--no-stabilize`), `--dehaze`, presets, `--curves`, `--vibrance`,
  `--selective-color`, `--hue-shift`, `--color-balance`, `--denoise`,
  `--sharpen`, a `--lut` without `--lut-space acescct`, and any input other
  than DJI D-Log. These were built for a Rec.709 image; use
  `--output-color rec709` to keep them.
- **Source tags** — the source's YUV matrix and range tags drive the
  conversion to RGB. Untagged sources are read as BT.709, limited range (logged
  as a warning).
  When stitching, every clip is read with that matrix and range and joined at
  10 bits, so a stitch may mix tagged and untagged or 8-bit and 10-bit clips;
  clips whose matrix or range differ are refused.

The OpenColorIO config is the ACES 2.0 studio config built into OpenColorIO
2.5 (`ocio://studio-config-v4.0.0_aces-v2.0_ocio-v2.5`), pinned in the code;
the `OCIO` environment variable is ignored.

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

# Stitch a folder of DJI D-Log clips into a 10× Rec.709 hyperlapse. The
# speed-up decimates frames back to the source fps, so the output is a short,
# normal-frame-rate clip (not a ~300 fps file). With `--output-color rec709`,
# `--profile d-log` auto-applies the bundled D-Log LUT when one is present
# under luts/ (and is skipped with a warning otherwise); or grade with your own
# via `--lut /path/to/your.cube`. Without `--output-color rec709` this would be
# an HDR job (see "HDR Output").
speedy -i /path/to/DCIM/DJI_001 \
  --profile d-log --output-color rec709 --speed 10 --codec h265 -o combined_10x.mp4

# The full Rec.709 drone pipeline: stitch + D-Log LUT + dehaze + 10× +
# per-segment stabilization, in one command. Each clip is graded and stabilized
# on its own before joining, so the stabilizer never invents a pan across a
# cut. Dehaze and stabilization are Rec.709-only, hence `--output-color rec709`.
speedy -i /path/to/DCIM/DJI_001 \
  --profile d-log --output-color rec709 --speed 10 --dehaze 0.2 --stabilize \
  -o combined_10x.mp4
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
| `-l, --lut <FILE>` | `.cube` LUT for color grading (HDR: needs `--lut-space acescct`) | — |
| `--lut-space <SPACE>` | Color space the LUT works in: `acescct` (HDR output only) | — |
| `--input-color <COLOR>` | Source encoding: `standard`, `dji-dlog`, `s-log`, `c-log`, `v-log`, `f-log` | `standard` |
| `-p, --profile <PROFILE>` | Older spelling of `--input-color` (`d-log` = `dji-dlog`); the two cannot be combined | `standard` |
| `--output-color <COLOR>` | `rec709` (SDR) or `hlg` (HDR, Rec.2100 HLG) | `hlg` for D-Log input without a preset, else `rec709` |
| `--exposure <STOPS>` | Exposure in stops (−3 to 3), in ACEScg (HDR output only) | `0.7` (HDR) |
| `-c, --contrast <V>` | Contrast (0.0–2.0; HDR: 0.3–2.0, in ACEScct) | `1.0` |
| `-S, --saturation <V>` | Saturation (0.0–2.0; HDR: in ACEScct) | `1.0` |
| `--codec <CODEC>` | `h264`, `h265`/`hevc`, `vp9`, `av1`, `prores` (HDR: `h265` only) | `h264` (HDR: `h265`) |
| `-b, --bitrate <MBPS>` | Target video bitrate in Mbps | — |
| `-q, --quality <CRF>` | CRF quality (0–51, lower is better) | `23` (HDR: `18`) |
| `--hw-accel` | Enable hardware-accelerated decoding if available | off |
| `-t, --threads <N>` | Number of encoding threads | auto |
| `--stabilize` | Two-pass vidstab stabilization (per-segment when stitching) | off |
| `--no-stabilize` | Turn stabilization off, including a preset's | off |
| `--stabilize-smoothing <FRAMES>` | Stabilization smoothing window (higher = glassier) | `20` |
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

Presets are Rec.709 grades: with a preset (including `mavic4pro-dlog`) the
output stays Rec.709, and combining one with `--output-color hlg` is an error.
The flags from `--stabilize` down to `--selective-color` in the table (except
`--no-stabilize` and `--no-auto-rotate`) are likewise Rec.709-only.

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
│       ├── lib.rs            # Public API
│       ├── color.rs          # Color pipeline types, ACES/HDR filter chain
│       ├── ffmpeg_wrapper.rs # FFmpeg command builder + ffprobe
│       ├── video_processor.rs# Processing pipeline / stitching
│       ├── stabilize.rs      # Two-pass vidstab stabilization
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
use speedy_core::{InputColor, VideoProcessor};

fn main() -> anyhow::Result<()> {
    let processor = VideoProcessor::new("input.mp4", "output.mp4")
        .speed(2.0)
        .input_color(InputColor::DjiDLogDGamut)
        .vibrance(0.5)
        .quality(20);

    processor.process()?;
    Ok(())
}
```

The library's output color defaults to Rec.709 whatever the input (the HLG
default for D-Log is a CLI choice). Ask for HDR explicitly:

```rust
use speedy_core::{InputColor, OutputColor, VideoProcessor};

VideoProcessor::new("dlog.mp4", "hdr.mp4")
    .input_color(InputColor::DjiDLogDGamut)
    .output_color(OutputColor::Rec2100Hlg)
    .exposure(0.5)
    .process()?;
```

`ColorPipeline { input, working, output }` describes the three stages
(`WorkingColor` is `AcesCct` or `AcesCg`); `VideoProcessor::validate` reports
an unsupported combination without running anything.

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
