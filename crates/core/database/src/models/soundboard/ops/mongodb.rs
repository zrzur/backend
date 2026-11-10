use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::MongoDb;
use crate::{PartialSoundboardSound, SoundboardSound};

use super::AbstractSoundboard;

static COL: &str = "soundboard_sounds";

/// No server has more sounds than this, whatever its level
const MOST_SOUNDS: i64 = 64;

#[async_trait]
impl AbstractSoundboard for MongoDb {
    async fn insert_soundboard_sound(&self, sound: &SoundboardSound) -> Result<()> {
        query!(self, insert_one, COL, &sound).map(|_| ())
    }

    async fn fetch_soundboard_sound(&self, id: &str) -> Result<SoundboardSound> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Uses the `server_id` index (`server`, `_id`)
    async fn fetch_soundboard_sounds(&self, server_id: &str) -> Result<Vec<SoundboardSound>> {
        self.find_with_options(
            COL,
            doc! { "server": server_id },
            FindOptions::builder()
                .sort(doc! { "server": 1_i32, "_id": 1_i32 })
                .limit(MOST_SOUNDS)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn count_soundboard_sounds(&self, server_id: &str) -> Result<usize> {
        self.col::<SoundboardSound>(COL)
            .count_documents(doc! { "server": server_id })
            .await
            .map(|count| count as usize)
            .map_err(|_| create_database_error!("count_documents", COL))
    }

    async fn update_soundboard_sound(
        &self,
        id: &str,
        partial: &PartialSoundboardSound,
    ) -> Result<()> {
        query!(self, update_one_by_id, COL, id, partial, vec![], None).map(|_| ())
    }

    async fn delete_soundboard_sound(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, id).map(|_| ())
    }
}
