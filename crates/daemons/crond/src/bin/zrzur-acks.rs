//! Persist read receipts without starting scheduled deletion tasks.
#[path = "../tasks/acks.rs"]
mod acks;

use std::time::Duration;

use revolt_config::configure;
use revolt_database::{DatabaseInfo, AMQP};

#[tokio::main]
async fn main() {
    configure!(crond);
    let db = DatabaseInfo::Auto.connect().await.expect("database");
    let amqp = AMQP::new_auto().await;

    loop {
        if let Err(error) = acks::task(db.clone(), amqp.clone()).await {
            log::error!("Read receipt worker failed: {error:?}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
