//! Clear the stored values of Zrzur Desk forms (`zd:` custom ids) two minutes
//! after they were submitted, so a sign-in code that no desk route read does
//! not stay for the full 15-minute life of the interaction (DK-04).

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{Database, Interaction, AMQP};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _amqp: AMQP) -> Result<()> {
    loop {
        let cleared = db
            .clear_stale_desk_modal_fields(Interaction::desk_fields_expiry_cutoff(Timestamp::now_utc()))
            .await?;
        if cleared > 0 {
            log::info!("Cleared the stored fields of {cleared} Zrzur Desk forms");
        }

        sleep(Duration::from_secs(60)).await
    }
}
