use axum::{extract::{FromRef, FromRequestParts}, http::request::Parts};

use revolt_result::{create_error, Error, Result};

use crate::{Database, Session};

#[async_trait]
impl<S> FromRequestParts<S> for Session
where
    Database: FromRef<S>,
    S: Send + Sync
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let db = Database::from_ref(state);

        if let Some(Ok(token)) = parts.headers.get("x-session-token").map(|v| v.to_str()) {
            Session::from_token(&db, token).await
        } else {
            Err(create_error!(MissingHeaders))
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{extract::FromRequestParts, http::Request};
    use iso8601_timestamp::{Duration, Timestamp};
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use revolt_result::ErrorType;

    use crate::{Account, Session, User};

    fn parts(token: &str) -> axum::http::request::Parts {
        Request::builder()
            .header("x-session-token", token)
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    #[tokio::test]
    async fn axum_services_refuse_an_expired_session() {
        database_test!(|db| async move {
            let account = Account::new(
                &db,
                format!("{}@zrzur.cc", ulid::Ulid::new()),
                "password_insecure".to_owned(),
                false,
            )
            .await
            .unwrap();
            let mut session = account.create_session(&db, String::new()).await.unwrap();

            assert!(Session::from_request_parts(&mut parts(&session.token), &db)
                .await
                .is_ok());

            session.last_seen = Timestamp::now_utc() + Duration::days(-31);
            db.save_session(&session).await.unwrap();

            let error = Session::from_request_parts(&mut parts(&session.token), &db)
                .await
                .unwrap_err();
            assert!(matches!(error.error_type, ErrorType::InvalidSession));
            let error = User::from_request_parts(&mut parts(&session.token), &db)
                .await
                .unwrap_err();
            assert!(matches!(error.error_type, ErrorType::InvalidSession));
        });
    }
}
