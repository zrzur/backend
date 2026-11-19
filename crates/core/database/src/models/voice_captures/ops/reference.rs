use revolt_models::v0::{VoiceCapture, VoiceCaptureKind};
use revolt_result::Result;

use crate::ReferenceDb;

use super::AbstractVoiceCaptures;

#[async_trait]
impl AbstractVoiceCaptures for ReferenceDb {
    async fn insert_voice_capture(&self, capture: &VoiceCapture) -> Result<()> {
        let mut captures = self.voice_captures.lock().await;
        if captures.contains_key(&capture.id) {
            return Err(create_database_error!("insert", "voice_captures"));
        }
        captures.insert(capture.id.clone(), capture.clone());
        Ok(())
    }

    async fn fetch_voice_capture(&self, id: &str) -> Result<VoiceCapture> {
        self.voice_captures
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_voice_captures_in_channel(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>> {
        let mut captures: Vec<VoiceCapture> = self
            .voice_captures
            .lock()
            .await
            .values()
            .filter(|capture| capture.channel == channel_id && !capture.removed)
            .cloned()
            .collect();
        captures.sort_by(|a, b| b.id.cmp(&a.id));
        captures.truncate(limit);
        Ok(captures)
    }

    async fn fetch_voice_captures_since(
        &self,
        kind: VoiceCaptureKind,
        author_id: Option<&str>,
        server_id: Option<&str>,
        since_id: &str,
    ) -> Result<Vec<VoiceCapture>> {
        Ok(self
            .voice_captures
            .lock()
            .await
            .values()
            .filter(|capture| {
                capture.kind == kind
                    && capture.id.as_str() >= since_id
                    && author_id.map_or(true, |author| capture.author == author)
                    && server_id.map_or(true, |server| capture.server == server)
            })
            .cloned()
            .collect())
    }

    async fn fetch_voice_episodes_before(
        &self,
        before_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>> {
        let mut captures: Vec<VoiceCapture> = self
            .voice_captures
            .lock()
            .await
            .values()
            .filter(|capture| {
                capture.kind == VoiceCaptureKind::Episode
                    && !capture.removed
                    && capture.id.as_str() < before_id
            })
            .cloned()
            .collect();
        captures.sort_by(|a, b| a.id.cmp(&b.id));
        captures.truncate(limit);
        Ok(captures)
    }

    async fn remove_voice_capture(&self, id: &str) -> Result<()> {
        let mut captures = self.voice_captures.lock().await;
        let capture = captures.get_mut(id).ok_or_else(|| create_error!(NotFound))?;
        capture.removed = true;
        capture.included.clear();
        capture.file.clear();
        capture.message.clear();
        capture.title = None;
        Ok(())
    }
}
