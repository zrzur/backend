//! Notices and links of events: reminders (once, to the people who are
//! coming, never to someone who switched them off), a changed time, a
//! called-off event, invitations and a freed seat; the share link and what
//! it shows to whom; invitation links. Runs on REFERENCE and MONGODB
//! (TEST_DB).

use std::collections::HashMap;

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::overwrite_config;
use revolt_database::sent_notices;
use revolt_database::{EventNotice, ServerEvent, Session};
use rocket::http::{ContentType, Method, Status};
use serde_json::{json, Value};

use super::events_answers_tests::{answer, create_with, event_path, join, park};
use super::events_tests::{at, call, create, setup, Setup};

fn id_of(event: &Value) -> String {
    event["_id"].as_str().expect("an id").to_string()
}

/// The time of a crond pass a little after `minutes` before the start of an
/// event that starts in two hours
fn pass_time(minutes_before_start: i64) -> Timestamp {
    Timestamp::now_utc() + Duration::minutes(120 - minutes_before_start + 1)
}

async fn edit(s: &Setup, event: &Value, data: Value) -> (Status, Value) {
    call(&s.harness, Method::Patch, &event_path(s, event), &s.owner_session, Some(data)).await
}

async fn mute(s: &Setup, session: &Session, event: &Value, muted: bool) -> (Status, Value) {
    call(
        &s.harness,
        Method::Put,
        &format!("{}/notices", event_path(s, event)),
        session,
        Some(json!({ "muted": muted })),
    )
    .await
}

/// A request without a session
async fn anonymous_get(s: &Setup, path: &str) -> (Status, Value) {
    let response = s
        .harness
        .client
        .get(path.to_string())
        .header(ContentType::JSON)
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[rocket::async_test]
async fn reminders_are_checked_kept_in_order_and_copied() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "reminders": [15, 1440, 15] })).await;
    assert_eq!(event["reminders"], json!([1440, 15]));

    for wrong in [json!([4]), json!([10081]), json!([5, 10, 15, 20])] {
        let (status, error) = edit(&s, &event, json!({ "reminders": wrong })).await;
        assert_eq!(status, Status::BadRequest, "{error}");
        assert_eq!(error["type"], "FailedValidation", "{error}");
    }
    let (status, edited) = edit(&s, &event, json!({ "reminders": [60] })).await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["reminders"], json!([60]));

    let (status, copy) = call(
        &s.harness,
        Method::Post,
        &format!("{}/copy", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{copy}");
    assert_eq!(copy["reminders"], json!([60]));

    // none: the field is not there
    let (_, cleared) = edit(&s, &event, json!({ "reminders": [] })).await;
    assert!(cleared.get("reminders").is_none(), "{cleared}");
}

#[rocket::async_test]
async fn a_due_reminder_goes_out_once_to_the_people_who_are_coming() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "reminders": [60] })).await;
    let id = id_of(&event);
    let (maybe_session, maybe) = join(&s).await;
    let (no_session, _) = join(&s).await;
    join(&s).await; // someone who never answered
    assert_eq!(answer(&s, &s.member_session, &event, "Going").await.0, Status::Ok);
    assert_eq!(answer(&s, &maybe_session, &event, "Maybe").await.0, Status::Ok);
    assert_eq!(answer(&s, &no_session, &event, "NotGoing").await.0, Status::Ok);

    // an hour and a half before the start nothing is due
    let early = ServerEvent::run_reminder_pass(db, pass_time(90)).await.unwrap();
    assert_eq!(early, Default::default(), "{early:?}");
    assert!(sent_notices::of(&id).is_empty());

    let report = ServerEvent::run_reminder_pass(db, pass_time(60)).await.unwrap();
    assert_eq!((report.sent, report.people, report.late), (1, 2, 0), "{report:?}");
    let sent = sent_notices::of(&id);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, EventNotice::Reminder(60));
    let mut told = sent[0].1.clone();
    told.sort();
    let mut expected = vec![s.member.id.clone(), maybe.id.clone()];
    expected.sort();
    assert_eq!(told, expected);

    // the next pass, and a second crond at the same moment, send nothing
    let again = ServerEvent::run_reminder_pass(db, pass_time(59)).await.unwrap();
    assert_eq!(again, Default::default(), "{again:?}");
    assert_eq!(sent_notices::of(&id).len(), 1);
    assert!(!db.mark_server_event_reminder(&id, 60).await.unwrap());
}

#[rocket::async_test]
async fn a_reminder_that_is_too_late_is_noted_and_not_sent() {
    let s = setup(true).await;
    let db = &s.harness.db;
    // made two hours before the start with a reminder a day before
    let event = create_with(&s, park(), json!({ "reminders": [1440, 60] })).await;
    let id = id_of(&event);
    assert_eq!(answer(&s, &s.member_session, &event, "Going").await.0, Status::Ok);

    let report = ServerEvent::run_reminder_pass(db, pass_time(119)).await.unwrap();
    assert_eq!((report.sent, report.late), (0, 1), "{report:?}");
    assert!(sent_notices::of(&id).is_empty());
    // the hour reminder still goes out at its time
    let report = ServerEvent::run_reminder_pass(db, pass_time(60)).await.unwrap();
    assert_eq!((report.sent, report.late), (1, 0), "{report:?}");
}

#[rocket::async_test]
async fn people_who_switched_notices_off_are_not_told() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "reminders": [60] })).await;
    let id = id_of(&event);
    let (quiet_session, _) = join(&s).await;
    let (off_session, off) = join(&s).await;
    let (keen_session, keen) = join(&s).await;

    // without an answer there is nothing to switch
    let (status, error) = mute(&s, &quiet_session, &event, true).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "InvalidOperation");

    for session in [&quiet_session, &off_session, &keen_session] {
        assert_eq!(answer(&s, session, &event, "Going").await.0, Status::Ok);
    }
    let (status, muted) = mute(&s, &quiet_session, &event, true).await;
    assert_eq!(status, Status::Ok, "{muted}");
    assert_eq!(muted["rsvp"]["muted"], true);
    // changing the answer keeps the switch
    let (_, changed) = answer(&s, &quiet_session, &event, "Maybe").await;
    assert_eq!(changed["rsvp"]["muted"], true, "{changed}");

    // reminders off for every event, in the person's synced settings
    db.set_user_settings(
        &off.id,
        &HashMap::from([(
            "event_notices".to_string(),
            (0_i64, r#"{"reminders":false}"#.to_string()),
        )]),
    )
    .await
    .unwrap();

    let report = ServerEvent::run_reminder_pass(db, pass_time(60)).await.unwrap();
    assert_eq!((report.sent, report.people), (1, 1), "{report:?}");
    assert_eq!(sent_notices::of(&id)[0].1, vec![keen.id.clone()]);

    // "changes" is another switch: the person with reminders off hears of
    // a called-off event, the one who muted this event does not
    let (status, body) = call(
        &s.harness,
        Method::Post,
        &format!("{}/cancel", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let sent = sent_notices::of(&id);
    let (notice, mut told) = sent.last().cloned().expect("a notice");
    assert_eq!(notice, EventNotice::Cancelled);
    told.sort();
    let mut expected = vec![off.id.clone(), keen.id.clone()];
    expected.sort();
    assert_eq!(told, expected);
}

#[rocket::async_test]
async fn a_new_time_tells_people_once_and_the_reminders_count_again() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "reminders": [60] })).await;
    let id = id_of(&event);
    assert_eq!(answer(&s, &s.member_session, &event, "Going").await.0, Status::Ok);
    assert!(db.mark_server_event_reminder(&id, 60).await.unwrap());

    // a new title is no notice
    let (status, body) = edit(&s, &event, json!({ "title": "Board games" })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert!(sent_notices::of(&id).is_empty());

    let (status, body) = edit(&s, &event, json!({ "starts_at": at(3), "ends_at": at(5) })).await;
    assert_eq!(status, Status::Ok, "{body}");
    let sent = sent_notices::of(&id);
    assert_eq!(sent, vec![(EventNotice::Changed, vec![s.member.id.clone()])]);
    assert!(db.fetch_server_event(&id).await.unwrap().reminders_sent.is_empty());

    // a new place is one more notice; the same place again is none
    let place = json!({ "type": "External", "location": "Library" });
    let (status, body) = edit(&s, &event, json!({ "place": place.clone() })).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(sent_notices::of(&id).len(), 2);
    let (status, _) = edit(&s, &event, json!({ "place": place })).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(sent_notices::of(&id).len(), 2);
}

#[rocket::async_test]
async fn invited_people_are_told_when_they_are_invited() {
    let s = setup(true).await;
    let (_, second) = join(&s).await;
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Invited", "invited": [s.member.id, s.owner.id] }),
    )
    .await;
    let id = id_of(&event);
    // the person who invites is not told about their own invitation
    assert_eq!(
        sent_notices::of(&id),
        vec![(EventNotice::Invited, vec![s.member.id.clone()])]
    );

    let (status, body) = edit(
        &s,
        &event,
        json!({ "invited": [s.member.id, s.owner.id, second.id] }),
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    let sent = sent_notices::of(&id);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[1], (EventNotice::Invited, vec![second.id.clone()]));

    // a draft tells nobody until it is published
    let draft = create_with(
        &s,
        park(),
        json!({ "draft": true, "visibility": "Invited", "invited": [s.member.id] }),
    )
    .await;
    let draft_id = id_of(&draft);
    assert!(sent_notices::of(&draft_id).is_empty());
    let (status, body) = call(
        &s.harness,
        Method::Post,
        &format!("{}/publish", event_path(&s, &draft)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(
        sent_notices::of(&draft_id),
        vec![(EventNotice::Invited, vec![s.member.id.clone()])]
    );
}

#[rocket::async_test]
async fn a_freed_seat_tells_the_person_who_gets_it() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let id = id_of(&event);
    let (waiting_session, waiting) = join(&s).await;
    assert_eq!(answer(&s, &s.member_session, &event, "Going").await.0, Status::Ok);
    let (_, queued) = answer(&s, &waiting_session, &event, "Going").await;
    assert_eq!(queued["rsvp"]["waiting"], true, "{queued}");
    assert!(sent_notices::of(&id).is_empty());

    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(
        sent_notices::of(&id),
        vec![(EventNotice::Seated, vec![waiting.id.clone()])]
    );
}

#[rocket::async_test]
async fn a_share_link_shows_outsiders_a_little_and_only_when_opened() {
    let s = setup(true).await;
    let event = create(&s, park()).await;
    let link_route = format!("{}/link", event_path(&s, &event));
    let (_, outsider_session, _) = s.harness.new_user().await;

    // only organisers make links
    let (status, error) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.member_session,
        Some(json!({ "name": "Quiz" })),
    )
    .await;
    assert_eq!(status, Status::Forbidden, "{error}");

    let (status, linked) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.owner_session,
        Some(json!({ "name": " Quiz Night! " })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{linked}");
    assert_eq!(linked["link"]["name"], "quiz-night");
    assert_eq!(linked["link_public"], false);
    let code = linked["link"]["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 8);

    // another name keeps the code; the old path still leads here
    let (_, renamed) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.owner_session,
        Some(json!({ "name": "games" })),
    )
    .await;
    assert_eq!(renamed["link"]["code"], code.as_str());
    assert_eq!(renamed["link"]["name"], "games");
    let path = format!("/invites/event/quiz-night-{code}");

    // a member who sees the event learns where to open it
    let (status, preview) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{preview}");
    assert_eq!(preview["title"], "Game night");
    assert_eq!(preview["server"], s.server.id.as_str());
    assert_eq!(preview["event"], event["_id"]);

    // closed to everyone else, signed in or not
    let (status, _) = call(&s.harness, Method::Get, &path, &outsider_session, None).await;
    assert_eq!(status, Status::NotFound);
    assert_eq!(anonymous_get(&s, &path).await.0, Status::NotFound);

    // opening it needs the platform switch
    let (status, error) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.owner_session,
        Some(json!({ "public": true })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "FeatureDisabled");

    overwrite_config(|config| config.features.server_event_public_links = true).await;
    let (status, opened) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.owner_session,
        Some(json!({ "public": true })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{opened}");
    assert_eq!(opened["link_public"], true);
    assert_eq!(opened["link"]["name"], "games");

    // someone is going: an outsider must not learn that
    assert_eq!(answer(&s, &s.member_session, &event, "Going").await.0, Status::Ok);
    for (status, preview) in [
        call(&s.harness, Method::Get, &path, &outsider_session, None).await,
        anonymous_get(&s, &path).await,
    ] {
        assert_eq!(status, Status::Ok, "{preview}");
        let mut keys: Vec<&str> = preview
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["ends_at", "server_name", "starts_at", "status", "title"],
            "{preview}"
        );
        assert_eq!(preview["server_name"], s.server.name.as_str());
    }

    // a path that only looks like a link
    assert_eq!(anonymous_get(&s, "/invites/event/quiz-night").await.0, Status::NotFound);
    assert_eq!(anonymous_get(&s, "/invites/event/bbbbbbbb").await.0, Status::NotFound);

    // taken away: gone for everyone, and a new link has a new code
    let (status, removed) =
        call(&s.harness, Method::Delete, &link_route, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{removed}");
    assert!(removed.get("link").is_none(), "{removed}");
    assert_eq!(removed["link_public"], false);
    assert_eq!(anonymous_get(&s, &path).await.0, Status::NotFound);
    let (status, _) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::NotFound);
    let (_, again) = call(
        &s.harness,
        Method::Put,
        &link_route,
        &s.owner_session,
        Some(json!({})),
    )
    .await;
    assert_ne!(again["link"]["code"], code.as_str());
}

#[rocket::async_test]
async fn an_opened_link_of_a_private_event_names_nobody_and_a_draft_stays_closed() {
    let s = setup(true).await;
    overwrite_config(|config| config.features.server_event_public_links = true).await;
    let (other_session, _) = join(&s).await;
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Invited", "invited": [s.member.id] }),
    )
    .await;
    let (status, linked) = call(
        &s.harness,
        Method::Put,
        &format!("{}/link", event_path(&s, &event)),
        &s.owner_session,
        Some(json!({ "public": true })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{linked}");
    let path = format!("/invites/event/{}", linked["link"]["code"].as_str().unwrap());

    // a member who is not invited gets what an outsider gets: no ids
    let (status, preview) = call(&s.harness, Method::Get, &path, &other_session, None).await;
    assert_eq!(status, Status::Ok, "{preview}");
    assert!(preview.get("event").is_none() && preview.get("server").is_none(), "{preview}");
    assert!(preview.get("invited").is_none() && preview.get("counts").is_none());
    // the invited member gets the ids
    let (_, preview) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(preview["event"], event["_id"]);

    let draft = create_with(&s, park(), json!({ "draft": true })).await;
    let (status, linked) = call(
        &s.harness,
        Method::Put,
        &format!("{}/link", event_path(&s, &draft)),
        &s.owner_session,
        Some(json!({ "public": true })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{linked}");
    let path = format!("/invites/event/{}", linked["link"]["code"].as_str().unwrap());
    assert_eq!(anonymous_get(&s, &path).await.0, Status::NotFound);
}

#[rocket::async_test]
async fn an_invitation_link_adds_a_member_to_the_invited_people() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let (guest_session, guest) = join(&s).await;
    let (late_session, _) = join(&s).await;
    let (_, outsider_session, _) = s.harness.new_user().await;

    // only an event for invited people has invitation links
    let open = create(&s, park()).await;
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &format!("{}/invitations", event_path(&s, &open)),
        &s.owner_session,
        Some(json!({})),
    )
    .await;
    assert_ne!(status, Status::Ok);
    assert_eq!(error["type"], "ServerEventInvalid", "{error}");

    let event = create_with(&s, park(), json!({ "visibility": "Invited", "invited": [] })).await;
    let id = id_of(&event);
    let route = format!("{}/invitations", event_path(&s, &event));
    let (status, _) = call(&s.harness, Method::Post, &route, &s.member_session, Some(json!({}))).await;
    assert_eq!(status, Status::NotFound, "an event the member cannot see");

    let (status, once) = call(
        &s.harness,
        Method::Post,
        &route,
        &s.owner_session,
        Some(json!({ "max_uses": 1, "expires_in": 3600 })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{once}");
    let code = once["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 16);
    assert_eq!(once["uses"], 0);
    assert!(once["expires_at"].is_string());
    let accept = format!("/invites/event/{code}");

    // the guest cannot see the event yet
    let (status, _) = call(&s.harness, Method::Get, &event_path(&s, &event), &guest_session, None).await;
    assert_eq!(status, Status::NotFound);

    // someone outside the server is told to join first, and uses nothing
    let (status, error) = call(&s.harness, Method::Post, &accept, &outsider_session, None).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "InvalidOperation");

    // the owner sees it already: nothing is used
    let (status, body) = call(&s.harness, Method::Post, &accept, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{body}");
    assert_eq!(db.fetch_server_event(&id).await.unwrap().invitations[0].uses, 0);

    let (status, accepted) = call(&s.harness, Method::Post, &accept, &guest_session, None).await;
    assert_eq!(status, Status::Ok, "{accepted}");
    assert_eq!(accepted["event"], id.as_str());
    assert_eq!(accepted["server"], s.server.id.as_str());
    let (status, seen) = call(&s.harness, Method::Get, &event_path(&s, &event), &guest_session, None).await;
    assert_eq!(status, Status::Ok, "{seen}");
    let stored = db.fetch_server_event(&id).await.unwrap();
    assert_eq!(stored.invited, vec![guest.id.clone()]);
    assert_eq!(stored.invitations[0].uses, 1);

    // again by the same person: nothing more is used
    let (status, _) = call(&s.harness, Method::Post, &accept, &guest_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(db.fetch_server_event(&id).await.unwrap().invitations[0].uses, 1);

    // used up for the next person
    let (status, _) = call(&s.harness, Method::Post, &accept, &late_session, None).await;
    assert_eq!(status, Status::NotFound);

    // the organisers read the links with their uses
    let (status, list) = call(&s.harness, Method::Get, &route, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{list}");
    assert_eq!(list["invitations"][0]["uses"], 1);
    let (status, _) = call(&s.harness, Method::Get, &route, &guest_session, None).await;
    assert_eq!(status, Status::Forbidden);

    // a link that is taken away stops working; the guest stays invited
    let (_, open_link) = call(&s.harness, Method::Post, &route, &s.owner_session, Some(json!({}))).await;
    let open_code = open_link["code"].as_str().unwrap().to_string();
    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{route}/{open_code}"),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("/invites/event/{open_code}"),
        &late_session,
        None,
    )
    .await;
    assert_eq!(status, Status::NotFound);
    assert_eq!(db.fetch_server_event(&id).await.unwrap().invited, vec![guest.id.clone()]);

    // the invited list never reaches the gateway copy of the event
    let (_, shown) = call(&s.harness, Method::Get, &event_path(&s, &event), &guest_session, None).await;
    assert!(shown["event"].get("invitations").is_none(), "{shown}");
}

#[rocket::async_test]
async fn an_event_has_at_most_ten_invitation_links() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "visibility": "Invited", "invited": [] })).await;
    let route = format!("{}/invitations", event_path(&s, &event));
    for _ in 0..10 {
        let (status, body) = call(&s.harness, Method::Post, &route, &s.owner_session, Some(json!({}))).await;
        assert_eq!(status, Status::Ok, "{body}");
    }
    let (status, error) = call(&s.harness, Method::Post, &route, &s.owner_session, Some(json!({}))).await;
    assert_ne!(status, Status::Ok);
    assert_eq!(error["reason"], "too_many", "{error}");
    let (_, list) = call(&s.harness, Method::Get, &route, &s.owner_session, None).await;
    assert_eq!(list["invitations"].as_array().unwrap().len(), 10);
}
