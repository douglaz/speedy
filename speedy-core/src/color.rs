//! Colour pipeline description and the scene-referred HDR filter chain.
//!
//! A [`ColorPipeline`] separates what the source is ([`InputColor`]), where
//! grading happens ([`WorkingColor`]) and what is delivered ([`OutputColor`]).
//!
//! Rec.709 output keeps the display-referred route (profile LUT + `eq` & co.).
//! Rec.2100 HLG output takes the ACES route built here:
//!
//! ```text
//! YUV -> RGB float (explicit matrix/range) -> OCIO IDT -> ACEScg / ACEScct
//!     -> grade -> ACES 2.0 Output Transform -> RGB -> BT.2020 NCL limited 10-bit
//! ```
//!
//! The OpenColorIO work is done by ffmpeg's `ocio` filter against a pinned
//! built-in config, so the result never depends on `$OCIO`.

use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use std::process::Command;

/// Built-in ACES 2.0 studio config shipped inside OpenColorIO 2.5. Pinned so
/// the same speedy + ffmpeg build always renders the same picture.
pub const OCIO_CONFIG: &str = "ocio://studio-config-v4.0.0_aces-v2.0_ocio-v2.5";
/// DJI D-Log / D-Gamut colorspace name in [`OCIO_CONFIG`].
pub const OCIO_DJI_DLOG: &str = "D-Log D-Gamut";
/// Rec.2100 HLG display in [`OCIO_CONFIG`].
pub const OCIO_HLG_DISPLAY: &str = "Rec.2100-HLG - Display";
/// ACES 2.0 1000-nit output transform: the only HDR view the config offers for
/// the HLG display (P3-D65 limited gamut inside the Rec.2100 container).
pub const OCIO_HLG_VIEW: &str = "ACES 2.0 - HDR 1000 nits (P3 D65)";

/// Exposure used on the HDR route when none is given. The ACES 2.0 1000-nit
/// rendering puts D-Log 18% grey at ~30% HLG signal, about 0.7 stop under the
/// HLG reference level of 38% (ITU-R BT.2408); this lifts it back so the
/// default output matches other HLG material, e.g. on YouTube.
pub const HLG_DEFAULT_EXPOSURE: f32 = 0.7;

/// ACEScct code value of 18% grey, the pivot of the contrast operator.
const ACESCCT_MID_GREY: f64 = 0.413_588_4;
/// AP1 luminance weights (R, G, B), used by the saturation operator.
const AP1_LUMA: [f64; 3] = [0.272_228_72, 0.674_081_77, 0.053_689_52];

/// What the source footage is encoded as.
#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq, Eq)]
pub enum InputColor {
    /// Display-referred footage, used as-is
    #[default]
    Standard,
    /// DJI D-Log / D-Gamut (the only input with an HDR route)
    #[value(name = "dji-dlog", alias = "d-log")]
    DjiDLogDGamut,
    /// DJI D-Log M, e.g. Avata 2 (Rec.709 route only: DJI publishes a LUT but
    /// no curve to build an ACES input transform from)
    #[value(name = "dji-dlog-m", alias = "d-log-m")]
    DjiDLogM,
    /// Sony S-Log (Rec.709 route only)
    SLog,
    /// Canon C-Log (Rec.709 route only)
    CLog,
    /// Panasonic V-Log (Rec.709 route only)
    VLog,
    /// Fujifilm F-Log (Rec.709 route only)
    FLog,
}

impl InputColor {
    pub fn label(&self) -> &'static str {
        match self {
            InputColor::Standard => "Standard",
            InputColor::DjiDLogDGamut => "D-Log",
            InputColor::DjiDLogM => "D-Log M",
            InputColor::SLog => "S-Log",
            InputColor::CLog => "C-Log",
            InputColor::VLog => "V-Log",
            InputColor::FLog => "F-Log",
        }
    }
}

/// Scene-referred space the image sits in between the input and output
/// transforms on the HDR route. Operators that need a specific space (exposure
/// in ACEScg; contrast, saturation and LUTs in ACEScct) convert as needed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkingColor {
    AcesCg,
    #[default]
    AcesCct,
}

impl WorkingColor {
    fn ocio_name(self) -> &'static str {
        match self {
            WorkingColor::AcesCg => "ACEScg",
            WorkingColor::AcesCct => "ACEScct",
        }
    }
}

/// What is delivered.
#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq, Eq)]
pub enum OutputColor {
    /// SDR Rec.709 (display-referred route)
    #[default]
    Rec709,
    /// HDR Rec.2100 HLG, 10-bit HEVC (ACES route)
    #[value(name = "hlg")]
    Rec2100Hlg,
}

/// Colour space a `--lut` expects and produces on the HDR route.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum LutSpace {
    #[value(name = "acescct")]
    AcesCct,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorPipeline {
    pub input: InputColor,
    pub working: WorkingColor,
    pub output: OutputColor,
}

impl ColorPipeline {
    /// Whether this pipeline takes the scene-referred ACES route.
    pub fn is_hdr(&self) -> bool {
        self.output == OutputColor::Rec2100Hlg
    }
}

/// YUV matrix and range of the source, as `zscale` option values. Needed to
/// turn the source into RGB correctly: a colour tag on the output only labels
/// the stream, it converts nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceYuv {
    matrix: &'static str,
    range: &'static str,
}

impl SourceYuv {
    /// Map ffprobe's `color_space` / `color_range` tags. Untagged values fall
    /// back to BT.709 and limited range (what DJI and virtually every HD/UHD
    /// camera records) instead of ffmpeg's BT.601 guess; the fallback is logged.
    pub fn from_tags(color_space: Option<&str>, color_range: Option<&str>) -> Result<Self> {
        let matrix = match color_space {
            Some("bt709") => "709",
            Some("bt2020nc") => "2020_ncl",
            Some("smpte170m") => "170m",
            Some("bt470bg") => "470bg",
            None | Some("unknown" | "unspecified") => {
                log::warn!("Source has no YUV matrix tag; assuming BT.709");
                "709"
            }
            Some(other) => bail!("Unsupported source YUV matrix {other:?} for HDR output"),
        };
        let range = match color_range {
            Some("tv") => "limited",
            Some("pc") => "full",
            None | Some("unknown" | "unspecified") => {
                log::warn!("Source has no color range tag; assuming limited (tv) range");
                "limited"
            }
            Some(other) => bail!("Unsupported source color range {other:?} for HDR output"),
        };
        Ok(Self { matrix, range })
    }
}

/// The grade applied between the input and output transforms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrGrade<'a> {
    /// Exposure in stops, applied as a linear gain in ACEScg.
    pub exposure: f32,
    /// ACEScct contrast around 18% grey (1.0 = identity).
    pub contrast: f32,
    /// ACEScct saturation (1.0 = identity).
    pub saturation: f32,
    /// File name of an ACEScct-to-ACEScct 3D LUT, relative to ffmpeg's
    /// working directory.
    pub lut: Option<&'a str>,
}

/// Escape an option value for use inside a filtergraph description: `\`, `:`
/// and `'` are escaped for the option parser, and the result is single-quoted
/// for the graph parser (which would otherwise split on `,`, `;`, `[`, `]`).
/// `ocio://` URIs carry a `:`, and OCIO names carry spaces and parentheses.
pub fn filter_quote(value: &str) -> String {
    let mut out = String::from("'");
    for c in value.chars() {
        match c {
            // Inside graph-level quotes a backslash is literal, so this is the
            // option-level escape.
            '\\' | ':' => {
                out.push('\\');
                out.push(c);
            }
            // Option-level `\'`, with the quote itself stepping out of the
            // graph-level quoting.
            '\'' => out.push_str("\\'\\''"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn ocio_colorspace(input: &str, output: &str) -> String {
    format!(
        "ocio=config={config}:input={input}:output={output}:format=gbrpf32le",
        config = filter_quote(OCIO_CONFIG),
        input = filter_quote(input),
        output = filter_quote(output),
    )
}

/// Explicit YUV -> full-range float RGB, using the source's own matrix/range.
pub(crate) fn input_stage(source: SourceYuv) -> String {
    format!(
        "zscale=matrixin={matrix}:rangein={range}:matrix=gbr:range=full,format=gbrpf32le",
        matrix = source.matrix,
        range = source.range,
    )
}

/// What each stitched input's `scale` filter is pinned to before the concat
/// filter (see `FFmpegCommand::concat_input_pin`): the source's matrix and
/// range on both sides, so the clip is read as resolved and nothing is
/// converted, and a 10-bit format. Unpinned, concat converts every clip to the
/// first one's format and tags: an 8-bit first clip narrows the rest, and an
/// untagged clip is taken for BT.601.
pub(crate) fn stitch_input_pin(source: SourceYuv) -> String {
    // `scale` spells the matrices the way ffprobe does.
    let matrix = match source.matrix {
        "2020_ncl" => "bt2020nc",
        "170m" => "smpte170m",
        "470bg" => "bt470bg",
        _ => "bt709",
    };
    format!(
        ":in_color_matrix={matrix}:out_color_matrix={matrix}:in_range={range}:out_range={range},\
         format=yuv420p10le|yuv422p10le|yuv444p10le",
        range = source.range,
    )
}

/// Explicit HLG RGB -> BT.2020 non-constant-luminance, limited range, 10-bit
/// 4:2:0, then frame tags that overwrite whatever the source carried.
pub(crate) fn output_stage() -> String {
    "zscale=matrixin=gbr:rangein=full:matrix=2020_ncl:range=limited,format=yuv420p10le,\
     setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc:range=tv"
        .to_string()
}

/// Encoder-side signaling for Rec.2100 HLG: stream colour tags plus the same
/// values in the x265 VUI, so the HEVC bitstream itself carries them.
pub(crate) fn hlg_output_args() -> Vec<String> {
    [
        "-color_primaries",
        "bt2020",
        "-color_trc",
        "arib-std-b67",
        "-colorspace",
        "bt2020nc",
        "-color_range",
        "tv",
        "-x265-params",
        "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited",
    ]
    .map(String::from)
    .to_vec()
}

/// The HDR colour chain, in order: input conversion, IDT, grade, ODT, output
/// conversion. Every filter between the two conversions runs on `gbrpf32le`.
pub(crate) fn hdr_filters(
    working: WorkingColor,
    source: SourceYuv,
    grade: &HdrGrade,
) -> Vec<String> {
    let has_cct_ops = grade.contrast != 1.0 || grade.saturation != 1.0 || grade.lut.is_some();
    let mut space = if grade.exposure != 0.0 {
        WorkingColor::AcesCg
    } else if has_cct_ops {
        WorkingColor::AcesCct
    } else {
        working
    };

    let mut filters = vec![
        input_stage(source),
        ocio_colorspace(OCIO_DJI_DLOG, space.ocio_name()),
    ];

    if grade.exposure != 0.0 {
        // (in - black) * 2^stops with black = 0: a pure linear gain.
        filters.push(format!(
            "exposure=exposure={stops:.4}:black=0",
            stops = grade.exposure
        ));
    }

    if has_cct_ops {
        if space != WorkingColor::AcesCct {
            filters.push(ocio_colorspace(
                space.ocio_name(),
                WorkingColor::AcesCct.ocio_name(),
            ));
            space = WorkingColor::AcesCct;
        }
        if grade.contrast != 1.0 {
            // out = (in - pivot) * contrast + pivot. `exposure` is the float
            // filter with a gain and an offset: it computes
            // (in - black) / (2^-exposure - black), so solve for both.
            let contrast = f64::from(grade.contrast);
            let black = ACESCCT_MID_GREY * (1.0 - 1.0 / contrast);
            let exposure = -(ACESCCT_MID_GREY + (1.0 - ACESCCT_MID_GREY) / contrast).log2();
            filters.push(format!("exposure=exposure={exposure:.6}:black={black:.6}"));
        }
        if grade.saturation != 1.0 {
            // out = luma + saturation * (in - luma), as a 3x3 matrix.
            let s = f64::from(grade.saturation);
            let [r, g, b] = AP1_LUMA.map(|w| w * (1.0 - s));
            filters.push(format!(
                "colorchannelmixer=rr={rr:.6}:rg={g:.6}:rb={b:.6}:gr={r:.6}:gg={gg:.6}:gb={b:.6}:br={r:.6}:bg={g:.6}:bb={bb:.6}",
                rr = r + s,
                gg = g + s,
                bb = b + s,
            ));
        }
        if let Some(lut) = grade.lut {
            filters.push(format!("lut3d=file={lut}", lut = filter_quote(lut)));
        }
    }

    filters.push(format!(
        "ocio=config={config}:input={input}:display={display}:view={view}:format=gbrpf32le",
        config = filter_quote(OCIO_CONFIG),
        input = filter_quote(space.ocio_name()),
        display = filter_quote(OCIO_HLG_DISPLAY),
        view = filter_quote(OCIO_HLG_VIEW),
    ));
    filters.push(output_stage());
    filters
}

/// Check that the `ffmpeg` on `PATH` has the `ocio` filter, which the HDR route
/// cannot work without. speedy shells out to whatever ffmpeg is installed, and
/// stock builds do not enable OpenColorIO.
pub fn ensure_ocio_filter() -> Result<()> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .context("Failed to run ffmpeg to list its filters")?;
    check_filter_listing(&String::from_utf8_lossy(&output.stdout))
}

/// Look for the `ocio` filter in `ffmpeg -filters` output.
fn check_filter_listing(listing: &str) -> Result<()> {
    let has_ocio = listing
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some("ocio"));
    ensure!(
        has_ocio,
        "HDR output needs an ffmpeg built with the OpenColorIO `ocio` filter \
         (--enable-libopencolorio), and the ffmpeg on PATH does not have it. Run speedy inside \
         this repository's Nix dev shell (`nix develop`), whose ffmpeg build includes it."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const BT709_TV: SourceYuv = SourceYuv {
        matrix: "709",
        range: "limited",
    };

    const NO_GRADE: HdrGrade = HdrGrade {
        exposure: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        lut: None,
    };

    const QUOTED_CONFIG: &str = r"'ocio\://studio-config-v4.0.0_aces-v2.0_ocio-v2.5'";

    #[test]
    fn filter_quote_escapes_both_parser_levels() {
        // The URI's colon is escaped for the option parser; the quotes keep
        // spaces, commas and parentheses away from the graph parser.
        assert_eq!(filter_quote(OCIO_CONFIG), QUOTED_CONFIG);
        assert_eq!(
            filter_quote(OCIO_HLG_VIEW),
            "'ACES 2.0 - HDR 1000 nits (P3 D65)'"
        );
        assert_eq!(filter_quote(r"a\b"), r"'a\\b'");
        assert_eq!(filter_quote("it's, ok"), r"'it\'\''s, ok'");
    }

    #[test]
    fn ungraded_chain_is_input_idt_odt_output() {
        let filters = hdr_filters(WorkingColor::AcesCct, BT709_TV, &NO_GRADE);
        assert_eq!(
            filters,
            vec![
                "zscale=matrixin=709:rangein=limited:matrix=gbr:range=full,format=gbrpf32le"
                    .to_string(),
                format!(
                    "ocio=config={QUOTED_CONFIG}:input='D-Log D-Gamut':output='ACEScct':format=gbrpf32le"
                ),
                format!(
                    "ocio=config={QUOTED_CONFIG}:input='ACEScct':display='Rec.2100-HLG - Display':view='ACES 2.0 - HDR 1000 nits (P3 D65)':format=gbrpf32le"
                ),
                "zscale=matrixin=gbr:rangein=full:matrix=2020_ncl:range=limited,format=yuv420p10le,\
                 setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc:range=tv"
                    .to_string(),
            ]
        );
    }

    #[test]
    fn working_space_is_honoured_when_no_operator_needs_another() {
        let filters = hdr_filters(WorkingColor::AcesCg, BT709_TV, &NO_GRADE);
        assert!(filters[1].contains(":output='ACEScg':"), "{filters:?}");
        assert!(filters[2].contains(":input='ACEScg':"), "{filters:?}");
    }

    #[test]
    fn full_grade_runs_exposure_in_acescg_then_grades_in_acescct() {
        let grade = HdrGrade {
            exposure: 1.0,
            contrast: 2.0,
            saturation: 0.0,
            lut: Some("look.cube"),
        };
        let filters = hdr_filters(WorkingColor::AcesCct, BT709_TV, &grade);
        let stage = |needle: &str| {
            filters
                .iter()
                .position(|f| f.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} in {filters:?}"))
        };
        let order = [
            stage("zscale=matrixin=709"),
            stage("input='D-Log D-Gamut':output='ACEScg'"),
            stage("exposure=exposure=1.0000:black=0"),
            stage("input='ACEScg':output='ACEScct'"),
            // Contrast 2 around the ACEScct grey pivot.
            stage("exposure=exposure=0.500638:black=0.206794"),
            // Saturation 0: every output channel is the AP1 luminance.
            stage(
                "colorchannelmixer=rr=0.272229:rg=0.674082:rb=0.053690:gr=0.272229:gg=0.674082:gb=0.053690:br=0.272229:bg=0.674082:bb=0.053690",
            ),
            stage("lut3d=file='look.cube'"),
            stage("input='ACEScct':display='Rec.2100-HLG - Display'"),
            stage("matrix=2020_ncl:range=limited"),
        ];
        assert!(order.is_sorted(), "stage order {order:?} in {filters:?}");
        assert_eq!(filters.len(), order.len(), "{filters:?}");
    }

    #[test]
    fn exposure_alone_renders_straight_from_acescg() {
        let grade = HdrGrade {
            exposure: -0.5,
            ..NO_GRADE
        };
        let filters = hdr_filters(WorkingColor::AcesCct, BT709_TV, &grade);
        assert_eq!(filters.len(), 5, "{filters:?}");
        assert_eq!(filters[2], "exposure=exposure=-0.5000:black=0");
        assert!(
            filters[3].contains(":input='ACEScg':display="),
            "{filters:?}"
        );
    }

    #[test]
    fn source_tags_select_the_input_matrix_and_range() -> Result<()> {
        let tagged = SourceYuv::from_tags(Some("bt2020nc"), Some("pc"))?;
        assert_eq!(
            input_stage(tagged),
            "zscale=matrixin=2020_ncl:rangein=full:matrix=gbr:range=full,format=gbrpf32le"
        );
        assert_eq!(
            SourceYuv::from_tags(Some("smpte170m"), Some("tv"))?.matrix,
            "170m"
        );
        // Untagged input falls back to BT.709 limited, never ffmpeg's BT.601.
        assert_eq!(SourceYuv::from_tags(None, None)?, BT709_TV);
        assert_eq!(
            SourceYuv::from_tags(Some("unknown"), Some("unknown"))?,
            BT709_TV
        );
        assert!(SourceYuv::from_tags(Some("ycgco"), Some("tv")).is_err());
        Ok(())
    }

    #[test]
    fn hlg_output_args_tag_the_stream_and_the_x265_vui() {
        let args = hlg_output_args();
        let pair = |a: &str, b: &str| args.windows(2).any(|w| w[0] == a && w[1] == b);
        assert!(pair("-color_primaries", "bt2020"), "{args:?}");
        assert!(pair("-color_trc", "arib-std-b67"), "{args:?}");
        assert!(pair("-colorspace", "bt2020nc"), "{args:?}");
        assert!(pair("-color_range", "tv"), "{args:?}");
        assert!(
            pair(
                "-x265-params",
                "colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc:range=limited"
            ),
            "{args:?}"
        );
    }

    #[test]
    fn missing_ocio_filter_names_the_flake_build() {
        let stock = " T. exposure          V->V       Adjust exposure of the video stream.\n \
                     .S zscale            V->V       Apply resizing, colorspace and bit depth conversion.\n";
        let message = check_filter_listing(stock)
            .expect_err("no ocio filter listed")
            .to_string();
        assert!(message.contains("`ocio` filter"), "{message}");
        assert!(message.contains("nix develop"), "{message}");

        let with_ocio =
            format!("{stock} .S ocio              V->V       Apply OCIO Display/View transform\n");
        assert!(check_filter_listing(&with_ocio).is_ok());
    }

    // ---- Tests below run ffmpeg. They need the OpenColorIO-enabled build from
    // the Nix dev shell and return early (passing) where the `ocio` filter is
    // absent, so a stock ffmpeg does not fail the suite.

    fn ocio_available() -> bool {
        let available = ensure_ocio_filter().is_ok();
        if !available {
            eprintln!("skipped: the ffmpeg on PATH has no `ocio` filter (use `nix develop`)");
        }
        available
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Result<Self> {
            let dir = std::env::temp_dir().join(format!(
                "speedy-hdr-test-{pid}-{name}",
                pid = std::process::id()
            ));
            std::fs::create_dir_all(&dir)?;
            Ok(Self(dir))
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program).args(args).output()?;
        ensure!(
            output.status.success(),
            "{program} {args:?} failed: {stderr}",
            stderr = String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn path_str(path: &Path) -> &str {
        path.to_str().expect("test paths are UTF-8")
    }

    /// DJI's published D-Log OETF (scene-linear reflectance -> code value 0-1).
    fn dlog_encode(x: f64) -> f64 {
        if x <= 0.0078 {
            6.025 * x + 0.0929
        } else {
            0.256663 * (x * 0.9892 + 0.0108).log10() + 0.584555
        }
    }

    /// 10-bit limited-range luma code of D-Log-encoded 18% grey.
    fn dlog_grey_code() -> u16 {
        (64.0 + 876.0 * dlog_encode(0.18)).round() as u16
    }

    /// Write a flat 10-bit clip of the given luma code (neutral chroma) as
    /// lossless HEVC, with the given colour tag options.
    fn flat_clip(path: &Path, luma: u16, tags: &[&str]) -> Result<()> {
        let source =
            format!("color=s=320x180:r=25:d=0.4,format=yuv420p10le,geq=lum={luma}:cb=512:cr=512");
        let mut args = vec!["-v", "error", "-y", "-f", "lavfi", "-i", &source];
        args.extend([
            "-c:v",
            "libx265",
            "-x265-params",
            "lossless=1:log-level=error",
        ]);
        args.extend(tags);
        args.push(path_str(path));
        run("ffmpeg", &args).map(drop)
    }

    /// ffprobe's view of the first video stream's format and colour signaling.
    fn probe_signaling(path: &Path, demuxer: Option<&str>) -> Result<String> {
        let mut args = vec!["-v", "error"];
        if let Some(demuxer) = demuxer {
            args.extend(["-f", demuxer]);
        }
        args.extend([
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=profile,pix_fmt,color_range,color_space,color_transfer,color_primaries",
            "-of",
            "default=noprint_wrappers=1",
            path_str(path),
        ]);
        run("ffprobe", &args)
    }

    fn assert_hlg_signaling(report: &str) {
        for expected in [
            "profile=Main 10",
            "pix_fmt=yuv420p10le",
            "color_range=tv",
            "color_space=bt2020nc",
            "color_transfer=arib-std-b67",
            "color_primaries=bt2020",
        ] {
            assert!(
                report.lines().any(|line| line == expected),
                "missing {expected} in:\n{report}"
            );
        }
    }

    fn hlg_processor(input: &Path, output: &Path) -> crate::VideoProcessor {
        crate::VideoProcessor::new(input, output)
            .input_color(InputColor::DjiDLogDGamut)
            .output_color(OutputColor::Rec2100Hlg)
    }

    /// Check both the MP4 and a raw `-c copy -f hevc` remux of it: the latter
    /// has no container, so its tags can only come from the HEVC VUI.
    fn assert_hlg_container_and_bitstream(scratch: &Scratch, mp4: &Path) -> Result<()> {
        assert_hlg_signaling(&probe_signaling(mp4, None)?);
        let raw = scratch.path("remux.hevc");
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-i",
                path_str(mp4),
                "-map",
                "0:v",
                "-c",
                "copy",
                "-f",
                "hevc",
                path_str(&raw),
            ],
        )?;
        assert_hlg_signaling(&probe_signaling(&raw, Some("hevc"))?);
        Ok(())
    }

    #[test]
    fn hdr_output_is_main10_hlg_in_container_and_bitstream() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        let scratch = Scratch::new("bitstream")?;
        // A moving 10-bit source with audio, deliberately tagged Rec.709: none
        // of those tags may survive into the HLG output.
        let source = scratch.path("dlog.mp4");
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=320x180:r=25:d=0.4,format=yuv420p10le",
                "-f",
                "lavfi",
                "-i",
                "sine=d=0.4",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
                "-color_range",
                "tv",
                "-c:a",
                "aac",
                path_str(&source),
            ],
        )?;
        let output = scratch.path("hlg.mp4");
        hlg_processor(&source, &output).process()?;

        assert_hlg_container_and_bitstream(&scratch, &output)?;
        // Audio is kept for a single clip, as on the SDR route.
        let audio = run(
            "ffprobe",
            &[
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
                path_str(&output),
            ],
        )?;
        assert_eq!(audio.trim(), "audio");
        Ok(())
    }

    #[test]
    fn untagged_and_stitched_sources_still_yield_hlg_signaling() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        let scratch = Scratch::new("stitch")?;
        let (a, b) = (scratch.path("a.mp4"), scratch.path("b.mp4"));
        flat_clip(&a, dlog_grey_code(), &[])?;
        flat_clip(&b, dlog_grey_code(), &[])?;
        let output = scratch.path("hlg.mp4");
        crate::VideoProcessor::new_multi(vec![a, b], &output)
            .input_color(InputColor::DjiDLogDGamut)
            .output_color(OutputColor::Rec2100Hlg)
            .speed(2.0)
            .scale("160:90")
            .process()?;
        assert_hlg_container_and_bitstream(&scratch, &output)
    }

    #[test]
    fn pinned_stitch_inputs_reach_concat_unconverted() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        let scratch = Scratch::new("pin")?;
        // An 8-bit untagged clip first, then a 10-bit BT.709-tagged one whose
        // codes do not survive 8 bits. Unpinned, concat would convert the
        // second clip to the first one's depth and (BT.601-guessed) matrix.
        let clip = |name: &str, source: &str| -> Result<PathBuf> {
            let path = scratch.path(name);
            let source = format!("color=s=320x180:r=25:d=0.4,{source}");
            run(
                "ffmpeg",
                &[
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-c:v",
                    "libx265",
                    "-x265-params",
                    "lossless=1:log-level=error",
                    path_str(&path),
                ],
            )?;
            Ok(path)
        };
        let a = clip("a.mp4", "format=yuv420p,geq=lum=120:cb=90:cr=200")?;
        let b = clip(
            "b.mp4",
            "format=yuv420p10le,geq=lum=481:cb=361:cr=803,setparams=colorspace=bt709:range=tv",
        )?;

        let command = crate::FFmpegCommand::new_multi(vec![a.clone(), b.clone()], "unused.mp4")
            .video_codec("libx265")
            .concat_normalize(320, 180, "25")
            .concat_input_pin(&stitch_input_pin(BT709_TV))
            .build();
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .and_then(|at| args.get(at + 1))
            .context("no -filter_complex")?;
        let raw = scratch.path("stitched.yuv");
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-i",
                path_str(&a),
                "-i",
                path_str(&b),
                "-filter_complex",
                graph,
                "-map",
                "[v]",
                "-f",
                "rawvideo",
                path_str(&raw),
            ],
        )?;

        // yuv420p10le frames: centre luma, Cb and Cr of the first and last.
        let data = std::fs::read(&raw)?;
        let (luma_samples, chroma_samples) = (320 * 180, 160 * 90);
        let frame_bytes = 2 * (luma_samples + 2 * chroma_samples);
        let frames = data.len() / frame_bytes;
        ensure!(frames >= 2, "expected both clips, got {frames} frame(s)");
        let codes = |frame: usize| {
            let code = |sample: usize| {
                let at = frame * frame_bytes + 2 * sample;
                u16::from_le_bytes([data[at], data[at + 1]])
            };
            [
                code(320 * 90 + 160),
                code(luma_samples + chroma_samples / 2 + 80),
                code(luma_samples + chroma_samples + chroma_samples / 2 + 80),
            ]
        };
        assert_eq!(codes(0), [480, 360, 800], "8-bit clip, widened only");
        assert_eq!(codes(frames - 1), [481, 361, 803], "10-bit clip, untouched");
        Ok(())
    }

    #[test]
    fn dlog_mid_grey_lands_near_hlg_mid_grey() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        // DJI's curve puts 18% grey at ~39.9% of full scale.
        assert!((dlog_encode(0.18) - 0.3988).abs() < 0.001);

        let scratch = Scratch::new("grey")?;
        let source = scratch.path("grey.mp4");
        flat_clip(
            &source,
            dlog_grey_code(),
            &["-colorspace", "bt709", "-color_range", "tv"],
        )?;
        let output = scratch.path("hlg.mp4");
        hlg_processor(&source, &output).quality(4).process()?;

        let raw = scratch.path("frame.yuv");
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-i",
                path_str(&output),
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p10le",
                path_str(&raw),
            ],
        )?;
        let frame = std::fs::read(&raw)?;
        let code =
            |offset: usize| f64::from(u16::from_le_bytes([frame[offset], frame[offset + 1]]));
        let (luma_samples, chroma_samples) = (320 * 180, 160 * 90);
        let luma = code(2 * (320 * 90 + 160));
        let cb = code(2 * (luma_samples + chroma_samples / 2 + 80));
        let cr = code(2 * (luma_samples + chroma_samples + chroma_samples / 2 + 80));

        // The HLG reference puts an 18% grey card at 38% signal; the default
        // exposure (HLG_DEFAULT_EXPOSURE) exists to land there.
        let signal = (luma - 64.0) / 876.0;
        eprintln!("D-Log 18% grey -> HLG signal {signal:.4} (Y'={luma}, Cb={cb}, Cr={cr})");
        assert!((signal - 0.38).abs() < 0.02, "HLG mid grey at {signal}");
        // A neutral patch must stay neutral.
        assert!((cb - 512.0).abs() <= 2.0 && (cr - 512.0).abs() <= 2.0);
        Ok(())
    }

    #[test]
    fn output_stage_encodes_rgb_with_the_bt2020_ncl_matrix() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        let scratch = Scratch::new("matrix")?;
        // A flat 16x16 gbrpf32le frame (planes in G, B, R order).
        let (r, g, b) = (0.75f32, 0.40f32, 0.10f32);
        let patch: Vec<u8> = [g, b, r]
            .iter()
            .flat_map(|v| std::iter::repeat_n(v.to_le_bytes(), 16 * 16))
            .flatten()
            .collect();
        let source = scratch.path("patch.raw");
        std::fs::write(&source, patch)?;
        let encoded = scratch.path("patch.yuv");
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-noauto_conversion_filters",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gbrpf32le",
                "-s",
                "16x16",
                "-i",
                path_str(&source),
                "-vf",
                &output_stage(),
                "-f",
                "rawvideo",
                path_str(&encoded),
            ],
        )?;
        let frame = std::fs::read(&encoded)?;
        // yuv420p10le: 256 luma samples, then 64 Cb, then 64 Cr.
        assert_eq!(frame.len(), 2 * (256 + 64 + 64));
        let code = |sample: usize| {
            f64::from(u16::from_le_bytes([
                frame[2 * sample],
                frame[2 * sample + 1],
            ]))
        };
        // Decode limited-range BT.2020 NCL (Kr = 0.2627, Kb = 0.0593) by hand.
        let y = (code(0) - 64.0) / 876.0;
        let cb = (code(256) - 512.0) / 896.0;
        let cr = (code(256 + 64) - 512.0) / 896.0;
        let decoded_r = y + 1.4746 * cr;
        let decoded_b = y + 1.8814 * cb;
        let decoded_g = (y - 0.2627 * decoded_r - 0.0593 * decoded_b) / 0.6780;
        for (name, decoded, expected) in [
            ("R", decoded_r, r),
            ("G", decoded_g, g),
            ("B", decoded_b, b),
        ] {
            assert!(
                (decoded - f64::from(expected)).abs() < 0.003,
                "{name}: decoded {decoded}, expected {expected}"
            );
        }
        Ok(())
    }

    #[test]
    fn hdr_chain_needs_no_automatic_format_conversion() -> Result<()> {
        if !ocio_available() {
            return Ok(());
        }
        // With auto-inserted conversions disabled, ffmpeg refuses to build the
        // graph if any filter between the two explicit conversions cannot take
        // gbrpf32le — which would otherwise quantize silently.
        let scratch = Scratch::new("float")?;
        std::fs::write(
            scratch.path("identity.cube"),
            "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n",
        )?;
        let grade = HdrGrade {
            exposure: 0.5,
            contrast: 1.2,
            saturation: 1.3,
            lut: Some("identity.cube"),
        };
        let chain = hdr_filters(WorkingColor::AcesCct, BT709_TV, &grade).join(",");
        let output = Command::new("ffmpeg")
            .current_dir(&scratch.0)
            .args(["-v", "error", "-noauto_conversion_filters", "-f", "lavfi"])
            .args(["-i", "testsrc2=s=64x64:r=25:d=0.2,format=yuv420p10le"])
            .args(["-vf", &chain, "-f", "null", "-"])
            .output()?;
        assert!(
            output.status.success(),
            "{chain}\n{stderr}",
            stderr = String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}
