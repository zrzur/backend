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
            InsightCounter::VoiceMinutes(minutes) => {
                doc! { format!("voice.{slot}"): *minutes as i64 }
            }
        };
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": ServerInsightDay::key(server, day) },
                doc! {
                    "$setOnInsert": { "server": server, "day": day },
                    "$inc": inc,
                },
            )
            .upsert(true)
            .await
            .map_err(|_| create_database_error!("update_one", COL))
            .map(|_| ())
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

        let mut inc = doc! { format!("messages.{of_day}"): counts.messages as i64 };
        for (channel, n) in &counts.channels {
            inc.insert(format!("channels.{channel}"), *n as i64);
        }

        // only when this hour is not in yet: the filter then misses an
        // existing row and the upsert's insert fails on its _id
        let result = self
            .col::<Document>(COL)
            .update_one(
                doc! { "_id": &key, "hours": { "$ne": of_day } },
                doc! {
                    "$setOnInsert": { "server": server, "day": day },
                    "$inc": inc,
                    "$addToSet": {
                        "hours": of_day,
                        "authors": { "$each": counts.authors.clone() },
                    },
                },
            )
            .upsert(true)
            .await;
        match result {
            Ok(_) => {}
            Err(error) if is_duplicate(&error) => return Ok(false),
            Err(_) => return Err(create_database_error!("update_one", COL)),
        }

        // the number of authors follows the ids (a second run sets the same)
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": &key, "authors": { "$exists": true } },
                vec![doc! { "$set": { "active": { "$size": "$authors" } } }],
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;
        Ok(true)
    }

    async fn close_server_insight_days(&self, day: i64) -> Result<u64> {
        self.col::<Document>(COL)
            .update_many(
                doc! { "day": { "$lt": day }, "authors": { "$exists": true } },
                vec![
                    doc! { "$set": { "active": { "$size": "$authors" } } },
                    doc! { "$unset": "authors" },
                ],
            )
            .await
            .map_err(|_| create_database_error!("update_many", COL))
            .map(|result| result.modified_count)
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
            .projection(doc! { "authors": 0_i32 })
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
        Ok(state.and_then(|state| match state.get("hour") {
            Some(bson::Bson::Int64(hour)) => Some(*hour),
            Some(bson::Bson::Int32(hour)) => Some(*hour as i64),
            _ => None,
        }))
    }

    async fn set_server_insight_cursor(&self, hour: i64) -> Result<()> {
        self.col::<Document>(STATE)
            .update_one(doc! { "_id": CURSOR }, doc! { "$set": { "hour": hour } })
            .upsert(true)
            .await
            .map_err(|_| create_database_error!("update_one", STATE))
            .map(|_| ())
    }
}
