//! Shared helpers for the webhook tests: everything goes through the HTTP
//! routes, the way a real caller does.

use revolt_database::{Session, User};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

pub use rocket::http::Method::{Delete, Get, Patch, Post};

pub struct Reply {
    pub status: Status,
    pub body: Value,
    pub text: String,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One request; `from` is the caller's address when it matters
pub async fn call(
    harness: &TestHarness,
    method: Method,
    path: &str,
    session: Option<&Session>,
    body: Option<Value>,
    from: Option<&str>,
) -> Reply {
    let mut request = harness.client.req(method, path.to_string());
    if let Some(body) = body {
        request = request.header(ContentType::JSON).body(body.to_string());
    }
    if let Some(session) = session {
        request = request.header(Header::new("x-session-token", session.token.to_string()));
    }
    if let Some(address) = from {
        request = request.remote(address.parse().unwrap());
    }
    let response = request.dispatch().await;
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|header| (header.name().to_string(), header.value().to_string()))
        .collect();
    let text = response.into_string().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Reply { status, body, text, headers }
}

/// A manager, their server, its first channel
pub struct Scene {
    pub session: Session,
    pub user: User,
    pub server: revolt_database::Server,
    pub channel: String,
}

pub async fn scene(harness: &TestHarness) -> Scene {
    let (_, session, user) = harness.new_user().await;
    let (server, channels) = harness.new_server(&user).await;
    let channel = channels[0].id().to_string();
    Scene { session, user, server, channel }
}

/// A member of the scene's server with no special permission
pub async fn member(harness: &TestHarness, scene: &Scene) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    revolt_database::Member::create(&harness.db, &scene.server, &user, None)
        .await
        .expect("member");
    (session, user)
}

/// Create a webhook; `(id, token)`
pub async fn webhook(harness: &TestHarness, scene: &Scene, name: &str) -> (String, String) {
    webhook_in(harness, scene, &scene.channel, name).await
}

pub async fn webhook_in(
    harness: &TestHarness,
    scene: &Scene,
    channel: &str,
    name: &str,
) -> (String, String) {
    let reply = call(
        harness,
        Post,
        &format!("/channels/{channel}/webhooks"),
        Some(&scene.session),
        Some(json!({ "name": name })),
        None,
    )
    .await;
    assert_eq!(reply.status, Status::Ok, "{}", reply.text);
    (
        reply.body["id"].as_str().unwrap().to_string(),
        reply.body["token"].as_str().unwrap().to_string(),
    )
}

/// Execute and ask for the message back
pub async fn post(harness: &TestHarness, id: &str, token: &str, body: Value) -> Reply {
    call(
        harness,
        Post,
        &format!("/webhooks/{id}/{token}?wait=true"),
        None,
        Some(body),
        None,
    )
    .await
}
