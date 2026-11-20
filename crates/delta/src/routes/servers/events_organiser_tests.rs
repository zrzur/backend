//! Organising events: drafts and publishing, copies, helpers, the Manage
//! Events permission, removing and seating people, the audit log, the clash
//! check, statistics, the list of one person's events across servers, and
//! what goes when a server or an account is deleted.
//! Runs on REFERENCE and MONGODB (TEST_DB).

use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::events::client::EventV1;
use revolt_database::{AuditLogQuery, Member, PartialServer, ServerEvent, Session};
use revolt_models::v0::{self, ServerEventAnswer};
use revolt_permissions::{ChannelPermission, OverrideField};
use rocket::http::{ContentType, Header, Method, Status};
use serde_json::{json, Value};

use super::events_answers_tests::{
    about, answer, assert_silent, create_with, event_path, expect_event, join, park,
};
use super::events_tests::{at, body, call, create, setup, voice_place, Setup};
use crate::util::test::PubSubTestHelper;

fn events_path(s: &Setup) -> String {
    format!("/servers/{}/events", s.server.id)
}

fn id_of(event: &Value) -> String {
    event["_id"].as_str().expect("an id").to_string()
}

fn listed(list: &Value, id: &str) -> bool {
    list["events"]
        .as_array()
        .expect("events")
        .iter()
        .any(|event| event["_id"] == id)
}

/// The audit log of the server as (type, user, target)
async fn audit(s: &Setup) -> Vec<(String, String, Option<String>)> {
    s.harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|entry| {
            let action = serde_json::to_value(&entry.action).unwrap();
            (
                action["type"].as_str().unwrap().to_string(),
                entry.user,
                entry.target,
            )
        })
        .collect()
}

/// Give a member a role with these permissions
async fn grant(s: &Setup, user: &str, permissions: u64) {
    let role = s
        .harness
        .new_role(
            &s.server,
            1,
            Some(OverrideField {
                a: permissions as i64,
                d: 0,
            }),
        )
        .await;
    let mut member = s.harness.db.fetch_member(&s.server.id, user).await.unwrap();
    member
        .update(
            &s.harness.db,
            revolt_database::PartialMember {
                roles: Some(vec![role.id.clone()]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
}

#[rocket::async_test]
async fn a_draft_is_for_its_organisers_until_it_is_published() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let draft = create_with(&s, park(), json!({ "draft": true })).await;
    let id = id_of(&draft);
    let path = event_path(&s, &draft);
    assert_eq!(draft["status"], "Draft");
    assert_silent(&mut everyone, "a draft on the gateway", |e| about(e, &id)).await;

    // members do not find it, with or without asking for drafts
    for query in ["", "?drafts=true"] {
        let (status, list) = call(
            &s.harness,
            Method::Get,
            &format!("{}{query}", events_path(&s)),
            &s.member_session,
            None,
        )
        .await;
        assert_eq!(status, Status::Ok);
        assert!(!listed(&list, &id), "{list}");
    }
    let (status, _) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::NotFound);
    let (status, _) =
        call(&s.harness, Method::Post, &format!("{path}/publish"), &s.member_session, None).await;
    assert_eq!(status, Status::NotFound);

    // the organiser sees it when asking for drafts; the plain list is as
    // the older app expects it
    let (_, plain) = call(&s.harness, Method::Get, &events_path(&s), &s.owner_session, None).await;
    assert!(!listed(&plain, &id), "{plain}");
    let (_, with_drafts) = call(
        &s.harness,
        Method::Get,
        &format!("{}?drafts=true", events_path(&s)),
        &s.owner_session,
        None,
    )
    .await;
    assert!(listed(&with_drafts, &id), "{with_drafts}");

    // nobody answers a draft, and nothing starts or ends it
    let (status, _) = answer(&s, &s.owner_session, &draft, "Going").await;
    assert_eq!(status, Status::BadRequest);
    for action in ["start", "end", "cancel"] {
        let (status, _) =
            call(&s.harness, Method::Post, &format!("{path}/{action}"), &s.owner_session, None).await;
        assert_eq!(status, Status::BadRequest, "{action}");
    }
    let report = ServerEvent::run_pass(db, Timestamp::now_utc() + Duration::hours(5))
        .await
        .unwrap();
    assert_eq!(report.started, 0, "{report:?}");
    assert_eq!(
        db.fetch_server_event(&id).await.unwrap().status,
        v0::ServerEventStatus::Draft
    );

    // a draft may hold a start that has passed, but is not published so
    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "starts_at": at(-30), "remove": ["EndsAt"], "title": "Later" })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    let (status, error) =
        call(&s.harness, Method::Post, &format!("{path}/publish"), &s.owner_session, None).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "FailedValidation");
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "starts_at": at(1) })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_silent(&mut everyone, "edits of a draft on the gateway", |e| about(e, &id)).await;

    let (status, published) =
        call(&s.harness, Method::Post, &format!("{path}/publish"), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{published}");
    assert_eq!(published["status"], "Scheduled");
    assert_eq!(published["title"], "Later");
    expect_event(&mut everyone, "the published event", |e| {
        matches!(e, EventV1::ServerEventCreate(created) if created.id == id)
    })
    .await;
    let (_, list) = call(&s.harness, Method::Get, &events_path(&s), &s.member_session, None).await;
    assert!(listed(&list, &id), "{list}");
    // published once
    let (status, _) =
        call(&s.harness, Method::Post, &format!("{path}/publish"), &s.owner_session, None).await;
    assert_eq!(status, Status::BadRequest);
    // crond starts it at its time like any other event
    let report = ServerEvent::run_pass(db, Timestamp::now_utc() + Duration::hours(2))
        .await
        .unwrap();
    assert_eq!(report.started, 1, "{report:?}");
}

#[rocket::async_test]
async fn a_server_has_a_limit_of_drafts_of_its_own() {
    let s = setup(true).await;
    for n in 0..25 {
        let (status, draft) = call(
            &s.harness,
            Method::Post,
            &events_path(&s),
            &s.owner_session,
            Some({
                let mut data = body(park());
                data["draft"] = json!(true);
                data
            }),
        )
        .await;
        assert_eq!(status, Status::Ok, "draft {n}: {draft}");
    }
    let mut data = body(park());
    data["draft"] = json!(true);
    let (status, error) =
        call(&s.harness, Method::Post, &events_path(&s), &s.owner_session, Some(data)).await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "TooManyServerEventDrafts");
    assert_eq!(error["max"], 25);
    // drafts do not use up the places of scheduled events
    let event = create(&s, park()).await;
    assert_eq!(event["status"], "Scheduled");
    // and a copy is a draft too
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &format!("{}/copy", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "TooManyServerEventDrafts");
}

#[rocket::async_test]
async fn a_copy_is_a_draft_with_the_same_settings_and_nobodys_answers() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let role = s.harness.new_role(&s.server, 5, None).await;
    let source = create_with(
        &s,
        voice_place(&s),
        json!({
            "capacity": 5,
            "visibility": "Roles",
            "visible_roles": [role.id],
            "attendee_list": "Participants",
            "repeat": "Weekly",
            "co_organisers": [s.member.id],
            "time_zone": "UTC",
        }),
    )
    .await;
    answer(&s, &s.owner_session, &source, "Going").await;
    let path = format!("{}/copy", event_path(&s, &source));

    // a helper of the event may not copy it: making events needs the permission
    let (status, _) = call(&s.harness, Method::Post, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);

    let (status, copy) = call(&s.harness, Method::Post, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{copy}");
    assert_ne!(copy["_id"], source["_id"]);
    assert_eq!(copy["status"], "Draft");
    assert_eq!(copy["creator"], s.owner.id.as_str());
    for same in [
        "title",
        "description",
        "starts_at",
        "ends_at",
        "place",
        "capacity",
        "waitlist",
        "visibility",
        "visible_roles",
        "attendee_list",
        "repeat",
        "co_organisers",
        "time_zone",
    ] {
        assert_eq!(copy[same], source[same], "{same}");
    }
    assert_eq!(copy["counts"], json!({ "going": 0, "maybe": 0, "not_going": 0, "waiting": 0 }));
    assert!(copy.get("previous").is_none_or(Value::is_null));
    let copy_id = id_of(&copy);
    assert!(db
        .fetch_server_event_answer(&copy_id, &s.owner.id)
        .await
        .unwrap()
        .is_none());
    let roster = db.fetch_server_event_roster(&copy_id).await.unwrap().expect("its seat list");
    assert_eq!(roster.capacity, Some(5));
    assert!(roster.going.is_empty());

    // a past event can be copied as well
    let mut stored = db.fetch_server_event(&id_of(&source)).await.unwrap();
    assert!(stored.cancel(db, true).await.unwrap());
    let (status, again) = call(&s.harness, Method::Post, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{again}");
    assert_eq!(again["status"], "Draft");
}

#[rocket::async_test]
async fn helpers_manage_their_event_and_nothing_else() {
    let s = setup(true).await;
    let event = create_with(
        &s,
        voice_place(&s),
        json!({ "attendee_list": "Organisers", "visibility": "Invited", "invited": [] }),
    )
    .await;
    let path = event_path(&s, &event);

    // not a helper yet: the invited-only event is not even there
    let (status, _) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::NotFound);

    // only members of the server can help
    let (_, _, stranger) = s.harness.new_user().await;
    let (status, error) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "co_organisers": [stranger.id] })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["field"], "co_organisers");
    assert_eq!(error["reason"], "not_a_member");

    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "co_organisers": [s.member.id] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(edited["co_organisers"], json!([s.member.id]));

    // the helper sees it as an organiser does
    let (status, seen) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{seen}");
    assert_eq!(seen["organiser"], true);
    assert_eq!(seen["event"]["invited"], json!([]));

    // and manages this event
    let (status, renamed) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.member_session,
        Some(json!({ "title": "By the helper", "capacity": 3 })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{renamed}");
    assert_eq!(renamed["title"], "By the helper");
    for read in ["attendees", "stats"] {
        let (status, body) =
            call(&s.harness, Method::Get, &format!("{path}/{read}"), &s.member_session, None).await;
        assert_eq!(status, Status::Ok, "{read}: {body}");
    }

}

/// An event of the owner with the member as its helper
async fn helped_event(s: &Setup) -> Value {
    create_with(
        s,
        voice_place(s),
        json!({ "attendee_list": "Organisers", "co_organisers": [s.member.id] }),
    )
    .await
}

#[rocket::async_test]
async fn helpers_name_no_helpers_delete_nothing_and_make_no_events() {
    let s = setup(true).await;
    let event = helped_event(&s).await;
    let other = create(&s, park()).await;
    let path = event_path(&s, &event);
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.member_session,
        Some(json!({ "co_organisers": [s.member.id, s.owner.id] })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = call(&s.harness, Method::Delete, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &events_path(&s),
        &s.member_session,
        Some(body(park())),
    )
    .await;
    assert_eq!(status, Status::Forbidden);
    assert_eq!(error["permission"], "ManageEvents");
    // another event of the server is not theirs
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &event_path(&s, &other),
        &s.member_session,
        Some(json!({ "title": "Not mine" })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);

}

#[rocket::async_test]
async fn a_helper_starts_and_ends_the_event_unless_timed_out_and_is_in_the_audit_log() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = helped_event(&s).await;
    let path = event_path(&s, &event);
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.member_session,
        Some(json!({ "title": "By the helper" })),
    )
    .await;
    assert_eq!(status, Status::Ok);

    // timed out: refused
    let mut member = db.fetch_member(&s.server.id, &s.member.id).await.unwrap();
    member
        .update(
            db,
            revolt_database::PartialMember {
                timeout: Some(Timestamp::now_utc() + Duration::hours(1)),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let (status, _) =
        call(&s.harness, Method::Post, &format!("{path}/start"), &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);
    member
        .update(db, Default::default(), vec![revolt_database::FieldsMember::Timeout])
        .await
        .unwrap();

    let (status, started) =
        call(&s.harness, Method::Post, &format!("{path}/start"), &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{started}");
    assert_eq!(started["status"], "Active");
    let (status, ended) =
        call(&s.harness, Method::Post, &format!("{path}/end"), &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{ended}");

    // the audit log names the helper for each action
    let log = audit(&s).await;
    for kind in ["ServerEventEdit", "ServerEventStart", "ServerEventEnd"] {
        assert!(
            log.iter().any(|(name, user, _)| name == kind && user == &s.member.id),
            "{kind} by the helper missing in {log:?}"
        );
    }

    // a helper who leaves the server is a helper no more
    let helped = create_with(&s, park(), json!({ "co_organisers": [s.member.id] })).await;
    db.fetch_member(&s.server.id, &s.member.id)
        .await
        .unwrap()
        .remove(db, &s.server, revolt_database::RemovalIntention::Leave, true)
        .await
        .unwrap();
    let stored = db.fetch_server_event(&id_of(&helped)).await.unwrap();
    assert!(stored.co_organisers.is_empty(), "{:?}", stored.co_organisers);
}

#[rocket::async_test]
async fn manage_events_is_a_permission_of_its_own() {
    let s = setup(true).await;
    assert_eq!(ChannelPermission::ManageEvents as u64, 1 << 49);
    // never a default
    assert_eq!(
        *revolt_permissions::DEFAULT_PERMISSION_SERVER & ChannelPermission::ManageEvents as u64,
        0
    );
    // inside the "safe" set: owners and Administrator hold it
    assert_ne!(
        ChannelPermission::GrantAllSafe as u64 & ChannelPermission::ManageEvents as u64,
        0
    );
    let (status, error) = call(
        &s.harness,
        Method::Post,
        &events_path(&s),
        &s.member_session,
        Some(body(park())),
    )
    .await;
    assert_eq!(status, Status::Forbidden, "{error}");
    assert_eq!(error["type"], "MissingPermission");
    assert_eq!(error["permission"], "ManageEvents");

    grant(&s, &s.member.id, ChannelPermission::ManageEvents as u64).await;
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &events_path(&s),
        &s.member_session,
        Some(body(park())),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    let path = event_path(&s, &event);
    // every event of the server, not only their own
    let owners = create(&s, park()).await;
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &event_path(&s, &owners),
        &s.member_session,
        Some(json!({ "title": "Edited", "co_organisers": [s.owner.id] })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (status, _) = call(&s.harness, Method::Delete, &path, &s.member_session, None).await;
    assert_eq!(status, Status::NoContent);
    // it is not Manage Server: the server itself stays closed to them
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &format!("/servers/{}", s.server.id),
        &s.member_session,
        Some(json!({ "name": "Mine now" })),
    )
    .await;
    assert_eq!(status, Status::Forbidden);

    // Manage Server still implies it
    let (session, user) = join(&s).await;
    grant(&s, &user.id, ChannelPermission::ManageServer as u64).await;
    let (status, event) =
        call(&s.harness, Method::Post, &events_path(&s), &session, Some(body(park()))).await;
    assert_eq!(status, Status::Ok, "{event}");
}

#[rocket::async_test]
async fn organisers_remove_and_seat_people_and_the_audit_log_names_it() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let id = id_of(&event);
    let path = event_path(&s, &event);
    answer(&s, &s.member_session, &event, "Going").await;
    let (guest_session, guest) = join(&s).await;
    let (_, queued) = answer(&s, &guest_session, &event, "Going").await;
    assert_eq!(queued["rsvp"]["waiting"], true);
    let seat = format!("{path}/attendees/{}/seat", guest.id);
    let remove = format!("{path}/attendees/{}", s.member.id);

    // people taking part manage nobody
    let (status, _) = call(&s.harness, Method::Post, &seat, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);
    let (status, _) = call(&s.harness, Method::Delete, &remove, &guest_session, None).await;
    assert_eq!(status, Status::Forbidden);

    // every seat is taken: seating the waiting person is refused until the
    // organiser says they mean to go beyond the limit
    let (status, error) = call(&s.harness, Method::Post, &seat, &s.owner_session, None).await;
    assert_eq!(status, Status::Conflict, "{error}");
    assert_eq!(error["type"], "ServerEventOverCapacity");
    assert_eq!(error["capacity"], 1);
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{seat}?over_capacity=false"),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Conflict);
    let unchanged = db.fetch_server_event(&id).await.unwrap();
    assert_eq!((unchanged.going_count, unchanged.waitlist_count), (1, 1));
    assert!(!audit(&s).await.iter().any(|(kind, _, _)| kind == "ServerEventAttendeeSeat"));

    let mut guest_feed = PubSubTestHelper::new(&format!("{}!", guest.id)).await;
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let (status, seated) = call(
        &s.harness,
        Method::Post,
        &format!("{seat}?over_capacity=true"),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{seated}");
    assert_eq!(seated["counts"]["going"], 2);
    assert_eq!(seated["counts"]["waiting"], 0);
    // the event says it is over its limit, to everyone who sees it
    assert_eq!(seated["capacity"], 1);
    assert_eq!(seated["over_capacity"], true);
    expect_event(&mut everyone, "the event is over its limit", |e| {
        matches!(e, EventV1::ServerEventUpdate { event } if event.id == id && event.over_capacity)
    })
    .await;
    let (_, seen_by_member) =
        call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(seen_by_member["event"]["over_capacity"], true);
    // and the audit log says the limit was passed, and by whom
    let entries = s
        .harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert!(
        entries.iter().any(|entry| entry.user == s.owner.id
            && matches!(
                &entry.action,
                revolt_database::AuditLogEntryAction::ServerEventAttendeeSeat {
                    user,
                    over_capacity: true,
                    ..
                } if user == &guest.id
            )),
        "no audit entry for the seat beyond the limit"
    );
    expect_event(&mut guest_feed, "a seat from the organiser", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: Some(rsvp), .. }
            if event == &id && !rsvp.waiting)
    })
    .await;
    let row = db.fetch_server_event_answer(&id, &guest.id).await.unwrap().unwrap();
    assert!(!row.waiting);
    // someone who is not waiting is not seated this way
    let (status, _) = call(&s.harness, Method::Post, &seat, &s.owner_session, None).await;
    assert_eq!(status, Status::BadRequest);

    // the organiser removes a person: their answer is gone, they are told
    let mut member_feed = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;
    let (status, _) = call(&s.harness, Method::Delete, &remove, &s.owner_session, None).await;
    assert_eq!(status, Status::NoContent);
    expect_event(&mut member_feed, "removed by the organiser", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: None, .. } if event == &id)
    })
    .await;
    assert!(db.fetch_server_event_answer(&id, &s.member.id).await.unwrap().is_none());
    let stored = db.fetch_server_event(&id).await.unwrap();
    assert_eq!((stored.going_count, stored.waitlist_count), (1, 0));
    let (status, _) = call(&s.harness, Method::Delete, &remove, &s.owner_session, None).await;
    assert_eq!(status, Status::NotFound);
    // they may answer again
    let (status, back) = answer(&s, &s.member_session, &event, "Maybe").await;
    assert_eq!(status, Status::Ok, "{back}");

    // by hand: start and end leave their entries too
    for action in ["start", "end"] {
        let (status, _) =
            call(&s.harness, Method::Post, &format!("{path}/{action}"), &s.owner_session, None).await;
        assert_eq!(status, Status::Ok, "{action}");
    }
    let draft = create_with(&s, park(), json!({ "draft": true })).await;
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{}/publish", event_path(&s, &draft)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);

    let log = audit(&s).await;
    for (kind, target) in [
        ("ServerEventCreate", None),
        ("ServerEventAttendeeSeat", Some(guest.id.clone())),
        ("ServerEventAttendeeRemove", Some(s.member.id.clone())),
        ("ServerEventStart", None),
        ("ServerEventEnd", None),
        ("ServerEventPublish", None),
    ] {
        assert!(
            log.iter()
                .any(|(name, user, to)| name == kind && user == &s.owner.id && to == &target),
            "{kind} missing in {log:?}"
        );
    }
}

fn clash_query(s: &Setup, from: i64, to: i64, more: &str) -> String {
    let time = |hours: i64| at(hours).as_str().unwrap().replace('+', "%2B");
    format!(
        "{}/clashes?starts_at={}&ends_at={}{more}",
        events_path(s),
        time(from),
        time(to)
    )
}

#[rocket::async_test]
async fn the_clash_check_names_overlapping_events_and_stops_nothing() {
    let s = setup(true).await;
    // in the voice channel from 2 to 4 hours from now
    let event = create(&s, voice_place(&s)).await;
    let id = id_of(&event);
    let outside = create(&s, json!({ "type": "External", "location": "Town Hall" })).await;
    let draft = create_with(&s, voice_place(&s), json!({ "draft": true })).await;
    let voice = format!("&channel={}", s.voice.id());
    let ids = |clashes: &Value| -> Vec<String> {
        clashes["events"].as_array().unwrap().iter().map(id_of).collect()
    };

    let (status, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 3, 5, &voice), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{found}");
    // the organiser is warned of the draft too
    let mut expected = vec![id.clone(), id_of(&draft)];
    expected.sort();
    let mut got = ids(&found);
    got.sort();
    assert_eq!(got, expected);
    // a member is not told of the draft
    let (_, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 3, 5, &voice), &s.member_session, None).await;
    assert_eq!(ids(&found), vec![id.clone()]);

    // another channel, or another time: nothing
    let text = format!("&channel={}", s.text.id());
    let (_, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 3, 5, &text), &s.owner_session, None).await;
    assert_eq!(found["events"], json!([]));
    let (_, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 5, 6, &voice), &s.owner_session, None).await;
    assert_eq!(found["events"], json!([]));
    // over before the other starts: no clash
    let (_, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 0, 1, &voice), &s.member_session, None).await;
    assert_eq!(found["events"], json!([]));

    // an outside place, whatever the capitals
    let (_, found) = call(
        &s.harness,
        Method::Get,
        &clash_query(&s, 3, 5, "&location=town%20hall"),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(ids(&found), vec![id_of(&outside)]);
    // no place given: everything at that time
    let (_, found) =
        call(&s.harness, Method::Get, &clash_query(&s, 3, 5, ""), &s.member_session, None).await;
    assert_eq!(found["events"].as_array().unwrap().len(), 2);
    // the event being edited is left out
    let (_, found) = call(
        &s.harness,
        Method::Get,
        &clash_query(&s, 3, 5, &format!("{voice}&exclude={id}")),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(found["events"], json!([]));

    // a time that is no time
    let (status, error) = call(
        &s.harness,
        Method::Get,
        &format!("{}/clashes?starts_at=tomorrow", events_path(&s)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "ServerEventInvalid");
    assert_eq!(error["reason"], "not_a_time");

    // a warning only: a second event at the same time and place is made
    let second = create(&s, voice_place(&s)).await;
    assert_eq!(second["status"], "Scheduled");
}

#[rocket::async_test]
async fn statistics_are_for_organisers() {
    let s = setup(true).await;
    let event = create_with(&s, voice_place(&s), json!({ "capacity": 1 })).await;
    let path = format!("{}/stats", event_path(&s, &event));
    answer(&s, &s.owner_session, &event, "Going").await;
    answer(&s, &s.member_session, &event, "Going").await;
    let (maybe_session, _) = join(&s).await;
    answer(&s, &maybe_session, &event, "Maybe").await;
    let (no_session, _) = join(&s).await;
    answer(&s, &no_session, &event, "NotGoing").await;
    // someone who answered and took it back is in no number
    let (gone_session, _) = join(&s).await;
    answer(&s, &gone_session, &event, "Maybe").await;
    call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &gone_session,
        None,
    )
    .await;

    let (status, _) = call(&s.harness, Method::Get, &path, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);

    let (status, stats) = call(&s.harness, Method::Get, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{stats}");
    assert_eq!(
        stats["counts"],
        json!({ "going": 1, "maybe": 1, "not_going": 1, "waiting": 1 })
    );
    assert_eq!(stats["capacity"], 1);
    let days = stats["registrations"].as_array().unwrap();
    let sum = |name: &str| days.iter().map(|day| day[name].as_u64().unwrap()).sum::<u64>();
    // the going answers of the day, seated or waiting
    assert_eq!((sum("going"), sum("maybe"), sum("not_going")), (2, 1, 1));
    assert!(days.iter().all(|day| day["day"].as_str().unwrap().len() == 10), "{stats}");
    assert!(stats.get("present_at_start").is_none_or(Value::is_null));
    // no names anywhere in it
    assert!(!stats.to_string().contains(&s.member.id), "{stats}");

    // started: a voice place notes how many were in the channel, as a number
    // (two people are put into the channel as the voice code does it)
    let room = revolt_database::voice::UserVoiceChannel {
        id: s.voice.id().to_string(),
        server_id: Some(s.server.id.clone()),
    };
    {
        use redis_kiss::AsyncCommands;
        let mut redis = redis_kiss::get_connection().await.unwrap();
        for user in [&s.owner.id, &s.member.id] {
            redis
                .sadd::<_, _, ()>(format!("{}{}", revolt_database::VOICE_MEMBERS_KEY, room.id), user)
                .await
                .unwrap();
        }
    }
    // the same set the voice code reads
    let inside = revolt_database::voice::get_voice_channel_members(&room)
        .await
        .unwrap()
        .unwrap_or_default();
    assert_eq!(inside.len(), 2, "{inside:?}");
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{}/start", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (_, stats) = call(&s.harness, Method::Get, &path, &s.owner_session, None).await;
    assert_eq!(stats["present_at_start"], 2, "{stats}");
    // a number only: nobody who was there is named
    assert!(!stats.to_string().contains(&s.owner.id), "{stats}");
    assert!(stats["started_at"].is_string());

    // an outside place has no such number
    let outside = create(&s, park()).await;
    call(
        &s.harness,
        Method::Post,
        &format!("{}/start", event_path(&s, &outside)),
        &s.owner_session,
        None,
    )
    .await;
    let (_, stats) = call(
        &s.harness,
        Method::Get,
        &format!("{}/stats", event_path(&s, &outside)),
        &s.owner_session,
        None,
    )
    .await;
    assert!(stats.get("present_at_start").is_none_or(Value::is_null), "{stats}");
}

/// Another server the owner and the member are in, with events on or off
async fn second_server(s: &Setup, events_on: bool) -> String {
    let (server, _) = s.harness.new_server(&s.owner).await;
    Member::create(&s.harness.db, &server, &s.owner, None)
        .await
        .expect("owner");
    Member::create(&s.harness.db, &server, &s.member, None)
        .await
        .expect("member");
    s.harness
        .db
        .update_server(
            &server.id,
            &PartialServer {
                events_enabled: Some(events_on),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    server.id
}

async fn create_in(s: &Setup, server: &str, data: Value) -> Value {
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{server}/events"),
        &s.owner_session,
        Some(data),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    event
}

#[rocket::async_test]
async fn my_events_holds_what_i_may_see_across_my_servers() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let other = second_server(&s, true).await;
    let off = second_server(&s, false).await;

    let mut soon = body(park());
    soon["starts_at"] = at(1);
    soon["ends_at"] = at(2);
    let here = create_in(&s, &s.server.id, soon).await;
    let mut later = body(park());
    later["starts_at"] = at(30);
    later["ends_at"] = at(31);
    let there = create_in(&s, &other, later).await;
    // not for the member: invited-only, a draft, and one far ahead
    let mut hidden = body(park());
    hidden["visibility"] = json!("Invited");
    let hidden = create_in(&s, &other, hidden).await;
    let mut draft = body(park());
    draft["draft"] = json!(true);
    let draft = create_in(&s, &other, draft).await;
    let mut far = body(park());
    far["starts_at"] = at(24 * 200);
    far["ends_at"] = at(24 * 200 + 1);
    let far = create_in(&s, &other, far).await;
    // in a server with events switched off
    let silent = ServerEvent::new(
        &off,
        &s.owner.id,
        "Off".into(),
        None,
        Timestamp::now_utc() + Duration::hours(3),
        None,
        v0::ServerEventPlace::External {
            location: "Park".into(),
        },
        None,
    );
    db.insert_server_event(&silent).await.unwrap();

    let ids = |list: &Value| -> Vec<String> {
        list["events"].as_array().unwrap().iter().map(id_of).collect()
    };
    let (status, mine) =
        call(&s.harness, Method::Get, "/users/@me/events", &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{mine}");
    // soonest first, from both servers, and only what the member may see
    assert_eq!(ids(&mine), vec![id_of(&here), id_of(&there)]);
    assert_eq!(mine["answers"], json!([]));

    // the organiser sees the invited-only one too, but drafts are not on a calendar
    let (_, owners) =
        call(&s.harness, Method::Get, "/users/@me/events", &s.owner_session, None).await;
    let owners = ids(&owners);
    assert!(owners.contains(&id_of(&hidden)));
    assert!(!owners.contains(&id_of(&draft)));
    assert!(!owners.contains(&silent.id));

    // by answer
    let (status, _) = call(
        &s.harness,
        Method::Put,
        &format!("/servers/{other}/events/{}/rsvp", id_of(&there)),
        &s.member_session,
        Some(json!({ "answer": "Going" })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (_, going) = call(
        &s.harness,
        Method::Get,
        "/users/@me/events?answer=Going",
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(ids(&going), vec![id_of(&there)]);
    assert_eq!(going["answers"][0]["answer"], "Going");
    let (_, maybe) = call(
        &s.harness,
        Method::Get,
        "/users/@me/events?answer=Maybe",
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(maybe["events"], json!([]));
    let (_, any) =
        call(&s.harness, Method::Get, "/users/@me/events?answer=Any", &s.member_session, None).await;
    assert_eq!(ids(&any), vec![id_of(&there)]);

    // a time range
    let time = |hours: i64| at(hours).as_str().unwrap().replace('+', "%2B");
    let (status, ranged) = call(
        &s.harness,
        Method::Get,
        &format!("/users/@me/events?from={}&to={}", time(24 * 150), time(24 * 250)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{ranged}");
    assert_eq!(ids(&ranged), vec![id_of(&far)]);
    let (_, one) =
        call(&s.harness, Method::Get, "/users/@me/events?limit=1", &s.member_session, None).await;
    assert_eq!(ids(&one), vec![id_of(&here)]);
    for bad in [
        format!("/users/@me/events?from={}&to={}", time(5), time(1)),
        format!("/users/@me/events?from={}&to={}", time(0), time(24 * 500)),
        "/users/@me/events?from=soon".to_string(),
        "/users/@me/events?answer=Never".to_string(),
    ] {
        let (status, error) = call(&s.harness, Method::Get, &bad, &s.member_session, None).await;
        assert_eq!(status, Status::BadRequest, "{bad}: {error}");
        assert_eq!(error["type"], "ServerEventInvalid", "{bad}: {error}");
    }

    // someone in no server has no events
    let (_, loner, _) = s.harness.new_user().await;
    let (status, none) = call(&s.harness, Method::Get, "/users/@me/events", &loner, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(none["events"], json!([]));
}

#[rocket::async_test]
async fn a_time_range_lists_past_and_cancelled_events_too() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let done = create(&s, park()).await;
    let mut stored = db.fetch_server_event(&id_of(&done)).await.unwrap();
    assert!(stored.start(db).await.unwrap());
    assert!(stored.complete(db).await.unwrap());
    let off = create(&s, park()).await;
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{}/cancel", event_path(&s, &off)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let ahead = create(&s, park()).await;

    let (_, plain) = call(&s.harness, Method::Get, &events_path(&s), &s.member_session, None).await;
    assert!(listed(&plain, &id_of(&ahead)));
    assert!(!listed(&plain, &id_of(&done)) && !listed(&plain, &id_of(&off)));

    let time = |hours: i64| at(hours).as_str().unwrap().replace('+', "%2B");
    let (status, month) = call(
        &s.harness,
        Method::Get,
        &format!("{}?from={}&to={}", events_path(&s), time(-1), time(48)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{month}");
    for event in [&done, &off, &ahead] {
        assert!(listed(&month, &id_of(event)), "{month}");
    }
    let (_, earlier) = call(
        &s.harness,
        Method::Get,
        &format!("{}?from={}&to={}", events_path(&s), time(-48), time(-1)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(earlier["events"], json!([]));
}

/// The events of the server that is about to be deleted, and one of
/// another server that must not be touched
struct Doomed {
    /// Repeats weekly, one seat: the member holds it, the owner waits
    ahead: String,
    /// Started, with a "maybe"
    running: String,
    draft: String,
    /// Ended 30 days ago
    done: String,
    /// In another server: one seat, the member seated, the owner waiting
    elsewhere: String,
    other_server: String,
}

async fn doomed_events(s: &Setup) -> Doomed {
    let db = &s.harness.db;
    let ahead = create_with(s, park(), json!({ "capacity": 1, "repeat": "Weekly" })).await;
    answer(s, &s.member_session, &ahead, "Going").await;
    answer(s, &s.owner_session, &ahead, "Going").await;
    let running = create(s, park()).await;
    answer(s, &s.member_session, &running, "Maybe").await;
    let mut started = db.fetch_server_event(&id_of(&running)).await.unwrap();
    assert!(started.start(db).await.unwrap());
    let draft = create_with(s, park(), json!({ "draft": true })).await;
    let done = create(s, park()).await;
    ended(s, &id_of(&done), v0::ServerEventStatus::Completed, 30).await;

    let other_server = second_server(s, true).await;
    let mut data = body(park());
    data["capacity"] = json!(1);
    let elsewhere = create_in(s, &other_server, data).await;
    for session in [&s.member_session, &s.owner_session] {
        let (status, _) = call(
            &s.harness,
            Method::Put,
            &format!("/servers/{other_server}/events/{}/rsvp", id_of(&elsewhere)),
            session,
            Some(json!({ "answer": "Going" })),
        )
        .await;
        assert_eq!(status, Status::Ok);
    }
    Doomed {
        ahead: id_of(&ahead),
        running: id_of(&running),
        draft: id_of(&draft),
        done: id_of(&done),
        elsewhere: id_of(&elsewhere),
        other_server,
    }
}

/// The event of the other server is exactly as it was
async fn assert_elsewhere_untouched(s: &Setup, d: &Doomed) {
    let db = &s.harness.db;
    let event = db.fetch_server_event(&d.elsewhere).await.expect("the other server's event");
    assert_eq!(event.status, v0::ServerEventStatus::Scheduled);
    assert_eq!(event.server, d.other_server);
    assert!(event.people_removed_at.is_none());
    assert_eq!((event.going_count, event.waitlist_count), (1, 1));
    let roster = db.fetch_server_event_roster(&d.elsewhere).await.unwrap().expect("its seat list");
    assert_eq!(roster.going, vec![s.member.id.clone()]);
    assert_eq!(roster.waiting, vec![s.owner.id.clone()]);
    for user in [&s.member.id, &s.owner.id] {
        assert!(db.fetch_server_event_answer(&d.elsewhere, user).await.unwrap().is_some());
    }
}

/// Deleting a server while one event runs, one repeats with a waiting list
/// and one is a draft: all are called off, nothing is lost, nobody is told
#[rocket::async_test]
async fn deleting_a_server_calls_its_events_off_quietly_and_loses_nothing() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let d = doomed_events(&s).await;
    let before = db.fetch_server_event_roster(&d.ahead).await.unwrap().unwrap();
    assert_eq!((before.going.len(), before.waiting.len()), (1, 1));

    let (_connection, mut pushes) = push_queue().await;
    let mut member_feed = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;
    let mut owner_feed = PubSubTestHelper::new(&format!("{}!", s.owner.id)).await;
    let mut server_feed = PubSubTestHelper::new(&s.server.id).await;
    db.fetch_server(&s.server.id).await.unwrap().delete(db).await.unwrap();

    // nobody is told anything about the events: no gateway event, no push
    let ids = [&d.ahead, &d.running, &d.draft, &d.done, &d.elsewhere];
    for (feed, who) in [
        (&mut server_feed, "the server topic"),
        (&mut member_feed, "the member"),
        (&mut owner_feed, "the one waiting"),
    ] {
        assert_silent(feed, who, |e| ids.iter().any(|id| about(e, id))).await;
    }
    let pushed = push_arrives(&mut pushes, &ids, 1500).await;
    assert!(pushed.is_none(), "a push was queued: {pushed:?}");

    // what was to come, running or a draft is called off at this moment
    let now = Timestamp::now_utc();
    for id in [&d.ahead, &d.running, &d.draft] {
        let event = db.fetch_server_event(id).await.expect("the event is kept");
        assert_eq!(event.status, v0::ServerEventStatus::Cancelled, "{id}");
        let ended = event.ended_at.expect("when it was called off");
        assert!(now.duration_since(ended).whole_seconds().abs() < 120, "{id}");
    }
    assert!(db.fetch_server_event(&d.running).await.unwrap().started_at.is_some());
    assert_eq!(
        db.fetch_server_event(&d.done).await.unwrap().status,
        v0::ServerEventStatus::Completed
    );
    // the seat list, the line, the counts and every answer are as they were:
    // nobody moved up, and no next occurrence was made
    let after = db.fetch_server_event_roster(&d.ahead).await.unwrap().unwrap();
    assert_eq!(after, before);
    let kept = db.fetch_server_event(&d.ahead).await.unwrap();
    assert_eq!((kept.going_count, kept.waitlist_count), (1, 1));
    assert!(kept.next.is_none(), "a next occurrence was scheduled");
    assert!(db.fetch_server_event_answer(&d.ahead, &s.member.id).await.unwrap().is_some());
    assert!(db.fetch_server_event_answer(&d.ahead, &s.owner.id).await.unwrap().is_some());
    assert!(db.fetch_server_event_answer(&d.running, &s.member.id).await.unwrap().is_some());

    // crond passes three hours later, when every event made here would
    // have started: of the deleted server nothing starts and nothing new is
    // scheduled; the only event that starts is the other server's
    let report = ServerEvent::run_pass(db, now + Duration::hours(3)).await.unwrap();
    assert_eq!((report.started, report.scheduled, report.failed), (1, 0, 0), "{report:?}");
    assert_eq!(
        db.fetch_server_event(&d.elsewhere).await.unwrap().status,
        v0::ServerEventStatus::Active
    );
    for id in [&d.ahead, &d.running, &d.draft] {
        assert_eq!(
            db.fetch_server_event(id).await.unwrap().status,
            v0::ServerEventStatus::Cancelled,
            "{id}"
        );
    }
    ServerEvent::close_all_of_server(db, &s.server.id).await.unwrap();
    assert_eq!(db.fetch_server_event_roster(&d.ahead).await.unwrap().unwrap(), before);
    assert!(db.fetch_server_event(&d.ahead).await.unwrap().next.is_none());
    assert!(db.fetch_upcoming_server_events(&s.server.id).await.unwrap().is_empty());
    let gone = [&d.ahead, &d.running, &d.draft, &d.done];
    let pushed = push_arrives(&mut pushes, &gone, 800).await;
    assert!(pushed.is_none(), "crond told someone: {pushed:?}");
}

/// After a server is deleted its events and their answers stay for their
/// retention time and go when it is up; another server's event is never
/// touched by any of it
#[rocket::async_test]
async fn events_of_a_deleted_server_go_when_their_retention_time_is_up_and_not_before() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let d = doomed_events(&s).await;
    // one more event of the server, whose retention time was up before the
    // server is deleted: its people are gone already, the event is there
    let long_ago = people_event(&s, 2).await;
    ended(&s, &long_ago, v0::ServerEventStatus::Completed, 400).await;
    assert_eq!(ServerEvent::run_retention_pass(db, Timestamp::now_utc()).await.unwrap(), 1);
    assert!(db.fetch_server_event(&long_ago).await.unwrap().people_removed_at.is_some());

    db.fetch_server(&s.server.id).await.unwrap().delete(db).await.unwrap();
    assert_elsewhere_untouched(&s, &d).await;
    // nothing of that old event is kept for anyone any more: it goes with
    // the server, since no later pass would ever find it
    assert!(db.fetch_server_event(&long_ago).await.is_err());
    let now = Timestamp::now_utc();

    // 200 days on: everything of the deleted server is still there
    assert_eq!(ServerEvent::run_retention_pass(db, now + Duration::days(200)).await.unwrap(), 0);
    for id in [&d.ahead, &d.running, &d.draft, &d.done] {
        assert!(db.fetch_server_event(id).await.is_ok(), "{id}");
    }
    assert!(db.fetch_server_event_answer(&d.ahead, &s.member.id).await.unwrap().is_some());
    assert!(db.fetch_server_event_answer(&d.ahead, &s.owner.id).await.unwrap().is_some());
    assert!(db.fetch_server_event_roster(&d.ahead).await.unwrap().is_some());

    // twelve months after each ended they go as a whole: first the one
    // that had ended before the server was deleted, then the others
    let first_due = months_plus(db, &d.done).await;
    assert_eq!(
        ServerEvent::run_retention_pass(db, first_due - Duration::minutes(1)).await.unwrap(),
        0,
        "a minute early"
    );
    assert_eq!(ServerEvent::run_retention_pass(db, first_due).await.unwrap(), 1);
    assert!(db.fetch_server_event(&d.done).await.is_err());
    assert!(db.fetch_server_event(&d.ahead).await.is_ok());
    assert!(db.fetch_server_event_answer(&d.ahead, &s.member.id).await.unwrap().is_some());
    let all_due = months_plus(db, &d.ahead).await + Duration::minutes(5);
    assert_eq!(ServerEvent::run_retention_pass(db, all_due).await.unwrap(), 3);
    for id in [&d.ahead, &d.running, &d.draft] {
        assert!(db.fetch_server_event(id).await.is_err(), "{id}");
        assert!(db.fetch_server_event_roster(id).await.unwrap().is_none(), "{id}");
    }
    // the only answers left are those to the other server's event
    for user in [&s.member.id, &s.owner.id] {
        let rows = db.fetch_server_event_answers_by_user(user, None).await.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id.event, d.elsewhere);
    }
    assert_elsewhere_untouched(&s, &d).await;
    // the audit entries say what was done, with no text and no person
    let entries = db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let deleted = entries
        .iter()
        .filter(|entry| matches!(
            &entry.action,
            revolt_database::AuditLogEntryAction::ServerEventRetention { outcome, .. }
                if outcome == "EventDeleted"
        ))
        .count();
    assert_eq!(deleted, 4);
}

/// A person deletes their account after their server was deleted: their
/// answers to its kept events go at once (a deletion request does not wait
/// for the retention time), and nothing else moves: nobody is given their
/// seat in an event that is over, and nobody is told anything
#[rocket::async_test]
async fn an_account_deleted_after_its_server_moves_nobody_up_and_tells_nobody() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let d = doomed_events(&s).await;
    db.fetch_server(&s.server.id).await.unwrap().delete(db).await.unwrap();
    let kept = db.fetch_server_event(&d.ahead).await.unwrap();

    let (_connection, mut pushes) = push_queue().await;
    let mut owner_feed = PubSubTestHelper::new(&format!("{}!", s.owner.id)).await;
    let mut member = db.fetch_user(&s.member.id).await.unwrap();
    member.delete(db).await.unwrap();

    // the member's answers are gone everywhere
    assert!(db
        .fetch_server_event_answers_by_user(&s.member.id, None)
        .await
        .unwrap()
        .is_empty());
    // in the called-off event of the deleted server the seat is not handed
    // on: the owner still waits, as when the server was deleted
    let roster = db.fetch_server_event_roster(&d.ahead).await.unwrap().unwrap();
    assert!(roster.going.is_empty(), "{roster:?}");
    assert_eq!(roster.waiting, vec![s.owner.id.clone()]);
    let row = db.fetch_server_event_answer(&d.ahead, &s.owner.id).await.unwrap().unwrap();
    assert!(row.waiting);
    // the numbers of the past event stay as they were
    let after = db.fetch_server_event(&d.ahead).await.unwrap();
    assert_eq!(
        (after.going_count, after.waitlist_count, after.status.clone()),
        (kept.going_count, kept.waitlist_count, kept.status.clone())
    );
    // nobody hears of the events of the deleted server
    let gone = [&d.ahead, &d.running, &d.draft, &d.done];
    assert_silent(&mut owner_feed, "an event of a deleted server", |e| {
        gone.iter().any(|id| about(e, id))
    })
    .await;
    let pushed = push_arrives(&mut pushes, &gone, 800).await;
    assert!(pushed.is_none(), "a push was queued: {pushed:?}");
    // in the other server, whose event is still to come, the seat does
    // move on, as it should
    let elsewhere = db.fetch_server_event_roster(&d.elsewhere).await.unwrap().unwrap();
    assert_eq!(elsewhere.going, vec![s.owner.id.clone()]);
}

/// The audit log is read by people who need not be among those who may
/// see an event: entries of a draft or a restricted event carry no title
#[rocket::async_test]
async fn the_audit_log_does_not_name_drafts_and_restricted_events() {
    let s = setup(true).await;
    let mut secret = body(park());
    secret["title"] = json!("Surprise party for Sam");
    secret["visibility"] = json!("Invited");
    let secret = create_in(&s, &s.server.id, secret).await;
    let mut draft = body(park());
    draft["title"] = json!("Unannounced launch");
    draft["draft"] = json!(true);
    let draft = create_in(&s, &s.server.id, draft).await;
    for (event, action) in [(&secret, "start"), (&secret, "end"), (&draft, "publish")] {
        let (status, _) = call(
            &s.harness,
            Method::Post,
            &format!("{}/{action}", event_path(&s, event)),
            &s.owner_session,
            None,
        )
        .await;
        assert_eq!(status, Status::Ok, "{action}");
    }
    let open = create(&s, park()).await;
    let entries = s
        .harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let text = serde_json::to_string(&entries.iter().map(|e| &e.action).collect::<Vec<_>>()).unwrap();
    assert!(!text.contains("Surprise party"), "{text}");
    // the draft was published as an open event: from then on it is named
    assert_eq!(text.matches("Unannounced launch").count(), 1, "{text}");
    assert!(text.contains(&id_of(&secret)), "the entry still says which event");
    assert!(text.contains("Game night"), "an open event is named: {text}");
    let _ = open;
}

/// When the retention time of an event is up
async fn months_plus(db: &revolt_database::Database, id: &str) -> Timestamp {
    db.fetch_server_event(id)
        .await
        .unwrap()
        .people_kept_until()
        .expect("an ended event")
}

#[rocket::async_test]
async fn a_deleted_account_leaves_no_answers_and_frees_its_seat() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(
        &s,
        park(),
        json!({ "capacity": 1, "co_organisers": [s.member.id] }),
    )
    .await;
    let id = id_of(&event);
    answer(&s, &s.member_session, &event, "Going").await;
    let (waiting_session, waiting) = join(&s).await;
    answer(&s, &waiting_session, &event, "Going").await;
    let other = create(&s, park()).await;
    answer(&s, &s.member_session, &other, "Maybe").await;

    let mut user = db.fetch_user(&s.member.id).await.unwrap();
    user.delete(db).await.unwrap();

    assert!(db
        .fetch_server_event_answers_by_user(&s.member.id, None)
        .await
        .unwrap()
        .is_empty());
    let stored = db.fetch_server_event(&id).await.unwrap();
    assert_eq!((stored.going_count, stored.waitlist_count), (1, 0));
    assert!(stored.co_organisers.is_empty());
    let roster = db.fetch_server_event_roster(&id).await.unwrap().unwrap();
    assert_eq!(roster.going, vec![waiting.id.clone()]);
    assert_eq!(
        db.fetch_server_event(&id_of(&other)).await.unwrap().interested_count,
        0
    );
}

#[rocket::async_test]
async fn the_copy_of_my_data_holds_my_answers_and_my_events() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let (account, session, user) = s.harness.new_user().await;
    Member::create(db, &s.server, &user, None).await.unwrap();
    let event = create(&s, park()).await;
    answer(&s, &session, &event, "Going").await;
    // the owner's answer is not in someone else's copy
    answer(&s, &s.owner_session, &event, "NotGoing").await;

    let zip = revolt_database::build_export_zip(db, &account, &user).await.unwrap();
    let text = String::from_utf8_lossy(&zip);
    assert!(text.contains("events.json"), "no events.json in the copy");
    let start = text.find("{\"answers\":[").expect("the answers");
    let own = &text[start..];
    assert!(own.contains(&format!("\"event\":\"{}\"", id_of(&event))), "{}", &own[..300.min(own.len())]);
    assert!(own.contains("\"answer\":\"Going\""));
    assert!(!text.contains("\"answer\":\"NotGoing\""));
    assert!(own.contains("\"title\":\"Game night\""), "the answer names its event");

    // the owner's copy names the event they created
    let owner_account = db.fetch_account(&s.owner.id).await.unwrap();
    let zip = revolt_database::build_export_zip(db, &owner_account, &s.owner).await.unwrap();
    let text = String::from_utf8_lossy(&zip);
    assert!(text.contains(&format!("\"id\":\"{}\"", id_of(&event))));
    assert!(text.contains("\"answer\":\"NotGoing\""));

    // events that name a person are in their copy
    let helped = create_with(
        &s,
        park(),
        json!({ "visibility": "Invited", "invited": [user.id], "co_organisers": [user.id] }),
    )
    .await;
    let zip = revolt_database::build_export_zip(db, &account, &user).await.unwrap();
    let text = String::from_utf8_lossy(&zip);
    for list in ["invited_to", "helping_to_organise"] {
        let start = text.find(&format!("\"{list}\":[")).unwrap_or_else(|| panic!("no {list}"));
        let end = start + text[start..].find(']').unwrap();
        assert!(text[start..end].contains(&id_of(&helped)), "{list}: {}", &text[start..end]);
    }
}

async fn bucket(s: &Setup, method: Method, path: &str, session: &Session, body: Option<Value>) -> String {
    let client = &s.harness.client;
    let request = match method {
        Method::Get => client.get(path.to_string()),
        Method::Post => client.post(path.to_string()),
        Method::Put => client.put(path.to_string()),
        Method::Delete => client.delete(path.to_string()),
        other => panic!("unused method {other}"),
    }
    .header(Header::new("x-session-token", session.token.clone()))
    .header(ContentType::JSON);
    let request = match body {
        Some(body) => request.body(body.to_string()),
        None => request,
    };
    let response = request.dispatch().await;
    assert_ne!(response.status(), Status::NotFound, "{path}");
    response
        .headers()
        .get_one("x-ratelimit-bucket")
        .unwrap_or_else(|| panic!("{path} is not rate limited"))
        .to_string()
}

#[rocket::async_test]
async fn every_new_route_is_rate_limited_with_the_events_buckets() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let path = event_path(&s, &event);
    let owner = &s.owner_session;
    answer(&s, &s.member_session, &event, "Going").await;
    let (waiting_session, waiting) = join(&s).await;
    answer(&s, &waiting_session, &event, "Going").await;

    // reads: the bucket of the events list
    let reads = bucket(&s, Method::Get, &events_path(&s), owner, None).await;
    for read in [
        format!("{path}/attendees"),
        format!("{path}/stats"),
        clash_query(&s, 1, 2, ""),
        "/users/@me/events".to_string(),
    ] {
        assert_eq!(bucket(&s, Method::Get, &read, owner, None).await, reads, "{read}");
    }
    // and not the bucket of the people routes
    assert_ne!(bucket(&s, Method::Get, "/users/@me", owner, None).await, reads);

    // writes: the per-server bucket of creating and editing events
    let writes = bucket(&s, Method::Post, &format!("{path}/cancel?series=false"), &s.member_session, None).await;
    let mut seen = vec![];
    for (method, write, body) in [
        (Method::Put, format!("{path}/rsvp"), Some(json!({ "answer": "Maybe" }))),
        (Method::Delete, format!("{path}/rsvp"), None),
        (Method::Post, format!("{path}/publish"), None),
        (Method::Post, format!("{path}/copy"), None),
        (Method::Post, format!("{path}/attendees/{}/seat?over_capacity=true", waiting.id), None),
        (Method::Delete, format!("{path}/attendees/{}", waiting.id), None),
    ] {
        seen.push((write.clone(), bucket(&s, method, &write, &s.member_session, body).await));
    }
    for (write, got) in seen {
        assert_eq!(got, writes, "{write}");
    }
    assert_ne!(writes, reads);
}

#[rocket::async_test]
async fn the_gateway_never_says_who_answered() {
    let s = setup(true).await;
    let event = create(&s, voice_place(&s)).await;
    let id = id_of(&event);
    let mut channel_feed = PubSubTestHelper::new(s.voice.id()).await;
    answer(&s, &s.member_session, &event, "Maybe").await;
    call(
        &s.harness,
        Method::Put,
        &format!("{}/interested", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    expect_event(&mut channel_feed, "the count of two", |e| {
        matches!(e, EventV1::ServerEventInterest { id: event, count: 2, .. } if event == &id)
    })
    .await;
    assert_silent(&mut channel_feed, "a name on the topic everyone hears", |e| {
        matches!(e, EventV1::ServerEventInterest { id: event, user, .. }
            if event == &id && !user.is_empty())
            || matches!(e, EventV1::ServerEventRsvpUpdate { .. })
    })
    .await;
    // answers are not part of the event the gateway sends either
    let stored = s.harness.db.fetch_server_event(&id).await.unwrap();
    let sent = serde_json::to_string(&v0::ServerEvent::from(stored)).unwrap();
    assert!(!sent.contains(&s.member.id), "{sent}");
    let _ = ServerEventAnswer::Going;
}

#[rocket::async_test]
async fn an_event_says_when_it_is_over_its_seat_limit() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let path = event_path(&s, &event);
    assert_eq!(event["over_capacity"], false);
    answer(&s, &s.member_session, &event, "Going").await;
    let (second_session, _) = join(&s).await;
    answer(&s, &second_session, &event, "Going").await;

    // more seats: the waiting person is seated by the limit itself, nobody
    // has to confirm anything and the event is within its limit
    let edit = |capacity: u32| {
        call(
            &s.harness,
            Method::Patch,
            &path,
            &s.owner_session,
            Some(json!({ "capacity": capacity })),
        )
    };
    let (status, wider) = edit(2).await;
    assert_eq!(status, Status::Ok, "{wider}");
    assert_eq!(wider["counts"]["going"], 2);
    assert_eq!(wider["over_capacity"], false);

    // fewer seats than people seated: nobody loses a seat, and the event
    // says it is over its limit
    let (_, narrower) = edit(1).await;
    assert_eq!(narrower["counts"]["going"], 2);
    assert_eq!(narrower["over_capacity"], true);

    // a third person waits; seating them needs the organiser's word
    let (third_session, third) = join(&s).await;
    let (_, queued) = answer(&s, &third_session, &event, "Going").await;
    assert_eq!(queued["rsvp"]["waiting"], true);
    let seat = format!("{path}/attendees/{}/seat", third.id);
    let (status, error) = call(&s.harness, Method::Post, &seat, &s.owner_session, None).await;
    assert_eq!(status, Status::Conflict, "{error}");
    assert_eq!(error["type"], "ServerEventOverCapacity");
    let (status, seated) = call(
        &s.harness,
        Method::Post,
        &format!("{seat}?over_capacity=true"),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{seated}");
    assert_eq!(seated["counts"]["going"], 3);
    assert_eq!(seated["over_capacity"], true);

    // registering by oneself never passes the limit, however often
    let (fourth_session, _) = join(&s).await;
    let (_, fourth) = answer(&s, &fourth_session, &event, "Going").await;
    assert_eq!(fourth["rsvp"]["waiting"], true);
    assert_eq!(fourth["event"]["counts"]["going"], 3);
}

/// Our own queue beside the push service's, bound to the same key: what
/// arrives here is what the push service is given
async fn push_queue() -> (lapin::Connection, lapin::Consumer) {
    use lapin::options::{BasicConsumeOptions, QueueBindOptions, QueueDeclareOptions};
    use lapin::types::FieldTable;
    let config = revolt_config::config().await;
    let connection = lapin::Connection::connect(
        &format!(
            "amqp://{}:{}@{}:{}",
            config.rabbit.username, config.rabbit.password, config.rabbit.host, config.rabbit.port
        ),
        lapin::ConnectionProperties::default(),
    )
    .await
    .expect("rabbit");
    let channel = connection.create_channel().await.unwrap();
    let queue = channel
        .queue_declare(
            "".into(),
            QueueDeclareOptions {
                exclusive: true,
                auto_delete: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .unwrap();
    channel
        .queue_bind(
            queue.name().as_str().into(),
            config.pushd.exchange.clone().into(),
            config.pushd.get_message_routing_key().into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await
        .unwrap();
    let pushes = channel
        .basic_consume(
            queue.name().as_str().into(),
            "events-test".into(),
            BasicConsumeOptions {
                no_ack: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .unwrap();
    (connection, pushes)
}

/// The push about one of these events, if one arrives within the wait
async fn push_arrives(
    pushes: &mut lapin::Consumer,
    events: &[&String],
    wait_ms: u64,
) -> Option<Value> {
    use futures::StreamExt;
    tokio::time::timeout(std::time::Duration::from_millis(wait_ms), async {
        loop {
            let delivery = pushes.next().await.expect("the queue").expect("a delivery");
            let payload: Value = serde_json::from_slice(&delivery.data).unwrap();
            let tag = payload["notification"]["tag"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if events.iter().any(|id| tag == format!("event-{id}")) {
                return payload;
            }
        }
    })
    .await
    .ok()
}

/// What really goes to the push service when an event starts: the message
/// on the queue names only people who may see the event
#[rocket::async_test]
async fn the_push_at_the_start_of_a_private_event_goes_only_to_people_who_see_it() {
    let s = setup(true).await;
    let (_connection, mut pushes) = push_queue().await;

    // invited: the guest and the member; the member then loses sight of it
    // (the event moves to a channel they cannot view), an outsider answered
    // nothing and was never invited
    let (guest_session, guest) = join(&s).await;
    let (_, outsider) = join(&s).await;
    let event = create_with(
        &s,
        voice_place(&s),
        json!({ "visibility": "Invited", "invited": [guest.id, s.member.id] }),
    )
    .await;
    let id = id_of(&event);
    // first the notice that they are invited (same tag as the start's push):
    // to the two invited people only, never the outsider
    let invited = push_arrives(&mut pushes, &[&id], 15_000)
        .await
        .expect("no push was queued for the invitation");
    let mut told: Vec<&str> = invited["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|user| user.as_str().unwrap())
        .collect();
    told.sort();
    let mut expected = vec![guest.id.as_str(), s.member.id.as_str()];
    expected.sort();
    assert_eq!(told, expected, "{invited}");
    assert!(!invited.to_string().contains(&outsider.id), "{invited}");
    answer(&s, &guest_session, &event, "Going").await;
    answer(&s, &s.member_session, &event, "Maybe").await;
    // a row of someone who cannot see the event, written behind the checks,
    // as a fault elsewhere might leave one
    s.harness
        .db
        .set_server_event_answer(&id, &s.server.id, &outsider.id, ServerEventAnswer::Maybe, false)
        .await
        .unwrap();
    let mut moved = s.harness.db.fetch_server_event(&id).await.unwrap();
    moved.invited = vec![guest.id.clone()];
    s.harness.db.update_server_event(&moved).await.unwrap();

    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{}/start", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);

    // the message for this event, as the push service will read it
    let payload = push_arrives(&mut pushes, &[&id], 15_000)
        .await
        .expect("no push was queued for the start of the event");
    let users: Vec<&str> = payload["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|user| user.as_str().unwrap())
        .collect();
    assert_eq!(users, vec![guest.id.as_str()], "{payload}");
    let text = payload.to_string();
    assert!(!text.contains(&s.member.id) && !text.contains(&outsider.id), "{text}");

    // an event nobody but the organiser may see queues no push at all
    let lonely = create_with(&s, park(), json!({ "visibility": "Invited", "invited": [] })).await;
    let lonely_id = id_of(&lonely);
    s.harness
        .db
        .set_server_event_answer(&lonely_id, &s.server.id, &outsider.id, ServerEventAnswer::Going, false)
        .await
        .unwrap();
    call(
        &s.harness,
        Method::Post,
        &format!("{}/start", event_path(&s, &lonely)),
        &s.owner_session,
        None,
    )
    .await;
    let leaked = push_arrives(&mut pushes, &[&lonely_id], 1500).await;
    assert!(leaked.is_none(), "a push was queued: {leaked:?}");
}

/// Write when an event ended, as time passing would
async fn ended(s: &Setup, id: &str, status: v0::ServerEventStatus, days_ago: i64) {
    let db = &s.harness.db;
    let from = db.fetch_server_event(id).await.unwrap().status;
    assert!(db
        .set_server_event_status(id, from, status, Timestamp::now_utc() - Duration::days(days_ago))
        .await
        .unwrap());
}

/// An invited-only event with one seat: the owner seated, and with `n` of
/// 2 the member waiting
async fn people_event(s: &Setup, n: usize) -> String {
    let event = create_with(
        s,
        park(),
        json!({
            "capacity": 1,
            "visibility": "Invited",
            "invited": [s.member.id],
            "co_organisers": [s.member.id],
        }),
    )
    .await;
    answer(s, &s.owner_session, &event, "Going").await;
    if n > 1 {
        answer(s, &s.member_session, &event, "Going").await;
    }
    id_of(&event)
}

#[rocket::async_test]
async fn people_of_an_event_are_removed_twelve_months_after_it_ended_and_never_before() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let now = Timestamp::now_utc();
    // ended 370 days ago; ended 360 days ago; called off 370 days ago;
    // called off 300 days ago; still scheduled; active for a long time
    let old = people_event(&s, 2).await;
    ended(&s, &old, v0::ServerEventStatus::Completed, 370).await;
    let recent = people_event(&s, 2).await;
    ended(&s, &recent, v0::ServerEventStatus::Completed, 360).await;
    let off_old = people_event(&s, 1).await;
    ended(&s, &off_old, v0::ServerEventStatus::Cancelled, 370).await;
    let off_recent = people_event(&s, 1).await;
    ended(&s, &off_recent, v0::ServerEventStatus::Cancelled, 300).await;
    let scheduled = people_event(&s, 1).await;
    let active = people_event(&s, 1).await;
    ended(&s, &active, v0::ServerEventStatus::Active, 500).await;

    // before the time is up (360 days after the oldest ended), nothing goes
    let early = ServerEvent::run_retention_pass(db, now - Duration::days(10)).await.unwrap();
    assert_eq!(early, 0);
    for id in [&old, &recent, &off_old, &off_recent, &scheduled, &active] {
        assert!(
            db.fetch_server_event_answer(id, &s.owner.id).await.unwrap().is_some(),
            "removed before its time: {id}"
        );
    }

    let done = ServerEvent::run_retention_pass(db, now).await.unwrap();
    assert_eq!(done, 2, "only the two that ended more than 12 months ago");
    for id in [&old, &off_old] {
        let event = db.fetch_server_event(id).await.unwrap();
        assert!(event.people_removed_at.is_some(), "{id}");
        assert!(event.invited.is_empty(), "{id}");
        // the helpers' ids go too; who made the event stays
        assert!(event.co_organisers.is_empty(), "{id}");
        assert_eq!(event.creator, s.owner.id, "{id}");
        assert!(db.fetch_server_event_roster(id).await.unwrap().is_none(), "{id}");
        assert!(db
            .fetch_server_event_answers(
                id,
                &[ServerEventAnswer::Going, ServerEventAnswer::Maybe, ServerEventAnswer::NotGoing],
                None,
                None,
                100
            )
            .await
            .unwrap()
            .is_empty());
    }
    // the event and its numbers stay
    let kept = db.fetch_server_event(&old).await.unwrap();
    assert_eq!((kept.going_count, kept.waitlist_count), (1, 1));
    assert_eq!(kept.title, "Game night");
    // everything else is untouched
    for id in [&recent, &off_recent, &scheduled, &active] {
        let event = db.fetch_server_event(id).await.unwrap();
        assert!(event.people_removed_at.is_none(), "{id}");
        assert_eq!(event.invited, vec![s.member.id.clone()], "{id}");
        assert_eq!(event.co_organisers, vec![s.member.id.clone()], "{id}");
        assert!(db.fetch_server_event_answer(id, &s.owner.id).await.unwrap().is_some(), "{id}");
        assert!(db.fetch_server_event_roster(id).await.unwrap().is_some(), "{id}");
    }
    assert!(db.fetch_server_event(&scheduled).await.unwrap().people_kept_until().is_none());
    assert!(db.fetch_server_event(&active).await.unwrap().people_kept_until().is_none());

    // a second pass finds nothing more; the next one is due once its own
    // twelve months have passed, to the day
    assert_eq!(ServerEvent::run_retention_pass(db, now).await.unwrap(), 0);
    let recent_until = db
        .fetch_server_event(&recent)
        .await
        .unwrap()
        .people_kept_until()
        .expect("an ended event");
    assert_eq!(
        ServerEvent::run_retention_pass(db, recent_until - Duration::minutes(1)).await.unwrap(),
        0,
        "a minute early"
    );
    assert_eq!(ServerEvent::run_retention_pass(db, recent_until).await.unwrap(), 1);
    assert!(db.fetch_server_event(&recent).await.unwrap().people_removed_at.is_some());
    assert!(db.fetch_server_event(&off_recent).await.unwrap().people_removed_at.is_none());

    // the audit entry: numbers and the event, no person
    let entries = db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let retention: Vec<_> = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.action,
                revolt_database::AuditLogEntryAction::ServerEventRetention { .. }
            )
        })
        .collect();
    assert_eq!(retention.len(), 3);
    for entry in &retention {
        let text = serde_json::to_string(&entry.action).unwrap();
        assert!(!text.contains(&s.owner.id) && !text.contains(&s.member.id), "{text}");
        assert_eq!(entry.user, revolt_database::RETENTION_ACTOR);
        assert!(entry.target.is_none());
    }
    assert!(retention.iter().any(|entry| matches!(
        &entry.action,
        revolt_database::AuditLogEntryAction::ServerEventRetention { event, records: 2, outcome }
            if event == &old && outcome == "PeopleRemoved"
    )));
    // no title or other text of the event in it
    assert!(retention
        .iter()
        .all(|entry| !serde_json::to_string(&entry.action).unwrap().contains("Game night")));

    // what the API shows of such an event: counts, no names, and the date
    let path = format!("{}/{old}", events_path(&s));
    let (status, shown) = call(&s.harness, Method::Get, &path, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{shown}");
    assert!(shown["event"]["people_removed_at"].is_string());
    assert_eq!(shown["event"]["counts"]["going"], 1);
    assert!(shown.get("rsvp").is_none_or(Value::is_null));
    let (_, page) =
        call(&s.harness, Method::Get, &format!("{path}/attendees"), &s.owner_session, None).await;
    assert_eq!(page["attendees"], json!([]));
}

#[rocket::async_test]
async fn each_occurrence_of_a_series_has_its_own_retention_time() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let first = create_with(&s, park(), json!({ "repeat": "Weekly" })).await;
    let first_id = id_of(&first);
    answer(&s, &s.member_session, &first, "Going").await;
    let mut stored = db.fetch_server_event(&first_id).await.unwrap();
    assert!(stored.start(db).await.unwrap());
    assert!(stored.complete(db).await.unwrap());
    let next = db
        .fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap()
        .into_iter()
        .find(|next| next.previous.as_deref() == Some(first_id.as_str()))
        .expect("the next week");
    assert!(db.fetch_server_event_answer(&next.id, &s.member.id).await.unwrap().is_some());
    // the first occurrence ended long ago; the series goes on
    ended(&s, &first_id, v0::ServerEventStatus::Completed, 400).await;
    let now = Timestamp::now_utc();
    assert_eq!(ServerEvent::run_retention_pass(db, now).await.unwrap(), 1);
    assert!(db.fetch_server_event(&first_id).await.unwrap().people_removed_at.is_some());
    assert!(db.fetch_server_event_answer(&first_id, &s.member.id).await.unwrap().is_none());
    // the next occurrence keeps its people: its time runs from its own end
    let upcoming = db.fetch_server_event(&next.id).await.unwrap();
    assert!(upcoming.people_removed_at.is_none());
    assert!(db.fetch_server_event_answer(&next.id, &s.member.id).await.unwrap().is_some());
    let roster = db.fetch_server_event_roster(&next.id).await.unwrap().expect("its seat list");
    assert_eq!(roster.going, vec![s.member.id.clone()]);
    assert_eq!(upcoming.going_count, 1);
    assert_eq!(upcoming.previous.as_deref(), Some(first_id.as_str()));
    let mut upcoming = upcoming;
    assert!(upcoming.start(db).await.unwrap());
    assert!(upcoming.complete(db).await.unwrap());
    assert_eq!(
        ServerEvent::run_retention_pass(db, now + Duration::days(300)).await.unwrap(),
        0,
        "counted from the series' first day instead of this occurrence's end"
    );
    assert!(db.fetch_server_event_answer(&next.id, &s.member.id).await.unwrap().is_some());
    assert_eq!(
        ServerEvent::run_retention_pass(db, now + Duration::days(368)).await.unwrap(),
        1
    );
    assert!(db.fetch_server_event_answer(&next.id, &s.member.id).await.unwrap().is_none());
}

/// A request with a written reason, as the audit log takes it
async fn call_with_reason(
    s: &Setup,
    path: &str,
    session: &Session,
    reason: &str,
) -> (Status, Value) {
    let response = s
        .harness
        .client
        .get(path.to_string())
        .header(Header::new("x-session-token", session.token.clone()))
        .header(Header::new("x-audit-log-reason", reason.to_string()))
        .header(ContentType::JSON)
        .dispatch()
        .await;
    let status = response.status();
    let text = response.into_string().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// An invited-only event made by the member (who manages events and reads
/// the audit log), with a helper; one guest was seated beyond the limit and
/// one removed. Returns the event, the helper, the two guests and a
/// moderator who only reads the audit log.
struct Private {
    event: Value,
    helper: (Session, revolt_database::User),
    seated: revolt_database::User,
    removed: revolt_database::User,
    moderator: Session,
}

async fn private_event_with_managed_people(s: &Setup) -> Private {
    let audit = ChannelPermission::ViewAuditLogs as u64;
    grant(s, &s.member.id, ChannelPermission::ManageEvents as u64 | audit).await;
    let helper = join(s).await;
    grant(s, &helper.1.id, audit).await;
    let (moderator, moderator_user) = join(s).await;
    grant(s, &moderator_user.id, audit).await;
    let (removed_session, removed) = join(s).await;
    let (seated_session, seated) = join(s).await;

    let mut data = body(park());
    data["title"] = json!("Surprise party for Sam");
    data["visibility"] = json!("Invited");
    data["invited"] = json!([removed.id, seated.id]);
    data["co_organisers"] = json!([helper.1.id]);
    data["capacity"] = json!(1);
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &events_path(s),
        &s.member_session,
        Some(data),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    answer(s, &removed_session, &event, "Going").await;
    answer(s, &seated_session, &event, "Going").await;
    let path = event_path(s, &event);
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{path}/attendees/{}/seat?over_capacity=true", seated.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{path}/attendees/{}", removed.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    Private {
        event,
        helper,
        seated,
        removed,
        moderator,
    }
}

#[rocket::async_test]
async fn the_audit_log_shows_the_people_of_a_private_event_only_to_those_named_on_it() {
    let s = setup(true).await;
    let p = private_event_with_managed_people(&s).await;
    let id = id_of(&p.event);
    let log = format!("/servers/{}/audit_logs?limit=100", s.server.id);

    // the ids are stored, whoever reads
    let stored = audit(&s).await;
    for (kind, who) in [
        ("ServerEventAttendeeSeat", &p.seated.id),
        ("ServerEventAttendeeRemove", &p.removed.id),
    ] {
        assert!(
            stored.iter().any(|(name, _, target)| name == kind && target.as_ref() == Some(who)),
            "{kind} is not stored with its person"
        );
    }

    // the creator and the helper read who was seated and who was removed
    for session in [&s.member_session, &p.helper.0] {
        let (status, seen) = call(&s.harness, Method::Get, &log, session, None).await;
        assert_eq!(status, Status::Ok, "{seen}");
        let text = seen.to_string();
        assert!(text.contains(&p.seated.id) && text.contains(&p.removed.id), "{text}");
    }

    // a moderator who is not invited, and the server's owner, read that the
    // actions happened, on which event, by whom - not to whom, and no title
    for (who, session) in [("a moderator", &p.moderator), ("the owner", &s.owner_session)] {
        let (status, seen) = call(&s.harness, Method::Get, &log, session, None).await;
        assert_eq!(status, Status::Ok, "{who}: {seen}");
        let text = seen.to_string();
        assert!(!text.contains(&p.seated.id), "{who} reads who was seated: {text}");
        assert!(!text.contains(&p.removed.id), "{who} reads who was removed: {text}");
        assert!(!text.contains("Surprise party"), "{who} reads the title: {text}");
        let actions: Vec<&Value> = seen["audit_logs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["action"]["event"] == id.as_str())
            .collect();
        for kind in ["ServerEventAttendeeSeat", "ServerEventAttendeeRemove"] {
            let entry = actions
                .iter()
                .find(|entry| entry["action"]["type"] == kind)
                .unwrap_or_else(|| panic!("{who} does not see that {kind} happened"));
            assert_eq!(entry["action"]["user"], "", "{who}: {entry}");
            assert!(entry.get("target").is_none_or(Value::is_null), "{who}: {entry}");
            assert_eq!(entry["user"], s.member.id.as_str(), "who did it stays");
        }
        // asking for the entries about one person answers nothing
        let (_, by_person) = call(
            &s.harness,
            Method::Get,
            &format!("{log}&target={}", p.removed.id),
            session,
            None,
        )
        .await;
        assert_eq!(by_person["audit_logs"], json!([]), "{who}: {by_person}");
    }
    // the same question from the creator is answered
    let (_, by_person) = call(
        &s.harness,
        Method::Get,
        &format!("{log}&target={}", p.removed.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(by_person["audit_logs"].as_array().unwrap().len(), 1, "{by_person}");
}

#[rocket::async_test]
async fn the_owner_reads_who_takes_part_in_a_private_event_only_with_a_reason_that_is_logged() {
    let s = setup(true).await;
    let p = private_event_with_managed_people(&s).await;
    let id = id_of(&p.event);
    let list = format!("{}/attendees", event_path(&s, &p.event));

    // the owner manages events but is not named on this one: the event is
    // there for them, its people are not
    let (status, seen) = call(&s.harness, Method::Get, &event_path(&s, &p.event), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{seen}");
    assert_eq!(seen["organiser"], true);
    assert!(seen["event"].get("invited").is_none_or(Value::is_null), "{seen}");
    assert_eq!(seen["event"]["counts"]["going"], 1);
    for path in [list.clone(), format!("{list}?answer=NotGoing")] {
        let (status, error) = call(&s.harness, Method::Get, &path, &s.owner_session, None).await;
        assert_eq!(status, Status::Forbidden, "{error}");
        assert_eq!(error["type"], "ServerEventReasonRequired");
    }
    let (status, _) = call_with_reason(&s, &list, &s.owner_session, "   ").await;
    assert_eq!(status, Status::Forbidden, "an empty reason is no reason");
    let (_, old) = call(
        &s.harness,
        Method::Get,
        &format!("{}/interested", event_path(&s, &p.event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(old["users"], json!([]));
    assert!(
        !audit(&s).await.iter().any(|(kind, _, _)| kind == "ServerEventPeopleAccess"),
        "a refused read is not an access"
    );

    // with a reason they get the list, and the access is written down
    let (status, page) =
        call_with_reason(&s, &list, &s.owner_session, "Complaint 4711 about this event").await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["attendees"][0]["user"]["_id"], p.seated.id.as_str());
    let entries = s
        .harness
        .db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let access: Vec<_> = entries
        .iter()
        .filter(|entry| matches!(
            &entry.action,
            revolt_database::AuditLogEntryAction::ServerEventPeopleAccess { event } if event == &id
        ))
        .collect();
    assert_eq!(access.len(), 1);
    assert_eq!(access[0].user, s.owner.id);
    assert_eq!(access[0].reason.as_deref(), Some("Complaint 4711 about this event"));
    // the people named on the event can read that it happened
    let (_, seen) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/audit_logs?limit=100", s.server.id),
        &p.helper.0,
        None,
    )
    .await;
    assert!(seen.to_string().contains("ServerEventPeopleAccess"), "{seen}");

    // the people named on it need no reason; someone who only reads the
    // audit log gets nothing, with or without one
    for session in [&s.member_session, &p.helper.0] {
        let (status, page) = call(&s.harness, Method::Get, &list, session, None).await;
        assert_eq!(status, Status::Ok, "{page}");
    }
    let (status, _) = call_with_reason(&s, &list, &p.moderator, "curious").await;
    assert_eq!(status, Status::NotFound, "the event is not there for them at all");

    // an event open to everyone who sees the place is as before: whoever
    // manages events reads its list without a reason
    let open = create_with(&s, park(), json!({ "attendee_list": "Organisers" })).await;
    let (status, _) = call(
        &s.harness,
        Method::Get,
        &format!("{}/attendees", event_path(&s, &open)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
}

/// Deleting an event, also one whose people the retention removed before,
/// does not delete what the audit log says about it: those entries stay
/// until their own time is up
#[rocket::async_test]
async fn deleting_an_event_keeps_its_audit_entries() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let id = people_event(&s, 2).await;
    ended(&s, &id, v0::ServerEventStatus::Completed, 400).await;
    assert_eq!(ServerEvent::run_retention_pass(db, Timestamp::now_utc()).await.unwrap(), 1);
    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/{id}", events_path(&s)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::NoContent);
    assert!(db.fetch_server_event(&id).await.is_err());

    let entries = db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    let kinds: Vec<String> = entries
        .iter()
        .map(|entry| serde_json::to_value(&entry.action).unwrap())
        .filter(|action| action["event"] == id.as_str())
        .map(|action| action["type"].as_str().unwrap().to_string())
        .collect();
    for kind in ["ServerEventCreate", "ServerEventRetention", "ServerEventDelete"] {
        assert!(kinds.iter().any(|name| name == kind), "{kind} missing in {kinds:?}");
    }
}

/// A deleted server keeps its audit log until each entry's own time is up
#[rocket::async_test]
async fn a_deleted_server_keeps_its_audit_log_until_it_expires() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create(&s, park()).await;
    let (status, _) = call(
        &s.harness,
        Method::Post,
        &format!("{}/cancel", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    let before = audit(&s).await;
    assert!(before.len() >= 2, "{before:?}");

    db.fetch_server(&s.server.id).await.unwrap().delete(db).await.unwrap();

    let entries = db
        .get_server_audit_logs(
            &s.server.id,
            AuditLogQuery {
                user: None,
                target: None,
                r#type: None,
                before: None,
                after: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(entries.len(), before.len(), "entries went with the server");
    let now = Timestamp::now_utc();
    for entry in &entries {
        // each still has its own end, in the future, and no later than the
        // configured time from now
        assert!(entry.expires_at > now, "{}", entry.id);
        assert!(entry.expires_at <= now + Duration::days(91), "{}", entry.id);
    }
    assert!(entries.iter().any(|entry| matches!(
        &entry.action,
        revolt_database::AuditLogEntryAction::ServerEventCancel { event: id, .. } if id == &id_of(&event)
    )));
}
