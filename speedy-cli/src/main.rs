use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::{Path, PathBuf};

use speedy_core::{
    InputColor, LutSpace, OutputColor, Preset, VideoProcessor, check_ffmpeg, ensure_ocio_filter,
};

/// Appended to HDR errors when HDR was only reached through the D-Log default,
/// so a command line that used to produce Rec.709 says how to get that back.
const REC709_HINT: &str = "HLG is the default output for D-Log input; pass --output-color rec709 for the previous Rec.709 behaviour.";

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

    /// LUT file path for color grading (supports .cube files). With HDR
    /// output it must be an ACEScct LUT, declared with --lut-space acescct.
    #[arg(short, long)]
    lut: Option<PathBuf>,

    /// Color space the --lut works in. Required for a LUT with HDR output.
    #[arg(long, value_enum, value_name = "SPACE")]
    lut_space: Option<LutSpace>,

    /// Color profile of the source footage (same as --input-color; `d-log` is
    /// an alias for `dji-dlog`, `d-log-m` for `dji-dlog-m`)
    #[arg(
        short = 'p',
        long,
        value_enum,
        default_value = "standard",
        conflicts_with = "input_color"
    )]
    profile: InputColor,

    /// Color encoding of the source footage
    #[arg(long, value_enum, value_name = "COLOR")]
    input_color: Option<InputColor>,

    /// Color of the output. Defaults to `hlg` (HDR, for YouTube) when the
    /// input is D-Log and no preset is used, otherwise `rec709`. `hlg` needs
    /// the ffmpeg build with OpenColorIO from the Nix dev shell, encodes HEVC
    /// Main 10, and refuses stabilization, presets and the Rec.709-only
    /// adjustments. Pass `rec709` for the SDR route through the profile LUT.
    #[arg(long, value_enum, value_name = "COLOR")]
    output_color: Option<OutputColor>,

    /// Exposure compensation in stops (-3 to 3), applied in ACEScg linear
    /// light. HDR output only; defaults to 0.7, which puts 18% grey at the
    /// HLG reference level. A value given here replaces the default.
    #[arg(long, value_name = "STOPS", allow_negative_numbers = true)]
    exposure: Option<f32>,

    /// Contrast enhancement level (0.0 to 2.0; HDR: 0.3 to 2.0, in ACEScct)
    #[arg(short = 'c', long, default_value = "1.0")]
    contrast: f32,

    /// Saturation enhancement level (0.0 to 2.0; HDR: applied in ACEScct)
    #[arg(short = 'S', long, default_value = "1.0")]
    saturation: f32,

    /// Video codec for output (HDR output defaults to h265 and accepts
    /// nothing else)
    #[arg(long, default_value = "h264")]
    codec: String,

    /// Video bitrate in Mbps
    #[arg(short, long)]
    bitrate: Option<u32>,

    /// Output video quality (0-51, lower is better; HDR output defaults to 18)
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

    /// Disable stabilization, overriding a preset that enables it (e.g.
    /// mavic4pro-dlog). Keeps the image 10-bit, which vidstab cannot.
    #[arg(long, conflicts_with = "stabilize")]
    no_stabilize: bool,

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

    /// Remove atmospheric haze at the given strength (~0.5 medium, 1.0 strong).
    /// Rec.709: clamped to 0.0-1.0; pulls the black point, adds contrast, and
    /// restores saturation/vibrance. HDR: must be 0.0-1.0; subtracts the haze
    /// veil in linear ACEScg and adds ACEScct contrast and saturation.
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
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Output file required"))?;

    log::info!("Starting video processing...");
    if inputs.len() == 1 {
        log::info!("Input: {:?}", inputs[0]);
    } else {
        log::info!("Inputs ({}): {:?}", inputs.len(), inputs);
    }
    log::info!("Output: {:?}", output);

    let (processor, hdr_defaulted) = build_processor(&args, &matches, inputs, &output)?;
    if processor.color().output == OutputColor::Rec2100Hlg {
        log::info!("Output color: Rec.2100 HLG (HDR) via ACES");
        ensure_ocio_filter().map_err(|e| with_rec709_hint(e, hdr_defaulted))?;
    }

    // Create output directory if it doesn't exist. Skip an empty parent (a bare
    // filename like `out.mp4`), where create_dir_all("") would error.
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context("Failed to create output directory")?;
    }

    // Process the video
    processor.process()?;

    println!("\n✅ Video processing completed successfully!");
    println!("📁 Output saved to: {:?}", output);

    Ok(())
}

/// Build the processor a command line describes: preset first, then the
/// individual flags, then the output colour default. Returns whether HDR output
/// was only reached through that default. The configuration is validated here,
/// before anything runs.
fn build_processor(
    args: &Args,
    matches: &clap::ArgMatches,
    inputs: Vec<PathBuf>,
    output: &Path,
) -> Result<(VideoProcessor, bool)> {
    let mut processor = VideoProcessor::new_multi(inputs, output);

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
    // Codec and quality are only forwarded when passed: left unset, the
    // processor picks the default for the output colour (h264 / CRF 23 for
    // Rec.709, h265 / CRF 18 for HDR).
    if explicit("codec") {
        processor = processor.codec(&args.codec);
    }
    if explicit("quality") {
        processor = processor.quality(args.quality);
    }
    if !preset_used || explicit("profile") || args.input_color.is_some() {
        processor = processor.input_color(args.input_color.unwrap_or(args.profile));
    }
    // D-Log footage is delivered as HDR unless a preset (all of which are
    // Rec.709 grades) or an explicit --output-color says otherwise.
    let (output_color, hdr_defaulted) =
        resolve_output_color(processor.color().input, preset_used, args.output_color);
    processor = processor.output_color(output_color);
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
    // --no-stabilize is the only way to switch a preset's stabilization back off,
    // since the `--stabilize` flag's default is indistinguishable from "unset".
    if args.no_stabilize {
        processor = processor.stabilize(false);
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

    if let Some(lut) = &args.lut {
        processor = processor.lut(lut);
    }

    if let Some(space) = args.lut_space {
        processor = processor.lut_space(space);
    }

    if let Some(stops) = args.exposure {
        processor = processor.exposure(stops);
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
        // The no-op warning lives in VideoProcessor::process, where the
        // effective stabilization state (after presets) is known.
        processor = processor.stabilize_smoothing(smoothing);
    }

    if let Some(curves) = &args.curves {
        processor = processor.curves(curves);
    }

    if let Some(hue_shift) = args.hue_shift {
        processor = processor.hue_shift(hue_shift);
    }

    if let Some(color_balance) = &args.color_balance {
        processor = processor.color_balance_str(color_balance);
    }

    if let Some(selective_color) = &args.selective_color {
        processor = processor.selective_color(selective_color);
    }

    if let Some(scale) = &args.scale {
        processor = processor.scale(scale);
    }

    if let Some(output_fps) = &args.output_fps {
        processor = processor.output_fps(output_fps);
    }

    processor
        .validate()
        .map_err(|e| with_rec709_hint(e, hdr_defaulted))?;
    Ok((processor, hdr_defaulted))
}

/// The output colour for a run and whether it came from the D-Log default
/// rather than an explicit `--output-color`.
fn resolve_output_color(
    input: InputColor,
    preset_used: bool,
    requested: Option<OutputColor>,
) -> (OutputColor, bool) {
    match requested {
        Some(output) => (output, false),
        None if input == InputColor::DjiDLogDGamut && !preset_used => {
            (OutputColor::Rec2100Hlg, true)
        }
        None => (OutputColor::Rec709, false),
    }
}

/// Add [`REC709_HINT`] to an HDR error when HDR output was not asked for
/// explicitly.
fn with_rec709_hint(error: anyhow::Error, hdr_defaulted: bool) -> anyhow::Error {
    if hdr_defaulted {
        anyhow::anyhow!("{error:#}\n{REC709_HINT}")
    } else {
        error
    }
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
    use speedy_core::{ProbedInputs, VideoInfo};

    /// Build the processor for a command line, as `main` does.
    fn parse(argv: &[&str]) -> Result<(VideoProcessor, bool)> {
        let matches = Args::command().try_get_matches_from(["speedy"].iter().chain(argv))?;
        let args = Args::from_arg_matches(&matches)?;
        let output = args.output.clone().context("no output")?;
        build_processor(&args, &matches, args.input.clone(), &output)
    }

    fn error_of(argv: &[&str]) -> String {
        match parse(argv) {
            Ok(_) => panic!("{argv:?} should have been rejected"),
            Err(e) => format!("{e:#}"),
        }
    }

    fn clip(width: u32, height: u32, has_audio: bool) -> VideoInfo {
        VideoInfo {
            duration: 1.0,
            width,
            height,
            fps: 30.0,
            rotation: 0,
            has_audio,
            color_space: Some("bt709".to_string()),
            color_range: Some("tv".to_string()),
        }
    }

    /// The ffmpeg argument vector and working directory a command line
    /// produces. The probe results stand in for `ffprobe`: clip A is
    /// 1920x1080 with audio, clip B (when stitching) 1280x720 without, both
    /// 30 fps, and the input colour's Rec.709 LUT counts as installed.
    fn plan(argv: &[&str]) -> Result<(Vec<String>, Option<PathBuf>)> {
        let (processor, _) = parse(argv)?;
        let stitching = argv.contains(&"/clips/b.mp4");
        let mut infos = vec![clip(1920, 1080, true)];
        if stitching {
            infos.push(clip(1280, 720, false));
        }
        let command = processor
            .plan(&ProbedInputs {
                infos,
                stitch_fps: "30/1".to_string(),
                target_fps: Some("30/1".to_string()),
                profile_lut: processor.profile_lut_path(),
            })?
            .build();
        let args = command
            .get_args()
            .map(|a| unix_style(a.to_string_lossy().into_owned()))
            .collect();
        let dir = command
            .get_current_dir()
            .map(|d| PathBuf::from(unix_style(d.to_string_lossy().into_owned())));
        Ok((args, dir))
    }

    /// Inputs, outputs and the LUT directory are absolutized, which on Windows
    /// turns "/clips/a.mp4" into "D:\clips\a.mp4"; map back so the vectors
    /// pinned below hold on every platform.
    fn unix_style(path: String) -> String {
        match path.get(1..3) {
            Some(":\\") if cfg!(windows) => path[2..].replace('\\', "/"),
            _ => path,
        }
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let at = args.iter().position(|a| a == flag)?;
        args.get(at + 1).map(String::as_str)
    }

    const SINGLE: [&str; 4] = ["-i", "/clips/a.mp4", "-o", "/out/out.mp4"];
    const STITCH: [&str; 5] = ["-i", "/clips/a.mp4", "/clips/b.mp4", "-o", "/out/out.mp4"];

    fn with<'a>(base: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
        base.iter().chain(extra).copied().collect()
    }

    // ---- SDR regression: argument vectors captured from the last release
    // before the HDR route existed (commit c03ba66), by running that binary
    // against an argv-logging `ffmpeg`. Rec.709 output must not drift.

    fn assert_sdr(extra: &[&str], out: &str, expected: &[&str], lut_dir: bool) -> Result<()> {
        let argv: Vec<&str> = ["-i", "/clips/a.mp4"]
            .iter()
            .chain(extra.iter().filter(|a| **a == "/clips/b.mp4"))
            .chain(["-o", out].iter())
            .chain(extra.iter().filter(|a| **a != "/clips/b.mp4"))
            .copied()
            .collect();
        let (args, dir) = plan(&argv)?;
        assert_eq!(args, expected, "argv for {argv:?}");
        // The profile LUT is referenced by name from inside `luts/`.
        assert_eq!(
            dir.as_deref().is_some_and(|d| d.ends_with("luts")),
            lut_dir,
            "working directory {dir:?} for {argv:?}"
        );
        Ok(())
    }

    const TAIL_MP4: [&str; 5] = [
        "-map_metadata",
        "0",
        "-movflags",
        "use_metadata_tags",
        "/out/out.mp4",
    ];

    fn expect<'a>(head: &[&'a str], tail: &[&'a str]) -> Vec<&'a str> {
        with(head, tail)
    }

    #[test]
    fn sdr_plain_and_speed_argv_unchanged() -> Result<()> {
        assert_sdr(
            &[],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            ),
            false,
        )?;
        assert_sdr(
            &["--speed", "2"],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]setpts=PTS/2,fps=30/1,format=yuv420p[v]; [0:a]atempo=2[a]",
                    "-map",
                    "[v]",
                    "-map",
                    "[a]",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            ),
            false,
        )
    }

    #[test]
    fn sdr_lut_argv_unchanged() -> Result<()> {
        let (args, dir) = plan(&with(
            &SINGLE,
            &[
                "--lut",
                "/grades/grade.cube",
                "--contrast",
                "1.1",
                "--saturation",
                "1.2",
            ],
        ))?;
        assert_eq!(
            args,
            expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]lut3d=file='grade.cube',eq=contrast=1.10:saturation=1.20,format=yuv420p[v]",
                    "-map",
                    "[v]",
                    "-map",
                    "0:a?",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            )
        );
        assert_eq!(dir.as_deref(), Some(Path::new("/grades")));
        Ok(())
    }

    #[test]
    fn dlog_m_takes_the_dji_lut_and_stays_rec709() -> Result<()> {
        let dlog_m = [
            "-y",
            "-i",
            "/clips/a.mp4",
            "-filter_complex",
            "[0:v]lut3d=file='dji_dlogm_to_rec709.cube',format=yuv420p[v]",
            "-map",
            "[v]",
            "-map",
            "0:a?",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "23",
        ];
        // No --output-color: unlike D-Log, D-Log M has no HDR route to default to.
        for spelling in [["--profile", "d-log-m"], ["--input-color", "dji-dlog-m"]] {
            assert_sdr(&spelling, "/out/out.mp4", &expect(&dlog_m, &TAIL_MP4), true)?;
        }
        let hdr = error_of(&with(
            &SINGLE,
            &["--profile", "d-log-m", "--output-color", "hlg"],
        ));
        assert!(hdr.contains("D-Log M input has no HDR route"), "{hdr}");
        Ok(())
    }

    #[test]
    fn sdr_dlog_rec709_argv_unchanged() -> Result<()> {
        // What bare `--profile d-log` produced before HLG became its default.
        let dlog = [
            "-y",
            "-i",
            "/clips/a.mp4",
            "-filter_complex",
            "[0:v]lut3d=file='mavic4_pro_dlog_to_rec709.cube',format=yuv420p[v]",
            "-map",
            "[v]",
            "-map",
            "0:a?",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "23",
        ];
        for spelling in [
            ["--profile", "d-log"],
            ["--profile", "dji-dlog"],
            ["--input-color", "dji-dlog"],
            ["--input-color", "d-log"],
        ] {
            assert_sdr(
                &with(&spelling, &["--output-color", "rec709"]),
                "/out/out.mp4",
                &expect(&dlog, &TAIL_MP4),
                true,
            )?;
        }
        assert_sdr(
            &[
                "--profile",
                "d-log",
                "--output-color",
                "rec709",
                "--dehaze",
                "0.5",
                "--speed",
                "10",
            ],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]setpts=PTS/10,fps=30/1,lut3d=file='mavic4_pro_dlog_to_rec709.cube',curves=all='0.050/0 1/1',eq=contrast=1.075:saturation=1.175:gamma=1.030,vibrance=intensity=0.450,format=yuv420p[v]; [0:a]atempo=2.0,atempo=2.0,atempo=2.0,atempo=1.25[a]",
                    "-map",
                    "[v]",
                    "-map",
                    "[a]",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            ),
            true,
        )
    }

    #[test]
    fn sdr_stitch_argv_unchanged() -> Result<()> {
        assert_sdr(
            &[
                "/clips/b.mp4",
                "--profile",
                "d-log",
                "--output-color",
                "rec709",
                "--speed",
                "10",
                "--codec",
                "h265",
            ],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-i",
                    "/clips/b.mp4",
                    "-filter_complex",
                    "[0:v]scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1,setpts=PTS-STARTPTS[v0];[1:v]scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1,setpts=PTS-STARTPTS[v1];[v0][v1]concat=n=2:v=1[cat];[cat]setpts=PTS/10,fps=30/1,lut3d=file='mavic4_pro_dlog_to_rec709.cube',format=yuv420p10le[v]",
                    "-map",
                    "[v]",
                    "-c:v",
                    "libx265",
                    "-pix_fmt",
                    "yuv420p10le",
                    "-flags",
                    "+cgop",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            ),
            true,
        )
    }

    #[test]
    fn sdr_preset_argv_unchanged() -> Result<()> {
        assert_sdr(
            &["--preset", "youtube"],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]eq=contrast=1.05:saturation=1.05,format=yuv420p[v]",
                    "-map",
                    "[v]",
                    "-map",
                    "0:a?",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "18",
                    "-b:v",
                    "16M",
                ],
                &TAIL_MP4,
            ),
            false,
        )?;
        // The D-Log preset stays on the Rec.709 LUT route.
        assert_sdr(
            &["--preset", "mavic4pro-dlog", "--no-stabilize"],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]lut3d=file='mavic4_pro_dlog_to_rec709.cube',eq=contrast=1.15:saturation=1.00,vibrance=intensity=0.30,format=yuv420p10le[v]",
                    "-map",
                    "[v]",
                    "-map",
                    "0:a?",
                    "-c:v",
                    "libx265",
                    "-pix_fmt",
                    "yuv420p10le",
                    "-flags",
                    "+cgop",
                    "-crf",
                    "20",
                ],
                &TAIL_MP4,
            ),
            true,
        )
    }

    #[test]
    fn sdr_x265_argv_unchanged() -> Result<()> {
        let mut expected = vec!["-y"];
        if cfg!(target_os = "linux") {
            expected.extend(["-hwaccel", "vaapi"]);
        } else if cfg!(target_os = "macos") {
            expected.extend(["-hwaccel", "videotoolbox"]);
        } else if cfg!(target_os = "windows") {
            expected.extend(["-hwaccel", "dxva2"]);
        }
        expected.extend([
            "-i",
            "/clips/a.mp4",
            "-filter_complex",
            "[0:v]nlmeans=s=3,scale=1280:-1,format=yuv420p10le[v]",
            "-map",
            "[v]",
            "-map",
            "0:a?",
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            "-flags",
            "+cgop",
            "-crf",
            "18",
            "-b:v",
            "8M",
            "-threads",
            "4",
        ]);
        assert_sdr(
            &[
                "--codec",
                "h265",
                "--quality",
                "18",
                "--scale",
                "1280:-1",
                "--denoise",
                "3",
                "--bitrate",
                "8",
                "--threads",
                "4",
                "--hw-accel",
            ],
            "/out/out.mp4",
            &expect(&expected, &TAIL_MP4),
            false,
        )
    }

    #[test]
    fn sdr_prores_and_log_profile_argv_unchanged() -> Result<()> {
        assert_sdr(
            &[
                "--codec",
                "prores",
                "--vibrance",
                "0.3",
                "--no-auto-rotate",
                "--curves",
                "preset=lighter",
                "--hue-shift",
                "5",
                "--sharpen",
                "0.5",
            ],
            "/out/out.mov",
            &[
                "-y",
                "-noautorotate",
                "-i",
                "/clips/a.mp4",
                "-filter_complex",
                "[0:v]unsharp=5:5:0.50:5:5:0.25,vibrance=intensity=0.30,curves=preset=lighter,hue=h=5.0,format=yuv422p10le[v]",
                "-map",
                "[v]",
                "-map",
                "0:a?",
                "-c:v",
                "prores_ks",
                "-pix_fmt",
                "yuv422p10le",
                "-crf",
                "23",
                "-map_metadata",
                "0",
                "-movflags",
                "use_metadata_tags",
                "/out/out.mov",
            ],
            false,
        )?;
        // A non-DJI log profile: Rec.709 route, its own LUT, untouched.
        assert_sdr(
            &[
                "--profile",
                "s-log",
                "--selective-color",
                "reds=0.1:0:-0.1:0",
                "--color-balance",
                "0.1:-0.1:0,0:0:0,-0.1:0:0.1",
            ],
            "/out/out.mp4",
            &expect(
                &[
                    "-y",
                    "-i",
                    "/clips/a.mp4",
                    "-filter_complex",
                    "[0:v]lut3d=file='sony_slog_to_rec709.cube',colorbalance=rs=0.10:gs=-0.10:bs=0.00:rm=0.00:gm=0.00:bm=0.00:rh=-0.10:gh=0.00:bh=0.10,selectivecolor=reds=0.1:0:-0.1:0,format=yuv420p[v]",
                    "-map",
                    "[v]",
                    "-map",
                    "0:a?",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-crf",
                    "23",
                ],
                &TAIL_MP4,
            ),
            true,
        )
    }

    // ---- Output colour default.

    #[test]
    fn bare_dlog_defaults_to_hlg_everything_else_to_rec709() -> Result<()> {
        for dlog in [["--profile", "d-log"], ["--input-color", "dji-dlog"]] {
            let (processor, defaulted) = parse(&with(&SINGLE, &dlog))?;
            assert_eq!(processor.color().input, InputColor::DjiDLogDGamut);
            assert_eq!(processor.color().output, OutputColor::Rec2100Hlg);
            assert!(defaulted, "{dlog:?} reaches HDR through the default");
        }

        let (processor, defaulted) = parse(&with(
            &SINGLE,
            &["--profile", "d-log", "--output-color", "hlg"],
        ))?;
        assert_eq!(processor.color().output, OutputColor::Rec2100Hlg);
        assert!(!defaulted, "explicit --output-color is not the default");

        for rec709 in [
            vec![],
            vec!["--profile", "standard"],
            vec!["--profile", "s-log"],
            vec!["--preset", "mavic4pro-dlog"],
            vec!["--preset", "mavic4pro-dlog", "--profile", "d-log"],
            vec!["--preset", "youtube", "--input-color", "dji-dlog"],
            vec!["--profile", "d-log", "--output-color", "rec709"],
        ] {
            let (processor, defaulted) = parse(&with(&SINGLE, &rec709))?;
            assert_eq!(processor.color().output, OutputColor::Rec709, "{rec709:?}");
            assert!(!defaulted, "{rec709:?}");
        }

        // The D-Log preset still declares D-Log input, for its Rec.709 LUT.
        let (processor, _) = parse(&with(&SINGLE, &["--preset", "mavic4pro-dlog"]))?;
        assert_eq!(processor.color().input, InputColor::DjiDLogDGamut);
        Ok(())
    }

    #[test]
    fn profile_and_input_color_cannot_be_combined() {
        let message = error_of(&with(
            &SINGLE,
            &["--profile", "d-log", "--input-color", "dji-dlog"],
        ));
        assert!(message.contains("cannot be used with"), "{message}");
    }

    // ---- HDR command.

    const QUOTED_CONFIG: &str = r"'ocio\://studio-config-v4.0.0_aces-v2.0_ocio-v2.5'";

    /// Every pixel format named anywhere in an argument vector.
    fn pixel_formats(args: &[String]) -> Vec<String> {
        let mut formats: Vec<String> = args
            .iter()
            .flat_map(|arg| arg.split("format=").skip(1))
            .map(|rest| {
                rest.split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        formats.extend(value_after(args, "-pix_fmt").map(String::from));
        formats
    }

    fn assert_hdr_encode(args: &[String], crf: &str) {
        assert_eq!(value_after(args, "-c:v"), Some("libx265"), "{args:?}");
        assert_eq!(
            value_after(args, "-pix_fmt"),
            Some("yuv420p10le"),
            "{args:?}"
        );
        assert_eq!(value_after(args, "-crf"), Some(crf), "{args:?}");
        assert_eq!(value_after(args, "-color_primaries"), Some("bt2020"));
        assert_eq!(value_after(args, "-color_trc"), Some("arib-std-b67"));
        assert_eq!(value_after(args, "-colorspace"), Some("bt2020nc"));
        assert_eq!(value_after(args, "-color_range"), Some("tv"));
        assert_eq!(
            value_after(args, "-x265-params"),
            Some("colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited")
        );
        // Nothing in the graph or at the encoder may name an 8-bit format.
        let formats = pixel_formats(args);
        assert!(formats.len() >= 4, "{formats:?}");
        for format in &formats {
            assert!(
                format == "gbrpf32le" || format == "yuv420p10le",
                "8-bit leak: {format} in {args:?}"
            );
        }
        // And the Rec.709 LUT never appears on the HDR route.
        assert!(!args.iter().any(|a| a.contains("rec709")), "{args:?}");
    }

    #[test]
    fn hdr_single_clip_argv() -> Result<()> {
        let (args, dir) = plan(&with(&SINGLE, &["--profile", "d-log"]))?;
        let graph = format!(
            "[0:v]zscale=matrixin=709:rangein=limited:matrix=gbr:range=full,format=gbrpf32le,\
             ocio=config={QUOTED_CONFIG}:input='D-Log D-Gamut':output='ACEScg':format=gbrpf32le,\
             exposure=exposure=0.7000:black=0,\
             ocio=config={QUOTED_CONFIG}:input='ACEScg':display='Rec.2100-HLG - Display':view='ACES 2.0 - HDR 1000 nits (P3 D65)':format=gbrpf32le,\
             zscale=matrixin=gbr:rangein=full:matrix=2020_ncl:range=limited,format=yuv420p10le,\
             setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc:range=tv,\
             format=yuv420p10le[v]"
        );
        assert_eq!(
            args,
            [
                "-y",
                "-i",
                "/clips/a.mp4",
                "-filter_complex",
                graph.as_str(),
                "-map",
                "[v]",
                "-map",
                "0:a?",
                "-c:v",
                "libx265",
                "-pix_fmt",
                "yuv420p10le",
                "-flags",
                "+cgop",
                "-crf",
                "18",
                "-map_metadata",
                "0",
                "-movflags",
                "use_metadata_tags",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
                "-color_range",
                "tv",
                "-x265-params",
                "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited",
                "/out/out.mp4",
            ]
        );
        assert_eq!(dir, None, "no LUT directory on the HDR route");
        assert_hdr_encode(&args, "18");
        Ok(())
    }

    #[test]
    fn hdr_geometry_runs_before_the_float_conversion() -> Result<()> {
        // Speed + fps + scale, rotation left on and switched off.
        for rotate in [&[][..], &["--no-auto-rotate"][..]] {
            let extra = with(
                &[
                    "--input-color",
                    "dji-dlog",
                    "--speed",
                    "4",
                    "--scale",
                    "1920:-1",
                    "--quality",
                    "14",
                ],
                rotate,
            );
            let (args, _) = plan(&with(&SINGLE, &extra))?;
            assert_hdr_encode(&args, "14");
            assert_eq!(
                args.contains(&"-noautorotate".to_string()),
                !rotate.is_empty()
            );
            let graph = value_after(&args, "-filter_complex").context("no graph")?;
            assert!(
                graph.starts_with(
                    "[0:v]setpts=PTS/4,fps=30/1,scale=1920:-1,zscale=matrixin=709:rangein=limited:matrix=gbr:range=full,format=gbrpf32le,ocio="
                ),
                "{graph}"
            );
            // Audio follows the speed change, as on the SDR route.
            assert!(graph.ends_with("; [0:a]atempo=2.0,atempo=2[a]"), "{graph}");
        }
        Ok(())
    }

    #[test]
    fn hdr_stitch_argv_normalizes_then_grades_once() -> Result<()> {
        let (args, _) = plan(&with(&STITCH, &["--profile", "d-log", "--speed", "10"]))?;
        assert_hdr_encode(&args, "18");
        let graph = value_after(&args, "-filter_complex").context("no graph")?;
        assert!(
            graph.starts_with(
                "[0:v]scale=1280:720:force_original_aspect_ratio=decrease:in_color_matrix=bt709:out_color_matrix=bt709:in_range=limited:out_range=limited,format=yuv420p10le|yuv422p10le|yuv444p10le,pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1,setpts=PTS-STARTPTS[v0];[1:v]scale=1280:720:force_original_aspect_ratio=decrease:in_color_matrix=bt709:"
            ),
            "{graph}"
        );
        assert!(
            graph.contains(
                "[v0][v1]concat=n=2:v=1[cat];[cat]setpts=PTS/10,fps=30/1,zscale=matrixin=709:rangein=limited:matrix=gbr:range=full,format=gbrpf32le,ocio="
            ),
            "{graph}"
        );
        assert_eq!(
            graph.matches("zscale=").count(),
            2,
            "one conversion each way: {graph}"
        );
        Ok(())
    }

    #[test]
    fn hdr_grade_flags_reach_the_aces_chain_in_order() -> Result<()> {
        let (args, dir) = plan(&with(
            &SINGLE,
            &[
                "--profile",
                "d-log",
                "--exposure",
                "-0.5",
                "--contrast",
                "1.2",
                "--saturation",
                "1.1",
                "--lut",
                "/grades/my look.cube",
                "--lut-space",
                "acescct",
                "--codec",
                "hevc",
            ],
        ))?;
        assert_hdr_encode(&args, "18");
        assert_eq!(dir.as_deref(), Some(Path::new("/grades")));
        let graph = value_after(&args, "-filter_complex").context("no graph")?;
        let stages = [
            "zscale=matrixin=709",
            "input='D-Log D-Gamut':output='ACEScg'",
            "exposure=exposure=-0.5000:black=0",
            "input='ACEScg':output='ACEScct'",
            "exposure=exposure=0.148377:black=0.068931",
            "colorchannelmixer=rr=1.072777:",
            "lut3d=file='my look.cube'",
            "input='ACEScct':display='Rec.2100-HLG - Display':view='ACES 2.0 - HDR 1000 nits (P3 D65)'",
            "matrix=2020_ncl:range=limited,format=yuv420p10le",
        ];
        let positions: Vec<usize> = stages
            .iter()
            .map(|stage| {
                graph
                    .find(stage)
                    .with_context(|| format!("no {stage} in {graph}"))
            })
            .collect::<Result<_>>()?;
        assert!(positions.is_sorted(), "{positions:?} in {graph}");
        Ok(())
    }

    #[test]
    fn hdr_dehaze_reaches_the_aces_chain() -> Result<()> {
        let (args, _) = plan(&with(&SINGLE, &["--profile", "d-log", "--dehaze", "0.5"]))?;
        assert_hdr_encode(&args, "18");
        let graph = value_after(&args, "-filter_complex").context("no graph")?;
        let stages = [
            "input='D-Log D-Gamut':output='ACEScg'",
            "exposure=exposure=0.828853:black=0.050000",
            "input='ACEScg':output='ACEScct'",
            "exposure=exposure=0.114798:black=0.053946",
            "colorchannelmixer=rr=1.254720:",
            "input='ACEScct':display='Rec.2100-HLG - Display'",
        ];
        let positions: Vec<usize> = stages
            .iter()
            .map(|stage| {
                graph
                    .find(stage)
                    .with_context(|| format!("no {stage} in {graph}"))
            })
            .collect::<Result<_>>()?;
        assert!(positions.is_sorted(), "{positions:?} in {graph}");
        Ok(())
    }

    // ---- HDR hard errors.

    const HLG: [&str; 4] = ["--input-color", "dji-dlog", "--output-color", "hlg"];

    #[test]
    fn hdr_rejects_everything_outside_the_aces_grade() {
        let cases: [(&[&str], &str); 14] = [
            (
                &["--stabilize"],
                "HDR output requires a 10-bit-safe pipeline. The current vidstab backend is 8-bit only. Use --no-stabilize.",
            ),
            (&["--dehaze", "1.5"], "Invalid --dehaze 1.5 for HDR output"),
            (
                &["--curves", "preset=lighter"],
                "--curves is not available with HDR output",
            ),
            (
                &["--vibrance", "0.3"],
                "--vibrance is not available with HDR output",
            ),
            (
                &["--selective-color", "reds=0.1:0:-0.1:0"],
                "--selective-color is not available with HDR output",
            ),
            (
                &["--hue-shift", "5"],
                "--hue-shift is not available with HDR output",
            ),
            (
                &["--color-balance", "0.1:-0.1:0,0:0:0,-0.1:0:0.1"],
                "--color-balance is not available with HDR output",
            ),
            // A value that does not parse is refused just the same.
            (
                &["--color-balance", "oops"],
                "--color-balance is not available with HDR output",
            ),
            (
                &["--denoise", "3"],
                "--denoise is not available with HDR output",
            ),
            (
                &["--sharpen", "0.5"],
                "--sharpen is not available with HDR output",
            ),
            (&["--lut", "grade.cube"], "--lut-space acescct"),
            (&["--codec", "h264"], "HDR output requires HEVC Main 10"),
            (&["--codec", "prores"], "HDR output requires HEVC Main 10"),
            (
                &["--codec", "hevc_vaapi"],
                "only verified with the libx265 software encoder",
            ),
        ];
        for (flags, expected) in cases {
            // Explicit --output-color hlg: the error stands alone.
            let explicit = error_of(&with(&SINGLE, &with(&HLG, flags)));
            assert!(explicit.contains(expected), "{flags:?}: {explicit}");
            assert!(
                !explicit.contains("--output-color rec709"),
                "{flags:?}: {explicit}"
            );

            // Bare D-Log: same error, plus the way back to the old behaviour.
            let defaulted = error_of(&with(&SINGLE, &with(&["--profile", "d-log"], flags)));
            assert!(defaulted.contains(expected), "{flags:?}: {defaulted}");
            assert!(defaulted.contains(REC709_HINT), "{flags:?}: {defaulted}");

            // And the same flags are fine on the Rec.709 route.
            let rec709 = with(&["--profile", "d-log", "--output-color", "rec709"], flags);
            assert!(parse(&with(&SINGLE, &rec709)).is_ok(), "{flags:?}");
        }
    }

    #[test]
    fn hdr_rejects_presets_and_other_inputs() {
        for preset in ["mavic4pro-dlog", "youtube", "archive"] {
            let message = error_of(&with(&SINGLE, &with(&HLG, &["--preset", preset])));
            assert!(
                message.contains("Presets are Rec.709 grades"),
                "{preset}: {message}"
            );
        }
        for input in ["standard", "s-log", "c-log", "v-log", "f-log"] {
            let message = error_of(&with(
                &SINGLE,
                &["--input-color", input, "--output-color", "hlg"],
            ));
            assert!(
                message.contains("HDR output requires DJI D-Log/D-Gamut input"),
                "{input}: {message}"
            );
        }
        // Out-of-range grade values are refused rather than handed to ffmpeg.
        for bad in [
            ["--exposure", "4"],
            ["--contrast", "0.1"],
            ["--saturation", "3"],
        ] {
            let message = error_of(&with(&SINGLE, &with(&HLG, &bad)));
            assert!(message.contains("Invalid"), "{bad:?}: {message}");
        }
    }

    #[test]
    fn hdr_accepts_explicit_software_hevc_and_non_colour_flags() -> Result<()> {
        for codec in ["h265", "hevc", "libx265"] {
            let (args, _) = plan(&with(&SINGLE, &with(&HLG, &["--codec", codec])))?;
            assert_hdr_encode(&args, "18");
        }
        let (args, _) = plan(&with(
            &SINGLE,
            &with(
                &HLG,
                &[
                    "--bitrate",
                    "40",
                    "--threads",
                    "8",
                    "--hw-accel",
                    "--no-stabilize",
                ],
            ),
        ))?;
        assert_hdr_encode(&args, "18");
        assert_eq!(value_after(&args, "-b:v"), Some("40M"));
        assert_eq!(value_after(&args, "-threads"), Some("8"));
        Ok(())
    }

    #[test]
    fn hdr_only_flags_are_rejected_on_the_rec709_route() {
        for route in [
            &[][..],
            &["--profile", "d-log", "--output-color", "rec709"][..],
        ] {
            let exposure = error_of(&with(&SINGLE, &with(route, &["--exposure", "1"])));
            assert!(
                exposure.contains(
                    "--exposure is applied in ACEScg and is only available with HDR output"
                ),
                "{exposure}"
            );
            let lut_space = error_of(&with(
                &SINGLE,
                &with(route, &["--lut", "grade.cube", "--lut-space", "acescct"]),
            ));
            assert!(
                lut_space.contains("--lut-space only applies to HDR output"),
                "{lut_space}"
            );
        }
    }

    #[test]
    fn rec709_hint_is_only_added_on_the_defaulted_route() {
        let error = || anyhow::anyhow!("no ocio filter");
        assert_eq!(
            with_rec709_hint(error(), false).to_string(),
            "no ocio filter"
        );
        let hinted = with_rec709_hint(error(), true).to_string();
        assert!(hinted.starts_with("no ocio filter\n"), "{hinted}");
        assert!(hinted.contains("--output-color rec709"), "{hinted}");
    }

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
