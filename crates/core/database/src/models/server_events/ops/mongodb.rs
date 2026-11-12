use bson::{to_bson, to_document, Document};
use futures::StreamExt;
use iso8601_timestamp::Timestamp;
use mongodb::options::{FindOptions, ReturnDocument};
use revolt_models::v0::ServerEventStatus;
use revolt_result::Result;

use crate::MongoDb;
use crate::{ServerEvent, ServerEventInterest, ServerEventInterestKey};

use super::AbstractServerEvents;

static COL: &str = "server_events";
static INTERESTS: &str = "server_event_interests";

fn status_name(status: &ServerEventStatus) -> &'static str {
    match status {
        ServerEventStatus::Scheduled => "Scheduled",
        ServerEventStatus::Active => "Active",
        ServerEventStatus::Completed => "Completed",
        ServerEventStatus::Cancelled => "Cancelled",
    }
}

/// Optional fields a manager can clear
const OPTIONAL: [&str; 5] = ["description", "ends_at", "cover", "repeat", "previous"];

impl MongoDb {
    async fn find_events(&self, filter: Document, sort: Option<Document>) -> Result<Vec<ServerEvent>> {
        let options = FindOptions::builder().sort(sort).build();
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

    async fn change_count(&self, event: &str, by: i32) -> Result<u32> {
        let updated = self
            .col::<ServerEvent>(COL)
            .find_one_and_update(
                doc! { "_id": event },
                doc! { "$inc": { "interested_count": by } },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(|_| create_database_error!("find_one_and_update", COL))?
            .ok_or_else(|| create_error!(NotFound))?;
        Ok(updated.interested_count)
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
        )
        .await
    }

    async fn update_server_event(&self, event: &ServerEvent) -> Result<()> {
        let mut set =
            to_document(event).map_err(|_| create_database_error!("to_document", COL))?;
        for owned in [
            "_id",
            "status",
            "interested_count",
            "started_at",
            "ended_at",
            "server",
            "creator",
            "series_start",
            "occurrence",
            "next",
            "time_zone",
        ] {
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
        let field = if to == ServerEventStatus::Active {
            "started_at"
        } else {
            "ended_at"
        };
        self.col::<ServerEvent>(COL)
            .update_one(
                // only from the expected status: crond and a manager acting
                // at once can't both start (or end) it
                doc! { "_id": id, "status": status_name(&from) },
                {
                    let mut set = doc! { "status": status_name(&to) };
                    set.insert(field, at);
                    doc! { "$set": set }
                },
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
        self.col::<Document>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    async fn fetch_server_events_to_start(&self, now: Timestamp) -> Result<Vec<ServerEvent>> {
        let now = to_bson(&now).map_err(|_| create_database_error!("to_bson", COL))?;
        self.find_events(
            doc! { "status": "Scheduled", "starts_at": { "$lte": now } },
            Some(doc! { "starts_at": 1_i32 }),
        )
        .await
    }

    async fn fetch_active_server_events(&self) -> Result<Vec<ServerEvent>> {
        self.find_events(doc! { "status": "Active" }, None).await
    }

    async fn add_server_event_interest(
        &self,
        event: &str,
        server: &str,
        user: &str,
    ) -> Result<Option<u32>> {
        let interest = ServerEventInterest {
            id: ServerEventInterestKey {
                event: event.to_string(),
                user: user.to_string(),
            },
            server: server.to_string(),
            created_at: Timestamp::now_utc(),
        };
        match self
            .col::<ServerEventInterest>(INTERESTS)
            .insert_one(&interest)
            .await
        {
            Ok(_) => Ok(Some(self.change_count(event, 1).await?)),
            // the _id (event + user) is taken: marked already
            Err(error) if error.to_string().contains("E11000") => Ok(None),
            Err(_) => Err(create_database_error!("insert_one", INTERESTS)),
        }
    }

    async fn remove_server_event_interest(&self, event: &str, user: &str) -> Result<Option<u32>> {
        let key = to_bson(&ServerEventInterestKey {
            event: event.to_string(),
            user: user.to_string(),
        })
        .map_err(|_| create_database_error!("to_bson", INTERESTS))?;
        let deleted = self
            .col::<Document>(INTERESTS)
            .delete_one(doc! { "_id": key })
            .await
            .map_err(|_| create_database_error!("delete_one", INTERESTS))?
            .deleted_count;
        if deleted == 0 {
            return Ok(None);
        }
        Ok(Some(self.change_count(event, -1).await?))
    }

    async fn fetch_server_event_interested(
        &self,
        event: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>> {
        let mut filter = doc! { "_id.event": event };
        if let Some(after) = after {
            filter.insert("_id.user", doc! { "$gt": after });
        }
        let options = FindOptions::builder()
            .sort(doc! { "_id.user": 1_i32 })
            .limit(limit)
            .build();
        let mut cursor = self
            .col::<ServerEventInterest>(INTERESTS)
            .find(filter)
            .with_options(options)
            .await
            .map_err(|_| create_database_error!("find", INTERESTS))?;
        let mut users = vec![];
        while let Some(interest) = cursor.next().await {
            users.push(
                interest
                    .map_err(|_| create_database_error!("find", INTERESTS))?
                    .id
                    .user,
            );
        }
        Ok(users)
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
        )
        .await
    }

    async fn remove_server_event_interests_of(
        &self,
        server: &str,
        user: &str,
    ) -> Result<Vec<String>> {
        let mut cursor = self
            .col::<ServerEventInterest>(INTERESTS)
            .find(doc! { "server": server, "_id.user": user })
            .await
            .map_err(|_| create_database_error!("find", INTERESTS))?;
        let mut events = vec![];
        while let Some(interest) = cursor.next().await {
            events.push(
                interest
                    .map_err(|_| create_database_error!("find", INTERESTS))?
                    .id
                    .event,
            );
        }
        let mut changed = vec![];
        for event in events {
            if self.remove_server_event_interest(&event, user).await?.is_some() {
                changed.push(event);
            }
        }
        Ok(changed)
    }

    async fn fetch_server_event_interests_of(
        &self,
        user: &str,
        events: &[String],
    ) -> Result<Vec<String>> {
        if events.is_empty() {
            return Ok(vec![]);
        }
        let mut cursor = self
            .col::<ServerEventInterest>(INTERESTS)
            .find(doc! { "_id.user": user, "_id.event": { "$in": events } })
            .await
            .map_err(|_| create_database_error!("find", INTERESTS))?;
        let mut ids = vec![];
        while let Some(interest) = cursor.next().await {
            ids.push(
                interest
                    .map_err(|_| create_database_error!("find", INTERESTS))?
                    .id
                    .event,
            );
        }
        Ok(ids)
    }
}
