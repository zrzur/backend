use revolt_result::Result;

use crate::FileHash;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractAttachmentHashes: Sync + Send {
    /// Insert a new attachment hash into the database.
    async fn insert_attachment_hash(&self, hash: &FileHash) -> Result<()>;

    /// Fetch an attachment hash entry by sha256 hash.
    async fn fetch_attachment_hash(&self, hash: &str) -> Result<FileHash>;

    /// Which of these hashes are stored (uploaded, with a nonce); one query,
    /// at most `ids.len()` results
    async fn fetch_stored_attachment_hash_ids(&self, ids: &[String]) -> Result<Vec<String>>;

    /// Update an attachment hash nonce value.
    async fn set_attachment_hash_nonce(&self, hash: &str, nonce: &str) -> Result<()>;

    /// Updates the attachments animated metadata value.
    ///
    /// The primary use for this is to update the metadata for existing uploaded files, this
    /// can only be used for images.
    async fn set_attachment_hash_animated(&self, hash: &str, animated: bool) -> Result<()>;

    /// Delete attachment hash by id.
    async fn delete_attachment_hash(&self, id: &str) -> Result<()>;

    /// Delete the attachment hash with this id only while it still points at
    /// this path. Whether a record was removed.
    async fn delete_attachment_hash_at(&self, id: &str, path: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use iso8601_timestamp::Timestamp;

    #[allow(unused_imports)]
    use crate::{AbstractAttachmentHashes, AbstractMigrations, FileHash, Metadata};

    fn copy(path: &str) -> FileHash {
        FileHash {
            id: "deleteat.m4a".to_owned(),
            processed_hash: "deleteat.m4a".to_owned(),
            created_at: Timestamp::now_utc(),
            bucket_id: "bucket".to_owned(),
            path: path.to_owned(),
            iv: "iv".to_owned(),
            metadata: Metadata::Audio { duration_ms: None },
            content_type: "audio/mp4".to_owned(),
            size: 10,
        }
    }

    /// REFERENCE and MONGODB (TEST_DB): the record goes only while it still
    /// points at the path asked for
    #[tokio::test]
    async fn delete_at_path_removes_only_a_matching_record() {
        let db = crate::DatabaseInfo::Test("file_hash_delete_at".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        if let crate::Database::MongoDb(_) = &db {
            db.drop_database().await;
        }
        db.insert_attachment_hash(&copy("deleteat.m4a.NEW")).await.unwrap();

        // another path: nothing removed
        assert!(!db.delete_attachment_hash_at("deleteat.m4a", "deleteat.m4a.OLD").await.unwrap());
        assert_eq!(
            db.fetch_attachment_hash("deleteat.m4a").await.unwrap().path,
            "deleteat.m4a.NEW"
        );
        // the same path: removed, once
        assert!(db.delete_attachment_hash_at("deleteat.m4a", "deleteat.m4a.NEW").await.unwrap());
        assert!(db.fetch_attachment_hash("deleteat.m4a").await.is_err());
        assert!(!db.delete_attachment_hash_at("deleteat.m4a", "deleteat.m4a.NEW").await.unwrap());
    }
}
