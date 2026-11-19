use revolt_models::v0::{VoiceCapture, VoiceCaptureKind};
use revolt_result::Result;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractVoiceCaptures: Sync + Send {
    /// Insert a clip or an episode
    async fn insert_voice_capture(&self, capture: &VoiceCapture) -> Result<()>;

    /// Fetch one by its id (removed ones included)
    async fn fetch_voice_capture(&self, id: &str) -> Result<VoiceCapture>;

    /// Clips and episodes made in a voice room, newest first, without the
    /// removed ones
    async fn fetch_voice_captures_in_channel(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>>;

    /// Everything of one kind saved since `since_id` by a person (clips) or
    /// in a server (episodes). Removed ones are included: they still count
    /// towards the month.
    async fn fetch_voice_captures_since(
        &self,
        kind: VoiceCaptureKind,
        author_id: Option<&str>,
        server_id: Option<&str>,
        since_id: &str,
    ) -> Result<Vec<VoiceCapture>>;

    /// Episodes saved before `before_id` that are not removed yet, oldest
    /// first
    async fn fetch_voice_episodes_before(
        &self,
        before_id: &str,
        limit: usize,
    ) -> Result<Vec<VoiceCapture>>;

    /// Mark as removed: the file, the message, the name and the list of
    /// people go; the length stays for the count of the month
    async fn remove_voice_capture(&self, id: &str) -> Result<()>;
}
