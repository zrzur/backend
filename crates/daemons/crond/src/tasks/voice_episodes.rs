use std::time::Duration;

use revolt_database::{sweep_old_voice_episodes, Database};
use revolt_result::Result;
use tokio::time::sleep;

/// Remove recorded episodes older than `features.voice_capture.episode_retention_days`
/// (their message and file go, the length stays for the count of its month).
/// Does nothing while recorded rooms are off or the setting is 0.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        if revolt_config::config().await.features.voice_recordings_enabled {
            loop {
                let removed = sweep_old_voice_episodes(&db).await?;
                if removed > 0 {
                    log::info!("Removed {removed} old voice episodes.");
                }
                if removed < 100 {
                    break;
                }
            }
        }

        sleep(Duration::from_secs(3600)).await
    }
}
