use revolt_result::Result;

use crate::{events::client::EventV1, Database};

/// Longest sound a soundboard plays (clients also stop playing here)
pub const SOUNDBOARD_MAX_DURATION_MS: u32 = 5_200;

/// Sounds a server has without Wings
pub const SOUNDBOARD_BASE_SOUNDS: usize = 8;
/// Sounds each Wings level adds
pub const SOUNDBOARD_SOUNDS_PER_WINGS_LEVEL: usize = 8;
/// Wings levels that add sounds
pub const SOUNDBOARD_WINGS_LEVELS: usize = 3;

fn default_volume() -> u8 {
    100
}

auto_derived_partial!(
    /// A sound of a server's soundboard. Its id is the id of its file
    /// (tag `sounds`).
    pub struct SoundboardSound {
        /// Unique id (the file id)
        #[serde(rename = "_id")]
        pub id: String,
        /// Server it belongs to
        pub server: String,
        /// Who added it
        pub creator_id: String,
        /// Name (1-32 characters)
        pub name: String,
        /// An emoji shown with it (a unicode emoji or a custom emoji id)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub emoji: Option<String>,
        /// Length in milliseconds, measured by the uploader's client
        pub duration_ms: u32,
        /// Volume it plays at, 1-100
        #[serde(default = "default_volume")]
        pub volume: u8,
    },
    "PartialSoundboardSound"
);

/// Most sounds a server of this Wings level may have
pub fn soundboard_limit(wings_level: usize) -> usize {
    SOUNDBOARD_BASE_SOUNDS
        + SOUNDBOARD_SOUNDS_PER_WINGS_LEVEL * wings_level.min(SOUNDBOARD_WINGS_LEVELS)
}

#[allow(clippy::disallowed_methods)]
impl SoundboardSound {
    /// Save a new sound and tell the server. The limit is checked after the
    /// insert, so two uploads at once can't both take the last place: the
    /// one over the limit is taken back. Returns false when it was.
    pub async fn create(&self, db: &Database, max: usize) -> Result<bool> {
        db.insert_soundboard_sound(self).await?;
        if db.count_soundboard_sounds(&self.server).await? > max {
            db.delete_soundboard_sound(&self.id).await?;
            return Ok(false);
        }
        EventV1::SoundboardSoundCreate(self.clone().into())
            .p(self.server.clone())
            .await;
        Ok(true)
    }

    /// Change a sound and tell the server
    pub async fn update(&mut self, db: &Database, partial: PartialSoundboardSound) -> Result<()> {
        self.apply_options(partial.clone());
        db.update_soundboard_sound(&self.id, &partial).await?;
        EventV1::SoundboardSoundUpdate(self.clone().into())
            .p(self.server.clone())
            .await;
        Ok(())
    }

    /// Remove a sound (its file goes too) and tell the server
    pub async fn delete(self, db: &Database) -> Result<()> {
        db.delete_soundboard_sound(&self.id).await?;
        db.mark_attachment_as_deleted(&self.id).await?;
        EventV1::SoundboardSoundDelete {
            id: self.id,
            server: self.server.clone(),
        }
        .p(self.server)
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::soundboard_limit;

    #[test]
    fn wings_give_more_sounds() {
        assert_eq!(soundboard_limit(0), 8);
        assert_eq!(soundboard_limit(1), 16);
        assert_eq!(soundboard_limit(3), 32);
        // nothing past the top level
        assert_eq!(soundboard_limit(9), 32);
    }
}
