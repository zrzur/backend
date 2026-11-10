//! The sound library (built into every server binary) stored like uploads:
//! every library sound is one file hash in S3. Adding a library sound to a
//! server then only makes a new file of that hash (delta), and the sound is
//! served like any other. Autumn stores the missing ones at start and checks
//! again every 10 minutes, so a hash that went away (an older crond, a
//! cleanup by hand) comes back on its own.
use std::{future::Future, time::Duration};

use revolt_database::{sound_library::sound_library, Database};
use revolt_files::upload_to_s3;
use revolt_result::Result;
use tracing::{error, info};

/// How often the library is checked again
pub const RECHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// Store the library sounds that are missing, now and then every 10 minutes
pub async fn keep_library_stored(db: Database) {
    loop {
        let bucket = revolt_config::config().await.files.s3.default_bucket;
        let upload = |bucket: String, path: &'static str, bytes: &'static [u8]| async move {
            upload_to_s3(&bucket, path, bytes).await
        };
        match store_missing(&db, &bucket, upload).await {
            Ok(0) => {}
            Ok(stored) => info!(
                "Sound library: {stored} sounds stored now, {} in all",
                sound_library().sounds.len()
            ),
            Err(err) => error!("Sound library check failed: {err:?}"),
        }
        tokio::time::sleep(RECHECK_EVERY).await;
    }
}

/// Store the library sounds that are not stored; returns how many were.
/// One query finds the stored ones (by id, at most 115 results); only the
/// missing ones are uploaded.
pub async fn store_missing<U, F>(db: &Database, bucket: &str, upload: U) -> Result<usize>
where
    U: Fn(String, &'static str, &'static [u8]) -> F,
    F: Future<Output = Result<String>>,
{
    let library = sound_library();
    let ids: Vec<String> = library.sounds.iter().map(|sound| sound.sha256.clone()).collect();
    let stored = db.fetch_stored_attachment_hash_ids(&ids).await?;
    let mut count = 0;
    for sound in library.sounds.iter().filter(|sound| !stored.contains(&sound.sha256)) {
        // a hash saved before whose upload never finished is uploaded again
        // into its own bucket; a hash that is not there is saved first (if
        // another autumn saves it at the same moment, that one uploads it)
        let bucket = match db.fetch_attachment_hash(&sound.sha256).await {
            Ok(hash) => hash.bucket_id,
            Err(_) => {
                if db
                    .insert_attachment_hash(&sound.file_hash(bucket.to_string(), String::new()))
                    .await
                    .is_err()
                {
                    continue;
                }
                bucket.to_string()
            }
        };
        match upload(bucket, sound.sha256.as_str(), sound.bytes).await {
            Ok(nonce) => match db.set_attachment_hash_nonce(&sound.sha256, &nonce).await {
                Ok(()) => count += 1,
                Err(err) => error!("library sound {}: {err:?}", sound.id),
            },
            Err(err) => error!("library sound {} could not be stored: {err:?}", sound.id),
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    use revolt_database::{sound_library::sound_library, DatabaseInfo};

    use super::store_missing;
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

    /// A library hash that went away comes back at the next check, and a
    /// check with everything stored uploads nothing
    #[tokio::test]
    async fn a_removed_library_hash_comes_back_at_the_next_check() {
        let db = DatabaseInfo::Reference.connect().await.unwrap();
        let uploads = Arc::new(Mutex::new(Vec::<String>::new()));
        let upload = |seen: Arc<Mutex<Vec<String>>>| {
            move |_bucket: String, path: &'static str, _bytes: &'static [u8]| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(path.to_string());
                    Ok("nonce".to_string())
                }
            }
        };
        let all = sound_library().sounds.len();

        assert_eq!(store_missing(&db, "uploads", upload(uploads.clone())).await.unwrap(), all);
        assert_eq!(store_missing(&db, "uploads", upload(uploads.clone())).await.unwrap(), 0);
        assert_eq!(uploads.lock().unwrap().len(), all);

        // an older crond removes one
        let go = sound_library().sounds.iter().find(|s| s.id == "voice-go").unwrap();
        db.delete_attachment_hash(&go.sha256).await.unwrap();
        assert!(db.fetch_attachment_hash(&go.sha256).await.is_err());

        assert_eq!(store_missing(&db, "uploads", upload(uploads.clone())).await.unwrap(), 1);
        let back = db.fetch_attachment_hash(&go.sha256).await.unwrap();
        assert_eq!(back.iv, "nonce");
        assert_eq!(uploads.lock().unwrap().last().unwrap(), &go.sha256);

        // one whose upload never finished (no nonce) is uploaded again
        let ready = sound_library().sounds.iter().find(|s| s.id == "voice-ready").unwrap();
        db.set_attachment_hash_nonce(&ready.sha256, "").await.unwrap();
        assert_eq!(store_missing(&db, "uploads", upload(uploads.clone())).await.unwrap(), 1);
        assert_eq!(db.fetch_attachment_hash(&ready.sha256).await.unwrap().iv, "nonce");
    }
}
