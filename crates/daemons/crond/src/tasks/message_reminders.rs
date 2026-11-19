//! Delivers the reminders people set on messages when their time comes, and
//! removes delivered ones the setting no longer keeps. The work is
//! `deliver_due_reminders`; one reminder failing is logged there and the
//! others go on. A reminder arrives within this task's pause of its time.

use std::time::Duration;

use revolt_database::{deliver_due_reminders, now_ms, Database};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        if revolt_config::config().await.features.saved_reminders.enabled {
            match deliver_due_reminders(&db, now_ms()).await {
                Ok(report) if report != Default::default() => {
                    log::info!("Message reminders: {report:?}");
                }
                Ok(_) => {}
                // the queries themselves failed: try again on the next pass
                Err(error) => log::error!("Message reminders pass failed: {error:?}"),
            }
        }
        sleep(Duration::from_secs(20)).await;
    }
}
