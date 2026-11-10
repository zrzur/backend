//! Rolls the messages of each finished hour into the servers' insight rows,
//! closes finished days (author ids dropped, their number kept) and deletes
//! rows past the retention, and moves voice time noted in Redis into the
//! rows. The work is `ServerInsightDay::run_pass`; it notes each hour done,
//! so a restart goes on where it stopped.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, ServerInsightDay};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        // each step logs its own failure; an hour that failed is not noted,
        // so the next pass tries it again
        let report = ServerInsightDay::run_pass(&db, Timestamp::now_utc()).await;
        if report.failures > 0 {
            log::warn!("Server insights: {report:?}");
        } else if report != Default::default() {
            log::info!("Server insights: {report:?}");
        }
        // an hour is rolled up within five minutes of its end
        sleep(Duration::from_secs(300)).await;
    }
}
