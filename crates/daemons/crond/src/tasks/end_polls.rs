use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, Message};
use revolt_result::Result;
use tokio::time::sleep;

/// Close polls whose time is up and post their "poll ended" reply, and post
/// the reply of any closed poll that still lacks one (a close whose reply
/// failed). A vote on an expired poll is already refused before this runs;
/// this makes the result final for everyone within a minute. One poll
/// failing is logged inside the pass and the others go on.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        loop {
            let finished = Message::close_due_polls(&db, Timestamp::now_utc(), 100).await?;
            if finished > 0 {
                log::info!("Finished {finished} polls.");
            }
            if finished < 100 {
                break;
            }
        }

        sleep(Duration::from_secs(30)).await
    }
}
