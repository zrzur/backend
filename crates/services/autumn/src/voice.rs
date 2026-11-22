//! Voice messages: an AAC copy for browsers that can't play the recording.
//!
//! Browsers record Opus in Ogg (Firefox) or WebM (Chrome). Safari plays Ogg
//! only from 18.4 (macOS 15.4, iOS 18.4) and WebM Opus only from 17.4, so on
//! older iPhones a voice message never played. Every voice message gets an
//! AAC copy in MP4 that every browser plays; the web app asks for it with
//! `?format=m4a` where `canPlayType` says no.
//!
//! The copy is made right after the upload, and on the first request for a
//! file that has none yet (older uploads). That request comes from anyone who
//! can see the link, so nothing is done for it before these checks pass:
//! - the stored file is small enough ([`MAX_SOURCE_BYTES`]) and of a sound
//!   type ([`may_have_copy`]);
//! - no earlier attempt failed for it ([`REFUSED`]: a file that could not be
//!   read, or ran out of time, for an hour; a full queue for a minute);
//! - one attempt per file at a time (voice_copy.rs), in at most
//!   [`MAX_CONVERSIONS`] at once ([`TURNS`]), with the per-address limit
//!   of `ratelimits.rs` in front.
//!
//! ffprobe and ffmpeg run as for WebM designs (webm.rs): no shell, no stdin,
//! an empty environment, local files only, the audio demuxers and decoders
//! named here alone, one thread, a time limit, a size limit on what comes out.

use std::{process::Stdio, sync::Arc, time::Duration};

use lazy_static::lazy_static;
use tokio::{process::Command, sync::Semaphore, time::timeout};

use crate::webm::{WebmRefusal, run_limited};

/// Conversions (and probes) running at once
pub const MAX_CONVERSIONS: usize = 2;
/// How long a request waits for its turn
const QUEUE_WAIT: Duration = Duration::from_secs(10);
/// ffmpeg is killed after this long (the longest recording is 5 minutes)
const TIME_LIMIT: Duration = Duration::from_secs(60);
/// ffprobe is killed after this long
const PROBE_LIMIT: Duration = Duration::from_secs(10);
/// Largest single allocation ffmpeg may make
const MAX_ALLOC_BYTES: u64 = 64 * 1024 * 1024;
/// Longest copy made, in seconds: a little over the 300 s recording limit
const MAX_SECONDS: &str = "310";
/// Speech in mono: clear at this rate, and small
const BITRATE: &str = "64k";
/// Largest copy ffmpeg may write (310 s at 64 kbit/s is about 2.5 MB)
const MAX_OUTPUT_BYTES: &str = "4000000";
/// Largest stored recording a copy is made of: 300 s of Opus at the highest
/// rate a browser records is about 5 MB
pub const MAX_SOURCE_BYTES: isize = 8_000_000;
/// The containers ffprobe and ffmpeg may read
const FORMATS: &str = "ogg,matroska,webm,mov,mp4,m4a";
/// The decoders ffmpeg may use
const DECODERS: &str = "opus,libopus,vorbis,aac,mp3";

/// Content type of the copy
pub const RENDITION_CONTENT_TYPE: &str = "audio/mp4";

lazy_static! {
    static ref TURNS: Arc<Semaphore> = Arc::new(Semaphore::new(MAX_CONVERSIONS));
    /// Files no copy could be made of, and why; each entry lives as long as
    /// its reason says ([`VoiceRefusal::remembered_for`])
    pub static ref REFUSED: moka::future::Cache<String, VoiceRefusal> =
        moka::future::Cache::builder()
            .max_capacity(100_000)
            .expire_after(RefusalExpiry)
            .build();
}

/// Why there is no AAC copy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceRefusal {
    /// The stored file is over [`MAX_SOURCE_BYTES`]
    TooLarge,
    /// Not a sound (or a type no browser records)
    NotSound,
    /// ffprobe or ffmpeg could not read it
    Unreadable,
    /// ffprobe or ffmpeg ran out of time on it: it will again
    TooSlow,
    /// Waited too long for a turn: the others were busy
    Busy,
    /// ffmpeg or ffprobe is not there, or the storage could not be reached
    Unavailable,
}

impl VoiceRefusal {
    /// How long a refusal is remembered for the file
    pub fn remembered_for(self) -> Duration {
        match self {
            VoiceRefusal::Unreadable
            | VoiceRefusal::TooSlow
            | VoiceRefusal::TooLarge
            | VoiceRefusal::NotSound => Duration::from_secs(60 * 60),
            VoiceRefusal::Busy | VoiceRefusal::Unavailable => Duration::from_secs(60),
        }
    }

    /// Asking again later may help: 503 with Retry-After, never stored
    pub fn try_later(self) -> bool {
        matches!(self, VoiceRefusal::Busy | VoiceRefusal::Unavailable)
    }

    fn from_run(refusal: WebmRefusal) -> Self {
        match refusal {
            WebmRefusal::Unreadable => VoiceRefusal::Unreadable,
            WebmRefusal::TooSlow => VoiceRefusal::TooSlow,
            WebmRefusal::Unavailable => VoiceRefusal::Unavailable,
        }
    }
}

struct RefusalExpiry;

impl moka::Expiry<String, VoiceRefusal> for RefusalExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &VoiceRefusal,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.remembered_for())
    }
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

/// Checked before any work is done for a request: small enough, and a type
/// a browser records sound in. Chrome's WebM recordings are stored as
/// `video/webm` (infer reads the container, not the tracks), Safari's MP4
/// ones as `audio/mp4` or, before the stream check, `video/mp4`.
pub fn may_have_copy(content_type: &str, size: isize) -> Result<(), VoiceRefusal> {
    if size > MAX_SOURCE_BYTES {
        return Err(VoiceRefusal::TooLarge);
    }
    if content_type.starts_with("audio/") || matches!(content_type, "video/mp4" | "video/webm") {
        Ok(())
    } else {
        Err(VoiceRefusal::NotSound)
    }
}

/// An MP4 container: the sound in it may be AAC already
pub fn is_mp4_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" | "video/mp4"
    )
}

/// The original is AAC already (Safari records AAC in MP4): no copy needed
pub fn is_aac(content_type: &str, codec: Option<&str>) -> bool {
    is_mp4_type(content_type) && codec == Some("aac")
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
        FORMATS,
        "-codec_whitelist",
        DECODERS,
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
        // never more than this, whatever the input claims
        "-fs",
        MAX_OUTPUT_BYTES,
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

/// Everything ffprobe is told: the codec of the first sound, nothing more
pub fn ffprobe_args(input: &str) -> Vec<String> {
    [
        "-v",
        "error",
        "-max_alloc",
        &MAX_ALLOC_BYTES.to_string(),
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        FORMATS,
        "-codec_whitelist",
        DECODERS,
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=codec_name",
        "-of",
        "default=nw=1:nk=1",
        input,
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

/// The codec of the first sound, under the same rules as ffmpeg
async fn probe_codec(program: &str, path: &str) -> Result<String, VoiceRefusal> {
    let mut command = Command::new(program);
    command
        .args(ffprobe_args(path))
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(search) = std::env::var_os("PATH") {
        command.env("PATH", search);
    }
    let child = command.spawn().map_err(|error| {
        tracing::warn!("ffprobe could not start: {error}");
        VoiceRefusal::Unavailable
    })?;
    match timeout(PROBE_LIMIT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let codec = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if codec.is_empty() {
                Err(VoiceRefusal::Unreadable)
            } else {
                Ok(codec)
            }
        }
        Ok(_) => Err(VoiceRefusal::Unreadable),
        // dropped with the future: kill_on_drop ends it
        Err(_) => Err(VoiceRefusal::TooSlow),
    }
}

/// What came of a recording
#[derive(Debug, PartialEq, Eq)]
pub enum Made {
    /// It is AAC in MP4 already: serve it as it is
    Original,
    /// The AAC copy
    Copy(Vec<u8>),
}

/// Make the AAC copy of a recording (or find it needs none)
pub async fn make(bytes: &[u8], content_type: &str) -> Result<Made, VoiceRefusal> {
    make_with("ffmpeg", "ffprobe", bytes, content_type).await
}

/// The same, with the programs named, so a test can run it without them
pub async fn make_with(
    ffmpeg: &str,
    ffprobe: &str,
    bytes: &[u8],
    content_type: &str,
) -> Result<Made, VoiceRefusal> {
    may_have_copy(content_type, bytes.len() as isize)?;
    let _turn = timeout(QUEUE_WAIT, TURNS.clone().acquire_owned())
        .await
        .map_err(|_| VoiceRefusal::Busy)?
        .map_err(|_| VoiceRefusal::Unavailable)?;

    let input = tempfile::NamedTempFile::new().map_err(|_| VoiceRefusal::Unavailable)?;
    tokio::fs::write(input.path(), bytes)
        .await
        .map_err(|_| VoiceRefusal::Unavailable)?;
    let in_path = input.path().to_str().ok_or(VoiceRefusal::Unavailable)?;

    // only an MP4 can be AAC already; nothing else is probed
    if is_mp4_type(content_type) && probe_codec(ffprobe, in_path).await? == "aac" {
        return Ok(Made::Original);
    }

    let output = tempfile::Builder::new()
        .suffix(".m4a")
        .tempfile()
        .map_err(|_| VoiceRefusal::Unavailable)?;
    let out_path = output.path().to_str().ok_or(VoiceRefusal::Unavailable)?;
    run_limited(ffmpeg, &ffmpeg_args(in_path, out_path), TIME_LIMIT)
        .await
        .map_err(VoiceRefusal::from_run)?;

    let copy = tokio::fs::read(output.path())
        .await
        .map_err(|_| VoiceRefusal::Unreadable)?;
    if copy.is_empty() {
        return Err(VoiceRefusal::Unreadable);
    }
    Ok(Made::Copy(copy))
}

/// What a request asks for, from its query string. Keys and values are
/// decoded as a form would send them (`%66ormat=m4a` is `format=m4a`);
/// any other key (a tracker on a shared link) is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantedFormat {
    /// No `format`: the file as it was uploaded
    Original,
    /// `format=m4a`: the AAC copy
    M4a,
    /// Another format, or `format` given twice with different values
    Other,
}

fn form_decode(part: &str) -> String {
    url_escape::decode(&part.replace('+', " ")).into_owned()
}

pub fn wanted_format(query: Option<&str>) -> WantedFormat {
    let mut wanted = WantedFormat::Original;
    for pair in query.unwrap_or_default().split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if form_decode(key) != "format" {
            continue;
        }
        let this = if form_decode(value) == "m4a" {
            WantedFormat::M4a
        } else {
            WantedFormat::Other
        };
        wanted = match (wanted, this) {
            (WantedFormat::Original, this) => this,
            (same, this) if same == this => same,
            _ => WantedFormat::Other,
        };
    }
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A tone in this container and codec, made by ffmpeg (into a file: the
    /// MP4 muxers can't write to a pipe)
    fn tone(seconds: u32, format: &str, codec: &str) -> Option<Vec<u8>> {
        let file = tempfile::NamedTempFile::new().ok()?;
        let status = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .args(["-ac", "1", "-c:a", codec, "-f", format])
            .arg(file.path())
            .status()
            .ok()?;
        let bytes = std::fs::read(file.path()).ok()?;
        (status.success() && !bytes.is_empty()).then_some(bytes)
    }

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
    fn nothing_is_done_for_big_files_or_other_types() {
        assert_eq!(may_have_copy("audio/opus", MAX_SOURCE_BYTES + 1), Err(VoiceRefusal::TooLarge));
        assert_eq!(may_have_copy("image/png", 10), Err(VoiceRefusal::NotSound));
        assert_eq!(may_have_copy("application/pdf", 10), Err(VoiceRefusal::NotSound));
        assert_eq!(may_have_copy("video/quicktime", 10), Err(VoiceRefusal::NotSound));
        assert!(may_have_copy("audio/opus", 10).is_ok());
        assert!(may_have_copy("video/webm", 10).is_ok());
        assert!(may_have_copy("video/mp4", 10).is_ok());
    }

    #[test]
    fn refusals_are_remembered_and_some_are_asked_again() {
        assert_eq!(VoiceRefusal::Unreadable.remembered_for(), Duration::from_secs(3600));
        assert!(!VoiceRefusal::Unreadable.try_later());
        assert!(VoiceRefusal::Unavailable.try_later());
        assert!(VoiceRefusal::Busy.remembered_for() < VoiceRefusal::Unreadable.remembered_for());
    }

    #[tokio::test]
    async fn the_refused_cache_keeps_an_unreadable_file() {
        let key = "test-unreadable-hash".to_owned();
        REFUSED.insert(key.clone(), VoiceRefusal::Unreadable).await;
        assert_eq!(REFUSED.get(&key).await, Some(VoiceRefusal::Unreadable));
    }

    #[test]
    fn aac_needs_no_copy() {
        assert!(is_aac("audio/mp4", Some("aac")));
        assert!(is_aac("audio/m4a", Some("aac")));
        assert!(is_aac("video/mp4", Some("aac")));
        assert!(!is_aac("audio/mp4", Some("opus")));
        assert!(!is_aac("audio/opus", Some("opus")));
        assert!(!is_aac("audio/mp4", None));
    }

    #[test]
    fn ffmpeg_reads_only_audio_containers_and_writes_little_aac() {
        let args = ffmpeg_args("/tmp/in", "/tmp/out.m4a");
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
        assert_eq!(after("-protocol_whitelist").as_deref(), Some("file"));
        assert_eq!(after("-c:a").as_deref(), Some("aac"));
        assert_eq!(after("-map").as_deref(), Some("0:a:0"));
        assert_eq!(after("-f").as_deref(), Some("mp4"));
        assert_eq!(after("-fs").as_deref(), Some("4000000"));
        assert!(args.contains(&"-nostdin".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("/tmp/out.m4a"));
    }

    #[test]
    fn ffprobe_reads_only_local_audio_containers() {
        let args = ffprobe_args("/tmp/in");
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
        assert_eq!(after("-protocol_whitelist").as_deref(), Some("file"));
        assert_eq!(after("-format_whitelist").as_deref(), Some(FORMATS));
        assert_eq!(after("-codec_whitelist").as_deref(), Some(DECODERS));
        assert!(after("-max_alloc").is_some());
        assert_eq!(after("-select_streams").as_deref(), Some("a:0"));
    }

    #[tokio::test]
    async fn missing_programs_are_unavailable_and_wrong_types_never_start_them() {
        assert_eq!(
            make_with("/nonexistent/ffmpeg", "/nonexistent/ffprobe", b"OggS", "audio/opus").await,
            Err(VoiceRefusal::Unavailable)
        );
        assert_eq!(
            make_with("/nonexistent/ffmpeg", "/nonexistent/ffprobe", b"OggS", "image/png").await,
            Err(VoiceRefusal::NotSound)
        );
    }

    #[test]
    fn the_format_is_read_as_a_form_would_send_it() {
        assert_eq!(wanted_format(None), WantedFormat::Original);
        assert_eq!(wanted_format(Some("")), WantedFormat::Original);
        assert_eq!(wanted_format(Some("format=m4a")), WantedFormat::M4a);
        assert_eq!(wanted_format(Some("%66ormat=m4a")), WantedFormat::M4a);
        assert_eq!(wanted_format(Some("format=%6d4a")), WantedFormat::M4a);
        assert_eq!(wanted_format(Some("format=m4a&format=m4a")), WantedFormat::M4a);
        assert_eq!(wanted_format(Some("format=m4a&format=mp3")), WantedFormat::Other);
        assert_eq!(wanted_format(Some("format=mp3")), WantedFormat::Other);
        // trackers on a shared link are ignored
        assert_eq!(wanted_format(Some("utm_source=x&format=m4a&fbclid=1")), WantedFormat::M4a);
        assert_eq!(wanted_format(Some("utm_source=x")), WantedFormat::Original);
    }

    #[test]
    fn a_full_queue_is_asked_again_and_a_slow_file_is_not() {
        assert!(VoiceRefusal::Busy.try_later());
        assert_eq!(VoiceRefusal::Busy.remembered_for(), Duration::from_secs(60));
        assert!(!VoiceRefusal::TooSlow.try_later());
        assert_eq!(VoiceRefusal::TooSlow.remembered_for(), Duration::from_secs(3600));
    }

    /// Real conversions where ffmpeg is installed (WSL, the autumn image)
    #[tokio::test]
    async fn opus_becomes_aac_and_aac_stays_as_it_is() {
        if !have("ffmpeg") || !have("ffprobe") {
            eprintln!("SKIPPED: ffmpeg/ffprobe are not installed here");
            return;
        }
        let Some(ogg) = tone(2, "ogg", "libopus") else {
            eprintln!("SKIPPED: this ffmpeg can't make Opus");
            return;
        };
        let Made::Copy(copy) = make(&ogg, "audio/opus").await.expect("converted") else {
            panic!("an Opus recording needs a copy");
        };
        assert_eq!(&copy[4..8], b"ftyp");

        // what Safari records: AAC in MP4, as it is
        let mp4 = tone(2, "ipod", "aac").expect("ffmpeg makes AAC");
        assert_eq!(make(&mp4, "audio/m4a").await, Ok(Made::Original));
        // the copy itself counts as AAC
        assert_eq!(make(&copy, "audio/mp4").await, Ok(Made::Original));
        // junk is unreadable
        assert_eq!(make(b"OggS not really", "audio/opus").await, Err(VoiceRefusal::Unreadable));
    }

    /// What the upload code stores for each kind of browser recording
    #[test]
    fn browser_recordings_are_stored_as_sound_types() {
        if !have("ffmpeg") {
            eprintln!("SKIPPED: ffmpeg is not installed here");
            return;
        }
        let cases = [
            ("ogg", "libopus", "voice-message-1.ogg"),
            ("webm", "libopus", "voice-message-1.webm"),
            ("ipod", "aac", "voice-message-1.m4a"),
            ("mp4", "aac", "voice-message-1.mp4"),
        ];
        let mut checked = 0;
        for (format, codec, name) in cases {
            let Some(bytes) = tone(1, format, codec) else {
                // ffmpeg's own AAC encoder is always there: only Opus may be missing
                assert_ne!(codec, "aac", "ffmpeg could not make {format}/{codec}");
                eprintln!("SKIPPED: this ffmpeg can't make {format}/{codec}");
                continue;
            };
            checked += 1;
            let mut file = tempfile::NamedTempFile::new().unwrap();
            std::io::Write::write_all(&mut file, &bytes).unwrap();
            let stored = crate::mime_type::determine_mime_type(&mut file, &bytes, name);
            eprintln!("{format}/{codec}: stored as {stored}");
            assert!(may_have_copy(stored, bytes.len() as isize).is_ok(), "{name}: {stored}");
            if codec == "aac" {
                assert!(is_mp4_type(stored), "{name}: {stored} is not an MP4 type");
            }
        }
        assert!(checked >= 2, "only {checked} recordings were checked");
    }
}
