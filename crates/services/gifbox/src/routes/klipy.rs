use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use revolt_database::User;
use revolt_result::{create_error, Result};
use serde::Deserialize;
use serde_json::Value;
use utoipa::IntoParams;

use crate::klipy::{now, parse_range, Klipy, KlipyError, RangeSpec};

fn failure(error: KlipyError) -> revolt_result::Error {
    match error {
        KlipyError::Invalid => create_error!(InvalidOperation),
        // never say more: nothing from the upstream request may leak
        KlipyError::Unavailable | KlipyError::Upstream | KlipyError::Busy => {
            create_error!(InternalError)
        }
    }
}

#[derive(Deserialize, IntoParams)]
pub struct KlipyCategoriesParams {
    /// Users locale
    #[param(example = "en")]
    pub locale: Option<String>,
}

/// GIF categories (KLIPY)
#[utoipa::path(
    get,
    path = "/klipy/categories",
    tag = "GIFs",
    security(("User Token" = []), ("Bot Token" = [])),
    params(KlipyCategoriesParams)
)]
pub async fn categories(
    _user: User,
    Query(params): Query<KlipyCategoriesParams>,
    State(klipy): State<Klipy>,
) -> Result<Json<Value>> {
    let url = klipy
        .categories_url(params.locale.as_deref().unwrap_or("en"))
        .map_err(failure)?;
    klipy.get_json(url).await.map(Json).map_err(failure)
}

#[derive(Deserialize, IntoParams)]
pub struct KlipyGifsParams {
    /// Search text; empty for trending
    pub query: Option<String>,
    /// Users locale
    pub locale: Option<String>,
    /// Page, starting at 1
    pub page: Option<u32>,
    /// Results per page (1 to 50)
    pub per_page: Option<u32>,
}

/// Search or trending GIFs (KLIPY)
#[utoipa::path(
    get,
    path = "/klipy/gifs",
    tag = "GIFs",
    security(("User Token" = []), ("Bot Token" = [])),
    params(KlipyGifsParams)
)]
pub async fn gifs(
    _user: User,
    Query(params): Query<KlipyGifsParams>,
    State(klipy): State<Klipy>,
) -> Result<Json<Value>> {
    let url = klipy
        .gifs_url(
            params.query.as_deref().unwrap_or(""),
            params.page.unwrap_or(1),
            params.per_page.unwrap_or(24),
            params.locale.as_deref().unwrap_or("en"),
        )
        .map_err(failure)?;
    klipy.get_json(url).await.map(Json).map_err(failure)
}

#[derive(Deserialize, IntoParams)]
pub struct KlipyMediaParams {
    /// A https URL on the KLIPY media host
    pub u: String,
    /// Expiry (unix seconds), as signed by gifbox
    pub e: u64,
    /// Signature made by gifbox when it listed the GIF
    pub s: String,
}

fn status_only(status: StatusCode) -> Response {
    status.into_response()
}

fn media_error(error: KlipyError) -> Response {
    match error {
        KlipyError::Invalid => status_only(StatusCode::BAD_REQUEST),
        KlipyError::Busy => {
            let mut response = status_only(StatusCode::SERVICE_UNAVAILABLE);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            response
        }
        KlipyError::Unavailable => status_only(StatusCode::SERVICE_UNAVAILABLE),
        KlipyError::Upstream => status_only(StatusCode::BAD_GATEWAY),
    }
}

fn media_headers(content_type: &str) -> Option<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).ok()?);
    // private: no shared cache keeps KLIPY files (their terms on caching are unchecked)
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "Cross-Origin-Resource-Policy",
        HeaderValue::from_static("cross-origin"),
    );
    Some(headers)
}

/// Checks the signature; the error is the answer to send.
fn check_link(klipy: &Klipy, params: &KlipyMediaParams) -> std::result::Result<(), Response> {
    if !klipy.enabled() {
        return Err(status_only(StatusCode::SERVICE_UNAVAILABLE));
    }
    if !klipy.verify_link(&params.u, params.e, &params.s, now()) {
        return Err(status_only(StatusCode::UNAUTHORIZED));
    }
    Ok(())
}

/// One GIF/preview file from KLIPY, served through gifbox.
///
/// No login, because images in the page cannot send a token, but only links
/// that gifbox signed (for a logged-in search, valid for an hour) work, so
/// this is not an open proxy. Supports one `Range` (Safari plays video with it).
#[utoipa::path(get, path = "/klipy/media", tag = "GIFs", params(KlipyMediaParams))]
pub async fn media(
    Query(params): Query<KlipyMediaParams>,
    State(klipy): State<Klipy>,
    request_headers: HeaderMap,
) -> Response {
    if let Err(response) = check_link(&klipy, &params) {
        return response;
    }
    let (content_type, bytes) = match klipy.get_media(&params.u).await {
        Ok(found) => found,
        Err(error) => return media_error(error),
    };
    let Some(mut headers) = media_headers(&content_type) else {
        return status_only(StatusCode::BAD_GATEWAY);
    };
    let total = bytes.len();
    let range = request_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, total))
        .unwrap_or(RangeSpec::Full);
    match range {
        RangeSpec::Partial(start, end) => {
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            // a slice of the same buffer, no copy
            (StatusCode::PARTIAL_CONTENT, headers, bytes.slice(start..=end)).into_response()
        }
        RangeSpec::Unsatisfiable => {
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response()
        }
        RangeSpec::Full => (StatusCode::OK, headers, bytes).into_response(),
    }
}

/// Headers of one media file without loading the whole file.
#[utoipa::path(head, path = "/klipy/media", tag = "GIFs", params(KlipyMediaParams))]
pub async fn media_head(
    Query(params): Query<KlipyMediaParams>,
    State(klipy): State<Klipy>,
) -> Response {
    if let Err(response) = check_link(&klipy, &params) {
        return response;
    }
    let (content_type, length) = match klipy.head_media(&params.u).await {
        Ok(found) => found,
        Err(error) => return media_error(error),
    };
    let Some(mut headers) = media_headers(&content_type) else {
        return status_only(StatusCode::BAD_GATEWAY);
    };
    if let Some(length) = length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    (StatusCode::OK, headers, Body::empty()).into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use axum::{extract::Request, routing::any, Router};
    use revolt_database::DatabaseInfo;
    use tokio::net::TcpListener;

    use super::*;
    use crate::{ratelimits::GifboxRatelimits, tenor::Tenor, AppState};

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    async fn serve(klipy: Klipy) -> SocketAddr {
        let state = AppState {
            database: DatabaseInfo::Reference.connect().await.unwrap(),
            tenor: Tenor::new(""),
            klipy,
            ratelimit_storage: revolt_ratelimits::axum::RatelimitStorage::new(GifboxRatelimits),
        };
        let app = super::super::router(Default::default()).with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    /// Stand-in for the KLIPY media host; records the methods it was asked with.
    async fn media_host(size: usize, methods: Arc<Mutex<Vec<String>>>) -> u16 {
        let app = Router::new().fallback(any(move |req: Request| {
            let methods = methods.clone();
            async move {
                methods.lock().unwrap().push(req.method().to_string());
                ([(header::CONTENT_TYPE, "image/gif")], vec![7u8; size])
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    /// A gifbox with working signed links to a local media host of `size` bytes.
    async fn with_media(size: usize) -> (SocketAddr, String, Arc<Mutex<Vec<String>>>) {
        let methods = Arc::new(Mutex::new(Vec::new()));
        let port = media_host(size, methods.clone()).await;
        let klipy = Klipy::configure(Some("key".into()), None, Some(SECRET.into()), true)
            .with_local_media("127.0.0.1", port);
        let url = format!("http://127.0.0.1:{port}/a.gif");
        let link = klipy.signed_link(&url, now());
        (serve(klipy).await, link, methods)
    }

    fn default_klipy() -> Klipy {
        Klipy::configure(Some("key".into()), None, Some(SECRET.into()), true)
    }

    #[tokio::test]
    async fn search_and_categories_need_a_login() {
        let addr = serve(default_klipy()).await;
        let client = reqwest::Client::new();
        for path in ["/klipy/gifs?query=cat", "/klipy/categories"] {
            let url = format!("http://{addr}{path}");
            let none = client.get(&url).send().await.unwrap();
            assert_eq!(none.status(), 401, "{path} without token");
            let bad = client
                .get(&url)
                .header("X-Session-Token", "not-a-session")
                .send()
                .await
                .unwrap();
            assert_eq!(bad.status(), 401, "{path} with a bad token");
        }
    }

    #[tokio::test]
    async fn media_needs_a_valid_signature() {
        let (addr, link, _) = with_media(100).await;
        let client = reqwest::Client::new();
        let u = "https://static.klipy.com/a.gif";
        let in_window = now() + 100;
        for query in [
            format!("u={u}"),
            format!("u={u}&e=9999999999&s=abc"),
            format!("u={u}&e=1&s={}", "0".repeat(64)),
        ] {
            let res = client
                .get(format!("http://{addr}/klipy/media?{query}"))
                .send()
                .await
                .unwrap();
            assert!(
                res.status() == 401 || res.status() == 400,
                "{query}: {}",
                res.status()
            );
        }
        // inside the time window but signed wrongly
        let res = client
            .get(format!(
                "http://{addr}/klipy/media?u={u}&e={in_window}&s={}",
                "0".repeat(64)
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
        // the genuine link still works
        let ok = client.get(format!("http://{addr}{link}")).send().await.unwrap();
        assert_eq!(ok.status(), 200);
    }

    #[tokio::test]
    async fn signed_link_gives_ranges_and_safe_headers() {
        let (addr, link, _) = with_media(100).await;
        let client = reqwest::Client::new();
        let get = |range: Option<&'static str>| {
            let mut request = client.get(format!("http://{addr}{link}"));
            if let Some(range) = range {
                request = request.header("Range", range);
            }
            request.send()
        };

        let partial = get(Some("bytes=0-9")).await.unwrap();
        assert_eq!(partial.status(), 206);
        let headers = partial.headers().clone();
        assert_eq!(headers["content-range"], "bytes 0-9/100");
        assert_eq!(headers["accept-ranges"], "bytes");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert!(headers["cache-control"].to_str().unwrap().starts_with("private"));
        assert_eq!(headers["content-type"], "image/gif");
        assert_eq!(partial.bytes().await.unwrap().len(), 10);

        // more than one range: the whole file
        let many = get(Some("bytes=0-1,5-6")).await.unwrap();
        assert_eq!(many.status(), 200);
        assert_eq!(many.bytes().await.unwrap().len(), 100);

        // starting after the end
        let late = get(Some("bytes=500-")).await.unwrap();
        assert_eq!(late.status(), 416);
        assert_eq!(late.headers()["content-range"], "bytes */100");
    }

    #[tokio::test]
    async fn media_over_the_size_cap_is_refused() {
        let (addr, link, _) = with_media(crate::klipy::MAX_MEDIA_BYTES + 1).await;
        let res = reqwest::get(format!("http://{addr}{link}")).await.unwrap();
        assert_eq!(res.status(), 502);
    }

    #[tokio::test]
    async fn head_does_not_load_the_file() {
        let (addr, link, methods) = with_media(100).await;
        let client = reqwest::Client::new();
        let res = client
            .head(format!("http://{addr}{link}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "image/gif");
        assert_eq!(*methods.lock().unwrap(), vec!["HEAD".to_string()]);
    }
}
