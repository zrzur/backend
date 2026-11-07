use revolt_database::{PlatformPermission, PlatformRole, Session, User};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::{InstallView, PluginCard, PluginDetails, PluginPage, ReleaseView, ReviewItem};
use crate::util::test::TestHarness;

fn manifest(id: &str, version: &str, permissions: &[&str]) -> Value {
    json!({
        "manifest_version": 1,
        "id": id,
        "name": "Midnight",
        "version": version,
        "description": "A deep blue theme",
        "author": { "name": "Tester" },
        "category": "appearance",
        "api_version": 1,
        "min_app_version": "26.0.0",
        "platforms": ["web", "pwa", "desktop"],
        "entrypoint": { "type": "declarative" },
        "permissions": permissions,
        "contributes": {
            "theme": { "colors": { "primary": "#7aa2ff", "surface": "#0b1020", "on-surface": "#e6e9f5" } }
        }
    })
}

/// A fresh plugin id, so repeated runs against one database never collide
fn rand_id(name: &str) -> String {
    let suffix: String = TestHarness::rand_string()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect();
    format!("dev.t{suffix}.{name}")
}

fn auth(session: &Session) -> Header<'static> {
    Header::new("x-session-token", session.token.clone())
}

async fn send(
    harness: &TestHarness,
    session: &Session,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (Status, String) {
    let client = &harness.client;
    let request = match method {
        "GET" => client.get(path.to_string()),
        "POST" => client.post(path.to_string()),
        "PUT" => client.put(path.to_string()),
        "PATCH" => client.patch(path.to_string()),
        "DELETE" => client.delete(path.to_string()),
        _ => unreachable!(),
    };
    let request = request.header(auth(session)).header(ContentType::JSON);
    let request = match body {
        Some(body) => request.body(body.to_string()),
        None => request,
    };
    let response = request.dispatch().await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

async fn reviewer(harness: &TestHarness) -> (Session, User) {
    let (_, session, user) = harness.new_user().await;
    harness
        .db
        .save_platform_role(&PlatformRole {
            id: user.id.clone(),
            permissions: vec![PlatformPermission::PluginsReview],
            creator: None,
            show_public_name: false,
            updated_at: 0,
            updated_by: "test".to_string(),
        })
        .await
        .unwrap();
    (session, user)
}

/// Create a project and submit its first release
async fn publish(harness: &TestHarness, dev: &Session, id: &str) -> ReleaseView {
    let (status, body) = send(
        harness,
        dev,
        "POST",
        "/plugins",
        Some(json!({ "id": id, "name": "Midnight", "category": "appearance" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let (status, body) = send(
        harness,
        dev,
        "POST",
        &format!("/plugins/{id}/releases"),
        Some(json!({ "manifest": manifest(id, "1.0.0", &["theme.colors"]), "changelog": "First" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[rocket::async_test]
async fn full_lifecycle() {
    let harness = TestHarness::new().await;
    let (_, dev, _) = harness.new_user().await;
    let (rev, _) = reviewer(&harness).await;
    let (_, fan, fan_user) = harness.new_user().await;
    let id = rand_id("midnight");

    // 1-3: create, validate, submit
    let release = publish(&harness, &dev, &id).await;
    assert_eq!(release.status, "PendingReview");

    // not in the store, not visible to others, not installable
    let (_, body) = send(&harness, &fan, "GET", "/plugins", None).await;
    let page: PluginPage = serde_json::from_str(&body).unwrap();
    assert!(!page.plugins.iter().any(|p| p.id == id));
    let (status, _) = send(&harness, &fan, "GET", &format!("/plugins/{id}"), None).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = send(
        &harness,
        &fan,
        "PUT",
        &format!("/plugins/@me/installs/{id}"),
        Some(json!({ "release_id": release.id, "permissions": ["theme.colors"] })),
    )
    .await;
    assert_eq!(status, Status::NotFound);

    // only reviewers see the queue and approve
    let (status, _) = send(&harness, &fan, "GET", "/plugins/review/queue", None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = send(&harness, &dev, "POST", &format!("/plugins/releases/{}/approve", release.id), Some(json!({}))).await;
    assert_eq!(status, Status::Forbidden);
    let (_, body) = send(&harness, &rev, "GET", "/plugins/review/queue", None).await;
    let queue: Vec<ReviewItem> = serde_json::from_str(&body).unwrap();
    assert!(queue.iter().any(|i| i.release.id == release.id));

    // 4: approve
    let (status, body) = send(&harness, &rev, "POST", &format!("/plugins/releases/{}/approve", release.id), Some(json!({ "verify_publisher": true }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    let (_, body) = send(&harness, &fan, "GET", &format!("/plugins?query=midnight"), None).await;
    let page: PluginPage = serde_json::from_str(&body).unwrap();
    assert!(page.plugins.iter().any(|p| p.id == id));
    assert!(page.publishers.iter().any(|p| p.verified));

    // 5-7: install needs exactly the requested permissions
    let (status, _) = send(
        &harness,
        &fan,
        "PUT",
        &format!("/plugins/@me/installs/{id}"),
        Some(json!({ "release_id": release.id, "permissions": [] })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(
        &harness,
        &fan,
        "PUT",
        &format!("/plugins/@me/installs/{id}"),
        Some(json!({ "release_id": release.id, "permissions": ["theme.colors", "auth.token"] })),
    )
    .await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(
        &harness,
        &fan,
        "PUT",
        &format!("/plugins/@me/installs/{id}"),
        Some(json!({ "release_id": release.id, "permissions": ["theme.colors"] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let view: InstallView = serde_json::from_str(&body).unwrap();
    assert!(view.enabled && view.runnable);
    assert_eq!(view.granted, vec!["theme.colors".to_string()]);

    // 9-10: disable, enable
    let (_, body) = send(&harness, &fan, "PATCH", &format!("/plugins/@me/installs/{id}"), Some(json!({ "enabled": false }))).await;
    assert!(!serde_json::from_str::<InstallView>(&body).unwrap().enabled);
    let (_, body) = send(&harness, &fan, "PATCH", &format!("/plugins/@me/installs/{id}"), Some(json!({ "enabled": true }))).await;
    assert!(serde_json::from_str::<InstallView>(&body).unwrap().enabled);

    // 11: an update with a new permission needs new consent
    let (status, body) = send(
        &harness,
        &dev,
        "POST",
        &format!("/plugins/{id}/releases"),
        Some(json!({ "manifest": manifest(&id, "1.1.0", &["theme.colors", "notifications.show"]) })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let update: ReleaseView = serde_json::from_str(&body).unwrap();
    send(&harness, &rev, "POST", &format!("/plugins/releases/{}/approve", update.id), Some(json!({}))).await;
    let (_, body) = send(&harness, &fan, "GET", "/plugins/@me/installs", None).await;
    let installs: Vec<InstallView> = serde_json::from_str(&body).unwrap();
    let mine = installs.iter().find(|i| i.plugin.id == id).unwrap();
    let pending = mine.update.as_ref().expect("update offered");
    assert_eq!(pending.new_permissions, vec!["notifications.show".to_string()]);
    let (status, body) = send(
        &harness,
        &fan,
        "PUT",
        &format!("/plugins/@me/installs/{id}"),
        Some(json!({ "release_id": update.id, "permissions": ["theme.colors", "notifications.show"] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(serde_json::from_str::<InstallView>(&body).unwrap().release.version, "1.1.0");

    // revoking the update falls back to 1.0.0 and blocks the install
    send(&harness, &rev, "POST", &format!("/plugins/releases/{}/revoke", update.id), Some(json!({ "reason": "unsafe" }))).await;
    let (_, body) = send(&harness, &fan, "GET", "/plugins/@me/installs", None).await;
    let installs: Vec<InstallView> = serde_json::from_str(&body).unwrap();
    let mine = installs.iter().find(|i| i.plugin.id == id).unwrap();
    assert_eq!(mine.blocked.as_deref(), Some("revoked"));
    assert!(mine.release.code.is_none());
    let fallback = mine.update.as_ref().expect("fallback offered");
    assert!(fallback.fallback);
    assert_eq!(fallback.version, "1.0.0");

    // 12-13: uninstall leaves no install, grant or settings
    let (status, _) = send(&harness, &fan, "DELETE", &format!("/plugins/@me/installs/{id}"), None).await;
    assert_eq!(status, Status::NoContent);
    let (_, body) = send(&harness, &fan, "GET", "/plugins/@me/installs", None).await;
    assert!(!serde_json::from_str::<Vec<InstallView>>(&body).unwrap().iter().any(|i| i.plugin.id == id));
    let fan_user = fan_user.id;
    assert!(harness.db.fetch_plugin_grant(&fan_user, &id).await.unwrap().is_none());
    assert!(harness.db.fetch_plugin_settings(&fan_user, &id).await.unwrap().is_none());
    assert!(harness.db.fetch_plugin_install(&fan_user, &id).await.unwrap().is_none());

    // the log has every step
    let (_, body) = send(&harness, &dev, "GET", &format!("/plugins/{id}/log"), None).await;
    let log: Vec<Value> = serde_json::from_str(&body).unwrap();
    for action in ["Create", "Submit", "Approve", "VerifyPublisher", "Revoke"] {
        assert!(log.iter().any(|l| l["action"] == action), "missing {action}");
    }
    let (status, _) = send(&harness, &fan, "GET", &format!("/plugins/{id}/log"), None).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn rejects_bad_packages() {
    let harness = TestHarness::new().await;
    let (_, dev, _) = harness.new_user().await;
    let id = &rand_id("pkg");
    send(&harness, &dev, "POST", "/plugins", Some(json!({ "id": id, "name": "Bad", "category": "developer" }))).await;

    let cases = [
        // forbidden permission
        json!({ "manifest": manifest(id, "1.0.0", &["theme.colors", "auth.token"]) }),
        // unknown privileged capability
        json!({ "manifest": manifest(id, "1.0.0", &["theme.colors", "root.everything"]) }),
        // third-party code is blocked
        json!({ "manifest": {
            "manifest_version": 1, "id": id, "name": "Bad", "version": "1.0.0", "description": "x",
            "author": { "name": "x" }, "category": "developer", "api_version": 1, "min_app_version": "26.0.0",
            "platforms": ["web"], "entrypoint": { "type": "worker", "file": "main.js" }, "permissions": [],
            "integrity": { "sha256": revolt_database::sha256_hex(b"1") }
        }, "code": "1" }),
        // id mismatch
        json!({ "manifest": manifest(&rand_id("other"), "1.0.0", &["theme.colors"]) }),
        // leaked secret
        json!({ "manifest": { "x-session-token": 1 } }),
        // declarative with smuggled code
        json!({ "manifest": manifest(id, "1.0.0", &["theme.colors"]), "code": "alert(1)" }),
    ];
    for case in cases {
        let (status, body) = send(&harness, &dev, "POST", &format!("/plugins/{id}/releases"), Some(case.clone())).await;
        assert_eq!(status, Status::BadRequest, "{case} → {body}");
    }

    // dry-run validation explains every issue
    let (_, body) = send(
        &harness,
        &dev,
        "POST",
        "/plugins/validate",
        Some(json!({ "manifest": manifest(id, "1.0.0", &["theme.colors", "shell"]) })),
    )
    .await;
    let result: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(result["ok"], false);
    assert!(result["issues"][0].as_str().unwrap().contains("never be granted"));

    // nothing was stored; the auto-rejections are logged
    let (_, body) = send(&harness, &dev, "GET", "/plugins/@me/projects", None).await;
    let projects: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(projects[0]["releases"].as_array().unwrap().len(), 0);
    let (_, body) = send(&harness, &dev, "GET", &format!("/plugins/{id}/log"), None).await;
    assert!(body.contains("AutoReject"));
}

#[rocket::async_test]
async fn ownership_and_reserved_ids() {
    let harness = TestHarness::new().await;
    let (_, dev, _) = harness.new_user().await;
    let (_, other, _) = harness.new_user().await;
    let own = rand_id("theme");
    let own_path = format!("/plugins/{own}/releases");

    // zrzur.* is reserved for privileged accounts
    let (status, _) = send(&harness, &dev, "POST", "/plugins", Some(json!({ "id": "zrzur.fake", "name": "Fake", "category": "appearance" }))).await;
    assert_eq!(status, Status::BadRequest);
    // path traversal / bad ids
    let (status, _) = send(&harness, &dev, "POST", "/plugins", Some(json!({ "id": "../etc", "name": "X", "category": "appearance" }))).await;
    assert_eq!(status, Status::BadRequest);

    let release = publish(&harness, &dev, &own).await;
    // someone else can't submit to it, withdraw it or take its id
    let (status, _) = send(&harness, &other, "POST", &own_path, Some(json!({ "manifest": manifest(&own, "2.0.0", &["theme.colors"]) }))).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = send(&harness, &other, "POST", &format!("/plugins/releases/{}/withdraw", release.id), None).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = send(&harness, &other, "POST", "/plugins", Some(json!({ "id": own, "name": "Mine", "category": "appearance" }))).await;
    assert_eq!(status, Status::BadRequest);

    // versions must go up
    let (status, _) = send(&harness, &dev, "POST", &own_path, Some(json!({ "manifest": manifest(&own, "1.0.0", &["theme.colors"]) }))).await;
    assert_eq!(status, Status::BadRequest);

    // a reviewer can't approve their own plugin
    let (rev_session, _) = reviewer(&harness).await;
    let mine = rand_id("self");
    send(&harness, &rev_session, "POST", "/plugins", Some(json!({ "id": mine, "name": "Self", "category": "appearance" }))).await;
    let (_, body) = send(&harness, &rev_session, "POST", &format!("/plugins/{mine}/releases"), Some(json!({ "manifest": manifest(&mine, "1.0.0", &["theme.colors"]) }))).await;
    let own: ReleaseView = serde_json::from_str(&body).unwrap();
    let (status, _) = send(&harness, &rev_session, "POST", &format!("/plugins/releases/{}/approve", own.id), Some(json!({}))).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn suspension_reports_and_settings() {
    let harness = TestHarness::new().await;
    let (_, dev, dev_user) = harness.new_user().await;
    let (rev, _) = reviewer(&harness).await;
    let id = &rand_id("focus");
    send(&harness, &dev, "POST", "/plugins", Some(json!({ "id": id, "name": "Focus", "category": "productivity" }))).await;
    let mut m = manifest(id, "1.0.0", &["theme.colors", "storage.settings"]);
    m["contributes"]["settings"] = json!([{ "key": "accent", "type": "color", "title": "Accent", "default": "#7aa2ff" }]);
    m["contributes"]["theme"]["colors"]["primary"] = json!("$setting:accent");
    let (status, body) = send(&harness, &dev, "POST", &format!("/plugins/{id}/releases"), Some(json!({ "manifest": m }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    let release: ReleaseView = serde_json::from_str(&body).unwrap();
    send(&harness, &rev, "POST", &format!("/plugins/releases/{}/approve", release.id), Some(json!({}))).await;

    let (_, user, _) = harness.new_user().await;
    send(&harness, &user, "PUT", &format!("/plugins/@me/installs/{id}"), Some(json!({ "release_id": release.id, "permissions": ["theme.colors", "storage.settings"] }))).await;

    // settings are checked against the manifest
    let (status, _) = send(&harness, &user, "PUT", &format!("/plugins/@me/installs/{id}/settings"), Some(json!({ "values": { "accent": "red; background:url(x)" } }))).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &user, "PUT", &format!("/plugins/@me/installs/{id}/settings"), Some(json!({ "values": { "accent": "#ff8800", "__proto__": { "x": 1 } } }))).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body, r##"{"accent":"#ff8800"}"##);

    // suspension blocks running and enabling
    send(&harness, &rev, "POST", &format!("/plugins/{id}/suspend"), Some(json!({ "reason": "investigating" }))).await;
    let (_, body) = send(&harness, &user, "GET", "/plugins/@me/installs", None).await;
    let installs: Vec<InstallView> = serde_json::from_str(&body).unwrap();
    assert_eq!(installs[0].blocked.as_deref(), Some("suspended"));
    send(&harness, &user, "PATCH", &format!("/plugins/@me/installs/{id}"), Some(json!({ "enabled": false }))).await;
    let (status, _) = send(&harness, &user, "PATCH", &format!("/plugins/@me/installs/{id}"), Some(json!({ "enabled": true }))).await;
    assert_eq!(status, Status::BadRequest);
    let (_, body) = send(&harness, &rev, "POST", &format!("/plugins/{id}/reinstate"), Some(json!({}))).await;
    assert_eq!(serde_json::from_str::<PluginCard>(&body).unwrap().status, "Listed");

    // five reports from brand-new accounts don't take it down
    let status_now = |body: String| serde_json::from_str::<PluginDetails>(&body).unwrap().plugin.status;
    for _ in 0..5 {
        let (_, reporter, _) = harness.new_user().await;
        let (status, _) = send(&harness, &reporter, "POST", &format!("/plugins/{id}/report"), Some(json!({ "reason": "spam" }))).await;
        assert_eq!(status, Status::NoContent);
    }
    let (_, body) = send(&harness, &rev, "GET", &format!("/plugins/{id}"), None).await;
    assert_eq!(status_now(body), "Listed");

    // five from settled accounts (older than a week) suspend it automatically
    let month_ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 - 30 * 86_400_000;
    for n in 0..4 {
        harness.db.report_plugin(id, &ulid::Ulid::from_parts(month_ago, n).to_string()).await.unwrap();
    }
    let (_, body) = send(&harness, &rev, "GET", &format!("/plugins/{id}"), None).await;
    assert_eq!(status_now(body), "Listed");
    harness.db.report_plugin(id, &ulid::Ulid::from_parts(month_ago, 9).to_string()).await.unwrap();
    // the next report (from anyone) checks the count again
    let (_, reporter, _) = harness.new_user().await;
    send(&harness, &reporter, "POST", &format!("/plugins/{id}/report"), Some(json!({ "reason": "spam" }))).await;
    let (_, body) = send(&harness, &rev, "GET", &format!("/plugins/{id}"), None).await;
    assert_eq!(status_now(body), "Suspended");

    // a suspended publisher can't submit
    send(&harness, &rev, "POST", &format!("/plugins/publishers/{}/suspend", dev_user.id), Some(json!({ "reason": "abuse" }))).await;
    let (status, _) = send(&harness, &dev, "POST", "/plugins", Some(json!({ "id": rand_id("next"), "name": "Next", "category": "productivity" }))).await;
    assert_eq!(status, Status::Forbidden);
}

#[rocket::async_test]
async fn removed_plugins_leave_the_review_queue() {
    let harness = TestHarness::new().await;
    let (_, dev, _) = harness.new_user().await;
    let (rev, _) = reviewer(&harness).await;
    let id = &rand_id("gone");
    let release = publish(&harness, &dev, id).await;
    let queued = |body: String| body.contains(&release.id);
    let (_, body) = send(&harness, &rev, "GET", "/plugins/review/queue", None).await;
    assert!(queued(body));
    send(&harness, &dev, "POST", &format!("/plugins/{id}/remove"), Some(json!({}))).await;
    let (_, body) = send(&harness, &rev, "GET", "/plugins/review/queue", None).await;
    assert!(!queued(body));
    let (status, body) = send(&harness, &rev, "POST", &format!("/plugins/releases/{}/approve", release.id), Some(json!({}))).await;
    assert_eq!(status, Status::BadRequest, "{body}");
}
