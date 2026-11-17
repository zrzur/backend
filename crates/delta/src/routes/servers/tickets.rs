use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use revolt_config::config;
use revolt_database::{
    events::client::EventV1,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, PartialChannel, PartialServer, Server, SystemMessage, TicketInformation,
    User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, OverrideField};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;
use validator::Validate;

/// What the opener and the ticket staff may do in a ticket
const TALK: i64 = ChannelPermission::ViewChannel as i64
    | ChannelPermission::ReadMessageHistory as i64
    | ChannelPermission::SendMessage as i64
    | ChannelPermission::SendEmbeds as i64
    | ChannelPermission::UploadFiles as i64
    | ChannelPermission::React as i64;

/// A member opens at most this many tickets an hour in one server
const TICKETS_AN_HOUR: usize = 3;

fn ticket_name(number: u32, closed: bool) -> String {
    format!("{}-{number:04}", if closed { "closed" } else { "ticket" })
}

/// Ticket settings as a manager sends them: the category and the roles are
/// this server's, the first message is at most 500 characters, and the count
/// of opened tickets keeps its stored value
pub fn check_settings(tickets: &mut v0::ServerTickets, server: &Server) -> Result<()> {
    if let Some(category) = &tickets.category {
        let known = server
            .categories
            .as_ref()
            .is_some_and(|categories| categories.iter().any(|c| &c.id == category));
        if !known {
            return Err(create_error!(NotFound));
        }
    }

    let mut seen = HashSet::new();
    tickets.staff_roles.retain(|role| seen.insert(role.clone()));
    if tickets.staff_roles.len() > 10 {
        return Err(create_error!(FailedValidation {
            error: "tickets have at most 10 staff roles".to_string()
        }));
    }
    if tickets
        .staff_roles
        .iter()
        .any(|role| !server.roles.contains_key(role))
    {
        return Err(create_error!(InvalidRole));
    }

    tickets.message = tickets
        .message
        .take()
        .map(|message| message.trim().to_string())
        .filter(|message| !message.is_empty());
    if tickets
        .message
        .as_ref()
        .is_some_and(|message| message.chars().count() > 500)
    {
        return Err(create_error!(FailedValidation {
            error: "the first message of a ticket is at most 500 characters".to_string()
        }));
    }

    tickets.opened = server.tickets.as_ref().map_or(0, |stored| stored.opened);
    Ok(())
}

/// # Open a Ticket
///
/// Opens a private channel between you and the server's ticket staff, when
/// the server has tickets switched on. Only you, the ticket staff roles and
/// the people who see every channel can see it.
///
/// A member has one open ticket per server: asking again returns it. At most
/// 3 tickets an hour. Not for bots.
#[openapi(tag = "Server Information")]
#[post("/<target>/tickets", data = "<data>")]
pub async fn open(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataOpenServerTicket>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    let mut server = target.as_server(db).await?;
    // only members; to anyone else the server does not exist
    if db.fetch_member(&server.id, &user.id).await.is_err() {
        return Err(create_error!(NotFound));
    }
    let Some(settings) = server.tickets.clone().filter(|tickets| tickets.enabled) else {
        return Err(create_error!(InvalidOperation));
    };

    // the ticket they have open is handed back
    let mut recent = Vec::new();
    for channel in db.fetch_channels(&server.channels).await? {
        let Channel::TextChannel {
            id,
            ticket: Some(ticket),
            ..
        } = &channel
        else {
            continue;
        };
        if ticket.opener != user.id {
            continue;
        }
        if !ticket.closed {
            return Ok(Json(channel.into()));
        }
        if let Ok(id) = Ulid::from_string(id) {
            recent.push(id.datetime());
        }
    }
    let hour = Duration::from_secs(3600);
    recent.retain(|opened| {
        SystemTime::now()
            .duration_since(*opened)
            .is_ok_and(|age| age < hour)
    });
    if recent.len() >= TICKETS_AN_HOUR {
        recent.sort();
        let wait = recent
            .first()
            .and_then(|oldest| (*oldest + hour).duration_since(SystemTime::now()).ok())
            .unwrap_or(hour);
        return Err(create_error!(RateLimited {
            retry_after: wait.as_millis() as u64,
            global: false,
        }));
    }

    let limit = config().await.features.limits.global.server_channels;
    if server.channels.len() >= limit {
        return Err(create_error!(TooManyChannels { max: limit }));
    }

    let number = settings.opened + 1;
    let id = Ulid::new().to_string();
    let allow = OverrideField { a: TALK, d: 0 };
    let channel = Channel::TextChannel {
        id: id.clone(),
        server: server.id.clone(),
        name: ticket_name(number, false),
        description: data.subject.map(|subject| subject.trim().to_string()).filter(|s| !s.is_empty()),
        icon: None,
        last_message_id: None,
        // hidden from everyone; the staff roles and the opener are let in
        default_permissions: Some(OverrideField {
            a: 0,
            d: ChannelPermission::ViewChannel as i64,
        }),
        role_permissions: settings
            .staff_roles
            .iter()
            .filter(|role| server.roles.contains_key(*role))
            .map(|role| (role.clone(), allow))
            .collect(),
        member_permissions: HashMap::from([(user.id.clone(), allow)]),
        nsfw: false,
        voice: None,
        slowmode: None,
        announcement: false,
        permissions_synced: false,
        parent: None,
        thread: None,
        forum: None,
        ticket: Some(TicketInformation {
            opener: user.id.clone(),
            number,
            closed: false,
        }),
    };
    db.insert_channel(&channel).await?;

    let mut channels = server.channels.clone();
    channels.push(id.clone());
    let categories = settings.category.as_ref().and_then(|category| {
        let mut categories = server.categories.clone()?;
        categories
            .iter_mut()
            .find(|c| &c.id == category)?
            .channels
            .push(id.clone());
        Some(categories)
    });
    server
        .update(
            db,
            PartialServer {
                channels: Some(channels),
                categories,
                tickets: Some(v0::ServerTickets {
                    opened: number,
                    ..settings.clone()
                }),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    EventV1::ChannelCreate(channel.clone().into())
        .p(server.id.clone())
        .await;

    // the server's first words in the ticket. Best effort: the ticket is open
    // either way.
    if let Some(content) = settings.message {
        let sent = SystemMessage::Text { content }
            .into_message(id)
            .send(
                db,
                Some(amqp),
                v0::MessageAuthor::System {
                    username: &server.name,
                    avatar: server.icon.as_ref().map(|file| file.id.as_ref()),
                },
                None,
                None,
                &channel,
                false,
            )
            .await;
        if let Err(error) = sent {
            log::warn!("first message of ticket {}: {error:?}", channel.id());
        }
    }

    Ok(Json(channel.into()))
}

/// A ticket of this server, and whether the user is its opener. Anyone else
/// who can see the ticket is ticket staff.
async fn ticket_of(
    db: &Database,
    user: &User,
    server: &Server,
    channel: Reference<'_>,
) -> Result<(Channel, TicketInformation, bool)> {
    let channel = channel.as_channel(db).await?;
    let Channel::TextChannel {
        server: channel_server,
        ticket: Some(ticket),
        ..
    } = &channel
    else {
        return Err(create_error!(NotFound));
    };
    if channel_server != &server.id {
        return Err(create_error!(NotFound));
    }
    let ticket = ticket.clone();

    let mut query = DatabasePermissionQuery::new(db, user).channel(&channel);
    if !calculate_channel_permissions(&mut query)
        .await
        .has_channel_permission(ChannelPermission::ViewChannel)
    {
        return Err(create_error!(NotFound));
    }
    let opener = ticket.opener == user.id;
    Ok((channel, ticket, opener))
}

/// # Close a Ticket
///
/// Closes a ticket: its opener no longer sees it, the staff keep it and can
/// reopen or delete it. For the opener and the ticket staff.
#[openapi(tag = "Server Information")]
#[post("/<target>/tickets/<channel>/close")]
pub async fn close(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    channel: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    let server = target.as_server(db).await?;
    let (mut channel, mut ticket, _) = ticket_of(db, &user, &server, channel).await?;
    if ticket.closed {
        return Ok(Json(channel.into()));
    }

    ticket.closed = true;
    channel
        .set_member_permission(db, &ticket.opener, OverrideField { a: 0, d: 0 })
        .await?;
    channel
        .update(
            db,
            PartialChannel {
                name: Some(ticket_name(ticket.number, true)),
                ticket: Some(ticket),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    Ok(Json(channel.into()))
}

/// # Reopen a Ticket
///
/// Opens a closed ticket again: its opener sees it again. For the ticket
/// staff, while the opener is still a member and has no other open ticket.
#[openapi(tag = "Server Information")]
#[post("/<target>/tickets/<channel>/reopen")]
pub async fn reopen(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    channel: Reference<'_>,
) -> Result<Json<v0::Channel>> {
    let server = target.as_server(db).await?;
    let (mut channel, mut ticket, _) = ticket_of(db, &user, &server, channel).await?;
    if !ticket.closed {
        return Ok(Json(channel.into()));
    }

    if db.fetch_member(&server.id, &ticket.opener).await.is_err() {
        return Err(create_error!(NotFound));
    }
    for other in db.fetch_channels(&server.channels).await? {
        if let Channel::TextChannel {
            ticket: Some(open), ..
        } = &other
        {
            if open.opener == ticket.opener && !open.closed {
                return Err(create_error!(InvalidOperation));
            }
        }
    }

    ticket.closed = false;
    channel
        .set_member_permission(db, &ticket.opener, OverrideField { a: TALK, d: 0 })
        .await?;
    channel
        .update(
            db,
            PartialChannel {
                name: Some(ticket_name(ticket.number, false)),
                ticket: Some(ticket),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    Ok(Json(channel.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        util::permissions::DatabasePermissionQuery, Channel, Member, PartialMember, Role, Server,
        Session, User,
    };
    use revolt_models::v0;
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
    use rocket::http::{ContentType, Header, Status};

    struct Setup {
        harness: TestHarness,
        server: Server,
        owner: Session,
        member: User,
        member_session: Session,
        staff: User,
        staff_session: Session,
        staff_role: Role,
        other: User,
        other_session: Session,
    }

    async fn join(harness: &TestHarness, server: &Server, user: &User, role: Option<&Role>) {
        let (mut member, _) = Member::create(&harness.db, server, user, None).await.unwrap();
        if let Some(role) = role {
            member
                .update(
                    &harness.db,
                    PartialMember { roles: Some(vec![role.id.clone()]), ..Default::default() },
                    vec![],
                )
                .await
                .unwrap();
        }
    }

    /// A server with an owner, a staff role and its holder, and two plain members
    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, owner, owner_user) = harness.new_user().await;
        let (_, member_session, member) = harness.new_user().await;
        let (_, staff_session, staff) = harness.new_user().await;
        let (_, other_session, other) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner_user).await;
        let staff_role = harness.new_role(&server, 0, None).await;
        let server = harness.db.fetch_server(&server.id).await.unwrap();
        join(&harness, &server, &member, None).await;
        join(&harness, &server, &staff, Some(&staff_role)).await;
        join(&harness, &server, &other, None).await;
        Setup {
            harness,
            server,
            owner,
            member,
            member_session,
            staff,
            staff_session,
            staff_role,
            other,
            other_session,
        }
    }

    async fn settings(s: &Setup, session: &Session, tickets: serde_json::Value) -> (Status, String) {
        let response = s
            .harness
            .client
            .patch(format!("/servers/{}", s.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(json!({ "tickets": tickets }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn switch_on(s: &Setup) {
        let (status, body) = settings(
            s,
            &s.owner,
            json!({ "enabled": true, "staff_roles": [s.staff_role.id], "message": "  How can we help?  " }),
        )
        .await;
        assert_eq!(status, Status::Ok, "{body}");
    }

    async fn post(s: &Setup, session: &Session, path: &str, body: serde_json::Value) -> (Status, String) {
        let response = s
            .harness
            .client
            .post(format!("/servers/{}/tickets{path}", s.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(body.to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    async fn open(s: &Setup, session: &Session) -> Channel {
        let (status, body) = post(s, session, "", json!({ "subject": "My role is missing" })).await;
        assert_eq!(status, Status::Ok, "{body}");
        let channel: v0::Channel = serde_json::from_str(&body).unwrap();
        channel.into()
    }

    async fn can(s: &Setup, user: &User, channel: &Channel, permission: ChannelPermission) -> bool {
        let channel = s.harness.db.fetch_channel(channel.id()).await.unwrap();
        let mut query = DatabasePermissionQuery::new(&s.harness.db, user).channel(&channel);
        calculate_channel_permissions(&mut query)
            .await
            .has_channel_permission(permission)
    }

    #[rocket::async_test]
    async fn no_tickets_until_a_manager_switches_them_on() {
        let s = setup().await;
        let (status, body) = post(&s, &s.member_session, "", json!({})).await;
        assert_eq!(status, Status::BadRequest, "{body}");

        // a member cannot change the settings
        let (status, body) = settings(&s, &s.member_session, json!({ "enabled": true })).await;
        assert_eq!(status, Status::Forbidden, "{body}");

        switch_on(&s).await;
        let stored = s.harness.db.fetch_server(&s.server.id).await.unwrap();
        let tickets = stored.tickets.unwrap();
        assert!(tickets.enabled);
        assert_eq!(tickets.message.as_deref(), Some("How can we help?"));
        assert_eq!(tickets.opened, 0);
    }

    #[rocket::async_test]
    async fn settings_point_only_at_this_server() {
        let s = setup().await;
        let (status, body) = settings(&s, &s.owner, json!({ "enabled": true, "staff_roles": ["NOROLE"] })).await;
        assert_ne!(status, Status::Ok, "{body}");
        assert!(body.contains("InvalidRole"), "{body}");

        let (status, body) = settings(&s, &s.owner, json!({ "enabled": true, "category": "NOCATEGORY" })).await;
        assert_eq!(status, Status::NotFound, "{body}");

        let (status, body) = settings(&s, &s.owner, json!({ "enabled": true, "message": "x".repeat(501) })).await;
        assert_eq!(status, Status::BadRequest, "{body}");

        // the count of opened tickets is not a manager's to set
        let (status, body) = settings(&s, &s.owner, json!({ "enabled": true, "opened": 40 })).await;
        assert_eq!(status, Status::Ok, "{body}");
        let stored = s.harness.db.fetch_server(&s.server.id).await.unwrap();
        assert_eq!(stored.tickets.unwrap().opened, 0);
    }

    #[rocket::async_test]
    async fn a_ticket_is_seen_by_its_opener_and_the_staff_only() {
        let s = setup().await;
        switch_on(&s).await;

        let ticket = open(&s, &s.member_session).await;
        let Channel::TextChannel { name, description, ticket: Some(state), .. } = &ticket else {
            panic!("not a ticket");
        };
        assert_eq!(name, "ticket-0001");
        assert_eq!(description.as_deref(), Some("My role is missing"));
        assert_eq!(state.opener, s.member.id);
        assert_eq!(state.number, 1);
        assert!(!state.closed);

        for (user, sees) in [(&s.member, true), (&s.staff, true), (&s.other, false)] {
            assert_eq!(can(&s, user, &ticket, ChannelPermission::ViewChannel).await, sees);
            assert_eq!(can(&s, user, &ticket, ChannelPermission::SendMessage).await, sees);
        }
        // the opener does not manage the channel
        assert!(!can(&s, &s.member, &ticket, ChannelPermission::ManageChannel).await);

        // the server's first words are in it
        let response = s
            .harness
            .client
            .get(format!("/channels/{}/messages", ticket.id()))
            .header(Header::new("x-session-token", s.staff_session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let messages = response.into_string().await.unwrap();
        assert!(messages.contains("How can we help?"), "{messages}");

        // it is in the server, and counted
        let server = s.harness.db.fetch_server(&s.server.id).await.unwrap();
        assert!(server.channels.iter().any(|id| id == ticket.id()));
        assert_eq!(server.tickets.unwrap().opened, 1);
    }

    #[rocket::async_test]
    async fn one_open_ticket_per_member_and_numbers_go_up() {
        let s = setup().await;
        switch_on(&s).await;

        let first = open(&s, &s.member_session).await;
        let again = open(&s, &s.member_session).await;
        assert_eq!(first.id(), again.id());

        let second = open(&s, &s.other_session).await;
        assert!(matches!(&second, Channel::TextChannel { name, .. } if name == "ticket-0002"));
    }

    #[rocket::async_test]
    async fn closing_takes_the_opener_out_and_the_staff_can_reopen() {
        let s = setup().await;
        switch_on(&s).await;
        let ticket = open(&s, &s.member_session).await;
        let path = |action: &str| format!("/{}/{action}", ticket.id());

        // someone who cannot see the ticket cannot close it
        let (status, body) = post(&s, &s.other_session, &path("close"), json!({})).await;
        assert_eq!(status, Status::NotFound, "{body}");

        // its opener closes it
        let (status, body) = post(&s, &s.member_session, &path("close"), json!({})).await;
        assert_eq!(status, Status::Ok, "{body}");
        let stored = s.harness.db.fetch_channel(ticket.id()).await.unwrap();
        assert!(matches!(&stored, Channel::TextChannel { name, ticket: Some(state), .. }
            if name == "closed-0001" && state.closed));
        assert!(!can(&s, &s.member, &ticket, ChannelPermission::ViewChannel).await);
        assert!(can(&s, &s.staff, &ticket, ChannelPermission::ViewChannel).await);

        // the opener cannot reopen what they no longer see; the staff can
        let (status, body) = post(&s, &s.member_session, &path("reopen"), json!({})).await;
        assert_eq!(status, Status::NotFound, "{body}");
        let (status, body) = post(&s, &s.staff_session, &path("reopen"), json!({})).await;
        assert_eq!(status, Status::Ok, "{body}");
        let stored = s.harness.db.fetch_channel(ticket.id()).await.unwrap();
        assert!(matches!(&stored, Channel::TextChannel { name, ticket: Some(state), .. }
            if name == "ticket-0001" && !state.closed));
        assert!(can(&s, &s.member, &ticket, ChannelPermission::SendMessage).await);

        // the staff close it too
        let (status, body) = post(&s, &s.staff_session, &path("close"), json!({})).await;
        assert_eq!(status, Status::Ok, "{body}");
        assert!(!can(&s, &s.member, &ticket, ChannelPermission::ViewChannel).await);

        // a closed ticket is no reason to refuse a new one, and the old one
        // then stays closed
        let next = open(&s, &s.member_session).await;
        assert_ne!(next.id(), ticket.id());
        let (status, body) = post(&s, &s.staff_session, &path("reopen"), json!({})).await;
        assert_eq!(status, Status::BadRequest, "{body}");
    }

    #[rocket::async_test]
    async fn a_member_opens_three_tickets_an_hour() {
        let s = setup().await;
        switch_on(&s).await;
        for _ in 0..3 {
            let ticket = open(&s, &s.member_session).await;
            let (status, body) =
                post(&s, &s.member_session, &format!("/{}/close", ticket.id()), json!({})).await;
            assert_eq!(status, Status::Ok, "{body}");
        }
        let (status, body) = post(&s, &s.member_session, "", json!({})).await;
        assert_eq!(status, Status::TooManyRequests, "{body}");
    }

    #[rocket::async_test]
    async fn outsiders_and_plain_channels_are_refused() {
        let s = setup().await;
        switch_on(&s).await;
        let (_, outsider, _) = s.harness.new_user().await;
        let (status, body) = post(&s, &outsider, "", json!({})).await;
        assert_eq!(status, Status::NotFound, "{body}");

        let plain = s.harness.new_channel(&s.server).await;
        let (status, body) =
            post(&s, &s.owner, &format!("/{}/close", plain.id()), json!({})).await;
        assert_eq!(status, Status::NotFound, "{body}");
    }
}
