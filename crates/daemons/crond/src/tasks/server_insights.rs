//! Rolls the messages of each finished hour into the servers' insight rows,
//! closes finished days (author ids dropped, their number kept) and deletes
//! rows past the retention. The work is `ServerInsightDay::run_pass`; it
//! notes each hour done, so a restart goes on where it stopped.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, ServerInsightDay};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        match ServerInsightDay::run_pass(&db, Timestamp::now_utc()).await {
            Ok(report) if report != Default::default() => {
                log::info!("Server insights: {report:?}");
            }
            Ok(_) => {}
            // the queries failed: the hour is not noted, the next pass retries
            Err(error) => log::error!("Server insights pass failed: {error:?}"),
        }
        // an hour is rolled up within five minutes of its end
        sleep(Duration::from_secs(300)).await;
    }
}
