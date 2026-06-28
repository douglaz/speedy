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
use crate::ffmpeg_wrapper::is_mp4_family;

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

/// The trailing filename of a path (for referencing a `.trf` by name from the
/// ffmpeg working directory), falling back to the full path string.
fn file_name_str(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The directory to run ffmpeg from for a `.trf` path: its parent, unless that
/// is empty (a bare filename), in which case `None`.
fn work_dir(path: &Path) -> Option<&Path> {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => Some(p),
        _ => None,
    }
}

/// Count the video frames in a file by counting demuxed packets, or `None` when
/// the file is missing/unreadable (e.g. a truncated file from a crashed encode).
///
/// Counting packets (one per coded frame for the codecs used here) is
/// container-agnostic and fast — unlike `nb_frames`, which Matroska does not
/// populate (our stabilization intermediates are `.mkv`).
pub fn frame_count(path: &Path) -> Option<u64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
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
    // Reference the .trf by filename and run from its directory, so an absolute
    // path with colons/backslashes (e.g. a Windows temp dir) never reaches the
    // filtergraph parser (which mis-parses such paths even when escaped/quoted).
    let vf = format!(
        "normalize=smoothing=0,vidstabdetect=shakiness={shakiness}:accuracy={accuracy}:result={name}",
        shakiness = params.shakiness,
        accuracy = params.accuracy,
        name = file_name_str(trf),
    );
    // Absolutize input now: ffmpeg runs from the .trf directory below, so a
    // relative input would otherwise resolve against that, not the caller's cwd.
    let input_abs = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
    for attempt in 1..=attempts {
        if let Err(e) = std::fs::remove_file(trf)
            && trf.exists()
        {
            log::debug!("could not remove stale trf {trf}: {e}", trf = trf.display());
        }
        let mut command = Command::new("ffmpeg");
        if let Some(dir) = work_dir(trf) {
            command.current_dir(dir);
        }
        let status = command
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .arg("-i")
            .arg(&input_abs)
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
    // Absolutize input/output now: ffmpeg runs from the .trf directory below,
    // so relative paths would otherwise resolve against that, not the cwd.
    let input_abs = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
    let want = frame_count(&input_abs);
    // optzoom=1 crops just enough to hide the stabilization borders; the unsharp
    // counters the softening introduced by the warp interpolation. The trailing
    // format is added by FFmpegCommand for encoder compatibility. The .trf is
    // referenced by filename (with current_dir) to dodge filtergraph path
    // escaping; the output is absolutized so current_dir doesn't redirect it.
    let vf = format!(
        "vidstabtransform=input={name}:smoothing={smoothing}:optzoom=1:interpol=bicubic,\
         unsharp=5:5:0.6:3:3:0.3",
        name = file_name_str(trf),
        smoothing = params.smoothing,
    );
    let output_abs = std::path::absolute(output).unwrap_or_else(|_| output.to_path_buf());
    for attempt in 1..=attempts {
        let mut cmd = FFmpegCommand::new(&input_abs, &output_abs)
            .video_filter(&vf)
            .video_codec(enc.codec)
            .quality(enc.quality)
            .video_only()
            .overwrite();
        if let Some(dir) = work_dir(trf) {
            cmd = cmd.current_dir(dir);
        }
        if let Some(bitrate) = enc.bitrate {
            cmd = cmd.bitrate(bitrate);
        }
        if let Some(threads) = enc.threads {
            cmd = cmd.threads(threads);
        }
        let ran = cmd.execute(|_, _| {});
        let got = frame_count(&output_abs);
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
///
/// ffmpeg runs from the segments' directory and references each by filename, so
/// segment/list paths (which may sit under a TMPDIR with spaces, quotes,
/// backslashes, or non-UTF-8 bytes) never need ffconcat escaping. The list lives
/// in that same per-run temp dir, so concurrent runs don't share it. The output
/// is absolutized so the working-directory change can't redirect it.
pub fn concat(segments: &[PathBuf], output: &Path) -> Result<()> {
    if segments.is_empty() {
        bail!("no segments to concat");
    }
    // We reference segments by filename and run from one directory, so they must
    // all live in it; otherwise a basename could resolve to the wrong file.
    let parent0 = segments[0].parent();
    if segments.iter().any(|s| s.parent() != parent0) {
        bail!("all concat segments must be in the same directory");
    }
    let dir = parent0.unwrap_or_else(|| Path::new("."));
    let list = dir.join("concat-list.txt");
    let mut body = String::new();
    for seg in segments {
        let name = seg
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| seg.to_string_lossy().into_owned());
        body.push_str(&format!("file '{name}'\n"));
    }
    std::fs::write(&list, &body)?;

    let output_abs = std::path::absolute(output).unwrap_or_else(|_| output.to_path_buf());
    let mut cmd = Command::new("ffmpeg");
    cmd.current_dir(dir).args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        "concat-list.txt",
        "-c",
        "copy",
    ]);
    // -movflags +faststart is MP4/MOV-only; skip it for other containers.
    if is_mp4_family(&output_abs) {
        cmd.args(["-movflags", "+faststart"]);
    }
    let status = cmd.arg(&output_abs).status();
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
    fn concat_rejects_segments_from_different_dirs() {
        // Referencing by basename from one dir would resolve the wrong file.
        let segs = vec![PathBuf::from("/a/x.mkv"), PathBuf::from("/b/y.mkv")];
        assert!(concat(&segs, Path::new("/tmp/out.mp4")).is_err());
    }

    #[test]
    fn trf_is_referenced_by_bare_filename() {
        // The filter must reference the .trf by name (no directory), so an
        // absolute path with colons/backslashes never hits the filtergraph.
        let name = file_name_str(Path::new("/tmp/speedy-stab-1/t_0.trf"));
        assert_eq!(name, "t_0.trf");
        assert!(!name.contains('/') && !name.contains(':'), "name: {name}");
    }

    #[test]
    fn work_dir_is_the_parent_or_none() {
        assert_eq!(
            work_dir(Path::new("/tmp/speedy-stab-1/t_0.trf")),
            Some(Path::new("/tmp/speedy-stab-1"))
        );
        // A bare filename has no usable working directory.
        assert_eq!(work_dir(Path::new("t_0.trf")), None);
    }
}
