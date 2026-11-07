//! Invites expire after 7 days unless their creator chose another time or
//! "never".

use crate::{rocket, util::test::TestHarness};
use iso8601_timestamp::{Duration, Timestamp};
use rocket::http::{ContentType, Header, Status};

struct Setup {
    harness: TestHarness,
    channel: String,
    token: String,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (_, channels) = harness.new_server(&owner).await;
    Setup {
        channel: channels[0].id().to_string(),
        token: session.token.clone(),
        harness,
    }
}

impl Setup {
    async fn create(&self, body: Option<serde_json::Value>) -> (Status, serde_json::Value) {
        let mut request = self
            .harness
            .client
            .post(format!("/channels/{}/invites", self.channel))
            .header(Header::new("x-session-token", self.token.clone()));
        if let Some(body) = body {
            request = request.header(ContentType::JSON).body(body.to_string());
        }
        let response = request.dispatch().await;
        let status = response.status();
        let body = response
            .into_string()
            .await
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        (status, body)
    }
}

fn expires(invite: &serde_json::Value) -> Option<Timestamp> {
    invite["expires"]
        .as_str()
        .map(|value| serde_json::from_value(serde_json::Value::String(value.into())).unwrap())
}

fn about_from_now(at: Timestamp, after: Duration) -> bool {
    let target = Timestamp::now_utc() + after;
    at > target - Duration::minutes(2) && at < target + Duration::minutes(2)
}

#[rocket::async_test]
async fn an_invite_without_a_body_expires_in_7_days() {
    let setup = setup().await;
    for body in [None, Some(json!({})), Some(json!({ "max_uses": 5 }))] {
        let (status, invite) = setup.create(body).await;
        assert_eq!(status, Status::Ok, "{invite}");
        let at = expires(&invite).expect("an expiry");
        assert!(about_from_now(at, Duration::days(7)), "{invite}");
    }
}

#[rocket::async_test]
async fn never_stays_possible_when_chosen() {
    let setup = setup().await;
    let (status, invite) = setup.create(Some(json!({ "never_expires": true }))).await;
    assert_eq!(status, Status::Ok, "{invite}");
    assert_eq!(expires(&invite), None, "{invite}");
}

#[rocket::async_test]
async fn a_chosen_time_and_uses_are_kept() {
    let setup = setup().await;
    let at = Timestamp::now_utc() + Duration::hours(1);
    let (status, invite) = setup
        .create(Some(json!({ "expires": at, "max_uses": 10 })))
        .await;
    assert_eq!(status, Status::Ok, "{invite}");
    assert!(about_from_now(expires(&invite).unwrap(), Duration::hours(1)), "{invite}");
    assert_eq!(invite["max_uses"], 10);
}

#[rocket::async_test]
async fn more_than_30_days_or_never_with_a_time_is_refused() {
    let setup = setup().await;
    let late = Timestamp::now_utc() + Duration::days(31);
    let (status, _) = setup.create(Some(json!({ "expires": late }))).await;
    assert_eq!(status, Status::BadRequest);

    let soon = Timestamp::now_utc() + Duration::hours(1);
    let (status, _) = setup
        .create(Some(json!({ "expires": soon, "never_expires": true })))
        .await;
    assert_eq!(status, Status::BadRequest);
}
