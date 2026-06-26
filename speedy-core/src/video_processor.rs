use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};

use crate::{ColorProfile, FFmpegCommand, check_ffmpeg, get_video_info};

// Type alias for color balance values (shadows RGB, midtones RGB, highlights RGB)
type ColorBalanceValues = (f32, f32, f32, f32, f32, f32, f32, f32, f32);

pub struct VideoProcessor {
    /// One or more input clips. When more than one is given they are stitched
    /// together (in order) into a single output via the concat filter, with each
    /// clip normalized to a common resolution first.
    inputs: Vec<PathBuf>,
    output_path: PathBuf,
    speed_multiplier: f64,
    codec: String,
    bitrate: Option<u32>,
    quality: u8,
    contrast: f32,
    saturation: f32,
    profile: ColorProfile,
    lut_file: Option<PathBuf>,
    hw_accel: bool,
    threads: Option<usize>,
    stabilize: bool,
    auto_rotate: bool,
    denoise: Option<u8>,
    sharpen: Option<f32>,
    scale: Option<String>,
    vibrance: Option<f32>,
    curves: Option<String>,
    hue_shift: Option<f32>,
    color_balance: Option<ColorBalanceValues>,
    selective_color: Option<String>,
    /// Target output frame rate used when the speed is changed. `None` defaults
    /// to the source frame rate, which makes a speed-up drop frames instead of
    /// inflating the frame rate.
    output_fps: Option<String>,
}

impl VideoProcessor {
    pub fn new(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Self {
        Self::new_multi(vec![input.as_ref().to_path_buf()], output)
    }

    /// Create a processor that stitches multiple input clips into one output.
    /// The clips are concatenated in the order given.
    pub fn new_multi(inputs: Vec<PathBuf>, output: impl AsRef<Path>) -> Self {
        Self {
            inputs,
            output_path: output.as_ref().to_path_buf(),
            speed_multiplier: 1.0,
            codec: "libx264".to_string(),
            bitrate: None,
            quality: 23,
            contrast: 1.0,
            saturation: 1.0,
            profile: ColorProfile::Standard,
            lut_file: None,
            hw_accel: false,
            threads: None,
            stabilize: false,
            auto_rotate: true,
            denoise: None,
            sharpen: None,
            scale: None,
            vibrance: None,
            curves: None,
            hue_shift: None,
            color_balance: None,
            selective_color: None,
            output_fps: None,
        }
    }

    pub fn speed(mut self, multiplier: f64) -> Self {
        self.speed_multiplier = multiplier;
        self
    }

    /// Set the target output frame rate used when the speed is changed (e.g.
    /// `"30"` or `"30000/1001"`). Defaults to the source frame rate, so a
    /// speed-up drops frames rather than producing a higher-fps file.
    pub fn output_fps(mut self, fps: &str) -> Self {
        self.output_fps = Some(fps.to_string());
        self
    }

    pub fn codec(mut self, codec: &str) -> Self {
        self.codec = match codec {
            "h264" => "libx264",
            "h265" | "hevc" => "libx265",
            "vp9" => "libvpx-vp9",
            "av1" => "libaom-av1",
            "prores" => "prores_ks",
            other => other,
        }
        .to_string();
        self
    }

    pub fn bitrate(mut self, mbps: u32) -> Self {
        self.bitrate = Some(mbps);
        self
    }

    pub fn quality(mut self, crf: u8) -> Self {
        self.quality = crf;
        self
    }

    pub fn contrast(mut self, value: f32) -> Self {
        self.contrast = value;
        self
    }

    pub fn saturation(mut self, value: f32) -> Self {
        self.saturation = value;
        self
    }

    pub fn profile(mut self, profile: ColorProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn lut(mut self, lut_file: impl AsRef<Path>) -> Self {
        self.lut_file = Some(lut_file.as_ref().to_path_buf());
        self
    }

    pub fn hardware_accel(mut self, enabled: bool) -> Self {
        self.hw_accel = enabled;
        self
    }

    pub fn threads(mut self, count: usize) -> Self {
        self.threads = Some(count);
        self
    }

    pub fn stabilize(mut self, enabled: bool) -> Self {
        self.stabilize = enabled;
        self
    }

    pub fn auto_rotate(mut self, enabled: bool) -> Self {
        self.auto_rotate = enabled;
        self
    }

    pub fn denoise(mut self, strength: u8) -> Self {
        self.denoise = Some(strength);
        self
    }

    pub fn sharpen(mut self, strength: f32) -> Self {
        self.sharpen = Some(strength);
        self
    }

    pub fn scale(mut self, scale_str: &str) -> Self {
        self.scale = Some(scale_str.to_string());
        self
    }

    pub fn vibrance(mut self, intensity: f32) -> Self {
        self.vibrance = Some(intensity);
        self
    }

    pub fn curves(mut self, curves: &str) -> Self {
        self.curves = Some(curves.to_string());
        self
    }

    pub fn hue_shift(mut self, degrees: f32) -> Self {
        self.hue_shift = Some(degrees);
        self
    }

    pub fn color_balance_str(mut self, balance_str: &str) -> Self {
        // Parse color balance string format: "rs:gs:bs,rm:gm:bm,rh:gh:bh"
        let parts: Vec<&str> = balance_str.split(',').collect();
        if parts.len() == 3 {
            let shadows: Vec<f32> = parts[0].split(':').filter_map(|s| s.parse().ok()).collect();
            let midtones: Vec<f32> = parts[1].split(':').filter_map(|s| s.parse().ok()).collect();
            let highlights: Vec<f32> = parts[2].split(':').filter_map(|s| s.parse().ok()).collect();

            if shadows.len() == 3 && midtones.len() == 3 && highlights.len() == 3 {
                self.color_balance = Some((
                    shadows[0],
                    shadows[1],
                    shadows[2],
                    midtones[0],
                    midtones[1],
                    midtones[2],
                    highlights[0],
                    highlights[1],
                    highlights[2],
                ));
            }
        }
        self
    }

    pub fn selective_color(mut self, config: &str) -> Self {
        self.selective_color = Some(config.to_string());
        self
    }

    /// Get the appropriate LUT file for the color profile, if one is available.
    ///
    /// A missing profile LUT is not fatal: the LUT assets are not shipped with
    /// the repository (they are git-ignored), so we log a warning and skip the
    /// color conversion rather than aborting, letting the other preset
    /// adjustments still apply.
    fn get_profile_lut(&self) -> Option<PathBuf> {
        let (path, label) = match self.profile {
            ColorProfile::DLog => ("luts/mavic4_pro_dlog_to_rec709.cube", "D-Log"),
            ColorProfile::SLog => ("luts/sony_slog_to_rec709.cube", "S-Log"),
            ColorProfile::CLog => ("luts/canon_clog_to_rec709.cube", "C-Log"),
            _ => return None,
        };

        let lut_path = PathBuf::from(path);
        if lut_path.exists() {
            Some(lut_path)
        } else {
            log::warn!("{label} LUT not found at {path}; skipping color conversion");
            None
        }
    }

    /// Process the video using FFmpeg CLI
    pub fn process(&self) -> Result<()> {
        // Guard the indexing below: library callers can construct an empty
        // processor via `new_multi`, which the CLI never does.
        if self.inputs.is_empty() {
            anyhow::bail!("No input files provided");
        }

        // Check FFmpeg availability
        let ffmpeg_version = check_ffmpeg()?;
        log::info!("Using FFmpeg version: {}", ffmpeg_version);

        // Get video info from the first clip (all stitched clips are assumed to
        // share the same format, as they come from the same camera/source).
        log::info!("Analyzing input video...");
        let info = get_video_info(&self.inputs[0])?;
        log::info!(
            "Video info: {}x{}, {:.2} fps, {:.2}s duration, rotation: {}°, audio: {}",
            info.width,
            info.height,
            info.fps,
            info.duration,
            info.rotation,
            if info.has_audio { "yes" } else { "no" }
        );

        // When multiple clips are given, probe every clip so we can pick a
        // common output resolution and sum the durations (for the progress bar).
        let stitching = self.inputs.len() > 1;
        let stitch_plan = if stitching {
            let infos = self
                .inputs
                .iter()
                .map(get_video_info)
                .collect::<Result<Vec<_>>>()?;
            let total: f64 = infos.iter().map(|i| i.duration).sum();
            // Target the smallest display size across clips so nothing is
            // upscaled; clips of other sizes are scaled to fit and padded.
            let (width, height) = infos
                .iter()
                .map(display_dimensions)
                .reduce(|(aw, ah), (bw, bh)| (aw.min(bw), ah.min(bh)))
                .unwrap_or((info.width, info.height));
            log::info!(
                "Stitching {} clips ({total:.2}s total) at {width}x{height} into {:?}",
                self.inputs.len(),
                self.output_path
            );
            // Stitching currently produces a video-only output; warn loudly so
            // dropped audio is never a silent surprise.
            if infos.iter().any(|i| i.has_audio) {
                log::warn!(
                    "Some input clips have audio, but stitched output is video-only; audio will be dropped"
                );
            }
            Some((width, height, total))
        } else {
            None
        };

        // Build FFmpeg command. In stitch mode all inputs are passed together;
        // otherwise just the single clip.
        let mut cmd = if stitch_plan.is_some() {
            FFmpegCommand::new_multi(self.inputs.clone(), &self.output_path)
        } else {
            FFmpegCommand::new(&self.inputs[0], &self.output_path)
        }
        .video_codec(&self.codec)
        .quality(self.quality)
        .overwrite()
        .preserve_metadata();

        if let Some((width, height, total)) = stitch_plan {
            // Probe the first video stream's frame rate specifically, so a file
            // whose first stream is audio/data does not feed a bogus fps into
            // the concat graph.
            let fps = probe_video_fps(&self.inputs[0], info.fps);
            cmd = cmd
                .concat_normalize(width, height, &fps)
                .total_duration(total);
        }

        // Set bitrate if specified
        if let Some(bitrate) = self.bitrate {
            cmd = cmd.bitrate(bitrate);
        }

        // Set threads if specified
        if let Some(threads) = self.threads {
            cmd = cmd.threads(threads);
        }

        // Hardware acceleration
        if self.hw_accel {
            // Try to detect best hardware acceleration method
            #[cfg(target_os = "linux")]
            {
                cmd = cmd.hardware_accel("vaapi");
            }
            #[cfg(target_os = "macos")]
            {
                cmd = cmd.hardware_accel("videotoolbox");
            }
            #[cfg(target_os = "windows")]
            {
                cmd = cmd.hardware_accel("dxva2");
            }
        }

        // Apply speed adjustment. Resample to a target frame rate (the source
        // fps unless overridden) so a speed-up yields a shorter clip at a normal
        // frame rate instead of re-encoding every source frame at a multiplied
        // fps. A degenerate/unknown source fps falls back to no resampling.
        if self.speed_multiplier != 1.0 {
            let target_fps = match &self.output_fps {
                Some(fps) => {
                    // Validate an explicit override up front so a bad value
                    // (e.g. `--output-fps 0` or `0/0`) fails fast with a clear
                    // message instead of as a late, cryptic ffmpeg error.
                    if fps_string_value(fps).is_none() {
                        anyhow::bail!(
                            "Invalid --output-fps {fps:?}; expected a positive number like \"30\" or \"30000/1001\""
                        );
                    }
                    Some(fps.clone())
                }
                None => {
                    // Default to the source's average cadence, which is the
                    // correct decimation target for variable-frame-rate inputs.
                    let probed = probe_target_fps(&self.inputs[0], info.fps);
                    fps_string_value(&probed).map(|_| probed)
                }
            };
            if let Some(ref fps) = target_fps {
                log::info!(
                    "Resampling to {fps} fps after a {speed}x speed change",
                    speed = self.speed_multiplier
                );
            }
            cmd = cmd.speed(self.speed_multiplier, info.has_audio, target_fps.as_deref());
        }

        // Apply LUT if specified or use profile LUT
        if let Some(ref lut) = self.lut_file {
            cmd = cmd.lut3d(lut);
        } else if let Some(profile_lut) = self.get_profile_lut() {
            log::info!(
                "Applying {} profile LUT: {}",
                self.profile.to_string(),
                profile_lut.display()
            );
            cmd = cmd.lut3d(profile_lut);
        }

        // Apply color adjustments
        if self.contrast != 1.0 || self.saturation != 1.0 {
            cmd = cmd.color_enhance(self.contrast, self.saturation);
        }

        // Apply stabilization if requested
        if self.stabilize {
            log::info!("Applying video stabilization");
            cmd = cmd.stabilize();
        }

        // Handle rotation
        if self.auto_rotate {
            cmd = cmd.auto_rotate();
        } else if info.rotation != 0 {
            // Manual rotation based on metadata
            match info.rotation {
                90 => cmd = cmd.rotate(1),        // 90° clockwise
                -90 | 270 => cmd = cmd.rotate(0), // 90° counter-clockwise
                180 => cmd = cmd.rotate(2),       // 180°
                _ => {}
            }
        }

        // Apply denoising if requested
        if let Some(strength) = self.denoise {
            cmd = cmd.denoise(strength);
        }

        // Apply sharpening if requested
        if let Some(strength) = self.sharpen {
            cmd = cmd.sharpen(strength);
        }

        // Apply vibrance if requested
        if let Some(vibrance) = self.vibrance {
            cmd = cmd.vibrance(vibrance);
        }

        // Apply curves if requested
        if let Some(ref curves) = self.curves {
            cmd = cmd.curves(curves);
        }

        // Apply hue shift if requested
        if let Some(hue_shift) = self.hue_shift {
            cmd = cmd.hue_shift(hue_shift);
        }

        // Apply color balance if requested
        if let Some(balance) = self.color_balance {
            cmd = cmd.color_balance(
                (balance.0, balance.1, balance.2),
                (balance.3, balance.4, balance.5),
                (balance.6, balance.7, balance.8),
            );
        }

        // Apply selective color if requested
        if let Some(ref selective) = self.selective_color {
            cmd = cmd.selective_color(selective);
        }

        // Apply scaling if requested
        if let Some(ref scale_str) = self.scale {
            // Parse scale string (e.g., "1920x1080" or "1920:-1")
            if let Some((width_str, height_str)) = scale_str.split_once('x') {
                if let Ok(width) = width_str.parse::<i32>() {
                    let height = height_str.parse::<i32>().unwrap_or(-1);
                    cmd = cmd.scale(width, height);
                }
            } else if let Some((width_str, height_str)) = scale_str.split_once(':')
                && let Ok(width) = width_str.parse::<i32>()
            {
                let height = height_str.parse::<i32>().unwrap_or(-1);
                cmd = cmd.scale(width, height);
            }
        }

        // Set up progress bar
        let pb = ProgressBar::new(100);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}% {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );

        // Execute FFmpeg with progress tracking
        let pb_clone = pb.clone();
        cmd.execute(move |progress, message| {
            pb_clone.set_position(progress as u64);
            if progress >= 100.0 {
                pb_clone.finish_with_message("Processing complete!");
            } else {
                pb_clone.set_message(message);
            }
        })?;

        log::info!("Video processing completed successfully!");
        log::info!("Output saved to: {:?}", self.output_path);

        Ok(())
    }
}

/// Probe the first *video* stream's base frame rate (`r_frame_rate`) as an
/// ffmpeg-ready string (e.g. `"30000/1001"`). Falls back to the formatted
/// `default` when the value is missing or degenerate (e.g. a non-video first
/// stream reporting `0/0`). Used to set a common CFR cadence when stitching.
fn probe_video_fps(path: &Path, default: f64) -> String {
    probe_stream_rate(path, "r_frame_rate").unwrap_or_else(|| format!("{default:.5}"))
}

/// Probe the decimation target for a speed change: the first video stream's
/// average cadence (`avg_frame_rate`), falling back to the base `r_frame_rate`
/// and then the formatted `default`. The average rate is the right target for
/// variable-frame-rate sources — there `r_frame_rate` is only a timebase guess
/// and can be far higher than the real cadence, which would otherwise keep too
/// many frames after a speed-up.
fn probe_target_fps(path: &Path, default: f64) -> String {
    probe_stream_rate(path, "avg_frame_rate")
        .or_else(|| probe_stream_rate(path, "r_frame_rate"))
        .unwrap_or_else(|| format!("{default:.5}"))
}

/// Read a single rational rate entry (`r_frame_rate` or `avg_frame_rate`) for
/// the first video stream, returning it verbatim only when it is a positive
/// rational (`num > 0 && den > 0`); otherwise `None`.
fn probe_stream_rate(path: &Path, entry: &str) -> Option<String> {
    let show_entries = format!("stream={entry}");
    let rate = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            show_entries.as_str(),
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())?;

    if let Some((num, den)) = rate.split_once('/')
        && let (Ok(num), Ok(den)) = (num.parse::<f64>(), den.parse::<f64>())
        && num > 0.0
        && den > 0.0
    {
        Some(rate)
    } else {
        None
    }
}

/// Parse an ffmpeg frame-rate string (`"30000/1001"` or `"29.97"`) into a
/// positive float, returning `None` when it is missing, malformed, or
/// non-positive. Used to reject a degenerate source fps before feeding it to the
/// `fps` filter (where `fps=0` would be invalid).
fn fps_string_value(s: &str) -> Option<f64> {
    if let Some((num, den)) = s.split_once('/') {
        let num: f64 = num.trim().parse().ok()?;
        let den: f64 = den.trim().parse().ok()?;
        if num > 0.0 && den > 0.0 {
            Some(num / den)
        } else {
            None
        }
    } else {
        let value: f64 = s.trim().parse().ok()?;
        (value > 0.0).then_some(value)
    }
}

/// Display dimensions of a clip, accounting for a 90°/270° rotation flag
/// (cameras often store rotated footage with a rotation tag).
fn display_dimensions(info: &crate::VideoInfo) -> (u32, u32) {
    if info.rotation.abs() % 180 == 90 {
        (info.height, info.width)
    } else {
        (info.width, info.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VideoInfo;

    fn info(width: u32, height: u32, rotation: i32) -> VideoInfo {
        VideoInfo {
            duration: 0.0,
            width,
            height,
            fps: 30.0,
            rotation,
            has_audio: false,
        }
    }

    #[test]
    fn display_dimensions_swap_on_quarter_turns_only() {
        // Upright / half-turn: dimensions stay as stored.
        assert_eq!(display_dimensions(&info(3840, 2160, 0)), (3840, 2160));
        assert_eq!(display_dimensions(&info(3840, 2160, 180)), (3840, 2160));
        // Quarter turns (camera stored the frame rotated): width/height swap.
        assert_eq!(display_dimensions(&info(3384, 6016, -90)), (6016, 3384));
        assert_eq!(display_dimensions(&info(3384, 6016, 90)), (6016, 3384));
        assert_eq!(display_dimensions(&info(3384, 6016, 270)), (6016, 3384));
    }

    #[test]
    fn missing_profile_lut_degrades_to_none() {
        // LUT assets are git-ignored and not shipped, so a log profile whose
        // LUT is absent must skip color conversion (None) rather than abort.
        let lut = PathBuf::from("luts/mavic4_pro_dlog_to_rec709.cube");
        if lut.exists() {
            // Skip when a developer happens to have the LUT present locally.
            return;
        }
        let processor = VideoProcessor::new("in.mp4", "out.mp4").profile(crate::ColorProfile::DLog);
        assert_eq!(processor.get_profile_lut(), None);
    }

    #[test]
    fn fps_string_value_parses_rational_and_decimal() {
        assert_eq!(fps_string_value("30000/1001"), Some(30000.0 / 1001.0));
        assert_eq!(fps_string_value("30"), Some(30.0));
        assert_eq!(fps_string_value("60.0"), Some(60.0));
        // Degenerate or malformed rates are rejected so they never reach `fps=`.
        assert_eq!(fps_string_value("0/0"), None);
        assert_eq!(fps_string_value("0"), None);
        assert_eq!(fps_string_value("abc"), None);
    }

    #[test]
    fn output_fps_builder_sets_target() {
        let processor = VideoProcessor::new("in.mp4", "out.mp4").output_fps("60");
        assert_eq!(processor.output_fps.as_deref(), Some("60"));
        // Unset by default, so the source fps is used.
        let default = VideoProcessor::new("in.mp4", "out.mp4");
        assert_eq!(default.output_fps, None);
    }

    #[test]
    fn process_rejects_empty_inputs() {
        // A library caller can build an empty processor; it must return an
        // error rather than panicking on input indexing.
        let result = VideoProcessor::new_multi(Vec::new(), "out.mp4").process();
        assert!(result.is_err(), "empty inputs should error, not panic");
    }
}
