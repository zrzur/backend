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

    async fn upload(&self, uploader: &User, hash: &str) -> String {
        let id = ulid::Ulid::new().to_string();
        self.harness
            .db
            .insert_attachment(&File {
                id: id.clone(),
                tag: "attachments".to_string(),
                filename: "a.png".to_string(),
                hash: Some(hash.to_string()),
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
    assert_eq!(origin.channel, s.general.id());
    assert_eq!(origin.server.as_deref(), Some(s.server.id.as_str()));
    assert_eq!(origin.message, original.id);
    assert_eq!(origin.author, s.owner.id);
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
    let file = s.upload(&s.owner, "forward-test-hash").await;
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
    let file = s.upload(&s.owner, "forward-deleted-hash").await;
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
