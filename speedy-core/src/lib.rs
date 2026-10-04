//! Speedy Core - A video processing library using FFmpeg CLI
//!
//! This library provides tools for video processing by wrapping the FFmpeg
//! command-line tool, including:
//! - Speed adjustment with automatic audio pitch correction
//! - Color grading and enhancement (vibrance, curves, color balance)
//! - HDR delivery: DJI D-Log/D-Gamut through ACES to Rec.2100 HLG
//! - Hardware acceleration support
//! - Multiple codec support (H.264, H.265, VP9, AV1, ProRes)
//! - Video stabilization and denoising
//! - Smart presets for common workflows

pub mod color;
pub mod ffmpeg_wrapper;
pub mod presets;
pub mod stabilize;
pub mod video_processor;

// Re-export commonly used types at the crate root
pub use color::{
    ColorPipeline, InputColor, LutSpace, OutputColor, WorkingColor, ensure_ocio_filter,
};
pub use ffmpeg_wrapper::{FFmpegCommand, VideoInfo, check_ffmpeg, get_video_info};
pub use presets::Preset;
pub use video_processor::{ProbedInputs, VideoProcessor};
