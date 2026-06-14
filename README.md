# Speedy - Video Processing Tool

A powerful command-line video processing tool built with Rust and FFmpeg, designed for speed adjustment, color grading, and advanced video enhancement.

## Project Structure

This project is organized as a Rust workspace with two main crates:

- **`speedy-core`**: The core library containing all video processing logic, FFmpeg integration, and color manipulation algorithms
- **`speedy-cli`**: The command-line interface that provides user-friendly access to the core functionality

## Features

- **Speed Adjustment**: Speed up or slow down videos with automatic audio pitch correction
- **Clip Stitching**: Combine multiple clips (or a whole folder) into one output, normalizing differing resolutions/orientations to a common frame
- **Color Grading**: Apply LUTs (Look-Up Tables) for professional color grading
- **Advanced Color Enhancement**:
  - Vibrance (intelligent saturation that protects skin tones)
  - Color curves
  - Color balance (shadows, midtones, highlights)
  - Selective color adjustments
  - Hue shifting
- **Log Profile Support**: Built-in support for D-Log, S-Log, C-Log, V-Log, and F-Log profiles
- **Smart Presets**: Pre-configured settings for common workflows (DJI drones, GoPro, social media, etc.)
- **Hardware Acceleration**: Optional hardware acceleration support
- **Video Stabilization**: Built-in video stabilization
- **Denoising and Sharpening**: Advanced video enhancement filters

## Installation

### Prerequisites

- Rust 1.75 or later
- FFmpeg 4.0 or later

### Building from Source

```bash
git clone https://github.com/yourusername/speedy.git
cd speedy
cargo build --release
```

The binary will be available at `target/release/speedy`.

## Usage

### Basic Usage

```bash
# Speed up a video 2x
speedy -i input.mp4 -o output.mp4 --speed 2.0

# Apply a LUT file
speedy -i input.mp4 -o output.mp4 --lut color_grade.cube

# Use a preset for DJI Mavic 4 Pro D-Log footage
speedy -i drone_footage.mp4 -o processed.mp4 --preset mavic4pro-dlog
```

### Stitching Multiple Clips

Pass several inputs (or a directory) to stitch them into a single output, in
order. Clips of different resolution or orientation are normalized to a common
frame (scaled to fit and padded), so mixed 4K/6K and portrait/landscape footage
can be combined. Any color grading is applied once over the joined timeline.

> Note: stitched output is currently video-only — audio tracks are not
> concatenated.

```bash
# Stitch specific clips, in the given order
speedy -i clip1.mp4 clip2.mp4 clip3.mp4 -o combined.mp4

# Stitch every video in a folder (sorted by filename) and grade from D-Log
speedy -i /path/to/DCIM/DJI_001 --preset mavic4pro-dlog -o combined.mp4
```

### Advanced Color Grading

```bash
# Apply vibrance and curves
speedy -i input.mp4 -o output.mp4 --vibrance 0.5 --curves "preset=lighter"

# Cinematic teal and orange look
speedy -i input.mp4 -o output.mp4 --preset cinematic

# Custom color balance
speedy -i input.mp4 -o output.mp4 --color-balance "0.1:-0.1:0,0:0:0,-0.1:0:0.1"
```

### Available Presets

- `mavic4pro-dlog`: DJI Mavic 4 Pro footage with D-Log profile
- `dji`: DJI drone standard footage
- `gopro`: GoPro action camera
- `sony-slog`: Sony S-Log footage
- `canon-clog`: Canon C-Log footage
- `instagram`: Optimized for Instagram
- `youtube`: Optimized for YouTube
- `tiktok`: Optimized for TikTok
- `cinema4k`: Cinema 4K ProRes export
- `natural`: Natural color enhancement
- `cinematic`: Teal and orange cinematic look
- `portrait`: Portrait mode with skin tone protection

List all presets with: `speedy --list-presets`

## Development

This project uses Git hooks for maintaining code quality. When you enter the development environment, hooks are automatically configured:

```bash
$ nix develop
📎 Setting up Git hooks for code quality checks...
✅ Git hooks configured automatically!
   • pre-commit: Checks code formatting
   • pre-push: Runs formatting and clippy checks
```

### Git Hooks

The project includes two Git hooks that help maintain code quality:

1. **pre-commit**: Ensures code is properly formatted before committing
2. **pre-push**: Runs both formatting and clippy checks before pushing

These hooks are automatically configured when you enter the nix development shell. To manually configure them:

```bash
git config core.hooksPath .githooks
```

To disable the hooks temporarily:

```bash
git config --unset core.hooksPath
```

### Running Checks Manually

You can run the quality checks manually at any time:

```bash
# Check formatting
nix develop -c cargo fmt --check

# Fix formatting
nix develop -c cargo fmt

# Run clippy
nix develop -c cargo clippy --workspace -- -D warnings

# Run all checks
nix develop -c cargo fmt --check && nix develop -c cargo clippy --workspace -- -D warnings
```

### Workspace Structure

```
speedy/
├── Cargo.toml           # Workspace configuration
├── speedy-core/         # Core library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── ffmpeg_wrapper.rs
│       ├── video_processor.rs
│       ├── presets.rs
│       ├── color_enhance.rs
│       └── lut.rs
└── speedy-cli/          # CLI application
    ├── Cargo.toml
    └── src/
        └── main.rs
```

### Using speedy-core as a Library

Add to your `Cargo.toml`:

```toml
[dependencies]
speedy-core = { git = "https://github.com/yourusername/speedy.git" }
```

Example usage:

```rust
use speedy_core::{VideoProcessor, ColorProfile, Preset};

fn main() -> anyhow::Result<()> {
    let mut processor = VideoProcessor::new("input.mp4", "output.mp4");
    
    processor = processor
        .speed(2.0)
        .profile(ColorProfile::DLog)
        .vibrance(0.5)
        .quality(20);
    
    processor.process()?;
    Ok(())
}
```

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.