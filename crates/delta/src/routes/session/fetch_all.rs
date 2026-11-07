//! Fetch all sessions
//! GET /session/all
use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_result::Result;
use revolt_database::{Database, Session};
use revolt_models::v0;
use rocket::serde::json::Json;
use rocket::State;

/// # Fetch Sessions
///
/// Fetch all sessions associated with this account.
#[openapi(tag = "Session")]
#[get("/all")]
pub async fn fetch_all(
    db: &State<Database>,
    session: Session,
) -> Result<Json<Vec<v0::SessionInfo>>> {
    let now = Timestamp::now_utc();
    let idle_days = config().await.api.sessions.idle_days;

    // one that has expired but is not swept yet no longer works: do not list it
    db
        .fetch_sessions(&session.user_id)
        .await
        .map(|ok| {
            ok.into_iter()
                .filter(|session| !session.is_idle_at(now, idle_days))
                .map(|session| session.into())
                .collect()
        })
        .map(Json)
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::{Duration, Timestamp};
    use rocket::http::{Header, Status};
    use revolt_models::v0;

    #[rocket::async_test]
    async fn success() {
        let harness = TestHarness::new().await;
        let (account, session, _) = harness.new_user().await;

        for i in 1..=3 {
            account
                .create_session(&harness.db, format!("session{}", i))
                .await
                .unwrap();
        }

        let res = harness.client
            .get("/auth/session/all")
            .header(Header::new("X-Session-Token", session.token))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        assert_eq!(res.into_json::<Vec<v0::SessionInfo>>().await.unwrap().len(), 4);
    }

    #[rocket::async_test]
    async fn lists_when_each_session_was_last_used_and_leaves_out_expired_ones() {
        let harness = TestHarness::new().await;
        let (account, session, _) = harness.new_user().await;

        let used = Timestamp::now_utc() + Duration::days(-3);
        let mut other = account
            .create_session(&harness.db, "other".to_string())
            .await
            .unwrap();
        other.last_seen = used;
        harness.db.save_session(&other).await.unwrap();

        let mut expired = account
            .create_session(&harness.db, "expired".to_string())
            .await
            .unwrap();
        expired.last_seen = Timestamp::now_utc() + Duration::days(-31);
        harness.db.save_session(&expired).await.unwrap();

        let res = harness.client
            .get("/auth/session/all")
            .header(Header::new("X-Session-Token", session.token))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        let list = res.into_json::<Vec<v0::SessionInfo>>().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(!list.iter().any(|entry| entry.id == expired.id));
        let listed = list.iter().find(|entry| entry.id == other.id).unwrap();
        // stored times keep milliseconds
        assert_eq!(listed.last_seen.duration_since(used).whole_seconds(), 0);
    }

    #[rocket::async_test]
    async fn an_expired_token_is_refused_on_any_route() {
        let harness = TestHarness::new().await;
        let (_, mut session, _) = harness.new_user().await;

        session.last_seen = Timestamp::now_utc() + Duration::days(-31);
        harness.db.save_session(&session).await.unwrap();

        for path in ["/auth/session/all", "/users/@me"] {
            let res = harness.client
                .get(path)
                .header(Header::new("X-Session-Token", session.token.clone()))
                .dispatch()
                .await;
            assert_eq!(res.status(), Status::Unauthorized, "{path}");
        }
    }

    #[rocket::async_test]
    async fn use_through_http_counts_as_use() {
        let harness = TestHarness::new().await;
        let (_, mut session, _) = harness.new_user().await;

        let stale = Timestamp::now_utc() + Duration::days(-29);
        session.last_seen = stale;
        harness.db.save_session(&session).await.unwrap();

        let res = harness.client
            .get("/users/@me")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);

        let stored = harness.db.fetch_session(&session.id).await.unwrap();
        assert!(stored.last_seen > stale);
    }
}
