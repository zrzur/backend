//! Channel visibility in the event stream (zrzur-u7-01).
//!
//! These run against the in-memory reference database; no Redis or MongoDB.

use std::{collections::HashMap, num::NonZeroUsize};

use revolt_database::{
    events::client::EventV1, Channel, Database, DatabaseInfo, Member, MemberCompositeKey,
    PartialChannel, Role, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{ChannelPermission, OverrideField, DEFAULT_PERMISSION_SERVER};

use super::state::{State, SubscriptionStateChange};

const SERVER: &str = "server";
const CHANNEL: &str = "channel";
const STAFF: &str = "staff";
const OWNER: &str = "owner";

fn view() -> i64 {
    ChannelPermission::ViewChannel as i64
}

/// A server whose @everyone can or cannot see channels. The staff role adds
/// nothing at server level; channels grant it access with an override.
fn server(private: bool) -> Server {
    let default = *DEFAULT_PERMISSION_SERVER as i64;
    serde_json::from_value(serde_json::json!({
        "_id": SERVER,
        "owner": OWNER,
        "name": "Test",
        "channels": [CHANNEL],
        "default_permissions": if private { default & !view() } else { default },
        "roles": {
            STAFF: {
                "_id": STAFF,
                "name": "Staff",
                "permissions": { "a": 0, "d": 0 },
                "hoist": false,
                "rank": 1
            }
        }
    }))
    .expect("valid server")
}

fn channel(
    role_permissions: HashMap<String, OverrideField>,
    default_permissions: Option<OverrideField>,
) -> Channel {
    Channel::TextChannel {
        id: CHANNEL.to_string(),
        server: SERVER.to_string(),
        name: "staff-only".to_string(),
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions,
        role_permissions,
        member_permissions: HashMap::new(),
        nsfw: false,
        voice: None,
        slowmode: None,
        announcement: false,
        permissions_synced: false,
        parent: None,
        thread: None,
        forum: None,
    }
}

fn staff_can_view() -> HashMap<String, OverrideField> {
    HashMap::from([(STAFF.to_string(), OverrideField { a: view(), d: 0 })])
}

async fn db() -> Database {
    DatabaseInfo::Reference.connect().await.expect("reference db")
}

async fn state(user_id: &str, server: &Server, roles: &[&str]) -> State {
    let user = User {
        id: user_id.to_string(),
        username: user_id.to_string(),
        ..Default::default()
    };

    let mut state = State::from(user, "session".to_string(), NonZeroUsize::new(16).unwrap());
    state.state = SubscriptionStateChange::None;
    state.cache.servers.insert(SERVER.to_string(), server.clone());
    state.cache.members.insert(
        SERVER.to_string(),
        Member {
            id: MemberCompositeKey {
                server: SERVER.to_string(),
                user: user_id.to_string(),
            },
            roles: roles.iter().map(|r| r.to_string()).collect(),
            ..Default::default()
        },
    );
    state.insert_subscription(SERVER.to_string()).await;
    state
}

/// Put a channel the user can already see into its state, as Ready would.
async fn follow(state: &mut State, channel: &Channel) {
    state
        .cache
        .channels
        .insert(CHANNEL.to_string(), channel.clone());
    state.insert_subscription(CHANNEL.to_string()).await;
}

async fn subscribed(state: &State, topic: &str) -> bool {
    state.subscribed.read().await.contains(topic)
}

fn permissions_update(data: PartialChannel) -> EventV1 {
    EventV1::ChannelUpdate {
        id: CHANNEL.to_string(),
        data: data.into(),
        clear: vec![],
    }
}

// ChannelCreate

#[tokio::test]
async fn hidden_channel_create_is_dropped_for_member() {
    let db = db().await;
    let server = server(true);
    let mut state = state("member", &server, &[]).await;
    let mut event = EventV1::ChannelCreate(channel(staff_can_view(), None).into());

    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(!subscribed(&state, CHANNEL).await);
    assert!(!state.cache.channels.contains_key(CHANNEL));
}

#[tokio::test]
async fn hidden_channel_create_reaches_role_with_access() {
    let db = db().await;
    let server = server(true);
    let mut state = state("staff-member", &server, &[STAFF]).await;
    let mut event = EventV1::ChannelCreate(channel(staff_can_view(), None).into());

    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn hidden_channel_create_reaches_owner() {
    let db = db().await;
    let server = server(true);
    let mut state = state(OWNER, &server, &[]).await;
    let mut event = EventV1::ChannelCreate(channel(HashMap::new(), None).into());

    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn public_channel_create_reaches_member() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    let mut event = EventV1::ChannelCreate(channel(HashMap::new(), None).into());

    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn channel_denied_to_everyone_is_dropped_for_member() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    let deny = Some(OverrideField { a: 0, d: view() });
    let mut event = EventV1::ChannelCreate(channel(HashMap::new(), deny).into());

    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(!subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn group_create_is_unchanged() {
    let db = db().await;
    let server = server(true);
    let mut state = state("member", &server, &[]).await;
    let group = Channel::Group {
        id: "group".to_string(),
        name: "Group".to_string(),
        owner: "member".to_string(),
        description: None,
        recipients: vec!["member".to_string(), "friend".to_string()],
        icon: None,
        last_message_id: None,
        permissions: None,
        nsfw: false,
    };
    let mut event = EventV1::ChannelCreate(group.into());

    assert!(state.handle_incoming_event_v1(&db, &mut event, "member!").await);
    assert!(subscribed(&state, "group").await);
}

// ChannelUpdate after a permission change

#[tokio::test]
async fn grant_on_server_topic_opens_channel_for_role() {
    let db = db().await;
    db.insert_channel(&channel(staff_can_view(), None))
        .await
        .unwrap();
    let server = server(true);
    let mut state = state("staff-member", &server, &[STAFF]).await;
    let mut event = permissions_update(PartialChannel {
        role_permissions: Some(staff_can_view()),
        ..Default::default()
    });

    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(matches!(event, EventV1::ChannelCreate(_)));
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn grant_on_server_topic_is_dropped_for_others() {
    let db = db().await;
    db.insert_channel(&channel(staff_can_view(), None))
        .await
        .unwrap();
    let server = server(true);
    let mut state = state("member", &server, &[]).await;
    let mut event = permissions_update(PartialChannel {
        role_permissions: Some(staff_can_view()),
        ..Default::default()
    });

    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(!subscribed(&state, CHANNEL).await);
    assert!(!state.cache.channels.contains_key(CHANNEL));
}

#[tokio::test]
async fn update_reaches_viewer_once() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let change = PartialChannel {
        role_permissions: Some(staff_can_view()),
        ..Default::default()
    };

    // The server topic copy is only for members without access.
    let mut event = permissions_update(change.clone());
    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);

    let mut event = permissions_update(change);
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelUpdate { .. }));
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn revoke_removes_channel_from_member() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let change = PartialChannel {
        default_permissions: Some(OverrideField { a: 0, d: view() }),
        ..Default::default()
    };

    let mut event = permissions_update(change.clone());
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelDelete { .. }));
    assert!(!subscribed(&state, CHANNEL).await);

    // The other copy of the same change says nothing more.
    let mut event = permissions_update(change);
    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
}

#[tokio::test]
async fn revoke_on_server_topic_first_removes_channel_too() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let change = PartialChannel {
        default_permissions: Some(OverrideField { a: 0, d: view() }),
        ..Default::default()
    };

    let mut event = permissions_update(change.clone());
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(matches!(event, EventV1::ChannelDelete { .. }));
    assert!(!subscribed(&state, CHANNEL).await);

    let mut event = permissions_update(change);
    assert!(!state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
}

// Roles created after Ready

#[tokio::test]
async fn new_role_keeps_channel_after_everyone_is_denied() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let ticket = "ticket";

    // Role::create announces the whole role.
    let role = Role {
        id: ticket.to_string(),
        name: "Ticket".to_string(),
        permissions: OverrideField { a: 0, d: 0 },
        colour: None,
        hoist: false,
        rank: 2,
        icon: None,
        owner: None,
    };
    let mut event = EventV1::ServerRoleUpdate {
        id: SERVER.to_string(),
        role_id: ticket.to_string(),
        data: role.into_optional().into(),
        clear: vec![],
    };
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(state.cache.servers[SERVER].roles.contains_key(ticket));

    let mut event = permissions_update(PartialChannel {
        role_permissions: Some(HashMap::from([(
            ticket.to_string(),
            OverrideField { a: view(), d: 0 },
        )])),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelUpdate { .. }));

    let mut event = EventV1::ServerMemberUpdate {
        id: v0::MemberCompositeKey {
            server: SERVER.to_string(),
            user: "member".to_string(),
        },
        data: v0::PartialMember {
            roles: Some(vec![ticket.to_string()]),
            ..Default::default()
        },
        clear: vec![],
    };
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(subscribed(&state, CHANNEL).await);

    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(OverrideField { a: 0, d: view() }),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelUpdate { .. }));
    assert!(subscribed(&state, CHANNEL).await);
    assert!(state.cache.channels.contains_key(CHANNEL));
}

#[tokio::test]
async fn reopened_channel_can_be_closed_again() {
    let db = db().await;
    let open = OverrideField { a: view(), d: 0 };
    let closed = OverrideField { a: 0, d: view() };
    // the database already holds the opened channel when the event arrives
    db.insert_channel(&channel(HashMap::new(), Some(open.clone())))
        .await
        .unwrap();
    let server = server(false);
    let mut state = state("member", &server, &[]).await;

    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(open.clone()),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(matches!(event, EventV1::ChannelCreate(_)));

    // the channel topic copy of the same change, once subscribed
    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(open),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelUpdate { .. }));

    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(closed),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(matches!(event, EventV1::ChannelDelete { .. }));
}

// Age safety: mature channels only reach declared adults

fn mature_channel() -> Channel {
    let mut channel = channel(HashMap::new(), None);
    if let Channel::TextChannel { nsfw, .. } = &mut channel {
        *nsfw = true;
    }
    channel
}

#[tokio::test]
async fn mature_channel_is_not_followed_by_minor() {
    let server = server(false);
    let mut state = state("teen", &server, &[]).await;
    state.cache.mature_allowed = false;
    follow(&mut state, &mature_channel()).await;
    assert!(!subscribed(&state, CHANNEL).await);
    // the Server itself is still followed (names, members, no messages)
    assert!(subscribed(&state, SERVER).await);
}

#[tokio::test]
async fn mature_channel_is_followed_by_declared_adult() {
    let server = server(false);
    let mut state = state("adult", &server, &[]).await;
    state.cache.mature_allowed = true;
    follow(&mut state, &mature_channel()).await;
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn channels_of_mature_server_are_not_followed_by_minor() {
    let mut server = server(false);
    server.nsfw = true;
    let mut state = state("teen", &server, &[]).await;
    state.cache.mature_allowed = false;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    assert!(!subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn lowering_the_age_drops_mature_channels_at_once() {
    let db = db().await;
    let server = server(false);
    let mut state = state("adult", &server, &[]).await;
    state.cache.mature_allowed = true;
    follow(&mut state, &mature_channel()).await;
    assert!(subscribed(&state, CHANNEL).await);

    let mut event = EventV1::AccountAgeUpdate {
        band: v0::AgeBand::Teen,
    };
    assert!(state.handle_incoming_event_v1(&db, &mut event, "adult!").await);
    assert!(!state.cache.mature_allowed);
    assert!(!subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn marking_a_channel_mature_drops_it_for_minor() {
    let db = db().await;
    let server = server(false);
    let mut state = state("teen", &server, &[]).await;
    state.cache.mature_allowed = false;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    assert!(subscribed(&state, CHANNEL).await);

    let mut event = permissions_update(PartialChannel {
        nsfw: Some(true),
        ..Default::default()
    });
    state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await;
    assert!(!subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn mature_message_is_dropped_for_minor() {
    let db = db().await;
    let server = server(false);
    let mut state = state("teen", &server, &[]).await;
    state.cache.mature_allowed = false;
    state
        .cache
        .channels
        .insert(CHANNEL.to_string(), mature_channel());

    let message: v0::Message = serde_json::from_value(serde_json::json!({
        "_id": "message",
        "channel": CHANNEL,
        "author": OWNER,
    }))
    .expect("valid message");
    let mut event = EventV1::Message(message);
    assert!(!state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
}

// Gateway intents (bot security hardening, task 1)

const BOT: &str = "bot";
const DM: &str = "dm";

/// A bot in the server, with the given intents, that can see the channel
async fn bot_state(intents: u64) -> State {
    let server = server(false);
    let mut state = state(BOT, &server, &[]).await;
    state.cache.is_bot = true;
    state.cache.intents = intents;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    state.cache.channels.insert(
        DM.to_string(),
        Channel::DirectMessage {
            id: DM.to_string(),
            active: true,
            recipients: vec![BOT.to_string(), OWNER.to_string()],
            last_message_id: None,
        },
    );
    state
}

fn message(channel: &str, author: &str, mentions: &[&str]) -> EventV1 {
    EventV1::Message(
        serde_json::from_value(serde_json::json!({
            "_id": "message",
            "channel": channel,
            "author": author,
            "content": "secret plans",
            "embeds": [{ "type": "Text", "title": "card" }],
            "mentions": mentions,
        }))
        .expect("valid message"),
    )
}

/// What the bot receives of a message, or None when it was dropped
async fn deliver(state: &mut State, mut event: EventV1) -> Option<v0::Message> {
    let db = db().await;
    let topic = match &event {
        EventV1::Message(m) => m.channel.clone(),
        _ => CHANNEL.to_string(),
    };
    if !state.handle_incoming_event_v1(&db, &mut event, &topic).await {
        return None;
    }
    match event {
        EventV1::Message(m) => Some(m),
        _ => None,
    }
}

use v0::GatewayIntents as I;

#[tokio::test]
async fn without_message_content_a_bot_gets_no_text() {
    let mut state = bot_state(I::DEFAULT).await;
    let got = deliver(&mut state, message(CHANNEL, OWNER, &[])).await.expect("delivered");
    assert_eq!(got.content.as_deref(), Some(""));
    assert!(got.embeds.is_none());
    // who wrote it, and where, are still there
    assert_eq!(got.author, OWNER);
}

#[tokio::test]
async fn without_message_content_mentions_dms_and_own_messages_are_whole() {
    let mut state = bot_state(I::DEFAULT).await;
    let mentioned = deliver(&mut state, message(CHANNEL, OWNER, &[BOT])).await.unwrap();
    assert_eq!(mentioned.content.as_deref(), Some("secret plans"));
    let direct = deliver(&mut state, message(DM, OWNER, &[])).await.unwrap();
    assert_eq!(direct.content.as_deref(), Some("secret plans"));
    let own = deliver(&mut state, message(CHANNEL, BOT, &[])).await.unwrap();
    assert_eq!(own.content.as_deref(), Some("secret plans"));
    assert!(own.embeds.is_some());
}

#[tokio::test]
async fn with_message_content_a_bot_gets_everything() {
    let mut state = bot_state(I::DEFAULT | I::MESSAGE_CONTENT).await;
    let got = deliver(&mut state, message(CHANNEL, OWNER, &[])).await.unwrap();
    assert_eq!(got.content.as_deref(), Some("secret plans"));
}

#[tokio::test]
async fn people_always_get_message_content() {
    let server = server(false);
    let mut state = state("person", &server, &[]).await;
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let got = deliver(&mut state, message(CHANNEL, OWNER, &[])).await.unwrap();
    assert_eq!(got.content.as_deref(), Some("secret plans"));
}

#[tokio::test]
async fn without_server_messages_server_channel_messages_are_dropped() {
    let mut state = bot_state(I::DIRECT_MESSAGES | I::MESSAGE_CONTENT).await;
    assert!(deliver(&mut state, message(CHANNEL, OWNER, &[])).await.is_none());
    assert!(deliver(&mut state, message(DM, OWNER, &[])).await.is_some());
}

fn member_join(user: &str) -> EventV1 {
    #[allow(deprecated)]
    EventV1::ServerMemberJoin {
        id: SERVER.to_string(),
        user: user.to_string(),
        member: serde_json::from_value(serde_json::json!({
            "_id": { "server": SERVER, "user": user },
            "joined_at": "2026-11-02T00:00:00Z"
        }))
        .expect("valid member"),
    }
}

#[tokio::test]
async fn without_server_members_a_bot_gets_no_member_join() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT).await;
    let mut event = member_join("someone");
    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    // with the intent it does
    state.cache.intents = I::DEFAULT | I::SERVER_MEMBERS;
    let mut event = member_join("someone");
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
}

#[tokio::test]
async fn without_presence_a_bot_gets_no_online_changes() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT).await;
    let mut online = EventV1::UserUpdate {
        id: OWNER.to_string(),
        data: v0::PartialUser {
            online: Some(true),
            ..Default::default()
        },
        clear: vec![],
        event_id: None,
    };
    assert!(!state.handle_incoming_event_v1(&db, &mut online, SERVER).await);
    // other changes still arrive, without the presence part
    let mut renamed = EventV1::UserUpdate {
        id: OWNER.to_string(),
        data: v0::PartialUser {
            online: Some(true),
            display_name: Some("New name".to_string()),
            ..Default::default()
        },
        clear: vec![],
        event_id: None,
    };
    assert!(state.handle_incoming_event_v1(&db, &mut renamed, SERVER).await);
    if let EventV1::UserUpdate { data, .. } = renamed {
        assert_eq!(data.online, None);
        assert_eq!(data.display_name.as_deref(), Some("New name"));
    }
}

// Gateway intents: what each one really covers (stream bots-parity)

fn server_update() -> EventV1 {
    EventV1::ServerUpdate {
        id: SERVER.to_string(),
        data: v0::PartialServer {
            name: Some("Renamed".to_string()),
            ..Default::default()
        },
        clear: vec![],
    }
}

#[tokio::test]
async fn without_servers_a_bot_gets_no_server_events_but_keeps_its_state() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT & !I::SERVERS).await;

    let mut event = server_update();
    assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    // permission checks later on need the current server
    assert_eq!(state.cache.servers[SERVER].name, "Renamed");

    for mut event in [
        EventV1::EmojiDelete { id: "emoji".to_string() },
        EventV1::ServerRoleDelete { id: SERVER.to_string(), role_id: STAFF.to_string() },
        EventV1::WebhookDelete { id: "hook".to_string(), channel_id: CHANNEL.to_string() },
    ] {
        assert!(!state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    }

    // with the intent they arrive
    state.cache.intents = I::DEFAULT;
    let mut event = server_update();
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
}

#[tokio::test]
async fn without_servers_a_lost_channel_stays_quiet_and_is_forgotten() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT & !I::SERVERS).await;
    let mut event = EventV1::ChannelDelete { id: CHANNEL.to_string() };
    assert!(!state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(!state.cache.channels.contains_key(CHANNEL));
    assert!(!subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn people_get_server_events_whatever_the_intents_say() {
    let db = db().await;
    let server = server(false);
    let mut state = state("person", &server, &[]).await;
    state.cache.intents = 0;
    let mut event = server_update();
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
}

fn message_with_user(online: bool) -> EventV1 {
    EventV1::Message(
        serde_json::from_value(serde_json::json!({
            "_id": "message",
            "channel": CHANNEL,
            "author": OWNER,
            "user": {
                "_id": OWNER,
                "username": "owner",
                "discriminator": "0001",
                "relationship": "None",
                "online": online,
                "status": { "text": "at the dentist", "presence": "Busy" },
            },
        }))
        .expect("valid message"),
    )
}

#[tokio::test]
async fn without_presence_the_author_of_a_message_is_not_shown_online() {
    let mut state = bot_state(I::DEFAULT).await;
    let got = deliver(&mut state, message_with_user(true)).await.unwrap();
    let author = got.user.expect("author stays");
    assert!(!author.online);
    assert!(author.status.is_none());

    state.cache.intents = I::DEFAULT | I::PRESENCE;
    let got = deliver(&mut state, message_with_user(true)).await.unwrap();
    let author = got.user.unwrap();
    assert!(author.online);
    assert!(author.status.is_some());
}

#[tokio::test]
async fn without_voice_states_a_bot_gets_no_call_updates() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT & !I::VOICE_STATES).await;
    let mut event = EventV1::VoiceCallUpdate {
        initiator_id: OWNER.to_string(),
        channel_id: CHANNEL.to_string(),
        started_at: None,
        ended: false,
    };
    assert!(!state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
}

#[tokio::test]
async fn without_message_content_an_edit_of_a_mention_stays_readable() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT).await;

    let stored = |id: &str, mentions: Vec<String>| -> revolt_database::Message {
        serde_json::from_value(serde_json::json!({
            "_id": id,
            "channel": CHANNEL,
            "author": OWNER,
            "mentions": mentions,
        }))
        .expect("valid message")
    };
    db.insert_message(&stored("mentions-bot", vec![BOT.to_string()])).await.unwrap();
    db.insert_message(&stored("other", vec![])).await.unwrap();

    let edit = |id: &str| EventV1::MessageUpdate {
        id: id.to_string(),
        channel: CHANNEL.to_string(),
        data: v0::PartialMessage {
            content: Some("edited".to_string()),
            ..Default::default()
        },
        clear: vec![],
    };

    let mut event = edit("mentions-bot");
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    let EventV1::MessageUpdate { data, .. } = event else { panic!() };
    assert_eq!(data.content.as_deref(), Some("edited"));

    let mut event = edit("other");
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    let EventV1::MessageUpdate { data, .. } = event else { panic!() };
    assert_eq!(data.content.as_deref(), Some(""));
}

fn ready(channels: Vec<Channel>) -> EventV1 {
    let member = |user: &str| Member {
        id: MemberCompositeKey { server: SERVER.to_string(), user: user.to_string() },
        ..Default::default()
    };
    let user: v0::User = serde_json::from_value(serde_json::json!({
        "_id": OWNER,
        "username": "owner",
        "discriminator": "0001",
        "relationship": "None",
        "online": true,
        "status": { "text": "away", "presence": "Idle" },
    }))
    .unwrap();
    EventV1::Ready {
        users: Some(vec![user]),
        servers: Some(vec![]),
        channels: Some(channels.into_iter().map(Into::into).collect()),
        members: Some(vec![member(BOT).into(), member(OWNER).into()]),
        emojis: Some(vec![]),
        voice_states: Some(vec![]),
        user_settings: None,
        channel_unreads: None,
        policy_changes: None,
    }
}

#[tokio::test]
async fn ready_carries_only_what_the_intents_cover() {
    let state = bot_state(I::INTERACTIONS).await;
    let dm = state.cache.channels[DM].clone();
    let text = channel(HashMap::new(), None);

    let mut event = ready(vec![dm, text]);
    state.limit_ready(&mut event);
    let EventV1::Ready { users, servers, channels, members, emojis, voice_states, .. } = event
    else {
        panic!()
    };
    assert!(servers.is_none() && emojis.is_none() && voice_states.is_none());
    let channels = channels.unwrap();
    assert_eq!(channels.len(), 1, "the DM stays, the server channel goes");
    assert_eq!(channels[0].id(), DM);
    let members = members.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].id.user, BOT);
    let user = &users.unwrap()[0];
    assert!(!user.online && user.status.is_none());
}

#[tokio::test]
async fn ready_is_whole_with_every_intent_and_for_people() {
    let mut state = bot_state(I::ALL).await;
    let text = channel(HashMap::new(), None);
    let mut event = ready(vec![text.clone()]);
    state.limit_ready(&mut event);
    let EventV1::Ready { users, servers, members, .. } = event else { panic!() };
    assert!(servers.is_some());
    assert_eq!(members.unwrap().len(), 2);
    assert!(users.unwrap()[0].online);

    state.cache.is_bot = false;
    state.cache.intents = 0;
    let mut event = ready(vec![text]);
    state.limit_ready(&mut event);
    let EventV1::Ready { servers, members, .. } = event else { panic!() };
    assert!(servers.is_some());
    assert_eq!(members.unwrap().len(), 2);
}

#[tokio::test]
async fn without_presence_the_people_in_an_interaction_are_not_shown_online() {
    let db = db().await;
    let person = serde_json::json!({
        "_id": OWNER,
        "username": "owner",
        "discriminator": "0001",
        "relationship": "None",
        "online": true,
        "status": { "text": "away" },
    });
    let interaction = serde_json::json!({
        "_id": "interaction",
        "version": 1,
        "application_id": BOT,
        "type": "Command",
        "user": person,
        "channel_id": CHANNEL,
        "user_permissions": 0,
        "app_permissions": 0,
        "command": {
            "id": "command",
            "name": "who",
            "type": "ChatInput",
            "version": "1",
            "options": [],
            "resolved": { "users": { OWNER: person } },
        },
        "respond_by": "2026-11-04T00:00:00Z",
        "expires_at": "2026-11-04T00:00:00Z",
    });
    let mut event = EventV1::InteractionCreate {
        interaction: serde_json::from_value(interaction).expect("valid interaction"),
    };
    let mut state = bot_state(I::DEFAULT).await;
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    let EventV1::InteractionCreate { interaction } = event else { panic!() };
    assert!(!interaction.user.online && interaction.user.status.is_none());
    let resolved = interaction.command.unwrap().resolved.unwrap();
    assert!(!resolved.users[OWNER].online);
}

/// A user select and a form carry people too
#[tokio::test]
async fn without_presence_people_picked_in_selects_and_forms_are_not_shown_online() {
    let db = db().await;
    let person = serde_json::json!({
        "_id": OWNER,
        "username": "owner",
        "discriminator": "0001",
        "relationship": "None",
        "online": true,
        "status": { "text": "away" },
    });
    let base = serde_json::json!({
        "_id": "interaction",
        "version": 1,
        "application_id": BOT,
        "type": "Component",
        "user": person,
        "channel_id": CHANNEL,
        "user_permissions": 0,
        "app_permissions": 0,
        "respond_by": "2026-11-04T00:00:00Z",
        "expires_at": "2026-11-04T00:00:00Z",
    });
    let picked = serde_json::json!({ "users": { OWNER: person } });

    let mut select = base.clone();
    select["component"] = serde_json::json!({
        "custom_id": "who",
        "component_type": "UserSelect",
        "values": [OWNER],
        "resolved": picked,
    });
    let mut form = base;
    form["type"] = serde_json::json!("ModalSubmit");
    form["modal"] = serde_json::json!({
        "custom_id": "form",
        "fields": [],
        "resolved": picked,
    });

    let mut state = bot_state(I::DEFAULT).await;
    for interaction in [select, form] {
        let mut event = EventV1::InteractionCreate {
            interaction: serde_json::from_value(interaction).expect("valid interaction"),
        };
        assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
        let EventV1::InteractionCreate { interaction } = event else { panic!() };
        let resolved = interaction
            .component
            .and_then(|data| data.resolved)
            .or(interaction.modal.and_then(|data| data.resolved))
            .expect("people were picked");
        assert!(!resolved.users[OWNER].online);
        assert!(resolved.users[OWNER].status.is_none());
    }
}

// (d) only server channels belong to the Servers intent

#[tokio::test]
async fn without_servers_dm_and_group_channels_still_arrive() {
    let db = db().await;
    let mut state = bot_state(I::DEFAULT & !I::SERVERS).await;

    let mut update = EventV1::ChannelUpdate {
        id: DM.to_string(),
        data: v0::PartialChannel {
            active: Some(true),
            ..Default::default()
        },
        clear: vec![],
    };
    assert!(state.handle_incoming_event_v1(&db, &mut update, DM).await);

    let mut joined = EventV1::ChannelGroupJoin {
        id: DM.to_string(),
        user: OWNER.to_string(),
    };
    assert!(state.handle_incoming_event_v1(&db, &mut joined, DM).await);

    let group: v0::Channel = serde_json::from_value(serde_json::json!({
        "channel_type": "Group",
        "_id": "group",
        "name": "friends",
        "owner": OWNER,
        "recipients": [BOT, OWNER],
    }))
    .expect("valid group");
    let mut created = EventV1::ChannelCreate(group);
    assert!(state.handle_incoming_event_v1(&db, &mut created, "bot!").await);

    let mut deleted = EventV1::ChannelDelete { id: DM.to_string() };
    assert!(state.handle_incoming_event_v1(&db, &mut deleted, DM).await);
}

// (e) voice participants are the Voice States intent's

#[tokio::test]
async fn without_voice_states_ready_looks_up_no_voice_participants() {
    let fields = revolt_database::events::client::ReadyPayloadFields::default();
    let state = bot_state(I::DEFAULT & !I::VOICE_STATES).await;
    assert!(!state.wants_voice_states(&fields));
    let state = bot_state(I::DEFAULT).await;
    assert!(state.wants_voice_states(&fields));
}

fn webhook_event(channel: &str) -> EventV1 {
    EventV1::WebhookCreate(
        serde_json::from_value(serde_json::json!({
            "id": "W", "name": "Deploys", "creator_id": OWNER, "channel_id": channel, "permissions": 0
        }))
        .unwrap(),
    )
}

async fn webhook_events(state: &mut State, db: &Database, channel: &str) -> [bool; 3] {
    let events = [
        webhook_event(channel),
        EventV1::WebhookUpdate {
            id: "W".to_string(),
            channel_id: channel.to_string(),
            data: Default::default(),
            remove: vec![],
        },
        EventV1::WebhookDelete {
            id: "W".to_string(),
            channel_id: channel.to_string(),
        },
    ];
    let mut seen = [false; 3];
    for (index, mut event) in events.into_iter().enumerate() {
        seen[index] = state.handle_incoming_event_v1(db, &mut event, channel).await;
    }
    seen
}

#[tokio::test]
async fn webhook_events_are_for_managers_only() {
    let db = db().await;
    let server = server(false);
    let everyone_sees = channel(HashMap::new(), None);
    db.insert_channel(&everyone_sees).await.unwrap();

    // reads the channel, does not manage it
    let mut reader = state("reader", &server, &[]).await;
    follow(&mut reader, &everyone_sees).await;
    assert_eq!(webhook_events(&mut reader, &db, CHANNEL).await, [false; 3]);

    let mut owner = state(OWNER, &server, &[]).await;
    follow(&mut owner, &everyone_sees).await;
    assert_eq!(webhook_events(&mut owner, &db, CHANNEL).await, [true; 3]);
}

// A channel made private, then reopened by a role change (owner report 2026-11-09)

/// The bulk the gateway forwards after a role change, as event type names
fn created_in(event: &EventV1) -> bool {
    match event {
        EventV1::ChannelCreate(_) => true,
        EventV1::Bulk { v } => v.iter().any(created_in),
        _ => false,
    }
}

fn member_roles(roles: &[&str]) -> EventV1 {
    EventV1::ServerMemberUpdate {
        id: v0::MemberCompositeKey {
            server: SERVER.to_string(),
            user: "member".to_string(),
        },
        data: v0::PartialMember {
            roles: Some(roles.iter().map(|r| r.to_string()).collect()),
            ..Default::default()
        },
        clear: vec![],
    }
}

/// Everyone sees the channel, then @everyone is denied: the member's
/// connection drops it, as it would never have got it on Ready.
async fn made_private(db: &Database, state: &mut State) {
    let deny = OverrideField { a: 0, d: view() };
    db.insert_channel(&channel(staff_can_view(), Some(deny.clone())))
        .await
        .unwrap();
    follow(state, &channel(staff_can_view(), None)).await;
    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(deny),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelDelete { .. }));
    assert!(!state.cache.channels.contains_key(CHANNEL));
}

#[tokio::test]
async fn private_channel_arrives_when_role_is_given() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    made_private(&db, &mut state).await;

    let mut event = member_roles(&[STAFF]);
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(created_in(&event), "no ChannelCreate in {event:?}");
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn private_channel_arrives_when_role_gains_view() {
    let db = db().await;
    let server = server(false);
    // this role's channel override is not there: it gets View server-wide
    let mut state = state("member", &server, &[STAFF]).await;
    let deny = OverrideField { a: 0, d: view() };
    db.insert_channel(&channel(HashMap::new(), Some(deny.clone())))
        .await
        .unwrap();
    follow(&mut state, &channel(HashMap::new(), None)).await;
    let mut event = permissions_update(PartialChannel {
        default_permissions: Some(deny),
        ..Default::default()
    });
    assert!(state.handle_incoming_event_v1(&db, &mut event, CHANNEL).await);
    assert!(matches!(event, EventV1::ChannelDelete { .. }));

    let mut event = EventV1::ServerRoleUpdate {
        id: SERVER.to_string(),
        role_id: STAFF.to_string(),
        data: v0::PartialRole {
            permissions: Some(OverrideField { a: view(), d: 0 }),
            ..Default::default()
        },
        clear: vec![],
    };
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(created_in(&event), "no ChannelCreate in {event:?}");
    assert!(subscribed(&state, CHANNEL).await);
}

#[tokio::test]
async fn private_channel_leaves_and_returns_with_the_role() {
    let db = db().await;
    let server = server(false);
    let mut state = state("member", &server, &[]).await;
    made_private(&db, &mut state).await;

    let mut event = member_roles(&[STAFF]);
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(created_in(&event));

    let mut event = member_roles(&[]);
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(!subscribed(&state, CHANNEL).await);
    assert!(!state.cache.channels.contains_key(CHANNEL));

    let mut event = member_roles(&[STAFF]);
    assert!(state.handle_incoming_event_v1(&db, &mut event, SERVER).await);
    assert!(created_in(&event), "no ChannelCreate in {event:?}");
}
