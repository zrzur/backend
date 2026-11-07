//! Declare or correct your age
//! PATCH /account/age
use revolt_database::{Account, Database};
use revolt_models::v0;
use revolt_result::Result;
use rocket::serde::json::Json;
use rocket::State;

/// # Change Age
///
/// Declare or correct the year and month of birth on your account.
///
/// Making yourself younger is always allowed. Making yourself older is
/// allowed once, and only within the same age group; anything else goes
/// through support. A declared age is never treated as proof of age.
#[openapi(tag = "Account")]
#[patch("/age", data = "<data>")]
pub async fn change_age(
    db: &State<Database>,
    mut account: Account,
    data: Json<v0::DataChangeAge>,
) -> Result<Json<v0::AccountInfo>> {
    let data = data.into_inner();

    account
        .change_age(db, &data.birth_month, data.current_password.as_deref())
        .await?;

    Ok(Json(account.into_info().await))
}

#[cfg(test)]
mod tests {
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::Timestamp;
    use revolt_database::AccountAge;
    use revolt_models::v0::{self, AgeBand, MatureAccess};
    use revolt_result::{Error, ErrorType};
    use rocket::http::{ContentType, Header, Status};

    /// Year and month for someone who turned `years` this month
    fn born(years: i32) -> (i32, u8) {
        let now = Timestamp::now_utc();
        (now.year() - years, u8::from(now.month()))
    }

    fn month(birth: (i32, u8)) -> String {
        format!("{:04}-{:02}", birth.0, birth.1)
    }

    async fn change<'a>(
        harness: &'a TestHarness,
        token: &str,
        body: serde_json::Value,
    ) -> rocket::local::asynchronous::LocalResponse<'a> {
        harness
            .client
            .patch("/auth/account/age")
            .header(ContentType::JSON)
            .header(Header::new("X-Session-Token", token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await
    }

    async fn info(res: rocket::local::asynchronous::LocalResponse<'_>) -> v0::AccountAgeInfo {
        res.into_json::<v0::AccountInfo>()
            .await
            .unwrap()
            .age
            .expect("age")
    }

    #[rocket::async_test]
    async fn existing_account_without_age() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user_without_age().await;

        let res = harness
            .client
            .get("/auth/account")
            .header(Header::new("X-Session-Token", session.token))
            .dispatch()
            .await;

        assert_eq!(res.status(), Status::Ok);
        let age = info(res).await;
        assert_eq!(age.band, AgeBand::Unknown);
        assert_eq!(age.mature_access, MatureAccess::AgeUnknown);
        assert!(age.birth_month.is_none());
    }

    #[rocket::async_test]
    async fn success_first_declaration() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user_without_age().await;

        let res = change(
            &harness,
            &session.token,
            json!({ "birth_month": month(born(30)) }),
        )
        .await;

        assert_eq!(res.status(), Status::Ok);
        let age = info(res).await;
        assert_eq!(age.band, AgeBand::AdultDeclared);
        assert_eq!(age.mature_access, MatureAccess::Allowed);
        assert!(age.can_raise);
    }

    #[rocket::async_test]
    async fn success_lower_with_password() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user_without_age().await;
        account.age = Some(AccountAge::declare((1990, 1)));
        account.save(&harness.db).await.unwrap();

        let res = change(
            &harness,
            &session.token,
            json!({
                "birth_month": month(born(17)),
                "current_password": "password_insecure"
            }),
        )
        .await;

        assert_eq!(res.status(), Status::Ok);
        let age = info(res).await;
        assert_eq!(age.band, AgeBand::Teen);
        assert_eq!(age.mature_access, MatureAccess::Minor);

        // only the change of group is logged, not the dates
        let account = harness.db.fetch_account(&account.id).await.unwrap();
        let changes = account.age.expect("age").changes;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].from, AgeBand::AdultDeclared);
        assert_eq!(changes[0].to, AgeBand::Teen);
    }

    #[rocket::async_test]
    async fn fail_change_without_password() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user_without_age().await;
        account.age = Some(AccountAge::declare((1990, 1)));
        account.save(&harness.db).await.unwrap();

        let res = change(&harness, &session.token, json!({ "birth_month": "1995-01" })).await;

        assert_eq!(res.status(), Status::Unauthorized);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::InvalidCredentials,
        ));
    }

    #[rocket::async_test]
    async fn fail_raise_teen_to_adult() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user_without_age().await;
        account.age = Some(AccountAge::declare(born(17)));
        account.save(&harness.db).await.unwrap();

        let res = change(
            &harness,
            &session.token,
            json!({
                "birth_month": month(born(25)),
                "current_password": "password_insecure"
            }),
        )
        .await;

        assert_eq!(res.status(), Status::Forbidden);
        assert!(matches!(
            res.into_json::<Error>().await.unwrap().error_type,
            ErrorType::AgeChangeNotAllowed,
        ));
    }

    #[rocket::async_test]
    async fn lower_then_raise_cannot_climb_back() {
        let harness = TestHarness::new().await;
        let (mut account, session, _) = harness.new_user_without_age().await;
        account.age = Some(AccountAge::declare((1990, 6)));
        account.save(&harness.db).await.unwrap();

        // one correction within the group
        let first = change(
            &harness,
            &session.token,
            json!({ "birth_month": "1990-01", "current_password": "password_insecure" }),
        )
        .await;
        assert_eq!(first.status(), Status::Ok);
        assert!(!info(first).await.can_raise);

        // lowering is still fine, but there is no way back up
        let lower = change(
            &harness,
            &session.token,
            json!({ "birth_month": "1995-01", "current_password": "password_insecure" }),
        )
        .await;
        assert_eq!(lower.status(), Status::Ok);

        let again = change(
            &harness,
            &session.token,
            json!({ "birth_month": "1990-01", "current_password": "password_insecure" }),
        )
        .await;
        assert_eq!(again.status(), Status::Forbidden);
    }

    #[rocket::async_test]
    async fn under_minimum_is_restricted_not_deleted() {
        let harness = TestHarness::new().await;
        // an age on record, lowered below the minimum
        let (account, session, _) = harness.new_user().await;

        let res = change(
            &harness,
            &session.token,
            json!({ "birth_month": month(born(14)), "current_password": "password_insecure" }),
        )
        .await;
        assert_eq!(res.status(), Status::Ok);
        let age = info(res).await;
        assert_eq!(age.band, AgeBand::UnderMinimum);
        assert_eq!(age.mature_access, MatureAccess::Minor);

        // no automatic action: the account stays usable and is left for review
        let account = harness.db.fetch_account(&account.id).await.unwrap();
        assert!(!account.disabled);
        assert!(account.deletion.is_none());

        // and it cannot be raised back out of the group by itself
        let res = change(
            &harness,
            &session.token,
            json!({ "birth_month": month(born(20)), "current_password": "password_insecure" }),
        )
        .await;
        assert_eq!(res.status(), Status::Forbidden);
    }
}

#[cfg(test)]
mod declared_age_tests {
    use crate::{rocket, util::test::TestHarness};
    use iso8601_timestamp::Timestamp;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    /// The server, not only the app, waits for a declared age
    #[rocket::async_test]
    async fn everything_waits_until_an_age_is_declared() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user_without_age().await;
        let get = |path: &'static str| {
            harness
                .client
                .get(path)
                .header(Header::new("X-Session-Token", session.token.clone()))
        };

        // refused until declared, with a reason the app can act on
        let res = get("/users/@me").dispatch().await;
        assert_eq!(res.status(), Status::Forbidden);
        assert!(res.into_string().await.unwrap().contains("AgeNotDeclared"));
        let res = harness
            .client
            .post("/servers/create")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "name": "Mine" }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Forbidden);

        // the account itself and declaring the age stay open
        assert_eq!(get("/auth/account").dispatch().await.status(), Status::Ok);
        let now = Timestamp::now_utc();
        let res = harness
            .client
            .patch("/auth/account/age")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "birth_month": format!("{:04}-{:02}", now.year() - 25, u8::from(now.month())) }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Ok);

        // then everything works
        assert_eq!(get("/users/@me").dispatch().await.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn under_16_is_still_refused_and_nothing_is_stored() {
        let harness = TestHarness::new().await;
        let (account, session, _) = harness.new_user_without_age().await;
        let now = Timestamp::now_utc();
        let res = harness
            .client
            .patch("/auth/account/age")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "birth_month": format!("{:04}-{:02}", now.year() - 13, u8::from(now.month())) }).to_string())
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Forbidden);
        // a brand-new, empty account is removed: nothing is stored
        assert!(harness.db.fetch_account(&account.id).await.is_err());
        let res = harness
            .client
            .get("/users/@me")
            .header(Header::new("X-Session-Token", session.token.clone()))
            .dispatch()
            .await;
        assert_eq!(res.status(), Status::Unauthorized);
    }
}
