//! Speedy Core - A video processing library using FFmpeg CLI
//!
//! This library provides tools for video processing by wrapping the FFmpeg
//! command-line tool, including:
//! - Speed adjustment with automatic audio pitch correction
//! - Color grading and enhancement (vibrance, curves, color balance)
//! - Hardware acceleration support
//! - Multiple codec support (H.264, H.265, VP9, AV1, ProRes)
//! - Video stabilization and denoising
//! - Smart presets for common workflows

pub mod ffmpeg_wrapper;
pub mod presets;
pub mod video_processor;

// Re-export commonly used types at the crate root
pub use ffmpeg_wrapper::{FFmpegCommand, VideoInfo, check_ffmpeg, get_video_info};
pub use presets::Preset;
pub use video_processor::VideoProcessor;

use clap::ValueEnum;

#[derive(Clone, Debug, ValueEnum)]
pub enum ColorProfile {
    Standard,
    DLog,
    SLog,
    CLog,
    VLog,
    FLog,
}

impl ColorProfile {
    pub fn to_string(&self) -> &str {
        match self {
            ColorProfile::Standard => "Standard",
            ColorProfile::DLog => "D-Log",
            ColorProfile::SLog => "S-Log",
            ColorProfile::CLog => "C-Log",
            ColorProfile::VLog => "V-Log",
            ColorProfile::FLog => "F-Log",
        }
    }
}
