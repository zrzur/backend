//! Once a day (and once at start): check every server that holds a custom
//! invite link (zrzur.cc/<name>) against the rule of `features.vanity`.
//! A server that stopped meeting it is marked and its owner gets a notice
//! from the official Zrzur account; the link keeps working for
//! `notice_days` days, then the name is released. A server that meets the
//! rule again is unmarked. Official and partner servers always meet it.
//! All of it lives in `revolt_database::models::billing::vanity`.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::{send_vanity_notices, sync_vanity_invites, Database, AMQP};
use revolt_result::Result;
use tokio::time::sleep;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// One daily pass; split out so tests can run it without waiting a day
pub async fn run_once(db: &Database, amqp: Option<&AMQP>, now: Timestamp) -> Result<(usize, usize, usize, usize)> {
    // counted again here, not taken from a claim's recent count
    let report = sync_vanity_invites(db, now, true).await?;
    let notices = send_vanity_notices(db, amqp, now).await?;
    Ok((report.marked, notices, report.recovered, report.released))
}

pub async fn task(db: Database, amqp: AMQP) -> Result<()> {
    loop {
        match run_once(&db, Some(&amqp), Timestamp::now_utc()).await {
            Ok((marked, notices, recovered, released)) => log::info!(
                "vanity links: {marked} marked, {notices} notices, {recovered} meet the rule again, {released} released"
            ),
            // the next day's pass picks up whatever this one missed
            Err(error) => log::error!("vanity links job failed: {error:?}"),
        }
        sleep(DAY).await;
    }
}
