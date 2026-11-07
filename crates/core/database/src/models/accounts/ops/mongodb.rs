use crate::{
    AbstractAccounts, Account, DataExport, ExportSource, MongoDb, DATA_EXPORT_CHUNK_BYTES,
    DATA_EXPORT_MAX_MESSAGES,
};
use bson::Document;
use futures::TryStreamExt;
use bson::{to_bson, to_document};
use iso8601_timestamp::{Duration, Timestamp};
use mongodb::options::{
    Collation, CollationStrength, FindOneAndUpdateOptions, FindOneOptions, ReturnDocument,
    UpdateOptions,
};
use revolt_result::Result;

const COL: &str = "accounts";
const EXPORT_COL: &str = "data_exports";
const EXPORT_CHUNK_COL: &str = "data_export_chunks";

#[async_trait]
impl AbstractAccounts for MongoDb {
    /// Find account by id
    async fn fetch_account(&self, id: &str) -> Result<Account> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(UnknownUser))
    }

    /// Find account by normalised email
    async fn fetch_account_by_normalised_email(
        &self,
        normalised_email: &str,
    ) -> Result<Option<Account>> {
        query!(
            self,
            find_one_with_options,
            COL,
            doc! {
                "email_normalised": normalised_email
            },
            FindOneOptions::builder()
                .collation(
                    Collation::builder()
                        .locale("en")
                        .strength(CollationStrength::Secondary)
                        .build(),
                )
                .build()
        )
    }

    /// Find account with active pending email verification
    async fn fetch_account_with_email_verification(&self, token: &str) -> Result<Account> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "verification.token": token,
                "verification.expiry": {
                    "$gte": to_bson(&Timestamp::now_utc()).unwrap()
                }
            }
        )?
        .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find account with active password reset
    async fn fetch_account_with_password_reset(&self, token: &str) -> Result<Account> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "password_reset.token": token,
                "password_reset.expiry": {
                    "$gte": to_bson(&Timestamp::now_utc()).unwrap()
                }
            }
        )?
        .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find account with active deletion token
    async fn fetch_account_with_deletion_token(&self, token: &str) -> Result<Account> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "deletion.token": token,
                "deletion.expiry": {
                    "$gte": to_bson(&Timestamp::now_utc()).unwrap()
                }
            }
        )?
        .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find accounts which are due to be deleted
    async fn fetch_accounts_due_for_deletion(&self) -> Result<Vec<Account>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "deletion.status": "Scheduled",
                "deletion.after": {
                    "$lte": to_bson(&Timestamp::now_utc()).unwrap()
                }
            }
        )
    }

    // Save account
    async fn save_account(&self, account: &Account) -> Result<()> {
        self.col::<Account>(COL)
            .update_one(
                doc! {
                    "_id": &account.id
                },
                doc! {
                    "$set": to_document(account).map_err(|_| create_database_error!("to_document", COL))?
                },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map_err(|_| create_database_error!("find_one", COL))
            .map(|_| ())
    }

    async fn user_has_content(&self, user_id: &str) -> Result<bool> {
        for (col, filter) in [
            ("servers", doc! { "owner": user_id }),
            ("bots", doc! { "owner": user_id }),
            ("messages", doc! { "author": user_id }),
        ] {
            let hit = self
                .col::<Document>(col)
                .find_one(filter)
                .projection(doc! { "_id": 1 })
                .await
                .map_err(|_| create_database_error!("find_one", col))?;
            if hit.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn remove_account(&self, id: &str) -> Result<()> {
        self.col::<Account>(COL)
            .delete_one(doc! { "_id": id })
            .await
            .map_err(|_| create_database_error!("delete_one", COL))
            .map(|_| ())
    }

    async fn scrub_account(&self, id: &str) -> Result<()> {
        // A plain save only $sets fields that are present, so what is empty
        // (and skipped when saving) has to be unset here
        self.col::<Account>(COL)
            .update_one(
                doc! { "_id": id },
                doc! {
                    "$set": { "password": "" },
                    "$unset": {
                        "age": "",
                        "mfa.totp_token": "",
                        "mfa.recovery_codes": "",
                        "password_reset": "",
                        "lockout": ""
                    }
                },
            )
            .await
            .map_err(|_| create_database_error!("update_one", COL))
            .map(|_| ())
    }

    async fn fetch_data_export(&self, user_id: &str) -> Result<Option<DataExport>> {
        self.col::<DataExport>(EXPORT_COL)
            .find_one(doc! { "_id": user_id })
            .await
            .map_err(|_| create_database_error!("find_one", EXPORT_COL))
    }

    async fn scrub_user_fields(&self, user_id: &str) -> Result<()> {
        self.col::<Document>("users")
            .update_one(
                doc! { "_id": user_id },
                doc! { "$unset": {
                    "locale": "", "privacy": "", "profile": "",
                    "plus": "", "badges": "",
                } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", "users"))
            .map(|_| ())
    }

    async fn fetch_pending_data_exports(&self) -> Result<Vec<DataExport>> {
        self.col::<DataExport>(EXPORT_COL)
            .find(doc! { "status": "Pending" })
            .await
            .map_err(|_| create_database_error!("find", EXPORT_COL))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("collect", EXPORT_COL))
    }

    async fn fetch_expired_data_exports(&self) -> Result<Vec<DataExport>> {
        self.col::<DataExport>(EXPORT_COL)
            .find(doc! {
                "status": "Ready",
                "expires_at": { "$lte": to_bson(&Timestamp::now_utc()).unwrap() }
            })
            .await
            .map_err(|_| create_database_error!("find", EXPORT_COL))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("collect", EXPORT_COL))
    }

    async fn save_data_export(&self, export: &DataExport) -> Result<()> {
        // through to_document: dates are then stored as the strings that
        // fetch_expired_data_exports compares with (a typed replace_one
        // stores them in another format, and nothing would ever expire)
        let document =
            to_document(export).map_err(|_| create_database_error!("to_document", EXPORT_COL))?;
        self.col::<Document>(EXPORT_COL)
            .replace_one(doc! { "_id": &export.id }, document)
            .upsert(true)
            .await
            .map_err(|_| create_database_error!("replace_one", EXPORT_COL))
            .map(|_| ())
    }

    async fn save_data_export_bytes(&self, user_id: &str, bytes: &[u8]) -> Result<i32> {
        let col = self.col::<Document>(EXPORT_CHUNK_COL);
        col.delete_many(doc! { "user_id": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", EXPORT_CHUNK_COL))?;
        let mut count = 0;
        for (n, piece) in bytes.chunks(DATA_EXPORT_CHUNK_BYTES).enumerate() {
            col.insert_one(doc! {
                "_id": format!("{user_id}:{n}"),
                "user_id": user_id,
                "n": n as i32,
                "data": bson::Binary { subtype: bson::spec::BinarySubtype::Generic, bytes: piece.to_vec() },
            })
            .await
            .map_err(|_| create_database_error!("insert_one", EXPORT_CHUNK_COL))?;
            count += 1;
        }
        Ok(count)
    }

    async fn fetch_data_export_bytes(&self, user_id: &str) -> Result<Vec<u8>> {
        let mut docs: Vec<Document> = self
            .col::<Document>(EXPORT_CHUNK_COL)
            .find(doc! { "user_id": user_id })
            .await
            .map_err(|_| create_database_error!("find", EXPORT_CHUNK_COL))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("collect", EXPORT_CHUNK_COL))?;
        docs.sort_by_key(|d| d.get_i32("n").unwrap_or(0));
        let mut out = Vec::new();
        for d in docs {
            if let Ok(b) = d.get_binary_generic("data") {
                out.extend_from_slice(b);
            }
        }
        Ok(out)
    }

    async fn remove_data_exports_for_user(&self, user_id: &str) -> Result<()> {
        self.col::<Document>(EXPORT_CHUNK_COL)
            .delete_many(doc! { "user_id": user_id })
            .await
            .map_err(|_| create_database_error!("delete_many", EXPORT_CHUNK_COL))?;
        self.col::<DataExport>(EXPORT_COL)
            .delete_one(doc! { "_id": user_id })
            .await
            .map_err(|_| create_database_error!("delete_one", EXPORT_COL))
            .map(|_| ())
    }

    async fn export_documents(
        &self,
        user_id: &str,
        source: ExportSource,
    ) -> Result<Vec<serde_json::Value>> {
        let (col, filter, sort, limit) = match source {
            ExportSource::Messages => (
                "messages",
                doc! { "author": user_id },
                Some(doc! { "_id": -1 }),
                DATA_EXPORT_MAX_MESSAGES,
            ),
            ExportSource::Files => (
                "attachments",
                doc! { "uploader_id": user_id, "deleted": { "$ne": true } },
                None,
                0,
            ),
            ExportSource::Memberships => ("server_members", doc! { "_id.user": user_id }, None, 0),
            ExportSource::Reports => ("safety_reports", doc! { "author_id": user_id }, None, 0),
            ExportSource::Settings => ("user_settings", doc! { "_id": user_id }, None, 0),
            ExportSource::SupportTickets => ("support_tickets", doc! { "user_id": user_id }, None, 0),
        };
        let collection = self.col::<Document>(col);
        let mut find = collection.find(filter).limit(limit);
        if let Some(sort) = sort {
            find = find.sort(sort);
        }
        let docs: Vec<Document> = find
            .await
            .map_err(|_| create_database_error!("find", col))?
            .try_collect()
            .await
            .map_err(|_| create_database_error!("collect", col))?;
        Ok(docs
            .into_iter()
            .filter_map(|d| serde_json::to_value(d).ok())
            .collect())
    }

    async fn bump_lockout_count(&self, id: &str) -> Result<Account> {
        // Mirrors `Lockout::after_failure`, in one atomic step: a running lock
        // is left as it is, attempts older than an hour are forgotten.
        let now = Timestamp::now_utc();
        let now_bson = to_bson(&now).expect("bson time error");
        let forget_before = to_bson(&(now + Duration::hours(-1))).expect("bson time error");

        self.col::<Account>(COL)
            .find_one_and_update(
                doc! {
                    "_id": id
                },
                vec![doc! {
                    "$set": {
                        "lockout": {
                            "$let": {
                                "vars": {
                                    "running": { "$gt": ["$lockout.expiry", now_bson.clone()] },
                                    "last": { "$ifNull": ["$lockout.last_attempt", "$lockout.expiry"] }
                                },
                                "in": { "$let": {
                                    "vars": {
                                        "stale": { "$and": [
                                            { "$not": [{ "$in": [{ "$type": "$$last" }, ["missing", "null"]] }] },
                                            { "$lt": ["$$last", forget_before] }
                                        ] }
                                    },
                                    "in": { "$let": {
                                        "vars": {
                                            "n": { "$add": [
                                                { "$cond": ["$$stale", 0, { "$ifNull": ["$lockout.attempts", 0] }] },
                                                1
                                            ] }
                                        },
                                        "in": { "$cond": {
                                            "if": "$$running",
                                            "then": "$lockout",
                                            "else": {
                                                "attempts": "$$n",
                                                "last_attempt": now_bson.clone(),
                                                "expiry": { "$switch": {
                                                    "branches": [
                                                        {
                                                            "case": { "$gte": ["$$n", 5] },
                                                            "then": to_bson(&(now + Duration::hours(1))).expect("bson time error")
                                                        },
                                                        {
                                                            "case": { "$eq": ["$$n", 4] },
                                                            "then": to_bson(&(now + Duration::minutes(5))).expect("bson time error")
                                                        },
                                                        {
                                                            "case": { "$eq": ["$$n", 3] },
                                                            "then": to_bson(&(now + Duration::minutes(1))).expect("bson time error")
                                                        }
                                                    ],
                                                    "default": null
                                                } }
                                            }
                                        } }
                                    } }
                                } }
                            }
                        }
                    }
                }],
            )
            .with_options(
                FindOneAndUpdateOptions::builder()
                    .return_document(ReturnDocument::After)
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find_one_and_update", COL))?
            .ok_or_else(|| create_error!(UnknownUser))
    }
}
