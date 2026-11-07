use revolt_database::{Database, Session};
use revolt_result::Result;
use rocket_empty::EmptyResponse;

use rocket::State;

/// # Unsubscribe
///
/// Remove the Web Push subscription associated with the current session.
#[openapi(tag = "Web Push")]
#[post("/unsubscribe")]
pub async fn unsubscribe(db: &State<Database>, mut session: Session) -> Result<EmptyResponse> {
    session
        .remove_subscription(db)
        .await
        .map(|_| EmptyResponse)
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::WebPushSubscription;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn the_subscription_is_removed() {
        let harness = TestHarness::new().await;
        let (_, mut session, _) = harness.new_user().await;
        session
            .set_subscription(
                &harness.db,
                WebPushSubscription {
                    endpoint: "https://fcm.googleapis.com/fcm/send/a".to_string(),
                    p256dh: "key".to_string(),
                    auth: "auth".to_string(),
                },
            )
            .await
            .unwrap();

        let res = harness
            .client
            .post("/push/unsubscribe")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::NoContent);

        let stored = harness.db.fetch_session(&session.id).await.unwrap();
        assert!(stored.subscription.is_none());
    }
}
