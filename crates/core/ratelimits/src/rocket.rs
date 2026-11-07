use async_trait::async_trait;
use log::info;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::uri::Origin;
use rocket::http::{Method, Status};
use rocket::request::{FromRequest, Outcome};
use rocket::serde::json::Json;
use rocket::{Data, Request, Response, State};

use revolt_rocket_okapi::r#gen::OpenApiGenerator;
use revolt_rocket_okapi::request::{OpenApiFromRequest, RequestHeaderInput};
use revolt_database::{Database, Session, util::ip::rocket::to_real_ip};

use crate::ratelimiter::RequestKind;
use crate::ratelimiter::{
    RatelimitInformation, Ratelimiter, address_blocked, bot_for_token, count_refused, counts_as_refused, identifier_for_bucket,
};

#[derive(Clone, Copy)]
pub struct RocketRequestKind;

impl RequestKind for RocketRequestKind {
    type R<'a> = Request<'a>;
}

pub type RatelimitStorage = crate::ratelimiter::RatelimitStorage<RocketRequestKind>;

#[async_trait]
impl<'r> FromRequest<'r> for Ratelimiter {
    type Error = Ratelimiter;

    async fn from_request<'a>(request: &'r rocket::Request<'a>) -> Outcome<Self, Self::Error> {
        let ratelimiter = request
            .local_cache_async(async {
                use rocket::outcome::Outcome;

                let storage = request.guard::<&State<RatelimitStorage>>().await.unwrap();
                let ip = to_real_ip(request).await;

                // an address blocked for too many refused requests gets nothing
                if let Some(seconds) = address_blocked(&ip).await {
                    return Err(Ratelimiter {
                        key: 0,
                        limit: 0,
                        remaining: 0,
                        reset: seconds as u128 * 1000,
                        global: false,
                    });
                }

                // bots are counted by their id, from whichever address they call
                let bot = match (
                    request.headers().get_one("x-bot-token"),
                    request.guard::<&State<Database>>().await,
                ) {
                    (Some(token), Outcome::Success(db)) => bot_for_token(db, token).await,
                    _ => None,
                };
                let session = if bot.is_none() {
                    match request.guard::<Session>().await {
                        Outcome::Success(session) => Some(session.id),
                        _ => None,
                    }
                } else {
                    None
                };
                let (bucket, resource) = storage.resolver.resolve_bucket(request);
                let identifier =
                    identifier_for_bucket(bucket, bot.as_deref(), session.as_deref(), &ip);
                let limit = storage.resolver.resolve_bucket_limit(bucket);

                let route = Ratelimiter::from(&identifier, limit, (bucket, resource)).await;
                match (route, bot) {
                    // and every bot has one limit across all routes
                    (Ok(route), Some(bot)) => Ratelimiter::global_for_bot(&bot).await.map(|_| route),
                    (route, _) => route,
                }
            })
            .await;

        match ratelimiter {
            Ok(ratelimiter) => Outcome::Success(*ratelimiter),
            Err(ratelimiter) => Outcome::Error((Status::TooManyRequests, *ratelimiter)),
        }
    }
}

impl OpenApiFromRequest<'_> for Ratelimiter {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

/// CORS preflights are the browser's, not the client's: they neither count nor
/// get limited. A preflight in a bucket uses up the room of the call it
/// announces, and a 429 on it has no CORS headers, so the browser reports a
/// failed fetch instead of a limit. The call itself is still counted.
fn is_preflight(request: &Request<'_>) -> bool {
    request.method() == Method::Options
}

/// Attach ratelimiter to the Rocket application
pub struct RatelimitFairing;

#[async_trait]
impl Fairing for RatelimitFairing {
    fn info(&self) -> Info {
        Info {
            name: "Ratelimiter",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        use rocket::outcome::Outcome;
        if is_preflight(request) {
            return;
        }
        if let Outcome::Error(_) = request.guard::<Ratelimiter>().await {
            info!(
                "User rate-limited on route {}! (IP = {:?})",
                crate::ratelimiter::path_for_logs(request.uri().path().as_str()),
                to_real_ip(request).await
            );

            request.set_method(Method::Get);
            request.set_uri(Origin::parse("/ratelimit").unwrap())
        }
    }

    async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
        if is_preflight(request) {
            return;
        }
        let guard = request.guard::<Ratelimiter>().await;
        let (Outcome::Success(ratelimiter) | Outcome::Error((_, ratelimiter))) = guard else {
            unreachable!()
        };
        let Ratelimiter {
            key,
            limit,
            remaining,
            reset,
            global,
        } = ratelimiter;

        response.set_raw_header("X-RateLimit-Limit", limit.to_string());
        response.set_raw_header("X-RateLimit-Bucket", key.to_string());
        response.set_raw_header("X-RateLimit-Remaining", remaining.to_string());
        response.set_raw_header("X-RateLimit-Reset-After", reset.to_string());
        if global {
            response.set_raw_header("X-RateLimit-Global", "true");
        }

        if guard.is_error() {
            response.set_status(Status::TooManyRequests);
            response.set_raw_header("Retry-After", crate::ratelimiter::retry_after_seconds(reset).to_string());
        }

        // refused requests count against the address (token guessing, floods)
        if counts_as_refused(response.status().code) {
            count_refused(&to_real_ip(request).await).await;
        }
    }
}

#[async_trait]
impl<'r> FromRequest<'r> for RatelimitInformation {
    type Error = u128;

    async fn from_request(request: &'r rocket::Request<'_>) -> Outcome<Self, Self::Error> {
        let info = match request.guard::<Ratelimiter>().await {
            Outcome::Success(ratelimiter) => RatelimitInformation::Success(ratelimiter),
            Outcome::Error((_, ratelimiter)) => RatelimitInformation::failure(&ratelimiter),
            _ => unreachable!(),
        };
        Outcome::Success(info)
    }
}

#[rocket::get("/ratelimit")]
fn ratelimit_info(info: RatelimitInformation) -> Json<RatelimitInformation> {
    Json(info)
}

pub fn routes() -> Vec<rocket::Route> {
    rocket::routes![ratelimit_info]
}
