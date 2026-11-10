use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, Message};
use revolt_result::Result;
use tokio::time::sleep;

/// Close polls whose time is up and post their "poll ended" reply. A vote
/// on such a poll is already refused before this runs; this makes the
/// result final for everyone within a minute.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        loop {
            let closed = Message::end_expired_polls(&db, Timestamp::now_utc(), 100).await?;
            if closed > 0 {
                log::info!("Closed {closed} polls.");
            }
            if closed < 100 {
                break;
            }
        }

        sleep(Duration::from_secs(30)).await
    }
}
