//! Soundboard sounds: how long a sound really is. ffprobe reads the file in
//! a child process with a time limit (killed when it runs over or when the
//! upload is dropped); the length it reports must also fit the file's size,
//! so a forged header can't claim one second for minutes of audio.

use std::{path::Path, process::Stdio, time::Duration};

use tokio::{process::Command, time::timeout};

/// Longest soundboard sound, in milliseconds
pub const SOUND_MAX_MS: u32 = 5_200;

/// ffprobe gets this long for one short file
const PROBE_LIMIT: Duration = Duration::from_secs(10);

/// Most bytes one second of a sound may take: uncompressed WAV up to 48 kHz
/// stereo 24-bit (288 KB/s) with room for headers; compressed audio far less
const MAX_WAV_BYTES_PER_SECOND: f64 = 400_000.0;
const MAX_COMPRESSED_BYTES_PER_SECOND: f64 = 64_000.0;

/// Why a sound was refused
#[derive(Debug, PartialEq, Eq)]
pub enum SoundRefusal {
    /// ffprobe is not there or could not start
    Unavailable,
    /// Not a sound ffprobe can read, or no length
    Unreadable,
    /// ffprobe took too long
    TooSlow,
    /// Longer than a soundboard sound may be
    TooLong,
    /// The length does not fit the file's size
    Implausible,
}

impl SoundRefusal {
    /// The validation code the client shows
    pub fn code(&self) -> &'static str {
        match self {
            SoundRefusal::TooLong => "too_long",
            SoundRefusal::Implausible => "implausible_length",
            _ => "unreadable",
        }
    }
}

/// Whether a length fits a file of this size and type
pub fn plausible(bytes: u64, duration_ms: u32, mime_type: &str) -> bool {
    if duration_ms == 0 {
        return false;
    }
    let max = if mime_type.contains("wav") || mime_type.contains("wave") {
        MAX_WAV_BYTES_PER_SECOND
    } else {
        MAX_COMPRESSED_BYTES_PER_SECOND
    };
    bytes as f64 / (duration_ms as f64 / 1000.0) <= max
}

/// The length of the sound at `path`, in milliseconds, checked
pub async fn measure(path: &Path, bytes: u64, mime_type: &str) -> Result<u32, SoundRefusal> {
    let duration_ms = probe_ms(path).await?;
    if duration_ms > SOUND_MAX_MS {
        return Err(SoundRefusal::TooLong);
    }
    if !plausible(bytes, duration_ms, mime_type) {
        return Err(SoundRefusal::Implausible);
    }
    Ok(duration_ms)
}

/// What ffprobe says the length is
pub async fn probe_ms(path: &Path) -> Result<u32, SoundRefusal> {
    let mut command = Command::new("ffprobe");
    command
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(search) = std::env::var_os("PATH") {
        command.env("PATH", search);
    }
    let child = command.spawn().map_err(|error| {
        tracing::warn!("ffprobe could not start, sound refused: {error}");
        SoundRefusal::Unavailable
    })?;
    let output = match timeout(PROBE_LIMIT, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(_) => return Err(SoundRefusal::Unreadable),
        // the child is dropped with the future, and kill_on_drop ends it
        Err(_) => return Err(SoundRefusal::TooSlow),
    };
    let seconds: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| SoundRefusal::Unreadable)?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(SoundRefusal::Unreadable);
    }
    Ok((seconds * 1000.0).round() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Whether ffmpeg and ffprobe are on this machine (CI may not have them)
    fn have_ffmpeg() -> bool {
        let found = ["ffmpeg", "ffprobe"].iter().all(|tool| {
            std::process::Command::new(tool)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        if !found {
            eprintln!("SKIPPED: ffmpeg/ffprobe are not installed here, so sounds can't be measured");
        }
        found
    }

    /// A tone of this many milliseconds in this format, made by ffmpeg
    fn tone(ms: u32, extension: &str, codec: &str) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .suffix(&format!(".{extension}"))
            .tempfile()
            .unwrap();
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={}", ms as f64 / 1000.0))
            .args(["-c:a", codec])
            .arg(file.path())
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg could not make a {extension}");
        file
    }

    #[tokio::test]
    async fn mp3_ogg_and_wav_are_measured() {
        if !have_ffmpeg() {
            return;
        }
        for (extension, codec, mime) in [
            ("mp3", "libmp3lame", "audio/mpeg"),
            ("ogg", "libvorbis", "audio/ogg"),
            ("wav", "pcm_s16le", "audio/wav"),
        ] {
            let file = tone(1500, extension, codec);
            let bytes = file.as_file().metadata().unwrap().len();
            let measured = measure(file.path(), bytes, mime).await.unwrap();
            assert!((1400..=1600).contains(&measured), "{extension}: {measured} ms");
        }
    }

    #[tokio::test]
    async fn long_or_unreadable_sounds_are_refused() {
        if !have_ffmpeg() {
            return;
        }
        let long = tone(6000, "mp3", "libmp3lame");
        let bytes = long.as_file().metadata().unwrap().len();
        assert_eq!(measure(long.path(), bytes, "audio/mpeg").await, Err(SoundRefusal::TooLong));

        let mut junk = tempfile::NamedTempFile::new().unwrap();
        junk.write_all(b"not audio at all").unwrap();
        assert_eq!(probe_ms(junk.path()).await, Err(SoundRefusal::Unreadable));
    }

    #[test]
    fn a_length_must_fit_the_file_size() {
        // 1.5 s of 128 kbps MP3 is about 24 KB
        assert!(plausible(24_000, 1500, "audio/mpeg"));
        // 500 KB of MP3 claiming one second: minutes of audio behind a forged header
        assert!(!plausible(500_000, 1000, "audio/mpeg"));
        // 5 s of 44.1 kHz mono 16-bit WAV is about 441 KB
        assert!(plausible(441_000, 5000, "audio/wav"));
        assert!(!plausible(500_000, 500, "audio/wav"));
        assert!(!plausible(100, 0, "audio/ogg"));
    }
}
