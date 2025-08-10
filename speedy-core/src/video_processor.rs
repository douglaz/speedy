use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::{Path, PathBuf};

use crate::{ColorProfile, FFmpegCommand, check_ffmpeg, get_video_info};

// Type alias for color balance values (shadows RGB, midtones RGB, highlights RGB)
type ColorBalanceValues = (f32, f32, f32, f32, f32, f32, f32, f32, f32);

pub struct VideoProcessor {
    input_path: PathBuf,
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
}

impl VideoProcessor {
    pub fn new(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Self {
        Self {
            input_path: input.as_ref().to_path_buf(),
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
        }
    }

    pub fn speed(mut self, multiplier: f64) -> Self {
        self.speed_multiplier = multiplier;
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

    /// Get the appropriate LUT file for the color profile
    fn get_profile_lut(&self) -> Option<PathBuf> {
        match self.profile {
            ColorProfile::DLog => {
                // Check for built-in D-Log LUT
                let lut_path = PathBuf::from("luts/dji_dlog_to_rec709.cube");
                if lut_path.exists() {
                    Some(lut_path)
                } else {
                    log::warn!("D-Log LUT not found at luts/dji_dlog_to_rec709.cube");
                    None
                }
            }
            ColorProfile::SLog => {
                let lut_path = PathBuf::from("luts/sony_slog_to_rec709.cube");
                if lut_path.exists() {
                    Some(lut_path)
                } else {
                    None
                }
            }
            ColorProfile::CLog => {
                let lut_path = PathBuf::from("luts/canon_clog_to_rec709.cube");
                if lut_path.exists() {
                    Some(lut_path)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Process the video using FFmpeg CLI
    pub fn process(&self) -> Result<()> {
        // Check FFmpeg availability
        let ffmpeg_version = check_ffmpeg()?;
        log::info!("Using FFmpeg version: {}", ffmpeg_version);

        // Get video info
        log::info!("Analyzing input video...");
        let info = get_video_info(&self.input_path)?;
        log::info!(
            "Video info: {}x{}, {:.2} fps, {:.2}s duration, rotation: {}°, audio: {}",
            info.width,
            info.height,
            info.fps,
            info.duration,
            info.rotation,
            if info.has_audio { "yes" } else { "no" }
        );

        // Build FFmpeg command
        let mut cmd = FFmpegCommand::new(&self.input_path, &self.output_path)
            .video_codec(&self.codec)
            .quality(self.quality)
            .overwrite()
            .preserve_metadata();

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

        // Apply speed adjustment
        if self.speed_multiplier != 1.0 {
            cmd = cmd.speed(self.speed_multiplier, info.has_audio);
        }

        // Apply LUT if specified or use profile LUT
        if let Some(ref lut) = self.lut_file {
            cmd = cmd.lut3d(lut);
        } else if let Some(profile_lut) = self.get_profile_lut() {
            log::info!(
                "Applying {profile} profile LUT",
                profile = self.profile.to_string()
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
