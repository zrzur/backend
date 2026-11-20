//! The Team page of the admin console (`/staff/team`). A child of `test`, so
//! it uses the same staff helpers.

use super::*;
use revolt_config::{FeaturesTeamApplications, TeamRole};
use revolt_database::{
    TeamActivityCount, TeamActivityDay, TeamHistory, TeamIdentity, TeamMember, TeamMemberStatus,
};
use rocket::http::Method::Post;

const TEAM_ROUTES: [(&str, &str); 8] = [
    ("GET", "/staff/team"),
    ("GET", "/staff/team/MEMBER"),
    ("POST", "/staff/team/MEMBER/identity"),
    ("POST", "/staff/team/MEMBER/confirm"),
    ("POST", "/staff/team/MEMBER/away"),
    ("POST", "/staff/team/MEMBER/note"),
    ("POST", "/staff/team/MEMBER/counting"),
    ("POST", "/staff/team/MEMBER/remove"),
];

/// The Team feature on, with one team role in a real server; the settings
/// are looked up by the staff member's id
async fn desk_on(harness: &TestHarness, staff: &Member, volunteer: &User) -> String {
    let (server, _) = harness.new_server(volunteer).await;
    let mut settings = FeaturesTeamApplications {
        server: server.id.clone(),
        channel_apply: "apply".into(),
        channel_review: "review".into(),
        reviewer_role: "reviewer".into(),
        bot: staff.user.id.clone(),
        roles: vec![TeamRole {
            key: "support".into(),
            name: "Support".into(),
            server: server.id.clone(),
            ..Default::default()
        }],
        ..Default::default()
    };
    settings.volunteers.enabled = true;
    settings.volunteers.away_max_days = 90;
    settings.volunteers.trial_days = 30;
    crate::routes::team_desk::test_settings::register(settings);
    server.id
}

async fn volunteer(
    harness: &TestHarness,
    user: &User,
    server: &str,
    status: TeamMemberStatus,
) -> TeamMember {
    let member = TeamMember {
        id: ulid::Ulid::new().to_string(),
        user_id: user.id.clone(),
        role: "support".into(),
        server: server.to_string(),
        status,
        joined_at: staff_now() - 1000,
        trial_ends_at: staff_now() + 100_000,
        // a person who is not on trial any more was confirmed
        confirmed_at: (status != TeamMemberStatus::Trial).then(|| staff_now() - 500),
        away_until: None,
        left_at: None,
        low_months: 0,
        notes: None,
        invite: None,
        history: vec![TeamHistory { at: 1, actor: "system".into(), action: "accepted".into() }],
    };
    harness.db.insert_team_member(&member).await.unwrap();
    member
}

/// What the official Zrzur account wrote to `user`
async fn official_messages(harness: &TestHarness, user: &User) -> Vec<String> {
    let Ok(channel) = harness
        .db
        .find_direct_message_channel(revolt_database::OfficialAccount::Zrzur.id(), &user.id)
        .await
    else {
        return vec![];
    };
    harness
        .db
        .fetch_messages(revolt_database::MessageQuery {
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
        .filter_map(|message| message.content)
        .collect()
}

async fn stored(harness: &TestHarness, id: &str) -> TeamMember {
    harness.db.fetch_team_member(id).await.unwrap()
}

async fn owner_signed_in(harness: &TestHarness) -> (Member, String) {
    let owner = member(harness, vec![StaffRole::Owner]).await;
    let token = sign_in(harness, &owner, 0).await;
    (owner, token)
}

async fn all_audit(harness: &TestHarness) -> Vec<revolt_database::PlatformAuditEntry> {
    harness
        .db
        .fetch_platform_audit(revolt_database::PlatformAuditQuery { limit: 500, ..Default::default() })
        .await
        .unwrap()
}

#[rocket::async_test]
async fn team_list_needs_team_manage() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    volunteer(&harness, &person, &server, TeamMemberStatus::Trial).await;

    // another staff role: refused, and the refusal is logged
    let support = member(&harness, vec![StaffRole::Support]).await;
    let support_token = sign_in(&harness, &support, 0).await;
    let (status, body) = get(&harness, &support, &support_token, "/staff/team").await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert!(audit_actions(&harness).await.contains(&("staff.access.denied".into(), "Denied".into())));

    // the owner role holds it by default
    let (status, body) = get(&harness, &owner, &token, "/staff/team").await;
    assert_eq!(status, Status::Ok, "{body}");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "trial");
    assert_eq!(rows[0]["role"], "support");
    assert_eq!(rows[0]["has_identity"], false);
    assert_eq!(rows[0]["month_contributions"], 0);
}

#[rocket::async_test]
async fn list_pages_and_counts_this_month() {
    let harness = TestHarness::new().await;
    let (_, _, first) = harness.new_user().await;
    let (_, _, second) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &first).await;
    let a = volunteer(&harness, &first, &server, TeamMemberStatus::Active).await;
    let b = volunteer(&harness, &second, &server, TeamMemberStatus::Active).await;
    let today = revolt_database::util::team_activity::day_of(staff_now());
    harness
        .db
        .upsert_team_activity_day(&TeamActivityDay {
            id: format!("{}:{today}", first.id),
            user_id: first.id.clone(),
            day: today,
            counts: vec![TeamActivityCount {
                server: server.clone(),
                messages: 3,
                replies: 2,
                moderation: 1,
                reviews: 0,
                tickets: 4,
            }],
        })
        .await
        .unwrap();

    let mut ids = vec![a.id.clone(), b.id.clone()];
    ids.sort();
    let (_, body) = get(&harness, &owner, &token, "/staff/team?limit=1").await;
    let page = body.as_array().unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0]["member_id"], ids[0].as_str());
    let (_, body) =
        get(&harness, &owner, &token, &format!("/staff/team?limit=1&after={}", ids[0])).await;
    let rest = body.as_array().unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0]["member_id"], ids[1].as_str());

    let (_, body) = get(&harness, &owner, &token, "/staff/team?status=active").await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let mine = rows.iter().find(|r| r["user_id"] == first.id.as_str()).unwrap();
    assert_eq!(mine["month_contributions"], 10);
    let (status, _) = get(&harness, &owner, &token, "/staff/team?status=nonsense").await;
    assert_eq!(status, Status::BadRequest);

    // the person's page: six months, the counts per kind in the last
    let (_, body) = get(&harness, &owner, &token, &format!("/staff/team/{}", a.id)).await;
    let months = body["months"].as_array().unwrap();
    assert_eq!(months.len(), 6);
    assert_eq!(months[5]["month"], today / 100);
    assert_eq!(months[5]["messages"], 3);
    assert_eq!(months[5]["tickets"], 4);
    assert_eq!(months[0]["messages"], 0);
}

#[rocket::async_test]
async fn identity_needs_step_up_and_is_logged() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Trial).await;
    harness
        .db
        .upsert_team_identity(&TeamIdentity {
            id: person.id.clone(),
            name: "Sara <@everyone> Hidden".into(),
            age: 24,
            country: "Austria".into(),
            rules_version: "1".into(),
            rules_at: 1,
            purge_at: None,
            reviewer: false,
        })
        .await
        .unwrap();
    let path = format!("/staff/team/{}/identity", m.id);

    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({})).await;
    assert_eq!(status, Status::Forbidden, "{body}");
    assert_eq!(body["type"], "NotElevated");
    assert!(!audit_actions(&harness).await.iter().any(|(a, _)| a == "team.identity.view"));
    // the missing step-up is logged
    assert!(audit_actions(&harness).await.contains(&("team.step_up.missing".into(), "Denied".into())));

    step_up(&harness, &owner, &token).await;
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["age"], 24);
    // and when the age was given
    assert_eq!(body["age_given_at"], 1);
    assert_eq!(body["country"], "Austria");
    // raw: the console shows it as text (Solid escapes it), so no markdown
    // backslashes; a mention there is harmless text
    assert_eq!(body["name"], "Sara <@everyone> Hidden", "{body}");

    // logged, with the staff member as actor, and no identity in the log
    let entries = all_audit(&harness).await;
    let view = entries.iter().find(|e| e.action == "team.identity.view").expect("a record");
    assert_eq!(view.actor_id, owner.user.id);
    assert_eq!(view.target_id.as_deref(), Some(person.id.as_str()));
    for e in &entries {
        let text = format!("{:?}{:?}", e.summary, e.reason);
        assert!(!text.contains("Sara") && !text.contains("Austria"), "{text}");
    }
}

#[rocket::async_test]
async fn remove_needs_step_up_and_ends_membership() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Active).await;
    let path = format!("/staff/team/{}/remove", m.id);
    let body = json!({ "reason": "Not active for a long while" });

    let (status, answer) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::Forbidden, "{answer}");
    assert_eq!(answer["type"], "NotElevated");
    assert!(stored(&harness, &m.id).await.is_current());

    step_up(&harness, &owner, &token).await;
    let (status, answer) = send(&harness, &owner, &token, Post, &path, body.clone()).await;
    assert_eq!(status, Status::Ok, "{answer}");
    assert_eq!(answer["row"]["status"], "left");
    let after = stored(&harness, &m.id).await;
    assert!(!after.is_current());
    let last = after.history.last().unwrap();
    assert_eq!(last.action, "removed");
    assert_eq!(last.actor, owner.user.id);
    assert!(audit_actions(&harness).await.contains(&("team.member.remove".into(), "Success".into())));
    // the log keeps that there was a reason, never what it said
    let entries = all_audit(&harness).await;
    let removal = entries.iter().find(|e| e.action == "team.member.remove").expect("a record");
    assert_eq!(removal.reason, None);
    assert!(removal.summary.as_deref().unwrap_or_default().contains("reason of 27 characters"), "{:?}", removal.summary);
    for e in &entries {
        assert!(!format!("{:?}{:?}", e.summary, e.reason).contains("Not active"), "{:?}", e.summary);
    }

    // already gone: refused
    let (status, _) = send(&harness, &owner, &token, Post, &path, body).await;
    assert_eq!(status, Status::BadRequest);
}

#[rocket::async_test]
async fn remove_reason_length_checked() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Active).await;
    let path = format!("/staff/team/{}/remove", m.id);
    step_up(&harness, &owner, &token).await;

    for reason in ["", "too short", "         padded         ", &"x".repeat(501)] {
        let (status, body) =
            send(&harness, &owner, &token, Post, &path, json!({ "reason": reason })).await;
        assert_eq!(status, Status::BadRequest, "{reason:?}: {body}");
    }
    assert!(stored(&harness, &m.id).await.is_current());
    let (status, _) =
        send(&harness, &owner, &token, Post, &path, json!({ "reason": "x".repeat(500) })).await;
    assert_eq!(status, Status::Ok);
}

#[rocket::async_test]
async fn confirm_moves_trial_to_active() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Trial).await;

    let path = format!("/staff/team/{}/confirm", m.id);
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({})).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["row"]["status"], "active");
    let after = stored(&harness, &m.id).await;
    assert_eq!(after.status, TeamMemberStatus::Active);
    assert!(after.confirmed_at.is_some());
    let last = after.history.last().unwrap();
    assert_eq!((last.action.as_str(), last.actor.as_str()), ("confirmed", owner.user.id.as_str()));
}

#[rocket::async_test]
async fn confirm_only_from_trial() {
    let harness = TestHarness::new().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let (_, _, first) = harness.new_user().await;
    let server = desk_on(&harness, &owner, &first).await;
    let mut tried = 0;
    for status in [
        TeamMemberStatus::Active,
        TeamMemberStatus::Away,
        TeamMemberStatus::Inactive,
        TeamMemberStatus::Left,
    ] {
        let (_, _, person) = harness.new_user().await;
        let m = volunteer(&harness, &person, &server, status).await;
        let path = format!("/staff/team/{}/confirm", m.id);
        let (code, body) = send(&harness, &owner, &token, Post, &path, json!({})).await;
        assert_eq!(code, Status::BadRequest, "{status:?}: {body}");
        let after = stored(&harness, &m.id).await;
        assert_eq!(after.status, status);
        assert_eq!(after.confirmed_at, m.confirmed_at);
        tried += 1;
    }
    assert_eq!(tried, 4);
}

#[rocket::async_test]
async fn away_uses_the_volunteers_rules() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Active).await;
    let path = format!("/staff/team/{}/away", m.id);

    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "days": 91 })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, _) = send(&harness, &owner, &token, Post, &path, json!({ "days": -1 })).await;
    assert_eq!(status, Status::BadRequest);
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "days": 10 })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["row"]["status"], "away");
    // the limit comes from the settings
    assert_eq!(body["away_max_days"], 90);
    assert!(stored(&harness, &m.id).await.away_until.is_some());
    // the volunteer is told, as after their own "I'm away"
    let told = official_messages(&harness, &person).await;
    assert!(told.iter().any(|m| m.contains("Days away: 10")), "{told:?}");
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "days": 0 })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["row"]["status"], "active");
    let told = official_messages(&harness, &person).await;
    assert!(told.iter().any(|m| m.contains("active again")), "{told:?}");
}

#[rocket::async_test]
async fn notes_in_own_export_never_in_log() {
    let harness = TestHarness::new().await;
    let (account, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Active).await;
    let note = "Staff only: unmistakable-note-text-5521";
    let path = format!("/staff/team/{}/note", m.id);

    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "text": note })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["notes"], note);
    let (_, body) = get(&harness, &owner, &token, &format!("/staff/team/{}", m.id)).await;
    assert_eq!(body["notes"], note);

    // too long: refused, nothing changes
    let (status, _) =
        send(&harness, &owner, &token, Post, &path, json!({ "text": "x".repeat(2001) })).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(stored(&harness, &m.id).await.notes.as_deref(), Some(note));

    // the daily counts are the person's data too
    harness
        .db
        .upsert_team_activity_day(&TeamActivityDay {
            id: format!("{}:20261109", person.id),
            user_id: person.id.clone(),
            day: 20261109,
            counts: vec![TeamActivityCount {
                server: server.clone(),
                messages: 7731,
                replies: 0,
                moderation: 0,
                reviews: 0,
                tickets: 0,
            }],
        })
        .await
        .unwrap();

    // a staff member acted on the membership: their id is in the history
    let mut acted = stored(&harness, &m.id).await;
    acted.history.push(TeamHistory { at: 2, actor: owner.user.id.clone(), action: "confirmed".into() });
    harness.db.update_team_member(&acted).await.unwrap();

    // someone else in the team, with a note of their own
    let (_, _, other) = harness.new_user().await;
    let mut theirs = volunteer(&harness, &other, &server, TeamMemberStatus::Active).await;
    theirs.notes = Some("other-person-note-7713".into());
    harness.db.update_team_member(&theirs).await.unwrap();

    // in the person's own export (GDPR Art. 15), never in the audit log
    let zip = revolt_database::build_export_zip(&harness.db, &account, &person).await.unwrap();
    let text = String::from_utf8_lossy(&zip);
    assert!(text.contains("unmistakable-note-text-5521"));
    // who acted is not named (the staff member's id is removed from the history)
    let history = stored(&harness, &m.id).await.history;
    assert!(history.iter().any(|h| h.actor == "system"));
    assert!(!text.contains(&owner.user.id), "a staff id in the export");
    assert!(!text.contains("\"actor\""), "actor fields in the export");
    // nobody else's data
    assert!(!text.contains("other-person-note-7713"));
    assert!(!text.contains(&theirs.id));
    assert!(text.contains("team_memberships.json"));
    assert!(text.contains("team_activity.json"));
    assert!(text.contains("20261109") && text.contains("7731"), "the counted day is in the export");
    assert!(all_audit(&harness)
        .await
        .iter()
        .all(|e| !format!("{:?}", e.summary).contains("unmistakable")));
}

#[rocket::async_test]
async fn counting_stops_on_objection_and_resumes() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Active).await;
    let path = format!("/staff/team/{}/counting", m.id);

    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "on": false })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(body["counting_stopped"], true);
    let last = stored(&harness, &m.id).await.history.last().cloned().unwrap();
    assert_eq!((last.action.as_str(), last.actor.as_str()), ("counting_stopped", owner.user.id.as_str()));
    assert!(audit_actions(&harness).await.contains(&("team.member.counting".into(), "Success".into())));

    // asked again: nothing changes, nothing more is logged
    let records = all_audit(&harness).await.iter().filter(|e| e.action == "team.member.counting").count();
    let length = stored(&harness, &m.id).await.history.len();
    let (status, body) = send(&harness, &owner, &token, Post, &path, json!({ "on": false })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(stored(&harness, &m.id).await.history.len(), length);
    assert_eq!(all_audit(&harness).await.iter().filter(|e| e.action == "team.member.counting").count(), records);

    // resumed
    let (_, body) = send(&harness, &owner, &token, Post, &path, json!({ "on": true })).await;
    assert_eq!(body["counting_stopped"], false);
    assert_eq!(stored(&harness, &m.id).await.history.last().unwrap().action, "counting_resumed");
    let (_, body) = get(&harness, &owner, &token, &format!("/staff/team/{}", m.id)).await;
    assert_eq!(body["counting_stopped"], false);
}

#[rocket::async_test]
async fn names_come_raw_for_the_console_to_show_as_text() {
    let harness = TestHarness::new().await;
    let (_, _, first) = harness.new_user().await;
    let (owner, token) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &first).await;
    // a name with characters markdown would escape
    let yasmin = User {
        id: ulid::Ulid::new().to_string(),
        username: "Yasmin_8c95c9".to_string(),
        discriminator: "0001".to_string(),
        ..Default::default()
    };
    harness.db.insert_user(&yasmin).await.unwrap();
    let m = volunteer(&harness, &yasmin, &server, TeamMemberStatus::Active).await;
    let (status, body) = get(&harness, &owner, &token, "/staff/team").await;
    assert_eq!(status, Status::Ok, "{body}");
    let row = body.as_array().unwrap().iter().find(|r| r["member_id"] == m.id.as_str()).unwrap().clone();
    // no markdown backslash: the admin renders it as text
    assert_eq!(row["account_name"], "Yasmin_8c95c9", "{row}");
    let (_, body) = get(&harness, &owner, &token, &format!("/staff/team/{}", m.id)).await;
    assert_eq!(body["row"]["account_name"], "Yasmin_8c95c9");
    assert!(!body.to_string().contains("\\\\_"), "{body}");
}

#[rocket::async_test]
async fn retention_on_the_test_database() {
    use revolt_database::util::team_activity::{run_daily, TeamMessenger, DAY_MS};
    struct Quiet;
    #[rocket::async_trait]
    impl TeamMessenger for Quiet {
        async fn send(&self, _user_id: &str, _content: String) -> bool {
            true
        }
    }
    let harness = TestHarness::new().await;
    let (_, _, gone) = harness.new_user().await;
    let (_, _, stays) = harness.new_user().await;
    let (_, _, alone) = harness.new_user().await;
    let (owner, _) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &gone).await;
    let mut settings = crate::routes::team_desk::test_settings::get(&owner.user.id).unwrap();
    settings.volunteers.enabled = false;
    let now = staff_now();

    // left 91 days ago: goes with its counts; left 10 days ago: stays
    let mut old = volunteer(&harness, &gone, &server, TeamMemberStatus::Left).await;
    old.left_at = Some(now - 91 * DAY_MS);
    harness.db.update_team_member(&old).await.unwrap();
    harness
        .db
        .upsert_team_activity_day(&TeamActivityDay {
            id: format!("{}:20261101", gone.id),
            user_id: gone.id.clone(),
            day: 20261101,
            counts: vec![],
        })
        .await
        .unwrap();
    let mut recent = volunteer(&harness, &stays, &server, TeamMemberStatus::Left).await;
    recent.left_at = Some(now - 10 * DAY_MS);
    harness.db.update_team_member(&recent).await.unwrap();
    // "About you" that nothing holds: a day, set without writing the record
    let identity = TeamIdentity {
        id: alone.id.clone(),
        name: "Sara".into(),
        age: 24,
        country: "Austria".into(),
        rules_version: "1".into(),
        // written over an hour ago: an application in flight no longer claims it
        rules_at: now - 2 * 3_600_000,
        purge_at: None,
        reviewer: false,
    };
    harness.db.upsert_team_identity(&identity).await.unwrap();

    run_daily(&harness.db, &settings, &Quiet, now).await.unwrap();
    assert!(harness.db.fetch_team_member(&old.id).await.is_err());
    assert!(harness.db.fetch_team_activity(&gone.id, 0, 99_991_231).await.unwrap().is_empty());
    assert!(harness.db.fetch_team_member(&recent.id).await.is_ok());
    let marked = harness.db.fetch_team_identity(&alone.id).await.unwrap().unwrap();
    assert_eq!(marked.purge_at, Some(now + DAY_MS));
    assert_eq!(marked.name, "Sara");
    // the conditional writes themselves
    assert!(!harness.db.mark_team_identity_purge(&alone.id, now + 5).await.unwrap());
    assert!(harness.db.clear_team_identity_purge(&alone.id).await.unwrap());
    assert!(!harness.db.mark_team_identity_purge("nobody-here", now).await.unwrap());
    assert!(harness.db.fetch_team_identity("nobody-here").await.unwrap().is_none());
    let mut rejoined = harness.db.fetch_team_member(&recent.id).await.unwrap();
    rejoined.status = TeamMemberStatus::Trial;
    rejoined.left_at = None;
    harness.db.update_team_member(&rejoined).await.unwrap();
    assert_eq!(harness.db.delete_team_member_if_left(&recent.id, now).await.unwrap(), 0);
    assert!(harness.db.fetch_team_member(&recent.id).await.is_ok());
}

#[rocket::async_test]
async fn every_team_route_refuses_without_team_manage() {
    let harness = TestHarness::new().await;
    let (_, _, person) = harness.new_user().await;
    let (owner, _) = owner_signed_in(&harness).await;
    let server = desk_on(&harness, &owner, &person).await;
    let m = volunteer(&harness, &person, &server, TeamMemberStatus::Trial).await;

    // every built-in role except the owner, each with a fresh step-up
    let mut denied = 0;
    for role in StaffRole::ALL.into_iter().filter(|r| *r != StaffRole::Owner) {
        let who = member(&harness, vec![role]).await;
        let token = sign_in(&harness, &who, 0).await;
        step_up(&harness, &who, &token).await;
        for (method, template) in TEAM_ROUTES {
            let path = template.replace("MEMBER", &m.id);
            let body = json!({ "days": 1, "text": "x", "reason": "a long enough reason", "on": false });
            let (status, answer) = if method == "GET" {
                get(&harness, &who, &token, &path).await
            } else {
                send(&harness, &who, &token, Post, &path, body).await
            };
            assert_eq!(status, Status::Forbidden, "{role:?} {method} {path}: {answer}");
            assert_eq!(answer["type"], "MissingPlatformPermission", "{role:?} {path}");
            denied += 1;
        }
    }
    assert_eq!(denied, 8 * TEAM_ROUTES.len());
    // nothing changed
    let after = stored(&harness, &m.id).await;
    assert_eq!(after.status, TeamMemberStatus::Trial);
    assert!(after.notes.is_none());
    let logged = audit_actions(&harness)
        .await
        .iter()
        .filter(|(a, o)| a == "staff.access.denied" && o == "Denied")
        .count();
    assert!(logged >= denied, "{logged} of {denied}");
}
