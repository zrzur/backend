use bson::Document;
use futures::StreamExt;
use revolt_result::Result;

use crate::MongoDb;
use crate::{InsightCounter, InsightHour, MessageTally, ServerInsightDay};

use super::AbstractServerInsights;

static COL: &str = "server_insights";
/// crond's note of the last hour rolled up (one document)
static STATE: &str = "server_insights_state";
static CURSOR: &str = "rollup";

fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

fn as_i64(value: Option<&bson::Bson>) -> Option<i64> {
    match value {
        Some(bson::Bson::Int64(n)) => Some(*n),
        Some(bson::Bson::Int32(n)) => Some(*n as i64),
        _ => None,
    }
}

#[async_trait]
impl AbstractServerInsights for MongoDb {
    async fn add_server_insight(
        &self,
        server: &str,
        hour: i64,
        counter: &InsightCounter,
    ) -> Result<()> {
        let day = hour.div_euclid(24);
        let slot = hour.rem_euclid(24);
        let inc = match counter {
            InsightCounter::Join(source) => doc! {
                format!("joins.{slot}"): 1_i32,
                format!("sources.{}", source.key()): 1_i32,
            },
            InsightCounter::Leave => doc! { format!("leaves.{slot}"): 1_i32 },
            InsightCounter::VoiceSeconds(seconds) => {
                doc! { format!("voice.{slot}"): *seconds as i64 }
            }
        };
        let filter = doc! { "_id": ServerInsightDay::key(server, day) };
        let update = doc! {
            "$setOnInsert": { "server": server, "day": day },
            "$inc": inc,
        };
        // two first writes of a day at once: the loser's insert hits the
        // _id; the row is there now, so a plain update counts it
        match self
            .col::<Document>(COL)
            .update_one(filter.clone(), update.clone())
            .upsert(true)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_duplicate(&error) => self
                .col::<Document>(COL)
                .update_one(filter, update)
                .await
                .map(|_| ())
                .map_err(|_| create_database_error!("update_one", COL)),
            Err(_) => Err(create_database_error!("update_one", COL)),
        }
    }

    async fn tally_messages_between(&self, from: &str, to: &str) -> Result<Vec<MessageTally>> {
        #[derive(serde::Deserialize)]
        struct Key {
            c: String,
            a: String,
            w: bool,
        }
        #[derive(serde::Deserialize)]
        struct Group {
            #[serde(rename = "_id")]
            key: Key,
            n: i64,
        }

        // walks the _id index over one hour only
        let mut cursor = self
            .col::<Document>("messages")
            .aggregate(vec![
                doc! { "$match": {
                    "_id": { "$gte": from, "$lt": to },
                    "system": { "$exists": false },
                } },
                doc! { "$group": {
                    "_id": {
                        "c": "$channel",
                        "a": "$author",
                        "w": { "$ne": [{ "$type": "$webhook" }, "missing"] },
                    },
                    "n": { "$sum": 1_i32 },
                } },
            ])
            .await
            .map_err(|_| create_database_error!("aggregate", "messages"))?
            .with_type::<Group>();

        let mut tallies = vec![];
        while let Some(group) = cursor.next().await {
            let group = group.map_err(|_| create_database_error!("aggregate", "messages"))?;
            tallies.push(MessageTally {
                channel: group.key.c,
                author: (!group.key.w).then_some(group.key.a),
                count: group.n.clamp(0, u32::MAX as i64) as u32,
            });
        }
        Ok(tallies)
    }

    async fn add_server_insight_hour(
        &self,
        server: &str,
        hour: i64,
        counts: &InsightHour,
    ) -> Result<bool> {
        let day = hour.div_euclid(24);
        let of_day = hour.rem_euclid(24);
        let key = ServerInsightDay::key(server, day);

        let mut inc = Document::new();
        for (channel, n) in &counts.channels {
            inc.insert(format!("messages.{of_day}.{channel}"), *n as i64);
        }
        let mut set = doc! { "open": true };
        if let Some(active) = counts.active {
            set.insert("active", active as i64);
        }
        let mut update = doc! {
            "$setOnInsert": { "server": server, "day": day },
            "$set": set,
            "$addToSet": { "hours": of_day },
        };
        if !inc.is_empty() {
            update.insert("$inc", inc);
        }
        // only when this hour is not in yet
        let filter = doc! { "_id": &key, "hours": { "$ne": of_day } };

        match self
            .col::<Document>(COL)
            .update_one(filter.clone(), update.clone())
            .upsert(true)
            .await
        {
            Ok(_) => Ok(true),
            // the insert hit the _id: either the hour is in (the filter
            // missed an existing row), or someone created the row at the
            // same moment (a join). A plain update with the same filter
            // tells them apart.
            Err(error) if is_duplicate(&error) => {
                let result = self
                    .col::<Document>(COL)
                    .update_one(filter, update)
                    .await
                    .map_err(|_| create_database_error!("update_one", COL))?;
                Ok(result.matched_count == 1)
            }
            Err(_) => Err(create_database_error!("update_one", COL)),
        }
    }

    async fn fetch_open_server_insight_days(
        &self,
        day: i64,
        limit: i64,
    ) -> Result<Vec<(String, i64)>> {
        let mut cursor = self
            .col::<Document>(COL)
            .find(doc! { "day": { "$lt": day }, "open": true })
            .projection(doc! { "server": 1_i32, "day": 1_i32 })
            .limit(limit)
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut days = vec![];
        while let Some(row) = cursor.next().await {
            let row = row.map_err(|_| create_database_error!("find", COL))?;
            if let (Ok(server), Some(day)) = (row.get_str("server"), as_i64(row.get("day"))) {
                days.push((server.to_string(), day));
            }
        }
        Ok(days)
    }

    async fn close_server_insight_day(
        &self,
        server: &str,
        day: i64,
        active: Option<u32>,
    ) -> Result<()> {
        let mut update = doc! { "$unset": { "open": "" } };
        if let Some(active) = active {
            update.insert("$set", doc! { "active": active as i64 });
        }
        self.col::<Document>(COL)
            .update_one(doc! { "_id": ServerInsightDay::key(server, day) }, update)
            .await
            .map_err(|_| create_database_error!("update_one", COL))
            .map(|_| ())
    }

    async fn delete_server_insights_before(&self, day: i64) -> Result<u64> {
        self.col::<Document>(COL)
            .delete_many(doc! { "day": { "$lt": day } })
            .await
            .map_err(|_| create_database_error!("delete_many", COL))
            .map(|result| result.deleted_count)
    }

    async fn delete_server_insights_of(&self, server: &str) -> Result<()> {
        self.col::<Document>(COL)
            .delete_many(doc! { "server": server })
            .await
            .map_err(|_| create_database_error!("delete_many", COL))
            .map(|_| ())
    }

    async fn fetch_server_insights(
        &self,
        server: &str,
        first: i64,
        last: i64,
    ) -> Result<Vec<ServerInsightDay>> {
        let mut cursor = self
            .col::<ServerInsightDay>(COL)
            .find(doc! { "server": server, "day": { "$gte": first, "$lte": last } })
            .sort(doc! { "day": 1_i32 })
            .await
            .map_err(|_| create_database_error!("find", COL))?;
        let mut rows = vec![];
        while let Some(row) = cursor.next().await {
            rows.push(row.map_err(|_| create_database_error!("find", COL))?);
        }
        Ok(rows)
    }

    async fn fetch_server_insight_cursor(&self) -> Result<Option<i64>> {
        let state = self
            .col::<Document>(STATE)
            .find_one(doc! { "_id": CURSOR })
            .await
            .map_err(|_| create_database_error!("find_one", STATE))?;
        Ok(state.and_then(|state| as_i64(state.get("hour"))))
    }

    async fn set_server_insight_cursor(&self, hour: i64, since: Option<i64>) -> Result<()> {
        let mut update = doc! { "$set": { "hour": hour } };
        if let Some(since) = since {
            update.insert("$setOnInsert", doc! { "since": since });
        }
        self.col::<Document>(STATE)
            .update_one(doc! { "_id": CURSOR }, update)
            .upsert(true)
            .await
            .map_err(|_| create_database_error!("update_one", STATE))
            .map(|_| ())
    }

    async fn fetch_server_insight_since(&self) -> Result<Option<i64>> {
        let state = self
            .col::<Document>(STATE)
            .find_one(doc! { "_id": CURSOR })
            .await
            .map_err(|_| create_database_error!("find_one", STATE))?;
        Ok(state.and_then(|state| as_i64(state.get("since"))))
    }
}
