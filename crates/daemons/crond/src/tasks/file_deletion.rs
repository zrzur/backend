use std::{future::Future, time::Duration};

use log::{info, warn};
use revolt_database::{
    util::cdn_purge::{purge_files, PurgeOutcome},
    Database,
};
use revolt_files::{delete_from_s3, voice_rendition_id};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let files = db.fetch_deleted_attachments().await?;

        // Every deleted file passes here, however it was deleted, so this is
        // where its links leave the CDN. A failed purge is logged and never
        // holds up deleting the file.
        purge_files(&files).await;

        // Files kept as evidence stay out of the loop below, but not at the CDN
        let reported = db.fetch_reported_files_to_purge().await?;
        if !reported.is_empty() && purge_files(&reported).await == PurgeOutcome::Done {
            let ids: Vec<String> = reported.into_iter().map(|file| file.id).collect();
            db.mark_attachments_as_purged(&ids).await?;
        }

        for file in files {
            if let Some(hash) = &file.hash {
                let count = db.count_file_hash_references(hash).await?;

                // No other files reference this file on disk anymore. A
                // sound of the built-in library stays: the next server that
                // adds it gets a new file of the same hash.
                if count <= 1 && !revolt_database::sound_library::is_library_hash(hash) {
                    let file_hash = db.fetch_attachment_hash(hash).await?;

                    // First the AAC copy autumn made of a voice message: it
                    // can only be found through the original's hash
                    delete_voice_copy(&db, &file_hash.id, &file_hash.path, |bucket, path| async move {
                        delete_from_s3(&bucket, &path).await
                    })
                    .await;

                    // Delete from S3
                    delete_from_s3(&file_hash.bucket_id, &file_hash.path).await?;

                    // Delete the hash
                    db.delete_attachment_hash(&file_hash.id).await?;
                    info!("Deleted file hash {}", file_hash.id);
                }
            }

            // Delete the file
            db.delete_attachment(&file.id).await?;
            info!("Deleted file {}", file.id);
        }

        sleep(Duration::from_secs(60)).await;
    }
}

/// Delete the AAC copy of a voice message (autumn, `<hash>.m4a`), if it has
/// one. A failure is logged and never stops the loop: the original still goes.
/// The record goes even when its bytes could not be deleted (the path is in
/// the log), so nothing can serve them again. A copy that points at the
/// original's own bytes (an AAC original) leaves them to the original.
pub async fn delete_voice_copy<D, F>(db: &Database, hash_id: &str, original_path: &str, delete: D)
where
    D: FnOnce(String, String) -> F,
    F: Future<Output = Result<()>>,
{
    let Ok(copy) = db.fetch_attachment_hash(&voice_rendition_id(hash_id)).await else {
        return;
    };
    if copy.path != original_path {
        if let Err(error) = delete(copy.bucket_id.clone(), copy.path.clone()).await {
            warn!("AAC copy {} stays in storage at {}: {error:?}", copy.id, copy.path);
        }
    }
    match db.delete_attachment_hash(&copy.id).await {
        Ok(()) => info!("Deleted file hash {}", copy.id),
        Err(error) => warn!("AAC copy record {} not deleted: {error:?}", copy.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iso8601_timestamp::Timestamp;
    use revolt_database::{DatabaseInfo, FileHash, Metadata};
    use std::sync::{Arc, Mutex};

    fn hash(id: &str, path: &str) -> FileHash {
        FileHash {
            id: id.to_owned(),
            processed_hash: String::new(),
            created_at: Timestamp::now_utc(),
            bucket_id: "bucket".to_owned(),
            path: path.to_owned(),
            iv: "iv".to_owned(),
            metadata: Metadata::Audio { duration_ms: None },
            content_type: "audio/mp4".to_owned(),
            size: 10,
        }
    }

    #[test]
    fn the_copy_goes_even_when_storage_fails() {
        futures::executor::block_on(async {
            let db = DatabaseInfo::Reference.connect().await.unwrap();
            db.insert_attachment_hash(&hash("abc", "abc")).await.unwrap();
            db.insert_attachment_hash(&hash("abc.m4a", "abc.m4a.01J")).await.unwrap();

            let asked = Arc::new(Mutex::new(Vec::new()));
            let seen = asked.clone();
            delete_voice_copy(&db, "abc", "abc", |bucket, path| async move {
                seen.lock().unwrap().push(format!("{bucket}/{path}"));
                Err(revolt_result::create_error!(InternalError))
            })
            .await;

            assert_eq!(*asked.lock().unwrap(), vec!["bucket/abc.m4a.01J".to_owned()]);
            assert!(db.fetch_attachment_hash("abc.m4a").await.is_err());
            // the original is left for the loop
            assert!(db.fetch_attachment_hash("abc").await.is_ok());
        });
    }

    #[test]
    fn an_aac_original_keeps_its_bytes_and_no_copy_is_no_error() {
        futures::executor::block_on(async {
            let db = DatabaseInfo::Reference.connect().await.unwrap();
            db.insert_attachment_hash(&hash("aac.m4a", "aac")).await.unwrap();
            let asked = Arc::new(Mutex::new(0));
            let seen = asked.clone();
            delete_voice_copy(&db, "aac", "aac", |_, _| async move {
                *seen.lock().unwrap() += 1;
                Ok(())
            })
            .await;
            assert_eq!(*asked.lock().unwrap(), 0);
            assert!(db.fetch_attachment_hash("aac.m4a").await.is_err());

            // a file without a copy: nothing happens, nothing fails
            delete_voice_copy(&db, "none", "none", |_, _| async { Ok(()) }).await;
        });
    }
}
