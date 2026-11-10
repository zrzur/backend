//! The sound library (built into every server binary) stored like uploads:
//! at start, every library sound that is not stored yet becomes one file
//! hash in S3. Adding a library sound to a server then only makes a new file
//! of that hash (delta), and the sound is served like any other.
use revolt_database::{sound_library::sound_library, Database};
use revolt_files::upload_to_s3;
use tracing::{error, info};

/// Store the library sounds that are not stored yet; returns how many were
pub async fn store_library(db: &Database) -> usize {
    let bucket = revolt_config::config().await.files.s3.default_bucket;
    let mut stored = 0;
    for sound in &sound_library().sounds {
        let bucket = match db.fetch_attachment_hash(&sound.sha256).await {
            Ok(hash) if !hash.iv.is_empty() => continue,
            // saved before, but the upload never finished: upload it again
            Ok(hash) => hash.bucket_id,
            Err(_) => {
                // another autumn starting at the same time got there first
                if db
                    .insert_attachment_hash(&sound.file_hash(bucket.clone(), String::new()))
                    .await
                    .is_err()
                {
                    continue;
                }
                bucket.clone()
            }
        };
        match upload_to_s3(&bucket, &sound.sha256, sound.bytes).await {
            Ok(nonce) => match db.set_attachment_hash_nonce(&sound.sha256, &nonce).await {
                Ok(()) => stored += 1,
                Err(err) => error!("library sound {}: {err:?}", sound.id),
            },
            Err(err) => error!("library sound {} could not be stored: {err:?}", sound.id),
        }
    }
    info!("Sound library: {stored} sounds stored now, {} in all", sound_library().sounds.len());
    stored
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use revolt_database::sound_library::sound_library;

    use crate::sound::{probe_ms, SOUND_MAX_MS};

    /// ffprobe agrees with the length the library was built with, and every
    /// sound is one a soundboard plays
    #[tokio::test]
    async fn every_library_sound_is_measured_as_listed() {
        let found = std::process::Command::new("ffprobe")
            .arg("-version")
            .output()
            .is_ok_and(|out| out.status.success());
        if !found {
            eprintln!("SKIPPED: ffprobe is not installed here");
            return;
        }
        for sound in &sound_library().sounds {
            let mut file = tempfile::Builder::new().suffix(".ogg").tempfile().unwrap();
            file.write_all(sound.bytes).unwrap();
            let measured = probe_ms(file.path()).await.unwrap();
            assert!(measured <= SOUND_MAX_MS && measured <= 5_000, "{}: {measured} ms", sound.id);
            assert!(measured.abs_diff(sound.duration_ms) <= 20, "{}: {measured} ms", sound.id);
            let name = format!("{}.ogg", sound.id);
            let mime = crate::mime_type::determine_mime_type(&mut file, sound.bytes, &name);
            assert!(crate::api::is_sound_type(mime), "{}: {mime}", sound.id);
        }
    }
}
