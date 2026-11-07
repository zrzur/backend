use std::time::Duration;

use revolt_database::{Database, Session};
use revolt_result::Result;
use tokio::time::sleep;

/// Remove sessions nobody has used for `api.sessions.idle_days`, with the
/// normal delete, so clients are told and push subscriptions go too.
///
/// Sessions the expiry migration has not reached are left alone, so this is
/// safe when crond starts before delta. A session is refused on the auth path
/// from the moment it expires anyway.
pub async fn task(db: Database, _: revolt_database::AMQP) -> Result<()> {
    loop {
        let count = Session::delete_idle(&db).await?;
        log::info!("Closed {count} idle sessions.");

        sleep(Duration::from_hours(24)).await
    }
}
