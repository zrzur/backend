use bson::{to_bson, to_document, Bson, Document};
use futures::StreamExt;
use iso8601_timestamp::Timestamp;
use mongodb::options::{FindOptions, ReturnDocument};
use revolt_models::v0::{ServerEventAnswer, ServerEventStatus};
use revolt_result::Result;

use crate::MongoDb;
use crate::{
    RosterChange, ServerEvent, ServerEventInterest, ServerEventInterestKey,
    ServerEventInvitation, ServerEventRoster, MAX_SEATS, MAX_WAITING,
};

use super::AbstractServerEvents;

static COL: &str = "server_events";
/// The answers (the collection keeps the name it had when an answer was an
/// "Interested" mark)
static INTERESTS: &str = "server_event_interests";
static ROSTERS: &str = "server_event_rosters";

fn status_name(status: &ServerEventStatus) -> &'static str {
    match status {
        ServerEventStatus::Scheduled => "Scheduled",
        ServerEventStatus::Active => "Active",
        ServerEventStatus::Completed => "Completed",
        ServerEventStatus::Cancelled => "Cancelled",
        ServerEventStatus::Draft => "Draft",
    }
}

/// Optional fields an organiser can clear. A list that became empty is not
/// in the written document either, so it has to be removed the same way.
const OPTIONAL: [&str; 11] = [
    "reminders",
    "link",
    "description",
    "ends_at",
    "cover",
    "repeat",
    "previous",
    "capacity",
    "visible_roles",
    "invited",
    "co_organisers",
];

/// Fields that change on their own and are never written from an edit
const OWNED: [&str; 19] = [
    "reminders_sent",
    "invitations",
    "people_removed_at",
    "_id",
    "status",
    "interested_count",
    "going_count",
    "not_going_count",
    "waitlist_count",
    "roster_version",
    "present_at_start",
    "started_at",
    "ended_at",
    "server",
    "creator",
    "series_start",
    "occurrence",
    "next",
    "time_zone",
];

fn key_bson(event: &str, user: &str) -> Result<Bson> {
    to_bson(&ServerEventInterestKey {
        event: event.to_string(),
        user: user.to_string(),
    })
    .map_err(|_| create_database_error!("to_bson", INTERESTS))
}

/// Rows with one of these answers. A row from before answers has no
/// `answer` and counts as "maybe".
fn answer_filter(answers: &[ServerEventAnswer]) -> Document {
    let names: Vec<&str> = answers.iter().map(ServerEventAnswer::name).collect();
    if answers.contains(&ServerEventAnswer::Maybe) {
        doc! { "$or": [{ "answer": { "$in": names } }, { "answer": { "$exists": false } }] }
    } else {
        doc! { "answer": { "$in": names } }
    }
}

/// The update pipeline that makes one change to a seat list: the same rules
/// as `ServerEventRoster::apply`, in one write.
///
/// 1. `_g`, `_w`: the lists after the person joined, left or was seated;
///    `_p`: who an organiser seated; the new settings
/// 2. `_n`: how many waiting people move up into free seats
/// 3. the lists, who moved up, the next version
/// 4. the working fields go
pub fn roster_pipeline(change: &RosterChange) -> Vec<Document> {
    let user = |id: &str| doc! { "$literal": id };
    let seats = doc! { "$min": [MAX_SEATS as i64, { "$ifNull": ["$capacity", MAX_SEATS as i64] }] };
    let without = |list: &str, id: &str| {
        doc! { "$filter": { "input": list, "cond": { "$ne": ["$$this", user(id)] } } }
    };
    // how many waiting people fit into the free seats
    let settle = doc! { "$max": [0_i64, { "$min": [
        { "$size": "$_w" },
        { "$subtract": [seats.clone(), { "$size": "$_g" }] }
    ] }] };

    let (first, moved) = match change {
        RosterChange::Join(id) => {
            let absent = doc! { "$and": [
                { "$not": [{ "$in": [user(id), "$going"] }] },
                { "$not": [{ "$in": [user(id), "$waiting"] }] }
            ] };
            let free = doc! { "$lt": [{ "$size": "$going" }, seats.clone()] };
            (
                doc! {
                    "_g": { "$cond": [
                        { "$and": [absent.clone(), free.clone()] },
                        { "$concatArrays": ["$going", [user(id)]] },
                        "$going"
                    ] },
                    "_w": { "$cond": [
                        { "$and": [
                            absent,
                            { "$not": [free] },
                            { "$eq": ["$waitlist", true] },
                            { "$lt": [{ "$size": "$waiting" }, MAX_WAITING as i64] }
                        ] },
                        { "$concatArrays": ["$waiting", [user(id)]] },
                        "$waiting"
                    ] },
                    "_p": [],
                },
                Bson::Int64(0),
            )
        }
        RosterChange::Leave(id) => (
            doc! {
                "_g": without("$going", id),
                "_w": without("$waiting", id),
                "_p": [],
            },
            Bson::Document(settle),
        ),
        RosterChange::Drop(id) => (
            doc! {
                "_g": without("$going", id),
                "_w": without("$waiting", id),
                "_p": [],
            },
            Bson::Int64(0),
        ),
        RosterChange::Seat { user: id, over_capacity } => {
            // waiting, and a seat is free or the organiser said to go
            // beyond the limit: decided inside the same write
            let waits = doc! { "$and": [
                { "$in": [user(id), "$waiting"] },
                { "$or": [
                    *over_capacity,
                    { "$lt": [{ "$size": "$going" }, seats.clone()] }
                ] }
            ] };
            (
                doc! {
                    "_g": { "$cond": [
                        waits.clone(),
                        { "$concatArrays": ["$going", [user(id)]] },
                        "$going"
                    ] },
                    "_w": { "$cond": [waits.clone(), without("$waiting", id), "$waiting"] },
                    "_p": { "$cond": [waits, [user(id)], []] },
                },
                Bson::Int64(0),
            )
        }
        RosterChange::Settings { capacity, waitlist } => (
            doc! {
                "_g": "$going",
                "_w": "$waiting",
                "_p": [],
                "capacity": match capacity {
                    Some(capacity) => Bson::Int64(*capacity as i64),
                    None => Bson::String("$$REMOVE".to_string()),
                },
                "waitlist": *waitlist,
            },
            Bson::Document(settle),
        ),
    };

    let moves = doc! { "$gt": ["$_n", 0_i64] };
    let up = doc! { "$cond": [moves.clone(), { "$slice": ["$_w", "$_n"] }, []] };
    let rest = doc! { "$cond": [
        moves,
        { "$slice": ["$_w", "$_n", { "$max": [1_i64, { "$size": "$_w" }] }] },
        "$_w"
    ] };
    vec![
        doc! { "$set": first },
        doc! { "$set": { "_n": moved } },
        doc! { "$set": {
            "going": { "$concatArrays": ["$_g", up.clone()] },
            "waiting": rest,
            "promoted": { "$concatArrays": ["$_p", up] },
            "version": { "$add": [{ "$ifNull": ["$version", 0_i64] }, 1_i64] },
        } },
        doc! { "$unset": ["_g", "_w", "_p", "_n"] },
    ]
}

impl MongoDb {
    async fn find_events(
        &self,
        filter: Document,
        sort: Option<Document>,
        limit: Option<i64>,
    ) -> Result<Vec<ServerEvent>> {
        let options = FindOptions::builder().sort(sort).limit(limit).build();
        let mut cursor = self
            .col::<ServerEvent>(COL)
            .find(filter)
            .with_options(options)
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut events = vec![];
        while let Some(event) = cursor.next().await {
            events.push(event.map_err(|_| create_database_error!("find", COL))?);
        }
        Ok(events)
    }

    async fn find_answers(
        &self,
        filter: Document,
        sort: Option<Document>,
        limit: Option<i64>,
    ) -> Result<Vec<ServerEventInterest>> {
        let options = FindOptions::builder().sort(sort).limit(limit).build();
        let mut cursor = self
            .col::<ServerEventInterest>(INTERESTS)
            .find(filter)
            .with_options(options)
            .await
            .map_err(|_| create_database_error!("find", INTERESTS))?;
        let mut rows = vec![];
        while let Some(row) = cursor.next().await {
            rows.push(row.map_err(|_| create_database_error!("find", INTERESTS))?);
        }
        Ok(rows)
    }

    async fn count_answers(&self, filter: Document) -> Result<u32> {
        self.col::<Document>(INTERESTS)
            .count_documents(filter)
            .await
            .map(|count| count as u32)
            .map_err(|_| create_database_error!("count_documents", INTERESTS))
    }
}

#[async_trait]
impl AbstractServerEvents for MongoDb {
    async fn insert_server_event(&self, event: &ServerEvent) -> Result<()> {
        // through `to_document`, like the status changes and the crond query:
        // a typed insert stores the times in another BSON form, and the
        // `starts_at <= now` query then matches none of them
        let document =
            to_document(event).map_err(|_| create_database_error!("to_document", COL))?;
        self.col::<Document>(COL)
            .insert_one(document)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", COL))
    }

    async fn fetch_server_event(&self, id: &str) -> Result<ServerEvent> {
        self.col::<ServerEvent>(COL)
            .find_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("find_one", COL))?
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_upcoming_server_events(&self, server: &str) -> Result<Vec<ServerEvent>> {
        self.find_events(
            doc! {
                "server": server,
                "status": { "$in": ["Scheduled", "Active"] }
            },
            Some(doc! { "starts_at": 1_i32, "_id": 1_i32 }),
            None,
        )
        .await
    }

    async fn fetch_server_event_drafts(&self, server: &str) -> Result<Vec<ServerEvent>> {
        self.find_events(
            doc! { "server": server, "status": "Draft" },
            Some(doc! { "starts_at": 1_i32, "_id": 1_i32 }),
            None,
        )
        .await
    }

    async fn fetch_server_events_between(
        &self,
        servers: &[String],
        from: Timestamp,
        to: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>> {
        if servers.is_empty() {
            return Ok(vec![]);
        }
        let from = to_bson(&from).map_err(|_| create_database_error!("to_bson", COL))?;
        let to = to_bson(&to).map_err(|_| create_database_error!("to_bson", COL))?;
        self.find_events(
            doc! {
                "server": { "$in": servers },
                "status": { "$ne": "Draft" },
                "starts_at": { "$gte": from, "$lt": to }
            },
            Some(doc! { "starts_at": 1_i32, "_id": 1_i32 }),
            Some(limit),
        )
        .await
    }

    async fn fetch_server_events_created_by(&self, user: &str) -> Result<Vec<ServerEvent>> {
        self.find_events(doc! { "creator": user }, Some(doc! { "_id": 1_i32 }), None)
            .await
    }

    async fn fetch_server_events_naming(&self, user: &str) -> Result<Vec<ServerEvent>> {
        self.find_events(
            doc! { "$or": [{ "invited": user }, { "co_organisers": user }] },
            Some(doc! { "_id": 1_i32 }),
            None,
        )
        .await
    }

    async fn update_server_event(&self, event: &ServerEvent) -> Result<()> {
        let mut set =
            to_document(event).map_err(|_| create_database_error!("to_document", COL))?;
        for owned in OWNED {
            set.remove(owned);
        }
        let mut unset = Document::new();
        for field in OPTIONAL {
            if !set.contains_key(field) {
                unset.insert(field, 1_i32);
            }
        }
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }
        self.col::<ServerEvent>(COL)
            .update_one(doc! { "_id": &event.id }, update)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn set_server_event_status(
        &self,
        id: &str,
        from: ServerEventStatus,
        to: ServerEventStatus,
        at: Timestamp,
    ) -> Result<bool> {
        let at = to_bson(&at).map_err(|_| create_database_error!("to_bson", COL))?;
        let mut set = doc! { "status": status_name(&to) };
        match to {
            ServerEventStatus::Active => {
                set.insert("started_at", at);
            }
            // a draft that is published has neither started nor ended
            ServerEventStatus::Scheduled | ServerEventStatus::Draft => {}
            _ => {
                set.insert("ended_at", at);
            }
        }
        self.col::<ServerEvent>(COL)
            .update_one(
                // only from the expected status: crond and a manager acting
                // at once can't both start (or end) it
                doc! { "_id": id, "status": status_name(&from) },
                doc! { "$set": set },
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn delete_server_event(&self, id: &str) -> Result<()> {
        self.col::<Document>(INTERESTS)
            .delete_many(doc! { "_id.event": id })
            .await
            .map_err(|_| create_database_error!("delete_many", INTERESTS))?;
        self.col::<Document>(ROSTERS)
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("delete_one", ROSTERS))?;
        self.col::<Document>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    async fn delete_server_events_of(&self, server: &str) -> Result<Vec<ServerEvent>> {
        let events = self.find_events(doc! { "server": server }, None, None).await?;
        self.col::<Document>(INTERESTS)
            .delete_many(doc! { "server": server })
            .await
            .map_err(|_| create_database_error!("delete_many", INTERESTS))?;
        self.col::<Document>(ROSTERS)
            .delete_many(doc! { "server": server })
            .await
            .map_err(|_| create_database_error!("delete_many", ROSTERS))?;
        self.col::<Document>(COL)
            .delete_many(doc! { "server": server })
            .await
            .map_err(|_| create_database_error!("delete_many", COL))?;
        Ok(events)
    }

    async fn fetch_server_events_to_start(&self, now: Timestamp) -> Result<Vec<ServerEvent>> {
        let now = to_bson(&now).map_err(|_| create_database_error!("to_bson", COL))?;
        self.find_events(
            doc! { "status": "Scheduled", "starts_at": { "$lte": now } },
            Some(doc! { "starts_at": 1_i32 }),
            None,
        )
        .await
    }

    async fn fetch_active_server_events(&self) -> Result<Vec<ServerEvent>> {
        self.find_events(doc! { "status": "Active" }, None, None).await
    }

    async fn set_server_event_answer(
        &self,
        event: &str,
        server: &str,
        user: &str,
        answer: ServerEventAnswer,
        waiting: bool,
    ) -> Result<Option<ServerEventInterest>> {
        let now = to_bson(&Timestamp::now_utc())
            .map_err(|_| create_database_error!("to_bson", INTERESTS))?;
        let update = doc! {
            "$set": {
                "server": server,
                "answer": answer.name(),
                "waiting": waiting,
                "updated_at": now.clone(),
            },
            "$setOnInsert": { "created_at": now },
        };
        // two first answers of one person at once: the second insert meets
        // the row of the first and is tried again as an update
        for attempt in 0..3 {
            match self
                .col::<ServerEventInterest>(INTERESTS)
                .find_one_and_update(doc! { "_id": key_bson(event, user)? }, update.clone())
                .upsert(true)
                .return_document(ReturnDocument::Before)
                .await
            {
                Ok(before) => return Ok(before),
                Err(error) if attempt < 2 && error.to_string().contains("E11000") => continue,
                Err(_) => break,
            }
        }
        Err(create_database_error!("find_one_and_update", INTERESTS))
    }

    async fn delete_server_event_answer(
        &self,
        event: &str,
        user: &str,
        only: Option<ServerEventAnswer>,
    ) -> Result<Option<ServerEventInterest>> {
        let mut filter = doc! { "_id": key_bson(event, user)? };
        if let Some(only) = only {
            filter.extend(answer_filter(&[only]));
        }
        self.col::<ServerEventInterest>(INTERESTS)
            .find_one_and_delete(filter)
            .await
            .map_err(|_| create_database_error!("find_one_and_delete", INTERESTS))
    }

    async fn set_server_event_answer_waiting(
        &self,
        event: &str,
        user: &str,
        waiting: bool,
    ) -> Result<bool> {
        self.col::<Document>(INTERESTS)
            .update_one(
                doc! { "_id": key_bson(event, user)?, "answer": "Going" },
                doc! { "$set": { "waiting": waiting } },
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", INTERESTS))
    }

    async fn fetch_server_event_answer(
        &self,
        event: &str,
        user: &str,
    ) -> Result<Option<ServerEventInterest>> {
        self.col::<ServerEventInterest>(INTERESTS)
            .find_one(doc! { "_id": key_bson(event, user)? })
            .await
            .map_err(|_| create_database_error!("find_one", INTERESTS))
    }

    async fn fetch_server_event_answers_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<ServerEventInterest>> {
        if events.is_empty() {
            return Ok(vec![]);
        }
        self.find_answers(
            doc! { "_id.user": user, "_id.event": { "$in": events } },
            None,
            None,
        )
        .await
    }

    async fn fetch_server_event_answers_by_user(
        &self,
        user: &str,
        server: Option<&str>,
    ) -> Result<Vec<ServerEventInterest>> {
        let mut filter = doc! { "_id.user": user };
        if let Some(server) = server {
            filter.insert("server", server);
        }
        self.find_answers(filter, Some(doc! { "_id.event": 1_i32 }), None)
            .await
    }

    async fn fetch_server_event_answers(
        &self,
        event: &str,
        answers: &[ServerEventAnswer],
        waiting: Option<bool>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ServerEventInterest>> {
        let mut all = vec![doc! { "_id.event": event }, answer_filter(answers)];
        if let Some(after) = after {
            all.push(doc! { "_id.user": { "$gt": after } });
        }
        match waiting {
            // only "going" answers wait: the others pass either way
            Some(true) => all.push(doc! { "$or": [
                { "answer": { "$ne": "Going" } },
                { "waiting": true }
            ] }),
            Some(false) => all.push(doc! { "waiting": { "$ne": true } }),
            None => {}
        }
        self.find_answers(
            doc! { "$and": all },
            Some(doc! { "_id.user": 1_i32 }),
            Some(limit),
        )
        .await
    }

    async fn add_server_event_counts(
        &self,
        event: &str,
        maybe: i32,
        not_going: i32,
    ) -> Result<()> {
        if maybe == 0 && not_going == 0 {
            return Ok(());
        }
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": event },
                doc! { "$inc": { "interested_count": maybe, "not_going_count": not_going } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn set_server_event_seat_counts(
        &self,
        event: &str,
        going: u32,
        waiting: u32,
        version: i64,
    ) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": event,
                    "$or": [
                        { "roster_version": { "$exists": false } },
                        { "roster_version": { "$lt": version } }
                    ]
                },
                doc! { "$set": {
                    "going_count": going as i64,
                    "waitlist_count": waiting as i64,
                    "roster_version": version,
                } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn recount_server_event(&self, event: &str) -> Result<()> {
        let of = |more: Document| {
            let mut filter = doc! { "_id.event": event };
            filter.extend(more);
            filter
        };
        let maybe = self
            .count_answers(of(answer_filter(&[ServerEventAnswer::Maybe])))
            .await?;
        let not_going = self.count_answers(of(doc! { "answer": "NotGoing" })).await?;
        let mut set = doc! {
            "interested_count": maybe as i64,
            "not_going_count": not_going as i64,
        };
        // the seat list is what decides seats; without one, the rows do
        match self.fetch_server_event_roster(event).await? {
            Some(roster) => {
                set.insert("going_count", roster.going.len() as i64);
                set.insert("waitlist_count", roster.waiting.len() as i64);
                set.insert("roster_version", roster.version);
            }
            None => {
                let going = self
                    .count_answers(of(doc! { "answer": "Going", "waiting": { "$ne": true } }))
                    .await?;
                let waiting = self
                    .count_answers(of(doc! { "answer": "Going", "waiting": true }))
                    .await?;
                set.insert("going_count", going as i64);
                set.insert("waitlist_count", waiting as i64);
            }
        }
        self.col::<Document>(COL)
            .update_one(doc! { "_id": event }, doc! { "$set": set })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn insert_server_event_roster(&self, roster: &ServerEventRoster) -> Result<()> {
        let document =
            to_document(roster).map_err(|_| create_database_error!("to_document", ROSTERS))?;
        // written only when the event has none: two writers make one
        self.col::<Document>(ROSTERS)
            .update_one(
                doc! { "_id": &roster.id },
                doc! { "$setOnInsert": document },
            )
            .upsert(true)
            .await
            .map(|_| ())
            .or_else(|error| {
                if error.to_string().contains("E11000") {
                    Ok(())
                } else {
                    Err(create_database_error!("update_one", ROSTERS))
                }
            })
    }

    async fn fetch_server_event_roster(&self, event: &str) -> Result<Option<ServerEventRoster>> {
        self.col::<ServerEventRoster>(ROSTERS)
            .find_one(doc! { "_id": event })
            .await
            .map_err(|_| create_database_error!("find_one", ROSTERS))
    }

    async fn change_server_event_roster(
        &self,
        event: &str,
        change: &RosterChange,
    ) -> Result<Option<ServerEventRoster>> {
        self.col::<ServerEventRoster>(ROSTERS)
            .find_one_and_update(doc! { "_id": event }, roster_pipeline(change))
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| create_database_error!("find_one_and_update", ROSTERS))
    }

    async fn set_server_event_presence(&self, id: &str, count: u32) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": id },
                doc! { "$set": { "present_at_start": count as i64 } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn pull_server_event_person(&self, server: Option<&str>, user: &str) -> Result<()> {
        let mut filter = doc! { "$or": [{ "invited": user }, { "co_organisers": user }] };
        if let Some(server) = server {
            filter.insert("server", server);
        }
        self.col::<Document>(COL)
            .update_many(
                filter,
                doc! { "$pull": { "invited": user, "co_organisers": user } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", COL))
    }

    async fn fetch_server_events_past_retention(
        &self,
        before: Timestamp,
        limit: i64,
    ) -> Result<Vec<ServerEvent>> {
        let before = to_bson(&before).map_err(|_| create_database_error!("to_bson", COL))?;
        self.find_events(
            doc! {
                "status": { "$in": ["Completed", "Cancelled"] },
                "ended_at": { "$lte": before },
                "people_removed_at": { "$exists": false }
            },
            Some(doc! { "ended_at": 1_i32, "_id": 1_i32 }),
            Some(limit),
        )
        .await
    }

    async fn remove_server_event_people(&self, id: &str, at: Timestamp) -> Result<u64> {
        let at = to_bson(&at).map_err(|_| create_database_error!("to_bson", COL))?;
        let removed = self
            .col::<Document>(INTERESTS)
            .delete_many(doc! { "_id.event": id })
            .await
            .map_err(|_| create_database_error!("delete_many", INTERESTS))?
            .deleted_count;
        self.col::<Document>(ROSTERS)
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("delete_one", ROSTERS))?;
        // marked last: a pass that stopped half way does the event again
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": id },
                doc! {
                    "$set": { "people_removed_at": at },
                    "$unset": { "invited": 1_i32, "co_organisers": 1_i32 }
                },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(removed)
    }

    async fn set_server_event_next(&self, id: &str, next: &str) -> Result<()> {
        self.col::<ServerEvent>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "next": next } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn fetch_server_events_missing_next(&self) -> Result<Vec<ServerEvent>> {
        self.find_events(
            doc! {
                "status": { "$in": ["Completed", "Cancelled"] },
                "repeat": { "$exists": true },
                "next": { "$exists": false }
            },
            None,
            None,
        )
        .await
    }

    async fn fetch_server_events_with_reminders(
        &self,
        now: Timestamp,
        until: Timestamp,
    ) -> Result<Vec<ServerEvent>> {
        let now = to_bson(&now).map_err(|_| create_database_error!("to_bson", COL))?;
        let until = to_bson(&until).map_err(|_| create_database_error!("to_bson", COL))?;
        self.find_events(
            doc! {
                "status": "Scheduled",
                "starts_at": { "$gt": now, "$lte": until },
                "reminders.0": { "$exists": true }
            },
            Some(doc! { "starts_at": 1_i32 }),
            None,
        )
        .await
    }

    async fn mark_server_event_reminder(&self, id: &str, minutes: u32) -> Result<bool> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": id,
                    "status": "Scheduled",
                    "reminders_sent": { "$ne": minutes as i64 }
                },
                doc! { "$addToSet": { "reminders_sent": minutes as i64 } },
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn clear_server_event_reminders_sent(&self, id: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(doc! { "_id": id }, doc! { "$unset": { "reminders_sent": 1_i32 } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn set_server_event_answer_muted(
        &self,
        event: &str,
        user: &str,
        muted: bool,
    ) -> Result<bool> {
        self.col::<Document>(INTERESTS)
            .update_one(
                doc! { "_id": key_bson(event, user)? },
                doc! { "$set": { "muted": muted } },
            )
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", INTERESTS))
    }

    async fn fetch_server_event_muted(&self, event: &str) -> Result<Vec<String>> {
        Ok(self
            .find_answers(doc! { "_id.event": event, "muted": true }, None, None)
            .await?
            .into_iter()
            .map(|row| row.id.user)
            .collect())
    }

    async fn fetch_server_event_by_link(&self, code: &str) -> Result<Option<ServerEvent>> {
        self.col::<ServerEvent>(COL)
            .find_one(doc! { "link.code": code })
            .await
            .map_err(|_| create_database_error!("find_one", COL))
    }

    async fn fetch_server_event_by_invitation(&self, code: &str) -> Result<Option<ServerEvent>> {
        self.col::<ServerEvent>(COL)
            .find_one(doc! { "invitations.code": code })
            .await
            .map_err(|_| create_database_error!("find_one", COL))
    }

    async fn add_server_event_invitation(
        &self,
        id: &str,
        invitation: &ServerEventInvitation,
        max: usize,
    ) -> Result<bool> {
        let row = to_bson(invitation).map_err(|_| create_database_error!("to_bson", COL))?;
        // the slot `max - 1` is empty: fewer than `max` links
        let mut filter = doc! { "_id": id };
        filter.insert(format!("invitations.{}", max - 1), doc! { "$exists": false });
        self.col::<Document>(COL)
            .update_one(filter, doc! { "$push": { "invitations": row } })
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn remove_server_event_invitation(&self, id: &str, code: &str) -> Result<bool> {
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": id, "invitations.code": code },
                doc! { "$pull": { "invitations": { "code": code } } },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn use_server_event_invitation(
        &self,
        id: &str,
        code: &str,
        max_uses: Option<u32>,
        user: &str,
        max_invited: usize,
    ) -> Result<bool> {
        let mut link = doc! { "code": code };
        if let Some(max) = max_uses {
            link.insert("uses", doc! { "$lt": max as i64 });
        }
        let mut filter = doc! { "_id": id, "invited": { "$ne": user } };
        filter.insert(format!("invited.{}", max_invited - 1), doc! { "$exists": false });
        filter.insert("invitations", doc! { "$elemMatch": link });
        self.col::<Document>(COL)
            .update_one(
                filter,
                doc! {
                    "$push": { "invited": user },
                    "$inc": { "invitations.$[link].uses": 1_i32 }
                },
            )
            .array_filters(vec![doc! { "link.code": code }])
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }
}
