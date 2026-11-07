use revolt_database::{Database, Session, WebPushSubscription};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// # Push Subscribe
///
/// Create a new Web Push subscription.
///
/// If an existing subscription exists on this session, it will be removed.
#[openapi(tag = "Web Push")]
#[post("/subscribe", data = "<data>")]
pub async fn subscribe(
    db: &State<Database>,
    mut session: Session,
    data: Json<v0::WebPushSubscription>,
) -> Result<EmptyResponse> {
    let subscription: WebPushSubscription = data.into_inner().into();
    if !subscription.is_allowed() {
        return Err(create_error!(InvalidOperation));
    }

    session
        .set_subscription(db, subscription)
        .await
        .map(|_| EmptyResponse)
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use rocket::http::{ContentType, Header, Status};

    async fn subscribe(harness: &TestHarness, token: String, endpoint: &str) -> Status {
        harness
            .client
            .post("/push/subscribe")
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", token))
            .body(json!({ "endpoint": endpoint, "p256dh": "key", "auth": "auth" }).to_string())
            .dispatch()
            .await
            .status()
    }

    #[rocket::async_test]
    async fn a_browser_push_service_is_saved() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;
        let endpoint = "https://fcm.googleapis.com/fcm/send/abc";

        assert_eq!(
            subscribe(&harness, session.token.clone(), endpoint).await,
            Status::NoContent
        );
        let saved = harness.db.fetch_session(&session.id).await.unwrap();
        assert_eq!(saved.subscription.unwrap().endpoint, endpoint);
    }

    #[rocket::async_test]
    async fn an_internal_address_is_refused_and_not_saved() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        assert_eq!(
            subscribe(&harness, session.token.clone(), "https://169.254.169.254/x").await,
            Status::BadRequest
        );
        let saved = harness.db.fetch_session(&session.id).await.unwrap();
        assert!(saved.subscription.is_none());
    }
}
