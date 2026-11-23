//! Zrzur Desk: the bot hands back a press, the server decides. A stolen bot
//! token must gain nothing.

use revolt_config::FeaturesStaffDesk;
use revolt_database::{
    new_staff_token, staff_now, staff_token_hash, Account, Bot, Channel, Interaction,
    PlatformAuditQuery, Session, StaffGrant, StaffRole, StaffSession, StaffSessionKind, Totp,
    User, STAFF_SESSION_MAX_MS,
};
use revolt_models::v0::{CommandOptionType, InteractionDataOption, InteractionType, ModalFieldValue};
use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::session::{CODE_FIELD, LOGIN_MODAL, STEP_UP_MODAL};
use super::test_settings;
use crate::util::test::TestHarness;

const SECRET: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

/// A TOTP code `offset` steps from now (-1, 0 and +1 are accepted)
fn code(offset: i64) -> String {
    let key = base32::decode(base32::Alphabet::RFC4648 { padding: false }, SECRET).unwrap();
    let step = (staff_now() / 1000) as u64 / 30;
    let step = (step as i64 + offset) as u64;
    totp_lite::totp_custom::<totp_lite::Sha1>(30, 6, &key, step * 30)
}

/// Never a valid code (not six digits)
const WRONG_CODE: &str = "wrong!";

struct Person {
    user: User,
    session: Session,
}

/// What the press is
enum Kind<'a> {
    /// `/desk <sub>`
    Command(&'a str),
    /// A submitted form `zd:...` with this code
    Modal(&'a str, &'a str),
    /// A button `zd:...`
    Button(&'a str),
}

struct Stage {
    harness: TestHarness,
    bot: Bot,
    /// The configured place
    place: Channel,
    /// A channel of the same server that is not a place
    elsewhere: Channel,
    server_id: String,
}

async fn enable_totp(harness: &TestHarness, account: &mut Account) {
    account.mfa.totp_token = Totp::Enabled {
        secret: SECRET.to_string(),
    };
    account.save(&harness.db).await.unwrap();
}

async fn set_grant(harness: &TestHarness, user_id: &str, roles: Vec<StaffRole>, disabled: bool) {
    let current = harness.db.fetch_staff_grant(user_id).await.unwrap();
    let grant = StaffGrant {
        id: user_id.to_string(),
        roles,
        disabled,
        expires_at: None,
        last_totp_step: current.as_ref().map(|g| g.last_totp_step).unwrap_or(0),
        version: current.as_ref().map(|g| g.version + 1).unwrap_or(1),
        updated_at: staff_now(),
        updated_by: "test".to_string(),
    };
    assert!(harness
        .db
        .save_staff_grant(&grant, current.map(|g| g.version))
        .await
        .unwrap());
}

fn settings(bot: &Bot, places: Vec<String>, allow_dms: bool) -> FeaturesStaffDesk {
    FeaturesStaffDesk {
        enabled: true,
        bot: bot.id.clone(),
        places,
        allow_dms,
        areas: vec!["overview".to_string()],
    }
}

async fn stage() -> Stage {
    stage_with(false).await
}

/// A server with a desk channel and another channel; the desk is on for a
/// fresh bot, in the desk channel only (and DMs when `allow_dms`)
async fn stage_with(allow_dms: bool) -> Stage {
    let harness = TestHarness::new().await;
    let (_, _, host) = harness.new_user().await;
    let (server, channels) = harness.new_server(&host).await;
    let place = channels.into_iter().next().unwrap();
    let elsewhere = harness.new_channel(&server).await;
    let (bot, _) = harness.new_bot(&host).await;
    test_settings::register(settings(&bot, vec![place.id().to_string()], allow_dms));
    Stage {
        harness,
        bot,
        place,
        elsewhere,
        server_id: server.id,
    }
}

impl Stage {
    /// A person with TOTP on and these staff roles (none: no grant)
    async fn person(&self, roles: Vec<StaffRole>) -> Person {
        let (mut account, session, user) = self.harness.new_user().await;
        enable_totp(&self.harness, &mut account).await;
        if !roles.is_empty() {
            set_grant(&self.harness, &user.id, roles, false).await;
        }
        Person { user, session }
    }

    /// A press as the server stores it, received by `bot`; gives what the
    /// bot gets: id and token
    async fn press_by(
        &self,
        bot: &str,
        who: &Person,
        kind: Kind<'_>,
        channel: &str,
        server: Option<&str>,
    ) -> (String, String) {
        let (interaction_kind, custom_id) = match &kind {
            Kind::Command(_) => (InteractionType::Command, None),
            Kind::Modal(id, _) => (InteractionType::ModalSubmit, Some(id.to_string())),
            Kind::Button(id) => (InteractionType::Component, Some(id.to_string())),
        };
        let (mut interaction, token) = Interaction::new(
            interaction_kind,
            bot.to_string(),
            who.user.id.clone(),
            server.map(str::to_string),
            channel.to_string(),
            ulid::Ulid::new().to_string(),
        );
        interaction.custom_id = custom_id;
        // as `/interactions` stores it: the sign-in the person pressed from
        interaction.session_id = Some(who.session.id.clone());
        match kind {
            Kind::Command(sub) => {
                interaction.command_name = Some("desk".to_string());
                interaction.command_options = vec![InteractionDataOption {
                    name: sub.to_string(),
                    kind: CommandOptionType::SubCommand,
                    value: None,
                    options: None,
                }];
            }
            Kind::Modal(_, code) => {
                interaction.modal_fields = vec![ModalFieldValue {
                    custom_id: CODE_FIELD.to_string(),
                    value: Some(code.to_string()),
                    values: None,
                    checked: None,
                }];
            }
            Kind::Button(_) => {}
        }
        self.harness.db.insert_interaction(&interaction).await.unwrap();
        (interaction.id, token)
    }

    /// A press in the desk channel
    async fn press(&self, who: &Person, kind: Kind<'_>) -> (String, String) {
        self.press_by(&self.bot.id, who, kind, self.place.id(), Some(&self.server_id)).await
    }

    /// The bot hands a press back to `/staff/desk/<path>`
    async fn hand(&self, path: &str, pressed: &(String, String)) -> (Status, Value) {
        self.hand_with("x-bot-token", &self.bot.token, path, pressed).await
    }

    async fn hand_with(
        &self,
        header: &'static str,
        token: &str,
        path: &str,
        pressed: &(String, String),
    ) -> (Status, Value) {
        let response = self
            .harness
            .client
            .post(format!("/staff/desk/{path}"))
            .header(Header::new(header, token.to_string()))
            .header(ContentType::JSON)
            .body(json!({ "interaction_id": pressed.0, "token": pressed.1 }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    /// Signs in on the desk with the code `offset` steps from now
    async fn sign_in(&self, who: &Person, offset: i64) -> Value {
        let code = code(offset);
        let pressed = self.press(who, Kind::Modal(LOGIN_MODAL, &code)).await;
        let (status, body) = self.hand("session", &pressed).await;
        assert_eq!(status, Status::Ok, "{body}");
        body
    }

    async fn me(&self, who: &Person) -> (Status, Value) {
        let pressed = self.press(who, Kind::Command("me")).await;
        self.hand("me", &pressed).await
    }

    /// The open desk sessions of a person
    async fn desk_sessions(&self, who: &Person) -> Vec<StaffSession> {
        self.harness
            .db
            .fetch_staff_sessions_for_user(&who.user.id, staff_now())
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.kind == StaffSessionKind::Desk)
            .collect()
    }

    /// (action, outcome, summary) of a person's records, oldest first
    async fn records(&self, who: &Person) -> Vec<(String, String, String)> {
        let mut records: Vec<_> = self
            .harness
            .db
            .fetch_platform_audit(PlatformAuditQuery {
                actor_id: Some(who.user.id.clone()),
                limit: 500,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.action, format!("{:?}", e.outcome), e.summary.unwrap_or_default()))
            .collect();
        records.reverse();
        records
    }

    /// (action, outcome, staff_session_id) of a person's records, oldest first
    async fn record_sessions(&self, who: &Person) -> Vec<(String, String, Option<String>)> {
        let mut records: Vec<_> = self
            .harness
            .db
            .fetch_platform_audit(PlatformAuditQuery {
                actor_id: Some(who.user.id.clone()),
                limit: 500,
                ..Default::default()
            })
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.action, format!("{:?}", e.outcome), e.staff_session_id))
            .collect();
        records.reverse();
        records
    }

    /// Opens a console session (`x-staff-session`) with the code `offset`
    async fn console_open(&self, who: &Person, offset: i64) -> (Status, Value) {
        let response = self
            .harness
            .client
            .post("/staff/session")
            .header(Header::new("x-session-token", who.session.token.clone()))
            .header(ContentType::JSON)
            .body(json!({ "totp_code": code(offset) }).to_string())
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_json().await.unwrap_or(Value::Null))
    }

    async fn console_me(&self, who: &Person, staff_token: &str) -> Status {
        self.harness
            .client
            .get("/staff/me")
            .header(Header::new("x-session-token", who.session.token.clone()))
            .header(Header::new("x-staff-session", staff_token.to_string()))
            .dispatch()
            .await
            .status()
    }
}

fn error_type(body: &Value) -> &str {
    body["type"].as_str().unwrap_or_default()
}

// ---- Review Focus ----

/// A non-staff user runs `/desk`: no staff access, one Denied record, no
/// session (from `me` nor from the sign-in form)
#[rocket::async_test]
async fn non_staff_desk_denied() {
    let s = stage().await;
    let outsider = s.person(vec![]).await;

    let (status, body) = s.me(&outsider).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotPrivileged");
    assert_eq!(
        s.records(&outsider).await,
        vec![(
            "staff.desk.no_grant".to_string(),
            "Denied".to_string(),
            "desk: no active staff grant".to_string()
        )]
    );

    // the sign-in form, even with a right code, opens nothing
    let code = code(0);
    let pressed = s.press(&outsider, Kind::Modal(LOGIN_MODAL, &code)).await;
    let (status, body) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(s.desk_sessions(&outsider).await.is_empty());
    assert_eq!(s.records(&outsider).await.len(), 2);
}

/// A press from a channel not in `places`, or a DM while `allow_dms` is
/// off: Denied, nothing happens
#[rocket::async_test]
async fn press_outside_places_denied() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    let open = s.desk_sessions(&staff).await;
    assert_eq!(open.len(), 1);

    // another channel of the same server: a sign-out from there does nothing
    let pressed = s
        .press_by(&s.bot.id, &staff, Kind::Command("logout"), s.elsewhere.id(), Some(&s.server_id))
        .await;
    let (status, body) = s.hand("session/close", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "NotPrivileged");

    // a DM with the bot, DMs off: a sign-in from there opens nothing
    let dm = Channel::DirectMessage {
        id: ulid::Ulid::new().to_string(),
        active: true,
        recipients: vec![staff.user.id.clone(), s.bot.id.clone()],
        last_message_id: None,
    };
    s.harness.db.insert_channel(&dm).await.unwrap();
    let code = code(0);
    let pressed = s
        .press_by(&s.bot.id, &staff, Kind::Modal(LOGIN_MODAL, &code), dm.id(), None)
        .await;
    let (status, body) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");

    // the same desk session, untouched
    let still = s.desk_sessions(&staff).await;
    assert_eq!(still.len(), 1);
    assert_eq!(still[0].id, open[0].id);
    let places: Vec<_> = s
        .records(&staff)
        .await
        .into_iter()
        .filter(|(action, _, _)| action == "staff.desk.place")
        .collect();
    assert_eq!(places.len(), 2);
    assert!(places.iter().all(|(_, outcome, summary)| outcome == "Denied" && summary.starts_with("desk")));
    // the refusals name the open desk session
    let sessions: Vec<_> = s
        .record_sessions(&staff)
        .await
        .into_iter()
        .filter(|(action, _, _)| action == "staff.desk.place")
        .map(|(_, _, session)| session)
        .collect();
    assert_eq!(sessions, vec![Some(open[0].id.clone()), Some(open[0].id.clone())]);
}

/// With `allow_dms` the presser's DM with the bot is a place; another DM is not
#[rocket::async_test]
async fn dm_accepted_only_with_allow_dms() {
    let s = stage_with(true).await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let dm = Channel::DirectMessage {
        id: ulid::Ulid::new().to_string(),
        active: true,
        recipients: vec![staff.user.id.clone(), s.bot.id.clone()],
        last_message_id: None,
    };
    s.harness.db.insert_channel(&dm).await.unwrap();
    let code = code(0);
    let pressed = s
        .press_by(&s.bot.id, &staff, Kind::Modal(LOGIN_MODAL, &code), dm.id(), None)
        .await;
    let (status, body) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");

    // a DM between two people is not the desk
    let (_, _, other) = s.harness.new_user().await;
    let theirs = Channel::DirectMessage {
        id: ulid::Ulid::new().to_string(),
        active: true,
        recipients: vec![staff.user.id.clone(), other.id.clone()],
        last_message_id: None,
    };
    s.harness.db.insert_channel(&theirs).await.unwrap();
    let pressed = s
        .press_by(&s.bot.id, &staff, Kind::Command("me"), theirs.id(), None)
        .await;
    assert_eq!(s.hand("me", &pressed).await.0, Status::Forbidden);
}

/// A desk session used with the console header is refused, and a console
/// session does not open the desk
#[rocket::async_test]
async fn desk_session_not_valid_in_console() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;

    // a console session gives nothing on the desk
    let (status, body) = s.console_open(&staff, -1).await;
    assert_eq!(status, Status::Ok, "{body}");
    let console_token = body["token"].as_str().unwrap().to_string();
    assert_eq!(s.console_me(&staff, &console_token).await, Status::Ok);
    let (status, body) = s.me(&staff).await;
    assert_eq!(status, Status::Unauthorized, "{body}");
    assert_eq!(error_type(&body), "InvalidSession");

    // a desk session whose token someone knows, even bound to the person's
    // real session, is refused by the console guard
    let now = staff_now();
    let token = new_staff_token();
    let desk = StaffSession {
        id: ulid::Ulid::new().to_string(),
        token_hash: staff_token_hash(&token),
        user_id: staff.user.id.clone(),
        session_id: staff.session.id.clone(),
        created_at: now,
        last_seen_at: now,
        expires_at: now + STAFF_SESSION_MAX_MS,
        step_up_until: None,
        revoked_at: None,
        user_agent: None,
        kind: StaffSessionKind::Desk,
        bot_id: Some(s.bot.id.clone()),
    };
    s.harness.db.insert_staff_session(&desk).await.unwrap();
    assert_eq!(s.console_me(&staff, &token).await, Status::Unauthorized);

    // a desk session opened on the desk works there, not in the console
    s.sign_in(&staff, 0).await;
    assert_eq!(s.me(&staff).await.0, Status::Ok);
}

/// A staff member removed while their desk session is open: the next press
/// is refused and the desk session revoked
#[rocket::async_test]
async fn removed_grant_ends_desk_session() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    assert_eq!(s.me(&staff).await.0, Status::Ok);

    set_grant(&s.harness, &staff.user.id, vec![StaffRole::Support], true).await;
    let (status, body) = s.me(&staff).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(s.desk_sessions(&staff).await.is_empty());
    assert!(s
        .harness
        .db
        .fetch_desk_session(&staff.user.id, &s.bot.id, staff_now())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        s.records(&staff).await.last().unwrap(),
        &(
            "staff.desk.no_grant".to_string(),
            "Denied".to_string(),
            "desk: no active staff grant".to_string()
        )
    );

    // given back: the old session stays ended, a new sign-in is needed
    set_grant(&s.harness, &staff.user.id, vec![StaffRole::Support], false).await;
    let (status, body) = s.me(&staff).await;
    assert_eq!(status, Status::Unauthorized, "{body}");
}

/// Five wrong codes on the desk lock the console too (and the desk)
#[rocket::async_test]
async fn desk_code_failures_lock_console_too() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    for _ in 0..5 {
        let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, WRONG_CODE)).await;
        let (status, body) = s.hand("session", &pressed).await;
        assert_eq!(status, Status::Unauthorized, "{body}");
        assert_eq!(error_type(&body), "InvalidCredentials");
    }
    let (status, body) = s.console_open(&staff, 0).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "LockedOut");

    let code = code(0);
    let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, &code)).await;
    let (status, body) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(error_type(&body), "LockedOut");
    assert!(s.desk_sessions(&staff).await.is_empty());
}

// ---- the rest of the brief ----

/// A wrong code is Denied and opens no session
#[rocket::async_test]
async fn wrong_desk_code_denied_no_session() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, WRONG_CODE)).await;
    let (status, _) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Unauthorized);
    assert!(s.desk_sessions(&staff).await.is_empty());
    assert_eq!(
        s.records(&staff).await,
        vec![(
            "staff.session.create".to_string(),
            "Denied".to_string(),
            "desk: second factor missing or wrong".to_string()
        )]
    );
}

/// The same press twice: the second is refused
#[rocket::async_test]
async fn same_press_twice_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    let pressed = s.press(&staff, Kind::Command("me")).await;
    assert_eq!(s.hand("me", &pressed).await.0, Status::Ok);
    let (status, body) = s.hand("me", &pressed).await;
    assert_eq!(status, Status::Gone, "{body}");
    assert_eq!(error_type(&body), "InteractionExpired");

    // a wrong token is not the press
    let other = s.press(&staff, Kind::Command("me")).await;
    let (status, _) = s.hand("me", &(other.0.clone(), "made-up".to_string())).await;
    assert_eq!(status, Status::NotFound);
}

/// A press of another kind or name than the route expects is refused
#[rocket::async_test]
async fn wrong_kind_of_press_refused() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    // `/desk me` handed to sign-out
    let pressed = s.press(&staff, Kind::Command("me")).await;
    let (status, body) = s.hand("session/close", &pressed).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    // a button named like the sign-in form
    let pressed = s.press(&staff, Kind::Button(LOGIN_MODAL)).await;
    assert_eq!(s.hand("session", &pressed).await.0, Status::BadRequest);
    // the step-up form handed to sign-in, and the sign-in form to step-up
    let code = code(0);
    let pressed = s.press(&staff, Kind::Modal(STEP_UP_MODAL, &code)).await;
    assert_eq!(s.hand("session", &pressed).await.0, Status::BadRequest);
    let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, &code)).await;
    let (status, body) = s.hand("step-up", &pressed).await;
    assert_eq!(status, Status::BadRequest, "{body}");
    assert_eq!(error_type(&body), "InvalidOperation");
    // still signed in once
    assert_eq!(s.desk_sessions(&staff).await.len(), 1);
}

/// Another bot, or a person, calling the desk gets 404; so does a press
/// received by another bot
#[rocket::async_test]
async fn other_callers_not_found() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let (_, _, host) = s.harness.new_user().await;
    let (other_bot, _) = s.harness.new_bot(&host).await;

    let pressed = s.press(&staff, Kind::Command("me")).await;
    let (status, _) = s.hand_with("x-bot-token", &other_bot.token, "me", &pressed).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) = s
        .hand_with("x-session-token", &staff.session.token, "me", &pressed)
        .await;
    assert_eq!(status, Status::NotFound);

    // a press another bot received, handed in by the desk bot
    let theirs = s
        .press_by(&other_bot.id, &staff, Kind::Command("me"), s.place.id(), Some(&s.server_id))
        .await;
    let (status, body) = s.hand("me", &theirs).await;
    assert_eq!(status, Status::NotFound);
    assert_eq!(error_type(&body), "UnknownInteraction");
    assert!(s.records(&staff).await.is_empty());
}

/// Settings off: 404
#[rocket::async_test]
async fn settings_off_not_found() {
    let s = stage().await;
    let mut off = settings(&s.bot, vec![s.place.id().to_string()], false);
    off.enabled = false;
    test_settings::register(off);
    let staff = s.person(vec![StaffRole::Support]).await;
    let pressed = s.press(&staff, Kind::Command("me")).await;
    let (status, body) = s.hand("me", &pressed).await;
    assert_eq!(status, Status::NotFound);
    assert_eq!(error_type(&body), "NotFound");
}

/// A second sign-in ends the first desk session
#[rocket::async_test]
async fn second_sign_in_revokes_first() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    let first = s.desk_sessions(&staff).await;
    assert_eq!(first.len(), 1);
    s.sign_in(&staff, 0).await;
    let second = s.desk_sessions(&staff).await;
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].id, second[0].id);
    // bound to the sign-in the code was typed in
    assert_eq!(second[0].session_id, staff.session.id);
    assert_eq!(second[0].bot_id.as_deref(), Some(s.bot.id.as_str()));
    let created: Vec<_> = s
        .record_sessions(&staff)
        .await
        .into_iter()
        .filter(|(action, outcome, _)| action == "staff.session.create" && outcome == "Success")
        .map(|(_, _, session)| session)
        .collect();
    assert_eq!(created, vec![Some(first[0].id.clone()), Some(second[0].id.clone())]);

    // the records carry the desk session's id
    let creates: Vec<_> = s
        .records(&staff)
        .await
        .into_iter()
        .filter(|(action, outcome, summary)| {
            action == "staff.session.create" && outcome == "Success" && summary == "desk sign-in"
        })
        .collect();
    assert_eq!(creates.len(), 2);
}

/// `me` lists only the permissions of the presser's roles, and the areas
/// they may use; step-up and sign-out work on the desk session
#[rocket::async_test]
async fn me_lists_only_role_permissions() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let opened = s.sign_in(&staff, -1).await;

    let (status, me) = s.me(&staff).await;
    assert_eq!(status, Status::Ok, "{me}");
    let mut expected: Vec<Value> = StaffRole::Support
        .permissions()
        .iter()
        .map(|p| serde_json::to_value(p).unwrap())
        .collect();
    expected.sort_by_key(|v| v.to_string());
    expected.dedup();
    let mut got = me["permissions"].as_array().unwrap().clone();
    got.sort_by_key(|v| v.to_string());
    assert_eq!(got, expected);
    assert_eq!(me["roles"], json!(["support"]));
    assert_eq!(me["user_id"], json!(staff.user.id));
    assert_eq!(me["expires_at"], opened["expires_at"]);
    assert!(me["step_up_until"].is_null());
    let overview = StaffRole::Support
        .permissions()
        .contains(&revolt_database::StaffPermission::DashboardRead);
    assert_eq!(me["areas"], if overview { json!(["overview"]) } else { json!([]) });

    // an Owner sees the overview area
    let owner = s.person(vec![StaffRole::Owner]).await;
    s.sign_in(&owner, -1).await;
    assert_eq!(s.me(&owner).await.1["areas"], json!(["overview"]));

    // step up on the desk session
    let code = code(0);
    let pressed = s.press(&staff, Kind::Modal(STEP_UP_MODAL, &code)).await;
    let (status, body) = s.hand("step-up", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(body["step_up_until"].as_i64().unwrap() > staff_now());
    assert!(s.me(&staff).await.1["step_up_until"].is_i64());

    // sign out: the next press needs a sign-in
    let pressed = s.press(&staff, Kind::Command("logout")).await;
    assert_eq!(s.hand("session/close", &pressed).await.0, Status::Ok);
    assert_eq!(s.me(&staff).await.0, Status::Unauthorized);
    let summaries: Vec<String> = s.records(&staff).await.into_iter().map(|r| r.2).collect();
    assert!(summaries.iter().all(|summary| summary.starts_with("desk")), "{summaries:?}");
}

/// The members list shows each open session's kind and bot, never a hash
#[rocket::async_test]
async fn members_list_shows_session_kinds() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    let owner = s.person(vec![StaffRole::Owner]).await;
    let (status, body) = s.console_open(&owner, -1).await;
    assert_eq!(status, Status::Ok, "{body}");
    let token = body["token"].as_str().unwrap().to_string();

    let response = s
        .harness
        .client
        .get("/staff/members")
        .header(Header::new("x-session-token", owner.session.token.clone()))
        .header(Header::new("x-staff-session", token))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let text = response.into_string().await.unwrap();
    assert!(!text.contains("token_hash"));
    let members: Value = serde_json::from_str(&text).unwrap();
    let row = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == json!(staff.user.id))
        .unwrap();
    assert_eq!(row["open_sessions"], json!(1));
    assert_eq!(row["sessions"][0]["kind"], json!("desk"));
    assert_eq!(row["sessions"][0]["bot_id"], json!(s.bot.id));
    let mine = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == json!(owner.user.id))
        .unwrap();
    assert_eq!(mine["sessions"][0]["kind"], json!("console"));
}

// ---- security review fixes ----

/// A second ordinary session of the same person, as signing in on another device gives
async fn other_device(s: &Stage, who: &Person) -> Person {
    let mut session = who.session.clone();
    session.id = ulid::Ulid::new().to_string();
    session.token = TestHarness::rand_string();
    session.token_hash = TestHarness::rand_string();
    s.harness.db.insert_session(&session).await.unwrap();
    Person {
        user: who.user.clone(),
        session,
    }
}

/// The desk session is bound to the sign-in the code was typed in: a press
/// from another valid sign-in of the same person does not use it
#[rocket::async_test]
async fn desk_session_bound_to_its_sign_in() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    let phone = other_device(&s, &staff).await;

    let (status, body) = s.me(&phone).await;
    assert_eq!(status, Status::Unauthorized, "{body}");
    assert_eq!(error_type(&body), "InvalidSession");
    // the first device still works
    assert_eq!(s.me(&staff).await.0, Status::Ok);

    // a press with no sign-in on record (an old interaction) neither
    let (mut interaction, token) = Interaction::new(
        InteractionType::Command,
        s.bot.id.clone(),
        staff.user.id.clone(),
        Some(s.server_id.clone()),
        s.place.id().to_string(),
        ulid::Ulid::new().to_string(),
    );
    interaction.command_name = Some("desk".to_string());
    interaction.command_options = vec![InteractionDataOption {
        name: "me".to_string(),
        kind: CommandOptionType::SubCommand,
        value: None,
        options: None,
    }];
    s.harness.db.insert_interaction(&interaction).await.unwrap();
    assert_eq!(s.hand("me", &(interaction.id, token)).await.0, Status::Unauthorized);
}

/// Signing out (the ordinary session deleted, as "log out everywhere" and a
/// password change do) ends the desk session
#[rocket::async_test]
async fn ending_the_sign_in_ends_the_desk_session() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    s.sign_in(&staff, -1).await;
    assert_eq!(s.desk_sessions(&staff).await.len(), 1);

    s.harness.db.delete_session(&staff.session.id).await.unwrap();
    let (status, body) = s.me(&staff).await;
    assert_eq!(status, Status::Unauthorized, "{body}");
    assert!(s.desk_sessions(&staff).await.is_empty());

    // and no sign-in from a sign-in that is gone
    let code = code(0);
    let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, &code)).await;
    assert_eq!(s.hand("session", &pressed).await.0, Status::Unauthorized);
    assert!(s.desk_sessions(&staff).await.is_empty());
}

async fn next_interaction(
    events: &mut crate::util::test::PubSubTestHelper,
) -> revolt_models::v0::Interaction {
    use revolt_database::events::client::EventV1;
    match events
        .wait_for_event(|e| matches!(e, EventV1::InteractionCreate { .. }))
        .await
    {
        EventV1::InteractionCreate { interaction } => interaction,
        _ => unreachable!(),
    }
}

/// The real flow: `/desk me`, the bot opens `zd:login`, the person types the
/// code. The bot never sees the code, and the stored press forgets it once
/// `/staff/desk/session` has read it.
#[rocket::async_test]
async fn code_never_reaches_the_bot() {
    use crate::util::test::PubSubTestHelper;
    use revolt_database::Member;

    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let server = s.harness.db.fetch_server(&s.server_id).await.unwrap();
    let bot_user = s.harness.db.fetch_user(&s.bot.id).await.unwrap();
    for member in [&staff.user, &bot_user] {
        Member::create(&s.harness.db, &server, member, None).await.unwrap();
    }
    revolt_presence::create_session(&s.bot.id, 0).await;
    let command = revolt_database::create_application_command(
        &s.harness.db,
        &s.bot.id,
        None,
        serde_json::from_value(json!({
            "name": "desk",
            "description": "Zrzur Desk",
            "options": [
                { "type": "SubCommand", "name": "me", "description": "me" },
                { "type": "SubCommand", "name": "logout", "description": "logout" }
            ]
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let mut events = PubSubTestHelper::new(&format!("{}!", s.bot.id)).await;

    let interact = |body: Value| {
        s.harness
            .client
            .post("/interactions")
            .header(Header::new("x-session-token", staff.session.token.clone()))
            .header(ContentType::JSON)
            .body(body.to_string())
    };

    // `/desk me`
    let response = interact(json!({
        "type": "Command",
        "application_id": s.bot.id,
        "channel_id": s.place.id(),
        "command_id": command.id,
        "options": [{ "type": "SubCommand", "name": "me" }],
        "nonce": ulid::Ulid::new().to_string(),
    }))
    .dispatch()
    .await;
    assert_eq!(response.status(), Status::Ok);
    let command_press = next_interaction(&mut events).await;
    let stored = s.harness.db.fetch_interaction(&command_press.id).await.unwrap();
    assert_eq!(stored.session_id.as_deref(), Some(staff.session.id.as_str()));

    // the bot answers with the sign-in form
    let status = s
        .harness
        .client
        .post(format!(
            "/interactions/{}/{}/callback",
            command_press.id,
            command_press.token.clone().unwrap()
        ))
        .header(ContentType::JSON)
        .body(
            json!({ "type": "Modal", "modal": {
                "custom_id": LOGIN_MODAL,
                "title": "Zrzur Desk",
                "components": [{ "type": "ActionRow", "components": [{
                    "type": "TextInput", "custom_id": CODE_FIELD, "style": "Short",
                    "label": "Code", "min_length": 6, "max_length": 6
                }]}]
            }})
            .to_string(),
        )
        .dispatch()
        .await
        .status();
    assert_eq!(status, Status::NoContent);

    // the person types the code
    let typed = code(0);
    let response = interact(json!({
        "type": "ModalSubmit",
        "channel_id": s.place.id(),
        "parent_id": command_press.id,
        "custom_id": LOGIN_MODAL,
        "fields": [{ "custom_id": CODE_FIELD, "value": typed }],
        "nonce": ulid::Ulid::new().to_string(),
    }))
    .dispatch()
    .await;
    assert_eq!(response.status(), Status::Ok);
    let submit = next_interaction(&mut events).await;
    let fields = submit.modal.clone().unwrap().fields;
    assert!(fields.iter().all(|f| f.custom_id != CODE_FIELD), "{fields:?}");
    assert!(!serde_json::to_string(&submit).unwrap().contains(&typed));
    // stored until the desk reads it
    let stored = s.harness.db.fetch_interaction(&submit.id).await.unwrap();
    assert_eq!(stored.modal_fields.len(), 1);

    let pressed = (submit.id.clone(), submit.token.clone().unwrap());
    let (status, body) = s.hand("session", &pressed).await;
    assert_eq!(status, Status::Ok, "{body}");
    let stored = s.harness.db.fetch_interaction(&submit.id).await.unwrap();
    assert!(stored.modal_fields.is_empty());
    let open = s.desk_sessions(&staff).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].session_id, staff.session.id);
}

/// A wrong code is forgotten too
#[rocket::async_test]
async fn wrong_code_is_not_kept() {
    let s = stage().await;
    let staff = s.person(vec![StaffRole::Support]).await;
    let pressed = s.press(&staff, Kind::Modal(LOGIN_MODAL, WRONG_CODE)).await;
    assert_eq!(s.hand("session", &pressed).await.0, Status::Unauthorized);
    let stored = s.harness.db.fetch_interaction(&pressed.0).await.unwrap();
    assert!(stored.modal_fields.is_empty());
}
