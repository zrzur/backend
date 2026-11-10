//! Starts scheduled server events at their time, completes active ones at
//! their end (or `DEFAULT_LENGTH_HOURS` after the start without an end),
//! cancels events whose channel is gone and gives repeating events their
//! next occurrence. The work is `ServerEvent::run_pass`; one event failing
//! is logged and the others go on.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, ServerEvent};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        if revolt_config::config().await.features.server_events_enabled {
            match ServerEvent::run_pass(&db, Timestamp::now_utc()).await {
                Ok(report) if report != Default::default() => {
                    log::info!("Server events: {report:?}");
                }
                Ok(_) => {}
                // the queries themselves failed: try again on the next pass
                Err(error) => log::error!("Server events pass failed: {error:?}"),
            }
        }
        sleep(Duration::from_secs(30)).await;
    }
}
