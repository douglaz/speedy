use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

use speedy_core::{ColorProfile, Preset, VideoProcessor, check_ffmpeg};

#[derive(Parser, Debug)]
#[command(name = "speedy")]
#[command(
    about = "Video processing tool for speed adjustment, LUT application, and color enhancement"
)]
#[command(version)]
struct Args {
    /// Input video file path
    #[arg(short, long, required_unless_present = "list_presets")]
    input: Option<PathBuf>,

    /// Output video file path
    #[arg(short, long, required_unless_present = "list_presets")]
    output: Option<PathBuf>,

    /// Use a preset configuration
    #[arg(long, value_name = "PRESET")]
    preset: Option<String>,

    /// Speed multiplier (e.g., 2.0 for 2x speed)
    #[arg(short, long, default_value = "1.0")]
    speed: f64,

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

    /// Enable video stabilization
    #[arg(long)]
    stabilize: bool,

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
    let args = Args::parse();

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
        println!("\nUsage: speedy -i input.mp4 -o output.mp4 --preset dji-dlog");
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

    // Get input and output paths (they must exist if we get here)
    let input = args
        .input
        .ok_or_else(|| anyhow::anyhow!("Input file required"))?;
    let output = args
        .output
        .ok_or_else(|| anyhow::anyhow!("Output file required"))?;

    // Validate input file
    if !input.exists() {
        anyhow::bail!("Input file does not exist: {:?}", input);
    }

    // Create output directory if it doesn't exist
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).context("Failed to create output directory")?;
    }

    log::info!("Starting video processing...");
    log::info!("Input: {:?}", input);
    log::info!("Output: {:?}", output);

    // Create video processor
    let mut processor = VideoProcessor::new(&input, &output);

    // Apply preset if specified
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

    // Apply individual settings (these override preset values)
    processor = processor
        .speed(args.speed)
        .codec(&args.codec)
        .quality(args.quality)
        .profile(args.profile)
        .contrast(args.contrast)
        .saturation(args.saturation)
        .hardware_accel(args.hw_accel)
        .stabilize(args.stabilize)
        .auto_rotate(!args.no_auto_rotate);

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

    // Process the video
    processor.process()?;

    println!("\n✅ Video processing completed successfully!");
    println!("📁 Output saved to: {:?}", output);

    Ok(())
}
