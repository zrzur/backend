use mongodb::options::FindOptions;
use revolt_models::v0::{VoiceCapture, VoiceCaptureKind};
use revolt_result::Result;

use crate::MongoDb;

use super::AbstractVoiceCaptures;

static COL: &str = "voice_captures";

/// Most records the count of one month reads
const MOST_PER_MONTH: i64 = 2_000;

fn kind_name(kind: VoiceCaptureKind) -> &'static str {
    match kind {
        VoiceCaptureKind::Clip => "Clip",
        VoiceCaptureKind::Episode => "Episode",
    }
}

#[async_trait]
impl AbstractVoiceCaptures for MongoDb {
    async fn insert_voice_capture(&self, capture: &VoiceCapture) -> Result<()> {
        query!(self, insert_one, COL, &capture).map(|_| ())
    }

    async fn fetch_voice_capture(&self, id: &str) -> Result<VoiceCapture> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_voice_captures_in_channel(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>> {
        self.find_with_options(
            COL,
            doc! { "channel": channel_id, "removed": { "$ne": true } },
            FindOptions::builder()
                .sort(doc! { "_id": -1_i32 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_voice_captures_since(
        &self,
        kind: VoiceCaptureKind,
        author_id: Option<&str>,
        server_id: Option<&str>,
        since_id: &str,
    ) -> Result<Vec<VoiceCapture>> {
        let mut filter = doc! { "kind": kind_name(kind), "_id": { "$gte": since_id } };
        if let Some(author) = author_id {
            filter.insert("author", author);
        }
        if let Some(server) = server_id {
            filter.insert("server", server);
        }
        self.find_with_options(
            COL,
            filter,
            FindOptions::builder().limit(MOST_PER_MONTH).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn fetch_voice_episodes_before(
        &self,
        before_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>> {
        self.find_with_options(
            COL,
            doc! {
                "kind": "Episode",
                "removed": { "$ne": true },
                "_id": { "$lt": before_id }
            },
            FindOptions::builder()
                .sort(doc! { "_id": 1_i32 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn remove_voice_capture(&self, id: &str) -> Result<()> {
        self.col::<VoiceCapture>(COL)
            .update_one(
                doc! { "_id": id },
                doc! {
                    "$set": { "removed": true, "included": [], "file": "", "message": "" },
                    "$unset": { "title": 1_i32 }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }
}
