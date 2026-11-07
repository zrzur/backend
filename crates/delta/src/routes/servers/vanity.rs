use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, FieldsServer, Invite, PartialServer, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// # Claim Custom Invite Link
/// Claim or change this server's custom invite link (zrzur.cc/<code>).
/// Needs Manage Server and the Wings level that unlocks it.
#[openapi(tag = "Server Information")]
#[put("/<server_id>/vanity", data = "<data>")]
pub async fn claim(
    db: &State<Database>,
    user: User,
    server_id: Reference<'_>,
    data: Json<v0::DataVanityInvite>,
) -> Result<Json<v0::ServerVanityInvite>> {
    let mut server = server_id.as_server(db).await?;
    require_manage_server(db, &user, &server).await?;
    require_level(&server).await?;

    let code = normalise_vanity(&data.code)?;
    let channel = revolt_database::landing_channel(db, &server, data.channel.as_deref()).await?;
    let previous = server.vanity_invite.clone();
    let same_name = previous.as_ref().is_some_and(|p| p.code == code);

    // the unique invite code is what keeps two servers off one name
    if same_name {
        db.delete_invite(&code).await.ok();
    }
    db.insert_invite(&Invite::Server {
        code: code.clone(),
        server: server.id.clone(),
        creator: user.id.clone(),
        channel: channel.clone(),
        max_uses: None,
        uses: 0,
        expires: None,
        vanity: true,
    })
    .await
    .map_err(|_| create_error!(VanityTaken))?;
    if let Some(previous) = previous.as_ref().filter(|_| !same_name) {
        db.delete_invite(&previous.code).await.ok();
    }

    let vanity = v0::ServerVanityInvite {
        code,
        channel,
        claimed_at: previous
            .filter(|_| same_name)
            .map(|p| p.claimed_at)
            .unwrap_or_else(Timestamp::now_utc),
        below_level_since: None,
    };
    server
        .update(
            db,
            PartialServer {
                vanity_invite: Some(vanity.clone()),
                ..Default::default()
            },
            vec![],
        )
        .await?;
    Ok(Json(vanity))
}

/// # Remove Custom Invite Link
/// Remove this server's custom invite link and free its name.
#[openapi(tag = "Server Information")]
#[delete("/<server_id>/vanity")]
pub async fn remove(
    db: &State<Database>,
    user: User,
    server_id: Reference<'_>,
) -> Result<EmptyResponse> {
    let mut server = server_id.as_server(db).await?;
    require_manage_server(db, &user, &server).await?;
    if let Some(vanity) = server.vanity_invite.clone() {
        db.delete_invite(&vanity.code).await.ok();
        server
            .update(db, PartialServer::default(), vec![FieldsServer::VanityInvite])
            .await?;
    }
    Ok(EmptyResponse)
}

/// Lowercase a requested name and check it against the rules
pub fn normalise_vanity(code: &str) -> Result<String> {
    let code = code.trim().to_ascii_lowercase();
    let valid = (3..=32).contains(&code.len())
        && code.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if !valid || super::vanity_reserved::is_reserved(&code) {
        return Err(create_error!(VanityReserved));
    }
    Ok(code)
}

async fn require_manage_server(db: &Database, user: &User, server: &Server) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)
}

async fn require_level(server: &Server) -> Result<()> {
    // with billing off nothing is paid for, so the link is open to everyone
    if !config().await.features.billing.available() {
        return Ok(());
    }
    let required = config()
        .await
        .features
        .billing
        .vanity_level()
        .ok_or_else(|| create_error!(VanityUnavailable))?;
    if revolt_database::server_supercharge_level(server).await < required {
        return Err(create_error!(VanityUnavailable));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalise_vanity;
    use iso8601_timestamp::Timestamp;
    use crate::util::test::TestHarness;
    use revolt_config::overwrite_config;
    use revolt_database::{Member, PartialServer, Server, User};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};
    use serde_json::json;

    /// Billing in the built-in test mode, so Wings levels count in tests
    async fn harness() -> TestHarness {
        overwrite_config(|config| {
            config.features.billing.enabled = true;
            config.features.billing.test_mode = true;
            config.features.billing.provider.name = String::new();
        })
        .await;
        TestHarness::new().await
    }

    /// An owner with a server at the given Wings level
    async fn server_at(h: &TestHarness, level: u32) -> (Server, String, User) {
        let (_, session, owner) = h.new_user().await;
        let (server, channels) = h.new_server(&owner).await;
        Member::create(&h.db, &server, &owner, Some(channels)).await.unwrap();
        let mut server = h.db.fetch_server(&server.id).await.unwrap();
        server
            .update(
                &h.db,
                PartialServer {
                    supercharge: Some(v0::ServerSupercharge { count: level * 5, level }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        (server, session.token, owner)
    }

    async fn put(h: &TestHarness, token: &str, server: &str, code: &str) -> Status {
        h.client
            .put(format!("/servers/{server}/vanity"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(json!({ "code": code }).to_string())
            .dispatch()
            .await
            .status()
    }

    async fn fetch_invite(h: &TestHarness, code: &str) -> Status {
        h.client.get(format!("/invites/{code}")).dispatch().await.status()
    }

    #[test]
    fn names_follow_the_rules() {
        assert_eq!(normalise_vanity("Arab-Gamers").unwrap(), "arab-gamers");
        assert_eq!(normalise_vanity("  team42 ").unwrap(), "team42");
        for bad in ["ab", "-team", "team-", "te--am", "team_42", "تيم", "api", "invite", "zrzur", &"a".repeat(33)] {
            assert!(normalise_vanity(bad).is_err(), "{bad}");
        }
    }

    #[rocket::async_test]
    async fn below_level_is_refused() {
        let h = harness().await;
        for level in [0, 1, 2] {
            let (server, token, _) = server_at(&h, level).await;
            assert_eq!(put(&h, &token, &server.id, &format!("low-{level}")).await, Status::Forbidden);
        }
    }

    #[rocket::async_test]
    async fn billing_off_opens_the_link_to_every_server() {
        overwrite_config(|config| {
            config.features.billing.enabled = false;
        })
        .await;
        let h = TestHarness::new().await;
        let (server, token, _) = server_at(&h, 0).await;
        assert_eq!(put(&h, &token, &server.id, "free-link").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn level_three_owner_claims_and_it_resolves() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "Arab-Gamers").await, Status::Ok);
        let stored = h.db.fetch_server(&server.id).await.unwrap().vanity_invite.unwrap();
        assert_eq!(stored.code, "arab-gamers");
        match h.db.fetch_invite("arab-gamers").await.unwrap() {
            revolt_database::Invite::Server { server: id, vanity, max_uses, expires, .. } => {
                assert_eq!(id, server.id);
                assert!(vanity);
                assert!(max_uses.is_none() && expires.is_none());
            }
            _ => panic!("not a server invite"),
        }
        assert_eq!(fetch_invite(&h, "arab-gamers").await, Status::Ok);
        // vanity_lookup_is_case_insensitive
        assert_eq!(fetch_invite(&h, "ARAB-Gamers").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn member_without_manage_server_refused() {
        let h = harness().await;
        let (server, _, _) = server_at(&h, 3).await;
        let (_, session, member) = h.new_user().await;
        Member::create(&h.db, &server, &member, None).await.unwrap();
        assert_eq!(put(&h, &session.token, &server.id, "not-mine").await, Status::Forbidden);
    }

    #[rocket::async_test]
    async fn duplicate_is_refused() {
        let h = harness().await;
        let (first, first_token, _) = server_at(&h, 3).await;
        let (second, second_token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &first_token, &first.id, "same-name").await, Status::Ok);
        assert_eq!(put(&h, &second_token, &second.id, "same-name").await, Status::Conflict);
        // two servers racing for one name: exactly one wins
        let (a, ta, _) = server_at(&h, 3).await;
        let (b, tb, _) = server_at(&h, 3).await;
        let (ra, rb) = tokio::join!(put(&h, &ta, &a.id, "race-name"), put(&h, &tb, &b.id, "race-name"));
        assert_eq!([ra, rb].iter().filter(|s| **s == Status::Ok).count(), 1);
    }

    #[rocket::async_test]
    async fn reserved_and_malformed_refused() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        for bad in ["api", "a", "-x", "x--y", "discover"] {
            assert_eq!(put(&h, &token, &server.id, bad).await, Status::BadRequest, "{bad}");
        }
    }

    #[rocket::async_test]
    async fn vanity_cannot_take_existing_invite() {
        let h = harness().await;
        let (server, token, owner) = server_at(&h, 3).await;
        let channel = h.db.fetch_channel(&server.channels[0]).await.unwrap();
        let invite = revolt_database::Invite::create_channel_invite(&h.db, &owner, &channel, None, None)
            .await
            .unwrap();
        let taken = invite.code().to_lowercase();
        // random codes mix cases; a lowercase twin is still refused when it exists
        if invite.code() == taken {
            assert_eq!(put(&h, &token, &server.id, &taken).await, Status::Conflict);
        }
        assert_eq!(fetch_invite(&h, invite.code()).await, Status::Ok);
    }

    #[rocket::async_test]
    async fn changing_vanity_frees_old_name() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        let (other, other_token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "first-name").await, Status::Ok);
        assert_eq!(put(&h, &token, &server.id, "second-name").await, Status::Ok);
        assert_eq!(fetch_invite(&h, "first-name").await, Status::NotFound);
        assert_eq!(fetch_invite(&h, "second-name").await, Status::Ok);
        assert_eq!(put(&h, &other_token, &other.id, "first-name").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn remove_frees_name() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "short-lived").await, Status::Ok);
        let status = h
            .client
            .delete(format!("/servers/{}/vanity", server.id))
            .header(Header::new("x-session-token", token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::NoContent);
        assert!(h.db.fetch_server(&server.id).await.unwrap().vanity_invite.is_none());
        assert_eq!(fetch_invite(&h, "short-lived").await, Status::NotFound);
    }

    #[rocket::async_test]
    async fn vanity_is_not_deleted_as_plain_invite() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "stay-put").await, Status::Ok);
        let status = h
            .client
            .delete("/invites/stay-put")
            .header(Header::new("x-session-token", token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::BadRequest);
        assert_eq!(fetch_invite(&h, "stay-put").await, Status::Ok);
    }

    async fn join(h: &TestHarness, token: &str, code: &str) -> Status {
        h.client
            .post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", token.to_string()))
            .dispatch()
            .await
            .status()
    }

    /// Put a server at a level, its link marked below the level `days` ago
    async fn drop_level(h: &TestHarness, server_id: &str, level: u32, days: Option<i64>) {
        let mut server = h.db.fetch_server(server_id).await.unwrap();
        let mut vanity = server.vanity_invite.clone().unwrap();
        vanity.below_level_since =
            days.map(|d| Timestamp::now_utc() - iso8601_timestamp::Duration::days(d));
        server
            .update(
                &h.db,
                PartialServer {
                    supercharge: Some(v0::ServerSupercharge { count: level * 5, level }),
                    vanity_invite: Some(vanity),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    }

    #[rocket::async_test]
    async fn paused_link_refuses_fetch_and_join() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "paused-link").await, Status::Ok);
        let (_, visitor, _) = h.new_user().await;

        // 3 days below the level: still in the 7-day grace
        drop_level(&h, &server.id, 1, Some(3)).await;
        assert_eq!(fetch_invite(&h, "paused-link").await, Status::Ok);

        // 8 days below: paused for fetch and join
        drop_level(&h, &server.id, 1, Some(8)).await;
        assert_eq!(fetch_invite(&h, "paused-link").await, Status::Gone);
        assert_eq!(join(&h, &visitor.token, "paused-link").await, Status::Gone);

        // level regained: works again
        drop_level(&h, &server.id, 3, Some(8)).await;
        assert_eq!(fetch_invite(&h, "paused-link").await, Status::Ok);
        assert_eq!(join(&h, &visitor.token, "paused-link").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn vanity_join_respects_bans() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "no-banned").await, Status::Ok);
        let (_, banned_session, banned) = h.new_user().await;
        let status = h
            .client
            .put(format!("/servers/{}/bans/{}", server.id, banned.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.clone()))
            .body(json!({}).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Ok);
        assert_eq!(join(&h, &banned_session.token, "no-banned").await, Status::Forbidden);
    }

    #[rocket::async_test]
    async fn vanity_follows_deleted_channel() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        let second = h
            .client
            .post(format!("/servers/{}/channels", server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.clone()))
            .body(json!({ "type": "Text", "name": "second" }).to_string())
            .dispatch()
            .await
            .into_json::<serde_json::Value>()
            .await
            .unwrap()["_id"]
            .as_str()
            .unwrap()
            .to_string();
        let first = server.channels[0].clone();
        let claim = h
            .client
            .put(format!("/servers/{}/vanity", server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.clone()))
            .body(json!({ "code": "moving-link", "channel": first }).to_string())
            .dispatch()
            .await
            .status();
        assert_eq!(claim, Status::Ok);

        let deleted = h
            .client
            .delete(format!("/channels/{first}"))
            .header(Header::new("x-session-token", token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(deleted, Status::NoContent);

        let stored = h.db.fetch_server(&server.id).await.unwrap().vanity_invite.unwrap();
        assert_eq!(stored.channel, second);
        assert_eq!(fetch_invite(&h, "moving-link").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn release_frees_name_after_90_days() {
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        let (other, other_token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "keep-me").await, Status::Ok);

        // the level sync notices the drop, the name stays reserved
        drop_level(&h, &server.id, 1, None).await;
        let now = Timestamp::now_utc();
        revolt_database::sync_vanity_invites(&h.db, now).await.unwrap();
        let marked = h.db.fetch_server(&server.id).await.unwrap().vanity_invite.unwrap();
        assert!(marked.below_level_since.is_some());
        assert_eq!(put(&h, &other_token, &other.id, "keep-me").await, Status::Conflict);

        // 91 days later the reservation ends
        revolt_database::sync_vanity_invites(&h.db, now + iso8601_timestamp::Duration::days(91))
            .await
            .unwrap();
        assert!(h.db.fetch_server(&server.id).await.unwrap().vanity_invite.is_none());
        assert_eq!(put(&h, &other_token, &other.id, "keep-me").await, Status::Ok);
    }
}
