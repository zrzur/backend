//! What a server does when someone joins: roles given to everyone, and a
//! welcome message.
//! GET /servers/<target>/on_join, PUT /servers/<target>/on_join
//!
//! Both are applied where every join ends (`Member::create_from`): an invite,
//! Discover, an approved request, the short path from an invite link. Bots
//! are left out of both. The roles are given at the moment of joining, so an
//! Approval server gives them on approval, and they sit beside the roles the
//! lobby's questions hand out. A role given this way never lifts the server's
//! verification level (`util::verification`).
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, PartialServer, Server, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// Roles given to everyone at most
pub const MAX_ROLES: usize = 5;
/// Longest welcome message, in characters (placeholders as typed)
pub const MAX_WELCOME: usize = 1000;

/// Permissions that let their holder change the server or act on other
/// members: giving one to everyone who joins needs `confirm_powerful_roles`
const POWERFUL: &[ChannelPermission] = &[
    ChannelPermission::ManageChannel,
    ChannelPermission::ManageServer,
    ChannelPermission::ManagePermissions,
    ChannelPermission::ManageRole,
    ChannelPermission::ManageCustomisation,
    ChannelPermission::KickMembers,
    ChannelPermission::BanMembers,
    ChannelPermission::TimeoutMembers,
    ChannelPermission::AssignRoles,
    ChannelPermission::ManageNicknames,
    ChannelPermission::RemoveAvatars,
    ChannelPermission::ManageMessages,
    ChannelPermission::ManageWebhooks,
    ChannelPermission::MentionEveryone,
    ChannelPermission::MentionRoles,
    ChannelPermission::MuteMembers,
    ChannelPermission::DeafenMembers,
    ChannelPermission::MoveMembers,
    ChannelPermission::ViewAuditLogs,
    ChannelPermission::ManageJoinRequests,
];

/// Whether a role allows any of the powerful permissions
pub fn is_powerful(allow: i64) -> bool {
    POWERFUL.iter().any(|permission| allow & (*permission as i64) != 0)
}

/// What the caller may do here: (manage roles, manage the server, own rank)
async fn rights(db: &Database, user: &User, server: &Server) -> Result<(bool, bool, i64)> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    // people outside the server learn nothing about it
    if user.id != server.owner && !query.are_we_a_member().await {
        return Err(create_error!(NotFound));
    }
    let permissions = calculate_server_permissions(&mut query).await;
    let roles = permissions.has_channel_permission(ChannelPermission::ManageRole);
    let manage = permissions.has_channel_permission(ChannelPermission::ManageServer);
    if !roles && !manage {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    }
    Ok((roles, manage, query.get_member_rank().unwrap_or(i64::MAX)))
}

/// # Fetch On-Join Settings
///
/// The roles given to everyone who joins and the welcome message. For
/// people who may manage roles or manage the server.
#[openapi(tag = "Server Information")]
#[get("/<target>/on_join")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::ServerOnJoin>> {
    let server = target.as_server(db).await?;
    rights(db, &user, &server).await?;
    Ok(Json(server.on_join.unwrap_or_default()))
}

/// # Set On-Join Settings
///
/// Replace the roles given to everyone who joins and the welcome message.
///
/// Roles: needs Manage Roles; at most five; only roles of this server that
/// no bot owns; a role at or above the caller's own rank can be neither
/// added nor taken off (the owner is not limited); a role that can manage
/// the server or act on members is refused unless `confirm_powerful_roles`
/// is true.
///
/// Welcome message: needs Manage Server; a text channel of this server and
/// 1 to 1000 characters. `{member}` becomes a mention of the person and
/// `{server}` the server's name; a channel is written `<#id>`. It is posted
/// by the system, without notifications, at most ten times a minute per
/// server. Leave `welcome` out to switch it off.
#[openapi(tag = "Server Information")]
#[put("/<target>/on_join", data = "<data>")]
pub async fn set(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditServerOnJoin>,
) -> Result<Json<v0::ServerOnJoin>> {
    let data = data.into_inner();
    let server = target.as_server(db).await?;
    let (may_roles, may_manage, rank) = rights(db, &user, &server).await?;
    let stored = server.on_join.clone().unwrap_or_default();

    // roles: no doubles, in the order given
    let mut roles: Vec<String> = Vec::new();
    for id in data.roles {
        if !roles.contains(&id) {
            roles.push(id);
        }
    }
    // a role deleted since it was chosen is dropped, never an error
    let kept: Vec<String> = stored
        .roles
        .iter()
        .filter(|id| server.roles.contains_key(*id))
        .cloned()
        .collect();

    if roles != kept {
        if !may_roles {
            return Err(create_error!(MissingPermission {
                permission: ChannelPermission::ManageRole.to_string()
            }));
        }
        if roles.len() > MAX_ROLES {
            return Err(create_error!(TooManyRoles { max: MAX_ROLES }));
        }

        let is_owner = user.id == server.owner;
        let mut powerful = false;
        for id in &roles {
            let Some(role) = server.roles.get(id) else {
                return Err(create_error!(NotFound));
            };
            // a bot's own role belongs to that bot
            if role.owner.is_some() {
                return Err(create_error!(InvalidOperation));
            }
            powerful |= is_powerful(role.permissions.a);
        }
        // adding or taking off a role is a decision about that role: not
        // for a role at or above one's own
        for id in roles.iter().filter(|id| !kept.contains(id)).chain(kept.iter().filter(|id| !roles.contains(id))) {
            let role_rank = server.roles.get(id).map(|role| role.rank).unwrap_or(i64::MAX);
            if !is_owner && role_rank <= rank {
                return Err(create_error!(NotElevated));
            }
        }
        if powerful && !data.confirm_powerful_roles {
            return Err(create_error!(IncorrectData {
                with: "confirm_powerful_roles".to_string()
            }));
        }
    }

    let welcome = match data.welcome {
        Some(mut welcome) => {
            welcome.content = welcome.content.trim().to_string();
            Some(welcome)
        }
        None => None,
    };
    if welcome != stored.welcome {
        if !may_manage {
            return Err(create_error!(MissingPermission {
                permission: ChannelPermission::ManageServer.to_string()
            }));
        }
        if let Some(welcome) = &welcome {
            let length = welcome.content.chars().count();
            if length == 0 || length > MAX_WELCOME {
                return Err(create_error!(IncorrectData {
                    with: "welcome.content".to_string()
                }));
            }
            if !server.channels.contains(&welcome.channel) {
                return Err(create_error!(NotFound));
            }
            match db.fetch_channel(&welcome.channel).await? {
                Channel::TextChannel { voice: None, .. } => {}
                _ => return Err(create_error!(InvalidOperation)),
            }
        }
    }

    let on_join = v0::ServerOnJoin { roles, welcome };
    db.update_server(
        &server.id,
        &PartialServer {
            on_join: Some(on_join.clone()),
            ..Default::default()
        },
        vec![],
    )
    .await?;

    Ok(Json(on_join))
}

#[cfg(test)]
mod test {
    use super::is_powerful;
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{
        BotInformation, Channel, Member, MemberCompositeKey, PartialMember, PartialServer,
        PartialUser, Role, Server, Session, User,
    };
    use revolt_models::v0::{self, DataCreateInvite, Invite};
    use revolt_permissions::ChannelPermission;
    use rocket::http::{ContentType, Header, Status};
    use rocket::local::asynchronous::LocalResponse;
    use serde_json::{json, Value};

    struct Setup {
        harness: TestHarness,
        owner: Session,
        owner_user: User,
        server: Server,
        channels: Vec<Channel>,
    }

    async fn setup() -> Setup {
        let harness = TestHarness::new().await;
        let (_, owner, owner_user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner_user).await;
        Setup { harness, owner, owner_user, server, channels }
    }

    impl Setup {
        async fn put<'a>(&'a self, token: &str, body: Value) -> LocalResponse<'a> {
            self.harness
                .client
                .put(format!("/servers/{}/on_join", self.server.id))
                .header(ContentType::JSON)
                .header(Header::new("x-session-token", token.to_string()))
                .body(body.to_string())
                .dispatch()
                .await
        }

        /// A role with a rank and allowed permissions
        async fn role(&self, name: &str, rank: i64, allow: i64) -> String {
            let server = self.harness.db.fetch_server(&self.server.id).await.unwrap();
            let role = Role::create(&self.harness.db, &server, name.to_string()).await.unwrap();
            let mut server = self.harness.db.fetch_server(&self.server.id).await.unwrap();
            let stored = server.roles.get_mut(&role.id).expect("role");
            stored.rank = rank;
            stored.permissions.a = allow;
            self.harness
                .db
                .update_server(
                    &server.id,
                    &PartialServer { roles: Some(server.roles.clone()), ..Default::default() },
                    vec![],
                )
                .await
                .unwrap();
            role.id
        }

        async fn server(&self) -> Server {
            self.harness.db.fetch_server(&self.server.id).await.unwrap()
        }

        /// A member who joins now, the way every join ends
        async fn join(&self) -> (Session, User) {
            let (_, session, user) = self.harness.new_user().await;
            Member::create(&self.harness.db, &self.server().await, &user, None).await.unwrap();
            (session, user)
        }

        async fn roles_of(&self, user: &User) -> Vec<String> {
            self.harness.db.fetch_member(&self.server.id, &user.id).await.unwrap().roles
        }

        async fn invite(&self) -> String {
            let response = TestHarness::with_session(
                self.owner.clone(),
                self.harness
                    .client
                    .post(format!("/channels/{}/invites", self.channels[0].id()))
                    .json(&DataCreateInvite { max_uses: None, expires: None, never_expires: false }),
            )
            .await;
            match response.into_json::<Invite>().await.expect("`Invite`") {
                Invite::Server { code, .. } => code,
                _ => unreachable!(),
            }
        }

        /// The system's text messages in the first channel, oldest first
        async fn welcomes(&self) -> Vec<String> {
            let response = self
                .harness
                .client
                .get(format!("/channels/{}/messages?limit=100", self.channels[0].id()))
                .header(Header::new("x-session-token", self.owner.token.clone()))
                .dispatch()
                .await;
            let mut list: Vec<Value> = response.into_json().await.unwrap();
            list.reverse();
            list.iter()
                .filter(|m| m["system"]["type"] == "text")
                .map(|m| m["system"]["content"].as_str().unwrap().to_string())
                .collect()
        }
    }

    #[test]
    fn which_roles_count_as_powerful() {
        assert!(is_powerful(ChannelPermission::ManageServer as i64));
        assert!(is_powerful(ChannelPermission::KickMembers as i64 | ChannelPermission::SendMessage as i64));
        assert!(!is_powerful(
            ChannelPermission::ViewChannel as i64
                | ChannelPermission::SendMessage as i64
                | ChannelPermission::React as i64
                | ChannelPermission::Connect as i64
        ));
        assert!(!is_powerful(0));
    }

    #[rocket::async_test]
    async fn off_until_set_and_only_for_people_who_manage() {
        let s = setup().await;
        let (member, _) = s.join().await;
        let newcomer = s.role("Newcomer", 5, 0).await;

        // nothing stored: empty, and a join gets no role
        let response = s.harness.client.get(format!("/servers/{}/on_join", s.server.id))
            .header(Header::new("x-session-token", s.owner.token.clone())).dispatch().await;
        assert_eq!(response.into_json::<Value>().await.unwrap(), json!({}));

        // a plain member neither reads nor writes it
        let response = s.harness.client.get(format!("/servers/{}/on_join", s.server.id))
            .header(Header::new("x-session-token", member.token.clone())).dispatch().await;
        assert_eq!(response.status(), Status::Forbidden);
        assert_eq!(s.put(&member.token, json!({ "roles": [newcomer] })).await.status(), Status::Forbidden);

        // someone outside the server learns nothing
        let (_, outsider, _) = s.harness.new_user().await;
        assert_eq!(s.put(&outsider.token, json!({ "roles": [] })).await.status(), Status::NotFound);

        let response = s.put(&s.owner.token, json!({ "roles": [newcomer, newcomer] })).await;
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(response.into_json::<Value>().await.unwrap(), json!({ "roles": [newcomer] }));
    }

    #[rocket::async_test]
    async fn every_way_of_joining_gives_the_roles_and_bots_get_none() {
        let s = setup().await;
        let a = s.role("Newcomer", 5, 0).await;
        let b = s.role("Arabic", 6, 0).await;
        assert_eq!(s.put(&s.owner.token, json!({ "roles": [a, b] })).await.status(), Status::Ok);

        // an invite
        let code = s.invite().await;
        let (_, session, user) = s.harness.new_user().await;
        let response = s.harness.client.post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", session.token.clone())).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(s.roles_of(&user).await, vec![a.clone(), b.clone()]);

        // an approved request
        s.harness.db.update_server(
            &s.server.id,
            &PartialServer { access: Some(v0::ServerAccess::Approval), ..Default::default() },
            vec![],
        ).await.unwrap();
        let (_, session, asked) = s.harness.new_user().await;
        let response = s.harness.client.post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", session.token.clone())).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        // a request is not a member yet: no roles before approval
        assert!(s.harness.db.fetch_member(&s.server.id, &asked.id).await.is_err());
        let response = s.harness.client
            .post(format!("/servers/{}/join_requests/{}/approve", s.server.id, asked.id))
            .header(Header::new("x-session-token", s.owner.token.clone())).dispatch().await;
        assert_eq!(response.status(), Status::NoContent);
        assert_eq!(s.roles_of(&asked).await, vec![a.clone(), b.clone()]);

        // a bot
        let bot = User::create(
            &s.harness.db,
            TestHarness::rand_string(),
            None,
            PartialUser {
                bot: Some(BotInformation { owner: s.owner_user.id.clone(), verified: false }),
                ..Default::default()
            },
        ).await.unwrap();
        Member::create(&s.harness.db, &s.server().await, &bot, None).await.unwrap();
        assert!(s.roles_of(&bot).await.is_empty());

        // a role deleted afterwards is skipped, the other still given
        let mut server = s.server().await;
        server.roles.remove(&a);
        s.harness.db.update_server(
            &server.id,
            &PartialServer { roles: Some(server.roles.clone()), access: Some(v0::ServerAccess::Open), ..Default::default() },
            vec![],
        ).await.unwrap();
        let (_, user) = s.join().await;
        assert_eq!(s.roles_of(&user).await, vec![b]);
    }

    #[rocket::async_test]
    async fn the_short_path_from_an_invite_link_gives_the_roles_too() {
        revolt_config::overwrite_config(|config| config.features.join_by_link_enabled = true).await;
        let s = setup().await;
        let role = s.role("Newcomer", 5, 0).await;
        assert_eq!(s.put(&s.owner.token, json!({ "roles": [role] })).await.status(), Status::Ok);
        let code = s.invite().await;

        let response = s.harness.client.post("/auth/account/join")
            .header(ContentType::JSON)
            .body(json!({
                "email": format!("{}@validemail.com", TestHarness::rand_string().to_lowercase()),
                "display_name": "Lina", "birth_month": "1998-03", "invite": code
            }).to_string())
            .dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        let answer: Value = response.into_json().await.unwrap();
        let user_id = answer["session"]["user_id"].as_str().unwrap();
        let member = s.harness.db.fetch_member(&s.server.id, user_id).await.unwrap();
        assert_eq!(member.roles, vec![role]);
    }

    #[rocket::async_test]
    async fn nobody_hands_out_a_role_at_or_above_their_own() {
        let s = setup().await;
        let admin = s.role("Admin", 0, 0).await;
        let moderator = s.role("Moderator", 1, ChannelPermission::ManageRole as i64).await;
        let newcomer = s.role("Newcomer", 5, 0).await;
        let (session, user) = s.join().await;
        s.harness.db.update_member(
            &MemberCompositeKey { server: s.server.id.clone(), user: user.id.clone() },
            &PartialMember { roles: Some(vec![moderator.clone()]), ..Default::default() },
            vec![],
        ).await.unwrap();

        // below their own: fine
        assert_eq!(s.put(&session.token, json!({ "roles": [newcomer] })).await.status(), Status::Ok);
        // above, or their own: refused
        assert_eq!(s.put(&session.token, json!({ "roles": [newcomer, admin] })).await.status(), Status::Forbidden);
        assert_eq!(s.put(&session.token, json!({ "roles": [newcomer, moderator] })).await.status(), Status::Forbidden);
        // and they cannot take off one the owner chose above them
        assert_eq!(s.put(&s.owner.token, json!({ "roles": [newcomer, admin] })).await.status(), Status::Ok);
        assert_eq!(s.put(&session.token, json!({ "roles": [newcomer] })).await.status(), Status::Forbidden);
        // nor write the welcome message: that needs Manage Server
        let response = s.put(&session.token, json!({
            "roles": [newcomer, admin],
            "welcome": { "channel": s.channels[0].id(), "content": "hi" }
        })).await;
        assert_eq!(response.status(), Status::Forbidden);
    }

    #[rocket::async_test]
    async fn a_powerful_role_needs_a_second_yes_and_wrong_roles_are_refused() {
        let s = setup().await;
        let helper = s.role("Helper", 3, ChannelPermission::KickMembers as i64).await;
        let plain = s.role("Plain", 4, ChannelPermission::SendMessage as i64).await;

        let response = s.put(&s.owner.token, json!({ "roles": [plain, helper] })).await;
        assert_eq!(response.status(), Status::BadRequest);
        let error: Value = response.into_json().await.unwrap();
        assert_eq!(error["with"], "confirm_powerful_roles");
        assert!(s.server().await.on_join.is_none());

        let response = s.put(&s.owner.token, json!({ "roles": [plain, helper], "confirm_powerful_roles": true })).await;
        assert_eq!(response.status(), Status::Ok);

        // an unknown role, and more than five
        assert_eq!(s.put(&s.owner.token, json!({ "roles": ["01NOSUCHROLE"] })).await.status(), Status::NotFound);
        let mut many = vec![];
        for i in 0..6 {
            many.push(s.role(&format!("R{i}"), 10 + i, 0).await);
        }
        assert_ne!(s.put(&s.owner.token, json!({ "roles": many })).await.status(), Status::Ok);
    }

    #[rocket::async_test]
    async fn the_welcome_message_names_the_member_and_the_server() {
        let s = setup().await;
        let channel = s.channels[0].id().to_string();

        // empty, too long, a channel of another server
        for content in ["   ".to_string(), "x".repeat(1001)] {
            let response = s.put(&s.owner.token, json!({ "roles": [], "welcome": { "channel": channel, "content": content } })).await;
            assert_eq!(response.status(), Status::BadRequest);
        }
        let (_, _, other_owner) = s.harness.new_user().await;
        let (_, other_channels) = s.harness.new_server(&other_owner).await;
        let response = s.put(&s.owner.token, json!({ "roles": [], "welcome": { "channel": other_channels[0].id(), "content": "hi" } })).await;
        assert_eq!(response.status(), Status::NotFound);

        let response = s.put(&s.owner.token, json!({ "roles": [], "welcome": {
            "channel": channel,
            "content": format!("Welcome {{member}} to {{server}}! Start in <#{channel}>.")
        } })).await;
        assert_eq!(response.status(), Status::Ok);

        let (_, user) = s.join().await;
        assert_eq!(
            s.welcomes().await,
            vec![format!("Welcome <@{}> to Test Server! Start in <#{channel}>.", user.id)]
        );

        // switched off again: nothing more is posted
        assert_eq!(s.put(&s.owner.token, json!({ "roles": [] })).await.status(), Status::Ok);
        s.join().await;
        assert_eq!(s.welcomes().await.len(), 1);
    }

    #[rocket::async_test]
    async fn many_joins_at_once_post_ten_welcomes_a_minute_and_everyone_still_joins() {
        let s = setup().await;
        let response = s.put(&s.owner.token, json!({ "roles": [], "welcome": {
            "channel": s.channels[0].id(), "content": "Welcome {member}"
        } })).await;
        assert_eq!(response.status(), Status::Ok);

        let mut joined = vec![];
        for _ in 0..13 {
            joined.push(s.join().await.1);
        }
        assert_eq!(s.welcomes().await.len(), 10);
        for user in joined {
            s.harness.db.fetch_member(&s.server.id, &user.id).await.expect("`Member`");
        }
    }

    #[rocket::async_test]
    async fn a_role_given_to_everyone_does_not_lift_the_verification_level() {
        let s = setup().await;
        let role = s.role("Newcomer", 5, 0).await;
        assert_eq!(s.put(&s.owner.token, json!({ "roles": [role] })).await.status(), Status::Ok);
        let response = s.harness.client.patch(format!("/servers/{}", s.server.id))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", s.owner.token.clone()))
            .body(json!({ "verification_level": "High" }).to_string())
            .dispatch().await;
        assert_eq!(response.status(), Status::Ok);

        // joined a moment ago, holds the automatic role: still has to wait
        let (session, _) = s.join().await;
        let response = s.harness.client.post(format!("/channels/{}/messages", s.channels[0].id()))
            .header(Header::new("x-session-token", session.token.clone()))
            .json(&json!({ "content": "hi" })).dispatch().await;
        assert_eq!(response.status(), Status::Forbidden);
    }
}
