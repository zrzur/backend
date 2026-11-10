use revolt_result::Result;

use crate::{PartialSoundboardSound, SoundboardSound};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractSoundboard: Sync + Send {
    /// Insert a sound
    async fn insert_soundboard_sound(&self, sound: &SoundboardSound) -> Result<()>;

    /// Fetch a sound by its id
    async fn fetch_soundboard_sound(&self, id: &str) -> Result<SoundboardSound>;

    /// Sounds of a server (bounded by the soundboard limit), oldest first
    async fn fetch_soundboard_sounds(&self, server_id: &str) -> Result<Vec<SoundboardSound>>;

    /// Number of sounds of a server
    async fn count_soundboard_sounds(&self, server_id: &str) -> Result<usize>;

    /// Change a sound
    async fn update_soundboard_sound(
        &self,
        id: &str,
        partial: &PartialSoundboardSound,
    ) -> Result<()>;

    /// Remove a sound
    async fn delete_soundboard_sound(&self, id: &str) -> Result<()>;
}
