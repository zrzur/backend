//! Starts scheduled server events at their time and completes active ones
//! at their end (or `DEFAULT_LENGTH_HOURS` after the start without an end).
//! Starting announces the event; completing a repeating one schedules the
//! next. Every change is a single status step in the database, so a manager
//! acting at the same moment never makes it happen twice.

use std::time::Duration;

use iso8601_timestamp::Timestamp;
use revolt_database::Database;
use revolt_result::{ErrorType, Result};
use tokio::time::sleep;

/// One pass: start what is due, complete what has ended
pub async fn run_once(db: &Database, now: Timestamp) -> Result<(usize, usize)> {
    let mut started = 0;
    for mut event in db.fetch_server_events_to_start(now).await? {
        // the server is gone: nothing to start
        if let Err(error) = db.fetch_server(&event.server).await {
            if matches!(error.error_type, ErrorType::NotFound) {
                db.delete_server_event(&event.id).await?;
            }
            continue;
        }
        if event.start(db).await? {
            started += 1;
        }
    }

    let mut completed = 0;
    for mut event in db.fetch_active_server_events().await? {
        if event.completes_at() <= now && event.complete(db).await? {
            completed += 1;
        }
    }
    Ok((started, completed))
}

pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        if revolt_config::config().await.features.server_events_enabled {
            let (started, completed) = run_once(&db, Timestamp::now_utc()).await?;
            if started + completed > 0 {
                log::info!("Server events: started {started}, completed {completed}.");
            }
        }
        sleep(Duration::from_secs(30)).await;
    }
}
