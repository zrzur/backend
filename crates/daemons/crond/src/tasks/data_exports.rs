use std::time::Duration;

use revolt_database::Database;
use revolt_result::Result;
use tokio::time::sleep;

/// Build the data exports people asked for ("Download my data") and remove
/// the ones whose link has run out (7 days).
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        for mut export in db.fetch_pending_data_exports().await? {
            if let Err(error) = export.process(&db).await {
                log::error!("Data export for {} failed: {error:?}", export.id);
            } else {
                log::info!("Data export for {} is ready.", export.id);
            }
        }

        for export in db.fetch_expired_data_exports().await? {
            db.remove_data_exports_for_user(&export.id).await?;
            log::info!("Removed expired data export for {}.", export.id);
        }

        sleep(Duration::from_secs(30)).await
    }
}
