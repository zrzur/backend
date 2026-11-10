use std::time::Duration;

use log::info;
use revolt_database::{
    util::cdn_purge::{purge_files, PurgeOutcome},
    Database,
};
use revolt_files::delete_from_s3;
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
