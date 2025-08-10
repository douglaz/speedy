use crate::{ColorProfile, VideoProcessor};

/// Preset configurations for common video processing workflows
pub enum Preset {
    /// DJI drone footage with D-Log profile
    DjiDLog,
    /// DJI drone footage with standard profile
    DjiStandard,
    /// GoPro action camera footage
    GoPro,
    /// Sony camera with S-Log
    SonySLog,
    /// Canon camera with C-Log
    CanonCLog,
    /// Social media - Instagram
    Instagram,
    /// Social media - YouTube
    YouTube,
    /// Social media - TikTok
    TikTok,
    /// Cinema 4K export
    Cinema4K,
    /// Fast preview (lower quality, faster processing)
    FastPreview,
    /// High quality archival
    Archive,
    /// Natural color enhancement using vibrance
    NaturalEnhance,
    /// Cinematic teal and orange look
    CinematicTealOrange,
    /// Portrait mode with skin tone protection
    Portrait,
}

impl Preset {
    /// Apply preset to a video processor
    pub fn apply(&self, processor: VideoProcessor) -> VideoProcessor {
        match self {
            Preset::DjiDLog => {
                // DJI D-Log footage processing with vibrance instead of saturation
                processor
                    .profile(ColorProfile::DLog)
                    .contrast(1.15)
                    .vibrance(0.3) // Use vibrance for more natural color enhancement
                    .auto_rotate(true)
                    .stabilize(true)
                    .codec("h265")
                    .quality(20)
            }

            Preset::DjiStandard => {
                // DJI standard footage processing
                processor
                    .contrast(1.05)
                    .saturation(1.05)
                    .auto_rotate(true)
                    .stabilize(true)
                    .codec("h265")
                    .quality(22)
            }

            Preset::GoPro => {
                // GoPro footage with typical corrections
                processor
                    .contrast(1.1)
                    .saturation(1.15)
                    .stabilize(true)
                    .sharpen(0.5)
                    .codec("h264")
                    .quality(22)
            }

            Preset::SonySLog => {
                // Sony S-Log footage
                processor
                    .profile(ColorProfile::SLog)
                    .contrast(1.2)
                    .saturation(1.15)
                    .codec("h265")
                    .quality(20)
            }

            Preset::CanonCLog => {
                // Canon C-Log footage
                processor
                    .profile(ColorProfile::CLog)
                    .contrast(1.18)
                    .saturation(1.12)
                    .codec("h265")
                    .quality(20)
            }

            Preset::Instagram => {
                // Instagram optimized (1080x1080 square, high quality)
                processor
                    .codec("h264")
                    .quality(20)
                    .bitrate(5)
                    .contrast(1.1)
                    .saturation(1.2)
                // Note: Would need to add crop/scale in FFmpegCommand for square aspect
            }

            Preset::YouTube => {
                // YouTube optimized (high quality, good compression)
                processor
                    .codec("h264")
                    .quality(18)
                    .bitrate(16)
                    .contrast(1.05)
                    .saturation(1.05)
            }

            Preset::TikTok => {
                // TikTok optimized (vertical video, moderate quality)
                processor
                    .codec("h264")
                    .quality(23)
                    .bitrate(4)
                    .contrast(1.15)
                    .saturation(1.25)
            }

            Preset::Cinema4K => {
                // Cinema 4K export (very high quality)
                processor
                    .codec("prores")
                    .quality(0)
                    .contrast(1.0)
                    .saturation(1.0)
            }

            Preset::FastPreview => {
                // Fast preview (lower quality, faster processing)
                processor.codec("h264").quality(28).threads(1)
            }

            Preset::Archive => {
                // High quality archival
                processor
                    .codec("h265")
                    .quality(16)
                    .contrast(1.0)
                    .saturation(1.0)
            }

            Preset::NaturalEnhance => {
                // Natural color enhancement using vibrance
                processor
                    .vibrance(0.5) // Intelligent saturation that protects skin tones
                    .contrast(1.05)
                    .curves("preset=lighter") // Slightly lighter overall
                    .codec("h264")
                    .quality(20)
            }

            Preset::CinematicTealOrange => {
                // Cinematic teal and orange color grading
                processor
                    .curves("blue='0/0 0.5/0.58 1/1':red='0/0 0.5/0.42 1/1'") // Push blues toward teal, reds toward orange
                    .vibrance(0.3)
                    .contrast(1.1)
                    .color_balance_str("-0.05:0.05:0.1,0:0:-0.05,0.05:-0.05:-0.1") // Teal shadows, orange highlights
                    .codec("h265")
                    .quality(19)
            }

            Preset::Portrait => {
                // Portrait mode with skin tone protection
                processor
                    .vibrance(0.4) // Enhances colors while protecting skin tones
                    .curves("preset=lighter") // Brighten overall
                    .selective_color("reds=0:-0.05:0.05:0") // Subtle skin tone adjustment
                    .contrast(1.02)
                    .codec("h264")
                    .quality(20)
            }
        }
    }

    /// Get preset from string name
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "dji-dlog" | "dji_dlog" => Some(Preset::DjiDLog),
            "dji" | "dji-standard" => Some(Preset::DjiStandard),
            "gopro" => Some(Preset::GoPro),
            "sony-slog" | "slog" => Some(Preset::SonySLog),
            "canon-clog" | "clog" => Some(Preset::CanonCLog),
            "instagram" | "ig" => Some(Preset::Instagram),
            "youtube" | "yt" => Some(Preset::YouTube),
            "tiktok" | "tt" => Some(Preset::TikTok),
            "cinema" | "cinema4k" | "4k" => Some(Preset::Cinema4K),
            "preview" | "fast" => Some(Preset::FastPreview),
            "archive" | "archival" => Some(Preset::Archive),
            "natural" | "natural-enhance" => Some(Preset::NaturalEnhance),
            "cinematic" | "teal-orange" => Some(Preset::CinematicTealOrange),
            "portrait" => Some(Preset::Portrait),
            _ => None,
        }
    }

    /// Get description of the preset
    pub fn description(&self) -> &str {
        match self {
            Preset::DjiDLog => "DJI drone footage with D-Log color profile",
            Preset::DjiStandard => "DJI drone footage with standard color profile",
            Preset::GoPro => "GoPro action camera footage",
            Preset::SonySLog => "Sony camera footage with S-Log profile",
            Preset::CanonCLog => "Canon camera footage with C-Log profile",
            Preset::Instagram => "Optimized for Instagram (square crop, high quality)",
            Preset::YouTube => "Optimized for YouTube (high quality, good compression)",
            Preset::TikTok => "Optimized for TikTok (vertical video)",
            Preset::Cinema4K => "Cinema 4K export (ProRes, maximum quality)",
            Preset::FastPreview => "Fast preview (lower quality, faster processing)",
            Preset::Archive => "High quality archival (H.265, low CRF)",
            Preset::NaturalEnhance => "Natural color enhancement using vibrance",
            Preset::CinematicTealOrange => "Cinematic teal and orange color grading",
            Preset::Portrait => "Portrait mode with skin tone protection",
        }
    }

    /// List all available presets
    pub fn list_all() -> Vec<(&'static str, &'static str)> {
        vec![
            ("dji-dlog", "DJI drone footage with D-Log color profile"),
            ("dji", "DJI drone footage with standard color profile"),
            ("gopro", "GoPro action camera footage"),
            ("sony-slog", "Sony camera footage with S-Log profile"),
            ("canon-clog", "Canon camera footage with C-Log profile"),
            ("instagram", "Optimized for Instagram"),
            ("youtube", "Optimized for YouTube"),
            ("tiktok", "Optimized for TikTok"),
            ("cinema4k", "Cinema 4K export (ProRes)"),
            ("preview", "Fast preview mode"),
            ("archive", "High quality archival"),
            ("natural", "Natural color enhancement using vibrance"),
            ("cinematic", "Cinematic teal and orange look"),
            ("portrait", "Portrait mode with skin tone protection"),
        ]
    }
}
