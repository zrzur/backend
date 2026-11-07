//! "Download my data"
//! GET /account/data-export, POST /account/data-export,
//! GET /account/data-export/download
use std::io::Cursor;

use revolt_database::{
    Account, DataExport, DataExportStatus, Database, ValidatedTicket, ACCOUNT_CONTACT_EMAIL,
    ACCOUNT_DELETION_GRACE_DAYS,
};
use revolt_result::{create_error, Result};
use rocket::{
    http::{ContentType, Status},
    response::{self, Responder},
    serde::json::Json,
    Request, Response, State,
};
use schemars::JsonSchema;
use serde::Serialize;

/// Where a person's data export is
#[derive(Serialize, JsonSchema, Debug)]
pub struct DataExportInfo {
    /// `none`, `pending`, `ready` or `failed`
    pub status: String,
    /// When the copy was asked for
    pub requested_at: Option<String>,
    /// When the file is removed
    pub expires_at: Option<String>,
    /// Seconds until another copy can be asked for (0 = now)
    pub next_request_in_seconds: u64,
    /// Days between confirming an account deletion and its erasure
    pub deletion_grace_days: i64,
    /// Where to write for anything the page cannot do
    pub contact_email: String,
}

fn info(export: Option<DataExport>) -> DataExportInfo {
    let base = |status: &str| DataExportInfo {
        status: status.into(),
        requested_at: None,
        expires_at: None,
        next_request_in_seconds: 0,
        deletion_grace_days: ACCOUNT_DELETION_GRACE_DAYS,
        contact_email: ACCOUNT_CONTACT_EMAIL.to_string(),
    };
    match export {
        None => base("none"),
        Some(export) => {
            let status = match export.status {
                DataExportStatus::Pending => "pending",
                DataExportStatus::Ready => "ready",
                DataExportStatus::Failed => "failed",
            };
            DataExportInfo {
                requested_at: Some(export.requested_at.format().to_string()),
                expires_at: export.expires_at.map(|at| at.format().to_string()),
                next_request_in_seconds: if export.status == DataExportStatus::Failed {
                    0
                } else {
                    export.seconds_until_next_request()
                },
                ..base(status)
            }
        }
    }
}

/// # Fetch Data Export
///
/// Where your copy of your data is: not asked for, being prepared, ready
/// or failed.
#[openapi(tag = "Account")]
#[get("/data-export")]
pub async fn fetch_data_export(
    db: &State<Database>,
    account: Account,
) -> Result<Json<DataExportInfo>> {
    Ok(Json(info(db.fetch_data_export(&account.id).await?)))
}

/// # Request Data Export
///
/// Ask for a ZIP with your data. It is prepared in the background, an e-mail
/// says when it is ready, and it is kept for 7 days. One request per 24
/// hours. Needs a fresh confirmation (password or two-factor code).
#[openapi(tag = "Account")]
#[post("/data-export")]
pub async fn request_data_export(
    db: &State<Database>,
    account: Account,
    _ticket: ValidatedTicket,
) -> Result<Json<DataExportInfo>> {
    let export = DataExport::request(db, &account).await?;
    Ok(Json(info(Some(export))))
}

/// The ZIP, as a download
pub struct ZipDownload(Vec<u8>);

impl<'r> Responder<'r, 'static> for ZipDownload {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        Response::build()
            .status(Status::Ok)
            .header(ContentType::ZIP)
            .raw_header(
                "Content-Disposition",
                "attachment; filename=\"zrzur-data.zip\"",
            )
            .raw_header("Cache-Control", "no-store")
            .raw_header("Referrer-Policy", "no-referrer")
            .sized_body(self.0.len(), Cursor::new(self.0))
            .ok()
    }
}

/// # Download Data Export
///
/// The ZIP, for the signed-in owner only. Nothing secret is in the address.
#[openapi(skip)]
#[get("/data-export/download")]
pub async fn download_data_export(db: &State<Database>, account: Account) -> Result<ZipDownload> {
    let export = db
        .fetch_data_export(&account.id)
        .await?
        .filter(|export| export.status == DataExportStatus::Ready)
        .ok_or_else(|| create_error!(NotFound))?;
    if export
        .expires_at
        .is_some_and(|at| at <= iso8601_timestamp::Timestamp::now_utc())
    {
        return Err(create_error!(NotFound));
    }
    Ok(ZipDownload(db.fetch_data_export_bytes(&export.id).await?))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{DataExport, DataExportStatus, MFATicket};
    use rocket::http::{Header, Status};

    async fn request(harness: &TestHarness, token: &str, account_id: &str) -> Status {
        let ticket = MFATicket::new(account_id.to_string(), true);
        ticket.save(&harness.db).await.unwrap();
        harness
            .client
            .post("/auth/account/data-export")
            .header(Header::new("X-Session-Token", token.to_string()))
            .header(Header::new("X-MFA-Ticket", ticket.token))
            .dispatch()
            .await
            .status()
    }

    #[rocket::async_test]
    async fn request_build_download_and_limit() {
        let harness = TestHarness::new().await;
        let (account, session, user) = harness.new_user().await;
        harness.new_server(&user).await;

        // needs the confirmation
        let res = harness
            .client
            .post("/auth/account/data-export")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_ne!(res.status(), Status::Ok);

        assert_eq!(request(&harness, &session.token, &account.id).await, Status::Ok);
        // one request per 24 hours
        assert_eq!(
            request(&harness, &session.token, &account.id).await,
            Status::TooManyRequests
        );

        // crond builds it
        let mut export = harness
            .db
            .fetch_data_export(&account.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(export.status, DataExportStatus::Pending);
        export.process(&harness.db).await.unwrap();

        let res = harness
            .client
            .get("/auth/account/data-export")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        let body: serde_json::Value = res.into_json().await.unwrap();
        assert_eq!(body["status"], "ready");
        assert!(body.get("download_url").is_none());
        assert_eq!(body["contact_email"], "privacy@zrzur.com");

        // another signed-in person gets nothing, and so does nobody
        let (_, other_session, _) = harness.new_user().await;
        let res = harness
            .client
            .get("/auth/account/data-export/download")
            .header(Header::new("X-Session-Token", other_session.token))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::NotFound);
        let res = harness
            .client
            .get("/auth/account/data-export/download")
            .dispatch()
            .await;
        assert_ne!(res.status(), Status::Ok);

        let export = harness
            .db
            .fetch_data_export(&account.id)
            .await
            .unwrap()
            .unwrap();
        let res = harness
            .client
            .get("/auth/account/data-export/download")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.headers().get_one("Referrer-Policy"), Some("no-referrer"));
        assert_eq!(res.status(), Status::Ok);
        let bytes = res.into_bytes().await.unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4b, 0x03, 0x04]);
        let text = String::from_utf8_lossy(&bytes);
        for name in [
            "account.json",
            "profile.json",
            "messages.json",
            "files.json",
            "servers.json",
            "purchases.json",
            "reports.json",
            "settings.json",
            "support.json",
            "bots.json",
            "creations.json",
        ] {
            assert!(text.contains(name), "{name} missing");
        }
        // no secrets
        assert!(!text.contains(&account.password));
        assert!(!text.contains(&session.token));

        // an expired link gets nothing, and crond removes the file
        let mut export: DataExport = export;
        export.expires_at = Some(iso8601_timestamp::Timestamp::UNIX_EPOCH);
        harness.db.save_data_export(&export).await.unwrap();
        let res = harness
            .client
            .get("/auth/account/data-export/download")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::NotFound);
        let expired = harness.db.fetch_expired_data_exports().await.unwrap();
        assert!(expired.iter().any(|e| e.id == account.id));
    }
}

#[cfg(test)]
mod account_data_tests {
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::{Duration, Timestamp};
    use revolt_database::{
        AccountAge, DeletionInfo, PartialUser, Totp, ACCOUNT_DELETION_GRACE_DAYS,
    };
    use rocket::http::{ContentType, Header, Status};

    /// P-08: under 16 at the first statement = nothing stored
    #[rocket::async_test]
    async fn under_sixteen_at_first_entry_leaves_nothing() {
        let harness = TestHarness::new().await;
        let (account, session, user) = harness.new_user_without_age().await;
        let email = account.email.clone();

        let res = harness
            .client
            .patch("/auth/account/age")
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(r#"{"birth_month":"2014-05"}"#)
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Forbidden);

        assert!(harness.db.fetch_account(&account.id).await.is_err());
        assert!(harness.db.fetch_user(&user.id).await.is_err());
        assert!(harness.db.fetch_session_by_token(&session.token).await.is_err());
        assert!(harness.db.fetch_sessions(&account.id).await.unwrap().is_empty());
        assert!(harness
            .db
            .fetch_account_by_normalised_email(&revolt_database::util::email::normalise_email(
                email.clone()
            ))
            .await
            .unwrap()
            .is_none());

        // the same e-mail cannot log in
        let res = harness
            .client
            .post("/auth/session/login")
            .header(ContentType::JSON)
            .body(format!(r#"{{"email":"{email}","password":"password_insecure"}}"#))
            .dispatch()
            .await;
        assert_ne!(res.status(), Status::Ok);
    }

    /// P-07: deleting clears the secrets, the age, the sessions and the profile
    #[rocket::async_test]
    async fn deletion_clears_everything() {
        let harness = TestHarness::new().await;
        let (mut account, session, mut user) = harness.new_user().await;
        account.age = Some(AccountAge::declare((1990, 1)));
        account.mfa.totp_token = Totp::Enabled { secret: "JBSWY3DPEHPK3PXP".into() };
        account.mfa.generate_recovery_codes();
        account.save(&harness.db).await.unwrap();
        user.update(
            &harness.db,
            PartialUser {
                display_name: Some("Someone".into()),
                pronouns: Some("they/them".into()),
                locale: Some("ar".into()),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

        harness
            .db
            .set_user_settings(
                &user.id,
                &std::collections::HashMap::from([("theme".to_string(), (1, "dark".to_string()))]),
            )
            .await
            .unwrap();

        user.mark_deleted(&harness.db).await.unwrap();
        account.mark_deleted(&harness.db).await.unwrap();

        let account = harness.db.fetch_account(&account.id).await.unwrap();
        assert!(account.password.is_empty());
        assert!(account.age.is_none());
        assert!(account.mfa.recovery_codes.is_empty());
        assert!(account.mfa.totp_token.is_disabled());
        assert!(matches!(account.deletion, Some(DeletionInfo::Deleted)));
        assert!(harness.db.fetch_session_by_token(&session.token).await.is_err());
        let user = harness.db.fetch_user(&user.id).await.unwrap();
        assert!(user.display_name.is_none());
        assert!(user.pronouns.is_none());
        assert!(user.style.is_none());
        assert!(user.locale.is_none());
        assert!(user.privacy.is_none());
        assert!(user.plus.is_none());
        let settings = harness.db.fetch_user_settings(&user.id, &[]).await;
        assert!(settings.map(|s| s.is_empty()).unwrap_or(true));
    }

    /// An account from before the age question that owns a server is never
    /// removed by an under-16 answer: it is restricted, the server stays
    #[rocket::async_test]
    async fn older_account_with_a_server_is_restricted_not_removed() {
        let harness = TestHarness::new().await;
        let (account, session, user) = harness.new_user_without_age().await;
        let (server, _) = harness.new_server(&user).await;

        let res = harness
            .client
            .patch("/auth/account/age")
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", session.token.clone()))
            .body(r#"{"birth_month":"2014-05"}"#)
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);

        let stored = harness.db.fetch_account(&account.id).await.unwrap();
        assert!(stored.age.is_some());
        assert!(!stored.disabled);
        assert!(harness.db.fetch_user(&user.id).await.is_ok());
        assert!(harness.db.fetch_server(&server.id).await.is_ok());
        assert!(harness.db.fetch_session_by_token(&session.token).await.is_ok());
    }

    /// Created more than a day ago = not new, whatever it owns
    #[rocket::async_test]
    async fn account_created_long_ago_is_not_new() {
        let harness = TestHarness::new().await;
        let old_ms = (Timestamp::now_utc()
            .duration_since(Timestamp::UNIX_EPOCH)
            .whole_milliseconds() as u64)
            - 3 * 24 * 3_600_000;
        let id = ulid::Ulid::from_parts(old_ms, 1).to_string();
        let (account, _) = harness.account_from_user(id).await;
        assert!(!account.is_new_and_empty(&harness.db).await);

        let (fresh, _, _) = harness.new_user().await;
        assert!(fresh.is_new_and_empty(&harness.db).await);
    }

    /// A new account that owns a bot is not "empty"
    #[rocket::async_test]
    async fn new_account_with_a_bot_is_not_empty() {
        let harness = TestHarness::new().await;
        let (account, _, user) = harness.new_user_without_age().await;
        assert!(account.is_new_and_empty(&harness.db).await);
        harness.new_bot(&user).await;
        assert!(!account.is_new_and_empty(&harness.db).await);
    }

    /// A new account that has sent a message is not "empty"
    #[rocket::async_test]
    async fn new_account_with_a_message_is_not_empty() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (account, _, user) = harness.new_user_without_age().await;
        assert!(account.is_new_and_empty(&harness.db).await);
        harness.new_message(&user, &server, channels).await;
        assert!(!account.is_new_and_empty(&harness.db).await);
    }

    /// The grace period is one value
    #[rocket::async_test]
    async fn grace_period_comes_from_the_constant() {
        let harness = TestHarness::new().await;
        let (mut account, _, _) = harness.new_user().await;
        account.schedule_deletion(&harness.db).await.unwrap();
        let Some(DeletionInfo::Scheduled { after }) = account.deletion else {
            panic!("not scheduled")
        };
        let expected = Timestamp::now_utc() + Duration::days(ACCOUNT_DELETION_GRACE_DAYS);
        assert!((after.duration_since(expected).whole_seconds()).abs() < 60);
    }
}
