//! JSON bodies for errors Rocket raises itself.
//!
//! Without these, a request with a missing or wrong token, an unknown route
//! or a body that is not valid JSON got Rocket's HTML error page, so clients
//! could not read the error `type`. Status codes are unchanged.

use revolt_result::{create_error, Error};
use rocket::{
    catch, catchers,
    http::{ContentType, Status},
    Catcher, Request,
};

/// Authentication guards failed: no token, or a token that is not valid
#[catch(401)]
fn unauthorized() -> Error {
    create_error!(InvalidSession)
}

/// A guard refused the request (no declared age); other 403s come from
/// handlers and carry their own body
#[catch(403)]
fn forbidden(request: &Request) -> Error {
    request
        .local_cache(revolt_result::rocket::GuardRefusal::default)
        .0
        .clone()
        .unwrap_or_else(|| create_error!(NotPrivileged))
}

/// A guard hit a failing database (or a handler failed): say so, so clients
/// retry instead of treating it as a refused session
#[catch(500)]
fn internal(request: &Request) -> Error {
    request
        .local_cache(revolt_result::rocket::GuardRefusal::default)
        .0
        .clone()
        .unwrap_or_else(|| create_error!(InternalError))
}

/// No route matches the path
#[catch(404)]
fn not_found() -> Error {
    create_error!(NotFound)
}

/// The request body could not be read (keeps the original status)
fn invalid_body(status: Status) -> (Status, (ContentType, String)) {
    let body = serde_json::json!({
        "type": "FailedValidation",
        "error": "The request body is missing or is not valid JSON for this route",
    });
    (status, (ContentType::JSON, body.to_string()))
}

#[catch(400)]
fn bad_request(_: &Request) -> (Status, (ContentType, String)) {
    invalid_body(Status::BadRequest)
}

#[catch(422)]
fn unprocessable(_: &Request) -> (Status, (ContentType, String)) {
    invalid_body(Status::UnprocessableEntity)
}

pub fn catchers() -> Vec<Catcher> {
    catchers![unauthorized, forbidden, internal, not_found, bad_request, unprocessable]
}

#[cfg(test)]
mod test {
    use crate::util::test::TestHarness;
    use rocket::http::{ContentType, Header, Status};

    async fn json_type(response: rocket::local::asynchronous::LocalResponse<'_>) -> String {
        assert_eq!(response.content_type(), Some(ContentType::JSON));
        let body: serde_json::Value = response.into_json().await.expect("JSON body");
        body["type"].as_str().expect("type").to_owned()
    }

    #[rocket::async_test]
    async fn errors_raised_by_rocket_are_json() {
        let harness = TestHarness::new().await;

        // no token
        let response = harness.client.get("/users/@me").dispatch().await;
        assert_eq!(response.status(), Status::Unauthorized);
        assert_eq!(json_type(response).await, "InvalidSession");

        // a bot token that does not exist
        let response = harness
            .client
            .get("/users/@me")
            .header(Header::new("x-bot-token", "not-a-token"))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Unauthorized);
        assert_eq!(json_type(response).await, "InvalidSession");

        // unknown route
        let response = harness.client.get("/no/such/route").dispatch().await;
        assert_eq!(response.status(), Status::NotFound);
        assert_eq!(json_type(response).await, "NotFound");

        // body that is not JSON
        let (_, session, _) = harness.new_user().await;
        let response = harness
            .client
            .patch("/users/@me")
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(ContentType::JSON)
            .body("{not json")
            .dispatch()
            .await;
        assert!(response.status() == Status::BadRequest || response.status() == Status::UnprocessableEntity);
        assert_eq!(json_type(response).await, "FailedValidation");
    }
}
