use revolt_result::Result;

use crate::File;

use super::FileUsedFor;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractAttachments: Sync + Send {
    /// Insert attachment into database.
    async fn insert_attachment(&self, attachment: &File) -> Result<()>;

    /// Fetch an attachment by its id.
    async fn fetch_attachment(&self, tag: &str, file_id: &str) -> Result<File>;

    /// Fetch all deleted attachments.
    async fn fetch_deleted_attachments(&self) -> Result<Vec<File>>;

    /// Deleted files kept as evidence that may still be at the CDN.
    async fn fetch_reported_files_to_purge(&self) -> Result<Vec<File>>;

    /// Note that these files left the CDN.
    async fn mark_attachments_as_purged(&self, ids: &[String]) -> Result<()>;

    /// Fetch all dangling attachments.
    async fn fetch_dangling_files(&self) -> Result<Vec<File>>;

    /// Every file claimed by a community item that is not deleted yet: the
    /// current image, a waiting revision and images it used before.
    async fn fetch_community_item_files(&self, item_id: &str) -> Result<Vec<File>>;

    /// Every record of a forwarded file: the record `root` and the copies
    /// made from it (all share the stored `hash`)
    async fn fetch_file_family(&self, hash: &str, root: &str) -> Result<Vec<File>>;

    /// Count references to a given hash.
    async fn count_file_hash_references(&self, hash: &str) -> Result<usize>;

    /// Find an attachment by its details and mark it as used by a given parent.
    async fn find_and_use_attachment(
        &self,
        id: &str,
        tag: &str,
        used_for: FileUsedFor,
        uploader_id: String,
    ) -> Result<File>;

    /// Mark an attachment as having been reported.
    async fn mark_attachment_as_reported(&self, id: &str) -> Result<()>;

    /// Mark an attachment as having been deleted.
    async fn mark_attachment_as_deleted(&self, id: &str) -> Result<()>;

    /// Mark multiple attachments as having been deleted.
    async fn mark_attachments_as_deleted(&self, ids: &[String]) -> Result<()>;

    /// Delete the attachment entry.
    async fn delete_attachment(&self, id: &str) -> Result<()>;
}
