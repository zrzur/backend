//! Forwarding a message (`POST /channels/{target}/messages/forward`): who may
//! forward what, and what the copy keeps.
use revolt_config::overwrite_config;
use revolt_database::{Channel, File, Member, Metadata, PartialChannel, Server, Session, User};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField};
use revolt_result::{Error, ErrorType};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::TestHarness;

async fn forwarding_on() {
    overwrite_config(|config| config.features.message_forwarding_enabled = true).await;
}

/// A server with an owner and one plain member, the general channel and a
/// second channel
struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    server: Server,
    general: Channel,
    other: Channel,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let other = harness.new_channel(&server).await;
    Member::create(&harness.db, &server, &member, None)
        .await
        .expect("member");
    let server = harness.db.fetch_server(&server.id).await.unwrap();
    Setup {
        harness,
        owner,
        owner_session,
        member,
        member_session,
        server,
        general: channels[0].clone(),
        other,
    }
}

fn token(session: &Session) -> Header<'static> {
    Header::new("x-session-token", session.token.clone())
}

impl Setup {
    async fn send(&self, session: &Session, channel: &str, body: Value) -> v0::Message {
        let response = self
            .harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(ContentType::JSON)
            .header(token(session))
            .body(body.to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        response.into_json().await.unwrap()
    }

    async fn forward(
        &self,
        session: &Session,
        target: &str,
        body: Value,
    ) -> (Status, Option<v0::Message>, Option<ErrorType>) {
        let response = self
            .harness
            .client
            .post(format!("/channels/{target}/messages/forward"))
            .header(ContentType::JSON)
            .header(token(session))
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        let text = response.into_string().await.unwrap_or_default();
        if status == Status::Ok {
            (status, serde_json::from_str(&text).ok(), None)
        } else {
            let error: Option<Error> = serde_json::from_str(&text).ok();
            (status, None, error.map(|e| e.error_type))
        }
    }

    /// Change a channel's default permissions
    async fn restrict(&self, channel: &Channel, deny: ChannelPermission) -> Channel {
        let mut channel = self.harness.db.fetch_channel(channel.id()).await.unwrap();
        channel
            .update(
                &self.harness.db,
                PartialChannel {
                    default_permissions: Some(OverrideField {
                        a: 0,
                        d: deny as i64,
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
        channel
    }

    async fn upload(&self, uploader: &User, hash: Option<&str>) -> String {
        let id = ulid::Ulid::new().to_string();
        self.harness
            .db
            .insert_attachment(&File {
                id: id.clone(),
                tag: "attachments".to_string(),
                filename: "a.png".to_string(),
                hash: hash.map(str::to_string),
                uploaded_at: Some(iso8601_timestamp::Timestamp::now_utc()),
                uploader_id: Some(uploader.id.clone()),
                used_for: None,
                deleted: None,
                reported: None,
                purged: None,
                metadata: Metadata::Image {
                    width: 8,
                    height: 8,
                    thumbhash: None,
                    animated: None,
                },
                content_type: "image/png".to_string(),
                size: 100,
                message_id: None,
                user_id: None,
                server_id: None,
                object_id: None,
                forwarded_from: None,
            })
            .await
            .unwrap();
        id
    }
}

#[rocket::async_test]
async fn refused_while_the_feature_is_off() {
    let s = setup().await;
    let original = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "hello" }))
        .await;

    let (status, _, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::BadRequest, "{error:?}");
    assert!(matches!(error, Some(ErrorType::FeatureDisabled { .. })), "{error:?}");
}

#[rocket::async_test]
async fn round_trip_keeps_the_text_and_records_the_origin() {
    forwarding_on().await;
    let s = setup().await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": format!("look at this <@{}>", s.member.id) }),
        )
        .await;
    assert!(original.mentions.is_some());

    let (status, copy, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id, "note": "for you all" }),
        )
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();

    assert_eq!(copy.channel, s.other.id());
    assert_eq!(copy.author, s.member.id);
    assert_eq!(copy.content, original.content);
    // a forwarded copy pings nobody
    assert_eq!(copy.mentions, None);

    let origin = copy.forwarded.clone().expect("origin");
    assert_eq!(origin.channel.as_deref(), Some(s.general.id()));
    assert_eq!(origin.server.as_deref(), Some(s.server.id.as_str()));
    assert_eq!(origin.message, original.id);
    assert_eq!(origin.note.as_deref(), Some("for you all"));

    // stored, not just answered
    let stored = s.harness.db.fetch_message(&copy.id).await.unwrap();
    assert_eq!(stored.forwarded.map(|f| f.message), Some(original.id.clone()));

    // the copy shows someone else's words: no editing
    let response = s
        .harness
        .client
        .patch(format!("/channels/{}/messages/{}", s.other.id(), copy.id))
        .header(ContentType::JSON)
        .header(token(&s.member_session))
        .body(json!({ "content": "changed" }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);

    // outside servers too: the member's own saved notes
    let saved = Channel::SavedMessages {
        id: ulid::Ulid::new().to_string(),
        user: s.member.id.clone(),
    };
    s.harness.db.insert_channel(&saved).await.unwrap();
    let (status, copy, error) = s
        .forward(
            &s.member_session,
            saved.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert_eq!(copy.unwrap().forwarded.unwrap().note, None);
}

#[rocket::async_test]
async fn the_original_must_be_readable() {
    forwarding_on().await;
    let s = setup().await;
    let original = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "secret" }))
        .await;
    let body = json!({ "channel": s.general.id(), "message": original.id });

    // no ViewChannel on the source
    s.restrict(&s.general, ChannelPermission::ViewChannel).await;
    let (status, _, error) = s.forward(&s.member_session, s.other.id(), body.clone()).await;
    assert_eq!(status, Status::NotFound, "{error:?}");

    // no ReadMessageHistory on the source
    s.restrict(&s.general, ChannelPermission::ReadMessageHistory).await;
    let (status, _, error) = s.forward(&s.member_session, s.other.id(), body.clone()).await;
    assert_eq!(status, Status::NotFound, "{error:?}");

    // someone outside the server
    let (_, stranger_session, _) = s.harness.new_user().await;
    let (status, _, _) = s.forward(&stranger_session, s.other.id(), body.clone()).await;
    assert_ne!(status, Status::Ok);

    // a message id from another channel than the one named
    let elsewhere = s
        .send(&s.owner_session, s.other.id(), json!({ "content": "here" }))
        .await;
    let (status, _, _) = s
        .forward(
            &s.owner_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": elsewhere.id }),
        )
        .await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn the_target_must_allow_sending() {
    forwarding_on().await;
    let s = setup().await;
    let original = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "hello" }))
        .await;

    s.restrict(&s.other, ChannelPermission::SendMessage).await;
    let (status, _, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(
        matches!(error, Some(ErrorType::MissingPermission { ref permission }) if permission == "SendMessage"),
        "{error:?}"
    );
}

#[rocket::async_test]
async fn mature_messages_stay_in_mature_channels() {
    forwarding_on().await;
    let s = setup().await;
    let mut mature = s.harness.new_channel(&s.server).await;
    mature
        .update(
            &s.harness.db,
            PartialChannel {
                nsfw: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let mut mature_too = s.harness.new_channel(&s.server).await;
    mature_too
        .update(
            &s.harness.db,
            PartialChannel {
                nsfw: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let original = s
        .send(&s.owner_session, mature.id(), json!({ "content": "adults only" }))
        .await;
    let body = json!({ "channel": mature.id(), "message": original.id });

    let (status, _, error) = s.forward(&s.member_session, s.other.id(), body.clone()).await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(matches!(error, Some(ErrorType::ForwardMatureContent)), "{error:?}");

    let (status, _, error) = s.forward(&s.member_session, mature_too.id(), body).await;
    assert_eq!(status, Status::Ok, "{error:?}");
}

#[rocket::async_test]
async fn files_are_shared_not_moved() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-test-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "a picture", "attachments": [file] }),
        )
        .await;

    let (status, copy, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();
    let attachments = copy.attachments.expect("attachments");
    assert_eq!(attachments.len(), 1);
    assert_ne!(attachments[0].id, file, "a new record, not the original");

    // the original stays with its message; both point at the same stored file
    let first = s.harness.db.fetch_attachment("attachments", &file).await.unwrap();
    assert_eq!(first.used_for.map(|u| u.id), Some(original.id.clone()));
    let second = s
        .harness
        .db
        .fetch_attachment("attachments", &attachments[0].id)
        .await
        .unwrap();
    assert_eq!(second.used_for.map(|u| u.id), Some(copy.id.clone()));
    assert_eq!(second.uploader_id.as_deref(), Some(s.member.id.as_str()));
    assert_eq!(
        s.harness
            .db
            .count_file_hash_references("forward-test-hash")
            .await
            .unwrap(),
        2
    );

    // without Upload Files in the target, nothing is sent
    s.restrict(&s.other, ChannelPermission::UploadFiles).await;
    let (status, _, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(
        matches!(error, Some(ErrorType::MissingPermission { ref permission }) if permission == "UploadFiles"),
        "{error:?}"
    );
}

#[rocket::async_test]
async fn deleted_files_are_left_out() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-deleted-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "gone soon", "attachments": [file] }),
        )
        .await;
    s.harness.db.mark_attachment_as_deleted(&file).await.unwrap();

    let (status, copy, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id }),
        )
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert_eq!(copy.unwrap().attachments, None);
}

#[rocket::async_test]
async fn rich_embeds_go_along_where_embeds_are_allowed() {
    forwarding_on().await;
    let s = setup().await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "embeds": [{ "title": "Plans", "description": "Saturday at ten" }] }),
        )
        .await;

    let body = json!({ "channel": s.general.id(), "message": original.id });
    let (status, copy, error) = s.forward(&s.member_session, s.other.id(), body.clone()).await;
    assert_eq!(status, Status::Ok, "{error:?}");
    match copy.unwrap().embeds.as_deref() {
        Some([v0::Embed::Text(text)]) => {
            assert_eq!(text.title.as_deref(), Some("Plans"));
            assert_eq!(text.description.as_deref(), Some("Saturday at ten"));
        }
        other => panic!("expected one text embed, got {other:?}"),
    }

    // without Send Embeds in the target, nothing is sent
    s.restrict(&s.other, ChannelPermission::SendEmbeds).await;
    let (status, _, error) = s.forward(&s.member_session, s.other.id(), body).await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(
        matches!(error, Some(ErrorType::MissingPermission { ref permission }) if permission == "SendEmbeds"),
        "{error:?}"
    );
}

// ---- review round: refusals, origin privacy, safety cascade -------------

/// A message written straight into the database (system lines, other
/// people's conversations)
async fn stored(s: &Setup, channel: &str, author: &str, content: &str) -> revolt_database::Message {
    let message = revolt_database::Message {
        id: ulid::Ulid::new().to_string(),
        channel: channel.to_string(),
        author: author.to_string(),
        content: Some(content.to_string()),
        ..Default::default()
    };
    s.harness.db.insert_message(&message).await.unwrap();
    message
}

fn body(channel: &str, message: &str) -> Value {
    json!({ "channel": channel, "message": message })
}

async fn group_with(s: &Setup, recipients: Vec<String>) -> Channel {
    let group = Channel::Group {
        id: ulid::Ulid::new().to_string(),
        name: "Trip".to_string(),
        owner: s.owner.id.clone(),
        description: None,
        recipients,
        icon: None,
        last_message_id: None,
        permissions: None,
        nsfw: false,
    };
    s.harness.db.insert_channel(&group).await.unwrap();
    group
}

#[rocket::async_test]
async fn official_messages_are_not_forwarded() {
    forwarding_on().await;
    let s = setup().await;
    let official = revolt_database::official_account(
        &s.harness.db,
        revolt_database::OfficialAccount::Updates,
    )
    .await
    .unwrap();
    let news = stored(&s, s.general.id(), &official.id, "News").await;
    let (status, _, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &news.id))
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(matches!(error, Some(ErrorType::OfficialAccount)), "{error:?}");
}

#[rocket::async_test]
async fn conversations_the_person_is_not_in_are_out_of_reach() {
    forwarding_on().await;
    let s = setup().await;
    let (_, _, stranger) = s.harness.new_user().await;

    // a group the member left (no longer a recipient)
    let group = group_with(&s, vec![s.owner.id.clone(), stranger.id.clone()]).await;
    let in_group = stored(&s, group.id(), &s.owner.id, "plans").await;
    let (status, _, error) = s
        .forward(&s.member_session, s.other.id(), body(group.id(), &in_group.id))
        .await;
    assert_eq!(status, Status::NotFound, "left group: {error:?}");

    // a direct message between two other people
    let dm = Channel::DirectMessage {
        id: ulid::Ulid::new().to_string(),
        active: true,
        recipients: vec![s.owner.id.clone(), stranger.id.clone()],
        last_message_id: None,
    };
    s.harness.db.insert_channel(&dm).await.unwrap();
    let in_dm = stored(&s, dm.id(), &s.owner.id, "between us").await;
    let (status, _, error) = s
        .forward(&s.member_session, s.other.id(), body(dm.id(), &in_dm.id))
        .await;
    assert_eq!(status, Status::NotFound, "DM of others: {error:?}");
}

#[rocket::async_test]
async fn deleted_and_system_messages_are_refused() {
    forwarding_on().await;
    let s = setup().await;
    let gone = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "soon gone" }))
        .await;
    s.harness.db.delete_message(&gone.id).await.unwrap();
    let (status, _, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &gone.id))
        .await;
    assert_eq!(status, Status::NotFound);

    let line = revolt_database::Message {
        id: ulid::Ulid::new().to_string(),
        channel: s.general.id().to_string(),
        author: "00000000000000000000000000".to_string(),
        system: Some(revolt_database::SystemMessage::Text {
            content: "Welcome".to_string(),
        }),
        ..Default::default()
    };
    s.harness.db.insert_message(&line).await.unwrap();
    let (status, _, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &line.id))
        .await;
    assert_eq!(status, Status::BadRequest, "{error:?}");
    assert!(matches!(error, Some(ErrorType::InvalidOperation)), "{error:?}");
}

#[rocket::async_test]
async fn reported_and_legacy_files_are_left_out() {
    forwarding_on().await;
    let s = setup().await;
    let reported = s.upload(&s.owner, Some("forward-reported-hash")).await;
    let legacy = s.upload(&s.owner, None).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "two files", "attachments": [reported, legacy] }),
        )
        .await;
    s.harness.db.mark_attachment_as_reported(&reported).await.unwrap();

    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();
    assert_eq!(copy.attachments, None);
    assert_eq!(copy.content.as_deref(), Some("two files"));
}

#[rocket::async_test]
async fn bots_do_not_forward() {
    forwarding_on().await;
    let s = setup().await;
    let (bot, bot_user) = s.harness.new_bot(&s.owner).await;
    Member::create(&s.harness.db, &s.server, &bot_user, None).await.unwrap();
    let original = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "hi" }))
        .await;
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages/forward", s.other.id()))
        .header(ContentType::JSON)
        .header(Header::new("x-bot-token", bot.token.clone()))
        .body(body(s.general.id(), &original.id).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::BadRequest);
    let error: Error = response.into_json().await.unwrap();
    assert!(matches!(error.error_type, ErrorType::IsBot));
}

#[rocket::async_test]
async fn automod_reads_the_note_and_a_refusal_leaves_no_copies() {
    forwarding_on().await;
    let s = setup().await;
    let rule: v0::AutoModRule = serde_json::from_value(json!({
        "id": "rule",
        "name": "House rules",
        "enabled": true,
        "trigger": { "type": "Keywords", "words": ["forbidden"] },
        "actions": { "block": true },
    }))
    .unwrap();
    let mut server = s.server.clone();
    server
        .update(
            &s.harness.db,
            revolt_database::PartialServer {
                automod: Some(vec![rule]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    let file = s.upload(&s.owner, Some("forward-automod-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "fine", "attachments": [file] }),
        )
        .await;
    let (status, _, error) = s
        .forward(
            &s.member_session,
            s.other.id(),
            json!({ "channel": s.general.id(), "message": original.id, "note": "a forbidden word" }),
        )
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(matches!(error, Some(ErrorType::AutoModBlocked { .. })), "{error:?}");

    // the copy made for the refused send is marked deleted at once
    let dangling = s
        .harness
        .db
        .fetch_dangling_files()
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.hash.as_deref() == Some("forward-automod-hash"))
        .count();
    assert_eq!(dangling, 0);
    let deleted = s
        .harness
        .db
        .fetch_deleted_attachments()
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.hash.as_deref() == Some("forward-automod-hash"))
        .count();
    assert_eq!(deleted, 1);
}

#[rocket::async_test]
async fn a_forwarded_copy_pings_no_role_and_nobody_at_all() {
    forwarding_on().await;
    let s = setup().await;
    let role = s.harness.new_role(&s.server, 1, None).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": format!("@everyone and <%{}> and <@{}>", role.id, s.member.id) }),
        )
        .await;
    assert!(original.role_mentions.is_some(), "the original pings the role");

    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();
    assert_eq!(copy.mentions, None);
    assert_eq!(copy.role_mentions, None);
    assert_eq!(copy.flags & ((1 << 2) | (1 << 3)), 0, "no @everyone or @online");
}

#[rocket::async_test]
async fn a_teen_cannot_forward_from_a_mature_channel() {
    forwarding_on().await;
    let s = setup().await;
    let mut mature = s.harness.new_channel(&s.server).await;
    mature
        .update(
            &s.harness.db,
            PartialChannel {
                nsfw: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let original = s
        .send(&s.owner_session, mature.id(), json!({ "content": "adults only" }))
        .await;

    let mut account = s.harness.db.fetch_account(&s.member.id).await.unwrap();
    let now = iso8601_timestamp::Timestamp::now_utc();
    account.age = Some(revolt_database::AccountAge::declare((
        now.year() - 16,
        u8::from(now.month()),
    )));
    account.save(&s.harness.db).await.unwrap();

    let (status, _, error) = s
        .forward(&s.member_session, mature.id(), body(mature.id(), &original.id))
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(matches!(error, Some(ErrorType::AgeRestricted)), "{error:?}");
}

#[rocket::async_test]
async fn the_origin_names_no_place_for_private_or_masqueraded_messages() {
    forwarding_on().await;
    let s = setup().await;

    let group = group_with(&s, vec![s.owner.id.clone(), s.member.id.clone()]).await;
    let in_group = s
        .send(&s.owner_session, group.id(), json!({ "content": "plans" }))
        .await;
    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(group.id(), &in_group.id))
        .await;
    assert_eq!(status, Status::Ok, "group: {error:?}");
    let origin = copy.unwrap().forwarded.unwrap();
    assert_eq!(origin.channel, None);
    assert_eq!(origin.server, None);

    let masked = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "hello", "masquerade": { "name": "Narrator" } }),
        )
        .await;
    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &masked.id))
        .await;
    assert_eq!(status, Status::Ok, "masquerade: {error:?}");
    let origin = copy.unwrap().forwarded.unwrap();
    assert_eq!(origin.channel, None);
    assert_eq!(origin.server, None);
}

#[rocket::async_test]
async fn staff_removing_the_original_removes_every_copy() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-safety-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "bad picture", "attachments": [file] }),
        )
        .await;
    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy_file = copy.unwrap().attachments.unwrap()[0].id.clone();

    // a member reports the original: the copy is kept as evidence too
    let (_, reporter_session, reporter) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &reporter, None).await.unwrap();
    let response = s
        .harness
        .client
        .post("/safety/reports")
        .header(ContentType::JSON)
        .header(token(&reporter_session))
        .body(
            json!({
                "target": { "kind": "Message", "id": original.id },
                "category": "Harassment",
                "subcategory": "targeted",
                "description": "keeps posting this"
            })
            .to_string(),
        )
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let receipt: v0::SafetyReceipt = response.into_json().await.unwrap();
    let copy_record = s.harness.db.fetch_attachment("attachments", &copy_file).await.unwrap();
    assert_eq!(copy_record.reported, Some(true), "the report reaches the copy");

    // staff remove the original message
    let (_, staff_session, staff) = s.harness.new_user().await;
    s.harness
        .db
        .save_platform_role(&revolt_database::PlatformRole {
            id: staff.id.clone(),
            permissions: vec![
                revolt_database::PlatformPermission::SafetyReview,
                revolt_database::PlatformPermission::SafetyAction,
            ],
            creator: None,
            show_public_name: false,
            updated_at: 0,
            updated_by: "test".to_string(),
        })
        .await
        .unwrap();
    let case = s.harness.db.fetch_safety_case(&receipt.id).await.unwrap();
    let response = s
        .harness
        .client
        .post(format!("/safety/admin/reports/{}/actions", case.id))
        .header(ContentType::JSON)
        .header(token(&staff_session))
        .body(
            json!({
                "kind": "RemoveMessage", "public_reason": "Harassment", "version": case.version,
                "statement": { "ground": { "type": "Terms", "rule": "rule:Harassment" }, "facts": "Posted to harass" }
            })
            .to_string(),
        )
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    assert_eq!(status, Status::Ok, "{text}");

    for id in [&file, &copy_file] {
        let record = s.harness.db.fetch_attachment("attachments", id).await.unwrap();
        assert_eq!(record.deleted, Some(true), "{id} is unavailable");
    }
}

#[rocket::async_test]
async fn staff_removing_a_copy_removes_the_original_too() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-copy-safety-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "bad picture", "attachments": [file] }),
        )
        .await;
    let (_, copy, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    let copy = s.harness.db.fetch_message(&copy.unwrap().id).await.unwrap();

    revolt_database::File::remove_with_copies(&s.harness.db, copy.attachments.as_deref().unwrap_or_default())
        .await
        .unwrap();
    let record = s.harness.db.fetch_attachment("attachments", &file).await.unwrap();
    assert_eq!(record.deleted, Some(true));
}

#[rocket::async_test]
async fn deleting_the_original_removes_the_copies_but_not_the_other_way() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-delete-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "photo", "attachments": [file] }),
        )
        .await;
    let (_, first, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    let (_, second, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    let first = first.unwrap();
    let second = second.unwrap();
    let first_file = first.attachments.clone().unwrap()[0].id.clone();
    let second_file = second.attachments.clone().unwrap()[0].id.clone();

    // the forwarder deletes one copy: the original and the other copy stay
    s.harness
        .db
        .fetch_message(&first.id)
        .await
        .unwrap()
        .delete(&s.harness.db)
        .await
        .unwrap();
    for (id, deleted) in [(&file, None), (&first_file, Some(true)), (&second_file, None)] {
        let record = s.harness.db.fetch_attachment("attachments", id).await.unwrap();
        assert_eq!(record.deleted, deleted, "{id}");
    }

    // the author deletes the original: the remaining copy goes with it
    s.harness
        .db
        .fetch_message(&original.id)
        .await
        .unwrap()
        .delete(&s.harness.db)
        .await
        .unwrap();
    let record = s.harness.db.fetch_attachment("attachments", &second_file).await.unwrap();
    assert_eq!(record.deleted, Some(true));
}

// ---- review round 2: bulk deletion, account erasure, embed images -------

async fn deleted(s: &Setup, id: &str) -> Option<bool> {
    s.harness
        .db
        .fetch_attachment("attachments", id)
        .await
        .unwrap()
        .deleted
}

#[rocket::async_test]
async fn moderator_bulk_delete_removes_the_files_and_their_copies() {
    forwarding_on().await;
    let s = setup().await;
    let file = s.upload(&s.owner, Some("forward-bulk-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": "photo", "attachments": [file] }),
        )
        .await;
    let kept = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "stays" }))
        .await;
    let (_, copy, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    let copy_file = copy.unwrap().attachments.unwrap()[0].id.clone();

    let response = s
        .harness
        .client
        .delete(format!("/channels/{}/messages/bulk", s.general.id()))
        .header(ContentType::JSON)
        .header(token(&s.owner_session))
        .body(json!({ "ids": [original.id] }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);

    assert_eq!(deleted(&s, &file).await, Some(true), "the file of the message");
    assert_eq!(deleted(&s, &copy_file).await, Some(true), "its forwarded copy");
    assert!(
        s.harness.db.fetch_message(&kept.id).await.is_ok(),
        "a message not named stays"
    );
}

#[rocket::async_test]
async fn account_erasure_removes_the_files_and_their_copies() {
    forwarding_on().await;
    let s = setup().await;
    let (_, author_session, mut author) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &author, None).await.unwrap();
    let file = s.upload(&author, Some("forward-erasure-hash")).await;
    let original = s
        .send(
            &author_session,
            s.general.id(),
            json!({ "content": "mine", "attachments": [file] }),
        )
        .await;
    let (_, copy, _) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    let copy_file = copy.unwrap().attachments.unwrap()[0].id.clone();

    author.delete(&s.harness.db).await.unwrap();

    assert!(s.harness.db.fetch_message(&original.id).await.is_err());
    assert_eq!(deleted(&s, &file).await, Some(true), "the file of the message");
    assert_eq!(deleted(&s, &copy_file).await, Some(true), "its forwarded copy");
}

#[rocket::async_test]
async fn images_inside_forwarded_rich_embeds_are_copies_and_follow_the_original() {
    forwarding_on().await;
    let s = setup().await;
    let image = s.upload(&s.owner, Some("forward-embed-hash")).await;
    let original = s
        .send(
            &s.owner_session,
            s.general.id(),
            json!({ "embeds": [{ "title": "Poster", "media": image }] }),
        )
        .await;

    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copied_image = match copy.unwrap().embeds.as_deref() {
        Some([v0::Embed::Text(text)]) => text.media.clone().expect("image").id,
        other => panic!("expected one text embed, got {other:?}"),
    };
    assert_ne!(copied_image, image, "a new record, not the original");
    let record = s
        .harness
        .db
        .fetch_attachment("attachments", &copied_image)
        .await
        .unwrap();
    assert_eq!(record.forwarded_from.as_deref(), Some(image.as_str()));
    assert_eq!(record.hash.as_deref(), Some("forward-embed-hash"));

    s.harness
        .db
        .fetch_message(&original.id)
        .await
        .unwrap()
        .delete(&s.harness.db)
        .await
        .unwrap();
    assert_eq!(deleted(&s, &image).await, Some(true));
    assert_eq!(deleted(&s, &copied_image).await, Some(true), "the copied image goes too");
}

// ---- review round 3: ban with message deletion, paged erasure ----------

#[rocket::async_test]
async fn a_ban_that_deletes_messages_removes_their_files_and_copies() {
    forwarding_on().await;
    let s = setup().await;
    let (_, author_session, author) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &author, None).await.unwrap();
    // the author may send embeds like anyone with the default rights
    let file = s.upload(&author, Some("forward-ban-hash")).await;
    let image = s.upload(&author, Some("forward-ban-embed-hash")).await;
    let original = s
        .send(
            &author_session,
            s.general.id(),
            json!({ "content": "spam", "attachments": [file], "embeds": [{ "title": "Ad", "media": image }] }),
        )
        .await;
    let (status, copy, error) = s
        .forward(&s.member_session, s.other.id(), body(s.general.id(), &original.id))
        .await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();
    let copy_file = copy.attachments.clone().unwrap()[0].id.clone();
    let copy_image = match copy.embeds.as_deref() {
        Some([v0::Embed::Text(text)]) => text.media.clone().expect("image").id,
        other => panic!("expected one text embed, got {other:?}"),
    };

    let response = s
        .harness
        .client
        .put(format!("/servers/{}/bans/{}", s.server.id, author.id))
        .header(ContentType::JSON)
        .header(token(&s.owner_session))
        .body(json!({ "reason": "spam", "delete_message_seconds": 3600 }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);

    assert!(s.harness.db.fetch_message(&original.id).await.is_err());
    for (id, what) in [
        (&file, "the file"),
        (&image, "the embed image"),
        (&copy_file, "the forwarded copy"),
        (&copy_image, "the copied embed image"),
    ] {
        assert_eq!(deleted(&s, id).await, Some(true), "{what}");
    }
}

#[rocket::async_test]
async fn files_of_an_author_are_removed_page_by_page() {
    forwarding_on().await;
    let s = setup().await;
    let mut files = Vec::new();
    for n in 0..5 {
        let file = s.upload(&s.owner, Some(&format!("forward-page-hash-{n}"))).await;
        s.send(
            &s.owner_session,
            s.general.id(),
            json!({ "content": format!("photo {n}"), "attachments": [file] }),
        )
        .await;
        files.push(file);
    }

    // two at a time: five messages take three pages
    revolt_database::Message::remove_files_of_author(&s.harness.db, &s.owner.id, None, None, 2)
        .await
        .unwrap();
    for file in &files {
        assert_eq!(deleted(&s, file).await, Some(true), "{file}");
    }
}
