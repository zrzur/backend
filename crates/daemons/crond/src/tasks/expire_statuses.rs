use std::time::Duration;

use revolt_database::{Database, User};
use revolt_result::Result;
use tokio::time::sleep;

/// Clear custom statuses whose expiry has passed and tell other clients.
/// Reads already hide an expired status, so a minute's delay here is never
/// seen by anyone; this keeps storage and open clients in step.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let cleared = User::clear_expired_statuses(&db).await?;
        if cleared > 0 {
            log::info!("Cleared {cleared} expired statuses.");
        }

        sleep(Duration::from_secs(60)).await
    }
}
