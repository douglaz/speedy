use anyhow::{Context, Result, bail, ensure};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};

use crate::color::{self, HdrGrade, SourceYuv};
use crate::stabilize::{self, VidstabParams};
use crate::{
    ColorPipeline, FFmpegCommand, InputColor, LutSpace, OutputColor, VideoInfo, WorkingColor,
    check_ffmpeg, get_video_info,
};

// Type alias for color balance values (shadows RGB, midtones RGB, highlights RGB)
type ColorBalanceValues = (f32, f32, f32, f32, f32, f32, f32, f32, f32);

/// x265 CRF used for HDR output when no quality is set: the result is an
/// upload master that the hosting platform re-encodes.
const HDR_DEFAULT_QUALITY: u8 = 18;
/// CRF used for Rec.709 output when no quality is set.
const SDR_DEFAULT_QUALITY: u8 = 23;

/// DJI's conversion LUTs from `luts/`, compiled in so installed binaries
/// (release archives, the Nix package) have them without a checkout.
const EMBEDDED_LUTS: [(&str, &str); 2] = [
    (
        "mavic4_pro_dlog_to_rec709.cube",
        include_str!("../../luts/mavic4_pro_dlog_to_rec709.cube"),
    ),
    (
        "dji_dlogm_to_rec709.cube",
        include_str!("../../luts/dji_dlogm_to_rec709.cube"),
    ),
];

/// Everything `ffprobe` (and the filesystem) contributes to a command, gathered
/// up front so that [`VideoProcessor::plan`] itself is a pure function.
#[derive(Debug, Clone)]
pub struct ProbedInputs {
    /// One entry per input clip, in order.
    pub infos: Vec<VideoInfo>,
    /// Common frame rate that stitched clips are normalized to.
    pub stitch_fps: String,
    /// Frame rate a speed change is resampled to, if any.
    pub target_fps: Option<String>,
    /// Rec.709 conversion LUT for the input colour, when one is installed.
    pub profile_lut: Option<PathBuf>,
}

pub struct VideoProcessor {
    /// One or more input clips. When more than one is given they are stitched
    /// together (in order) into a single output via the concat filter, with each
    /// clip normalized to a common resolution first.
    inputs: Vec<PathBuf>,
    output_path: PathBuf,
    speed_multiplier: f64,
    /// `None` picks the default for the output colour (libx264 for Rec.709,
    /// libx265 for HDR).
    codec: Option<String>,
    bitrate: Option<u32>,
    /// `None` picks the default for the output colour.
    quality: Option<u8>,
    contrast: f32,
    saturation: f32,
    color: ColorPipeline,
    /// Exposure in stops, applied in ACEScg. HDR route only.
    exposure: Option<f32>,
    lut_file: Option<PathBuf>,
    /// Colour space the LUT is declared to work in. Required for a LUT on the
    /// HDR route, where an undeclared (Rec.709) LUT would be wrong.
    lut_space: Option<LutSpace>,
    /// Set when a preset configured this processor.
    preset_applied: bool,
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
    /// Set whenever a colour balance was asked for, even if its value did not
    /// parse: HDR output refuses the option itself, not just a valid value.
    color_balance_requested: bool,
    selective_color: Option<String>,
    /// Target output frame rate used when the speed is changed. `None` defaults
    /// to the source frame rate, which makes a speed-up drop frames instead of
    /// inflating the frame rate.
    output_fps: Option<String>,
    /// Haze-removal strength (~0.5 medium, 1.0 strong). `None` disables it.
    dehaze: Option<f32>,
    /// vidstab smoothing window (frames) used when `stabilize` is set. `None`
    /// uses the tuned default.
    stabilize_smoothing: Option<u32>,
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
            codec: None,
            bitrate: None,
            quality: None,
            contrast: 1.0,
            saturation: 1.0,
            color: ColorPipeline::default(),
            exposure: None,
            lut_file: None,
            lut_space: None,
            preset_applied: false,
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
            color_balance_requested: false,
            selective_color: None,
            output_fps: None,
            dehaze: None,
            stabilize_smoothing: None,
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

    /// Enable haze removal at the given strength (~0.5 medium, 1.0 strong).
    /// Pulls the black point, adds contrast, and restores saturation/vibrance.
    pub fn dehaze(mut self, strength: f32) -> Self {
        self.dehaze = Some(strength);
        self
    }

    /// Set the vidstab smoothing window (frames) used when stabilization is
    /// enabled. Higher is a glassier glide; lower follows the camera more.
    pub fn stabilize_smoothing(mut self, frames: u32) -> Self {
        self.stabilize_smoothing = Some(frames);
        self
    }

    pub fn codec(mut self, codec: &str) -> Self {
        self.codec = Some(
            match codec {
                "h264" => "libx264",
                "h265" | "hevc" => "libx265",
                "vp9" => "libvpx-vp9",
                "av1" => "libaom-av1",
                "prores" => "prores_ks",
                other => other,
            }
            .to_string(),
        );
        self
    }

    pub fn bitrate(mut self, mbps: u32) -> Self {
        self.bitrate = Some(mbps);
        self
    }

    pub fn quality(mut self, crf: u8) -> Self {
        self.quality = Some(crf);
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

    /// Declare what the source footage is encoded as.
    pub fn input_color(mut self, input: InputColor) -> Self {
        self.color.input = input;
        self
    }

    /// Scene-referred working space of the HDR route.
    pub fn working_color(mut self, working: WorkingColor) -> Self {
        self.color.working = working;
        self
    }

    /// Choose the delivery colour. Rec.709 (the default) keeps the
    /// display-referred route; Rec.2100 HLG takes the ACES route and needs an
    /// ffmpeg with the `ocio` filter.
    pub fn output_color(mut self, output: OutputColor) -> Self {
        self.color.output = output;
        self
    }

    pub fn color_pipeline(mut self, color: ColorPipeline) -> Self {
        self.color = color;
        self
    }

    /// The colour pipeline as configured so far.
    pub fn color(&self) -> ColorPipeline {
        self.color
    }

    /// Exposure compensation in stops, applied as a linear gain in ACEScg.
    /// Only available with HDR output.
    pub fn exposure(mut self, stops: f32) -> Self {
        self.exposure = Some(stops);
        self
    }

    /// Declare the colour space the LUT works in (see [`LutSpace`]).
    pub fn lut_space(mut self, space: LutSpace) -> Self {
        self.lut_space = Some(space);
        self
    }

    pub(crate) fn mark_preset_applied(mut self) -> Self {
        self.preset_applied = true;
        self
    }

    fn effective_codec(&self) -> &str {
        match &self.codec {
            Some(codec) => codec,
            None if self.color.is_hdr() => "libx265",
            None => "libx264",
        }
    }

    fn effective_quality(&self) -> u8 {
        self.quality.unwrap_or(if self.color.is_hdr() {
            HDR_DEFAULT_QUALITY
        } else {
            SDR_DEFAULT_QUALITY
        })
    }

    /// Check the configuration without touching any file or running ffmpeg.
    ///
    /// The HDR route is deliberately narrow: only operators that are defined in
    /// ACES and run on float RGB are allowed, and everything tuned for a
    /// Rec.709 image is refused rather than silently applied to the wrong
    /// signal.
    pub fn validate(&self) -> Result<()> {
        if !self.color.is_hdr() {
            ensure!(
                self.exposure.is_none(),
                "--exposure is applied in ACEScg and is only available with HDR output (--output-color hlg)"
            );
            ensure!(
                self.lut_space.is_none(),
                "--lut-space only applies to HDR output (--output-color hlg); Rec.709 output applies the LUT as-is"
            );
            return Ok(());
        }

        ensure!(
            self.color.input == InputColor::DjiDLogDGamut,
            "HDR output requires DJI D-Log/D-Gamut input (--input-color dji-dlog); {label} input has no HDR route",
            label = self.color.input.label()
        );
        ensure!(
            !self.preset_applied,
            "Presets are Rec.709 grades and cannot be used with HDR output"
        );
        ensure!(
            !self.stabilize,
            "HDR output requires a 10-bit-safe pipeline. The current vidstab backend is 8-bit only. Use --no-stabilize."
        );
        if let Some(strength) = self.dehaze {
            ensure!(
                (0.0..=1.0).contains(&strength),
                "Invalid --dehaze {strength} for HDR output; must be between 0.0 and 1.0"
            );
        }
        let rec709_only = [
            ("--curves", self.curves.is_some()),
            ("--vibrance", self.vibrance.is_some()),
            ("--selective-color", self.selective_color.is_some()),
            ("--hue-shift", self.hue_shift.is_some()),
            ("--color-balance", self.color_balance_requested),
            ("--denoise", self.denoise.is_some()),
            ("--sharpen", self.sharpen.is_some()),
        ];
        for (flag, set) in rec709_only {
            ensure!(
                !set,
                "{flag} is not available with HDR output: the HDR grade is limited to --exposure, --contrast, --saturation, --dehaze and an ACEScct --lut"
            );
        }
        ensure!(
            self.lut_file.is_none() || self.lut_space == Some(LutSpace::AcesCct),
            "--lut with HDR output must be an ACEScct LUT declared with --lut-space acescct; a Rec.709 LUT cannot be used on the HDR route"
        );

        let codec = self.effective_codec();
        if codec != "libx265" {
            if codec.contains("hevc") || codec.contains("265") {
                bail!(
                    "HDR output is only verified with the libx265 software encoder; {codec} is not supported. Use --codec h265."
                );
            }
            bail!("HDR output requires HEVC Main 10 (--codec h265); {codec} cannot carry it");
        }

        if let Some(stops) = self.exposure {
            ensure!(
                stops.is_finite() && stops.abs() <= 3.0,
                "Invalid --exposure {stops}; must be between -3 and 3 stops"
            );
        }
        ensure!(
            (0.3..=2.0).contains(&self.contrast),
            "Invalid --contrast {contrast} for HDR output; must be between 0.3 and 2.0",
            contrast = self.contrast
        );
        ensure!(
            (0.0..=2.0).contains(&self.saturation),
            "Invalid --saturation {saturation} for HDR output; must be between 0.0 and 2.0",
            saturation = self.saturation
        );
        Ok(())
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
        self.color_balance_requested = true;
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
                return self;
            }
        }
        log::warn!(
            "Ignoring malformed --color-balance {balance_str:?}; expected \"rs:gs:bs,rm:gm:bm,rh:gh:bh\""
        );
        self
    }

    pub fn selective_color(mut self, config: &str) -> Self {
        self.selective_color = Some(config.to_string());
        self
    }

    /// Get the appropriate LUT file for the color profile, if one is available.
    ///
    /// A `luts/` folder in the working directory wins; otherwise the DJI LUTs
    /// come from the copies compiled into the binary. A missing profile LUT is
    /// not fatal: we log a warning and skip the color conversion rather than
    /// aborting, letting the other preset adjustments still apply.
    fn get_profile_lut(&self) -> Option<PathBuf> {
        let lut_path = self.profile_lut_path()?;
        if lut_path.exists() {
            return Some(lut_path);
        }
        let name = lut_path.file_name()?.to_str()?;
        if let Some(path) = embedded_lut(name, &user_cache_dir()) {
            return Some(path);
        }
        log::warn!(
            "{label} LUT not found at {path}; skipping color conversion",
            label = self.color.input.label(),
            path = lut_path.display()
        );
        None
    }

    /// The profile LUT to apply, looked up only when it would be used: an
    /// explicit `--lut` replaces it, and the HDR route never takes a Rec.709
    /// LUT.
    fn wanted_profile_lut(&self) -> Option<PathBuf> {
        if self.lut_file.is_some() || self.color.is_hdr() {
            return None;
        }
        self.get_profile_lut()
    }

    /// Where a local Rec.709 conversion LUT for the input colour is looked for
    /// (relative to the working directory), whether or not it is there.
    pub fn profile_lut_path(&self) -> Option<PathBuf> {
        let path = match self.color.input {
            InputColor::DjiDLogDGamut => "luts/mavic4_pro_dlog_to_rec709.cube",
            InputColor::DjiDLogM => "luts/dji_dlogm_to_rec709.cube",
            InputColor::SLog => "luts/sony_slog_to_rec709.cube",
            InputColor::CLog => "luts/canon_clog_to_rec709.cube",
            _ => return None,
        };
        Some(PathBuf::from(path))
    }

    /// Process the video using FFmpeg CLI
    pub fn process(&self) -> Result<()> {
        // Guard the indexing below: library callers can construct an empty
        // processor via `new_multi`, which the CLI never does.
        if self.inputs.is_empty() {
            anyhow::bail!("No input files provided");
        }

        // Reject a speed that would produce garbage or hang: setpts=inf and an
        // infinite atempo chaining loop for 0 / negative / non-finite speeds.
        validate_speed(self.speed_multiplier)?;

        // Refuse an unsupported configuration before any ffmpeg work.
        self.validate()?;

        // Check FFmpeg availability
        let ffmpeg_version = check_ffmpeg()?;
        log::info!("Using FFmpeg version: {}", ffmpeg_version);
        if self.color.is_hdr() {
            color::ensure_ocio_filter()?;
        }

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

        // Smoothing only affects the stabilization path; warn if it's a no-op
        // here, where the effective stabilize state (incl. presets) is known.
        if self.stabilize_smoothing.is_some() && !self.stabilize {
            log::warn!("stabilize_smoothing has no effect without stabilization enabled");
        }

        // Stabilization needs a different pipeline (per-clip, two-pass vidstab),
        // so route it out before building the single stitch/grade command.
        if self.stabilize {
            return self.process_stabilized(&info);
        }

        // When multiple clips are given, probe every clip so we can pick a
        // common output resolution and sum the durations (for the progress bar).
        let infos = if self.inputs.len() > 1 {
            self.inputs
                .iter()
                .map(get_video_info)
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![info.clone()]
        };
        // Probe the first video stream's frame rate specifically, so a file
        // whose first stream is audio/data does not feed a bogus fps into
        // the concat graph.
        let stitch_fps = if infos.len() > 1 {
            probe_video_fps(&self.inputs[0], info.fps)
        } else {
            String::new()
        };
        let target_fps = self.resolve_target_fps(&info)?;
        let cmd = self.plan(&ProbedInputs {
            infos,
            stitch_fps,
            target_fps,
            profile_lut: self.wanted_profile_lut(),
        })?;

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

    /// Build the single ffmpeg command for an unstabilized run from already
    /// probed inputs. Pure: no file or process is touched, so the argument
    /// vector can be inspected (and is pinned by tests).
    pub fn plan(&self, probed: &ProbedInputs) -> Result<FFmpegCommand> {
        // `plan` is public: a bad speed must not reach the atempo chaining loop.
        validate_speed(self.speed_multiplier)?;
        self.validate()?;
        ensure!(
            probed.infos.len() == self.inputs.len() && !self.inputs.is_empty(),
            "Expected probe results for {inputs} input(s), got {infos}",
            inputs = self.inputs.len(),
            infos = probed.infos.len()
        );
        let info = &probed.infos[0];

        let stitch_plan = if self.inputs.len() > 1 {
            let infos = &probed.infos;
            let total: f64 = infos.iter().map(|i| i.duration).sum();
            // Target the smallest display size across clips so nothing is
            // upscaled; clips of other sizes are scaled to fit and padded.
            let (width, height) = infos
                .iter()
                .map(|i| target_dimensions(i, self.auto_rotate))
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
        // otherwise just the single clip. Use absolute input/output paths so the
        // LUT working-directory trick (see apply_grade) can't redirect them.
        let abs_inputs: Vec<PathBuf> = self.inputs.iter().map(|p| absolutize(p)).collect();
        let abs_output = absolutize(&self.output_path);
        let mut cmd = if stitch_plan.is_some() {
            FFmpegCommand::new_multi(abs_inputs, &abs_output)
        } else {
            FFmpegCommand::new(&abs_inputs[0], &abs_output)
        }
        .video_codec(self.effective_codec())
        .quality(self.effective_quality())
        .overwrite()
        .preserve_metadata();

        if let Some((width, height, total)) = stitch_plan {
            cmd = cmd
                .concat_normalize(width, height, &probed.stitch_fps)
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

        let target_fps = probed.target_fps.as_deref();
        if self.color.is_hdr() {
            self.apply_hdr_grade(cmd, &probed.infos, target_fps)
        } else {
            // Apply the grade: speed, LUT, dehaze, colour, rotation, scaling, etc.
            Ok(self.apply_grade_with_lut(cmd, info, target_fps, probed.profile_lut.clone()))
        }
    }

    /// The HDR route: geometry on the camera's own 10-bit YUV first (so the
    /// float colour work only touches pixels that survive), then the ACES chain
    /// from [`color::hdr_filters`], then explicit HLG signaling.
    fn apply_hdr_grade(
        &self,
        mut cmd: FFmpegCommand,
        infos: &[VideoInfo],
        target_fps: Option<&str>,
    ) -> Result<FFmpegCommand> {
        let info = &infos[0];
        let source =
            SourceYuv::from_tags(info.color_space.as_deref(), info.color_range.as_deref())?;
        // Stitched clips share one conversion to RGB after the join.
        for other in &infos[1..] {
            ensure!(
                SourceYuv::from_tags(other.color_space.as_deref(), other.color_range.as_deref())?
                    == source,
                "Stitched clips must share one YUV matrix and range for HDR output"
            );
        }
        if infos.len() > 1 {
            // Left to itself the concat filter converts every clip to the
            // first one's format and tags, guessing BT.601 for an untagged one.
            cmd = cmd.concat_input_pin(&color::stitch_input_pin(source));
        }

        if self.speed_multiplier != 1.0 {
            cmd = cmd.speed(self.speed_multiplier, info.has_audio, target_fps);
        }
        if !self.auto_rotate {
            cmd = cmd.disable_autorotate();
        }
        if let Some(ref scale_str) = self.scale {
            let Some((width, height)) = parse_scale(scale_str) else {
                bail!(
                    "Malformed --scale {scale_str:?}; expected e.g. \"1920x1080\" or \"1920:-1\""
                );
            };
            cmd = cmd.scale(width, height);
        }

        // Reference the LUT by basename from its own directory, as on the
        // Rec.709 route (see apply_grade_with_lut).
        let lut_name = match &self.lut_file {
            Some(lut) => {
                if let Some(parent) = lut.parent().filter(|p| !p.as_os_str().is_empty()) {
                    cmd = cmd.current_dir(absolutize(parent));
                }
                let name = lut
                    .file_name()
                    .with_context(|| format!("Invalid LUT path: {path}", path = lut.display()))?;
                Some(name.to_string_lossy().into_owned())
            }
            None => None,
        };

        let grade = HdrGrade {
            exposure: self.exposure.unwrap_or(color::HLG_DEFAULT_EXPOSURE),
            contrast: self.contrast,
            saturation: self.saturation,
            dehaze: self.dehaze.unwrap_or(0.0),
            lut: lut_name.as_deref(),
        };
        for filter in color::hdr_filters(self.color.working, source, &grade) {
            cmd = cmd.video_filter(&filter);
        }
        Ok(cmd.custom_args(color::hlg_output_args()))
    }

    /// Resolve the decimation target frame rate for a speed change. `None` when
    /// the speed is unchanged or the source fps cannot be determined. Errors on
    /// an explicit but invalid `--output-fps`.
    fn resolve_target_fps(&self, info: &crate::VideoInfo) -> Result<Option<String>> {
        if self.speed_multiplier == 1.0 {
            return Ok(None);
        }
        let target = match &self.output_fps {
            Some(fps) => {
                if fps_string_value(fps).is_none() {
                    anyhow::bail!(
                        "Invalid --output-fps {fps:?}; expected a positive number like \"30\" or \"30000/1001\""
                    );
                }
                Some(fps.clone())
            }
            None => {
                let probed = probe_target_fps(&self.inputs[0], info.fps);
                fps_string_value(&probed).map(|_| probed)
            }
        };
        Ok(target)
    }

    /// Apply the colour/speed/geometry grade — everything except stitch
    /// normalization and stabilization — to a command in a fixed order. Shared
    /// by the single-command path and the per-clip stabilization path.
    fn apply_grade(
        &self,
        cmd: FFmpegCommand,
        info: &crate::VideoInfo,
        target_fps: Option<&str>,
    ) -> FFmpegCommand {
        self.apply_grade_with_lut(cmd, info, target_fps, self.wanted_profile_lut())
    }

    /// [`apply_grade`](Self::apply_grade) with the profile LUT lookup already
    /// done by the caller.
    fn apply_grade_with_lut(
        &self,
        mut cmd: FFmpegCommand,
        info: &crate::VideoInfo,
        target_fps: Option<&str>,
        profile_lut: Option<PathBuf>,
    ) -> FFmpegCommand {
        // Speed (resampled to the target fps so a speed-up drops frames).
        if self.speed_multiplier != 1.0 {
            if let Some(fps) = target_fps {
                log::info!(
                    "Resampling to {fps} fps after a {speed}x speed change",
                    speed = self.speed_multiplier
                );
            }
            cmd = cmd.speed(self.speed_multiplier, info.has_audio, target_fps);
        }

        // LUT (explicit, else from the colour profile). Run ffmpeg from the
        // LUT's directory and reference it by basename, so a path with
        // colons/backslashes/commas (Windows drives, odd dirs) isn't mis-parsed
        // as filtergraph syntax. Input/output paths are absolute, so changing
        // the working directory is safe.
        let lut = self.lut_file.clone().or(profile_lut);
        if let Some(lut) = lut {
            if self.lut_file.is_none() {
                log::info!(
                    "Applying {} profile LUT: {}",
                    self.color.input.label(),
                    lut.display()
                );
            }
            match (
                lut.parent().filter(|p| !p.as_os_str().is_empty()),
                lut.file_name(),
            ) {
                (Some(parent), Some(name)) => {
                    cmd = cmd.current_dir(absolutize(parent)).lut3d(name);
                }
                _ => cmd = cmd.lut3d(&lut),
            }
        }

        // Dehaze (after the LUT, so it grades the Rec.709 image).
        if let Some(strength) = self.dehaze
            && strength > 0.0
        {
            log::info!("Applying dehaze (strength {strength})");
            cmd = cmd.dehaze(strength);
        }

        // Contrast / saturation.
        if self.contrast != 1.0 || self.saturation != 1.0 {
            cmd = cmd.color_enhance(self.contrast, self.saturation);
        }

        // Rotation: ffmpeg autorotates by default. `--no-auto-rotate` disables
        // that (`-noautorotate`) so footage keeps its stored orientation. We do
        // NOT also transpose — that double-rotated, because autorotation stayed
        // active.
        if !self.auto_rotate {
            cmd = cmd.disable_autorotate();
        }

        if let Some(strength) = self.denoise {
            cmd = cmd.denoise(strength);
        }
        if let Some(strength) = self.sharpen {
            cmd = cmd.sharpen(strength);
        }
        if let Some(vibrance) = self.vibrance {
            cmd = cmd.vibrance(vibrance);
        }
        if let Some(ref curves) = self.curves {
            cmd = cmd.curves(curves);
        }
        if let Some(hue_shift) = self.hue_shift {
            cmd = cmd.hue_shift(hue_shift);
        }
        if let Some(balance) = self.color_balance {
            cmd = cmd.color_balance(
                (balance.0, balance.1, balance.2),
                (balance.3, balance.4, balance.5),
                (balance.6, balance.7, balance.8),
            );
        }
        if let Some(ref selective) = self.selective_color {
            cmd = cmd.selective_color(selective);
        }
        if let Some(ref scale_str) = self.scale {
            match parse_scale(scale_str) {
                Some((width, height)) => cmd = cmd.scale(width, height),
                None => log::warn!(
                    "Ignoring malformed --scale {scale_str:?}; expected e.g. \"1920x1080\" or \"1920:-1\""
                ),
            }
        }
        cmd
    }

    /// Stabilize with two-pass `vidstab`. When stitching, each clip is graded
    /// and stabilized independently before concatenation, so smoothing never
    /// crosses a cut (no artificial pan at boundaries). Motion is detected on a
    /// brightness-normalized copy so exposure (EV) changes don't induce shake.
    /// Stabilized output is video-only.
    fn process_stabilized(&self, info: &crate::VideoInfo) -> Result<()> {
        if self.hw_accel {
            log::warn!(
                "--hw-accel is not applied on the stabilization path; grade/detect/transform use the software codec"
            );
        }
        let params = VidstabParams {
            smoothing: self
                .stabilize_smoothing
                .unwrap_or(VidstabParams::default().smoothing),
            ..VidstabParams::default()
        };
        let target_fps = self.resolve_target_fps(info)?;
        // High-quality intermediates so the extra encode generation before the
        // warp does not visibly degrade the grade.
        let inter_q = self.effective_quality().min(16);

        // Unique per-call temp dir: the pid alone collides across concurrent
        // VideoProcessor runs in one process, which would clobber intermediates.
        let nonce = STAB_RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "speedy-stab-{pid}-{nonce}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&tmp)
            .with_context(|| format!("Failed to create temp dir {}", tmp.display()))?;

        let result = self.run_stabilize(info, &tmp, &params, target_fps.as_deref(), inter_q);

        if let Err(e) = std::fs::remove_dir_all(&tmp) {
            log::debug!("could not clean temp dir {tmp}: {e}", tmp = tmp.display());
        }
        result?;

        log::info!("Video processing completed successfully!");
        log::info!("Output saved to: {:?}", self.output_path);
        Ok(())
    }

    /// Inner stabilization driver (grade -> detect -> transform [-> concat]),
    /// writing intermediates under `tmp`.
    fn run_stabilize(
        &self,
        info: &crate::VideoInfo,
        tmp: &Path,
        params: &VidstabParams,
        target_fps: Option<&str>,
        inter_q: u8,
    ) -> Result<()> {
        // Final-encode settings, mirrored so --bitrate/--threads are honored.
        let enc = stabilize::EncodeOpts {
            codec: self.effective_codec(),
            quality: self.effective_quality(),
            bitrate: self.bitrate,
            threads: self.threads,
        };
        // Matroska intermediates accept every codec speedy supports (incl.
        // ProRes/VP9/AV1), unlike an `.mp4` intermediate.
        if self.inputs.len() == 1 {
            log::info!(
                "Stabilizing (two-pass vidstab, smoothing={})",
                params.smoothing
            );
            let graded = tmp.join("graded_0.mkv");
            let mut clip_info = info.clone();
            clip_info.has_audio = false;
            let mut cmd = FFmpegCommand::new(absolutize(&self.inputs[0]), &graded)
                .video_codec(self.effective_codec())
                .quality(inter_q)
                .video_only()
                .overwrite();
            if let Some(threads) = self.threads {
                cmd = cmd.threads(threads);
            }
            self.apply_grade(cmd, &clip_info, target_fps)
                .execute(|_, _| {})?;
            let trf = tmp.join("t_0.trf");
            stabilize::detect(&graded, &trf, params, RETRY_ATTEMPTS)?;
            stabilize::transform(
                &graded,
                &self.output_path,
                &trf,
                &enc,
                params,
                RETRY_ATTEMPTS,
            )?;
            return Ok(());
        }

        // Stitch + stabilize: grade and stabilize each clip independently.
        let infos = self
            .inputs
            .iter()
            .map(get_video_info)
            .collect::<Result<Vec<_>>>()?;
        let (width, height) = infos
            .iter()
            .map(|i| target_dimensions(i, self.auto_rotate))
            .reduce(|(aw, ah), (bw, bh)| (aw.min(bw), ah.min(bh)))
            .unwrap_or((info.width, info.height));
        log::info!(
            "Stabilizing {count} clips per-segment at {width}x{height} (two-pass vidstab, smoothing={smoothing})",
            count = self.inputs.len(),
            smoothing = params.smoothing
        );
        if infos.iter().any(|i| i.has_audio) {
            log::warn!(
                "Some clips have audio, but stabilized stitched output is video-only; audio will be dropped"
            );
        }
        // Normalize every segment to a common frame rate so the stream-copy
        // concat sees matching time bases (mirrors the non-stabilized path).
        let common_fps = probe_video_fps(&self.inputs[0], info.fps);

        let mut segments = Vec::with_capacity(self.inputs.len());
        for (i, clip) in self.inputs.iter().enumerate() {
            log::info!(
                "Segment {n}/{total}: grade + stabilize",
                n = i + 1,
                total = self.inputs.len()
            );
            let mut clip_info = infos[i].clone();
            clip_info.has_audio = false;
            let graded = tmp.join(format!("graded_{i}.mkv"));
            let mut cmd = FFmpegCommand::new(absolutize(clip), &graded)
                .video_codec(self.effective_codec())
                .quality(inter_q)
                .video_only()
                .overwrite()
                .scale_pad(width, height, &common_fps);
            if let Some(threads) = self.threads {
                cmd = cmd.threads(threads);
            }
            self.apply_grade(cmd, &clip_info, target_fps)
                .execute(|_, _| {})?;
            let trf = tmp.join(format!("t_{i}.trf"));
            stabilize::detect(&graded, &trf, params, RETRY_ATTEMPTS)?;
            let stab = tmp.join(format!("stab_{i}.mkv"));
            stabilize::transform(&graded, &stab, &trf, &enc, params, RETRY_ATTEMPTS)?;
            segments.push(stab);
        }
        stabilize::concat(&segments, &self.output_path)
    }
}

/// Number of attempts for each stabilization ffmpeg pass before giving up.
/// `vidstab`/encoder crashes can be intermittent, leaving a truncated file; we
/// retry until the pass validates rather than trusting one exit code.
const RETRY_ATTEMPTS: u32 = 6;

/// Per-process counter making each stabilization run's temp dir unique, so
/// concurrent `process()` calls in one process don't clobber each other.
static STAB_RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

/// Make a path absolute (without requiring it to exist), falling back to the
/// path as-is. Lets us change ffmpeg's working directory for the LUT/vidstab
/// path tricks without redirecting relative input/output paths.
fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Path of the compiled-in LUT `name` under `cache/speedy/luts`, written there
/// first when missing or different (ffmpeg's `lut3d` reads a file). `None`
/// when no LUT of that name is compiled in or the file cannot be written.
fn embedded_lut(name: &str, cache: &Path) -> Option<PathBuf> {
    let (_, content) = EMBEDDED_LUTS.iter().find(|(n, _)| *n == name)?;
    let dir = cache.join("speedy").join("luts");
    let path = dir.join(name);
    if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == *content) {
        return Some(path);
    }
    // Write to a per-process name and rename, so concurrent runs never read a
    // half-written LUT.
    let tmp = dir.join(format!("{name}.{pid}.tmp", pid = std::process::id()));
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&tmp, content))
        .and_then(|()| std::fs::rename(&tmp, &path));
    match written {
        Ok(()) => Some(path),
        Err(e) => {
            log::warn!(
                "Could not write the built-in {name} to {dir}: {e}",
                dir = dir.display()
            );
            None
        }
    }
}

/// The per-user cache directory: `$XDG_CACHE_HOME`, `~/.cache`, or
/// `%LOCALAPPDATA%`, else the system temp dir.
fn user_cache_dir() -> PathBuf {
    let var = |key| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CACHE_HOME")
        .or_else(|| var("HOME").map(|home| home.join(".cache")))
        .or_else(|| var("LOCALAPPDATA"))
        .unwrap_or_else(std::env::temp_dir)
}

/// Validate a speed multiplier. `1.0` (no-op) is fine; otherwise it must be
/// finite and positive, or `setpts` becomes inf/NaN and the audio `atempo`
/// chaining loop can spin forever.
fn validate_speed(multiplier: f64) -> Result<()> {
    if multiplier != 1.0 && (!multiplier.is_finite() || multiplier <= 0.0) {
        anyhow::bail!("Invalid speed {multiplier}; must be a positive, finite number");
    }
    Ok(())
}

/// Parse a `--scale` spec (`"WxH"` or `"W:H"`; `-1` = auto height). Returns
/// `None` for malformed input so the caller can warn instead of silently
/// dropping it.
fn parse_scale(spec: &str) -> Option<(i32, i32)> {
    let (w, h) = spec.split_once('x').or_else(|| spec.split_once(':'))?;
    let width: i32 = w.trim().parse().ok()?;
    // -1 (auto) is valid and parses fine; a non-numeric height is malformed.
    let height: i32 = h.trim().parse().ok()?;
    Some((width, height))
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

/// The frame size the scale/pad target should match for stitching. With
/// autorotation on (default), filters see the rotated display frame, so use
/// display dimensions. With `--no-auto-rotate`, ffmpeg keeps the stored frame,
/// so use the stored dimensions — otherwise a rotated clip is scaled/padded into
/// a swapped canvas and comes out sideways and letterboxed.
fn target_dimensions(info: &crate::VideoInfo, auto_rotate: bool) -> (u32, u32) {
    if auto_rotate {
        display_dimensions(info)
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
            color_space: None,
            color_range: None,
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
        // No S-Log LUT is shipped or compiled in, so the profile must skip
        // color conversion (None) rather than abort.
        let lut = PathBuf::from("luts/sony_slog_to_rec709.cube");
        if lut.exists() {
            // Skip when a developer happens to have the LUT present locally.
            return;
        }
        let processor =
            VideoProcessor::new("in.mp4", "out.mp4").input_color(crate::InputColor::SLog);
        assert_eq!(processor.get_profile_lut(), None);
    }

    #[test]
    fn embedded_lut_is_written_once_and_repaired() -> Result<()> {
        let cache =
            std::env::temp_dir().join(format!("speedy-lut-test-{pid}", pid = std::process::id()));
        let (name, content) = EMBEDDED_LUTS[1];
        let path = embedded_lut(name, &cache).context("embedded LUT not written")?;
        assert_eq!(path, cache.join("speedy/luts/dji_dlogm_to_rec709.cube"));
        assert_eq!(std::fs::read_to_string(&path)?, content);
        // A stale or tampered copy is replaced, not trusted.
        std::fs::write(&path, "LUT_3D_SIZE 2\n")?;
        embedded_lut(name, &cache).context("embedded LUT not rewritten")?;
        assert_eq!(std::fs::read_to_string(&path)?, content);
        assert_eq!(embedded_lut("sony_slog_to_rec709.cube", &cache), None);
        std::fs::remove_dir_all(&cache)?;
        Ok(())
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
    fn dehaze_builder_sets_strength() {
        let processor = VideoProcessor::new("in.mp4", "out.mp4").dehaze(0.5);
        assert_eq!(processor.dehaze, Some(0.5));
        // Off by default.
        assert_eq!(VideoProcessor::new("in.mp4", "out.mp4").dehaze, None);
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
    fn stabilize_smoothing_builder_sets_field() {
        let p = VideoProcessor::new("in.mp4", "out.mp4").stabilize_smoothing(40);
        assert_eq!(p.stabilize_smoothing, Some(40));
        assert_eq!(
            VideoProcessor::new("in.mp4", "out.mp4").stabilize_smoothing,
            None
        );
    }

    #[test]
    fn resolve_target_fps_is_none_when_speed_unchanged() -> Result<()> {
        // Default speed is 1.0, so there is no decimation target.
        let p = VideoProcessor::new("in.mp4", "out.mp4");
        assert_eq!(p.resolve_target_fps(&info(3840, 2160, 0))?, None);
        Ok(())
    }

    #[test]
    fn resolve_target_fps_rejects_invalid_override() {
        let p = VideoProcessor::new("in.mp4", "out.mp4")
            .speed(10.0)
            .output_fps("0");
        assert!(
            p.resolve_target_fps(&info(3840, 2160, 0)).is_err(),
            "an invalid --output-fps must error"
        );
    }

    #[test]
    fn apply_grade_orders_lut_before_dehaze() {
        // The dehaze must grade the Rec.709 image, i.e. run after the LUT.
        let p = VideoProcessor::new("in.mp4", "out.mp4")
            .lut("grade.cube")
            .dehaze(0.5);
        let built = p
            .apply_grade(
                crate::FFmpegCommand::new("in.mp4", "out.mp4"),
                &info(3840, 2160, 0),
                None,
            )
            .build();
        let args: Vec<String> = built
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let idx = args
            .iter()
            .position(|a| a == "-filter_complex")
            .expect("expected -filter_complex");
        let fc = &args[idx + 1];
        let lut_at = fc.find("lut3d=file='grade.cube'").expect("lut present");
        let dehaze_at = fc.find("curves=all=").expect("dehaze present");
        assert!(lut_at < dehaze_at, "lut must precede dehaze: {fc}");
    }

    #[test]
    fn target_dimensions_uses_stored_dims_when_autorotate_off() {
        // -90 clip: stored portrait 3384x6016, displays landscape 6016x3384.
        let rotated = info(3384, 6016, -90);
        assert_eq!(target_dimensions(&rotated, true), (6016, 3384)); // display
        assert_eq!(target_dimensions(&rotated, false), (3384, 6016)); // stored
        // Unrotated clip is identical either way.
        let upright = info(3840, 2160, 0);
        assert_eq!(target_dimensions(&upright, true), (3840, 2160));
        assert_eq!(target_dimensions(&upright, false), (3840, 2160));
    }

    #[test]
    fn validate_speed_accepts_positive_finite_rejects_bad() {
        for ok in [1.0, 2.0, 0.5, 10.0] {
            assert!(validate_speed(ok).is_ok(), "{ok} should be ok");
        }
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(validate_speed(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn parse_scale_parses_and_rejects() {
        assert_eq!(parse_scale("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_scale("1920:-1"), Some((1920, -1)));
        assert_eq!(parse_scale("1280x-1"), Some((1280, -1)));
        // No separator, or a non-numeric width/height: malformed.
        assert_eq!(parse_scale("1920"), None);
        assert_eq!(parse_scale("axb"), None);
        assert_eq!(parse_scale("1920xabc"), None);
    }

    #[test]
    fn absolutize_makes_relative_paths_absolute() {
        // A relative path becomes absolute (joined with cwd); cross-platform.
        assert!(absolutize(Path::new("rel/x.mp4")).is_absolute());
    }

    fn graph_of(cmd: &crate::FFmpegCommand) -> String {
        let built = cmd.build();
        let args: Vec<String> = built
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let idx = args
            .iter()
            .position(|a| a == "-filter_complex")
            .expect("expected -filter_complex");
        args[idx + 1].clone()
    }

    fn probed(infos: Vec<VideoInfo>) -> ProbedInputs {
        ProbedInputs {
            infos,
            stitch_fps: "30/1".to_string(),
            target_fps: Some("30/1".to_string()),
            profile_lut: None,
        }
    }

    #[test]
    fn plan_rejects_invalid_speed() {
        // Without the check, an input with audio spins forever in the atempo
        // chaining loop.
        let with_audio = VideoInfo {
            has_audio: true,
            ..info(1920, 1080, 0)
        };
        for bad in [0.0, -1.0, f64::NAN] {
            let result = VideoProcessor::new("in.mp4", "out.mp4")
                .speed(bad)
                .plan(&probed(vec![with_audio.clone()]));
            assert!(result.is_err(), "speed {bad} should be rejected by plan");
        }
    }

    #[test]
    fn malformed_color_balance_is_still_refused_with_hdr_output() {
        let hdr = |balance: &str| {
            VideoProcessor::new("in.mp4", "out.mp4")
                .input_color(crate::InputColor::DjiDLogDGamut)
                .output_color(crate::OutputColor::Rec2100Hlg)
                .color_balance_str(balance)
                .validate()
        };
        for balance in ["oops", "0.1:0:0,0:0:0,0:0:0.1"] {
            let error = hdr(balance).expect_err("HDR must refuse --color-balance");
            assert!(
                error
                    .to_string()
                    .contains("--color-balance is not available with HDR output"),
                "{balance}: {error}"
            );
        }
        // Rec.709 output keeps warning and ignoring a malformed value.
        let sdr = VideoProcessor::new("in.mp4", "out.mp4").color_balance_str("oops");
        assert!(sdr.validate().is_ok());
        assert_eq!(sdr.color_balance, None);
    }

    #[test]
    fn hdr_stitch_pins_every_input_to_the_resolved_matrix_and_10_bit() -> Result<()> {
        // An untagged clip resolves to BT.709 limited, the same as the tagged
        // one, so both must be read that way and neither narrowed to 8 bits.
        let tagged = VideoInfo {
            color_space: Some("bt709".to_string()),
            color_range: Some("tv".to_string()),
            ..info(1920, 1080, 0)
        };
        let hdr = |inputs: Vec<PathBuf>| {
            VideoProcessor::new_multi(inputs, "out.mp4")
                .input_color(crate::InputColor::DjiDLogDGamut)
                .output_color(crate::OutputColor::Rec2100Hlg)
        };
        let two = || vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")];
        let pin = "force_original_aspect_ratio=decrease:in_color_matrix=bt709:out_color_matrix=bt709:in_range=limited:out_range=limited,format=yuv420p10le|yuv422p10le|yuv444p10le,pad=";
        for infos in [
            vec![info(1920, 1080, 0), tagged.clone()],
            vec![tagged.clone(), tagged.clone()],
            vec![info(1920, 1080, 0), info(1920, 1080, 0)],
        ] {
            let graph = graph_of(&hdr(two()).plan(&probed(infos))?);
            assert_eq!(graph.matches(pin).count(), 2, "{graph}");
        }

        // Clips that resolve to different matrices are still refused.
        let bt2020 = VideoInfo {
            color_space: Some("bt2020nc".to_string()),
            ..tagged.clone()
        };
        assert!(
            hdr(two())
                .plan(&probed(vec![tagged.clone(), bt2020]))
                .is_err()
        );

        // A single HDR clip and a Rec.709 stitch carry no pin.
        let single =
            graph_of(&hdr(vec![PathBuf::from("a.mp4")]).plan(&probed(vec![tagged.clone()]))?);
        assert!(!single.contains("in_color_matrix"), "{single}");
        let sdr = VideoProcessor::new_multi(two(), "out.mp4")
            .plan(&probed(vec![info(1920, 1080, 0), tagged]))?;
        assert_eq!(
            graph_of(&sdr),
            "[0:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1,setpts=PTS-STARTPTS[v0];\
             [1:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30/1,setpts=PTS-STARTPTS[v1];\
             [v0][v1]concat=n=2:v=1[cat];[cat]format=yuv420p[v]"
        );
        Ok(())
    }

    #[test]
    fn process_rejects_empty_inputs() {
        // A library caller can build an empty processor; it must return an
        // error rather than panicking on input indexing.
        let result = VideoProcessor::new_multi(Vec::new(), "out.mp4").process();
        assert!(result.is_err(), "empty inputs should error, not panic");
    }
}
