use revolt_result::Result;

use crate::FileHash;
use crate::MongoDb;

use super::AbstractAttachmentHashes;

static COL: &str = "attachment_hashes";

#[async_trait]
impl AbstractAttachmentHashes for MongoDb {
    /// Insert a new attachment hash into the database.
    async fn insert_attachment_hash(&self, hash: &FileHash) -> Result<()> {
        query!(self, insert_one, COL, &hash).map(|_| ())
    }

    /// Fetch an attachment hash entry by sha256 hash.
    async fn fetch_attachment_hash(&self, hash: &str) -> Result<FileHash> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "$or": [
                    {"_id": hash},
                    {"processed_hash": hash}
                ]
            }
        )?
        .ok_or_else(|| create_error!(NotFound))
    }

    /// Which of these hashes are stored: one `$in` on `_id`, ids only
    async fn fetch_stored_attachment_hash_ids(&self, ids: &[String]) -> Result<Vec<String>> {
        #[derive(serde::Deserialize)]
        struct Id {
            #[serde(rename = "_id")]
            id: String,
        }
        let found: Vec<Id> = self
            .find_with_options(
                COL,
                doc! { "_id": { "$in": ids }, "iv": { "$ne": "" } },
                mongodb::options::FindOptions::builder()
                    .projection(doc! { "_id": 1_i32 })
                    .limit(ids.len() as i64)
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        Ok(found.into_iter().map(|hash| hash.id).collect())
    }

    /// Update an attachment hash nonce value.
    async fn set_attachment_hash_nonce(&self, hash: &str, nonce: &str) -> Result<()> {
        self.col::<FileHash>(COL)
            .update_one(
                doc! {
                    "_id": hash
                },
                doc! {
                    "$set": {
                        "iv": nonce
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Updates the attachments animated metadata value.
    ///
    /// The primary use for this is to update the metadata for existing uploaded files, this
    /// can only be used for images.
    async fn set_attachment_hash_animated(&self, hash: &str, animated: bool) -> Result<()> {
        self.col::<FileHash>(COL)
            .update_one(
                doc! {
                    "_id": hash,
                    "metadata.type": "Image",
                    "metadata.animated": { "$exists": false },
                },
                doc! {
                    "$set": {
                        "metadata.animated": animated
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Delete attachment hash by id.
    async fn delete_attachment_hash(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, id).map(|_| ())
    }
}
