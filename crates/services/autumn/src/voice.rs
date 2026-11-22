//! Voice messages: an AAC copy for browsers that can't play the recording.
//!
//! Browsers record Opus in Ogg (Firefox) or WebM (Chrome). Safari plays Ogg
//! only from 18.4 (macOS 15.4, iOS 18.4) and WebM Opus only from 17.4, so on
//! older iPhones a voice message never played. Every voice message gets an
//! AAC copy in MP4 that every browser plays; the web app asks for it with
//! `?format=m4a` where `canPlayType` says no.
//!
//! The copy is made right after the upload, and on the first request for a
//! file that has none yet (older uploads). ffmpeg runs as for WebM designs
//! (webm.rs): no shell, no stdin, an empty environment, local files only, the
//! audio demuxers and decoders named here alone, one thread, a time limit,
//! and only a few at once.

use std::{sync::Arc, time::Duration};

use lazy_static::lazy_static;
use tokio::{sync::Semaphore, time::timeout};

use crate::webm::{WebmRefusal, run_limited};

/// Conversions running at once
const MAX_CONVERSIONS: usize = 2;
/// How long a request waits for its turn
const QUEUE_WAIT: Duration = Duration::from_secs(10);
/// ffmpeg is killed after this long (the longest recording is 5 minutes)
const TIME_LIMIT: Duration = Duration::from_secs(60);
/// Largest single allocation ffmpeg may make
const MAX_ALLOC_BYTES: u64 = 64 * 1024 * 1024;
/// Longest copy made, in seconds: a little over the 300 s recording limit
const MAX_SECONDS: &str = "310";
/// Speech in mono: clear at this rate, and small
const BITRATE: &str = "64k";

/// Content type of the copy
pub const RENDITION_CONTENT_TYPE: &str = "audio/mp4";

lazy_static! {
    static ref TURNS: Arc<Semaphore> = Arc::new(Semaphore::new(MAX_CONVERSIONS));
}

/// A voice message as the web app names it: `voice-message-<digits>.<ext>`
pub fn is_voice_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("voice-message-") else {
        return false;
    };
    let Some((digits, ext)) = rest.split_once('.') else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(ext, "ogg" | "webm" | "m4a" | "mp4")
}

/// The content type a file is served with. `infer` calls an Ogg file with
/// Opus in it `audio/opus`, which names the codec for RTP, not a file type:
/// browsers are told the container and the codec.
pub fn served_content_type(content_type: &str) -> String {
    match content_type {
        "audio/opus" => "audio/ogg; codecs=opus".to_owned(),
        other => other.to_owned(),
    }
}

/// The original is AAC already (Safari records AAC in MP4): no copy needed
pub fn is_aac(content_type: &str, codec: Option<&str>) -> bool {
    matches!(content_type, "audio/mp4" | "audio/aac" | "audio/x-m4a" | "video/mp4")
        && codec == Some("aac")
}

/// Everything ffmpeg is told. No value in here comes from the upload: the
/// paths are our own temporary files.
pub fn ffmpeg_args(input: &str, output: &str) -> Vec<String> {
    [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-max_alloc",
        &MAX_ALLOC_BYTES.to_string(),
        // what may be read: the containers and codecs browsers record
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "ogg,matroska,webm,mov,mp4,m4a",
        "-codec_whitelist",
        "opus,libopus,vorbis,aac,mp3",
        "-threads",
        "1",
        "-i",
        input,
        // the first sound and nothing else: no picture, subtitles, data,
        // chapters or metadata
        "-map",
        "0:a:0",
        "-vn",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-t",
        MAX_SECONDS,
        "-ac",
        "1",
        "-c:a",
        "aac",
        "-b:a",
        BITRATE,
        // the index first: playback starts before the whole file is in
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
        "-y",
        output,
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

/// Make the AAC copy of a recording
pub async fn to_aac(bytes: &[u8]) -> Result<Vec<u8>, WebmRefusal> {
    to_aac_with("ffmpeg", bytes).await
}

/// The same, with the program named, so a test can run it without ffmpeg
pub async fn to_aac_with(program: &str, bytes: &[u8]) -> Result<Vec<u8>, WebmRefusal> {
    let _turn = timeout(QUEUE_WAIT, TURNS.clone().acquire_owned())
        .await
        .map_err(|_| WebmRefusal::TooSlow)?
        .map_err(|_| WebmRefusal::Unavailable)?;

    let input = tempfile::NamedTempFile::new().map_err(|_| WebmRefusal::Unavailable)?;
    tokio::fs::write(input.path(), bytes)
        .await
        .map_err(|_| WebmRefusal::Unavailable)?;
    let output = tempfile::Builder::new()
        .suffix(".m4a")
        .tempfile()
        .map_err(|_| WebmRefusal::Unavailable)?;
    let in_path = input.path().to_str().ok_or(WebmRefusal::Unavailable)?;
    let out_path = output.path().to_str().ok_or(WebmRefusal::Unavailable)?;
    run_limited(program, &ffmpeg_args(in_path, out_path), TIME_LIMIT).await?;

    let copy = tokio::fs::read(output.path())
        .await
        .map_err(|_| WebmRefusal::Unreadable)?;
    if copy.is_empty() {
        return Err(WebmRefusal::Unreadable);
    }
    Ok(copy)
}

/// The codec of the first sound in a file, as ffprobe names it
pub fn first_audio_codec(bytes: &[u8]) -> Option<String> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new().ok()?;
    file.write_all(bytes).ok()?;
    let probe = ffprobe::ffprobe(file.path()).ok()?;
    probe
        .streams
        .into_iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"))
        .and_then(|stream| stream.codec_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_names() {
        assert!(is_voice_name("voice-message-1729890000000.ogg"));
        assert!(is_voice_name("voice-message-1.WEBM"));
        assert!(is_voice_name("voice-message-1.m4a"));
        assert!(!is_voice_name("voice-message-.ogg"));
        assert!(!is_voice_name("voice-message-1.exe"));
        assert!(!is_voice_name("voice-message-1a.ogg"));
        assert!(!is_voice_name("SPOILER_voice-message-1.ogg"));
        assert!(!is_voice_name("song.ogg"));
    }

    #[test]
    fn ogg_opus_is_served_with_its_container() {
        assert_eq!(served_content_type("audio/opus"), "audio/ogg; codecs=opus");
        assert_eq!(served_content_type("audio/mp4"), "audio/mp4");
        assert_eq!(served_content_type("image/png"), "image/png");
    }

    #[test]
    fn aac_needs_no_copy() {
        assert!(is_aac("audio/mp4", Some("aac")));
        assert!(!is_aac("audio/mp4", Some("opus")));
        assert!(!is_aac("audio/opus", Some("opus")));
        assert!(!is_aac("audio/mp4", None));
    }

    #[test]
    fn ffmpeg_reads_only_audio_containers_and_writes_aac() {
        let args = ffmpeg_args("/tmp/in", "/tmp/out.m4a");
        let after = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(after("-protocol_whitelist").as_deref(), Some("file"));
        assert_eq!(after("-c:a").as_deref(), Some("aac"));
        assert_eq!(after("-map").as_deref(), Some("0:a:0"));
        assert_eq!(after("-f").as_deref(), Some("mp4"));
        assert!(args.contains(&"-nostdin".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("/tmp/out.m4a"));
    }

    #[tokio::test]
    async fn a_missing_ffmpeg_is_unavailable() {
        assert_eq!(
            to_aac_with("/nonexistent/ffmpeg", b"OggS").await,
            Err(WebmRefusal::Unavailable)
        );
    }

    /// A real conversion where ffmpeg is installed (WSL, the autumn image)
    #[tokio::test]
    async fn makes_an_aac_copy_of_opus() {
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i",
                "sine=frequency=440:duration=2", "-c:a", "libopus", "-f", "ogg", "-",
            ])
            .output();
        let Ok(out) = made else {
            eprintln!("SKIPPED: ffmpeg is not installed here");
            return;
        };
        if !out.status.success() || out.stdout.is_empty() {
            eprintln!("SKIPPED: this ffmpeg can't make Opus");
            return;
        }
        let copy = to_aac(&out.stdout).await.expect("converted");
        assert_eq!(&copy[4..8], b"ftyp");
        assert_eq!(first_audio_codec(&copy).as_deref(), Some("aac"));
        assert_eq!(first_audio_codec(&out.stdout).as_deref(), Some("opus"));
    }
}
