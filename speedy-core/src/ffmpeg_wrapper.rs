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
    input: PathBuf,
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
}

impl FFmpegCommand {
    pub fn new(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Self {
        Self {
            input: input.as_ref().to_path_buf(),
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
        }
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

    /// Set video speed (affects both video and audio)
    /// If has_audio is false, only video speed is adjusted
    pub fn speed(mut self, multiplier: f64, has_audio: bool) -> Self {
        if multiplier != 1.0 {
            // Video speed adjustment
            self.video_filters
                .push(format!("setpts={:.4}*PTS", 1.0 / multiplier));

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

    /// Build the FFmpeg command
    pub fn build(&self) -> Command {
        let mut cmd = Command::new("ffmpeg");

        // Global options
        if self.overwrite {
            cmd.arg("-y");
        }

        // Hardware acceleration
        if let Some(ref hw) = self.hw_accel {
            cmd.args(["-hwaccel", hw]);
        }

        // Input file (autorotate is enabled by default)
        cmd.args(["-i", self.input.to_str().unwrap()]);

        // Build filter complex if we have filters
        let mut filter_complex = String::new();
        let mut has_video_filters = false;
        let mut has_audio_filters = false;

        if !self.video_filters.is_empty() {
            filter_complex.push_str(&format!("[0:v]{}[v]", self.video_filters.join(",")));
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

        // Output file
        cmd.arg(self.output.to_str().unwrap());

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

        // Spawn thread to read stderr and parse progress
        let reader_thread = thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let duration_regex = Regex::new(r"Duration: (\d{2}):(\d{2}):(\d{2})\.(\d{2})").unwrap();
            let progress_regex = Regex::new(r"time=(\d{2}):(\d{2}):(\d{2})\.(\d{2})").unwrap();

            let mut total_duration: Option<f64> = None;
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
            path.as_ref().to_str().unwrap(),
        ])
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
