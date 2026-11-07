//! Joining with an invite spends a use only when the join happens: a refused
//! join (ban, already a member) leaves it, and two people can't both take
//! the last use.

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Invite, ServerBan};
use rocket::http::{Header, Status};

/// A 1-use invite to a new server's first channel
async fn one_use_invite(harness: &TestHarness) -> (revolt_database::Server, String) {
    let (_, _, owner) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let invite = Invite::create_channel_invite(&harness.db, &owner, &channels[0], Some(1), None)
        .await
        .unwrap();
    (server, invite.code().to_string())
}

async fn uses(harness: &TestHarness, code: &str) -> u64 {
    match harness.db.fetch_invite(code).await.unwrap() {
        Invite::Server { uses, .. } | Invite::Group { uses, .. } => uses,
    }
}

#[rocket::async_test]
async fn a_banned_person_spends_no_use() {
    let harness = TestHarness::new().await;
    let (server, code) = one_use_invite(&harness).await;
    let (_, session, banned) = harness.new_user().await;
    harness
        .db
        .insert_ban(&ServerBan {
            id: revolt_database::MemberCompositeKey {
                server: server.id.clone(),
                user: banned.id.clone(),
            },
            reason: None,
        })
        .await
        .unwrap();

    let response = harness
        .client
        .post(format!("/invites/{code}"))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);
    assert!(response.into_string().await.unwrap().contains("Banned"));
    assert_eq!(uses(&harness, &code).await, 0);

    // the use is still there for someone else
    let (_, guest, _) = harness.new_user().await;
    let response = harness
        .client
        .post(format!("/invites/{code}"))
        .header(Header::new("x-session-token", guest.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
}

#[rocket::async_test]
async fn a_member_joining_again_spends_no_use() {
    let harness = TestHarness::new().await;
    let (server, code) = one_use_invite(&harness).await;
    let (_, session, member) = harness.new_user().await;
    revolt_database::Member::create(&harness.db, &server, &member, None)
        .await
        .unwrap();

    let response = harness
        .client
        .post(format!("/invites/{code}"))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_ne!(response.status(), Status::Ok);
    assert_eq!(uses(&harness, &code).await, 0);
}

#[rocket::async_test]
async fn two_people_race_for_the_last_use() {
    let harness = TestHarness::new().await;
    let (_, code) = one_use_invite(&harness).await;
    let (_, first, _) = harness.new_user().await;
    let (_, second, _) = harness.new_user().await;

    let join = |token: String| {
        harness
            .client
            .post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", token))
            .dispatch()
    };
    let (a, b) = futures::join!(join(first.token.clone()), join(second.token.clone()));

    let ok = [a.status(), b.status()]
        .iter()
        .filter(|status| **status == Status::Ok)
        .count();
    assert_eq!(ok, 1);
    // one invite with no use left: it can no longer be fetched at all
    assert!(harness.db.fetch_invite(&code).await.is_err());
}

