use revolt_result::Result;

use crate::ReferenceDb;
use crate::{PartialSoundboardSound, SoundboardSound};

use super::AbstractSoundboard;

#[async_trait]
impl AbstractSoundboard for ReferenceDb {
    async fn insert_soundboard_sound(&self, sound: &SoundboardSound) -> Result<()> {
        let mut sounds = self.soundboard_sounds.lock().await;
        if sounds.contains_key(&sound.id) {
            return Err(create_database_error!("insert", "soundboard_sounds"));
        }
        sounds.insert(sound.id.clone(), sound.clone());
        Ok(())
    }

    async fn fetch_soundboard_sound(&self, id: &str) -> Result<SoundboardSound> {
        self.soundboard_sounds
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_soundboard_sounds(&self, server_id: &str) -> Result<Vec<SoundboardSound>> {
        let mut sounds: Vec<SoundboardSound> = self
            .soundboard_sounds
            .lock()
            .await
            .values()
            .filter(|sound| sound.server == server_id)
            .cloned()
            .collect();
        sounds.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(sounds)
    }

    async fn count_soundboard_sounds(&self, server_id: &str) -> Result<usize> {
        Ok(self
            .soundboard_sounds
            .lock()
            .await
            .values()
            .filter(|sound| sound.server == server_id)
            .count())
    }

    async fn update_soundboard_sound(
        &self,
        id: &str,
        partial: &PartialSoundboardSound,
    ) -> Result<()> {
        let mut sounds = self.soundboard_sounds.lock().await;
        let sound = sounds.get_mut(id).ok_or_else(|| create_error!(NotFound))?;
        sound.apply_options(partial.clone());
        Ok(())
    }

    async fn delete_soundboard_sound(&self, id: &str) -> Result<()> {
        self.soundboard_sounds
            .lock()
            .await
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| create_error!(NotFound))
    }
}
