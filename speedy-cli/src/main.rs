use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::{Path, PathBuf};

use speedy_core::{ColorProfile, Preset, VideoProcessor, check_ffmpeg};

#[derive(Parser, Debug)]
#[command(name = "speedy")]
#[command(
    about = "Video processing tool for speed adjustment, LUT application, and color enhancement"
)]
#[command(version)]
struct Args {
    /// Input video file(s) or a directory. Pass several to stitch them together
    /// in order; a directory is expanded to its video files sorted by name.
    #[arg(short, long, required_unless_present = "list_presets", num_args = 1..)]
    input: Vec<PathBuf>,

    /// Output video file path
    #[arg(short, long, required_unless_present = "list_presets")]
    output: Option<PathBuf>,

    /// Use a preset configuration
    #[arg(long, value_name = "PRESET")]
    preset: Option<String>,

    /// Speed multiplier (e.g., 2.0 for 2x speed)
    #[arg(short, long, default_value = "1.0")]
    speed: f64,

    /// Output frame rate for speed changes (e.g. "30" or "30000/1001").
    /// Defaults to the source frame rate, so a speed-up drops frames instead of
    /// inflating the frame rate (a 10x speed-up of 30fps stays 30fps).
    #[arg(long, value_name = "FPS")]
    output_fps: Option<String>,

    /// LUT file path for color grading (supports .cube files)
    #[arg(short, long)]
    lut: Option<PathBuf>,

    /// Color profile of the source footage
    #[arg(short = 'p', long, value_enum, default_value = "standard")]
    profile: ColorProfile,

    /// Contrast enhancement level (0.0 to 2.0)
    #[arg(short = 'c', long, default_value = "1.0")]
    contrast: f32,

    /// Saturation enhancement level (0.0 to 2.0)
    #[arg(short = 'S', long, default_value = "1.0")]
    saturation: f32,

    /// Video codec for output
    #[arg(long, default_value = "h264")]
    codec: String,

    /// Video bitrate in Mbps
    #[arg(short, long)]
    bitrate: Option<u32>,

    /// Output video quality (0-51, lower is better)
    #[arg(short, long, default_value = "23")]
    quality: u8,

    /// Enable hardware acceleration if available
    #[arg(long)]
    hw_accel: bool,

    /// Number of threads for processing
    #[arg(short, long)]
    threads: Option<usize>,

    /// Enable video stabilization (two-pass vidstab; per-segment when stitching)
    #[arg(long)]
    stabilize: bool,

    /// Stabilization smoothing window in frames (higher = glassier glide)
    #[arg(long, value_name = "FRAMES")]
    stabilize_smoothing: Option<u32>,

    /// Disable auto-rotation based on metadata
    #[arg(long)]
    no_auto_rotate: bool,

    /// Apply denoising (strength: 1-10)
    #[arg(long)]
    denoise: Option<u8>,

    /// Apply sharpening (strength: 0.1-2.0)
    #[arg(long)]
    sharpen: Option<f32>,

    /// Apply vibrance for intelligent saturation (-2.0 to 2.0, protects skin tones)
    #[arg(long)]
    vibrance: Option<f32>,

    /// Remove atmospheric haze at the given strength (~0.5 medium, 1.0 strong;
    /// clamped to 0.0-1.0). Pulls the black point, adds contrast, and restores
    /// saturation/vibrance.
    #[arg(long, value_name = "STRENGTH")]
    dehaze: Option<f32>,

    /// Apply color curves (e.g., "preset=lighter" or "red='0/0 0.5/0.6 1/1'")
    #[arg(long)]
    curves: Option<String>,

    /// Adjust hue in degrees (-180 to 180)
    #[arg(long)]
    hue_shift: Option<f32>,

    /// Color balance: shadows,midtones,highlights as r:g:b values (-1 to 1)
    /// Example: "0.1:-0.1:0,0:0:0,-0.1:0:0.1"
    #[arg(long)]
    color_balance: Option<String>,

    /// Selective color adjustment for specific color ranges
    /// Format: "reds=0.1:0:-0.1:0,blues=-0.1:0:0.1:0"
    #[arg(long)]
    selective_color: Option<String>,

    /// Scale video resolution (e.g., "1920x1080", "1920:-1" for auto height)
    #[arg(long)]
    scale: Option<String>,

    /// List available presets
    #[arg(long)]
    list_presets: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let matches = Args::command().get_matches();
    let args = Args::from_arg_matches(&matches)?;

    // Initialize logging
    if args.verbose {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    }

    // List presets if requested
    if args.list_presets {
        println!("\nAvailable presets:");
        println!("{:-<50}", "");
        for (name, description) in Preset::list_all() {
            println!("{:<15} - {}", name, description);
        }
        println!("\nUsage: speedy -i input.mp4 -o output.mp4 --preset mavic4pro-dlog");
        return Ok(());
    }

    // Check FFmpeg availability
    match check_ffmpeg() {
        Ok(version) => {
            log::info!("FFmpeg version {} detected", version);
        }
        Err(e) => {
            eprintln!("Error: FFmpeg not found!");
            eprintln!("Please install FFmpeg to use this tool.");
            eprintln!();
            eprintln!("Installation instructions:");
            eprintln!("  Ubuntu/Debian: sudo apt install ffmpeg");
            eprintln!("  macOS:         brew install ffmpeg");
            eprintln!("  Windows:       Download from https://ffmpeg.org/download.html");
            eprintln!();
            eprintln!("Details: {}", e);
            std::process::exit(1);
        }
    }

    // Resolve inputs: expand any directories into sorted video files.
    let inputs = resolve_inputs(&args.input)?;
    if inputs.is_empty() {
        anyhow::bail!("No input video files found");
    }
    for file in &inputs {
        if !file.exists() {
            anyhow::bail!("Input file does not exist: {:?}", file);
        }
    }

    let output = args
        .output
        .ok_or_else(|| anyhow::anyhow!("Output file required"))?;

    // Create output directory if it doesn't exist
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).context("Failed to create output directory")?;
    }

    log::info!("Starting video processing...");
    if inputs.len() == 1 {
        log::info!("Input: {:?}", inputs[0]);
    } else {
        log::info!("Inputs ({}): {:?}", inputs.len(), inputs);
    }
    log::info!("Output: {:?}", output);

    // Create video processor
    let mut processor = VideoProcessor::new_multi(inputs, &output);

    // Apply preset if specified
    let preset_used = args.preset.is_some();
    if let Some(preset_name) = &args.preset {
        if let Some(preset) = Preset::from_name(preset_name) {
            log::info!("Applying preset: {}", preset_name);
            processor = preset.apply(processor);
        } else {
            anyhow::bail!(
                "Unknown preset: {}. Use --list-presets to see available options.",
                preset_name
            );
        }
    }

    // Apply individual settings (these override preset values).
    // When a preset is used, only apply a setting if the user passed the flag
    // explicitly on the command line, so preset values are not clobbered by
    // the clap default values.
    let explicit =
        |id: &str| matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
    if !preset_used || explicit("speed") {
        processor = processor.speed(args.speed);
    }
    if !preset_used || explicit("codec") {
        processor = processor.codec(&args.codec);
    }
    if !preset_used || explicit("quality") {
        processor = processor.quality(args.quality);
    }
    if !preset_used || explicit("profile") {
        processor = processor.profile(args.profile);
    }
    if !preset_used || explicit("contrast") {
        processor = processor.contrast(args.contrast);
    }
    if !preset_used || explicit("saturation") {
        processor = processor.saturation(args.saturation);
    }
    // Boolean toggles are gated the same way, so a preset that turns them on
    // (e.g. stabilization) is not silently reset by the flag defaults.
    if !preset_used || explicit("hw_accel") {
        processor = processor.hardware_accel(args.hw_accel);
    }
    if !preset_used || explicit("stabilize") {
        processor = processor.stabilize(args.stabilize);
    }
    if !preset_used || explicit("no_auto_rotate") {
        processor = processor.auto_rotate(!args.no_auto_rotate);
    }

    // Apply optional settings
    if let Some(bitrate) = args.bitrate {
        processor = processor.bitrate(bitrate);
    }

    if let Some(threads) = args.threads {
        processor = processor.threads(threads);
    }

    if let Some(lut) = args.lut {
        processor = processor.lut(lut);
    }

    if let Some(denoise) = args.denoise {
        processor = processor.denoise(denoise);
    }

    if let Some(sharpen) = args.sharpen {
        processor = processor.sharpen(sharpen);
    }

    if let Some(vibrance) = args.vibrance {
        processor = processor.vibrance(vibrance);
    }

    if let Some(dehaze) = args.dehaze {
        processor = processor.dehaze(dehaze);
    }

    if let Some(smoothing) = args.stabilize_smoothing {
        processor = processor.stabilize_smoothing(smoothing);
    }

    if let Some(curves) = args.curves {
        processor = processor.curves(&curves);
    }

    if let Some(hue_shift) = args.hue_shift {
        processor = processor.hue_shift(hue_shift);
    }

    if let Some(color_balance) = args.color_balance {
        processor = processor.color_balance_str(&color_balance);
    }

    if let Some(selective_color) = args.selective_color {
        processor = processor.selective_color(&selective_color);
    }

    if let Some(scale) = args.scale {
        processor = processor.scale(&scale);
    }

    if let Some(output_fps) = args.output_fps {
        processor = processor.output_fps(&output_fps);
    }

    // Process the video
    processor.process()?;

    println!("\n✅ Video processing completed successfully!");
    println!("📁 Output saved to: {:?}", output);

    Ok(())
}

/// Expand the given paths into an ordered list of input files. Directories are
/// replaced by their video files sorted by name; regular paths are kept as-is.
fn resolve_inputs(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut dir_files: Vec<PathBuf> = std::fs::read_dir(path)
                .with_context(|| format!("Failed to read directory: {}", path.display()))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| is_video_file(p))
                .collect();
            dir_files.sort();
            if dir_files.is_empty() {
                anyhow::bail!("No video files found in directory: {}", path.display());
            }
            log::info!(
                "Found {} video file(s) in {}",
                dir_files.len(),
                path.display()
            );
            files.extend(dir_files);
        } else {
            files.push(path.clone());
        }
    }
    Ok(files)
}

/// Whether a path looks like a video file based on its extension.
fn is_video_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .as_deref(),
        Some("mp4" | "mov" | "m4v" | "mkv" | "avi" | "webm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_video_file_matches_extensions_case_insensitively() {
        for name in ["a.mp4", "a.MP4", "b.mov", "c.MKV", "d.webm"] {
            assert!(is_video_file(Path::new(name)), "{name} should be a video");
        }
        for name in ["telemetry.srt", "proxy.LRF", "notes.txt", "noext"] {
            assert!(
                !is_video_file(Path::new(name)),
                "{name} should not be a video"
            );
        }
    }

    #[test]
    fn resolve_inputs_passes_through_explicit_files_in_order() -> Result<()> {
        let inputs = vec![PathBuf::from("b.mp4"), PathBuf::from("a.mov")];
        // Explicit (non-directory) paths are kept as given, in order.
        assert_eq!(resolve_inputs(&inputs)?, inputs);
        Ok(())
    }

    #[test]
    fn resolve_inputs_expands_directory_sorted_video_only() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("speedy_resolve_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        for name in ["clip_b.mp4", "clip_a.mp4", "telemetry.srt", "proxy.LRF"] {
            std::fs::write(dir.join(name), b"")?;
        }

        let resolved = resolve_inputs(std::slice::from_ref(&dir));
        let _ = std::fs::remove_dir_all(&dir);

        let names: Vec<String> = resolved?
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        // Non-video files excluded; videos returned sorted by name.
        assert_eq!(names, vec!["clip_a.mp4", "clip_b.mp4"]);
        Ok(())
    }
}
