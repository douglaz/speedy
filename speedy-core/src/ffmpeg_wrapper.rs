use anyhow::{Context, Result};
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// FFmpeg command builder with fluent interface
#[derive(Debug, Clone)]
pub struct FFmpegCommand {
    inputs: Vec<PathBuf>,
    output: PathBuf,
    video_filters: Vec<String>,
    audio_filters: Vec<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    bitrate: Option<u32>,
    quality: Option<u8>,
    preset: Option<String>,
    threads: Option<usize>,
    overwrite: bool,
    extra_args: Vec<String>,
    metadata_args: Vec<String>,
    hw_accel: Option<String>,
    /// When set (with multiple inputs), each input is normalized to this
    /// `(width, height, fps)` and concatenated via the concat filter so clips
    /// of differing resolution/orientation can be stitched into one output.
    concat_normalize: Option<(u32, u32, String)>,
    /// Known total duration in seconds, used for progress because the concat
    /// filter does not produce a single `Duration` line FFmpeg can report.
    total_duration: Option<f64>,
}

impl FFmpegCommand {
    pub fn new(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Self {
        Self::new_multi(vec![input.as_ref().to_path_buf()], output)
    }

    /// Create a command with multiple inputs. When combined with
    /// [`concat_normalize`](Self::concat_normalize) the inputs are stitched
    /// together into a single output.
    pub fn new_multi(inputs: Vec<PathBuf>, output: impl AsRef<Path>) -> Self {
        Self {
            inputs,
            output: output.as_ref().to_path_buf(),
            video_filters: Vec::new(),
            audio_filters: Vec::new(),
            video_codec: None,
            audio_codec: None,
            bitrate: None,
            quality: None,
            preset: None,
            threads: None,
            overwrite: false,
            extra_args: Vec::new(),
            metadata_args: Vec::new(),
            hw_accel: None,
            concat_normalize: None,
            total_duration: None,
        }
    }

    /// Stitch multiple inputs into one output: each input is scaled (preserving
    /// aspect, padded if needed), auto-rotated, set to `fps`, then concatenated.
    /// Any configured video filters (e.g. a LUT) are applied once after the join.
    ///
    /// Note: the stitched output is currently video-only; audio tracks are not
    /// concatenated.
    pub fn concat_normalize(mut self, width: u32, height: u32, fps: &str) -> Self {
        self.concat_normalize = Some((width, height, fps.to_string()));
        self
    }

    /// Provide a known total duration (seconds) for progress reporting.
    /// Needed for concat, where FFmpeg cannot report a single Duration line.
    pub fn total_duration(mut self, seconds: f64) -> Self {
        self.total_duration = Some(seconds);
        self
    }

    /// Set video codec
    pub fn video_codec(mut self, codec: &str) -> Self {
        self.video_codec = Some(codec.to_string());
        self
    }

    /// Set audio codec
    pub fn audio_codec(mut self, codec: &str) -> Self {
        self.audio_codec = Some(codec.to_string());
        self
    }

    /// Set video bitrate in Mbps
    pub fn bitrate(mut self, mbps: u32) -> Self {
        self.bitrate = Some(mbps);
        self
    }

    /// Set quality (CRF value, 0-51 for x264/x265)
    pub fn quality(mut self, crf: u8) -> Self {
        self.quality = Some(crf);
        self
    }

    /// Set encoding preset (ultrafast, fast, medium, slow, veryslow)
    pub fn preset(mut self, preset: &str) -> Self {
        self.preset = Some(preset.to_string());
        self
    }

    /// Set number of threads
    pub fn threads(mut self, count: usize) -> Self {
        self.threads = Some(count);
        self
    }

    /// Enable overwrite without asking
    pub fn overwrite(mut self) -> Self {
        self.overwrite = true;
        self
    }

    /// Enable hardware acceleration
    pub fn hardware_accel(mut self, method: &str) -> Self {
        self.hw_accel = Some(method.to_string());
        self
    }

    /// Add a video filter
    pub fn video_filter(mut self, filter: &str) -> Self {
        self.video_filters.push(filter.to_string());
        self
    }

    /// Add an audio filter
    pub fn audio_filter(mut self, filter: &str) -> Self {
        self.audio_filters.push(filter.to_string());
        self
    }

    /// Set video speed (affects both video and audio).
    ///
    /// `setpts` only rescales timestamps, so a speed-up on its own keeps every
    /// source frame and inflates the frame rate (a 10x speed-up of 30fps would
    /// emit a ~300fps file with all frames re-encoded). When `output_fps` is
    /// given, a trailing `fps` filter resamples the retimed stream back to that
    /// rate: a speed-up then drops frames (a shorter clip at a normal fps) and a
    /// slow-down duplicates them. Pass `None` to keep the raw retimed stream.
    ///
    /// If `has_audio` is false, only video speed is adjusted.
    pub fn speed(mut self, multiplier: f64, has_audio: bool, output_fps: Option<&str>) -> Self {
        if multiplier != 1.0 {
            // Video speed adjustment
            self.video_filters
                .push(format!("setpts={:.4}*PTS", 1.0 / multiplier));

            // Resample the retimed stream to a sane frame rate so the output fps
            // does not scale with the speed multiplier. Placed right after
            // setpts so any later per-frame filters (e.g. a LUT) only process
            // the frames that survive decimation.
            if let Some(fps) = output_fps {
                self.video_filters.push(format!("fps={fps}"));
            }

            // Audio speed adjustment (with pitch correction) - only if audio exists
            if has_audio {
                if (0.5..=2.0).contains(&multiplier) {
                    self.audio_filters.push(format!("atempo={:.4}", multiplier));
                } else {
                    // For speeds outside 0.5-2.0 range, chain multiple atempo filters
                    let mut current = multiplier;
                    while current > 2.0 {
                        self.audio_filters.push("atempo=2.0".to_string());
                        current /= 2.0;
                    }
                    if current > 1.0 {
                        self.audio_filters.push(format!("atempo={:.4}", current));
                    }

                    while current < 0.5 {
                        self.audio_filters.push("atempo=0.5".to_string());
                        current *= 2.0;
                    }
                    if current < 1.0 {
                        self.audio_filters.push(format!("atempo={:.4}", current));
                    }
                }
            }
        }
        self
    }

    /// Apply contrast adjustment
    pub fn contrast(mut self, value: f32) -> Self {
        self.video_filters.push(format!("eq=contrast={:.2}", value));
        self
    }

    /// Apply saturation adjustment
    pub fn saturation(mut self, value: f32) -> Self {
        self.video_filters
            .push(format!("eq=saturation={:.2}", value));
        self
    }

    /// Apply both contrast and saturation
    pub fn color_enhance(mut self, contrast: f32, saturation: f32) -> Self {
        self.video_filters.push(format!(
            "eq=contrast={:.2}:saturation={:.2}",
            contrast, saturation
        ));
        self
    }

    /// Apply 3D LUT
    pub fn lut3d(mut self, lut_file: impl AsRef<Path>) -> Self {
        // Don't add extra quotes - FFmpeg handles the path properly
        self.video_filters
            .push(format!("lut3d={}", lut_file.as_ref().display()));
        self
    }

    /// Apply video stabilization
    pub fn stabilize(mut self) -> Self {
        self.video_filters.push("deshake".to_string());
        self
    }

    /// Auto-rotate based on metadata
    pub fn auto_rotate(self) -> Self {
        // autorotate is enabled by default in FFmpeg
        // We don't need to add it explicitly
        self
    }

    /// Rotate video (0=90CCW, 1=90CW, 2=180)
    pub fn rotate(mut self, direction: u8) -> Self {
        match direction {
            0 => self.video_filters.push("transpose=0".to_string()),
            1 => self.video_filters.push("transpose=1".to_string()),
            2 => self
                .video_filters
                .push("transpose=2,transpose=2".to_string()),
            _ => {}
        }
        self
    }

    /// Scale video
    pub fn scale(mut self, width: i32, height: i32) -> Self {
        self.video_filters
            .push(format!("scale={}:{}", width, height));
        self
    }

    /// Crop video
    pub fn crop(mut self, width: u32, height: u32, x: u32, y: u32) -> Self {
        self.video_filters
            .push(format!("crop={}:{}:{}:{}", width, height, x, y));
        self
    }

    /// Apply denoising
    pub fn denoise(mut self, strength: u8) -> Self {
        self.video_filters.push(format!("nlmeans=s={}", strength));
        self
    }

    /// Apply sharpening
    pub fn sharpen(mut self, strength: f32) -> Self {
        self.video_filters.push(format!(
            "unsharp=5:5:{:.2}:5:5:{:.2}",
            strength,
            strength * 0.5
        ));
        self
    }

    /// Apply vibrance (intelligent saturation)
    pub fn vibrance(mut self, intensity: f32) -> Self {
        // FFmpeg vibrance filter: intensity range is typically -2 to 2
        // Positive values increase vibrance, negative decrease
        self.video_filters
            .push(format!("vibrance=intensity={:.2}", intensity));
        self
    }

    /// Apply a haze-removal grade ("dehaze") of the given strength.
    ///
    /// Atmospheric haze lifts the black point and washes out contrast and
    /// colour. There is no native ffmpeg dehaze filter, so this approximates one
    /// (DaVinci-style): pull the black point down with `curves` to remove the
    /// veil, add contrast/saturation with `eq` (nudging gamma up so the lifted
    /// blacks don't crush the midtones), then restore colour with `vibrance`.
    /// All amounts scale with `strength`, where ~0.5 is a balanced "medium" and
    /// 1.0 is strong; values are clamped to be non-negative.
    pub fn dehaze(mut self, strength: f32) -> Self {
        let s = strength.max(0.0);
        let black_point = 0.10 * s;
        let contrast = 1.0 + 0.15 * s;
        let saturation = 1.0 + 0.35 * s;
        let gamma = 1.0 + 0.06 * s;
        let vibrance = 0.9 * s;
        self.video_filters
            .push(format!("curves=all='{black_point:.3}/0 1/1'"));
        self.video_filters.push(format!(
            "eq=contrast={contrast:.3}:saturation={saturation:.3}:gamma={gamma:.3}"
        ));
        self.video_filters
            .push(format!("vibrance=intensity={vibrance:.3}"));
        self
    }

    /// Apply color curves
    pub fn curves(mut self, curves_str: &str) -> Self {
        // Curves can be preset names or custom curve definitions
        // Examples: "preset=lighter", "red='0/0 0.5/0.6 1/1'"
        self.video_filters.push(format!("curves={}", curves_str));
        self
    }

    /// Apply color balance (shadows, midtones, highlights)
    pub fn color_balance(
        mut self,
        shadows: (f32, f32, f32),
        midtones: (f32, f32, f32),
        highlights: (f32, f32, f32),
    ) -> Self {
        // Color balance filter adjusts RGB for shadows, midtones, and highlights
        // Values range from -1 to 1
        self.video_filters.push(format!(
            "colorbalance=rs={:.2}:gs={:.2}:bs={:.2}:rm={:.2}:gm={:.2}:bm={:.2}:rh={:.2}:gh={:.2}:bh={:.2}",
            shadows.0, shadows.1, shadows.2,
            midtones.0, midtones.1, midtones.2,
            highlights.0, highlights.1, highlights.2
        ));
        self
    }

    /// Apply hue shift
    pub fn hue_shift(mut self, degrees: f32) -> Self {
        // Hue shift in degrees, can be positive or negative
        self.video_filters.push(format!("hue=h={:.1}", degrees));
        self
    }

    /// Apply selective color adjustments
    pub fn selective_color(mut self, config: &str) -> Self {
        // Selective color allows adjustment of specific color ranges
        // Format: "reds=r:g:b:n,yellows=r:g:b:n,..."
        self.video_filters
            .push(format!("selectivecolor={}", config));
        self
    }

    /// Preserve metadata
    pub fn preserve_metadata(mut self) -> Self {
        self.metadata_args.push("-map_metadata".to_string());
        self.metadata_args.push("0".to_string());
        self.metadata_args.push("-movflags".to_string());
        self.metadata_args.push("use_metadata_tags".to_string());
        self
    }

    /// Add custom FFmpeg arguments
    pub fn custom_args(mut self, args: Vec<String>) -> Self {
        self.extra_args.extend(args);
        self
    }

    /// Pixel format the filtered stream is normalized to before encoding.
    /// ProRes needs 10-bit 4:2:2; web codecs (H.264/H.265/VP9/AV1) use 8-bit
    /// 4:2:0. This is what converts the RGB output of filters like lut3d back to
    /// something the encoder accepts.
    fn output_pixel_format(&self) -> &'static str {
        match self.video_codec.as_deref() {
            Some(codec) if codec.contains("prores") => "yuv422p10le",
            _ => "yuv420p",
        }
    }

    /// Build the FFmpeg command
    pub fn build(&self) -> Command {
        let mut cmd = Command::new("ffmpeg");

        // Global options
        if self.overwrite {
            cmd.arg("-y");
        }

        // Hardware acceleration (applies to the inputs that follow)
        if let Some(ref hw) = self.hw_accel {
            cmd.args(["-hwaccel", hw]);
        }

        // Input files (autorotation is enabled by default for each).
        // Pass the path as an OsStr so non-UTF-8 paths don't panic.
        for input in &self.inputs {
            cmd.arg("-i").arg(input);
        }

        let video_chain = self.video_filters.join(",");
        let out_fmt = self.output_pixel_format();

        if let Some((w, h, ref fps)) = self.concat_normalize {
            // Stitch mode: normalize every input to a common size/fps (scaling
            // down to fit and padding to keep aspect), concatenate them, then
            // apply the shared video filter chain (e.g. the LUT) once.
            let n = self.inputs.len();
            let mut graph = String::new();
            for i in 0..n {
                // setpts=PTS-STARTPTS rebases each segment to start at 0, which
                // the concat filter requires; otherwise clips with non-zero
                // start PTS (trimmed sources, MP4 edit lists) can produce gaps
                // or non-monotonic-timestamp failures.
                graph.push_str(&format!(
                    "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,\
                     pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},\
                     setpts=PTS-STARTPTS[v{i}];"
                ));
            }
            for i in 0..n {
                graph.push_str(&format!("[v{i}]"));
            }
            graph.push_str(&format!("concat=n={n}:v=1[cat]"));
            // Normalize to a codec-friendly pixel format: RGB-producing filters
            // such as lut3d would otherwise leave the stream as gbrp (planar
            // RGB), which many encoders/players cannot handle.
            if video_chain.is_empty() {
                graph.push_str(&format!(";[cat]format={out_fmt}[v]"));
            } else {
                graph.push_str(&format!(";[cat]{video_chain},format={out_fmt}[v]"));
            }

            cmd.arg("-filter_complex");
            cmd.arg(&graph);
            // Stitched clips are treated as video-only (no synchronized audio).
            cmd.args(["-map", "[v]"]);
        } else {
            // Single-input mode: apply video/audio filters to input 0.
            let mut filter_complex = String::new();
            let mut has_video_filters = false;
            let mut has_audio_filters = false;

            if !self.video_filters.is_empty() {
                // The trailing format guards against RGB-producing filters (e.g.
                // lut3d) leaving the output as gbrp, which breaks many encoders.
                filter_complex.push_str(&format!("[0:v]{video_chain},format={out_fmt}[v]"));
                has_video_filters = true;
            }

            if !self.audio_filters.is_empty() {
                if has_video_filters {
                    filter_complex.push_str("; ");
                }
                filter_complex.push_str(&format!("[0:a]{}[a]", self.audio_filters.join(",")));
                has_audio_filters = true;
            }

            if !filter_complex.is_empty() {
                cmd.arg("-filter_complex");
                cmd.arg(&filter_complex);

                // Map the filtered outputs
                if has_video_filters {
                    cmd.args(["-map", "[v]"]);
                } else {
                    cmd.args(["-map", "0:v?"]);
                }

                if has_audio_filters {
                    cmd.args(["-map", "[a]"]);
                } else {
                    cmd.args(["-map", "0:a?"]);
                }
            }
        }

        // Video codec
        if let Some(ref codec) = self.video_codec {
            cmd.args(["-c:v", codec]);
        }

        // Audio codec
        if let Some(ref codec) = self.audio_codec {
            cmd.args(["-c:a", codec]);
        }

        // Quality settings
        if let Some(crf) = self.quality {
            cmd.args(["-crf", &crf.to_string()]);
        }

        if let Some(bitrate) = self.bitrate {
            cmd.args(["-b:v", &format!("{}M", bitrate)]);
        }

        if let Some(ref preset) = self.preset {
            cmd.args(["-preset", preset]);
        }

        // Thread count
        if let Some(threads) = self.threads {
            cmd.args(["-threads", &threads.to_string()]);
        }

        // Metadata preservation
        for arg in &self.metadata_args {
            cmd.arg(arg);
        }

        // Extra arguments
        for arg in &self.extra_args {
            cmd.arg(arg);
        }

        // Output file (passed as an OsStr to support non-UTF-8 paths)
        cmd.arg(&self.output);

        cmd
    }

    /// Execute the FFmpeg command with progress tracking
    pub fn execute<F>(&self, progress_callback: F) -> Result<()>
    where
        F: Fn(f64, String) + Send + 'static,
    {
        let mut cmd = self.build();
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        log::info!("Executing FFmpeg command: {:?}", cmd);

        // Debug: print the exact command being run
        let cmd_string = format!("{:?}", cmd);
        log::debug!("Raw command: {}", cmd_string);

        let mut child = cmd.spawn().context("Failed to spawn FFmpeg process")?;

        // Set up progress monitoring
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stderr"))?;

        let (tx, rx) = mpsc::channel();

        // Use the caller-provided duration when available (e.g. concat input,
        // where FFmpeg cannot report a real Duration line).
        let total_override = self.total_duration;

        // Spawn thread to read stderr and parse progress
        let reader_thread = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let duration_regex = Regex::new(r"Duration: (\d{2}):(\d{2}):(\d{2})\.(\d{2})").unwrap();
            let progress_regex = Regex::new(r"time=(\d{2}):(\d{2}):(\d{2})\.(\d{2})").unwrap();

            let mut total_duration: Option<f64> = total_override;
            let mut all_output = String::new();

            for line in reader.lines().map_while(Result::ok) {
                all_output.push_str(&line);
                all_output.push('\n');

                // Parse total duration
                if total_duration.is_none()
                    && let Some(caps) = duration_regex.captures(&line)
                {
                    let hours: f64 = caps[1].parse().unwrap_or(0.0);
                    let minutes: f64 = caps[2].parse().unwrap_or(0.0);
                    let seconds: f64 = caps[3].parse().unwrap_or(0.0);
                    let centis: f64 = caps[4].parse().unwrap_or(0.0);
                    total_duration =
                        Some(hours * 3600.0 + minutes * 60.0 + seconds + centis / 100.0);
                }

                // Parse current progress
                if let Some(caps) = progress_regex.captures(&line) {
                    let hours: f64 = caps[1].parse().unwrap_or(0.0);
                    let minutes: f64 = caps[2].parse().unwrap_or(0.0);
                    let seconds: f64 = caps[3].parse().unwrap_or(0.0);
                    let centis: f64 = caps[4].parse().unwrap_or(0.0);
                    let current_time = hours * 3600.0 + minutes * 60.0 + seconds + centis / 100.0;

                    if let Some(duration) = total_duration {
                        let progress = (current_time / duration * 100.0).min(100.0);
                        let _ = tx.send((progress, line.clone()));
                    }
                }

                // Also send the raw line for debugging
                let _ = tx.send((0.0, line));
            }
            all_output
        });

        // Process progress updates
        thread::spawn(move || {
            while let Ok((progress, _message)) = rx.recv() {
                if progress > 0.0 {
                    progress_callback(progress, format!("Processing: {:.1}%", progress));
                }
            }
        });

        // Wait for FFmpeg to complete
        let status = child.wait().context("Failed to wait for FFmpeg process")?;

        // Wait for reader thread to finish and get all output
        let all_output = reader_thread
            .join()
            .unwrap_or_else(|_| String::from("Failed to get output"));

        if !status.success() {
            log::error!("FFmpeg failed with output:\n{}", all_output);
            anyhow::bail!(
                "FFmpeg failed with exit code: {:?}. Check logs for details.",
                status.code()
            );
        }

        Ok(())
    }
}

/// Check if FFmpeg is available and return version info
pub fn check_ffmpeg() -> Result<String> {
    let output = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .context("FFmpeg not found. Please install FFmpeg.")?;

    let version = String::from_utf8_lossy(&output.stdout);

    // Extract version number
    let version_regex = Regex::new(r"ffmpeg version (\S+)").unwrap();
    if let Some(caps) = version_regex.captures(&version) {
        Ok(caps[1].to_string())
    } else {
        Ok("unknown".to_string())
    }
}

/// Get video metadata using ffprobe
pub fn get_video_info(path: impl AsRef<Path>) -> Result<VideoInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path.as_ref())
        .output()
        .context("Failed to run ffprobe")?;

    let json = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output (simplified for now)
    // In production, use serde_json to properly parse

    // Extract basic info using regex (simplified)
    let duration_regex = Regex::new(r#""duration":\s*"(\d+\.\d+)""#).unwrap();
    let width_regex = Regex::new(r#""width":\s*(\d+)"#).unwrap();
    let height_regex = Regex::new(r#""height":\s*(\d+)"#).unwrap();
    let fps_regex = Regex::new(r#""r_frame_rate":\s*"(\d+)/(\d+)""#).unwrap();
    let rotation_regex = Regex::new(r#""rotation":\s*(-?\d+)"#).unwrap();
    let audio_regex = Regex::new(r#""codec_type":\s*"audio""#).unwrap();

    let duration = duration_regex
        .captures(&json)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0.0);

    let width = width_regex
        .captures(&json)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);

    let height = height_regex
        .captures(&json)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);

    let fps = fps_regex
        .captures(&json)
        .and_then(|c| {
            let num: f64 = c[1].parse().ok()?;
            let den: f64 = c[2].parse().ok()?;
            Some(num / den)
        })
        .unwrap_or(0.0);

    let rotation = rotation_regex
        .captures(&json)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);

    let has_audio = audio_regex.is_match(&json);

    Ok(VideoInfo {
        duration,
        width,
        height,
        fps,
        rotation,
        has_audio,
    })
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub rotation: i32,
    pub has_audio: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the built command's arguments as owned strings for inspection.
    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Extract the value passed to `-filter_complex`, if any.
    fn filter_complex(args: &[String]) -> Option<&String> {
        let idx = args.iter().position(|a| a == "-filter_complex")?;
        args.get(idx + 1)
    }

    fn has_pair(args: &[String], first: &str, second: &str) -> bool {
        args.windows(2).any(|w| w[0] == first && w[1] == second)
    }

    #[test]
    fn single_input_without_filters_has_no_filter_complex() {
        let args = args_of(&FFmpegCommand::new("in.mp4", "out.mp4").build());
        assert!(!args.iter().any(|a| a == "-filter_complex"));
        assert!(has_pair(&args, "-i", "in.mp4"));
    }

    #[test]
    fn single_input_with_lut_forces_yuv420p() -> Result<()> {
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .lut3d("grade.cube")
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        // The LUT runs in RGB; the chain must end in yuv420p for compatibility.
        assert_eq!(fc, "[0:v]lut3d=grade.cube,format=yuv420p[v]");
        assert!(has_pair(&args, "-map", "[v]"));
        Ok(())
    }

    #[test]
    fn concat_normalize_builds_join_graph() -> Result<()> {
        let inputs = vec![
            PathBuf::from("a.mp4"),
            PathBuf::from("b.mp4"),
            PathBuf::from("c.mp4"),
        ];
        let args = args_of(
            &FFmpegCommand::new_multi(inputs, "out.mp4")
                .lut3d("grade.cube")
                .concat_normalize(3840, 2160, "30")
                .build(),
        );

        // One -i per input.
        assert_eq!(args.iter().filter(|a| a.as_str() == "-i").count(), 3);

        let fc = filter_complex(&args).context("expected -filter_complex")?;
        for i in 0..3 {
            assert!(
                fc.contains(&format!(
                    "[{i}:v]scale=3840:2160:force_original_aspect_ratio=decrease"
                )),
                "missing normalize chain for input {i}: {fc}"
            );
        }
        // Each segment must be rebased to start at PTS 0 for the concat filter.
        assert!(fc.contains("setpts=PTS-STARTPTS[v0]"), "graph: {fc}");
        assert!(fc.contains("concat=n=3:v=1[cat]"), "graph: {fc}");
        assert!(
            fc.contains("[cat]lut3d=grade.cube,format=yuv420p[v]"),
            "graph: {fc}"
        );
        assert!(has_pair(&args, "-map", "[v]"));
        Ok(())
    }

    #[test]
    fn prores_codec_keeps_10bit_422_pixel_format() -> Result<()> {
        // ProRes does not support yuv420p; forcing it would degrade or fail.
        let args = args_of(
            &FFmpegCommand::new("in.mov", "out.mov")
                .video_codec("prores_ks")
                .lut3d("grade.cube")
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert!(fc.ends_with("format=yuv422p10le[v]"), "fc: {fc}");
        Ok(())
    }

    #[test]
    fn speed_up_with_output_fps_decimates_frames() -> Result<()> {
        // A 10x speed-up must retime via setpts AND resample to the target fps;
        // without the fps filter the output keeps every source frame at ~10x the
        // frame rate instead of becoming a shorter clip.
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .speed(10.0, false, Some("30"))
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert_eq!(fc, "[0:v]setpts=0.1000*PTS,fps=30,format=yuv420p[v]");
        Ok(())
    }

    #[test]
    fn speed_change_without_output_fps_keeps_raw_retimed_stream() -> Result<()> {
        // Back-compat: with no target fps only setpts is applied (no decimation).
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .speed(2.0, false, None)
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert_eq!(fc, "[0:v]setpts=0.5000*PTS,format=yuv420p[v]");
        Ok(())
    }

    #[test]
    fn speed_resample_runs_before_a_lut() -> Result<()> {
        // The fps decimation must precede the LUT so the LUT only grades the
        // frames that survive the speed-up.
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .speed(10.0, false, Some("30"))
                .lut3d("grade.cube")
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert_eq!(
            fc,
            "[0:v]setpts=0.1000*PTS,fps=30,lut3d=grade.cube,format=yuv420p[v]"
        );
        Ok(())
    }

    #[test]
    fn speed_up_retimes_audio_independently_of_video_fps() -> Result<()> {
        // Video gets setpts + fps; audio is pitch-corrected with chained atempo
        // (4x = 2.0 * 2.0) and is unaffected by the video frame-rate target.
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .speed(4.0, true, Some("30"))
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert!(
            fc.contains("[0:v]setpts=0.2500*PTS,fps=30,format=yuv420p[v]"),
            "fc: {fc}"
        );
        assert!(fc.contains("[0:a]atempo=2.0,atempo=2.0000[a]"), "fc: {fc}");
        Ok(())
    }

    #[test]
    fn dehaze_builds_blackpoint_contrast_vibrance_chain() -> Result<()> {
        // strength 0.5 -> black 0.05, contrast 1.075, sat 1.175, gamma 1.030,
        // vibrance 0.45. The black-point curve must come first (veil removal),
        // then eq, then vibrance.
        let args = args_of(&FFmpegCommand::new("in.mp4", "out.mp4").dehaze(0.5).build());
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert_eq!(
            fc,
            "[0:v]curves=all='0.050/0 1/1',eq=contrast=1.075:saturation=1.175:gamma=1.030,vibrance=intensity=0.450,format=yuv420p[v]"
        );
        Ok(())
    }

    #[test]
    fn dehaze_runs_after_a_lut() -> Result<()> {
        // Dehaze grades the Rec.709 image, so it must follow the LUT.
        let args = args_of(
            &FFmpegCommand::new("in.mp4", "out.mp4")
                .lut3d("grade.cube")
                .dehaze(1.0)
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert!(
            fc.starts_with("[0:v]lut3d=grade.cube,curves=all="),
            "fc: {fc}"
        );
        assert!(fc.ends_with("format=yuv420p[v]"), "fc: {fc}");
        Ok(())
    }

    #[test]
    fn concat_normalize_without_filters_still_outputs_yuv420p() -> Result<()> {
        let inputs = vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")];
        let args = args_of(
            &FFmpegCommand::new_multi(inputs, "out.mp4")
                .concat_normalize(1920, 1080, "30")
                .build(),
        );
        let fc = filter_complex(&args).context("expected -filter_complex")?;
        assert!(
            fc.contains("concat=n=2:v=1[cat];[cat]format=yuv420p[v]"),
            "graph: {fc}"
        );
        Ok(())
    }
}
