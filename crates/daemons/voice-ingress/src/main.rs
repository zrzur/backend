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
            port: listen_port(),
            address: Ipv4Addr::new(0, 0, 0, 0).into(),
            ..Default::default()
        })
        .ignite()
        .await?
        .launch()
        .await?;

    Ok(())
}

/// 8500, or ZRZUR_VOICE_INGRESS_PORT (a second voice-ingress beside the dev
/// stack's, e.g. for a branch's own LiveKit); a value that is no port is
/// ignored with a warning
fn listen_port() -> u16 {
    match std::env::var("ZRZUR_VOICE_INGRESS_PORT") {
        Ok(port) => port.parse().unwrap_or_else(|_| {
            log::warn!("ZRZUR_VOICE_INGRESS_PORT={port:?} is not a port; listening on 8500");
            8500
        }),
        Err(_) => 8500,
    }
}
