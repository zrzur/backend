use crate::{AbstractSessions, MongoDb, Session, WebPushSubscription};
use bson::{to_bson, to_document, Document};
use futures::TryStreamExt;
use iso8601_timestamp::Timestamp;
#[cfg(any(test, feature = "test-utils"))]
use mongodb::options::UpdateOptions;
use revolt_result::Result;

const COL: &str = "sessions";

#[async_trait]
impl AbstractSessions for MongoDb {
    /// Find session by id
    async fn fetch_session(&self, id: &str) -> Result<Session> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(UnknownUser))
    }

    /// Find sessions by user id
    async fn fetch_sessions(&self, user_id: &str) -> Result<Vec<Session>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "user_id": user_id
            }
        )
    }

    /// Find sessions by user ids
    async fn fetch_sessions_with_subscription(&self, user_ids: &[String]) -> Result<Vec<Session>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "user_id": {
                    "$in": user_ids
                },
                "subscription": {
                    "$exists": true
                }
            }
        )
    }

    /// Find sessions last used at or before `before` whose grace, if any, is
    /// over at `now`
    async fn fetch_idle_sessions(
        &self,
        before: Timestamp,
        now: Timestamp,
        limit: i64,
    ) -> Result<Vec<Session>> {
        let before = to_bson(&before).map_err(|_| create_database_error!("to_bson", COL))?;
        let now = to_bson(&now).map_err(|_| create_database_error!("to_bson", COL))?;

        // timestamps are stored as fixed-width ISO 8601 strings, which sort
        // by time
        self.col::<Session>(COL)
            .find(doc! {
                "last_seen": { "$lte": before },
                // a missing date does not match: not migrated, not idle
                "idle_grace_until": { "$lte": now }
            })
            .limit(limit)
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("collect", COL))
    }

    /// Fetch a session from the database by token
    async fn fetch_session_by_token(&self, token: &str) -> Result<Session> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                // only the hash of a token is stored (see Session::token_hash)
                "token_hash": crate::models::hash_secret(token)
            }
        )?
        .ok_or_else(|| create_error!(InvalidSession))
    }

    /// Insert a new session; fails if the id or token is taken
    async fn insert_session(&self, session: &Session) -> Result<()> {
        // through `to_document`, as every session so far: a typed insert would
        // store timestamps in another form, and they are compared as strings
        let document =
            to_document(session).map_err(|_| create_database_error!("to_document", COL))?;
        self.col::<Document>(COL)
            .insert_one(document)
            .await
            .map_err(|_| create_database_error!("insert_one", COL))
            .map(|_| ())
    }

    /// Write a whole session, creating it if it is missing
    #[cfg(any(test, feature = "test-utils"))]
    async fn save_session(&self, session: &Session) -> Result<()> {
        self.col::<Session>(COL)
            .update_one(
                doc! {
                    "_id": &session.id
                },
                doc! {
                    "$set": to_document(session).map_err(|_| create_database_error!("to_document", COL))?,
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map_err(|_| create_database_error!("upsert_one", COL))
            .map(|_| ())
    }

    /// Set the name of a session; fails if the session is gone
    async fn update_session_name(&self, id: &str, name: &str) -> Result<()> {
        let result = self
            .col::<Session>(COL)
            .update_one(doc! { "_id": id }, doc! { "$set": { "name": name } })
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;

        if result.matched_count == 0 {
            return Err(create_error!(UnknownUser));
        }
        Ok(())
    }

    /// Set the push subscription of a session; fails if the session is gone
    async fn update_session_subscription(
        &self,
        id: &str,
        subscription: &WebPushSubscription,
    ) -> Result<()> {
        let subscription =
            to_bson(subscription).map_err(|_| create_database_error!("to_bson", COL))?;
        let result = self
            .col::<Session>(COL)
            .update_one(
                doc! { "_id": id },
                doc! { "$set": { "subscription": subscription } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;

        if result.matched_count == 0 {
            return Err(create_error!(InvalidSession));
        }
        Ok(())
    }

    /// Delete session
    async fn delete_session(&self, id: &str) -> Result<()> {
        self.col::<Session>(COL)
            .delete_one(doc! {
                "_id": id
            })
            .await
            .map_err(|_| create_database_error!("delete_one", COL))
            .map(|_| ())
    }

    /// Delete session
    async fn delete_all_sessions(&self, user_id: &str, ignore: Option<String>) -> Result<()> {
        let mut query = doc! {
            "user_id": user_id
        };

        if let Some(id) = ignore {
            query.insert(
                "_id",
                doc! {
                    "$ne": id
                },
            );
        }

        self.col::<Session>(COL)
            .delete_many(query)
            .await
            .map_err(|_| create_database_error!("delete_one", COL))
            .map(|_| ())
    }

    /// Remove push subscription for a session by session id
    async fn remove_push_subscription_by_session_id(&self, session_id: &str) -> Result<()> {
        self.col::<Session>(COL)
            .update_one(
                doc! {
                    "_id": session_id
                },
                doc! {
                    "$unset": {
                        "subscription": 1
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    async fn update_session_last_seen(&self, session_id: &str, when: Timestamp) -> Result<()> {
        self.col::<Session>(COL)
            .update_one(
                doc! {
                    "_id": session_id
                },
                doc! {
                    "$set": {
                        "last_seen": to_bson(&when).unwrap()
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Set `last_seen` unless the session is gone or idle
    async fn touch_session(
        &self,
        session_id: &str,
        when: Timestamp,
        idle_before: Option<Timestamp>,
    ) -> Result<bool> {
        let when = to_bson(&when).map_err(|_| create_database_error!("to_bson", COL))?;

        let mut filter = doc! { "_id": session_id };
        if let Some(before) = idle_before {
            let before = to_bson(&before).map_err(|_| create_database_error!("to_bson", COL))?;
            // the opposite of `fetch_idle_sessions`
            filter.insert(
                "$or",
                vec![
                    doc! { "last_seen": { "$gt": before } },
                    doc! { "idle_grace_until": { "$exists": false } },
                    doc! { "idle_grace_until": { "$gt": when.clone() } },
                ],
            );
        }

        self.col::<Session>(COL)
            .update_one(filter, doc! { "$set": { "last_seen": when } })
            .await
            .map(|result| result.matched_count > 0)
            .map_err(|_| create_database_error!("update_one", COL))
    }
}
