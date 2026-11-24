//! Community Builder and Big Community: counts every server's genuine human
//! members every six hours and gives each server's owner what it earned
//! (`revolt_database::community_badge_pass`). Metadata only: join times,
//! account age and flags, confirmed e-mail. Never a message.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{community_badge_pass, epoch_ms, CommunityThresholds, Database, COMMUNITY_PASS_EVERY_SECS};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let now = epoch_ms(Timestamp::now_utc());
        match community_badge_pass(&db, CommunityThresholds::default(), now).await {
            Ok(report) if report.failures > 0 => log::warn!("Community badges: {report:?}"),
            Ok(report) if report.granted > 0 || report.held > 0 => log::info!("Community badges: {report:?}"),
            Ok(_) => {}
            Err(error) => log::error!("Community badges: {error:?}"),
        }
        sleep(Duration::from_secs(COMMUNITY_PASS_EVERY_SECS)).await;
    }
}
