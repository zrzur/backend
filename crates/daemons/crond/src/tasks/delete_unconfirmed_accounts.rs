use std::time::Duration;

use revolt_config::config;
use revolt_database::{Account, Database};
use revolt_result::Result;
use tokio::time::sleep;

/// Accounts made from an invite link (`POST /auth/account/join`) have
/// `features.join_confirm_hours` to confirm their e-mail; after that they are
/// removed (`Account::remove_unconfirmed`). Looked at every quarter of an hour.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let hours = config().await.features.join_confirm_hours as i64;
        let count = Account::remove_unconfirmed(&db, hours).await?;
        if count > 0 {
            log::info!("Removed {count} accounts that never confirmed their e-mail.");
        }

        sleep(Duration::from_secs(15 * 60)).await
    }
}
