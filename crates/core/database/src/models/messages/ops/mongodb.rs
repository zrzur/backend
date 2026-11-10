use bson::{to_bson, Document};
use futures::try_join;
use futures::StreamExt;
use mongodb::options::FindOptions;
use revolt_models::v0::{MessageHas, MessageSort};
use revolt_result::Result;
use std::collections::{HashMap, HashSet};
use std::time::SystemTime;
use ulid::Ulid;

use crate::{
    AppendMessage, DocumentId, FieldsMessage, IntoDocumentPath, Message, MessageQuery,
    MessageTimePeriod, MongoDb, PartialMessage,
};

use super::AbstractMessages;

static COL: &str = "messages";

/// Longest a search may run in MongoDB
const SEARCH_MAX_TIME: std::time::Duration = std::time::Duration::from_secs(5);

#[async_trait]
impl AbstractMessages for MongoDb {
    /// Insert a new message into the database
    async fn insert_message(&self, message: &Message) -> Result<()> {
        query!(self, insert_one, COL, &message).map(|_| ())
    }

    /// Fetch a message by its id
    async fn fetch_message(&self, id: &str) -> Result<Message> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch multiple messages by given query
    async fn fetch_messages(&self, query: MessageQuery) -> Result<Vec<Message>> {
        let mut filter = doc! {};

        // Searches may scan many messages: they stop after this long
        let is_search = query.filter.query.is_some()
            || query.filter.authors.is_some()
            || query.filter.mentions.is_some()
            || query.filter.has.is_some()
            || query.filter.channels.is_some();
        let limit_time = |mut options: FindOptions| {
            if is_search {
                options.max_time = Some(SEARCH_MAX_TIME);
            }
            options
        };

        // 1. Apply message filters
        if let Some(channel) = query.filter.channel {
            filter.insert("channel", channel);
        } else if let Some(channels) = query.filter.channels {
            filter.insert("channel", doc! { "$in": channels });
        }

        let mut authors: Vec<String> = query.filter.author.into_iter().collect();
        authors.extend(query.filter.authors.into_iter().flatten());
        match authors.len() {
            0 => {}
            1 => {
                filter.insert("author", authors.remove(0));
            }
            _ => {
                filter.insert("author", doc! { "$in": authors });
            }
        }

        if let Some(user) = query.filter.mentions {
            filter.insert("mentions", user);
        }

        // every kind asked for must be there
        let has: Vec<Document> = query
            .filter
            .has
            .unwrap_or_default()
            .into_iter()
            .map(|kind| match kind {
                MessageHas::Link => doc! {
                    "content": { "$regex": "https?://", "$options": "i" }
                },
                MessageHas::File => doc! { "attachments.0": { "$exists": true } },
                MessageHas::Image => doc! { "attachments.metadata.type": "Image" },
                MessageHas::Video => doc! { "attachments.metadata.type": "Video" },
                MessageHas::Sound => doc! { "attachments.metadata.type": "Audio" },
                MessageHas::Embed => doc! { "embeds.0": { "$exists": true } },
            })
            .collect();
        if !has.is_empty() {
            filter.insert("$and", has);
        }

        let is_search_query = if let Some(query) = query.filter.query {
            filter.insert(
                "$text",
                doc! {
                    "$search": query
                },
            );

            true
        } else {
            false
        };

        if let Some(pinned) = query.filter.pinned {
            filter.insert("pinned", pinned);
        };

        // 2. Find query limit
        let limit = query.limit.unwrap_or(50);

        // 3. Apply message time period
        match query.time_period {
            MessageTimePeriod::Relative { nearby } => {
                // 3.1. Prepare filters
                let mut older_message_filter = filter.clone();
                let mut newer_message_filter = filter;

                older_message_filter.insert(
                    "_id",
                    doc! {
                        "$lt": &nearby
                    },
                );

                newer_message_filter.insert(
                    "_id",
                    doc! {
                        "$gte": &nearby
                    },
                );

                // 3.2. Execute in both directions
                let (a, b) = try_join!(
                    self.find_with_options::<_, Message>(
                        COL,
                        newer_message_filter,
                        limit_time(FindOptions::builder()
                            .limit(limit / 2 + 1)
                            .sort(doc! {
                                "_id": 1_i32
                            })
                            .build()),
                    ),
                    self.find_with_options::<_, Message>(
                        COL,
                        older_message_filter,
                        limit_time(FindOptions::builder()
                            .limit(limit / 2 + 1)
                            .sort(doc! {
                                "_id": -1_i32
                            })
                            .build()),
                    )
                )
                .map_err(|_| create_database_error!("find", COL))?;

                Ok([a, b].concat())
            }
            MessageTimePeriod::Absolute {
                before,
                after,
                sort,
            } => {
                // 3.1. Apply message ID filter
                if let Some(doc) = match (before, after) {
                    (Some(before), Some(after)) => Some(doc! {
                        "$lt": before,
                        "$gt": after
                    }),
                    (Some(before), _) => Some(doc! {
                        "$lt": before
                    }),
                    (_, Some(after)) => Some(doc! {
                        "$gt": after
                    }),
                    _ => None,
                } {
                    filter.insert("_id", doc);
                }

                // 3.2. Execute with given message sort
                self.find_with_options(
                    COL,
                    filter,
                    limit_time(FindOptions::builder()
                        .limit(limit)
                        .sort(match sort.unwrap_or(MessageSort::Latest) {
                            // Sort by relevance, fallback to latest
                            MessageSort::Relevance => {
                                if is_search_query {
                                    doc! {
                                        "score": {
                                            "$meta": "textScore"
                                        }
                                    }
                                } else {
                                    doc! {
                                        "_id": -1_i32
                                    }
                                }
                            }
                            // Sort by latest first
                            MessageSort::Latest => doc! {
                                "_id": -1_i32
                            },
                            // Sort by oldest first
                            MessageSort::Oldest => doc! {
                                "_id": 1_i32
                            },
                        })
                        .build()),
                )
                .await
                .map_err(|_| create_database_error!("find", COL))
            }
        }
    }

    /// Fetch messages that mention a user
    async fn fetch_mentions(
        &self,
        user_id: &str,
        role_ids: &[String],
        channel_ids: &[String],
        since: &str,
        limit: i64,
    ) -> Result<Vec<Message>> {
        // flags bits 2 and 3: @everyone and @here
        let mut any = vec![
            doc! { "mentions": user_id },
            doc! { "flags": { "$bitsAnySet": (1_i64 << 2) | (1_i64 << 3) } },
        ];
        if !role_ids.is_empty() {
            any.push(doc! { "role_mentions": { "$in": role_ids } });
        }

        self.find_with_options(
            COL,
            doc! {
                "channel": { "$in": channel_ids },
                "_id": { "$gte": since },
                "author": { "$ne": user_id },
                "$or": any,
            },
            FindOptions::builder()
                .limit(limit)
                .sort(doc! { "_id": -1_i32 })
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Fetch multiple messages by given IDs
    async fn fetch_messages_by_id(&self, ids: &[String]) -> Result<Vec<Message>> {
        self.find_with_options(
            COL,
            doc! {
                "_id": {
                    "$in": ids
                }
            },
            None,
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Update a given message with new information
    async fn update_message(
        &self,
        id: &str,
        message: &PartialMessage,
        remove: Vec<FieldsMessage>,
    ) -> Result<()> {
        query!(
            self,
            update_one_by_id,
            COL,
            id,
            message,
            remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
            None
        )
        .map(|_| ())
    }

    /// Append information to a given message
    async fn append_message(&self, id: &str, append: &AppendMessage) -> Result<()> {
        let mut query = doc! {};

        if let Some(embeds) = &append.embeds {
            if !embeds.is_empty() {
                query.insert(
                    "$push",
                    doc! {
                        "embeds": {
                            "$each": to_bson(embeds)
                                .map_err(|_| create_database_error!("to_bson", "embeds"))?
                        }
                    },
                );
            }
        }

        if query.is_empty() {
            return Ok(());
        }

        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": id
                },
                query,
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Add a new reaction to a message
    async fn add_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": id
                },
                doc! {
                    "$addToSet": {
                        format!("reactions.{emoji}"): user
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Remove a reaction from a message
    async fn remove_reaction(&self, id: &str, emoji: &str, user: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": id
                },
                doc! {
                    "$pull": {
                        format!("reactions.{emoji}"): user
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Remove reaction from a message
    async fn clear_reaction(&self, id: &str, emoji: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": id
                },
                doc! {
                    "$unset": {
                        format!("reactions.{emoji}"): 1
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Add a vote to an open poll, in one write
    async fn add_poll_vote(
        &self,
        id: &str,
        user: &str,
        answer: u32,
        single: bool,
        answer_count: usize,
        now: iso8601_timestamp::Timestamp,
    ) -> Result<Option<Message>> {
        let mut add = Document::new();
        let mut pull = Document::new();
        for index in 0..answer_count {
            let path = format!("poll.answers.{index}.voters");
            if index as u32 + 1 == answer {
                add.insert(path, user);
            } else if single {
                pull.insert(path, user);
            }
        }
        let mut update = doc! { "$addToSet": add, "$inc": { "poll.version": 1_i64 } };
        if !pull.is_empty() {
            update.insert("$pull", pull);
        }
        self.update_open_poll(id, now, update).await
    }

    /// Take a vote off an open poll
    async fn remove_poll_vote(
        &self,
        id: &str,
        user: &str,
        answer: u32,
        now: iso8601_timestamp::Timestamp,
    ) -> Result<Option<Message>> {
        let path = format!("poll.answers.{}.voters", answer.saturating_sub(1));
        self.update_open_poll(
            id,
            now,
            doc! { "$pull": { path: user }, "$inc": { "poll.version": 1_i64 } },
        )
        .await
    }

    /// Close a poll
    async fn end_poll(&self, id: &str, now: iso8601_timestamp::Timestamp) -> Result<bool> {
        // stored the way the driver writes every Timestamp (milliseconds)
        let (now, _) = timestamp_forms(now)?;
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": id, "poll.ended": false },
                doc! {
                    "$set": { "poll.ended": true, "poll.ended_at": now },
                    "$inc": { "poll.version": 1_i64 },
                },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// The poll's reply is posted
    async fn mark_poll_replied(&self, id: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "poll.replied": true } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Polls crond still has to finish (index `polls_to_close`)
    async fn fetch_polls_to_close(
        &self,
        before: iso8601_timestamp::Timestamp,
        limit: i64,
    ) -> Result<Vec<Message>> {
        let (as_millis, as_text) = timestamp_forms(before)?;
        self.find_with_options(
            COL,
            doc! {
                "poll.replied": false,
                "$or": [
                    { "poll.ended": true },
                    { "poll.expires_at": { "$lte": as_millis } },
                    { "poll.expires_at": { "$lte": as_text } },
                ],
            },
            FindOptions::builder()
                .sort(doc! { "poll.expires_at": 1_i32 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Polls the user voted on
    async fn fetch_messages_voted_by(&self, user: &str, limit: i64) -> Result<Vec<Message>> {
        self.find_with_options(
            COL,
            doc! { "poll.answers.voters": user },
            FindOptions::builder()
                .sort(doc! { "_id": -1_i32 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Delete a message from the database by its id
    async fn delete_message(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, id).map(|_| ())
    }

    /// Delete messages from a channel by their ids and corresponding channel id
    async fn delete_messages(&self, channel: &str, ids: &[String]) -> Result<()> {
        self.col::<Document>(COL)
            .delete_many(doc! {
                "channel": channel,
                "_id": {
                    "$in": ids
                }
            })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }

    /// Delete all messages from a specific author in a server from a certain ULID onwards
    async fn delete_messages_by_author_since(
        &self,
        channels: &[String],
        author: &str,
        since: SystemTime,
    ) -> Result<HashMap<String, Vec<String>>> {
        let threshold_ulid = Ulid::from_datetime(since).to_string();

        let filter = doc! {
            "author": author,
            "channel": { "$in": channels },
            "_id": { "$gte": &threshold_ulid }
        };

        let pipeline = vec![
            doc! { "$match": filter.clone() },
            doc! {
                "$project": {
                    "channel": 1_i32,
                    "message_id": "$_id",
                    "attachment_ids": {
                        "$map": {
                            "input": { "$ifNull": ["$attachments", Vec::<bson::Bson>::new()] },
                            "as": "a",
                            "in": "$$a._id"
                        }
                    }
                }
            },
            doc! {
                "$group": {
                    "_id": "$channel",
                    "message_ids": { "$push": "$message_id" },
                    "attachment_ids_nested": { "$push": "$attachment_ids" }
                }
            },
            doc! {
                "$project": {
                    "message_ids": 1_i32,
                    "attachment_ids": {
                        "$reduce": {
                            "input": "$attachment_ids_nested",
                            "initialValue": Vec::<bson::Bson>::new(),
                            "in": { "$setUnion": ["$$value", "$$this"] }
                        }
                    }
                }
            },
        ];

        #[derive(serde::Deserialize)]
        struct AggregatedChannel {
            #[serde(rename = "_id")]
            channel: String,
            message_ids: Vec<String>,
            #[serde(default)]
            attachment_ids: Vec<String>,
        }

        let mut cursor = self
            .col::<Document>(COL)
            .aggregate(pipeline)
            .await
            .map_err(|_| create_database_error!("aggregate", COL))?
            .with_type::<AggregatedChannel>();

        let mut deleted_messages: HashMap<String, Vec<String>> = HashMap::new();
        let mut attachment_ids: HashSet<String> = HashSet::new();

        while let Some(result) = cursor.next().await {
            if let Ok(item) = result {
                for id in item.attachment_ids {
                    attachment_ids.insert(id);
                }
                deleted_messages.insert(item.channel, item.message_ids);
            }
        }

        // Mark attachments as deleted before deleting messages
        if !attachment_ids.is_empty() {
            self.col::<Document>("attachments")
                .update_many(
                    doc! {
                        "_id": {
                            "$in": attachment_ids.into_iter().collect::<Vec<String>>()
                        }
                    },
                    doc! {
                        "$set": {
                            "deleted": true
                        }
                    },
                )
                .await
                .map_err(|_| create_database_error!("update_many", "attachments"))?;
        }

        self.col::<Document>(COL)
            .delete_many(filter)
            .await
            .map_err(|_| create_database_error!("delete_many", COL))?;

        Ok(deleted_messages)
    }

    async fn delete_messages_by_user(&self, user_id: &str) -> Result<()> {
        // files are removed before this through their messages
        // (Message::remove_files); the legacy `message_id` path stays for
        // records from before attachments were kept on the message
        self.delete_bulk_messages(doc! {
            "author": user_id,
        }).await
    }

    async fn fetch_messages_with_files(
        &self,
        author: &str,
        channels: Option<&[String]>,
        since: Option<&str>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let mut filter = doc! {
            "author": author,
            "$or": [
                { "attachments.0": { "$exists": true } },
                { "embeds.0": { "$exists": true } },
            ]
        };
        if let Some(channels) = channels {
            filter.insert("channel", doc! { "$in": channels });
        }
        let mut id = Document::new();
        if let Some(since) = since {
            id.insert("$gte", since);
        }
        if let Some(after) = after {
            id.insert("$gt", after);
        }
        if !id.is_empty() {
            filter.insert("_id", id);
        }

        self.find_with_options(
            COL,
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": 1_i32 })
                .limit(limit as i64)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }
}

impl IntoDocumentPath for FieldsMessage {
    fn as_path(&self) -> Option<&'static str> {
        Some(match self {
            FieldsMessage::Pinned => "pinned",
        })
    }
}

impl MongoDb {
    pub async fn delete_bulk_messages(&self, projection: Document) -> Result<()> {
        let mut for_attachments = projection.clone();
        for_attachments.insert(
            "attachments",
            doc! {
                "$exists": 1_i32
            },
        );

        // Check if there are any attachments we need to delete.
        let message_ids_with_attachments = self
            .find_with_options::<_, DocumentId>(
                COL,
                for_attachments,
                FindOptions::builder()
                    .projection(doc! { "_id": 1_i32 })
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find_many", "attachments"))?
            .into_iter()
            .map(|x| x.id)
            .collect::<Vec<String>>();

        // If we found any, mark them as deleted.
        if !message_ids_with_attachments.is_empty() {
            self.col::<Document>("attachments")
                .update_many(
                    doc! {
                        "message_id": {
                            "$in": message_ids_with_attachments
                        }
                    },
                    doc! {
                        "$set": {
                            "deleted": true
                        }
                    },
                )
                .await
                .map_err(|_| create_database_error!("update_many", "attachments"))?;
        }

        // And then delete said messages.
        self.col::<Document>(COL)
            .delete_many(projection)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }
}

/// A Timestamp as the driver stores it in a document (milliseconds since
/// 1970, as an int64: its raw serializer is not "human readable") and as
/// `to_bson` writes it (text). MongoDB compares only values of one type, so
/// poll queries give both.
fn timestamp_forms(at: iso8601_timestamp::Timestamp) -> Result<(i64, bson::Bson)> {
    let millis = at
        .duration_since(iso8601_timestamp::Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64;
    let text = to_bson(&at).map_err(|_| create_database_error!("to_bson", COL))?;
    Ok((millis, text))
}

impl MongoDb {
    /// Apply `update` to a message's poll only while it is open (not ended,
    /// `expires_at` after `now`); the message after the change, or None
    async fn update_open_poll(
        &self,
        id: &str,
        now: iso8601_timestamp::Timestamp,
        update: Document,
    ) -> Result<Option<Message>> {
        let (as_millis, as_text) = timestamp_forms(now)?;
        self.col::<Message>(COL)
            .find_one_and_update(
                doc! {
                    "_id": id,
                    "poll.ended": false,
                    "$or": [
                        { "poll.expires_at": { "$gt": as_millis } },
                        { "poll.expires_at": { "$gt": as_text } },
                    ],
                },
                update,
            )
            .with_options(
                mongodb::options::FindOneAndUpdateOptions::builder()
                    .return_document(mongodb::options::ReturnDocument::After)
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find_one_and_update", COL))
    }
}
