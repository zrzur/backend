//! Delete support tickets 12 months after they were closed, as the privacy
//! policy says (PR-14). Open and answered tickets stay; a reopened ticket
//! has no closing time and stays too.

use std::time::Duration;

use revolt_database::{safety_now, Database, AMQP, SUPPORT_KEEP_CLOSED_MS};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _amqp: AMQP) -> Result<()> {
    loop {
        let deleted = db
            .delete_support_tickets_closed_before(safety_now() - SUPPORT_KEEP_CLOSED_MS)
            .await?;
        if deleted > 0 {
            log::info!("Deleted {deleted} support tickets closed over 12 months ago");
        }

        sleep(Duration::from_secs(60 * 60)).await
    }
}
