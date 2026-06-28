//! Two-pass video stabilization using ffmpeg's `vidstab` (`vidstabdetect` +
//! `vidstabtransform`).
//!
//! This is more robust than the single-pass `deshake` filter, and adds two
//! refinements learned from stabilizing stitched drone footage:
//!
//! - **Brightness-normalized detection.** Motion detection runs on a
//!   per-frame-normalized copy (`normalize=smoothing=0`) so a sudden exposure
//!   (EV) change is not misread as camera motion (which otherwise injects a
//!   spurious shake at the moment the exposure shifts).
//! - **Retry + validate.** `vidstabtransform` and some encoders can crash
//!   intermittently (e.g. an NVENC session teardown segfault) and leave a
//!   truncated, moov-less file. Each pass is retried until its output validates
//!   (the transform's frame count must match the input), rather than trusting a
//!   single exit code.
//!
//! Per-segment stabilization (stabilizing each clip independently before
//! concatenation) lives in [`crate::video_processor`]; it relies on these
//! primitives so that smoothing never crosses a hard cut between clips.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::FFmpegCommand;

/// Tunables for the two `vidstab` passes.
#[derive(Debug, Clone, Copy)]
pub struct VidstabParams {
    /// `vidstabtransform` smoothing window in frames (higher = glassier glide).
    pub smoothing: u32,
    /// `vidstabdetect` shakiness estimate (1-10).
    pub shakiness: u32,
    /// `vidstabdetect` accuracy (1-15).
    pub accuracy: u32,
}

impl Default for VidstabParams {
    fn default() -> Self {
        Self {
            smoothing: 20,
            shakiness: 8,
            accuracy: 15,
        }
    }
}

/// Encoder settings for the stabilized output, mirrored from the requested
/// final encode so `--bitrate`/`--threads` are honored.
#[derive(Debug, Clone, Copy)]
pub struct EncodeOpts<'a> {
    pub codec: &'a str,
    pub quality: u8,
    pub bitrate: Option<u32>,
    pub threads: Option<usize>,
}

/// Escape a filesystem path for embedding as a filtergraph option value, so a
/// Windows drive path (`C:\...`), backslashes, or quotes are not parsed as
/// filter syntax. See ffmpeg's notes on filtergraph escaping.
fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(':', "\\:")
}

/// Count the video frames in a file via ffprobe's `nb_frames`, or `None` when it
/// is missing/unreadable (e.g. a truncated file from a crashed encode).
pub fn frame_count(path: &Path) -> Option<u64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Pass 1: detect camera motion into a transforms file (`.trf`).
///
/// Detection runs on a brightness-normalized copy so exposure changes do not
/// register as motion. Retried until the `.trf` is written non-empty.
pub fn detect(input: &Path, trf: &Path, params: &VidstabParams, attempts: u32) -> Result<()> {
    let vf = format!(
        "normalize=smoothing=0,vidstabdetect=shakiness={shakiness}:accuracy={accuracy}:result={trf}",
        shakiness = params.shakiness,
        accuracy = params.accuracy,
        trf = escape_filter_path(trf),
    );
    for attempt in 1..=attempts {
        if let Err(e) = std::fs::remove_file(trf)
            && trf.exists()
        {
            log::debug!("could not remove stale trf {trf}: {e}", trf = trf.display());
        }
        let status = Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .arg("-i")
            .arg(input)
            .args(["-vf", &vf, "-f", "null", "-"])
            .status();
        let wrote = trf.metadata().map(|m| m.len() > 0).unwrap_or(false);
        if matches!(&status, Ok(s) if s.success()) && wrote {
            return Ok(());
        }
        log::warn!(
            "vidstabdetect attempt {attempt}/{attempts} failed for {path}; retrying",
            path = input.display()
        );
    }
    bail!(
        "vidstabdetect failed for {path} after {attempts} attempts",
        path = input.display()
    );
}

/// Pass 2: warp the frames with the detected transforms and encode.
///
/// Retried until the output's frame count matches the input — guarding against
/// intermittent filter/encoder crashes that leave a truncated file.
pub fn transform(
    input: &Path,
    output: &Path,
    trf: &Path,
    enc: &EncodeOpts,
    params: &VidstabParams,
    attempts: u32,
) -> Result<()> {
    let want = frame_count(input);
    // optzoom=1 crops just enough to hide the stabilization borders; the unsharp
    // counters the softening introduced by the warp interpolation. The trailing
    // format is added by FFmpegCommand for encoder compatibility.
    let vf = format!(
        "vidstabtransform=input={trf}:smoothing={smoothing}:optzoom=1:interpol=bicubic,\
         unsharp=5:5:0.6:3:3:0.3",
        trf = escape_filter_path(trf),
        smoothing = params.smoothing,
    );
    for attempt in 1..=attempts {
        let mut cmd = FFmpegCommand::new(input, output)
            .video_filter(&vf)
            .video_codec(enc.codec)
            .quality(enc.quality)
            .video_only()
            .overwrite();
        if let Some(bitrate) = enc.bitrate {
            cmd = cmd.bitrate(bitrate);
        }
        if let Some(threads) = enc.threads {
            cmd = cmd.threads(threads);
        }
        let ran = cmd.execute(|_, _| {});
        let got = frame_count(output);
        if ran.is_ok() && want.is_some() && got == want {
            return Ok(());
        }
        log::warn!(
            "vidstabtransform attempt {attempt}/{attempts} for {path}: expected {want:?} frames, got {got:?}; retrying",
            path = input.display()
        );
    }
    bail!(
        "vidstabtransform failed for {path} after {attempts} attempts",
        path = input.display()
    );
}

/// Concatenate already-encoded segments (same codec/params) without re-encoding,
/// via ffmpeg's concat demuxer.
pub fn concat(segments: &[PathBuf], output: &Path) -> Result<()> {
    if segments.is_empty() {
        bail!("no segments to concat");
    }
    let list = output.with_file_name(format!(".speedy-concat-{}.txt", std::process::id()));
    let mut body = String::new();
    for seg in segments {
        // Temp segment paths are generated by us and contain no quotes.
        body.push_str(&format!("file '{path}'\n", path = seg.display()));
    }
    std::fs::write(&list, body)?;
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg(&list)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(output)
        .status();
    if let Err(e) = std::fs::remove_file(&list) {
        log::debug!(
            "could not remove concat list {list}: {e}",
            list = list.display()
        );
    }
    match status {
        Ok(s) if s.success() => Ok(()),
        other => bail!("concat failed: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_params_match_tuned_values() {
        let p = VidstabParams::default();
        assert_eq!((p.smoothing, p.shakiness, p.accuracy), (20, 8, 15));
    }

    #[test]
    fn frame_count_of_missing_file_is_none() {
        assert_eq!(frame_count(Path::new("/no/such/file.mp4")), None);
    }

    #[test]
    fn concat_rejects_empty_segment_list() {
        let r = concat(&[], Path::new("/tmp/out.mp4"));
        assert!(r.is_err(), "empty segment list must error");
    }

    #[test]
    fn escape_filter_path_escapes_colon_and_backslash() {
        let p = escape_filter_path(Path::new("C:\\tmp\\x.trf"));
        // Raw "C:" would be parsed as a filter option boundary; it must be escaped.
        assert!(!p.contains("C:"), "colon must be escaped: {p}");
        assert!(p.contains("C\\:"), "expected escaped colon: {p}");
        assert!(p.contains("\\\\tmp"), "expected escaped backslash: {p}");
        // A clean POSIX path is unchanged.
        assert_eq!(escape_filter_path(Path::new("/tmp/x.trf")), "/tmp/x.trf");
    }
}
