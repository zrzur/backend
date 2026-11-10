//! Polls in messages: sending one, voting, voters, ending, and what other
//! message routes do with a poll.
use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::overwrite_config;
use revolt_database::{
    events::client::EventV1, Channel, Member, PartialChannel, Server, Session, User,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField};
use revolt_result::{Error, ErrorType};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use crate::util::test::{PubSubTestHelper, TestHarness};

async fn polls_on() {
    overwrite_config(|config| {
        config.features.polls_enabled = true;
        config.features.message_forwarding_enabled = true;
    })
    .await;
}

struct Setup {
    harness: TestHarness,
    owner: User,
    owner_session: Session,
    member: User,
    member_session: Session,
    server: Server,
    general: Channel,
}

async fn setup() -> Setup {
    let harness = TestHarness::new().await;
    let (_, owner_session, owner) = harness.new_user().await;
    let (_, member_session, member) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
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
    }
}

fn token(session: &Session) -> Header<'static> {
    Header::new("x-session-token", session.token.clone())
}

fn poll_body(multi: bool) -> Value {
    json!({
        "poll": {
            "question": "Where do we meet?",
            "answers": [
                { "text": "Library", "emoji": "📚" },
                { "text": "Café" },
                { "text": "Park" }
            ],
            "duration": 24,
            "allow_multiselect": multi
        }
    })
}

type Answer<T> = (Status, Option<T>, Option<ErrorType>);

async fn read<T: serde::de::DeserializeOwned>(
    response: rocket::local::asynchronous::LocalResponse<'_>,
) -> Answer<T> {
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    if status == Status::Ok {
        (status, serde_json::from_str(&text).ok(), None)
    } else {
        let error: Option<Error> = serde_json::from_str(&text).ok();
        (status, None, error.map(|e| e.error_type))
    }
}

impl Setup {
    async fn send(&self, session: &Session, channel: &str, body: Value) -> Answer<v0::Message> {
        let response = self
            .harness
            .client
            .post(format!("/channels/{channel}/messages"))
            .header(ContentType::JSON)
            .header(token(session))
            .body(body.to_string())
            .dispatch()
            .await;
        read(response).await
    }

    async fn poll(&self, multi: bool) -> v0::Message {
        let (status, message, error) = self
            .send(&self.owner_session, self.general.id(), poll_body(multi))
            .await;
        assert_eq!(status, Status::Ok, "{error:?}");
        message.unwrap()
    }

    async fn vote(&self, session: &Session, message: &v0::Message, answer: u32) -> Answer<v0::Poll> {
        let response = self
            .harness
            .client
            .put(format!(
                "/channels/{}/messages/{}/polls/answers/{answer}/votes/@me",
                message.channel, message.id
            ))
            .header(token(session))
            .dispatch()
            .await;
        read(response).await
    }

    async fn unvote(&self, session: &Session, message: &v0::Message, answer: u32) -> Answer<v0::Poll> {
        let response = self
            .harness
            .client
            .delete(format!(
                "/channels/{}/messages/{}/polls/answers/{answer}/votes/@me",
                message.channel, message.id
            ))
            .header(token(session))
            .dispatch()
            .await;
        read(response).await
    }

    async fn voters(&self, session: &Session, message: &v0::Message, answer: u32, query: &str) -> Answer<v0::PollVoters> {
        let response = self
            .harness
            .client
            .get(format!(
                "/channels/{}/messages/{}/polls/answers/{answer}/voters{query}",
                message.channel, message.id
            ))
            .header(token(session))
            .dispatch()
            .await;
        read(response).await
    }

    async fn end(&self, session: &Session, message: &v0::Message) -> Answer<v0::Poll> {
        let response = self
            .harness
            .client
            .post(format!(
                "/channels/{}/messages/{}/polls/end",
                message.channel, message.id
            ))
            .header(token(session))
            .dispatch()
            .await;
        read(response).await
    }

    async fn deny_everyone(&self, channel: &Channel, deny: ChannelPermission) {
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
    }

    fn counts(poll: &v0::Poll) -> Vec<u32> {
        poll.answers.iter().map(|answer| answer.count).collect()
    }
}

#[rocket::async_test]
async fn a_message_can_carry_a_poll() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let poll = message.poll.expect("poll");
    assert_eq!(poll.question, "Where do we meet?");
    assert_eq!(poll.answers.len(), 3);
    assert_eq!(poll.answers[0].id, 1);
    assert_eq!(poll.answers[0].emoji.as_deref(), Some("📚"));
    assert!(poll.expires_at > Timestamp::now_utc() + Duration::hours(23));
    assert!(poll.expires_at <= Timestamp::now_utc() + Duration::hours(24));
    assert!(!poll.ended);
}

#[rocket::async_test]
async fn polls_are_off_unless_switched_on() {
    let s = setup().await;
    let (status, _, error) = s
        .send(&s.owner_session, s.general.id(), poll_body(false))
        .await;
    assert_ne!(status, Status::Ok);
    assert!(
        matches!(error, Some(ErrorType::FeatureDisabled { ref feature }) if feature == "polls"),
        "{error:?}"
    );
}

#[rocket::async_test]
async fn invalid_polls_are_refused() {
    polls_on().await;
    let s = setup().await;
    for poll in [
        json!({ "question": "", "answers": [{ "text": "a" }, { "text": "b" }], "duration": 1 }),
        json!({ "question": "q", "answers": [{ "text": "a" }], "duration": 1 }),
        json!({ "question": "q", "answers": [{ "text": "a" }, { "text": "b" }], "duration": 2 }),
        json!({ "question": "q", "answers": [{ "text": "a".repeat(56) }, { "text": "b" }], "duration": 1 }),
        json!({ "question": "q", "answers": [{ "text": "a", "emoji": "not-an-emoji" }, { "text": "b" }], "duration": 1 }),
    ] {
        let (status, _, error) = s
            .send(&s.owner_session, s.general.id(), json!({ "poll": poll }))
            .await;
        assert_eq!(status, Status::BadRequest, "{poll}: {error:?}");
    }
}

#[rocket::async_test]
async fn a_poll_needs_send_embeds() {
    polls_on().await;
    let s = setup().await;
    s.deny_everyone(&s.general, ChannelPermission::SendEmbeds).await;
    let (status, _, error) = s
        .send(&s.member_session, s.general.id(), poll_body(false))
        .await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(
        matches!(error, Some(ErrorType::MissingPermission { ref permission }) if permission == "SendEmbeds"),
        "{error:?}"
    );
}

#[rocket::async_test]
async fn single_choice_moves_the_vote() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;

    let (status, poll, error) = s.vote(&s.member_session, &message, 1).await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let poll = poll.unwrap();
    assert_eq!(Setup::counts(&poll), [1, 0, 0]);
    assert_eq!(poll.me, Some(vec![1]));

    let (_, poll, _) = s.vote(&s.member_session, &message, 2).await;
    let poll = poll.unwrap();
    assert_eq!(Setup::counts(&poll), [0, 1, 0]);
    assert_eq!(poll.me, Some(vec![2]));

    // voting the same answer again changes nothing
    let (_, poll, _) = s.vote(&s.member_session, &message, 2).await;
    assert_eq!(Setup::counts(&poll.unwrap()), [0, 1, 0]);

    let (_, poll, _) = s.unvote(&s.member_session, &message, 2).await;
    let poll = poll.unwrap();
    assert_eq!(Setup::counts(&poll), [0, 0, 0]);
    assert_eq!(poll.me, Some(vec![]));
}

#[rocket::async_test]
async fn multiple_choice_adds_votes() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(true).await;

    s.vote(&s.member_session, &message, 1).await;
    let (_, poll, _) = s.vote(&s.member_session, &message, 3).await;
    let poll = poll.unwrap();
    assert_eq!(Setup::counts(&poll), [1, 0, 1]);
    assert_eq!(poll.me, Some(vec![1, 3]));

    let (_, poll, _) = s.vote(&s.owner_session, &message, 3).await;
    assert_eq!(Setup::counts(&poll.unwrap()), [1, 0, 2]);

    let (_, poll, _) = s.unvote(&s.member_session, &message, 1).await;
    assert_eq!(poll.unwrap().me, Some(vec![3]));
}

#[rocket::async_test]
async fn readers_see_their_own_choice_and_counts_only() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    s.vote(&s.member_session, &message, 2).await;

    let response = s
        .harness
        .client
        .get(format!("/channels/{}/messages/{}", message.channel, message.id))
        .header(token(&s.member_session))
        .dispatch()
        .await;
    let text = response.into_string().await.unwrap();
    assert!(!text.contains(&s.member.id), "voter ids stay out of messages: {text}");
    let fetched: v0::Message = serde_json::from_str(&text).unwrap();
    let poll = fetched.poll.unwrap();
    assert_eq!(poll.me, Some(vec![2]));
    assert_eq!(Setup::counts(&poll), [0, 1, 0]);

    // the owner reads the same counts, with their own (empty) choice
    let response = s
        .harness
        .client
        .get(format!("/channels/{}/messages?limit=5", message.channel))
        .header(token(&s.owner_session))
        .dispatch()
        .await;
    let messages: Vec<v0::Message> = response.into_json().await.unwrap();
    let poll = messages
        .into_iter()
        .find(|m| m.id == message.id)
        .and_then(|m| m.poll)
        .unwrap();
    assert_eq!(poll.me, Some(vec![]));
    assert_eq!(Setup::counts(&poll), [0, 1, 0]);
}

#[rocket::async_test]
async fn votes_reach_the_channel_as_counts() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let mut pubsub = PubSubTestHelper::new(&message.channel).await;

    s.vote(&s.member_session, &message, 3).await;

    let event = pubsub
        .wait_for_event(|event| matches!(event, EventV1::MessagePollVoteAdd { .. }))
        .await;
    let EventV1::MessagePollVoteAdd {
        id,
        answer_id,
        answers,
        ..
    } = event
    else {
        unreachable!()
    };
    assert_eq!(id, message.id);
    assert_eq!(answer_id, 3);
    assert_eq!(
        answers.iter().map(|a| (a.id, a.count)).collect::<Vec<_>>(),
        [(1, 0), (2, 0), (3, 1)]
    );
}

#[rocket::async_test]
async fn voting_needs_react_and_a_real_answer() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;

    let (status, _, _) = s.vote(&s.member_session, &message, 9).await;
    assert_eq!(status, Status::NotFound);

    s.deny_everyone(&s.general, ChannelPermission::React).await;
    let (status, _, error) = s.vote(&s.member_session, &message, 1).await;
    assert_eq!(status, Status::Forbidden, "{error:?}");
    assert!(
        matches!(error, Some(ErrorType::MissingPermission { ref permission }) if permission == "React"),
        "{error:?}"
    );

    // a message without a poll has no answers
    let (_, plain, _) = s
        .send(&s.owner_session, s.general.id(), json!({ "content": "hi" }))
        .await;
    let (status, _, _) = s.vote(&s.owner_session, &plain.unwrap(), 1).await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn outsiders_cannot_vote_or_list_voters() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let (_, stranger_session, _) = s.harness.new_user().await;

    let (status, _, _) = s.vote(&stranger_session, &message, 1).await;
    assert_ne!(status, Status::Ok);
    let (status, _, _) = s.voters(&stranger_session, &message, 1, "").await;
    assert_ne!(status, Status::Ok);
}

#[rocket::async_test]
async fn mature_channels_follow_the_age_rules() {
    polls_on().await;
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
    let (_, poll, error) = s
        .send(&s.owner_session, mature.id(), poll_body(false))
        .await;
    let message = poll.unwrap_or_else(|| panic!("{error:?}"));

    let (_, young_session, young) = s.harness.new_user_without_age().await;
    Member::create(&s.harness.db, &s.server, &young, None)
        .await
        .unwrap();
    let (status, _, error) = s.vote(&young_session, &message, 1).await;
    assert_eq!(status, Status::Forbidden);
    assert!(matches!(error, Some(ErrorType::AgeRestricted | ErrorType::AgeNotDeclared)), "{error:?}");
    let (_, _, error) = s.voters(&young_session, &message, 1, "").await;
    assert!(matches!(error, Some(ErrorType::AgeRestricted | ErrorType::AgeNotDeclared)), "{error:?}");
}

#[rocket::async_test]
async fn voters_come_a_page_at_a_time() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    s.vote(&s.member_session, &message, 1).await;
    s.vote(&s.owner_session, &message, 1).await;

    let (status, page, error) = s.voters(&s.member_session, &message, 1, "?limit=1").await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let page = page.unwrap();
    assert_eq!(page.count, 2);
    assert_eq!(page.ids, [s.member.id.clone()]);
    assert_eq!(page.users.len(), 1);
    assert_eq!(page.members.as_ref().map(Vec::len), Some(1));

    let (_, page, _) = s
        .voters(&s.member_session, &message, 1, &format!("?limit=1&after={}", s.member.id))
        .await;
    assert_eq!(page.unwrap().ids, [s.owner.id.clone()]);

    let (_, page, _) = s.voters(&s.member_session, &message, 2, "").await;
    assert_eq!(page.unwrap().count, 0);
}

#[rocket::async_test]
async fn the_author_or_a_moderator_ends_a_poll() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    s.vote(&s.member_session, &message, 2).await;

    let (status, _, error) = s.end(&s.member_session, &message).await;
    assert_eq!(status, Status::Forbidden, "{error:?}");

    let mut pubsub = PubSubTestHelper::new(&message.channel).await;
    let (status, poll, error) = s.end(&s.owner_session, &message).await;
    assert_eq!(status, Status::Ok, "{error:?}");
    assert!(poll.unwrap().ended);

    // the reply names the winner
    let reply = pubsub
        .wait_for_event(|event| {
            matches!(
                event,
                EventV1::Message(v0::Message {
                    system: Some(v0::SystemMessage::PollEnded { .. }),
                    ..
                })
            )
        })
        .await;
    let EventV1::Message(reply) = reply else {
        unreachable!()
    };
    assert_eq!(reply.replies, Some(vec![message.id.clone()]));
    assert_eq!(
        reply.system,
        Some(v0::SystemMessage::PollEnded {
            id: message.id.clone(),
            question: "Where do we meet?".to_string(),
            winners: vec!["Café".to_string()],
            votes: 1,
            total: 1,
        })
    );

    // final: no more votes, no second end
    let (status, _, error) = s.vote(&s.member_session, &message, 1).await;
    assert_eq!(status, Status::Conflict);
    assert!(matches!(error, Some(ErrorType::PollEnded)), "{error:?}");
    let (_, _, error) = s.end(&s.owner_session, &message).await;
    assert!(matches!(error, Some(ErrorType::PollEnded)), "{error:?}");
}

#[rocket::async_test]
async fn moderators_end_other_peoples_polls() {
    polls_on().await;
    let s = setup().await;
    let (_, message, error) = s
        .send(&s.member_session, s.general.id(), poll_body(false))
        .await;
    let message = message.unwrap_or_else(|| panic!("{error:?}"));
    // the owner has Manage Messages
    let (status, _, error) = s.end(&s.owner_session, &message).await;
    assert_eq!(status, Status::Ok, "{error:?}");
}

#[rocket::async_test]
async fn expired_polls_are_closed_once() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    s.vote(&s.member_session, &message, 1).await;

    // the time is up: crond's pass closes it, a second pass finds nothing
    let later = Timestamp::now_utc() + Duration::hours(25);
    let closed = revolt_database::Message::close_due_polls(&s.harness.db, later, 100)
        .await
        .unwrap();
    assert_eq!(closed, 1);
    let closed = revolt_database::Message::close_due_polls(&s.harness.db, later, 100)
        .await
        .unwrap();
    assert_eq!(closed, 0);

    let stored = s.harness.db.fetch_message(&message.id).await.unwrap();
    assert!(stored.poll.unwrap().ended);
}

#[rocket::async_test]
async fn polls_cannot_be_edited() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let response = s
        .harness
        .client
        .patch(format!("/channels/{}/messages/{}", message.channel, message.id))
        .header(ContentType::JSON)
        .header(token(&s.owner_session))
        .body(json!({ "content": "changed" }).to_string())
        .dispatch()
        .await;
    let (_, _, error): Answer<v0::Message> = read(response).await;
    assert!(matches!(error, Some(ErrorType::CannotEditMessage)), "{error:?}");
}

#[rocket::async_test]
async fn deleting_the_message_deletes_the_votes() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    s.vote(&s.member_session, &message, 1).await;

    let response = s
        .harness
        .client
        .delete(format!("/channels/{}/messages/{}", message.channel, message.id))
        .header(token(&s.owner_session))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    let (status, _, _) = s.voters(&s.owner_session, &message, 1, "").await;
    assert_eq!(status, Status::NotFound);
}

#[rocket::async_test]
async fn a_forwarded_poll_is_text() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let other = s.harness.new_channel(&s.server).await;

    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages/forward", other.id()))
        .header(ContentType::JSON)
        .header(token(&s.member_session))
        .body(json!({ "channel": message.channel, "message": message.id }).to_string())
        .dispatch()
        .await;
    let (status, copy, error): Answer<v0::Message> = read(response).await;
    assert_eq!(status, Status::Ok, "{error:?}");
    let copy = copy.unwrap();
    assert!(copy.poll.is_none());
    assert_eq!(
        copy.content.as_deref(),
        Some("📊 Where do we meet?\n• 📚 Library\n• Café\n• Park")
    );
}

#[rocket::async_test]
async fn bots_do_not_vote() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    let (bot, bot_user) = s.harness.new_bot(&s.owner).await;
    Member::create(&s.harness.db, &s.server, &bot_user, None)
        .await
        .unwrap();
    let response = s
        .harness
        .client
        .put(format!(
            "/channels/{}/messages/{}/polls/answers/1/votes/@me",
            message.channel, message.id
        ))
        .header(Header::new("x-bot-token", bot.token.clone()))
        .dispatch()
        .await;
    let (_, _, error): Answer<v0::Poll> = read(response).await;
    assert!(matches!(error, Some(ErrorType::IsBot)), "{error:?}");
}

#[rocket::async_test]
async fn automod_reads_the_question_and_every_answer() {
    polls_on().await;
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

    for poll in [
        json!({ "question": "a forbidden question", "answers": [{ "text": "a" }, { "text": "b" }], "duration": 1 }),
        json!({ "question": "fine", "answers": [{ "text": "a" }, { "text": "forbidden" }], "duration": 1 }),
    ] {
        let (status, _, error) = s
            .send(&s.member_session, s.general.id(), json!({ "poll": poll }))
            .await;
        assert_eq!(status, Status::Forbidden, "{poll}: {error:?}");
        assert!(matches!(error, Some(ErrorType::AutoModBlocked { .. })), "{error:?}");
    }
}

#[rocket::async_test]
async fn events_carry_a_rising_version_and_the_total() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(true).await;
    let mut pubsub = PubSubTestHelper::new(&message.channel).await;

    let (_, first, _) = s.vote(&s.member_session, &message, 1).await;
    let (_, second, _) = s.vote(&s.member_session, &message, 2).await;
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(second.version > first.version, "{} then {}", first.version, second.version);

    let event = pubsub
        .wait_for_event(|event| {
            matches!(event, EventV1::MessagePollVoteAdd { answer_id: 2, .. })
        })
        .await;
    let EventV1::MessagePollVoteAdd { version, total, .. } = event else {
        unreachable!()
    };
    assert_eq!(version, second.version);
    assert_eq!(total, 2);
}

#[rocket::async_test]
async fn a_closed_poll_always_gets_its_reply() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;

    // closed, then the process stopped before the reply went out
    assert!(s.harness.db.end_poll(&message.id, Timestamp::now_utc()).await.unwrap());

    // crond's next pass posts the reply, once
    let now = Timestamp::now_utc();
    assert_eq!(
        revolt_database::Message::close_due_polls(&s.harness.db, now, 100).await.unwrap(),
        1
    );
    assert_eq!(
        revolt_database::Message::close_due_polls(&s.harness.db, now, 100).await.unwrap(),
        0
    );
    let replies: Vec<v0::Message> = s
        .harness
        .client
        .get(format!("/channels/{}/messages?limit=10", message.channel))
        .header(token(&s.owner_session))
        .dispatch()
        .await
        .into_json()
        .await
        .unwrap();
    let count = replies
        .iter()
        .filter(|m| matches!(m.system, Some(v0::SystemMessage::PollEnded { .. })))
        .count();
    assert_eq!(count, 1);
}

#[rocket::async_test]
async fn votes_have_their_own_rate_limit_bucket() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(true).await;
    let response = s
        .harness
        .client
        .put(format!(
            "/channels/{}/messages/{}/polls/answers/1/votes/@me",
            message.channel, message.id
        ))
        .header(token(&s.member_session))
        .dispatch()
        .await;
    let vote_bucket = response.headers().get_one("x-ratelimit-bucket").map(str::to_string);
    let vote_limit = response.headers().get_one("x-ratelimit-limit").map(str::to_string);
    let response = s
        .harness
        .client
        .post(format!("/channels/{}/messages", message.channel))
        .header(ContentType::JSON)
        .header(token(&s.member_session))
        .body(json!({ "content": "hi" }).to_string())
        .dispatch()
        .await;
    let send_bucket = response.headers().get_one("x-ratelimit-bucket").map(str::to_string);
    assert!(vote_bucket.is_some());
    assert_ne!(vote_bucket, send_bucket);
    assert_eq!(vote_limit.as_deref(), Some("10"));
}

#[rocket::async_test]
async fn a_poll_message_previews_as_its_summary() {
    polls_on().await;
    let s = setup().await;
    let message = s.poll(false).await;
    assert_eq!(
        message.preview_text().as_deref(),
        Some("📊 Where do we meet?\n• 📚 Library\n• Café\n• Park")
    );
}
