//! Bots in group DMs: only the group's owner chooses them.

use std::collections::HashSet;

use crate::{rocket, util::test::TestHarness};
use revolt_database::{Bot, Channel, PartialChannel, Session};
use revolt_models::v0;
use revolt_permissions::ChannelPermission;
use rocket::http::{ContentType, Header, Status};

async fn add_bot(
    harness: &TestHarness,
    session: &Session,
    bot: &Bot,
    group: &str,
) -> (Status, String) {
    let response = harness
        .client
        .post(format!("/bots/{}/invite", bot.id))
        .header(ContentType::JSON)
        .header(Header::new("x-session-token", session.token.clone()))
        .body(json!(v0::InviteBotDestination::Group { group: group.to_string() }).to_string())
        .dispatch()
        .await;
    let status = response.status();
    (status, response.into_string().await.unwrap_or_default())
}

#[rocket::async_test]
async fn only_the_group_owner_adds_a_bot() {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (bot, _) = harness.new_bot(&owner).await;
    // a public bot, so the member could pick it too
    harness
        .db
        .update_bot(
            &bot.id,
            &revolt_database::PartialBot {
                public: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let group = Channel::create_group(
        &harness.db,
        v0::DataCreateGroup {
            name: TestHarness::rand_string(),
            users: HashSet::from([member.id.clone()]),
            ..Default::default()
        },
        owner.id.clone(),
    )
    .await
    .unwrap();
    // members may invite people, but not bots
    harness
        .db
        .update_channel(
            group.id(),
            &PartialChannel {
                permissions: Some(ChannelPermission::InviteOthers as i64),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let (status, body) = add_bot(&harness, &member_session, &bot, group.id()).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(body.contains("NotOwner"), "{body}");
    let channel = harness.db.fetch_channel(group.id()).await.unwrap();
    let Channel::Group { recipients, .. } = &channel else {
        unreachable!()
    };
    assert!(!recipients.contains(&bot.id));

    let (status, body) = add_bot(&harness, &owner_session, &bot, group.id()).await;
    assert_eq!(status, Status::NoContent, "{body}");

    // everyone sees who added it
    let messages = harness
        .client
        .get(format!("/channels/{}/messages", group.id()))
        .header(Header::new("x-session-token", member_session.token.clone()))
        .dispatch()
        .await
        .into_string()
        .await
        .unwrap();
    assert!(messages.contains("\"user_added\""), "{messages}");
    assert!(messages.contains(&bot.id) && messages.contains(&owner.id), "{messages}");
}
