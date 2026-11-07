#[macro_use]
extern crate rocket;
#[macro_use]
extern crate revolt_rocket_okapi;
#[macro_use]
extern crate serde_json;

pub mod routes;
pub mod util;

use revolt_config::config;
use revolt_database::AMQP;
use revolt_ratelimits::rocket as ratelimiter;
use rocket::{Build, Rocket};
use revolt_config::{CorsOrigins, LOCAL_ORIGIN_PATTERNS};
use rocket_cors::{AllowedOrigins, CorsOptions};
use rocket_prometheus::PrometheusMetrics;
use std::net::Ipv4Addr;
use std::str::FromStr;

use revolt_database::voice::VoiceClient;
use rocket::data::ToByteUnit;

/// The shared list (S-02) in the form the CORS library here takes
fn allowed_origins(origins: &CorsOrigins) -> AllowedOrigins {
    let local: &[&str] = if origins.local {
        &LOCAL_ORIGIN_PATTERNS
    } else {
        &[]
    };
    AllowedOrigins::some(&origins.exact, local)
}

pub async fn web() -> Rocket<Build> {
    // Get settings
    let config = config().await;

    // Ensure environment variables are present
    config.preflight_checks();

    // Setup database
    let db = revolt_database::DatabaseInfo::Auto.connect().await.unwrap();
    db.migrate_database().await.unwrap();
    revolt_database::seed_official_accounts(&db).await;

    // Configure CORS
    let cors = CorsOptions {
        allowed_origins: allowed_origins(&CorsOrigins::from_settings(
            &config.hosts.app,
            &config.api.security.staff_origins,
        )),
        allowed_methods: [
            "Get", "Put", "Post", "Delete", "Options", "Head", "Trace", "Connect", "Patch",
        ]
        .iter()
        .map(|s| FromStr::from_str(s).unwrap())
        .collect(),
        expose_headers: revolt_ratelimits::ratelimiter::EXPOSED_HEADERS
        .iter()
        .map(|s| s.to_string())
        .collect(),
        ..Default::default()
    }
    .to_cors()
    .expect("Failed to create CORS.");

    // Configure Swagger
    let swagger = revolt_rocket_okapi::swagger_ui::make_swagger_ui(
        &revolt_rocket_okapi::swagger_ui::SwaggerUIConfig {
            url: "/openapi.json".to_owned(),
            ..Default::default()
        },
    )
    .into();

    // Voice handler
    let voice_client = VoiceClient::new(config.api.livekit.nodes.clone());
    // Configure Rabbit

    let amqp = AMQP::new_auto().await;

    // Launch background task workers
    revolt_database::tasks::start_workers(db.clone(), amqp.clone());

    // Configure Rocket
    let rocket = rocket::build();
    let prometheus = PrometheusMetrics::new();

    // Ratelimits
    let ratelimits = ratelimiter::RatelimitStorage::new(util::ratelimits::DeltaRatelimits {
        auth_limit: config.api.security.ratelimits.auth,
    });

    routes::mount(config, rocket)
        .attach(prometheus.clone())
        .attach(routes::StaffMetrics)
        .mount("/metrics", prometheus)
        // LiveKit posts webhooks to /{node} (local config: /worldwide).
        .mount("/", rocket::routes![revolt_voice_ingress::api::ingress])
        .mount("/", rocket_cors::catch_all_options_routes())
        .mount("/", ratelimiter::routes())
        .register("/", util::catchers::catchers())
        .mount("/swagger/", swagger)
        .manage(db)
        .manage(amqp)
        .manage(cors.clone())
        .manage(voice_client)
        .manage(ratelimits)
        .manage(routes::safety::PublicReportGuard::from_env())
        .attach(ratelimiter::RatelimitFairing)
        .attach(util::webhooks::WebhookLimitHeaders)
        .attach(cors)
        // the answer depends on the Origin: a cache must keep them apart
        .attach(rocket::fairing::AdHoc::on_response("Vary: Origin", |_, res| {
            Box::pin(async move {
                if !res.headers().get("Vary").any(|v| v.to_ascii_lowercase().contains("origin")) {
                    res.adjoin_raw_header("Vary", "Origin");
                }
            })
        }))
        .configure(rocket::Config {
            limits: rocket::data::Limits::default()
                .limit("string", 5.megabytes())
                // webhook uploads; the real per-file cap is checked by the route
                .limit("file", 25.megabytes())
                .limit("data-form", 130.megabytes()),
            address: Ipv4Addr::new(0, 0, 0, 0).into(),
            port: 14702,
            ip_header: Some("X-Forwarded-For".into()),
            ..Default::default()
        })
}

#[launch]
async fn rocket() -> _ {
    // Configure logging and environment
    revolt_config::configure!(api);

    // Start web server
    web().await
}
