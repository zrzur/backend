//! Taking part in events: the three answers and their counts, seats and the
//! waiting list (also with many people at once), who sees an event that is
//! limited to roles or invited people, and who may read the attendee list.
//! Runs on REFERENCE and MONGODB (TEST_DB).

use std::collections::HashSet;
use std::time::Duration as StdDuration;

use futures::future::join_all;
use revolt_database::events::client::EventV1;
use revolt_database::{
    Member, RosterChange, ServerEvent, ServerEventRoster, Session, User,
};
use revolt_models::v0::{self, ServerEventAnswer};
use rocket::http::{Method, Status};
use serde_json::{json, Value};

use super::events_tests::{body, call, create, setup, voice_place, Setup};
use crate::util::test::PubSubTestHelper;

/// Wait for a gateway event, and fail (not hang) when it never comes
pub(super) async fn expect_event<F: Fn(&EventV1) -> bool>(
    feed: &mut PubSubTestHelper,
    what: &str,
    predicate: F,
) -> EventV1 {
    tokio::time::timeout(StdDuration::from_secs(10), feed.wait_for_event(predicate))
        .await
        .unwrap_or_else(|_| panic!("the gateway never sent: {what}"))
}

/// Nothing matching arrives on this feed
pub(super) async fn assert_silent<F: Fn(&EventV1) -> bool>(
    feed: &mut PubSubTestHelper,
    what: &str,
    predicate: F,
) {
    assert!(
        tokio::time::timeout(StdDuration::from_millis(700), feed.wait_for_event(predicate))
            .await
            .is_err(),
        "leaked on the gateway: {what}"
    );
}

/// Whether a gateway event is about this event at all
pub(super) fn about(event: &EventV1, id: &str) -> bool {
    match event {
        EventV1::ServerEventCreate(created) => created.id == id,
        EventV1::ServerEventUpdate { event } => event.id == id,
        EventV1::ServerEventDelete { id: other, .. }
        | EventV1::ServerEventInterest { id: other, .. }
        | EventV1::ServerEventCounts { id: other, .. }
        | EventV1::ServerEventRsvpUpdate { id: other, .. } => other == id,
        _ => false,
    }
}

/// A new member of the server
pub(super) async fn join(s: &Setup) -> (Session, User) {
    let (_, session, user) = s.harness.new_user().await;
    Member::create(&s.harness.db, &s.server, &user, None)
        .await
        .expect("member");
    (session, user)
}

pub(super) fn park() -> Value {
    json!({ "type": "External", "location": "Park" })
}

/// Create an event as the owner with more fields than the plain body
pub(super) async fn create_with(s: &Setup, place: Value, more: Value) -> Value {
    let mut data = body(place);
    for (key, value) in more.as_object().expect("an object") {
        data[key] = value.clone();
    }
    let (status, event) = call(
        &s.harness,
        Method::Post,
        &format!("/servers/{}/events", s.server.id),
        &s.owner_session,
        Some(data),
    )
    .await;
    assert_eq!(status, Status::Ok, "{event}");
    event
}

pub(super) fn event_path(s: &Setup, event: &Value) -> String {
    format!("/servers/{}/events/{}", s.server.id, event["_id"].as_str().unwrap())
}

/// Answer an event; the status and the body
pub(super) async fn answer(
    s: &Setup,
    session: &Session,
    event: &Value,
    answer: &str,
) -> (Status, Value) {
    call(
        &s.harness,
        Method::Put,
        &format!("{}/rsvp", event_path(s, event)),
        session,
        Some(json!({ "answer": answer })),
    )
    .await
}

async fn fetch(s: &Setup, session: &Session, event: &Value) -> (Status, Value) {
    call(&s.harness, Method::Get, &event_path(s, event), session, None).await
}

fn counts(event: &Value) -> (u64, u64, u64, u64) {
    let of = |name: &str| event["counts"][name].as_u64().unwrap_or(u64::MAX);
    (of("going"), of("maybe"), of("not_going"), of("waiting"))
}

#[rocket::async_test]
async fn three_answers_replace_each_other_and_the_counts_follow() {
    let s = setup(true).await;
    let event = create(&s, park()).await;
    let id = event["_id"].as_str().unwrap().to_string();
    assert_eq!(counts(&event), (0, 0, 0, 0));
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let mut own = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;

    let (status, going) = answer(&s, &s.member_session, &event, "Going").await;
    assert_eq!(status, Status::Ok, "{going}");
    assert_eq!(going["rsvp"]["answer"], "Going");
    assert_eq!(going["rsvp"]["waiting"], false);
    assert_eq!(going["interested"], true);
    assert_eq!(counts(&going["event"]), (1, 0, 0, 0));
    // the older count: everyone taking part
    assert_eq!(going["event"]["interested_count"], 1);

    // everyone hears the counts, and nobody hears who answered
    expect_event(&mut everyone, "the new counts", |e| {
        matches!(e, EventV1::ServerEventCounts { id: event, counts, .. }
            if event == &id && counts.going == 1)
    })
    .await;
    let older = expect_event(&mut everyone, "the older count event", |e| {
        matches!(e, EventV1::ServerEventInterest { id: event, count: 1, .. } if event == &id)
    })
    .await;
    assert!(
        matches!(&older, EventV1::ServerEventInterest { user, .. } if user.is_empty()),
        "{older:?}"
    );
    // the person's own devices hear their answer
    expect_event(&mut own, "my own answer", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: Some(rsvp), .. }
            if event == &id && rsvp.answer == ServerEventAnswer::Going && !rsvp.waiting)
    })
    .await;

    // answering the same twice registers once
    let (_, again) = answer(&s, &s.member_session, &event, "Going").await;
    assert_eq!(counts(&again["event"]), (1, 0, 0, 0));
    let rows = s
        .harness
        .db
        .fetch_server_event_answers_by_user(&s.member.id, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    let (_, maybe) = answer(&s, &s.member_session, &event, "Maybe").await;
    assert_eq!(maybe["rsvp"]["answer"], "Maybe");
    assert_eq!(counts(&maybe["event"]), (0, 1, 0, 0));
    assert_eq!(maybe["event"]["interested_count"], 1);

    let (_, no) = answer(&s, &s.member_session, &event, "NotGoing").await;
    assert_eq!(no["rsvp"]["answer"], "NotGoing");
    assert_eq!(no["interested"], false);
    assert_eq!(counts(&no["event"]), (0, 0, 1, 0));
    assert_eq!(no["event"]["interested_count"], 0);

    // the list and the single fetch carry the answer
    let (_, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(list["interested"], json!([]));
    assert_eq!(list["answers"][0]["event"], id.as_str());
    assert_eq!(list["answers"][0]["answer"], "NotGoing");
    let (_, fetched) = fetch(&s, &s.member_session, &event).await;
    assert_eq!(fetched["rsvp"]["answer"], "NotGoing");
    assert_eq!(fetched["organiser"], false);

    // leaving
    let (status, left) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{left}");
    assert!(left.get("rsvp").is_none_or(Value::is_null), "{left}");
    assert_eq!(counts(&left["event"]), (0, 0, 0, 0));
    expect_event(&mut own, "my answer is gone", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: None, .. } if event == &id)
    })
    .await;
    // leaving twice changes nothing
    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &s.member_session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert_eq!(s.harness.db.fetch_server_event(&id).await.unwrap().interested_count, 0);

    // an answer that is no answer is refused
    let (status, _) = answer(&s, &s.member_session, &event, "Perhaps").await;
    assert!(status.code >= 400, "{status}");
}

#[rocket::async_test]
async fn the_interested_routes_still_work_and_mean_maybe() {
    let s = setup(true).await;
    let event = create(&s, park()).await;
    let mark = format!("{}/interested", event_path(&s, &event));

    let (status, marked) = call(&s.harness, Method::Put, &mark, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{marked}");
    assert_eq!(marked["interested"], true);
    assert_eq!(marked["rsvp"]["answer"], "Maybe");
    assert_eq!(counts(&marked["event"]), (0, 1, 0, 0));
    assert_eq!(marked["event"]["interested_count"], 1);

    // someone who is going is not lowered to "maybe" by the old button
    let (_, going) = answer(&s, &s.owner_session, &event, "Going").await;
    assert_eq!(going["rsvp"]["answer"], "Going");
    let (_, still) = call(&s.harness, Method::Put, &mark, &s.owner_session, None).await;
    assert_eq!(still["rsvp"]["answer"], "Going");
    assert_eq!(still["interested"], true);
    assert_eq!(counts(&still["event"]), (1, 1, 0, 0));
    assert_eq!(still["event"]["interested_count"], 2);

    // the old list names both (to the organiser: a new event's list is theirs)
    let (_, page) = call(&s.harness, Method::Get, &mark, &s.owner_session, None).await;
    assert_eq!(page["users"].as_array().unwrap().len(), 2, "{page}");

    // someone who said no is marked "maybe" by the old button
    let (session, _) = join(&s).await;
    answer(&s, &session, &event, "NotGoing").await;
    let (_, changed) = call(&s.harness, Method::Put, &mark, &session, None).await;
    assert_eq!(changed["rsvp"]["answer"], "Maybe");
    assert_eq!(counts(&changed["event"]), (1, 2, 0, 0));

    // unmarking withdraws
    let (_, unmarked) = call(&s.harness, Method::Delete, &mark, &s.owner_session, None).await;
    assert_eq!(unmarked["interested"], false);
    assert_eq!(counts(&unmarked["event"]), (0, 2, 0, 0));
}

#[rocket::async_test]
async fn seats_fill_then_people_wait_and_move_up_when_a_seat_frees() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 2 })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    assert_eq!(event["capacity"], 2);
    assert_eq!(event["waitlist"], true);
    let mut people = Vec::new();
    for _ in 0..4 {
        people.push(join(&s).await);
    }

    let mut answers = Vec::new();
    for (session, _) in &people {
        let (status, body) = answer(&s, session, &event, "Going").await;
        assert_eq!(status, Status::Ok, "{body}");
        answers.push(body);
    }
    assert_eq!(answers[1]["rsvp"]["waiting"], false);
    assert_eq!(answers[2]["rsvp"]["waiting"], true);
    assert_eq!(answers[2]["rsvp"]["position"], 1);
    assert_eq!(answers[3]["rsvp"]["position"], 2);
    assert_eq!(counts(&answers[3]["event"]), (2, 0, 0, 2));
    // waiting people are not counted as taking part yet, and the older
    // list names the same people its count counts
    assert_eq!(answers[3]["event"]["interested_count"], 2);
    let (_, old_list) = call(
        &s.harness,
        Method::Get,
        &format!("{}/interested", event_path(&s, &event)),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(old_list["users"].as_array().unwrap().len(), 2, "{old_list}");

    // the first seated person leaves: the first waiting person has the seat
    let mut third = PubSubTestHelper::new(&format!("{}!", people[2].1.id)).await;
    let (status, _) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &people[0].0,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    expect_event(&mut third, "a seat became mine", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: Some(rsvp), .. }
            if event == &id && rsvp.answer == ServerEventAnswer::Going && !rsvp.waiting)
    })
    .await;
    let (_, moved) = fetch(&s, &people[2].0, &event).await;
    assert_eq!(moved["rsvp"]["waiting"], false);
    assert_eq!(counts(&moved["event"]), (2, 0, 0, 1));
    let (_, next) = fetch(&s, &people[3].0, &event).await;
    assert_eq!(next["rsvp"]["waiting"], true);
    assert_eq!(next["rsvp"]["position"], 1);

    // a waiting person who changes to "maybe" gives up the place in line
    let (_, maybe) = answer(&s, &people[3].0, &event, "Maybe").await;
    assert_eq!(counts(&maybe["event"]), (2, 1, 0, 0));
    let roster = s.harness.db.fetch_server_event_roster(&id).await.unwrap().unwrap();
    assert_eq!(roster.going, vec![people[1].1.id.clone(), people[2].1.id.clone()]);
    assert!(roster.waiting.is_empty());
}

#[rocket::async_test]
async fn a_full_event_without_a_waiting_list_takes_nobody_else() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 1, "waitlist": false })).await;
    let (status, first) = answer(&s, &s.member_session, &event, "Going").await;
    assert_eq!(status, Status::Ok, "{first}");

    // someone who said "maybe" keeps that answer when "going" is refused
    answer(&s, &s.owner_session, &event, "Maybe").await;
    let (status, error) = answer(&s, &s.owner_session, &event, "Going").await;
    assert_eq!(status, Status::Conflict, "{error}");
    assert_eq!(error["type"], "ServerEventFull");
    let (_, kept) = fetch(&s, &s.owner_session, &event).await;
    assert_eq!(kept["rsvp"]["answer"], "Maybe");
    assert_eq!(counts(&kept["event"]), (1, 1, 0, 0));
}

#[rocket::async_test]
async fn more_seats_or_no_limit_move_waiting_people_up() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let mut people = Vec::new();
    for _ in 0..4 {
        let person = join(&s).await;
        answer(&s, &person.0, &event, "Going").await;
        people.push(person);
    }
    let path = event_path(&s, &event);
    let (status, wider) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "capacity": 3 })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{wider}");
    assert_eq!(counts(&wider), (3, 0, 0, 1));
    let (_, third) = fetch(&s, &people[2].0, &event).await;
    assert_eq!(third["rsvp"]["waiting"], false);

    // fewer seats than people seated: nobody loses a seat
    let (_, narrower) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "capacity": 1 })),
    )
    .await;
    assert_eq!(counts(&narrower), (3, 0, 0, 1));

    // no limit: everyone waiting is seated
    let (status, open) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "remove": ["Capacity"] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{open}");
    assert!(open.get("capacity").is_none_or(Value::is_null), "{open}");
    assert_eq!(counts(&open), (4, 0, 0, 0));
    let (_, last) = fetch(&s, &people[3].0, &event).await;
    assert_eq!(last["rsvp"]["waiting"], false);

    for (capacity, reason) in [(0, "out_of_range"), (10_001, "out_of_range")] {
        let (status, error) = call(
            &s.harness,
            Method::Patch,
            &path,
            &s.owner_session,
            Some(json!({ "capacity": capacity })),
        )
        .await;
        assert_eq!(status, Status::BadRequest, "{error}");
        assert_eq!(error["type"], "ServerEventInvalid");
        assert_eq!(error["field"], "capacity");
        assert_eq!(error["reason"], reason);
    }
}

/// What must hold whatever happened: nobody twice, never more seated than
/// seats, nobody waiting beside a free seat, rows and counts as the seat
/// list says
async fn assert_consistent(s: &Setup, id: &str, capacity: usize) -> ServerEventRoster {
    let db = &s.harness.db;
    let roster = db.fetch_server_event_roster(id).await.unwrap().expect("a seat list");
    let seated: HashSet<&String> = roster.going.iter().collect();
    let waiting: HashSet<&String> = roster.waiting.iter().collect();
    assert_eq!(seated.len(), roster.going.len(), "someone has two seats");
    assert_eq!(waiting.len(), roster.waiting.len(), "someone waits twice");
    assert!(seated.is_disjoint(&waiting), "someone is seated and waiting");
    assert!(roster.going.len() <= capacity, "{} seated, {capacity} seats", roster.going.len());
    assert!(
        roster.waiting.is_empty() || roster.going.len() == capacity,
        "people wait beside a free seat"
    );
    let rows = db
        .fetch_server_event_answers(id, &[ServerEventAnswer::Going], None, None, 100_000)
        .await
        .unwrap();
    assert_eq!(rows.len(), roster.going.len() + roster.waiting.len(), "rows and seat list differ");
    for row in &rows {
        assert_eq!(row.waiting, waiting.contains(&row.id.user), "{}", row.id.user);
        assert!(seated.contains(&row.id.user) || waiting.contains(&row.id.user));
    }
    let event = db.fetch_server_event(id).await.unwrap();
    assert_eq!(event.going_count as usize, roster.going.len());
    assert_eq!(event.waitlist_count as usize, roster.waiting.len());
    roster
}

#[rocket::async_test]
async fn many_people_registering_at_once_never_share_a_seat_or_lose_their_place() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "capacity": 10 })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let stored = db.fetch_server_event(&id).await.unwrap();
    // 60 people, all asking for a seat in the same moment (at this layer
    // nothing reads their accounts; the part over the API below uses real
    // members)
    let users: Vec<String> = (0..60).map(|n| format!("SEATUSER{n:018}")).collect();
    let results = join_all(users.iter().map(|user| {
        let mut event = stored.clone();
        async move { event.answer(db, user, ServerEventAnswer::Going).await }
    }))
    .await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    let roster = assert_consistent(&s, &id, 10).await;
    assert_eq!(roster.going.len(), 10);
    assert_eq!(roster.waiting.len(), 50);
    let everyone: HashSet<&String> = roster.going.iter().chain(&roster.waiting).collect();
    assert_eq!(everyone.len(), 60, "someone was lost");

    // each of them asks again at once: still one registration each
    join_all(users.iter().map(|user| {
        let mut event = stored.clone();
        async move { event.answer(db, user, ServerEventAnswer::Going).await }
    }))
    .await;
    let again = assert_consistent(&s, &id, 10).await;
    assert_eq!(again.going, roster.going);
    assert_eq!(again.waiting, roster.waiting);

    // the ten seated people all leave at once: the first ten of the line
    // have the seats, and the rest keep their order
    let leaving = roster.going.clone();
    let results = join_all(leaving.iter().map(|user| {
        let mut event = stored.clone();
        async move { event.withdraw(db, user).await }
    }))
    .await;
    assert!(results.iter().all(|left| matches!(left, Ok(true))), "{results:?}");
    let after = assert_consistent(&s, &id, 10).await;
    let moved_up: HashSet<&String> = after.going.iter().collect();
    let expected: HashSet<&String> = roster.waiting[..10].iter().collect();
    assert_eq!(moved_up, expected, "the seats did not go to the first ten waiting");
    assert_eq!(after.waiting, roster.waiting[10..].to_vec(), "the line changed order");

    // joining and leaving mixed, all at once
    let mixed = join_all(users.iter().enumerate().map(|(n, user)| {
        let mut event = stored.clone();
        async move {
            if n % 3 == 0 {
                event.withdraw(db, user).await.map(|_| ())
            } else {
                event
                    .answer(db, user, ServerEventAnswer::Going)
                    .await
                    .map(|_| ())
            }
        }
    }))
    .await;
    assert!(mixed.iter().all(Result::is_ok), "{mixed:?}");
    let end = assert_consistent(&s, &id, 10).await;
    assert_eq!(end.going.len(), 10);
    // the same over the API, each person with their own session
    let event_b = create_with(&s, park(), json!({ "capacity": 5 })).await;
    let mut sessions = Vec::new();
    for _ in 0..12 {
        sessions.push(join(&s).await.0);
    }
    let answers = join_all(
        sessions
            .iter()
            .map(|session| answer(&s, session, &event_b, "Going")),
    )
    .await;
    assert!(answers.iter().all(|(status, _)| *status == Status::Ok));
    let seated = answers
        .iter()
        .filter(|(_, body)| body["rsvp"]["waiting"] == false)
        .count();
    assert_eq!(seated, 5);
    let roster_b = assert_consistent(&s, event_b["_id"].as_str().unwrap(), 5).await;
    assert_eq!(roster_b.waiting.len(), 7);
}

#[rocket::async_test]
async fn one_person_sending_crossing_answers_ends_in_one_consistent_state() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "capacity": 2 })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let stored = db.fetch_server_event(&id).await.unwrap();
    let user = s.member.id.clone();
    for _ in 0..10 {
        let answers = [
            ServerEventAnswer::Going,
            ServerEventAnswer::Maybe,
            ServerEventAnswer::Going,
            ServerEventAnswer::NotGoing,
        ];
        join_all(answers.into_iter().map(|answer| {
            let mut event = stored.clone();
            let user = user.clone();
            async move { event.answer(db, &user, answer).await }
        }))
        .await;
        assert_consistent(&s, &id, 2).await;
        // one row at most, and the counts are what the rows say
        let rows = db.fetch_server_event_answers_by_user(&user, None).await.unwrap();
        assert!(rows.len() <= 1, "{rows:?}");
    }
    // a recount from the rows agrees with what the event shows for seats
    db.recount_server_event(&id).await.unwrap();
    assert_consistent(&s, &id, 2).await;
}

/// The MongoDB update pipeline and the rules in Rust are the same rules:
/// a long run of changes gives the same seat list step by step
#[rocket::async_test]
async fn the_database_changes_a_seat_list_exactly_as_the_rules_say() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let id = format!("ROSTER{}", ulid::Ulid::new());
    let mut model = ServerEventRoster::new(&id, &s.server.id, Some(3), true);
    db.insert_server_event_roster(&model).await.unwrap();
    // written once: a second insert keeps the first
    db.insert_server_event_roster(&ServerEventRoster::new(&id, &s.server.id, Some(99), false))
        .await
        .unwrap();
    assert_eq!(db.fetch_server_event_roster(&id).await.unwrap().unwrap(), model);

    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    for step in 0..500 {
        let user = format!("USER{:02}", next(12));
        let change = match next(20) {
            0..=9 => RosterChange::Join(user),
            10..=15 => RosterChange::Leave(user),
            16..=17 => RosterChange::Seat {
                user,
                over_capacity: next(2) == 0,
            },
            18 => RosterChange::Drop(user),
            _ => RosterChange::Settings {
                capacity: match next(6) {
                    0 => None,
                    seats => Some(seats as u32),
                },
                waitlist: next(4) != 0,
            },
        };
        model.apply(&change);
        let stored = db
            .change_server_event_roster(&id, &change)
            .await
            .unwrap()
            .expect("the seat list");
        assert_eq!(stored, model, "step {step}: {change:?}");
    }
    assert_eq!(db.fetch_server_event_roster(&id).await.unwrap().unwrap(), model);
    // a seat list that does not exist is not made by a change
    assert!(db
        .change_server_event_roster("NOSUCHEVENT", &RosterChange::Join("USER".into()))
        .await
        .unwrap()
        .is_none());
}

#[rocket::async_test]
async fn a_member_who_leaves_the_server_frees_their_seat() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "capacity": 1 })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    answer(&s, &s.member_session, &event, "Going").await;
    let (waiting_session, waiting) = join(&s).await;
    let (_, queued) = answer(&s, &waiting_session, &event, "Going").await;
    assert_eq!(queued["rsvp"]["waiting"], true);

    let member = db.fetch_member(&s.server.id, &s.member.id).await.unwrap();
    member
        .remove(db, &s.server, revolt_database::RemovalIntention::Leave, true)
        .await
        .unwrap();
    let roster = assert_consistent(&s, &id, 1).await;
    assert_eq!(roster.going, vec![waiting.id.clone()]);
    assert!(db
        .fetch_server_event_answer(&id, &s.member.id)
        .await
        .unwrap()
        .is_none());
}

#[rocket::async_test]
async fn a_repeating_event_takes_its_people_to_the_next_time_in_order() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, voice_place(&s), json!({ "capacity": 1, "repeat": "Daily" })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    answer(&s, &s.member_session, &event, "Going").await;
    answer(&s, &s.owner_session, &event, "Going").await;
    let (maybe_session, maybe) = join(&s).await;
    answer(&s, &maybe_session, &event, "Maybe").await;
    let (no_session, no) = join(&s).await;
    answer(&s, &no_session, &event, "NotGoing").await;

    let mut stored = db.fetch_server_event(&id).await.unwrap();
    assert!(stored.start(db).await.unwrap());
    assert!(stored.complete(db).await.unwrap());
    let next = db
        .fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap()
        .into_iter()
        .find(|next| next.previous.as_deref() == Some(id.as_str()))
        .expect("the next day");
    // daily: the same time one day later
    assert_eq!(
        next.starts_at,
        stored.starts_at + iso8601_timestamp::Duration::days(1)
    );
    assert_eq!(next.capacity, Some(1));
    assert_eq!(
        (next.going_count, next.waitlist_count, next.interested_count, next.not_going_count),
        (1, 1, 1, 0)
    );
    let roster = assert_consistent(&s, &next.id, 1).await;
    assert_eq!(roster.going, vec![s.member.id.clone()]);
    assert_eq!(roster.waiting, vec![s.owner.id.clone()]);
    assert!(db.fetch_server_event_answer(&next.id, &maybe.id).await.unwrap().is_some());
    // "not going" was an answer to that day only
    assert!(db.fetch_server_event_answer(&next.id, &no.id).await.unwrap().is_none());
}

/// A role and a member who holds it
async fn role_holder(s: &Setup) -> (String, Session, User) {
    let role = s.harness.new_role(&s.server, 5, None).await;
    let (session, user) = join(s).await;
    let mut member = s.harness.db.fetch_member(&s.server.id, &user.id).await.unwrap();
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
    (role.id, session, user)
}

/// Every way a member could learn of an event they may not see
async fn assert_hidden(s: &Setup, session: &Session, event: &Value) {
    let id = event["_id"].as_str().unwrap();
    let base = event_path(s, event);
    let (status, list) = call(
        &s.harness,
        Method::Get,
        &format!("/servers/{}/events", s.server.id),
        session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok);
    assert!(
        !list["events"].as_array().unwrap().iter().any(|e| e["_id"] == id),
        "listed: {list}"
    );
    for (method, path, body) in [
        (Method::Get, base.clone(), None),
        (Method::Put, format!("{base}/rsvp"), Some(json!({ "answer": "Going" }))),
        (Method::Delete, format!("{base}/rsvp"), None),
        (Method::Put, format!("{base}/interested"), None),
        (Method::Get, format!("{base}/interested"), None),
        (Method::Get, format!("{base}/attendees"), None),
        (Method::Get, format!("{base}/stats"), None),
        (Method::Patch, base.clone(), Some(json!({ "title": "Mine" }))),
        (Method::Post, format!("{base}/start"), None),
        (Method::Post, format!("{base}/copy"), None),
        (Method::Delete, base.clone(), None),
    ] {
        let (status, answer) = call(&s.harness, method, &path, session, body).await;
        assert_eq!(status, Status::NotFound, "{method} {path}: {answer}");
    }
    // not in the clash check
    let (status, clashes) = call(
        &s.harness,
        Method::Get,
        &format!(
            "/servers/{}/events/clashes?starts_at={}",
            s.server.id,
            event["starts_at"].as_str().unwrap().replace('+', "%2B")
        ),
        session,
        None,
    )
    .await;
    assert_eq!(status, Status::Ok, "{clashes}");
    assert!(
        !clashes["events"].as_array().unwrap().iter().any(|e| e["_id"] == id),
        "in the clash check: {clashes}"
    );
    // not among "my events"
    let (status, mine) = call(&s.harness, Method::Get, "/users/@me/events", session, None).await;
    assert_eq!(status, Status::Ok, "{mine}");
    assert!(
        !mine["events"].as_array().unwrap().iter().any(|e| e["_id"] == id),
        "among my events: {mine}"
    );
}

#[rocket::async_test]
async fn an_event_for_chosen_roles_is_hidden_from_everyone_else() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let (role, holder_session, holder) = role_holder(&s).await;
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let mut holder_feed = PubSubTestHelper::new(&format!("{}!", holder.id)).await;
    let mut outsider_feed = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;

    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Roles", "visible_roles": [role] }),
    )
    .await;
    let id = event["_id"].as_str().unwrap().to_string();
    assert_eq!(event["visibility"], "Roles");

    // the gateway tells the people it is for, each on their own topic
    expect_event(&mut holder_feed, "the new event, to a role holder", |e| {
        matches!(e, EventV1::ServerEventCreate(created) if created.id == id)
    })
    .await;
    assert_silent(&mut everyone, "a role event on the server topic", |e| about(e, &id)).await;
    assert_silent(&mut outsider_feed, "a role event to someone without the role", |e| {
        about(e, &id)
    })
    .await;

    // someone without the role learns nothing, by any route
    assert_hidden(&s, &s.member_session, &event).await;

    // a holder sees it, answers, and the counts stay among those who see it
    let (status, seen) = fetch(&s, &holder_session, &event).await;
    assert_eq!(status, Status::Ok, "{seen}");
    assert!(seen["event"].get("invited").is_none_or(Value::is_null));
    let (status, going) = answer(&s, &holder_session, &event, "Going").await;
    assert_eq!(status, Status::Ok, "{going}");
    expect_event(&mut holder_feed, "the counts, to a role holder", |e| {
        matches!(e, EventV1::ServerEventCounts { id: event, .. } if event == &id)
    })
    .await;
    assert_silent(&mut everyone, "counts of a role event on the server topic", |e| {
        about(e, &id)
    })
    .await;
    assert_silent(&mut outsider_feed, "counts of a role event to an outsider", |e| {
        about(e, &id)
    })
    .await;
    let (_, mine) = call(&s.harness, Method::Get, "/users/@me/events", &holder_session, None).await;
    assert!(mine["events"].as_array().unwrap().iter().any(|e| e["_id"] == id.as_str()));

    // the push at the start goes to people who still see it: a holder who
    // lost the role is left out
    let stored = db.fetch_server_event(&id).await.unwrap();
    let server = db.fetch_server(&s.server.id).await.unwrap();
    let place = stored.place_state(db).await;
    let told = stored
        .people_to_tell_at_start(db, &server, &place, &s.text)
        .await
        .unwrap();
    assert_eq!(told, vec![holder.id.clone()]);
    let mut member = db.fetch_member(&s.server.id, &holder.id).await.unwrap();
    member
        .update(
            db,
            revolt_database::PartialMember {
                roles: Some(vec![]),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();
    let told = stored
        .people_to_tell_at_start(db, &server, &place, &s.text)
        .await
        .unwrap();
    assert!(told.is_empty(), "{told:?}");
    assert_hidden(&s, &holder_session, &event).await;

    // and a start of a restricted event is never announced to the server
    assert!(ServerEvent::place_open_to_everyone(&server, None));
    assert!(stored.is_restricted());
}

#[rocket::async_test]
async fn an_event_for_invited_people_is_seen_by_them_only() {
    let s = setup(true).await;
    let (guest_session, guest) = join(&s).await;
    let mut guest_feed = PubSubTestHelper::new(&format!("{}!", guest.id)).await;
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Invited", "invited": [guest.id] }),
    )
    .await;
    let id = event["_id"].as_str().unwrap().to_string();
    // the organiser gets the list back, the guest never does
    assert_eq!(event["invited"], json!([guest.id]));
    expect_event(&mut guest_feed, "the new event, to the guest", |e| {
        matches!(e, EventV1::ServerEventCreate(created)
            if created.id == id && created.invited.is_none())
    })
    .await;
    assert_silent(&mut everyone, "an invited-only event on the server topic", |e| {
        about(e, &id)
    })
    .await;
    let (status, seen) = fetch(&s, &guest_session, &event).await;
    assert_eq!(status, Status::Ok, "{seen}");
    assert!(seen["event"].get("invited").is_none_or(Value::is_null), "{seen}");
    let (_, owner_view) = fetch(&s, &s.owner_session, &event).await;
    assert_eq!(owner_view["event"]["invited"], json!([guest.id]));
    assert_eq!(owner_view["organiser"], true);
    assert_hidden(&s, &s.member_session, &event).await;

    // someone who is not a member of the server cannot be invited
    let (_, _, stranger) = s.harness.new_user().await;
    let path = event_path(&s, &event);
    let (status, error) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "invited": [guest.id, stranger.id] })),
    )
    .await;
    assert_eq!(status, Status::BadRequest, "{error}");
    assert_eq!(error["type"], "ServerEventInvalid");
    assert_eq!(error["field"], "invited");
    assert_eq!(error["reason"], "not_a_member");

    // the member is invited and the guest is not any more: the guest hears
    // that it is gone, the member that it is there
    let mut member_feed = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;
    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "invited": [s.member.id] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    expect_event(&mut guest_feed, "the event is gone for the guest", |e| {
        matches!(e, EventV1::ServerEventDelete { id: event, .. } if event == &id)
    })
    .await;
    expect_event(&mut member_feed, "the event, to the newly invited", |e| {
        matches!(e, EventV1::ServerEventUpdate { event } if event.id == id)
    })
    .await;
    assert_hidden(&s, &guest_session, &event).await;
    let (status, _) = fetch(&s, &s.member_session, &event).await;
    assert_eq!(status, Status::Ok);

    // opened to everyone who sees the place: the lists are not kept
    let (_, opened) = call(
        &s.harness,
        Method::Patch,
        &path,
        &s.owner_session,
        Some(json!({ "visibility": "Place" })),
    )
    .await;
    assert_eq!(opened["visibility"], "Place");
    assert_eq!(opened["invited"], json!([]));
    expect_event(&mut everyone, "the event, now for everyone", |e| {
        matches!(e, EventV1::ServerEventUpdate { event } if event.id == id)
    })
    .await;
    let (status, _) = fetch(&s, &guest_session, &event).await;
    assert_eq!(status, Status::Ok);
}

#[rocket::async_test]
async fn a_restricted_event_in_a_channel_still_needs_the_channel() {
    let s = setup(true).await;
    // invited to an event in a channel the member cannot see
    let event = create_with(
        &s,
        json!({ "type": "Text", "channel": s.private.id() }),
        json!({ "visibility": "Invited", "invited": [s.member.id] }),
    )
    .await;
    assert_hidden(&s, &s.member_session, &event).await;
    let stored = s
        .harness
        .db
        .fetch_server_event(event["_id"].as_str().unwrap())
        .await
        .unwrap();
    let people = stored.people_who_see(&s.harness.db).await.unwrap();
    assert_eq!(people, vec![s.owner.id.clone()]);
}

#[rocket::async_test]
async fn visibility_settings_are_checked() {
    let s = setup(true).await;
    let path = format!("/servers/{}/events", s.server.id);
    let mut cases = Vec::new();
    // roles without a role
    cases.push((json!({ "visibility": "Roles" }), "visible_roles", "empty"));
    cases.push((
        json!({ "visibility": "Roles", "visible_roles": ["NOSUCHROLE"] }),
        "visible_roles",
        "unknown_role",
    ));
    let many: Vec<String> = (0..201).map(|n| format!("{n:026}")).collect();
    cases.push((json!({ "visibility": "Invited", "invited": many }), "invited", "too_many"));
    let helpers: Vec<String> = (0..11).map(|n| format!("{n:026}")).collect();
    cases.push((json!({ "co_organisers": helpers }), "co_organisers", "too_many"));
    cases.push((json!({ "capacity": 0 }), "capacity", "out_of_range"));
    for (more, field, reason) in cases {
        let mut data = body(park());
        for (key, value) in more.as_object().unwrap() {
            data[key] = value.clone();
        }
        let (status, error) =
            call(&s.harness, Method::Post, &path, &s.owner_session, Some(data)).await;
        assert_eq!(status, Status::BadRequest, "{more}: {error}");
        assert_eq!(error["type"], "ServerEventInvalid", "{more}: {error}");
        assert_eq!(error["field"], field, "{more}: {error}");
        assert_eq!(error["reason"], reason, "{more}: {error}");
    }
    // nothing was created by the refused requests
    assert!(s
        .harness
        .db
        .fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap()
        .is_empty());
}

#[rocket::async_test]
async fn the_attendee_list_follows_its_own_setting() {
    let s = setup(true).await;
    let event = create_with(&s, park(), json!({ "attendee_list": "Organisers" })).await;
    let base = event_path(&s, &event);
    let list = format!("{base}/attendees");
    answer(&s, &s.owner_session, &event, "Going").await;
    let (no_session, no) = join(&s).await;
    answer(&s, &no_session, &event, "NotGoing").await;

    // organisers only: a member is refused, and the older list is empty
    let (status, error) = call(&s.harness, Method::Get, &list, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden, "{error}");
    let (status, old) =
        call(&s.harness, Method::Get, &format!("{base}/interested"), &s.member_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(old["users"], json!([]));
    // the count stays visible, the people do not
    let (_, seen) = call(&s.harness, Method::Get, &base, &s.member_session, None).await;
    assert_eq!(seen["event"]["counts"]["going"], 1);
    let (status, page) = call(&s.harness, Method::Get, &list, &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["attendees"].as_array().unwrap().len(), 1);
    assert_eq!(page["attendees"][0]["user"]["_id"], s.owner.id.as_str());
    assert_eq!(page["attendees"][0]["answer"], "Going");
    // organisers see when someone answered
    assert!(page["attendees"][0]["since"].is_string(), "{page}");
    // who said no: organisers only
    let (status, refusals) =
        call(&s.harness, Method::Get, &format!("{list}?answer=NotGoing"), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok, "{refusals}");
    assert_eq!(refusals["attendees"][0]["user"]["_id"], no.id.as_str());

    // people taking part: only once you take part yourself
    let edit = |setting: &'static str| {
        call(
            &s.harness,
            Method::Patch,
            &base,
            &s.owner_session,
            Some(json!({ "attendee_list": setting })),
        )
    };
    let (status, _) = edit("Participants").await;
    assert_eq!(status, Status::Ok);
    let (status, _) = call(&s.harness, Method::Get, &list, &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);
    // saying no is not taking part
    let (status, _) = call(&s.harness, Method::Get, &list, &no_session, None).await;
    assert_eq!(status, Status::Forbidden);
    answer(&s, &s.member_session, &event, "Maybe").await;
    let (status, page) = call(&s.harness, Method::Get, &list, &s.member_session, None).await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["attendees"].as_array().unwrap().len(), 2);
    let (status, _) =
        call(&s.harness, Method::Get, &format!("{list}?answer=NotGoing"), &s.member_session, None).await;
    assert_eq!(status, Status::Forbidden);

    // everyone who sees the event
    let (status, _) = edit("Everyone").await;
    assert_eq!(status, Status::Ok);
    let (status, page) = call(&s.harness, Method::Get, &list, &no_session, None).await;
    assert_eq!(status, Status::Ok, "{page}");
    assert_eq!(page["attendees"].as_array().unwrap().len(), 2);
    // other readers do not learn when someone answered
    assert!(
        page["attendees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|attendee| attendee.get("since").is_none_or(Value::is_null)),
        "{page}"
    );
    let (status, maybes) =
        call(&s.harness, Method::Get, &format!("{list}?answer=Maybe"), &no_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(maybes["attendees"][0]["user"]["_id"], s.member.id.as_str());
    let (status, error) =
        call(&s.harness, Method::Get, &format!("{list}?answer=Sometimes"), &no_session, None).await;
    assert_eq!(status, Status::BadRequest);
    assert_eq!(error["type"], "ServerEventInvalid");

    // pages
    let (status, first) =
        call(&s.harness, Method::Get, &format!("{list}?limit=1"), &s.owner_session, None).await;
    assert_eq!(status, Status::Ok);
    assert_eq!(first["attendees"].as_array().unwrap().len(), 1);
    let next = first["next"].as_str().expect("a next page");
    let (_, rest) = call(
        &s.harness,
        Method::Get,
        &format!("{list}?limit=1&after={next}"),
        &s.owner_session,
        None,
    )
    .await;
    assert_eq!(rest["attendees"].as_array().unwrap().len(), 1);
    assert!(rest.get("next").is_none_or(Value::is_null));
    assert_ne!(first["attendees"][0]["user"]["_id"], rest["attendees"][0]["user"]["_id"]);
}

#[rocket::async_test]
async fn an_event_made_without_the_new_fields_is_as_before() {
    let s = setup(true).await;
    // what the app before answers sends
    let event = create(&s, voice_place(&s)).await;
    assert_eq!(event["visibility"], "Place");
    // who takes part is shown to the organisers until they choose otherwise
    assert_eq!(event["attendee_list"], "Organisers");
    assert_eq!(event["status"], "Scheduled");
    assert!(event.get("capacity").is_none_or(Value::is_null));
    assert_eq!(event["interested_count"], 0);
    let stored = s
        .harness
        .db
        .fetch_server_event(event["_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(stored.visibility, v0::ServerEventVisibility::Place);
    assert!(!stored.is_restricted());
}

#[rocket::async_test]
async fn someone_shut_out_of_an_event_does_not_stay_in_it() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let (guest_session, guest) = join(&s).await;
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Invited", "invited": [guest.id, s.member.id], "capacity": 1 }),
    )
    .await;
    let id = event["_id"].as_str().unwrap().to_string();
    let (status, _) = answer(&s, &guest_session, &event, "Going").await;
    assert_eq!(status, Status::Ok);
    let (_, waiting) = answer(&s, &s.member_session, &event, "Going").await;
    assert_eq!(waiting["rsvp"]["waiting"], true);

    // the organiser takes the guest off the list: the guest's answer goes
    // with it, the guest's devices hear it, and the seat moves on
    let mut guest_feed = PubSubTestHelper::new(&format!("{}!", guest.id)).await;
    let (status, edited) = call(
        &s.harness,
        Method::Patch,
        &event_path(&s, &event),
        &s.owner_session,
        Some(json!({ "invited": [s.member.id] })),
    )
    .await;
    assert_eq!(status, Status::Ok, "{edited}");
    assert_eq!(counts(&edited), (1, 0, 0, 0));
    expect_event(&mut guest_feed, "my answer went with my invitation", |e| {
        matches!(e, EventV1::ServerEventRsvpUpdate { id: event, rsvp: None, .. } if event == &id)
    })
    .await;
    assert!(db.fetch_server_event_answer(&id, &guest.id).await.unwrap().is_none());
    let roster = assert_consistent(&s, &id, 1).await;
    assert_eq!(roster.going, vec![s.member.id.clone()]);
}

#[rocket::async_test]
async fn someone_who_lost_the_role_can_still_take_their_answer_back() {
    let s = setup(true).await;
    let db = &s.harness.db;
    let (role, holder_session, holder) = role_holder(&s).await;
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Roles", "visible_roles": [role], "repeat": "Daily" }),
    )
    .await;
    let id = event["_id"].as_str().unwrap().to_string();
    answer(&s, &holder_session, &event, "Going").await;
    let lose_role = || async {
        let mut member = db.fetch_member(&s.server.id, &holder.id).await.unwrap();
        member
            .update(
                db,
                revolt_database::PartialMember {
                    roles: Some(vec![]),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
    };
    lose_role().await;

    // the next occurrence does not take along someone who cannot see it
    let mut stored = db.fetch_server_event(&id).await.unwrap();
    assert!(stored.start(db).await.unwrap());
    assert!(stored.complete(db).await.unwrap());
    let next = db
        .fetch_upcoming_server_events(&s.server.id)
        .await
        .unwrap()
        .into_iter()
        .find(|next| next.previous.as_deref() == Some(id.as_str()))
        .expect("the next day");
    assert_eq!(next.going_count, 0);
    assert!(db.fetch_server_event_answer(&next.id, &holder.id).await.unwrap().is_none());

    // the old answer is still theirs to take back: the route tells them
    // nothing about the event, and the answer is gone
    assert_eq!(db.fetch_server_event(&id).await.unwrap().going_count, 1);
    let (status, body) = call(
        &s.harness,
        Method::Delete,
        &format!("{}/rsvp", event_path(&s, &event)),
        &holder_session,
        None,
    )
    .await;
    assert_eq!(status, Status::NotFound, "{body}");
    assert!(body.get("event").is_none_or(Value::is_null), "{body}");
    assert!(db.fetch_server_event_answer(&id, &holder.id).await.unwrap().is_none());
    assert_eq!(db.fetch_server_event(&id).await.unwrap().going_count, 0);
    // and through the older route as well, for another event
    let other = create_with(&s, park(), json!({ "visibility": "Invited", "invited": [holder.id] })).await;
    let other_id = other["_id"].as_str().unwrap().to_string();
    let mark = format!("{}/interested", event_path(&s, &other));
    let (status, _) = call(&s.harness, Method::Put, &mark, &holder_session, None).await;
    assert_eq!(status, Status::Ok);
    // shut out without an edit of the event's lists: the event moves to a
    // channel they cannot view
    let mut moved = db.fetch_server_event(&other_id).await.unwrap();
    moved.place = v0::ServerEventPlace::Text {
        channel: s.private.id().to_string(),
    };
    db.update_server_event(&moved).await.unwrap();
    let (status, _) = call(&s.harness, Method::Delete, &mark, &holder_session, None).await;
    assert_eq!(status, Status::NotFound);
    assert!(db.fetch_server_event_answer(&other_id, &holder.id).await.unwrap().is_none());
    assert_eq!(db.fetch_server_event(&other_id).await.unwrap().interested_count, 0);
}

#[rocket::async_test]
async fn events_of_a_mature_server_are_not_shown_to_people_who_may_not_open_it() {
    let s = setup(true).await;
    let db = &s.harness.db;
    // a sixteen-year-old, already a member
    let (mut account, young_session, young) = s.harness.new_user().await;
    let age = account.age.as_mut().expect("a new account has an age");
    age.birth_month = "2010-06".to_string();
    // a save only sets the fields it has: write the account anew
    db.remove_account(&account.id).await.expect("account");
    db.save_account(&account).await.expect("account");
    Member::create(db, &s.server, &young, None).await.unwrap();
    let may = |user: User| async move {
        revolt_database::util::age::may_open_mature(db, &user)
            .await
            .unwrap_or(false)
    };
    assert!(may(s.owner.clone()).await, "an adult may open mature content");
    assert!(!may(young.clone()).await, "someone under age may not");
    let event = create(&s, park()).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let (status, _) = fetch(&s, &young_session, &event).await;
    assert_eq!(status, Status::Ok, "visible while the server is not mature");
    answer(&s, &young_session, &event, "Maybe").await;

    // staff mark the server as mature
    db.update_server(
        &s.server.id,
        &revolt_database::PartialServer {
            nsfw: Some(true),
            ..Default::default()
        },
        vec![],
    )
    .await
    .unwrap();

    assert_hidden(&s, &young_session, &event).await;
    let (status, _) = fetch(&s, &s.owner_session, &event).await;
    assert_eq!(status, Status::Ok);

    // the gateway: not on the server topic, where every member listens,
    // but to each person who may see it
    let stored = db.fetch_server_event(&id).await.unwrap();
    match stored.audience(db).await {
        revolt_database::EventAudience::People(people) => {
            assert!(people.contains(&s.owner.id), "{people:?}");
            assert!(people.contains(&s.member.id), "{people:?}");
            assert!(!people.contains(&young.id), "{people:?}");
        }
        other => panic!("sent to {other:?}"),
    }
    let mut everyone = PubSubTestHelper::new(&s.server.id).await;
    let mut young_feed = PubSubTestHelper::new(&format!("{}!", young.id)).await;
    let mut member_feed = PubSubTestHelper::new(&format!("{}!", s.member.id)).await;
    let (status, _) = call(
        &s.harness,
        Method::Patch,
        &event_path(&s, &event),
        &s.owner_session,
        Some(json!({ "title": "Adults only" })),
    )
    .await;
    assert_eq!(status, Status::Ok);
    expect_event(&mut member_feed, "the change, to an adult member", |e| {
        matches!(e, EventV1::ServerEventUpdate { event } if event.id == id)
    })
    .await;
    assert_silent(&mut everyone, "a mature server's event on the server topic", |e| {
        matches!(e, EventV1::ServerEventUpdate { .. } | EventV1::ServerEventCreate(_))
            && about(e, &id)
    })
    .await;
    assert_silent(&mut young_feed, "a mature server's event to someone under age", |e| {
        matches!(e, EventV1::ServerEventUpdate { .. } | EventV1::ServerEventCreate(_))
            && about(e, &id)
    })
    .await;
    // the push at the start leaves them out too
    let server = db.fetch_server(&s.server.id).await.unwrap();
    let place = stored.place_state(db).await;
    let told = stored
        .people_to_tell_at_start(db, &server, &place, &s.text)
        .await
        .unwrap();
    assert!(!told.contains(&young.id), "{told:?}");
}

/// Several organisers seat waiting people in the same moment. Without the
/// word to go beyond the limit, exactly as many are seated as seats are
/// free, whoever asks and however many ask.
#[rocket::async_test]
async fn organisers_seating_at_once_never_pass_the_limit_without_saying_so() {
    let s = setup(true).await;
    let db = &s.harness.db;
    // at the seat list itself: 3 seats, 2 taken, 8 people waiting, and
    // one free seat (a state the API never leaves behind, made here to
    // have a seat to race for)
    let id = format!("SEATRACE{}", ulid::Ulid::new());
    let mut roster = ServerEventRoster::new(&id, &s.server.id, Some(3), true);
    roster.going = vec!["A".to_string(), "B".to_string()];
    roster.waiting = (0..8).map(|n| format!("W{n}")).collect();
    db.insert_server_event_roster(&roster).await.unwrap();
    for round in 0..5 {
        let results = join_all(roster.waiting.iter().map(|user| {
            let change = RosterChange::Seat {
                user: user.clone(),
                over_capacity: false,
            };
            let id = id.clone();
            async move { db.change_server_event_roster(&id, &change).await }
        }))
        .await;
        assert!(results.iter().all(Result::is_ok));
        let now = db.fetch_server_event_roster(&id).await.unwrap().unwrap();
        assert_eq!(now.going.len(), 3, "round {round}: {:?}", now.going);
        assert_eq!(now.waiting.len(), 7, "round {round}");
        let everyone: HashSet<&String> = now.going.iter().chain(&now.waiting).collect();
        assert_eq!(everyone.len(), 10, "round {round}: someone twice or lost");
    }

    // over the API: a full event with five people waiting; the owner and a
    // helper both ask for all five at once
    let (helper_session, helper) = join(&s).await;
    let event = create_with(&s, park(), json!({ "capacity": 1, "co_organisers": [helper.id] })).await;
    let event_id = event["_id"].as_str().unwrap().to_string();
    answer(&s, &s.member_session, &event, "Going").await;
    let mut waiting = Vec::new();
    for _ in 0..5 {
        let (session, user) = join(&s).await;
        answer(&s, &session, &event, "Going").await;
        waiting.push(user.id);
    }
    let seat = |user: &String, session: &Session, confirm: bool| {
        let path = format!(
            "{}/attendees/{user}/seat{}",
            event_path(&s, &event),
            if confirm { "?over_capacity=true" } else { "" }
        );
        let session = session.clone();
        let harness = &s.harness;
        async move { call(harness, Method::Post, &path, &session, None).await }
    };
    let requests: Vec<_> = waiting
        .iter()
        .flat_map(|user| {
            [
                seat(user, &s.owner_session, false),
                seat(user, &helper_session, false),
            ]
        })
        .collect();
    let answers = join_all(requests).await;
    for (status, body) in &answers {
        assert_eq!(*status, Status::Conflict, "{body}");
        assert_eq!(body["type"], "ServerEventOverCapacity", "{body}");
    }
    let unchanged = assert_consistent(&s, &event_id, 1).await;
    assert_eq!(unchanged.going, vec![s.member.id.clone()]);
    assert_eq!(unchanged.waiting, waiting);

    // with the word, both ask for all five at once: each is seated once,
    // and every seat beyond the limit is written down as such
    let requests: Vec<_> = waiting
        .iter()
        .flat_map(|user| {
            [
                seat(user, &s.owner_session, true),
                seat(user, &helper_session, true),
            ]
        })
        .collect();
    let answers = join_all(requests).await;
    let seated = answers.iter().filter(|(status, _)| *status == Status::Ok).count();
    assert_eq!(seated, 5, "each waiting person seated exactly once");
    let roster = db.fetch_server_event_roster(&event_id).await.unwrap().unwrap();
    assert_eq!(roster.going.len(), 6);
    assert!(roster.waiting.is_empty());
    let distinct: HashSet<&String> = roster.going.iter().collect();
    assert_eq!(distinct.len(), 6, "someone holds two seats");
    let stored = db.fetch_server_event(&event_id).await.unwrap();
    assert_eq!((stored.going_count, stored.waitlist_count), (6, 0));
    let entries = db
        .get_server_audit_logs(
            &s.server.id,
            revolt_database::AuditLogQuery {
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
    let beyond = entries
        .iter()
        .filter(|entry| matches!(
            &entry.action,
            revolt_database::AuditLogEntryAction::ServerEventAttendeeSeat {
                event,
                over_capacity: true,
                ..
            } if event == &event_id
        ))
        .count();
    assert_eq!(beyond, 5, "one entry for each seat beyond the limit");
}

/// A measurement, not a check: an event limited to a role in a server with
/// many members. What it costs to work out who is told, and what one answer
/// costs with everything the API does after it. Runs only when the file
/// `/tmp/zrzur-events-load-private` exists, holding the number of members
/// with the role; writes to `/tmp/zrzur-events-load-result.txt`.
#[rocket::async_test]
async fn load_one_private_event_in_a_big_server() {
    use std::time::{Duration as Took, Instant};
    let Ok(setting) = std::fs::read_to_string("/tmp/zrzur-events-load-private") else {
        return;
    };
    let holders: usize = setting.trim().parse().unwrap_or(5_000);
    let note = |line: String| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/zrzur-events-load-result.txt")
            .unwrap();
        writeln!(file, "{line}").unwrap();
    };
    let spread = |times: &mut Vec<Took>| {
        times.sort();
        let at = |q: f64| times[((times.len() as f64 * q) as usize).min(times.len() - 1)];
        format!(
            "p50 {:.1?}, p95 {:.1?}, p99 {:.1?}, slowest {:.1?}",
            at(0.50),
            at(0.95),
            at(0.99),
            times[times.len() - 1]
        )
    };
    let s = setup(true).await;
    let db = &s.harness.db;
    let role = s.harness.new_role(&s.server, 5, None).await;
    // members with the role, written straight into the database
    let users: Vec<String> = (0..holders).map(|n| format!("ROLEUSER{n:018}")).collect();
    for id in &users {
        db.insert_user(&User {
            id: id.clone(),
            username: id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
        db.insert_or_merge_member(&Member {
            id: revolt_database::MemberCompositeKey {
                server: s.server.id.clone(),
                user: id.clone(),
            },
            roles: vec![role.id.clone()],
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let members = db.fetch_member_count(&s.server.id).await.unwrap();
    note(format!(
        "--- private event; database: {}; {members} members in the server, {holders} hold the role",
        std::env::var("TEST_DB").unwrap_or_default()
    ));

    let start = Instant::now();
    let event = create_with(
        &s,
        park(),
        json!({ "visibility": "Roles", "visible_roles": [role.id] }),
    )
    .await;
    note(format!(
        "creating the event over the API (works out and tells everyone who may see it): {:.1?}",
        start.elapsed()
    ));
    let id = event["_id"].as_str().unwrap().to_string();
    let stored = db.fetch_server_event(&id).await.unwrap();
    let start = Instant::now();
    let people = stored.people_who_see(db).await.unwrap();
    note(format!(
        "working out who sees it: {} people in {:.1?}",
        people.len(),
        start.elapsed()
    ));

    // answers by role holders, one after another: the answer itself, then
    // what the API does next (counts to everyone who sees it, the person's
    // own devices)
    let mut answering = Vec::new();
    let mut telling = Vec::new();
    for user in users.iter().take(200) {
        let mut event = stored.clone();
        let start = Instant::now();
        event.answer(db, user, ServerEventAnswer::Going).await.unwrap();
        answering.push(start.elapsed());
        let start = Instant::now();
        event.publish_counts(db).await;
        event.publish_own_answer(user, None).await;
        telling.push(start.elapsed());
    }
    note(format!("200 answers, the answer itself: {}", spread(&mut answering)));
    note(format!(
        "200 answers, telling who sees the event ({}): {}",
        if members > revolt_database::MAX_LIVE_COUNT_AUDIENCE {
            "server above the limit: counts are not sent live"
        } else {
            "counts sent live to each person"
        },
        spread(&mut telling)
    ));
    // what a member pays to read: the list and one event, as a role holder
    let start = Instant::now();
    let user = db.fetch_user(&users[0]).await.unwrap();
    let member = db.fetch_member(&s.server.id, &users[0]).await.unwrap();
    let server = db.fetch_server(&s.server.id).await.unwrap();
    let mut visible = Vec::new();
    for _ in 0..200 {
        let start = Instant::now();
        assert!(stored.visible_to(db, &user, &server, &member).await);
        visible.push(start.elapsed());
    }
    note(format!(
        "the visibility check for one reader, 200 times: {} (with loading the reader: {:.1?})",
        spread(&mut visible),
        start.elapsed()
    ));
    let start = Instant::now();
    let mut started = stored.clone();
    assert!(started.start(db).await.unwrap());
    note(format!(
        "starting it (update to everyone who sees it, push to the 200 who answered): {:.1?}",
        start.elapsed()
    ));
    note("done".into());
}

/// A measurement, not a check: how one event with many registrations
/// behaves on the database under test. It runs only when the file
/// `/tmp/zrzur-events-load` exists, holding "<people> <requests at once>"
/// (10,000 and 50 otherwise), and writes what it measured, as it goes, to
/// `/tmp/zrzur-events-load-result.txt`; otherwise it does nothing. Run it
/// with `internal/test-env/events-load.sh`, which has no time limit.
#[rocket::async_test]
async fn load_one_event_with_ten_thousand_registrations() {
    use std::time::{Duration as Took, Instant};
    let Ok(setting) = std::fs::read_to_string("/tmp/zrzur-events-load") else {
        return;
    };
    let mut numbers = setting.split_whitespace().filter_map(|n| n.parse::<usize>().ok());
    let people = numbers.next().unwrap_or(10_000);
    let at_once = numbers.next().unwrap_or(50).max(1);
    let seats = people / 2;
    let note = |line: String| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/zrzur-events-load-result.txt")
            .unwrap();
        writeln!(file, "{line}").unwrap();
    };
    // p50, p95, p99 and the slowest of a set of request times
    let spread = |times: &mut Vec<Took>| {
        times.sort();
        let at = |q: f64| times[((times.len() as f64 * q) as usize).min(times.len() - 1)];
        format!(
            "p50 {:.1?}, p95 {:.1?}, p99 {:.1?}, slowest {:.1?}",
            at(0.50),
            at(0.95),
            at(0.99),
            times[times.len() - 1]
        )
    };

    let s = setup(true).await;
    let db = &s.harness.db;
    let event = create_with(&s, park(), json!({ "capacity": seats })).await;
    let id = event["_id"].as_str().unwrap().to_string();
    let stored = db.fetch_server_event(&id).await.unwrap();
    let users: Vec<String> = (0..people).map(|n| format!("LOADUSER{n:018}")).collect();
    note(format!(
        "--- database: {}; {people} people, {seats} seats, {at_once} requests at once",
        std::env::var("TEST_DB").unwrap_or_default()
    ));

    // everyone registers, `at_once` requests in flight at a time; each
    // request is timed by itself
    let all = Instant::now();
    let mut every = Vec::with_capacity(people);
    let mut part = Vec::new();
    let step = (people / 5).max(1);
    for batch in users.chunks(at_once) {
        let times = join_all(batch.iter().map(|user| {
            let mut event = stored.clone();
            async move {
                let start = Instant::now();
                event
                    .answer(db, user, ServerEventAnswer::Going)
                    .await
                    .expect("a registration");
                start.elapsed()
            }
        }))
        .await;
        part.extend(times);
        let done = every.len() + part.len();
        if done % step < at_once || done == people {
            if part.len() >= step / 2 || done == people {
                note(format!(
                    "registrations {}..{done}: {} (running {:.1?})",
                    every.len(),
                    spread(&mut part),
                    all.elapsed()
                ));
                every.append(&mut part);
            }
        }
    }
    every.append(&mut part);
    let total = all.elapsed();
    note(format!(
        "all {people} registrations: {total:.1?}, {:.0} a second; {}",
        people as f64 / total.as_secs_f64(),
        spread(&mut every)
    ));
    let roster = assert_consistent(&s, &id, seats).await;
    assert_eq!((roster.going.len(), roster.waiting.len()), (seats, people - seats));
    note("after registering: nobody twice, never more seated than seats, rows and counts match the seat list".into());

    // a tenth of the seated people leave at once; each seat moves on
    let leaving: Vec<String> = roster.going.iter().take(seats / 10).cloned().collect();
    let start = Instant::now();
    let mut left = Vec::new();
    for batch in leaving.chunks(at_once) {
        left.extend(
            join_all(batch.iter().map(|user| {
                let mut event = stored.clone();
                async move {
                    let start = Instant::now();
                    assert!(event.withdraw(db, user).await.expect("a withdrawal"));
                    start.elapsed()
                }
            }))
            .await,
        );
    }
    note(format!(
        "{} seated people leave, each seat moves to the next in line: {:.1?} in all; {}",
        leaving.len(),
        start.elapsed(),
        spread(&mut left)
    ));
    let after = assert_consistent(&s, &id, seats).await;
    assert_eq!(after.going.len(), seats);
    assert_eq!(after.waiting, roster.waiting[leaving.len()..].to_vec());
    note("after leaving: every seat is taken again and the line kept its order".into());

    // reads, each repeated
    let repeat = 200;
    let mut pages = Vec::new();
    let mut deep = Vec::new();
    let mut own = Vec::new();
    let mut fetch = Vec::new();
    for n in 0..repeat {
        let start = Instant::now();
        let page = db
            .fetch_server_event_answers(&id, &[ServerEventAnswer::Going], Some(false), None, 101)
            .await
            .unwrap();
        assert_eq!(page.len(), 101.min(seats));
        pages.push(start.elapsed());
        let start = Instant::now();
        db.fetch_server_event_answers(
            &id,
            &[ServerEventAnswer::Going, ServerEventAnswer::Maybe],
            None,
            Some(&users[(n * 37) % people]),
            101,
        )
        .await
        .unwrap();
        deep.push(start.elapsed());
        let start = Instant::now();
        db.fetch_server_event_answers_of(&users[(n * 53) % people], std::slice::from_ref(&id))
            .await
            .unwrap();
        own.push(start.elapsed());
        let start = Instant::now();
        db.fetch_server_event(&id).await.unwrap();
        fetch.push(start.elapsed());
    }
    note(format!("attendee page of 100, first page, {repeat} times: {}", spread(&mut pages)));
    note(format!("attendee page of 100 from a place inside the list, {repeat} times: {}", spread(&mut deep)));
    note(format!("one person's own answer, {repeat} times: {}", spread(&mut own)));
    note(format!("the event with its counts, {repeat} times: {}", spread(&mut fetch)));
    let start = Instant::now();
    let mut after_user: Option<String> = None;
    let mut read = 0;
    loop {
        let rows = db
            .fetch_server_event_answers(
                &id,
                &[ServerEventAnswer::Going, ServerEventAnswer::Maybe, ServerEventAnswer::NotGoing],
                None,
                after_user.as_deref(),
                1000,
            )
            .await
            .unwrap();
        read += rows.len();
        if rows.len() < 1000 {
            break;
        }
        after_user = rows.last().map(|row| row.id.user.clone());
    }
    note(format!("every answer, as the statistics read them ({read} rows): {:.1?}", start.elapsed()));
    let start = Instant::now();
    db.fetch_server_event_roster(&id).await.unwrap();
    note(format!("the whole seat list: {:.1?}", start.elapsed()));
    let start = Instant::now();
    db.recount_server_event(&id).await.unwrap();
    note(format!("a recount of the event: {:.1?}", start.elapsed()));
    assert_consistent(&s, &id, seats).await;
    note("done".into());
}
