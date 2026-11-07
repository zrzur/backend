use super::AbstractChannels;
use crate::{
    util::ChunkedDatabaseGenerator, AbstractServers, Channel, FieldsChannel, IntoDocumentPath,
    MongoDb, PartialChannel,
};
use bson::{Bson, Document};
use futures::StreamExt;
use mongodb::options::{FindOptions, ReadConcern};
use revolt_permissions::OverrideField;
use revolt_result::Result;

static COL: &str = "channels";

#[async_trait]
impl AbstractChannels for MongoDb {
    /// Insert a new channel in the database
    async fn insert_channel(&self, channel: &Channel) -> Result<()> {
        query!(self, insert_one, COL, &channel).map(|_| ())
    }

    /// Fetch a channel from the database
    async fn fetch_channel(&self, channel_id: &str) -> Result<Channel> {
        query!(self, find_one_by_id, COL, channel_id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch all channels from the database
    async fn fetch_channels<'a>(&self, ids: &'a [String]) -> Result<Vec<Channel>> {
        Ok(self
            .col::<Channel>(COL)
            .find(doc! {
                "_id": {
                    "$in": ids
                }
            })
            .await
            .map_err(|_| create_database_error!("fetch", "channels"))?
            .filter_map(|s| async {
                if cfg!(debug_assertions) {
                    Some(s.unwrap())
                } else {
                    s.ok()
                }
            })
            .collect()
            .await)
    }

    /// Threads of a channel, newest first
    async fn fetch_threads(
        &self,
        parent: &str,
        archived: bool,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Channel>> {
        let mut filter = doc! {
            "parent": parent,
            "thread.archived": if archived { Bson::Boolean(true) } else { Bson::Document(doc! { "$ne": true }) },
        };
        if let Some(before) = before {
            filter.insert("_id", doc! { "$lt": before });
        }
        self.find_with_options(
            COL,
            filter,
            FindOptions::builder()
                .sort(doc! { "_id": -1_i32 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Active threads of these channels, newest first
    async fn fetch_active_threads(&self, parents: &[String], limit: i64) -> Result<Vec<Channel>> {
        if parents.is_empty() {
            return Ok(vec![]);
        }
        self.find_with_options(
            COL,
            doc! {
                "parent": { "$in": parents },
                "thread.archived": { "$ne": true },
            },
            FindOptions::builder()
                .sort(doc! { "_id": -1_i32 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    /// Fetch all direct messages for a user
    async fn find_direct_messages(&self, user_id: &str) -> Result<Vec<Channel>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "$or": [
                    {
                        "$or": [
                            {
                                "channel_type": "DirectMessage"
                            },
                            {
                                "channel_type": "Group"
                            }
                        ],
                        "recipients": user_id
                    },
                    {
                        "channel_type": "SavedMessages",
                        "user": user_id
                    }
                ]
            }
        )
    }

    // Fetch all group dms for a user
    async fn find_group_message_channels(
        &self,
        user_id: &str,
    ) -> Result<ChunkedDatabaseGenerator<Channel>> {
        let mut session = self
            .start_session()
            .await
            .map_err(|_| create_database_error!("start_session", COL))?;

        session
            .start_transaction()
            .read_concern(ReadConcern::snapshot())
            .await
            .map_err(|_| create_database_error!("start_transaction", COL))?;

        let cursor = self
            .col(COL)
            .find(doc! {
                "channel_type": "Group",
                "recipients": user_id
            })
            .session(&mut session)
            .batch_size(100)
            .await
            .map_err(|_| create_database_error!("find", COL))?;

        Ok(ChunkedDatabaseGenerator::new_mongo(session, cursor))
    }

    // Fetch saved messages channel
    async fn find_saved_messages_channel(&self, user_id: &str) -> Result<Channel> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "channel_type": "SavedMessages",
                "user": user_id
            }
        )?
        .ok_or_else(|| create_error!(InternalError))
    }

    // Fetch direct message channel (DM or Saved Messages)
    async fn find_direct_message_channel(&self, user_a: &str, user_b: &str) -> Result<Channel> {
        let doc = match (user_a, user_b) {
            self_user if self_user.0 == self_user.1 => {
                doc! {
                    "channel_type": "SavedMessages",
                    "user": self_user.0
                }
            }
            users => {
                doc! {
                    "channel_type": "DirectMessage",
                    "recipients": {
                        "$all": [ users.0, users.1 ]
                    }
                }
            }
        };
        query!(self, find_one, COL, doc)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Insert a user to a group
    async fn add_user_to_group(&self, channel: &str, user: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": channel
                },
                doc! {
                    "$push": {
                        "recipients": user
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "channel"))
    }

    /// Insert channel role permissions
    async fn set_channel_role_permission(
        &self,
        channel: &str,
        role: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! { "_id": channel },
                doc! {
                "$set": {
                    "role_permissions.".to_owned() + role: permissions
                }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "channel"))
    }

    // Update channel
    async fn update_channel(
        &self,
        id: &str,
        channel: &PartialChannel,
        remove: Vec<FieldsChannel>,
    ) -> Result<()> {
        query!(
            self,
            update_one_by_id,
            COL,
            id,
            channel,
            remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
            None
        )
        .map(|_| ())
    }

    // Remove a user from a group
    async fn remove_user_from_group(&self, channel: &str, user: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id": channel
                },
                doc! {
                    "$pull": {
                        "recipients": user
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", "channels"))
    }

    // Remove a user from all specified groups
    async fn remove_user_from_groups(&self, channel_ids: Vec<String>, user_id: &str) -> Result<()> {
        self.col::<Document>(COL)
            .update_many(
                doc! {
                    "_id": { "$in": channel_ids },
                },
                doc! {
                    "$pull": {
                        "recipients": user_id
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", COL))
    }

    // Delete a channel
    async fn delete_unattached_channels(&self, channel_ids: &[String]) -> Result<()> {
        self.col::<Document>(COL)
            .delete_many(doc! {
                "_id": { "$in": channel_ids }
            })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }

    async fn delete_channel(&self, channel: &Channel) -> Result<()> {
        let id = channel.id().to_string();
        let server_id = match channel {
            Channel::TextChannel { server, .. } => Some(server),
            _ => None,
        };

        // Delete invites and unreads.
        self.delete_associated_channel_objects(Bson::String(id.to_string()))
            .await?;

        // Delete messages.
        self.delete_bulk_messages(doc! {
            "channel": &id
        })
        .await?;

        // Remove from server object.
        if let Some(server) = server_id {
            let server = self.fetch_server(server).await?;
            let mut update = doc! {
                "$pull": {
                    "channels": &id
                }
            };

            // and from every category, so no category keeps a dead id
            // ($[] needs the array to exist)
            if server.categories.as_ref().is_some_and(|c| !c.is_empty()) {
                update
                    .get_document_mut("$pull")
                    .expect("$pull is set above")
                    .insert("categories.$[].channels", &id);
            }

            if let Some(sys) = &server.system_messages {
                let mut unset = doc! {};

                if let Some(cid) = &sys.user_joined {
                    if &id == cid {
                        unset.insert("system_messages.user_joined", 1_i32);
                    }
                }

                if let Some(cid) = &sys.user_left {
                    if &id == cid {
                        unset.insert("system_messages.user_left", 1_i32);
                    }
                }

                if let Some(cid) = &sys.user_kicked {
                    if &id == cid {
                        unset.insert("system_messages.user_kicked", 1_i32);
                    }
                }

                if let Some(cid) = &sys.user_banned {
                    if &id == cid {
                        unset.insert("system_messages.user_banned", 1_i32);
                    }
                }

                if !unset.is_empty() {
                    update.insert("$unset", unset);
                }
            }

            // a deleted rules channel leaves the server without one
            if server.rules_channel.as_deref() == Some(id.as_str()) {
                match update.get_document_mut("$unset") {
                    Ok(unset) => {
                        unset.insert("rules_channel", 1_i32);
                    }
                    Err(_) => {
                        update.insert("$unset", doc! { "rules_channel": 1_i32 });
                    }
                }
            }

            self.col::<Document>("servers")
                .update_one(
                    doc! {
                        "_id": server.id
                    },
                    update,
                )
                .await
                .map_err(|_| create_database_error!("update_one", "servers"))?;
        }

        // Delete associated attachments
        self.delete_many_attachments(doc! {
            "used_for.id": &id
        })
        .await?;

        // Delete the channel itself
        query!(self, delete_one_by_id, COL, channel.id()).map(|_| ())
    }

    async fn fetch_last_message(&self, channel_id: &str) -> Result<Option<String>> {
        self.col::<Document>("messages")
            .find_one(doc! {"channel": channel_id})
            .sort(doc! {"_id": -1})
            .projection(doc! {"_id": 1})
            .await
            .map(|doc| doc.map(|d| String::from(d.get_str("_id").expect("failed to get _id"))))
            .map_err(|_| create_database_error!("find_one", "messages"))
    }

    async fn update_last_messsage_id(
        &self,
        channel_id: &str,
        message_id: Option<&str>,
    ) -> Result<()> {
        if let Some(message_id) = message_id {
            self.col::<Document>(COL)
                .update_one(
                    doc! {"_id": channel_id},
                    doc! {"$set": {"last_message_id": message_id}},
                )
                .await
                .map(|_| ())
                .map_err(|_| create_database_error!("update_one", "channels"))
        } else {
            self.col::<Document>(COL)
                .update_one(
                    doc! {"_id": channel_id},
                    doc! {"$unset": {"last_message_id": ""}},
                )
                .await
                .map(|_| ())
                .map_err(|_| create_database_error!("update_one", "channels"))
        }
    }
}

impl MongoDb {
    pub async fn delete_associated_channel_objects(&self, id: Bson) -> Result<()> {
        // Delete all invites to these channels.
        self.col::<Document>("channel_invites")
            .delete_many(doc! {
                "channel": &id
            })
            .await
            .map_err(|_| create_database_error!("delete_many", "channel_invites"))?;

        // Delete unread message objects on channels.
        self.col::<Document>("channel_unreads")
            .delete_many(doc! {
                "_id.channel": &id
            })
            .await
            .map_err(|_| create_database_error!("delete_many", "channel_unreads"))
            .map(|_| ())?;

        // update many attachments with parent id

        // Delete all webhooks on this channel.
        crate::AbstractWebhooks::delete_webhooks_for_channel(self, id.as_str().unwrap_or_default()).await
    }
}
