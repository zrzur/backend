use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Database, FieldsServer, Invite, PartialServer, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// # Fetch Custom Invite Link Eligibility
/// Whether this server may hold a custom invite link (zrzur.cc/<code>), with
/// its members and recent activity against the rule, and when its current
/// link ends if it is counting down. Needs Manage Server.
#[openapi(tag = "Server Information")]
#[get("/<server_id>/vanity/eligibility")]
pub async fn eligibility(
    db: &State<Database>,
    user: User,
    server_id: Reference<'_>,
) -> Result<Json<v0::VanityEligibility>> {
    let server = server_id.as_server(db).await?;
    require_manage_server(db, &user, &server).await?;
    Ok(Json(revolt_database::vanity_eligibility(db, &server, false).await?))
}

/// # Claim Custom Invite Link
/// Claim or change this server's custom invite link (zrzur.cc/<code>).
/// Needs Manage Server, and the server must be official, a Zrzur partner,
/// or have enough members and recent activity (or, with billing on, the
/// Wings level that unlocks it). Otherwise `VanityNotEligible` says what
/// is missing.
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
    let check = revolt_database::vanity_eligibility(db, &server, false).await?;
    if !check.eligible {
        return Err(revolt_database::vanity_not_eligible(&check));
    }

    let code = normalise_vanity(&data.code, is_official(&server))?;
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
        via: check.via,
        notice_sent_at: None,
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

/// The server is run by the Zrzur team (flag bit 1). No route sets it: the
/// admin console can give the verified and partner marks only, so only the
/// project itself can make a server official.
fn is_official(server: &Server) -> bool {
    revolt_database::server_is_official(server)
}

/// Lowercase a requested name and check it against the rules. The names
/// kept for Zrzur (zrzur.cc/zrzur) are kept for its official server: it may
/// take them, nobody else can.
pub fn normalise_vanity(code: &str, official: bool) -> Result<String> {
    let code = code.trim().to_ascii_lowercase();
    let valid = (3..=32).contains(&code.len())
        && code.split('-').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if !valid || (!official && super::vanity_reserved::is_reserved(&code)) {
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
        assert_eq!(normalise_vanity("Arab-Gamers", false).unwrap(), "arab-gamers");
        assert_eq!(normalise_vanity("  team42 ", false).unwrap(), "team42");
        for bad in ["ab", "-team", "team-", "te--am", "team_42", "تيم", "api", "invite", "zrzur", &"a".repeat(33)] {
            assert!(normalise_vanity(bad, false).is_err(), "{bad}");
        }
        // the official server may take a kept name, never a malformed one
        assert_eq!(normalise_vanity("Zrzur", true).unwrap(), "zrzur");
        for bad in ["ab", "-team", "te--am", "team_42", "تيم"] {
            assert!(normalise_vanity(bad, true).is_err(), "{bad}");
        }
    }

    #[rocket::async_test]
    async fn only_the_official_server_takes_the_brand_name() {
        let h = harness().await;
        // a verified community is not the official server
        let (verified, verified_token, _) = server_at(&h, 3).await;
        let mut verified = h.db.fetch_server(&verified.id).await.unwrap();
        verified
            .update(&h.db, PartialServer { flags: Some(2), ..Default::default() }, vec![])
            .await
            .unwrap();
        assert_eq!(put(&h, &verified_token, &verified.id, "zrzur").await, Status::BadRequest);

        let (official, official_token, _) = server_at(&h, 3).await;
        let mut official = h.db.fetch_server(&official.id).await.unwrap();
        official
            .update(&h.db, PartialServer { flags: Some(1), ..Default::default() }, vec![])
            .await
            .unwrap();
        assert_eq!(put(&h, &official_token, &official.id, "zrzur").await, Status::Ok);
        assert_eq!(fetch_invite(&h, "zrzur").await, Status::Ok);
        // and the name is then taken like any other
        assert_eq!(put(&h, &verified_token, &verified.id, "zrzur").await, Status::BadRequest);
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
    async fn billing_off_no_longer_opens_the_link_to_every_server() {
        overwrite_config(|config| {
            config.features.billing.enabled = false;
        })
        .await;
        let h = TestHarness::new().await;
        let (server, token, _) = server_at(&h, 0).await;
        assert_eq!(put(&h, &token, &server.id, "free-link").await, Status::Forbidden);
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
        revolt_database::sync_vanity_invites(&h.db, now, true).await.unwrap();
        let marked = h.db.fetch_server(&server.id).await.unwrap().vanity_invite.unwrap();
        assert!(marked.below_level_since.is_some());
        assert_eq!(put(&h, &other_token, &other.id, "keep-me").await, Status::Conflict);

        // 91 days later the reservation ends
        revolt_database::sync_vanity_invites(&h.db, now + iso8601_timestamp::Duration::days(91), true)
            .await
            .unwrap();
        assert!(h.db.fetch_server(&server.id).await.unwrap().vanity_invite.is_none());
        assert_eq!(put(&h, &other_token, &other.id, "keep-me").await, Status::Ok);
    }

    // ---- the member and activity rule (owner, 2026-11-21) ----
    // Small numbers in the config stand in for 500 members, 100 messages
    // and 20 authors; the defaults are checked in `the_rule_defaults`.

    /// Billing off, so only the rule counts: 3 members, 4 messages from 2 people
    async fn rule_harness() -> TestHarness {
        overwrite_config(|config| {
            config.features.billing.enabled = false;
            config.features.vanity.min_members = 3;
            config.features.vanity.active_messages = 4;
            config.features.vanity.active_authors = 2;
        })
        .await;
        TestHarness::new().await
    }

    /// A server with its owner and `others` more members
    async fn community(h: &TestHarness, others: usize) -> (Server, String, User, Vec<User>) {
        let (_, session, owner) = h.new_user().await;
        let (server, channels) = h.new_server(&owner).await;
        Member::create(&h.db, &server, &owner, Some(channels)).await.unwrap();
        let mut people = Vec::new();
        for _ in 0..others {
            let (_, _, person) = h.new_user().await;
            Member::create(&h.db, &server, &person, None).await.unwrap();
            people.push(person);
        }
        let server = h.db.fetch_server(&server.id).await.unwrap();
        (server, session.token, owner, people)
    }

    /// `n` plain messages by `author` in the server's first channel
    async fn talk(h: &TestHarness, server: &Server, author: &str, n: usize) {
        for _ in 0..n {
            h.db.insert_message(&revolt_database::Message {
                id: ulid::Ulid::new().to_string(),
                channel: server.channels[0].clone(),
                author: author.to_string(),
                content: Some("hello".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        }
    }

    async fn put_body(h: &TestHarness, token: &str, server: &str, code: &str) -> (Status, serde_json::Value) {
        let response = h
            .client
            .put(format!("/servers/{server}/vanity"))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", token.to_string()))
            .body(json!({ "code": code }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or_default())
    }

    async fn eligibility(h: &TestHarness, token: &str, server: &str) -> serde_json::Value {
        h.client
            .get(format!("/servers/{server}/vanity/eligibility"))
            .header(Header::new("x-session-token", token.to_string()))
            .dispatch()
            .await
            .into_json()
            .await
            .unwrap()
    }

    async fn set_flags(h: &TestHarness, server_id: &str, flags: i32) {
        let mut server = h.db.fetch_server(server_id).await.unwrap();
        server
            .update(&h.db, PartialServer { flags: Some(flags), ..Default::default() }, vec![])
            .await
            .unwrap();
    }

    /// A link held from before the rule: stored directly, no claim
    async fn legacy_link(h: &TestHarness, server_id: &str, code: &str) {
        let mut server = h.db.fetch_server(server_id).await.unwrap();
        h.db.insert_invite(&revolt_database::Invite::Server {
            code: code.to_string(),
            server: server.id.clone(),
            creator: server.owner.clone(),
            channel: server.channels[0].clone(),
            max_uses: None,
            uses: 0,
            expires: None,
            vanity: true,
        })
        .await
        .unwrap();
        let vanity = v0::ServerVanityInvite {
            code: code.to_string(),
            channel: server.channels[0].clone(),
            claimed_at: Timestamp::now_utc(),
            below_level_since: None,
            via: None,
            notice_sent_at: None,
        };
        server
            .update(&h.db, PartialServer { vanity_invite: Some(vanity), ..Default::default() }, vec![])
            .await
            .unwrap();
    }

    async fn stored_vanity(h: &TestHarness, server_id: &str) -> Option<v0::ServerVanityInvite> {
        h.db.fetch_server(server_id).await.unwrap().vanity_invite
    }

    /// What the official Zrzur account wrote to a person
    async fn official_dms(h: &TestHarness, user_id: &str) -> Vec<String> {
        let Ok(channel) = h
            .db
            .find_direct_message_channel(revolt_database::OfficialAccount::Zrzur.id(), user_id)
            .await
        else {
            return vec![];
        };
        h.db.fetch_messages(revolt_database::MessageQuery {
            limit: Some(50),
            filter: revolt_database::MessageFilter {
                channel: Some(channel.id().to_string()),
                ..Default::default()
            },
            time_period: revolt_database::MessageTimePeriod::Absolute {
                before: None,
                after: None,
                sort: None,
            },
        })
        .await
        .unwrap()
        .into_iter()
        .filter_map(|m| m.content)
        .collect()
    }

    /// The daily crond pass, as `crond::tasks::vanity_links::run_once` runs it
    async fn daily(h: &TestHarness, now: Timestamp) -> (revolt_database::VanitySyncReport, usize) {
        let report = revolt_database::sync_vanity_invites(&h.db, now, true).await.unwrap();
        let notices = revolt_database::send_vanity_notices(&h.db, None, now).await.unwrap();
        (report, notices)
    }

    #[test]
    fn the_rule_defaults() {
        let defaults = revolt_config::FeaturesVanity::default();
        assert_eq!(defaults.min_members, 500);
        assert_eq!(defaults.active_messages, 100);
        assert_eq!(defaults.active_authors, 20);
        assert_eq!(defaults.active_days, 30);
        assert_eq!(defaults.notice_days, 30);
    }

    #[rocket::async_test]
    async fn claim_refused_below_the_member_count() {
        let h = rule_harness().await;
        // 2 members, active enough
        let (server, token, owner, people) = community(&h, 1).await;
        talk(&h, &server, &owner.id, 2).await;
        talk(&h, &server, &people[0].id, 2).await;
        let (status, body) = put_body(&h, &token, &server.id, "small-one").await;
        assert_eq!(status, Status::Forbidden, "{body}");
        assert_eq!(body["type"], "VanityNotEligible");
        assert_eq!(body["missing"], "Members");
        assert_eq!(body["members"], 2);
        assert_eq!(body["min_members"], 3);
        assert_eq!(body["messages"], 4);
        assert_eq!(body["authors"], 2);
        assert_eq!(fetch_invite(&h, "small-one").await, Status::NotFound);
        assert!(stored_vanity(&h, &server.id).await.is_none());

        // no members and no activity: both are missing
        let (lonely, lonely_token, _, _) = community(&h, 0).await;
        let (status, body) = put_body(&h, &lonely_token, &lonely.id, "lonely-one").await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(body["missing"], "Both");
    }

    #[rocket::async_test]
    async fn claim_refused_when_inactive() {
        let h = rule_harness().await;
        let (server, token, owner, people) = community(&h, 2).await;
        // enough messages from one person is not an active community
        talk(&h, &server, &owner.id, 10).await;
        let (status, body) = put_body(&h, &token, &server.id, "quiet-one").await;
        assert_eq!(status, Status::Forbidden, "{body}");
        assert_eq!(body["missing"], "Activity");
        // two people, too few messages
        let (other, other_token, other_owner, other_people) = community(&h, 2).await;
        talk(&h, &other, &other_owner.id, 1).await;
        talk(&h, &other, &other_people[0].id, 1).await;
        let (status, body) = put_body(&h, &other_token, &other.id, "quiet-two").await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(body["missing"], "Activity");
        let _ = people;
    }

    #[rocket::async_test]
    async fn official_and_partner_servers_claim_without_the_rule() {
        let h = rule_harness().await;
        let (official, official_token, _, _) = community(&h, 0).await;
        set_flags(&h, &official.id, 1).await;
        assert_eq!(put(&h, &official_token, &official.id, "our-own").await, Status::Ok);
        assert_eq!(stored_vanity(&h, &official.id).await.unwrap().via, Some(v0::VanityPath::Official));

        let (partner, partner_token, _, _) = community(&h, 0).await;
        // verified and partner together: the partner bit decides
        set_flags(&h, &partner.id, 2 | 4).await;
        assert_eq!(put(&h, &partner_token, &partner.id, "our-partner").await, Status::Ok);
        assert_eq!(stored_vanity(&h, &partner.id).await.unwrap().via, Some(v0::VanityPath::Partner));
        assert_eq!(fetch_invite(&h, "our-partner").await, Status::Ok);

        // the verified mark alone is not enough
        let (verified, verified_token, _, _) = community(&h, 0).await;
        set_flags(&h, &verified.id, 2).await;
        assert_eq!(put(&h, &verified_token, &verified.id, "only-verified").await, Status::Forbidden);
    }

    #[rocket::async_test]
    async fn big_active_server_claims() {
        let h = rule_harness().await;
        let (server, token, owner, people) = community(&h, 2).await;
        talk(&h, &server, &owner.id, 2).await;
        talk(&h, &server, &people[1].id, 2).await;
        let check = eligibility(&h, &token, &server.id).await;
        assert_eq!(check["eligible"], true, "{check}");
        assert_eq!(check["via"], "Community");
        assert_eq!(check["members"], 3);
        assert_eq!(put(&h, &token, &server.id, "big-and-busy").await, Status::Ok);
        assert_eq!(stored_vanity(&h, &server.id).await.unwrap().via, Some(v0::VanityPath::Community));
        assert_eq!(fetch_invite(&h, "big-and-busy").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn eligibility_needs_manage_server() {
        let h = rule_harness().await;
        let (server, _, _, people) = community(&h, 1).await;
        let (_, session, outsider) = h.new_user().await;
        Member::create(&h.db, &server, &outsider, None).await.unwrap();
        let status = h
            .client
            .get(format!("/servers/{}/vanity/eligibility", server.id))
            .header(Header::new("x-session-token", session.token.clone()))
            .dispatch()
            .await
            .status();
        assert_eq!(status, Status::Forbidden);
        let _ = people;
    }

    #[rocket::async_test]
    async fn activity_counts_people_only() {
        let h = rule_harness().await;
        let (server, _, owner, people) = community(&h, 1).await;
        let (_, bot_user) = h.new_bot(&owner).await;
        Member::create(&h.db, &server, &bot_user, None).await.unwrap();
        talk(&h, &server, &owner.id, 3).await;
        talk(&h, &server, &people[0].id, 2).await;
        // none of these count
        talk(&h, &server, &bot_user.id, 5).await;
        let channel = server.channels[0].clone();
        let skipped = [
            revolt_database::Message {
                id: ulid::Ulid::new().to_string(),
                channel: channel.clone(),
                author: owner.id.clone(),
                system: Some(revolt_database::SystemMessage::Text { content: "joined".into() }),
                ..Default::default()
            },
            revolt_database::Message {
                id: ulid::Ulid::new().to_string(),
                channel: channel.clone(),
                author: people[0].id.clone(),
                webhook: Some(v0::MessageWebhook { name: "hook".into(), avatar: None }),
                ..Default::default()
            },
            // 40 days old: before the window
            revolt_database::Message {
                id: ulid::Ulid::from_datetime(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400),
                )
                .to_string(),
                channel: channel.clone(),
                author: people[0].id.clone(),
                ..Default::default()
            },
        ];
        for message in &skipped {
            h.db.insert_message(message).await.unwrap();
        }
        // another server's messages
        let (elsewhere, _, elsewhere_owner, _) = community(&h, 0).await;
        talk(&h, &elsewhere, &elsewhere_owner.id, 7).await;

        let activity = revolt_database::server_activity(&h.db, &server, true).await.unwrap();
        assert_eq!(activity, revolt_database::MessageActivity { messages: 5, authors: 2 });
        // an empty server counts nothing
        let (empty, _, _, _) = community(&h, 0).await;
        let none = revolt_database::server_activity(&h.db, &empty, true).await.unwrap();
        assert_eq!(none, revolt_database::MessageActivity::default());
        // a recent count is reused until it is asked for fresh
        talk(&h, &server, &owner.id, 1).await;
        let cached = revolt_database::server_activity(&h.db, &server, false).await.unwrap();
        assert_eq!(cached.messages, 5);
        let fresh = revolt_database::server_activity(&h.db, &server, true).await.unwrap();
        assert_eq!(fresh.messages, 6);
    }

    #[rocket::async_test]
    async fn daily_job_marks_notifies_keeps_30_days_then_releases() {
        let h = rule_harness().await;
        let (server, token, owner, _) = community(&h, 0).await;
        legacy_link(&h, &server.id, "old-small").await;
        let now = Timestamp::now_utc();

        let (report, notices) = daily(&h, now).await;
        assert_eq!(report.marked, 1);
        assert_eq!(notices, 1);
        let marked = stored_vanity(&h, &server.id).await.unwrap();
        assert!(marked.below_level_since.is_some());
        assert!(marked.notice_sent_at.is_some());
        let dms = official_dms(&h, &owner.id).await;
        assert_eq!(dms.len(), 1, "{dms:?}");
        assert!(dms[0].contains("zrzur.cc/old-small"), "{}", dms[0]);
        let ends = now + iso8601_timestamp::Duration::days(30);
        let end_date: String = ends.format().to_string().chars().take(10).collect();
        assert!(dms[0].contains(&end_date), "{}", dms[0]);

        // the app sees the end date
        let check = eligibility(&h, &token, &server.id).await;
        assert_eq!(check["eligible"], false);
        assert!(check["link_ends_at"].is_string(), "{check}");

        // told once, not every day
        let (report, notices) = daily(&h, now + iso8601_timestamp::Duration::days(1)).await;
        assert_eq!((report.marked, notices), (0, 0));
        assert_eq!(official_dms(&h, &owner.id).await.len(), 1);

        // the link keeps working the whole 30 days
        assert_eq!(fetch_invite(&h, "old-small").await, Status::Ok);
        let (report, _) = daily(&h, now + iso8601_timestamp::Duration::days(29)).await;
        assert_eq!(report.released, 0);
        let mut moved = stored_vanity(&h, &server.id).await.unwrap();
        moved.below_level_since = Some(Timestamp::now_utc() - iso8601_timestamp::Duration::days(29));
        let mut stored = h.db.fetch_server(&server.id).await.unwrap();
        stored
            .update(&h.db, PartialServer { vanity_invite: Some(moved.clone()), ..Default::default() }, vec![])
            .await
            .unwrap();
        assert_eq!(fetch_invite(&h, "old-small").await, Status::Ok);
        // a day past the end, before crond runs: no longer opens
        moved.below_level_since = Some(Timestamp::now_utc() - iso8601_timestamp::Duration::days(31));
        stored
            .update(&h.db, PartialServer { vanity_invite: Some(moved), ..Default::default() }, vec![])
            .await
            .unwrap();
        assert_eq!(fetch_invite(&h, "old-small").await, Status::Gone);

        // then the name goes back, with no 90-day reserve
        let (report, _) = daily(&h, Timestamp::now_utc()).await;
        assert_eq!(report.released, 1);
        assert!(stored_vanity(&h, &server.id).await.is_none());
        assert_eq!(fetch_invite(&h, "old-small").await, Status::NotFound);
        let (taker, taker_token, _, _) = community(&h, 0).await;
        set_flags(&h, &taker.id, 1).await;
        assert_eq!(put(&h, &taker_token, &taker.id, "old-small").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn server_that_recovers_is_unmarked() {
        let h = rule_harness().await;
        let (server, _, owner, _) = community(&h, 0).await;
        legacy_link(&h, &server.id, "comes-back").await;
        let now = Timestamp::now_utc();
        let (report, _) = daily(&h, now).await;
        assert_eq!(report.marked, 1);

        // two more members and an active week
        for _ in 0..2 {
            let (_, _, person) = h.new_user().await;
            Member::create(&h.db, &server, &person, None).await.unwrap();
            talk(&h, &server, &person.id, 2).await;
        }
        talk(&h, &server, &owner.id, 1).await;
        let (report, notices) = daily(&h, now + iso8601_timestamp::Duration::days(3)).await;
        assert_eq!((report.recovered, notices), (1, 0));
        let vanity = stored_vanity(&h, &server.id).await.unwrap();
        assert_eq!(vanity.below_level_since, None);
        assert_eq!(vanity.notice_sent_at, None);
        assert_eq!(vanity.via, Some(v0::VanityPath::Community));
        // long after the old end date, still there
        let (report, _) = daily(&h, now + iso8601_timestamp::Duration::days(40)).await;
        assert_eq!(report.released, 0);
        assert_eq!(fetch_invite(&h, "comes-back").await, Status::Ok);
    }

    #[rocket::async_test]
    async fn official_and_partner_links_are_never_marked() {
        let h = rule_harness().await;
        let (official, _, official_owner, _) = community(&h, 0).await;
        let (partner, _, partner_owner, _) = community(&h, 0).await;
        legacy_link(&h, &official.id, "team-home").await;
        legacy_link(&h, &partner.id, "partner-home").await;
        set_flags(&h, &official.id, 1).await;
        set_flags(&h, &partner.id, 4).await;
        for days in [0, 31, 120] {
            let (report, notices) = daily(&h, Timestamp::now_utc() + iso8601_timestamp::Duration::days(days)).await;
            assert_eq!((report.marked, report.released, notices), (0, 0, 0), "day {days}");
        }
        assert_eq!(stored_vanity(&h, &official.id).await.unwrap().below_level_since, None);
        assert_eq!(stored_vanity(&h, &partner.id).await.unwrap().via, Some(v0::VanityPath::Partner));
        assert!(official_dms(&h, &official_owner.id).await.is_empty());
        assert!(official_dms(&h, &partner_owner.id).await.is_empty());
    }

    #[rocket::async_test]
    async fn wings_level_still_unlocks_the_link_with_billing_on() {
        // the Wings path stays beside the rule: a level-3 server with one member
        let h = harness().await;
        let (server, token, _) = server_at(&h, 3).await;
        assert_eq!(put(&h, &token, &server.id, "wings-only").await, Status::Ok);
        assert_eq!(stored_vanity(&h, &server.id).await.unwrap().via, Some(v0::VanityPath::Wings));
        // and below the level, the small server is refused with the rule's reasons
        let (low, low_token, _) = server_at(&h, 1).await;
        let (status, body) = put_body(&h, &low_token, &low.id, "wings-low").await;
        assert_eq!(status, Status::Forbidden);
        assert_eq!(body["type"], "VanityNotEligible");
    }
}
