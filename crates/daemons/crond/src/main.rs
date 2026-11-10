use std::{future::Future, panic::AssertUnwindSafe, time::Duration};

use futures::FutureExt;
use revolt_config::{capture_error, configure};
use revolt_database::{Database, DatabaseInfo, AMQP};
use revolt_result::Result;
use tasks::*;
use tokio::{join, time::sleep};

pub mod tasks;

pub async fn cron_task_wrapper<Fut: Future<Output = Result<()>>>(
    func: fn(Database, AMQP) -> Fut,
    db: Database,
    amqp: AMQP,
) {
    loop {
        let wrapper = AssertUnwindSafe(func(db.clone(), amqp.clone()));

        match wrapper.catch_unwind().await {
            Ok(Ok(())) => {
                log::error!("cron unexpectedly finshed, Retrying after 60s");
            }
            Ok(Err(error)) => {
                log::error!("cron task failed unexpectedly: {error:?}\nRetrying after 60s");
                capture_error(&error);
            }
            _ => {
                log::error!("cron task failed unexpectedly\nRetrying after 60s");
            }
        }

        sleep(Duration::from_secs(60)).await;
    }
}

#[tokio::main]
async fn main() {
    configure!(crond);

    let db = DatabaseInfo::Auto.connect().await.expect("database");
    let amqp = AMQP::new_auto().await;

    // a second crond beside a running one (a test stack) runs only the
    // insights roll-up, so no other task runs twice
    if let Ok(only) = std::env::var("CROND_ONLY") {
        if only != "server_insights" {
            log::warn!("CROND_ONLY={only:?} is not a task this crond can run alone; running every task");
        } else {
            log::warn!("CROND_ONLY=server_insights: only the insights roll-up runs here, no other crond task");
            cron_task_wrapper(server_insights::task, db, amqp).await;
            return;
        }
    }

    join!(
        cron_task_wrapper(file_deletion::task, db.clone(), amqp.clone()),
        cron_task_wrapper(prune_dangling_files::task, db.clone(), amqp.clone()),
        cron_task_wrapper(prune_members::task, db.clone(), amqp.clone()),
        cron_task_wrapper(delete_accounts::task, db.clone(), amqp.clone()),
        cron_task_wrapper(acks::task, db.clone(), amqp.clone()),
        cron_task_wrapper(delete_expired_invites::task, db.clone(), amqp.clone()),
        cron_task_wrapper(safety_retention::task, db.clone(), amqp.clone()),
        cron_task_wrapper(data_exports::task, db.clone(), amqp.clone()),
        cron_task_wrapper(expire_sessions::task, db.clone(), amqp.clone()),
        cron_task_wrapper(end_polls::task, db.clone(), amqp.clone()),
        cron_task_wrapper(server_events::task, db.clone(), amqp.clone()),
        cron_task_wrapper(server_insights::task, db.clone(), amqp.clone()),
    );
}
