use futures::StreamExt;
use iso8601_timestamp::Timestamp;
use mongodb::options::{ReplaceOptions, UpdateOptions};
use revolt_models::v0::SuperchargeSource;
use revolt_result::Result;
use ulid::Ulid;

use mongodb::options::FindOptions;
use revolt_models::v0::{OrderStatus, SubscriptionStatus};

use crate::{
    epoch_ms, BillingAuditEntry, BillingCustomer, BillingOrder, BillingWebhookEvent,
    DiscoverPromotion, DiscoverWaitlistEntry, Fulfilment, MongoDb, PlusSubscription,
    ReconciliationIssue, Supercharge, WebhookEventStatus,
};

use super::{
    AbstractBilling, OrderChanges, OrderSearch, PromotionPurchase, PromotionStat, ProviderRef,
};

static PLUS: &str = "plus_subscriptions";
static SUPERCHARGES: &str = "supercharges";
static PROMOTIONS: &str = "discover_promotions";
static SERVERS: &str = "servers";
static ORDERS: &str = "billing_orders";
static WAITLIST: &str = "discover_waitlist";
static WEBHOOK_EVENTS: &str = "billing_webhook_events";
static CUSTOMERS: &str = "billing_customers";
static AUDIT: &str = "billing_audit";
static ISSUES: &str = "billing_reconciliation";

fn to_bson<T: serde::Serialize>(value: &T) -> bson::Bson {
    bson::to_bson(value).expect("failed to serialize")
}

/// Whether a write failed because the id is taken
fn is_duplicate(error: &mongodb::error::Error) -> bool {
    error.to_string().contains("E11000")
}

#[async_trait]
impl AbstractBilling for MongoDb {
    async fn fetch_plus_subscription(&self, user_id: &str) -> Result<Option<PlusSubscription>> {
        query!(
            self,
            find_one,
            PLUS,
            doc! { "user_id": user_id, "ended": false }
        )
    }

    async fn insert_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        self.col::<PlusSubscription>(PLUS)
            .insert_one(subscription)
            .await
            .map(|_| ())
            .map_err(|error| {
                if is_duplicate(&error) {
                    create_error!(InvalidOperation)
                } else {
                    create_database_error!("insert_one", PLUS)
                }
            })
    }

    async fn save_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        self.col::<PlusSubscription>(PLUS)
            .replace_one(doc! { "_id": &subscription.id }, subscription)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", PLUS))
    }

    async fn extend_plus_subscription(&self, id: &str, add_ms: i64) -> Result<()> {
        self.col::<PlusSubscription>(PLUS)
            .update_one(
                doc! { "_id": id, "ended": false },
                doc! { "$inc": { "current_period_end": add_ms } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", PLUS))
    }

    async fn end_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        let mut history = subscription.clone();
        history.id = Ulid::new().to_string();
        history.ended = true;
        self.col::<PlusSubscription>(PLUS)
            .insert_one(&history)
            .await
            .map_err(|_| create_database_error!("insert_one", PLUS))?;
        query!(self, delete_one_by_id, PLUS, &subscription.id).map(|_| ())
    }

    async fn fetch_lapsed_plus_subscriptions(
        &self,
        now: Timestamp,
    ) -> Result<Vec<PlusSubscription>> {
        query!(
            self,
            find,
            PLUS,
            doc! { "ended": false, "current_period_end": { "$lte": epoch_ms(now) } }
        )
    }

    async fn fetch_supercharge(&self, id: &str) -> Result<Supercharge> {
        query!(self, find_one_by_id, SUPERCHARGES, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_supercharges_by_user(&self, user_id: &str) -> Result<Vec<Supercharge>> {
        query!(self, find, SUPERCHARGES, doc! { "user_id": user_id })
    }

    async fn fetch_supercharges_by_server(&self, server_id: &str) -> Result<Vec<Supercharge>> {
        query!(self, find, SUPERCHARGES, doc! { "server_id": server_id })
    }

    async fn fetch_lapsed_supercharges(&self, now: Timestamp) -> Result<Vec<Supercharge>> {
        query!(
            self,
            find,
            SUPERCHARGES,
            doc! {
                "source": to_bson(&SuperchargeSource::Purchase),
                "current_period_end": { "$lte": epoch_ms(now) },
            }
        )
    }

    async fn save_supercharge(&self, supercharge: &Supercharge) -> Result<()> {
        self.col::<Supercharge>(SUPERCHARGES)
            .replace_one(doc! { "_id": &supercharge.id }, supercharge)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", SUPERCHARGES))
    }

    async fn ensure_supercharge(&self, supercharge: &Supercharge) -> Result<()> {
        let document = bson::to_document(supercharge)
            .map_err(|_| create_database_error!("to_document", SUPERCHARGES))?;
        self.col::<bson::Document>(SUPERCHARGES)
            .update_one(
                doc! { "_id": &supercharge.id },
                doc! { "$setOnInsert": document },
            )
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .or_else(|error| {
                // a parallel call inserted it first: same result
                if is_duplicate(&error) {
                    Ok(())
                } else {
                    Err(create_database_error!("update_one", SUPERCHARGES))
                }
            })
    }

    async fn move_supercharge(
        &self,
        id: &str,
        user_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<bool> {
        let mut filter = doc! { "_id": id, "user_id": user_id };
        match from {
            Some(server_id) => filter.insert("server_id", server_id),
            None => filter.insert("server_id", doc! { "$exists": false }),
        };
        let update = match to {
            Some(server_id) => doc! { "$set": { "server_id": server_id } },
            None => doc! { "$unset": { "server_id": 1 } },
        };
        self.col::<Supercharge>(SUPERCHARGES)
            .update_one(filter, update)
            .await
            .map(|result| result.matched_count == 1)
            .map_err(|_| create_database_error!("update_one", SUPERCHARGES))
    }

    async fn delete_supercharge(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, SUPERCHARGES, id).map(|_| ())
    }

    async fn unplace_supercharges_from_server(&self, server_id: &str) -> Result<()> {
        self.col::<Supercharge>(SUPERCHARGES)
            .update_many(
                doc! { "server_id": server_id },
                doc! { "$unset": { "server_id": 1 } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", SUPERCHARGES))
    }

    async fn fetch_supercharge_server_ids(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .col::<bson::Document>(SUPERCHARGES)
            .distinct("server_id", doc! { "server_id": { "$exists": true } })
            .await
            .map_err(|_| create_database_error!("distinct", SUPERCHARGES))?
            .into_iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect();

        let mut cursor = self
            .col::<bson::Document>(SERVERS)
            .find(doc! { "supercharge.count": { "$gt": 0 } })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| create_database_error!("find", SERVERS))?;
        while let Some(Ok(document)) = cursor.next().await {
            if let Ok(id) = document.get_str("_id") {
                ids.push(id.to_string());
            }
        }

        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn purchase_promotion(&self, purchase: PromotionPurchase<'_>) -> Result<bool> {
        let mut filter = doc! { "_id": purchase.id };
        if purchase.only_if_free {
            filter.insert(
                "$or",
                vec![
                    doc! { "ends_at": { "$lte": purchase.now_ms } },
                    doc! { "server_id": purchase.server_id },
                    // a place held in the other world (sandbox vs live) is free here
                    doc! { "test": { "$ne": purchase.test } },
                ],
            );
        }

        // Same server: keep its start and counters and add the time on;
        // otherwise start fresh
        let same = doc! { "$eq": ["$server_id", purchase.server_id] };
        let pipeline = vec![doc! {
            "$set": {
                "kind": purchase.kind,
                "target_kind": purchase.target_kind,
                "buyer_id": purchase.buyer_id,
                "test": purchase.test,
                "started_at": { "$cond": [&same, "$started_at", purchase.now_ms] },
                "ends_at": {
                    "$add": [
                        { "$cond": [&same, { "$max": ["$ends_at", purchase.now_ms] }, purchase.now_ms] },
                        purchase.add_ms,
                    ]
                },
                "views": { "$cond": [&same, "$views", 0_i64] },
                "clicks": { "$cond": [&same, "$clicks", 0_i64] },
                "joins": { "$cond": [&same, "$joins", 0_i64] },
                "orders": {
                    "$cond": [
                        &same,
                        { "$concatArrays": [{ "$ifNull": ["$orders", []] }, [purchase.order_id]] },
                        [purchase.order_id],
                    ]
                },
                "server_id": purchase.server_id,
            }
        }];

        match self
            .col::<bson::Document>(PROMOTIONS)
            .update_one(filter, pipeline)
            .with_options(UpdateOptions::builder().upsert(true).build())
            .await
        {
            Ok(_) => Ok(true),
            // the place exists and belongs to someone else
            Err(error) if purchase.only_if_free && is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("update_one", PROMOTIONS)),
        }
    }

    async fn fetch_active_promotions(&self, now_ms: i64) -> Result<Vec<DiscoverPromotion>> {
        query!(
            self,
            find,
            PROMOTIONS,
            doc! { "ends_at": { "$gt": now_ms } }
        )
    }

    async fn insert_waitlist_entry(&self, entry: &DiscoverWaitlistEntry) -> Result<()> {
        query!(self, insert_one, WAITLIST, &entry).map(|_| ())
    }

    async fn fetch_waitlist(&self) -> Result<Vec<DiscoverWaitlistEntry>> {
        self.find_with_options(
            WAITLIST,
            doc! {},
            FindOptions::builder().sort(doc! { "created_at": 1 }).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", WAITLIST))
    }

    async fn delete_waitlist_entry(&self, id: &str) -> Result<()> {
        query!(self, delete_one_by_id, WAITLIST, id).map(|_| ())
    }

    async fn delete_promotions_for_target(&self, target_id: &str) -> Result<()> {
        self.col::<DiscoverPromotion>(PROMOTIONS)
            .delete_many(doc! { "server_id": target_id })
            .await
            .map_err(|_| create_database_error!("delete_many", PROMOTIONS))?;
        self.col::<DiscoverWaitlistEntry>(WAITLIST)
            .delete_many(doc! { "target_id": target_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", WAITLIST))
    }

    async fn insert_order(&self, order: &BillingOrder) -> Result<bool> {
        match self.col::<BillingOrder>(ORDERS).insert_one(order).await {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("insert_one", ORDERS)),
        }
    }

    async fn fetch_order(&self, id: &str) -> Result<BillingOrder> {
        query!(self, find_one_by_id, ORDERS, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_orders_by_user(&self, user_id: &str, limit: i64) -> Result<Vec<BillingOrder>> {
        self.find_with_options(
            ORDERS,
            doc! { "user_id": user_id },
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ORDERS))
    }

    async fn transition_order(
        &self,
        id: &str,
        from: OrderStatus,
        to: OrderStatus,
        paid_at: Option<i64>,
        failure: Option<String>,
    ) -> Result<bool> {
        let mut set = doc! { "status": to_bson(&to) };
        if let Some(paid_at) = paid_at {
            set.insert("paid_at", paid_at);
        }
        if let Some(failure) = failure {
            set.insert("failure", failure);
        }
        self.col::<BillingOrder>(ORDERS)
            .update_one(doc! { "_id": id, "status": to_bson(&from) }, doc! { "$set": set })
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn cancel_stale_orders(&self, before_ms: i64) -> Result<()> {
        self.col::<BillingOrder>(ORDERS)
            .update_many(
                // provider checkouts expire at the provider and are
                // reconciled instead
                doc! {
                    "status": to_bson(&OrderStatus::Pending),
                    "provider": "test",
                    "created_at": { "$lt": before_ms },
                },
                doc! { "$set": { "status": to_bson(&OrderStatus::Cancelled) } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", ORDERS))
    }

    async fn increment_promotion_stat(&self, id: &str, stat: PromotionStat) -> Result<()> {
        self.col::<DiscoverPromotion>(PROMOTIONS)
            .update_one(doc! { "_id": id }, doc! { "$inc": { stat.field(): 1_i64 } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", PROMOTIONS))
    }

    async fn move_order(
        &self,
        id: &str,
        to: OrderStatus,
        changes: &OrderChanges,
    ) -> Result<bool> {
        let sources: Vec<bson::Bson> = OrderStatus::sources_of(to).iter().map(to_bson).collect();
        if sources.is_empty() {
            return Ok(false);
        }
        let mut set = changes_doc(changes);
        set.insert("status", to_bson(&to));
        self.col::<BillingOrder>(ORDERS)
            .update_one(
                doc! { "_id": id, "status": { "$in": sources } },
                doc! { "$set": set },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn update_order(&self, id: &str, changes: &OrderChanges) -> Result<()> {
        let set = changes_doc(changes);
        if set.is_empty() {
            return Ok(());
        }
        self.col::<BillingOrder>(ORDERS)
            .update_one(doc! { "_id": id }, doc! { "$set": set })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn fetch_order_by_provider(
        &self,
        reference: ProviderRef,
        value: &str,
    ) -> Result<Option<BillingOrder>> {
        if value.is_empty() {
            return Ok(None);
        }
        // the first order of a subscription is the purchase; renewals carry
        // the same subscription id
        self.col::<BillingOrder>(ORDERS)
            .find_one(doc! { reference.field(): value })
            .sort(doc! { "created_at": 1 })
            .await
            .map_err(|_| create_database_error!("find_one", ORDERS))
    }

    async fn claim_fulfilment(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool> {
        self.col::<BillingOrder>(ORDERS)
            .update_one(
                doc! {
                    "_id": id,
                    "status": to_bson(&OrderStatus::Paid),
                    "$or": [
                        { "fulfilment": { "$in": [to_bson(&Fulfilment::None), to_bson(&Fulfilment::Failed)] } },
                        { "fulfilment": { "$exists": false } },
                        {
                            "fulfilment": to_bson(&Fulfilment::Processing),
                            "fulfilment_lease": { "$lt": now_ms },
                        },
                    ],
                },
                doc! {
                    "$set": {
                        "fulfilment": to_bson(&Fulfilment::Processing),
                        "fulfilment_lease": lease_until,
                    },
                    "$inc": { "fulfilment_attempts": 1 },
                },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn finish_fulfilment(&self, id: &str, ok: bool, error: Option<String>) -> Result<()> {
        let mut set = doc! {
            "fulfilment": to_bson(&if ok { Fulfilment::Done } else { Fulfilment::Failed }),
        };
        let mut update = doc! { "$unset": { "fulfilment_lease": 1 } };
        match error {
            Some(error) => {
                set.insert("fulfilment_error", error);
            }
            None => {
                update
                    .get_document_mut("$unset")
                    .expect("set above")
                    .insert("fulfilment_error", 1);
            }
        }
        update.insert("$set", set);
        self.col::<BillingOrder>(ORDERS)
            .update_one(
                doc! { "_id": id, "fulfilment": to_bson(&Fulfilment::Processing) },
                update,
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn fetch_undelivered_orders(
        &self,
        now_ms: i64,
        max_attempts: u32,
    ) -> Result<Vec<BillingOrder>> {
        self.find_with_options(
            ORDERS,
            doc! {
                "status": to_bson(&OrderStatus::Paid),
                "fulfilment_attempts": { "$lt": max_attempts as i64 },
                "$or": [
                    { "fulfilment": { "$in": [to_bson(&Fulfilment::None), to_bson(&Fulfilment::Failed)] } },
                    {
                        "fulfilment": to_bson(&Fulfilment::Processing),
                        "fulfilment_lease": { "$lt": now_ms },
                    },
                ],
            },
            FindOptions::builder()
                .sort(doc! { "created_at": 1 })
                .limit(100)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ORDERS))
    }

    async fn fetch_open_provider_orders(
        &self,
        before_ms: i64,
        limit: i64,
    ) -> Result<Vec<BillingOrder>> {
        let open: Vec<bson::Bson> = [
            OrderStatus::Created,
            OrderStatus::Pending,
            OrderStatus::RequiresAction,
            OrderStatus::Processing,
        ]
        .iter()
        .map(to_bson)
        .collect();
        self.find_with_options(
            ORDERS,
            doc! {
                "status": { "$in": open },
                "provider": { "$ne": "test" },
                "created_at": { "$lt": before_ms },
            },
            FindOptions::builder()
                .sort(doc! { "created_at": 1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ORDERS))
    }

    async fn mark_order_synced(&self, id: &str, now_ms: i64, not_since: i64) -> Result<bool> {
        self.col::<BillingOrder>(ORDERS)
            .update_one(
                doc! {
                    "_id": id,
                    "$or": [
                        { "synced_at": { "$exists": false } },
                        { "synced_at": { "$lt": not_since } },
                    ],
                },
                doc! { "$set": { "synced_at": now_ms } },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", ORDERS))
    }

    async fn search_orders(&self, search: &OrderSearch, limit: i64) -> Result<Vec<BillingOrder>> {
        let mut filter = doc! {};
        if let Some(user_id) = &search.user_id {
            filter.insert("user_id", user_id);
        }
        if let Some(status) = search.status {
            filter.insert("status", to_bson(&status));
        }
        if search.fulfilment_failed {
            filter.insert("fulfilment", to_bson(&Fulfilment::Failed));
        }
        if let Some(before) = search.before_ms {
            filter.insert("created_at", doc! { "$lt": before });
        }
        if let Some(reference) = &search.reference {
            filter.insert(
                "$or",
                vec![
                    doc! { "_id": reference },
                    doc! { "provider_checkout_id": reference },
                    doc! { "provider_payment_id": reference },
                    doc! { "provider_subscription_id": reference },
                    doc! { "provider_invoice_id": reference },
                    doc! { "provider_customer_id": reference },
                ],
            );
        }
        self.find_with_options(
            ORDERS,
            filter,
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ORDERS))
    }

    async fn count_orders_by_status(&self) -> Result<Vec<(OrderStatus, u64)>> {
        let mut cursor = self
            .col::<bson::Document>(ORDERS)
            .aggregate(vec![doc! { "$group": { "_id": "$status", "count": { "$sum": 1 } } }])
            .await
            .map_err(|_| create_database_error!("aggregate", ORDERS))?;
        let mut counts = vec![];
        while let Some(Ok(document)) = cursor.next().await {
            let Some(status) = document
                .get("_id")
                .and_then(|value| bson::from_bson::<OrderStatus>(value.clone()).ok())
            else {
                continue;
            };
            let count = document
                .get_i32("count")
                .map(|count| count as u64)
                .or_else(|_| document.get_i64("count").map(|count| count as u64))
                .unwrap_or_default();
            counts.push((status, count));
        }
        Ok(counts)
    }

    async fn insert_webhook_event(&self, event: &BillingWebhookEvent) -> Result<bool> {
        match self
            .col::<BillingWebhookEvent>(WEBHOOK_EVENTS)
            .insert_one(event)
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("insert_one", WEBHOOK_EVENTS)),
        }
    }

    async fn fetch_webhook_event(&self, id: &str) -> Result<Option<BillingWebhookEvent>> {
        query!(self, find_one_by_id, WEBHOOK_EVENTS, id)
    }

    async fn claim_webhook_event(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool> {
        self.col::<BillingWebhookEvent>(WEBHOOK_EVENTS)
            .update_one(
                doc! {
                    "_id": id,
                    "status": to_bson(&WebhookEventStatus::Received),
                    "next_attempt_at": { "$lte": now_ms },
                    "$or": [
                        { "lease": { "$exists": false } },
                        { "lease": { "$lt": now_ms } },
                    ],
                },
                doc! { "$set": { "lease": lease_until }, "$inc": { "attempts": 1 } },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", WEBHOOK_EVENTS))
    }

    async fn finish_webhook_event(
        &self,
        id: &str,
        status: WebhookEventStatus,
        error: Option<String>,
        next_attempt_at: i64,
        order_id: Option<String>,
        now_ms: i64,
    ) -> Result<()> {
        let mut set = doc! {
            "status": to_bson(&status),
            "next_attempt_at": next_attempt_at,
        };
        if status != WebhookEventStatus::Received {
            set.insert("processed_at", now_ms);
        }
        if let Some(error) = error {
            set.insert("last_error", error);
        }
        if let Some(order_id) = order_id {
            set.insert("order_id", order_id);
        }
        self.col::<BillingWebhookEvent>(WEBHOOK_EVENTS)
            .update_one(
                doc! { "_id": id },
                doc! { "$set": set, "$unset": { "lease": 1 } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", WEBHOOK_EVENTS))
    }

    async fn fetch_due_webhook_events(
        &self,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<BillingWebhookEvent>> {
        self.find_with_options(
            WEBHOOK_EVENTS,
            doc! {
                "status": to_bson(&WebhookEventStatus::Received),
                "next_attempt_at": { "$lte": now_ms },
                "$or": [
                    { "lease": { "$exists": false } },
                    { "lease": { "$lt": now_ms } },
                ],
            },
            FindOptions::builder()
                .sort(doc! { "created": 1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", WEBHOOK_EVENTS))
    }

    async fn fetch_webhook_events(
        &self,
        status: Option<WebhookEventStatus>,
        limit: i64,
    ) -> Result<Vec<BillingWebhookEvent>> {
        let filter = match status {
            Some(status) => doc! { "status": to_bson(&status) },
            None => doc! {},
        };
        self.find_with_options(
            WEBHOOK_EVENTS,
            filter,
            FindOptions::builder()
                .sort(doc! { "received_at": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", WEBHOOK_EVENTS))
    }

    async fn count_webhook_events(&self, status: WebhookEventStatus) -> Result<u64> {
        self.col::<BillingWebhookEvent>(WEBHOOK_EVENTS)
            .count_documents(doc! { "status": to_bson(&status) })
            .await
            .map_err(|_| create_database_error!("count_documents", WEBHOOK_EVENTS))
    }

    async fn fetch_billing_customer(&self, id: &str) -> Result<Option<BillingCustomer>> {
        query!(self, find_one_by_id, CUSTOMERS, id)
    }

    async fn insert_billing_customer(&self, customer: &BillingCustomer) -> Result<bool> {
        match self
            .col::<BillingCustomer>(CUSTOMERS)
            .insert_one(customer)
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if is_duplicate(&error) => Ok(false),
            Err(_) => Err(create_database_error!("insert_one", CUSTOMERS)),
        }
    }

    async fn insert_billing_audit(&self, entry: &BillingAuditEntry) -> Result<()> {
        query!(self, insert_one, AUDIT, &entry).map(|_| ())
    }

    async fn fetch_billing_audit(
        &self,
        order_id: Option<&str>,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BillingAuditEntry>> {
        let mut filter = doc! {};
        if let Some(order_id) = order_id {
            filter.insert("order_id", order_id);
        }
        if let Some(user_id) = user_id {
            filter.insert("user_id", user_id);
        }
        self.find_with_options(
            AUDIT,
            filter,
            FindOptions::builder()
                .sort(doc! { "created_at": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", AUDIT))
    }

    async fn save_reconciliation_issue(&self, issue: &ReconciliationIssue) -> Result<()> {
        self.col::<ReconciliationIssue>(ISSUES)
            .replace_one(doc! { "_id": &issue.id }, issue)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", ISSUES))
    }

    async fn fetch_reconciliation_issues(
        &self,
        resolved: bool,
        limit: i64,
    ) -> Result<Vec<ReconciliationIssue>> {
        self.find_with_options(
            ISSUES,
            doc! { "resolved": resolved },
            FindOptions::builder()
                .sort(doc! { "found_at": -1 })
                .limit(limit)
                .build(),
        )
        .await
        .map_err(|_| create_database_error!("find", ISSUES))
    }

    async fn count_open_reconciliation_issues(&self) -> Result<u64> {
        self.col::<ReconciliationIssue>(ISSUES)
            .count_documents(doc! { "resolved": false })
            .await
            .map_err(|_| create_database_error!("count_documents", ISSUES))
    }

    async fn fetch_plus_by_provider_subscription(
        &self,
        id: &str,
    ) -> Result<Option<PlusSubscription>> {
        query!(
            self,
            find_one,
            PLUS,
            doc! { "provider_subscription_id": id, "ended": false }
        )
    }

    async fn set_plus_provider_state(
        &self,
        id: &str,
        status: SubscriptionStatus,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()> {
        self.col::<bson::Document>(PLUS)
            .update_one(
                doc! { "_id": id, "ended": false },
                vec![doc! {
                    "$set": {
                        "status": to_bson(&status),
                        "cancel_at_period_end": cancel_at_period_end,
                        "current_period_end": {
                            "$add": [period_end_ms, { "$ifNull": ["$extra_ms", 0_i64] }]
                        },
                    }
                }],
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", PLUS))
    }

    async fn add_plus_time(&self, id: &str, order_id: &str, add_ms: i64) -> Result<bool> {
        self.col::<PlusSubscription>(PLUS)
            .update_one(
                doc! { "_id": id, "ended": false, "applied_orders": { "$ne": order_id } },
                doc! {
                    "$inc": { "current_period_end": add_ms, "extra_ms": add_ms },
                    "$push": { "applied_orders": order_id },
                },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", PLUS))
    }

    async fn remove_plus_time(&self, id: &str, order_id: &str, remove_ms: i64) -> Result<bool> {
        self.col::<PlusSubscription>(PLUS)
            .update_one(
                doc! { "_id": id, "ended": false, "applied_orders": order_id },
                doc! {
                    "$inc": { "current_period_end": -remove_ms, "extra_ms": -remove_ms },
                    "$pull": { "applied_orders": order_id },
                },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", PLUS))
    }

    async fn fetch_supercharges_by_order(&self, order_id: &str) -> Result<Vec<Supercharge>> {
        query!(self, find, SUPERCHARGES, doc! { "order_id": order_id })
    }

    async fn fetch_supercharges_by_provider_subscription(
        &self,
        id: &str,
    ) -> Result<Vec<Supercharge>> {
        query!(
            self,
            find,
            SUPERCHARGES,
            doc! { "provider_subscription_id": id }
        )
    }

    async fn set_supercharges_provider_state(
        &self,
        subscription_id: &str,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()> {
        self.col::<Supercharge>(SUPERCHARGES)
            .update_many(
                doc! { "provider_subscription_id": subscription_id },
                doc! {
                    "$set": {
                        "current_period_end": period_end_ms,
                        "cancel_at_period_end": cancel_at_period_end,
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", SUPERCHARGES))
    }

    async fn fetch_promotion_by_order(&self, order_id: &str) -> Result<Option<DiscoverPromotion>> {
        query!(self, find_one, PROMOTIONS, doc! { "orders": order_id })
    }

    async fn remove_promotion_time(
        &self,
        id: &str,
        order_id: &str,
        remove_ms: i64,
    ) -> Result<bool> {
        self.col::<DiscoverPromotion>(PROMOTIONS)
            .update_one(
                doc! { "_id": id, "orders": order_id },
                doc! {
                    "$inc": { "ends_at": -remove_ms },
                    "$pull": { "orders": order_id },
                },
            )
            .await
            .map(|result| result.modified_count == 1)
            .map_err(|_| create_database_error!("update_one", PROMOTIONS))
    }
}

/// `$set` document for order changes
fn changes_doc(changes: &OrderChanges) -> bson::Document {
    let mut set = doc! {};
    let strings = [
        ("failure", &changes.failure),
        ("provider_checkout_id", &changes.provider_checkout_id),
        ("checkout_url", &changes.checkout_url),
        ("provider_customer_id", &changes.provider_customer_id),
        ("provider_payment_id", &changes.provider_payment_id),
        ("provider_subscription_id", &changes.provider_subscription_id),
        ("provider_invoice_id", &changes.provider_invoice_id),
        ("invoice_url", &changes.invoice_url),
        ("receipt_url", &changes.receipt_url),
        ("dispute_status", &changes.dispute_status),
    ];
    for (field, value) in strings {
        if let Some(value) = value {
            set.insert(field, value.clone());
        }
    }
    if let Some(paid_at) = changes.paid_at {
        set.insert("paid_at", paid_at);
    }
    if let Some(expires) = changes.checkout_expires_at {
        set.insert("checkout_expires_at", expires);
    }
    if let Some(refunded) = changes.amount_refunded {
        set.insert("amount_refunded", refunded as i64);
    }
    if let Some(revoked) = changes.revoked {
        set.insert("revoked", revoked);
    }
    set
}
