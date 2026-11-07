//! `features.plugins_enabled`: off closes every plugin route, on leaves them
//! working. The routes come from the running server, so a route added later
//! is covered without touching this file.

use revolt_config::overwrite_config;
use revolt_database::{Session, User};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

/// 25 in `/plugins` and 4 in `/staff/plugins`
const ROUTES: usize = 29;

fn is_plugin_route(path: &str) -> bool {
    path.starts_with("/plugins") || path.starts_with("/staff/plugins")
}

/// The path of a mounted route with its parameters filled in and no query
fn request_path(route: &rocket::Route) -> String {
    route
        .uri
        .path()
        .split('/')
        .map(|part| if part.starts_with('<') { "someid" } else { part })
        .collect::<Vec<_>>()
        .join("/")
}

fn plugin_routes(harness: &TestHarness) -> Vec<(Method, String)> {
    let mut routes: Vec<_> = harness
        .client
        .rocket()
        .routes()
        .filter(|route| is_plugin_route(route.uri.path()))
        .map(|route| (route.method, request_path(route)))
        .collect();
    routes.sort_by_key(|(method, path)| (path.clone(), method.as_str().to_string()));
    routes
}

async fn call(
    harness: &TestHarness,
    method: Method,
    path: &str,
    session: Option<&Session>,
    body: Option<Value>,
) -> (Status, String) {
    let client = &harness.client;
    let mut request = match method {
        Method::Get => client.get(path.to_string()),
        Method::Post => client.post(path.to_string()),
        Method::Put => client.put(path.to_string()),
        Method::Patch => client.patch(path.to_string()),
        Method::Delete => client.delete(path.to_string()),
        other => panic!("no plugin route uses {other}"),
    };
    if let Some(session) = session {
        request = request.header(Header::new("x-session-token", session.token.clone()));
    }
    let request = request.header(ContentType::JSON);
    let request = match body {
        Some(body) => request.body(body.to_string()),
        None => request,
    };
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn root_says_plugins(harness: &TestHarness) -> Value {
    let response = harness.client.get("/").dispatch().await;
    assert_eq!(response.status(), Status::Ok);
    let text = response.into_string().await.unwrap();
    let body: Value = serde_json::from_str(&text).unwrap();
    body["features"]["plugins"].clone()
}

fn assert_refused(route: &str, status: Status, body: &str) {
    assert_eq!(status, Status::Forbidden, "{route}: {body}");
    let body: Value = serde_json::from_str(body).unwrap_or_else(|_| panic!("{route}: {body}"));
    assert_eq!(body["type"], "PluginsDisabled", "{route}: {body}");
}

async fn stored_installs(harness: &TestHarness, user: &User) -> usize {
    harness.db.fetch_plugin_installs(&user.id).await.unwrap().len()
}

#[rocket::async_test]
async fn every_route_is_refused_while_off() {
    overwrite_config(|config| config.features.plugins_enabled = false).await;
    let harness = TestHarness::new().await;
    assert_eq!(root_says_plugins(&harness).await, json!(false));

    let routes = plugin_routes(&harness);
    assert_eq!(routes.len(), ROUTES, "{routes:?}");

    for (method, path) in routes {
        let route = format!("{method} {path}");

        // signed in, with a body no route would accept. A person of their own
        // for each request, or the write limit answers before the route does
        let (_, session, _) = harness.new_user().await;
        let body = (method != Method::Get && method != Method::Delete).then(|| json!({ "x": 1 }));
        let (status, text) = call(&harness, method, &path, Some(&session), body).await;
        assert_refused(&route, status, &text);

        // not signed in and no body: the flag answers first
        let (status, text) = call(&harness, method, &path, None, None).await;
        assert_refused(&route, status, &text);
    }
}

#[rocket::async_test]
async fn nothing_is_written_while_off() {
    overwrite_config(|config| config.features.plugins_enabled = false).await;
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;

    let before = stored_installs(&harness, &user).await;
    let (status, text) = call(
        &harness,
        Method::Put,
        "/plugins/@me/installs/dev.test.midnight",
        Some(&session),
        Some(json!({ "version": "1.0.0", "permissions": [] })),
    )
    .await;
    assert_refused("install", status, &text);
    assert_eq!(stored_installs(&harness, &user).await, before);
}

/// The test configuration has the flag on, as a server that turned it on
#[rocket::async_test]
async fn routes_work_while_on() {
    let harness = TestHarness::new().await;
    assert_eq!(root_says_plugins(&harness).await, json!(true));
    assert_eq!(plugin_routes(&harness).len(), ROUTES);

    let (_, session, _) = harness.new_user().await;
    for path in [
        "/plugins",
        "/plugins/permissions",
        "/plugins/@me/installs",
        "/plugins/@me/projects",
    ] {
        let (status, text) = call(&harness, Method::Get, path, Some(&session), None).await;
        assert_eq!(status, Status::Ok, "{path}: {text}");
    }

    // a route that writes
    let suffix: String = TestHarness::rand_string()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect();
    let id = format!("dev.t{suffix}.flag");
    let (status, text) = call(
        &harness,
        Method::Post,
        "/plugins",
        Some(&session),
        Some(json!({ "id": id, "name": "Flag", "category": "appearance" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{text}");

    // past the flag, the routes' own checks answer
    for path in ["/plugins/review/queue", "/staff/plugins"] {
        let (status, text) = call(&harness, Method::Get, path, Some(&session), None).await;
        assert!(!text.contains("PluginsDisabled"), "{path}: {text}");
        assert_ne!(status, Status::Ok, "{path}: {text}");
    }
}
