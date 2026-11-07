use axum::{
    extract::Query, middleware::from_fn, response::IntoResponse, routing::get, Json, Router,
};
use reqwest::header;
use revolt_config::config;
use revolt_models::v0::Embed;
use revolt_result::{create_error, Result};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{limits, requests::Request};

pub static CACHE_CONTROL: &str = "public, max-age=600, immutable";

pub async fn router() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/proxy", get(proxy).layer(from_fn(limits::per_address)))
        .route(
            "/embed",
            get(embed)
                .layer(from_fn(limits::embed_key))
                .layer(from_fn(limits::per_address)),
        )
}

/// Successful root response
#[derive(Serialize, Debug, ToSchema)]
pub struct RootResponse {
    january: &'static str,
    version: &'static str,
}

/// Capture crate version from Cargo
static CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root response from service
#[utoipa::path(
    get,
    path = "/",
    responses(
        (status = 200, description = "Echo response", body = RootResponse)
    )
)]
async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        january: "Hello, I am a media proxy server!",
        version: CRATE_VERSION,
    })
}

#[derive(Deserialize)]
struct UrlQuery {
    url: String,
}

#[derive(Deserialize)]
struct ProxyQuery {
    url: String,
    /// Ticket from `POST /auth/session/proxy_ticket`
    t: Option<String>,
}

/// Proxy a given URL and load media
#[utoipa::path(
    get,
    path = "/proxy",
    responses(
        (status = 200, description = "Requested media file", body = Vec<u8>)
    ),
    params(
        ("url" = String, Query, description = "URL to fetch"),
        ("t" = Option<String>, Query, description = "Media proxy ticket")
    ),
)]
async fn proxy(Query(ProxyQuery { url, t }): Query<ProxyQuery>) -> Result<impl IntoResponse> {
    let config = config().await;
    if config.january.require_proxy_ticket
        && !limits::proxy_ticket_is_valid(&config.january.proxy_key, t.as_deref(), limits::now())
    {
        return Err(create_error!(NotAuthenticated));
    }

    Request::proxy_file(&url).await.map(|(content_type, data)| {
        (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_DISPOSITION, "inline".to_owned()),
                (header::CACHE_CONTROL, CACHE_CONTROL.to_owned()),
            ],
            data,
        )
    })
}

/// Generate embed for a given URL
#[utoipa::path(
    get,
    path = "/embed",
    responses(
        (status = 200, description = "Generated embed information", body = Embed)
    ),
    params(
        ("url" = String, Query, description = "URL to fetch")
    ),
    security(
        ("api_key" = [])
    )
)]
async fn embed(
    Query(UrlQuery { url }): Query<UrlQuery>,
    // TypedHeader(Authorization(_bearer)): TypedHeader<Authorization<Bearer>>,
) -> Result<impl IntoResponse> {
    match Request::generate_embed(url).await {
        Ok(Embed::None) => Err(create_error!(NoEmbedData)),
        result => result,
    }
    .map(Json)
}
