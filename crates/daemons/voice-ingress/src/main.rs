use std::env;

use revolt_database::DatabaseInfo;
use revolt_database::{voice::VoiceClient, AMQP};
use revolt_result::Result;
use rocket::{build, routes, Config};
use std::net::Ipv4Addr;

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    revolt_config::configure!(voice_ingress);

    let amqp = AMQP::new_auto().await;

    let database = DatabaseInfo::Auto.connect().await.unwrap();
    let voice_client = VoiceClient::from_revolt_config().await;

    // Clear seats that LiveKit lost (restarts, missed webhooks)
    let reconcile_client = VoiceClient::from_revolt_config().await;
    if reconcile_client.is_enabled() {
        rocket::tokio::spawn(async move {
            let mut interval = rocket::tokio::time::interval(revolt_voice_ingress::reconcile::INTERVAL);
            loop {
                interval.tick().await;
                if let Err(error) =
                    revolt_voice_ingress::reconcile::reconcile(&reconcile_client).await
                {
                    log::warn!("voice reconcile failed: {error:?}");
                }
            }
        });
    }

    let _rocket = build()
        .manage(database)
        .manage(voice_client)
        .manage(amqp)
        .mount("/", routes![revolt_voice_ingress::api::ingress])
        .configure(Config {
            port: 8500,
            address: Ipv4Addr::new(0, 0, 0, 0).into(),
            ..Default::default()
        })
        .ignite()
        .await?
        .launch()
        .await?;

    Ok(())
}
