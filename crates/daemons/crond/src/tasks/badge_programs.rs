//! Badge programs, every six hours:
//! - weekly activity of large servers for Partner (counts only, from
//!   message and read-position ids; never content; rows kept 26 weeks);
//! - retention: evidence and ownership codes, records and stale drafts
//!   past their dates are deleted;
//! - rechecks: partnerships past their 180 days are flagged for a person
//!   (never ended automatically);
//! - a safety net for Verified Server Owner and Verified Bot Developer: each
//!   holder is checked against the records again (through the badge gate).

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{activity_pass, epoch_ms, program_upkeep, resync_verified_badges, Database};
use revolt_result::Result;
use tokio::time::sleep;

const EVERY_SECS: u64 = 6 * 60 * 60;

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let programs = config().await.api.badge_programs;
        let now = epoch_ms(Timestamp::now_utc());
        match activity_pass(&db, &programs, now).await {
            Ok(report) if report.failures > 0 => log::warn!("Badge program activity: {report:?}"),
            Ok(report) if report.weeks > 0 || report.retention > 0 => log::info!("Badge program activity: {report:?}"),
            Ok(_) => {}
            Err(error) => log::error!("Badge program activity: {error:?}"),
        }
        match program_upkeep(&db, &programs, now).await {
            Ok(report) if report != Default::default() => log::info!("Badge program upkeep: {report:?}"),
            Ok(_) => {}
            Err(error) => log::error!("Badge program upkeep: {error:?}"),
        }
        if let Err(error) = resync_verified_badges(&db).await {
            log::error!("Verified badges resync: {error:?}");
        }
        sleep(Duration::from_secs(EVERY_SECS)).await;
    }
}
