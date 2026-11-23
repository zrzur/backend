use std::{future::Future, time::Duration};

use log::{info, warn};
use revolt_database::{
    util::cdn_purge::{purge_files, PurgeOutcome},
    Database, FileHash,
};
use revolt_files::{delete_from_s3, delete_prefix_from_s3, voice_rendition_id};
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
                    delete_voice_copy(&db, &file_hash, &S3).await;

                    // Delete from S3: only bytes stored under the hash's own
                    // id (a copy record has its own namespace, see above)
                    if owns_its_bytes(&file_hash) {
                        delete_from_s3(&file_hash.bucket_id, &file_hash.path).await?;
                    } else {
                        warn!(
                            "file hash {} points at {}: its bytes are left alone",
                            file_hash.id, file_hash.path
                        );
                    }

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

/// An original's bytes live under its own id; anything else is not its to
/// delete
pub fn owns_its_bytes(file_hash: &FileHash) -> bool {
    file_hash.path == file_hash.id
}

/// The storage crond deletes from
pub trait Bytes {
    fn delete_prefix(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> impl Future<Output = Result<usize>> + Send;
}

/// S3
pub struct S3;

impl Bytes for S3 {
    async fn delete_prefix(&self, bucket: &str, prefix: &str) -> Result<usize> {
        delete_prefix_from_s3(bucket, prefix).await
    }
}

/// Delete the AAC copy of a voice message (autumn, `<hash>.m4a`) before its
/// original: the record, then every object under `<hash>.m4a.` (the copy in
/// use and any left by a lost race or a broken attempt). The bytes of a copy
/// that points at the original's own bytes (an AAC original) are left to the
/// original. Failures are logged and never stop the loop; the orphans' prefix
/// is in the log for a sweep.
pub async fn delete_voice_copy(db: &Database, original: &FileHash, bytes: &impl Bytes) {
    let id = voice_rendition_id(&original.id);
    if let Ok(copy) = db.fetch_attachment_hash(&id).await {
        match db.delete_attachment_hash_at(&copy.id, &copy.path).await {
            Ok(true) => info!("Deleted file hash {}", copy.id),
            Ok(false) => warn!("AAC copy record {} changed while deleting", copy.id),
            Err(error) => warn!("AAC copy record {} not deleted: {error:?}", copy.id),
        }
    }
    let prefix = format!("{id}.");
    match bytes.delete_prefix(&original.bucket_id, &prefix).await {
        Ok(0) => {}
        Ok(count) => info!("Deleted {count} AAC copy objects under {prefix}"),
        Err(error) => warn!(
            "AAC copy objects under {}/{prefix} stay for a sweep: {error:?}",
            original.bucket_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iso8601_timestamp::Timestamp;
    use revolt_database::{DatabaseInfo, Metadata};
    use std::sync::Mutex;

    fn hash(id: &str, path: &str) -> FileHash {
        FileHash {
            id: id.to_owned(),
            processed_hash: id.to_owned(),
            created_at: Timestamp::now_utc(),
            bucket_id: "bucket".to_owned(),
            path: path.to_owned(),
            iv: "iv".to_owned(),
            metadata: Metadata::Audio { duration_ms: None },
            content_type: "audio/mp4".to_owned(),
            size: 10,
        }
    }

    /// Records what was asked; fails when told to
    #[derive(Default)]
    struct Fake {
        asked: Mutex<Vec<String>>,
        fail: bool,
    }

    impl Bytes for Fake {
        async fn delete_prefix(&self, bucket: &str, prefix: &str) -> Result<usize> {
            self.asked.lock().unwrap().push(format!("{bucket}/{prefix}"));
            if self.fail {
                Err(revolt_result::create_error!(InternalError))
            } else {
                Ok(2)
            }
        }
    }

    #[test]
    fn the_copy_record_and_every_copy_object_go_even_when_storage_fails() {
        futures::executor::block_on(async {
            let db = DatabaseInfo::Reference.connect().await.unwrap();
            let original = hash("abc", "abc");
            db.insert_attachment_hash(&original).await.unwrap();
            db.insert_attachment_hash(&hash("abc.m4a", "abc.m4a.01J")).await.unwrap();

            let bytes = Fake { fail: true, ..Default::default() };
            delete_voice_copy(&db, &original, &bytes).await;

            // the whole namespace of the copies, not only the copy in use
            assert_eq!(*bytes.asked.lock().unwrap(), vec!["bucket/abc.m4a.".to_owned()]);
            assert!(db.fetch_attachment_hash("abc.m4a").await.is_err());
            // the original is left for the loop
            assert!(db.fetch_attachment_hash("abc").await.is_ok());
        });
    }

    #[test]
    fn an_aac_original_keeps_its_bytes_and_no_copy_is_no_error() {
        futures::executor::block_on(async {
            let db = DatabaseInfo::Reference.connect().await.unwrap();
            let original = hash("aac", "aac");
            // the copy record points at the original's own bytes
            db.insert_attachment_hash(&hash("aac.m4a", "aac")).await.unwrap();
            let bytes = Fake::default();
            delete_voice_copy(&db, &original, &bytes).await;
            // only the copies' own namespace: never the path "aac" itself
            assert_eq!(*bytes.asked.lock().unwrap(), vec!["bucket/aac.m4a.".to_owned()]);
            assert!(db.fetch_attachment_hash("aac.m4a").await.is_err());

            // a file without a copy: nothing fails
            delete_voice_copy(&db, &hash("none", "none"), &Fake::default()).await;
        });
    }

    #[test]
    fn only_bytes_under_the_hash_s_own_id_are_deleted() {
        assert!(owns_its_bytes(&hash("abc", "abc")));
        // a copy record, or any record pointing elsewhere
        assert!(!owns_its_bytes(&hash("abc.m4a", "abc")));
        assert!(!owns_its_bytes(&hash("abc.m4a", "abc.m4a.01J")));
    }
}
