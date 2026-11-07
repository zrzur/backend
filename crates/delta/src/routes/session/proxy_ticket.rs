//! Ask for a media proxy ticket
//! POST /session/proxy_ticket
use chrono::Utc;
use revolt_config::config;
use revolt_database::{
    util::proxy_ticket::{sign_proxy_ticket, PROXY_TICKET_SECONDS},
    Session,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;

/// # Media Proxy Ticket
///
/// Fetch a short-lived ticket for loading media through the proxy. Pass it
/// as `t` on `GET /proxy`. Each ticket works for one hour.
#[openapi(tag = "Session")]
#[post("/proxy_ticket")]
pub async fn proxy_ticket(session: Session) -> Result<Json<v0::ProxyTicket>> {
    let key = config().await.january.proxy_key;
    // an empty key would sign tickets anyone can make
    if key.is_empty() {
        return Err(create_error!(FeatureDisabled {
            feature: "proxy_ticket".to_owned()
        }));
    }

    let expires_at = Utc::now().timestamp() + PROXY_TICKET_SECONDS;
    Ok(Json(v0::ProxyTicket {
        ticket: sign_proxy_ticket(&key, &session.user_id, expires_at),
        expires_at,
    }))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use chrono::Utc;
    use revolt_database::util::proxy_ticket::sign_proxy_ticket;
    use revolt_models::v0;
    use rocket::http::{Header, Status};

    #[rocket::async_test]
    async fn a_session_gets_a_ticket_for_one_hour() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let res = harness
            .client
            .post("/auth/session/proxy_ticket")
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);

        let body: v0::ProxyTicket = res.into_json().await.unwrap();
        let left = body.expires_at - Utc::now().timestamp();
        assert!(left > 3590 && left <= 3600, "{left}");
        assert_eq!(
            body.ticket,
            sign_proxy_ticket("test-proxy-key", &user.id, body.expires_at)
        );
    }

    #[rocket::async_test]
    async fn no_session_gets_no_ticket() {
        let harness = TestHarness::new().await;
        let res = harness
            .client
            .post("/auth/session/proxy_ticket")
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Unauthorized);
    }
}
