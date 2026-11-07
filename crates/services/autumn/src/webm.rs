//! WebM for community designs. A creator may upload one, but it is never
//! stored or served: ffmpeg turns it into an animated WebP here, and that image
//! then goes through every check an image upload gets.
//!
//! A WebM is a file a stranger made, read by a large parser, so ffmpeg runs
//! with as little as it can do its job with: no shell, no stdin, an empty
//! environment, local files only, the Matroska demuxer and the libvpx decoders
//! alone, a pixel and allocation limit, one thread, and it is killed on a time
//! limit. Only a few run at once.
//!
//! The libvpx decoders are named on purpose: ffmpeg's own vp8 and vp9 decoders
//! ignore the alpha layer, and the picture would come out on a solid ground.

use std::{path::Path, process::Stdio, sync::Arc, time::Duration};

use lazy_static::lazy_static;
use tokio::{
    process::Command,
    sync::Semaphore,
    time::{Instant, timeout},
};

/// Conversions running at once
const MAX_CONVERSIONS: usize = 2;
/// How long an upload waits for its turn
const QUEUE_WAIT: Duration = Duration::from_secs(5);
/// Largest single allocation ffmpeg may make
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
/// Quality of the WebP frames
const QUALITY: &str = "80";

lazy_static! {
    static ref TURNS: Arc<Semaphore> = Arc::new(Semaphore::new(MAX_CONVERSIONS));
}

/// Why a WebM could not become an image
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebmRefusal {
    /// Not a VP8 or VP9 WebM, or it does not decode
    Unreadable,
    /// Over the time limit, or too many are being converted
    TooSlow,
    /// ffmpeg is not there or could not start: nothing is stored
    Unavailable,
}

impl WebmRefusal {
    /// Code sent to the client with the refused field
    pub fn code(self) -> &'static str {
        match self {
            WebmRefusal::Unreadable => "webm",
            WebmRefusal::TooSlow | WebmRefusal::Unavailable => "webm_slow",
        }
    }
}

/// What the image made from a WebM may be
#[derive(Debug, Clone)]
pub struct WebmLimits {
    /// Largest width or height
    pub max_dimension: u32,
    pub max_frames: u32,
    pub max_duration_ms: u32,
    /// Largest image that comes out
    pub max_output_bytes: usize,
    /// How many times it plays: 1 for an effect, 0 (for ever) for the rest
    pub plays: u16,
    /// ffmpeg is killed after this long
    pub timeout: Duration,
}

/// The decoders allowed. Both understand the alpha layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebmCodec {
    Vp8,
    Vp9,
}

impl WebmCodec {
    fn decoder(self) -> &'static str {
        match self {
            WebmCodec::Vp8 => "libvpx",
            WebmCodec::Vp9 => "libvpx-vp9",
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// A file that says it is a WebM: an EBML header with the `webm` document type
pub fn is_webm(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) && find(&bytes[..bytes.len().min(64)], b"webm")
}

/// Which allowed codec the file names. Anything else never reaches ffmpeg, and
/// if a file names another one after all, the decoder set here refuses it.
pub fn webm_codec(bytes: &[u8]) -> Option<WebmCodec> {
    if find(bytes, b"V_VP9") {
        Some(WebmCodec::Vp9)
    } else if find(bytes, b"V_VP8") {
        Some(WebmCodec::Vp8)
    } else {
        None
    }
}

/// Frames a second of the image. Never faster than a frame in 33 ms, and slow
/// enough that the longest file allowed fits the frame limit.
fn frame_rate(limits: &WebmLimits) -> u32 {
    let fit = (limits.max_frames as u64 * 1000 / limits.max_duration_ms.max(1) as u64) as u32;
    fit.clamp(1, 30)
}

/// Everything ffmpeg is told. No value in here comes from the upload: the
/// paths are our own temporary files.
pub fn ffmpeg_args(
    input: &str,
    output: &str,
    codec: WebmCodec,
    limits: &WebmLimits,
) -> Vec<String> {
    let rate = frame_rate(limits);
    // just past the limits: a longer file comes out too long and is refused by
    // the image check, instead of being cut short without a word
    let seconds = (limits.max_duration_ms as f64 + 200.0) / 1000.0;
    let frames = limits.max_frames + 2;
    let pixels = limits.max_dimension as u64 * limits.max_dimension as u64;
    [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-max_alloc",
        &MAX_ALLOC_BYTES.to_string(),
        // what may be read
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "matroska,webm",
        "-codec_whitelist",
        "libvpx,libvpx-vp9",
        "-f",
        "matroska",
        "-threads",
        "1",
        "-max_pixels",
        &pixels.to_string(),
        "-c:v",
        codec.decoder(),
        "-i",
        input,
        // the first picture and nothing else: no sound, subtitles, attached
        // files, chapters or metadata
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-t",
        &seconds.to_string(),
        "-frames:v",
        &frames.to_string(),
        "-vf",
        &format!("fps={rate},format=yuva420p"),
        "-filter_threads",
        "1",
        "-c:v",
        "libwebp_anim",
        "-lossless",
        "0",
        "-quality",
        QUALITY,
        // a little over the limit, so the image check says what is wrong
        "-fs",
        &(limits.max_output_bytes as u64 + 1024).to_string(),
        "-f",
        "webp",
        "-y",
        output,
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

/// Run a program under the rules above. Killed when the time is up.
pub async fn run_limited(
    program: &str,
    args: &[String],
    limit: Duration,
) -> Result<(), WebmRefusal> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let mut child = command.spawn().map_err(|error| {
        tracing::error!("ffmpeg could not start: {error}");
        WebmRefusal::Unavailable
    })?;
    match timeout(limit, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(_) => Err(WebmRefusal::Unreadable),
        // dropping the child kills it, but say so now rather than later
        Err(_) => {
            let _ = child.kill().await;
            Err(WebmRefusal::TooSlow)
        }
    }
}

/// Set how often an animated WebP plays, and clear its background colour. The
/// encoder writes "for ever" on opaque white. Browsers ignore that colour, but
/// a decoder that honours it would see every clear pixel as solid.
/// `None` when the file has no animation header.
pub fn set_webp_plays(bytes: &mut [u8], plays: u16) -> Option<()> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if &bytes[at..at + 4] == b"ANIM" {
            if len < 6 || at + 14 > bytes.len() {
                return None;
            }
            bytes[at + 8..at + 12].fill(0);
            bytes[at + 12..at + 14].copy_from_slice(&plays.to_le_bytes());
            return Some(());
        }
        at = at.checked_add(8)?.checked_add(len + (len & 1))?;
    }
    None
}

/// Turn the WebM at `input` into an animated WebP
pub async fn convert_webm(
    input: &Path,
    bytes: &[u8],
    limits: &WebmLimits,
) -> Result<Vec<u8>, WebmRefusal> {
    convert_with("ffmpeg", input, bytes, limits).await
}

/// The same, with the program named, so a test can run it without ffmpeg
pub async fn convert_with(
    program: &str,
    input: &Path,
    bytes: &[u8],
    limits: &WebmLimits,
) -> Result<Vec<u8>, WebmRefusal> {
    let codec = webm_codec(bytes).ok_or(WebmRefusal::Unreadable)?;
    let started = Instant::now();
    let _turn = timeout(QUEUE_WAIT, TURNS.clone().acquire_owned())
        .await
        .map_err(|_| WebmRefusal::TooSlow)?
        .map_err(|_| WebmRefusal::Unavailable)?;

    let output = tempfile::NamedTempFile::new().map_err(|_| WebmRefusal::Unavailable)?;
    let in_path = input.to_str().ok_or(WebmRefusal::Unavailable)?;
    let out_path = output.path().to_str().ok_or(WebmRefusal::Unavailable)?;
    let left = limits.timeout.saturating_sub(started.elapsed());
    run_limited(program, &ffmpeg_args(in_path, out_path, codec, limits), left).await?;

    let mut image = tokio::fs::read(output.path())
        .await
        .map_err(|_| WebmRefusal::Unreadable)?;
    set_webp_plays(&mut image, limits.plays).ok_or(WebmRefusal::Unreadable)?;
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> WebmLimits {
        WebmLimits {
            max_dimension: 1024,
            max_frames: 120,
            max_duration_ms: 4_000,
            max_output_bytes: 2_000_000,
            plays: 1,
            timeout: Duration::from_secs(5),
        }
    }

    fn header() -> Vec<u8> {
        let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84];
        bytes.extend_from_slice(b"webm");
        bytes.extend_from_slice(&[0; 32]);
        bytes
    }

    #[test]
    fn only_a_webm_counts_as_one() {
        assert!(is_webm(&header()));
        let mut matroska = header();
        matroska[8..12].copy_from_slice(b"matr");
        assert!(!is_webm(&matroska));
        assert!(!is_webm(b"RIFF....WEBP"));
        assert!(!is_webm(b""));
    }

    #[test]
    fn only_vp8_and_vp9_reach_the_decoder() {
        let mut vp9 = header();
        vp9.extend_from_slice(b"V_VP9");
        assert_eq!(webm_codec(&vp9), Some(WebmCodec::Vp9));
        let mut vp8 = header();
        vp8.extend_from_slice(b"V_VP8");
        assert_eq!(webm_codec(&vp8), Some(WebmCodec::Vp8));
        let mut other = header();
        other.extend_from_slice(b"V_AV1");
        assert_eq!(webm_codec(&other), None);
    }

    #[test]
    fn ffmpeg_gets_no_more_than_it_needs() {
        let args = ffmpeg_args("/tmp/in", "/tmp/out", WebmCodec::Vp9, &limits());
        let after = |name: &str| {
            let at = args.iter().position(|arg| arg == name).unwrap_or_else(|| panic!("{name}"));
            args[at + 1].as_str()
        };
        assert_eq!(after("-protocol_whitelist"), "file");
        assert_eq!(after("-format_whitelist"), "matroska,webm");
        assert_eq!(after("-codec_whitelist"), "libvpx,libvpx-vp9");
        assert_eq!(after("-f"), "matroska");
        assert_eq!(after("-max_pixels"), "1048576");
        assert_eq!(after("-map"), "0:v:0");
        assert_eq!(after("-map_metadata"), "-1");
        assert_eq!(after("-map_chapters"), "-1");
        for flag in ["-nostdin", "-an", "-sn", "-dn"] {
            assert!(args.iter().any(|arg| arg == flag), "{flag}");
        }
        // the alpha-reading decoder comes before the input
        let decoder = args.iter().position(|arg| arg == "libvpx-vp9").unwrap();
        let input = args.iter().position(|arg| arg == "/tmp/in").unwrap();
        assert!(decoder < input);
        assert!(after("-vf").ends_with("format=yuva420p"));
        assert_eq!(args.last().map(String::as_str), Some("/tmp/out"));
    }

    #[test]
    fn the_frame_rate_fits_the_limits() {
        assert_eq!(frame_rate(&limits()), 30);
        let decoration = WebmLimits {
            max_frames: 240,
            max_duration_ms: 12_000,
            ..limits()
        };
        assert_eq!(frame_rate(&decoration), 20);
    }

    #[test]
    fn a_webp_can_be_set_to_play_once() {
        let mut image = b"RIFF\0\0\0\0WEBP".to_vec();
        image.extend_from_slice(b"VP8X");
        image.extend_from_slice(&10u32.to_le_bytes());
        image.extend_from_slice(&[0; 10]);
        image.extend_from_slice(b"ANIM");
        image.extend_from_slice(&6u32.to_le_bytes());
        image.extend_from_slice(&[255, 255, 255, 255, 0, 0]);
        assert_eq!(set_webp_plays(&mut image, 1), Some(()));
        assert_eq!(&image[image.len() - 6..], &[0, 0, 0, 0, 1, 0]);
        assert_eq!(set_webp_plays(&mut b"RIFF\0\0\0\0WEBPVP8 ".to_vec(), 1), None);
        assert_eq!(set_webp_plays(&mut b"not an image".to_vec(), 1), None);
    }

    #[tokio::test]
    async fn no_ffmpeg_means_no_file() {
        let mut bytes = header();
        bytes.extend_from_slice(b"V_VP9");
        let input = tempfile::NamedTempFile::new().unwrap();
        let answer = convert_with("/nonexistent/ffmpeg", input.path(), &bytes, &limits()).await;
        assert_eq!(answer, Err(WebmRefusal::Unavailable));
    }

    #[tokio::test]
    async fn another_codec_never_starts_the_program() {
        let input = tempfile::NamedTempFile::new().unwrap();
        // would be Unavailable if it had tried to run this
        let answer = convert_with("/nonexistent/ffmpeg", input.path(), &header(), &limits()).await;
        assert_eq!(answer, Err(WebmRefusal::Unreadable));
    }

    #[tokio::test]
    async fn a_program_over_time_is_killed() {
        let started = Instant::now();
        let answer = run_limited("sleep", &["30".to_string()], Duration::from_millis(200)).await;
        assert_eq!(answer, Err(WebmRefusal::TooSlow));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_failing_program_is_an_unreadable_file() {
        let answer = run_limited("false", &[], Duration::from_secs(5)).await;
        assert_eq!(answer, Err(WebmRefusal::Unreadable));
    }

    /// The tests below need ffmpeg with libvpx and libwebp on the PATH. They
    /// make their own tiny files.
    mod with_ffmpeg {
        use std::process::Command as Plain;

        use image::{AnimationDecoder, codecs::webp::WebPDecoder};

        use super::*;
        use crate::effects::{EffectRefusal, check_design, check_effect, seen};

        fn make(name: &str, args: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            let done = Plain::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .arg(&path)
                .status()
                .expect("ffmpeg is needed for this test");
            assert!(done.success(), "{name} was not made");
            (dir, path)
        }

        const DOT: &str = "color=c=black@0.0:s=144x256:r=20:d=2,format=yuva420p,drawbox=x=10:y=20:w=40:h=40:c=white@1:t=fill:replace=1:enable='lt(t,1)',drawbox=x=70:y=20:w=40:h=40:c=white@1:t=fill:replace=1:enable='gte(t,1)'";

        async fn convert(path: &Path) -> Result<Vec<u8>, WebmRefusal> {
            let bytes = std::fs::read(path).unwrap();
            convert_webm(path, &bytes, &limits()).await
        }

        fn alpha_range(image: &[u8]) -> (u8, u8) {
            let frames = WebPDecoder::new(std::io::Cursor::new(image))
                .unwrap()
                .into_frames()
                .collect_frames()
                .unwrap();
            let pixels: Vec<u8> = frames[0].buffer().pixels().map(|p| p.0[3]).collect();
            (*pixels.iter().min().unwrap(), *pixels.iter().max().unwrap())
        }

        #[tokio::test]
        async fn vp9_keeps_its_transparency() {
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", DOT, "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0"]);
            let image = convert(&path).await.unwrap();
            assert_eq!(alpha_range(&image), (0, 255));
            assert_eq!(check_effect(&image, &seen(&image)), Ok(()));
        }

        #[tokio::test]
        async fn vp8_keeps_its_transparency() {
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", DOT, "-c:v", "libvpx", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0"]);
            let image = convert(&path).await.unwrap();
            assert_eq!(alpha_range(&image), (0, 255));
        }

        #[tokio::test]
        async fn sound_and_metadata_are_gone() {
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", DOT, "-f", "lavfi", "-i", "anullsrc", "-shortest", "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0", "-c:a", "libopus", "-metadata", "title=GPS 47.07 Mira"]);
            let source = std::fs::read(&path).unwrap();
            assert!(find(&source, b"GPS 47.07"));
            let image = convert(&path).await.unwrap();
            assert!(!find(&image, b"GPS 47.07"));
            assert_eq!(alpha_range(&image).0, 0);
        }

        #[tokio::test]
        async fn a_strobe_comes_out_and_the_flash_check_refuses_it() {
            let strobe = "color=c=black@0.0:s=144x256:r=20:d=2,format=yuva420p,drawbox=x=0:y=0:w=144:h=110:c=white@1:t=fill:replace=1:enable='lt(mod(n,2),1)'";
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", strobe, "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0"]);
            let image = convert(&path).await.unwrap();
            assert_eq!(check_effect(&image, &seen(&image)), Err(EffectRefusal::Flashing));
        }

        const BLINK: &str = "format=yuva420p,drawbox=x=0:y=0:w=iw:h=ih:c=white@1:t=fill:replace=1:enable='lt(mod(n,2),1)'";

        /// Decorations and nameplates loop, and have the limits of their tag
        async fn design_of(size: &str, filter: &str) -> Vec<u8> {
            let source = format!("color=c=black@0.0:s={size}:r=20:d=2,{filter}");
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", &source, "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0"]);
            let limits = WebmLimits {
                max_frames: 240,
                max_duration_ms: 12_000,
                max_output_bytes: 2_500_000,
                plays: 0,
                ..limits()
            };
            let bytes = std::fs::read(&path).unwrap();
            convert_webm(&path, &bytes, &limits).await.unwrap()
        }

        #[tokio::test]
        async fn a_strobing_webm_is_refused_on_both_kinds_of_design() {
            for size in ["128x128", "256x48"] {
                let image = design_of(size, BLINK).await;
                assert_eq!(check_design(&image, &seen(&image)), Err(EffectRefusal::Flashing), "{size}");
            }
        }

        #[tokio::test]
        async fn a_calm_webm_passes_on_both_kinds_of_design() {
            for size in ["128x128", "256x48"] {
                let calm = "format=yuva420p,drawbox=x=0:y=0:w=8:h=8:c=white@1:t=fill:replace=1:enable='lt(t,1)',drawbox=x=20:y=0:w=8:h=8:c=white@1:t=fill:replace=1:enable='gte(t,1)'";
                let image = design_of(size, calm).await;
                assert_eq!(check_design(&image, &seen(&image)), Ok(()), "{size}");
            }
        }

        #[tokio::test]
        async fn a_cut_file_is_refused() {
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", DOT, "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0"]);
            let bytes = std::fs::read(&path).unwrap();
            let cut = &bytes[..bytes.len() / 8];
            std::fs::write(&path, cut).unwrap();
            assert_eq!(convert_webm(&path, cut, &limits()).await, Err(WebmRefusal::Unreadable));
        }

        #[tokio::test]
        async fn a_huge_picture_is_refused() {
            let (_dir, path) = make("a.webm", &["-f", "lavfi", "-i", "color=c=red:s=2048x2048:r=5:d=1", "-c:v", "libvpx-vp9"]);
            assert_eq!(convert(&path).await, Err(WebmRefusal::Unreadable));
        }
    }
}
