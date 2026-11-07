//! Trust & Safety housekeeping:
//! - delete report evidence past its retention time (held evidence stays,
//!   and so does the evidence of child sexual abuse material reports),
//!   and let the normal file deletion collect files nothing refers to;
//! - lift account suspensions whose time ran out.

use std::time::Duration;

use revolt_database::{expire_safety_suspensions, safety_now, Database, AMQP};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, _amqp: AMQP) -> Result<()> {
    loop {
        let released = db.purge_expired_safety_evidence(safety_now()).await?;
        if !released.is_empty() {
            log::info!("Released {} reported files past evidence retention", released.len());
        }

        // names and e-mail addresses of people who reported without an
        // account go 12 months after their case was closed
        let cleared = db
            .clear_public_reporters(safety_now() - 365 * 24 * 60 * 60 * 1000)
            .await?;
        if cleared > 0 {
            log::info!("Removed the contact details of {cleared} public reporters of old cases");
        }

        let lifted = expire_safety_suspensions(&db).await?;
        if lifted > 0 {
            log::info!("Lifted {lifted} expired suspensions");
        }

        sleep(Duration::from_secs(15 * 60)).await
    }
}
