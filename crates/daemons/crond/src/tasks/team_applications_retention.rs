//! Delete applications to join the team once their time is up: a rejected
//! one after `retention_days`, an accepted one after
//! `accepted_retention_days`, a withdrawn one (already without answers)
//! after an hour. The date is written on the record when it is decided
//! (`purge_at`); open applications have none and stay.

use std::time::Duration;

use revolt_database::{staff_now, Database, AMQP};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _amqp: AMQP) -> Result<()> {
    loop {
        let deleted = db.delete_team_applications_due(staff_now()).await?;
        if deleted > 0 {
            log::info!("Deleted {deleted} team applications whose time was up");
        }

        sleep(Duration::from_secs(15 * 60)).await
    }
}
