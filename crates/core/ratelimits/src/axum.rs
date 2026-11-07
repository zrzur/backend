use async_trait::async_trait;
use axum::{
    Json, RequestPartsExt, Router,
    body::Body,
    extract::{FromRef, FromRequestParts, State},
    http::{HeaderValue, Method, Request, StatusCode, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use revolt_database::{Database, User, util::ip::axum::to_real_ip};

use crate::ratelimiter::{
    RatelimitInformation, Ratelimiter, RequestKind, address_blocked, identifier,
};

#[derive(Clone, Copy)]
pub struct AxumRequestKind;

impl RequestKind for AxumRequestKind {
    type R<'a> = Parts;
}

pub type RatelimitStorage = crate::ratelimiter::RatelimitStorage<AxumRequestKind>;

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Ratelimiter
where
    Database: FromRef<S>,
    RatelimitStorage: FromRef<S>,
{
    type Rejection = Json<Ratelimiter>;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if parts
            .extensions
            .get::<Result<Ratelimiter, Json<Ratelimiter>>>()
            .is_none()
        {
            let storage = RatelimitStorage::from_ref(state);

            let ip = to_real_ip(parts).await;
            let user = parts.extract_with_state::<User, _>(state).await.ok();
            // bots by their id, from any address; people by their account
            let bot = user.as_ref().filter(|u| u.bot.is_some()).map(|u| u.id.clone());
            let identifier = identifier(
                bot.as_deref(),
                user.as_ref().map(|u| u.id.as_str()),
                &ip,
            );

            let (bucket, resource) = storage.resolver.resolve_bucket(parts);
            let limit = storage.resolver.resolve_bucket_limit(bucket);

            let ratelimiter = if let Some(seconds) = address_blocked(&ip).await {
                Err(Ratelimiter {
                    key: 0,
                    limit: 0,
                    remaining: 0,
                    reset: seconds as u128 * 1000,
                    global: false,
                })
            } else {
                match (Ratelimiter::from(&identifier, limit, (bucket, resource)).await, bot) {
                    (Ok(route), Some(bot)) => Ratelimiter::global_for_bot(&bot).await.map(|_| route),
                    (route, _) => route,
                }
            };

            parts.extensions.insert(ratelimiter.map_err(Json));
        };

        *parts
            .extensions
            .get::<Result<Ratelimiter, Json<Ratelimiter>>>()
            .unwrap()
    }
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for RatelimitInformation
where
    Database: FromRef<S>,
    RatelimitStorage: FromRef<S>,
{
    type Rejection = Json<RatelimitInformation>;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if parts
            .extensions
            .get::<Result<Ratelimiter, Json<Ratelimiter>>>()
            .is_none()
        {
            let ratelimiter = parts.extract_with_state::<Ratelimiter, S>(state).await;

            parts.extensions.insert(ratelimiter);
        };

        let ratelimiter = *parts
            .extensions
            .get::<Result<Ratelimiter, Json<Ratelimiter>>>()
            .unwrap();

        match ratelimiter {
            Ok(ratelimter) => Ok(RatelimitInformation::Success(ratelimter)),
            Err(ratelimiter) => Err(Json(RatelimitInformation::failure(&ratelimiter))),
        }
    }
}

pub async fn ratelimit_middleware(
    State(database): State<Database>,
    State(ratelimit_storage): State<RatelimitStorage>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // CORS preflights neither count nor get limited: the CORS layer sits
    // inside this one, so a 429 here has no CORS headers and the browser
    // reports a failed fetch. The call itself is still counted.
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }

    #[derive(axum::extract::FromRef)]
    struct TempState {
        database: Database,
        ratelimit_storage: RatelimitStorage,
    }

    let state = TempState {
        database,
        ratelimit_storage,
    };

    let (mut parts, body) = request.into_parts();
    let ip = to_real_ip(&parts).await;

    let res = Ratelimiter::from_request_parts(&mut parts, &state).await;

    let (Ok(ratelimiter) | Err(Json(ratelimiter))) = &res;

    let mut response = if res.is_ok() {
        let request = Request::from_parts(parts, body);

        next.run(request).await
    } else {
        let ratelimit_info = RatelimitInformation::from_request_parts(&mut parts, &state).await;

        ratelimit_info.map(Json).into_response()
    };

    let Ratelimiter {
        key,
        limit,
        remaining,
        reset,
        global,
    } = ratelimiter;

    // refused requests count against the address (token guessing, floods)
    if matches!(response.status().as_u16(), 401 | 403 | 429) {
        crate::ratelimiter::count_refused(&ip).await;
    }

    let headers = response.headers_mut();

    headers.insert(
        "X-RateLimit-Limit",
        HeaderValue::from_str(&limit.to_string()).unwrap(),
    );
    headers.insert(
        "X-RateLimit-Bucket",
        HeaderValue::from_str(&key.to_string()).unwrap(),
    );
    headers.insert(
        "X-RateLimit-Remaining",
        HeaderValue::from_str(&remaining.to_string()).unwrap(),
    );
    headers.insert(
        "X-RateLimit-Reset-After",
        HeaderValue::from_str(&reset.to_string()).unwrap(),
    );
    if *global {
        headers.insert("X-RateLimit-Global", HeaderValue::from_static("true"));
    }

    if res.is_err() {
        headers.insert(
            "Retry-After",
            HeaderValue::from_str(&crate::ratelimiter::retry_after_seconds(*reset).to_string()).unwrap(),
        );
        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    };

    response
}

async fn ratelimit_info(info: RatelimitInformation) -> Json<RatelimitInformation> {
    Json(info)
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S>
where
    Database: FromRef<S>,
    RatelimitStorage: FromRef<S>,
{
    Router::new().route("/ratelimit", get(ratelimit_info))
}
