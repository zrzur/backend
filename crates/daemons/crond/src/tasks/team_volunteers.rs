//! Look after the volunteers once a day at 00:30 UTC (and once at start, to
//! catch up): count yesterday (and up to six missed days before), follow
//! roles taken away by hand, tell the staff about trials that ended, delete
//! what is past its time, and on the first days of a month apply the gentle
//! monthly rule. All of it lives in `revolt_database::util::team_activity`.
//! While `volunteers.enabled` is off only the retention runs (identities
//! and activity days past their time are deleted all the same).

use std::time::Duration;

use revolt_config::config;
use revolt_database::{
    staff_now,
    util::team_activity::{run_daily, until_next_run, OfficialMessenger},
    Database, AMQP,
};
use revolt_result::Result;
use tokio::time::sleep;

pub async fn task(db: Database, amqp: AMQP) -> Result<()> {
    loop {
        let settings = config().await.features.team_applications;
        let messenger = OfficialMessenger {
            db: &db,
            amqp: Some(&amqp),
        };
        match run_daily(&db, &settings, &messenger, staff_now()).await {
            Ok(run) if run.ran => log::info!(
                "team volunteers: {} counting notices, {} days counted, {} ended (role removed), {} trial notices, {} aways ended, {} reminders, {} inactive, {} active again, {} months not judged (days missing)",
                run.counting_notices,
                run.days_counted,
                run.ended,
                run.trial_notices,
                run.away_ended,
                run.reminders,
                run.marked_inactive,
                run.back_to_active,
                run.months_skipped
            ),
            Ok(_) => {}
            // the next run counts the missing days and finishes the rest
            Err(error) => log::error!("team volunteers job failed: {error:?}"),
        }

        let wait = until_next_run(staff_now()).max(60_000);
        sleep(Duration::from_millis(wait as u64)).await
    }
}
