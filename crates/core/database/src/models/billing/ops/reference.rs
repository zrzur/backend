use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0::SuperchargeSource;
use revolt_result::Result;
use ulid::Ulid;

use revolt_models::v0::{OrderStatus, SubscriptionStatus};

use crate::{
    from_epoch_ms, BillingAuditEntry, BillingCustomer, BillingOrder, BillingWebhookEvent,
    DiscoverPromotion, DiscoverWaitlistEntry, Fulfilment, PlusSubscription, ReconciliationIssue,
    ReferenceDb, Supercharge, WebhookEventStatus,
};

use super::{
    AbstractBilling, OrderChanges, OrderSearch, PromotionPurchase, PromotionStat, ProviderRef,
};

#[async_trait]
impl AbstractBilling for ReferenceDb {
    async fn fetch_plus_subscription(&self, user_id: &str) -> Result<Option<PlusSubscription>> {
        Ok(self
            .plus_subscriptions
            .lock()
            .await
            .values()
            .find(|sub| sub.user_id == user_id && !sub.ended)
            .cloned())
    }

    async fn insert_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        let mut subscriptions = self.plus_subscriptions.lock().await;
        if subscriptions.contains_key(&subscription.id) {
            return Err(create_error!(InvalidOperation));
        }
        subscriptions.insert(subscription.id.clone(), subscription.clone());
        Ok(())
    }

    async fn save_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        self.plus_subscriptions
            .lock()
            .await
            .insert(subscription.id.clone(), subscription.clone());
        Ok(())
    }

    async fn extend_plus_subscription(&self, id: &str, add_ms: i64) -> Result<()> {
        if let Some(sub) = self.plus_subscriptions.lock().await.get_mut(id) {
            if !sub.ended {
                sub.current_period_end = sub.current_period_end + Duration::milliseconds(add_ms);
            }
        }
        Ok(())
    }

    async fn end_plus_subscription(&self, subscription: &PlusSubscription) -> Result<()> {
        let mut subscriptions = self.plus_subscriptions.lock().await;
        subscriptions.remove(&subscription.id);
        let mut history = subscription.clone();
        history.id = Ulid::new().to_string();
        history.ended = true;
        subscriptions.insert(history.id.clone(), history);
        Ok(())
    }

    async fn fetch_lapsed_plus_subscriptions(
        &self,
        now: Timestamp,
    ) -> Result<Vec<PlusSubscription>> {
        Ok(self
            .plus_subscriptions
            .lock()
            .await
            .values()
            .filter(|sub| !sub.ended && sub.current_period_end <= now)
            .cloned()
            .collect())
    }

    async fn fetch_supercharge(&self, id: &str) -> Result<Supercharge> {
        self.supercharges
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_supercharges_by_user(&self, user_id: &str) -> Result<Vec<Supercharge>> {
        Ok(self
            .supercharges
            .lock()
            .await
            .values()
            .filter(|item| item.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn fetch_supercharges_by_server(&self, server_id: &str) -> Result<Vec<Supercharge>> {
        Ok(self
            .supercharges
            .lock()
            .await
            .values()
            .filter(|item| item.server_id.as_deref() == Some(server_id))
            .cloned()
            .collect())
    }

    async fn fetch_lapsed_supercharges(&self, now: Timestamp) -> Result<Vec<Supercharge>> {
        Ok(self
            .supercharges
            .lock()
            .await
            .values()
            .filter(|item| {
                item.source == SuperchargeSource::Purchase
                    && item.current_period_end.is_some_and(|end| end <= now)
            })
            .cloned()
            .collect())
    }

    async fn save_supercharge(&self, supercharge: &Supercharge) -> Result<()> {
        self.supercharges
            .lock()
            .await
            .insert(supercharge.id.clone(), supercharge.clone());
        Ok(())
    }

    async fn ensure_supercharge(&self, supercharge: &Supercharge) -> Result<()> {
        self.supercharges
            .lock()
            .await
            .entry(supercharge.id.clone())
            .or_insert_with(|| supercharge.clone());
        Ok(())
    }

    async fn move_supercharge(
        &self,
        id: &str,
        user_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<bool> {
        let mut supercharges = self.supercharges.lock().await;
        match supercharges.get_mut(id) {
            Some(item) if item.user_id == user_id && item.server_id.as_deref() == from => {
                item.server_id = to.map(ToString::to_string);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_supercharge(&self, id: &str) -> Result<()> {
        self.supercharges.lock().await.remove(id);
        Ok(())
    }

    async fn unplace_supercharges_from_server(&self, server_id: &str) -> Result<()> {
        for item in self.supercharges.lock().await.values_mut() {
            if item.server_id.as_deref() == Some(server_id) {
                item.server_id = None;
            }
        }
        Ok(())
    }

    async fn fetch_supercharge_server_ids(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .supercharges
            .lock()
            .await
            .values()
            .filter_map(|item| item.server_id.clone())
            .collect();
        for server in self.servers.lock().await.values() {
            if server.supercharge.as_ref().is_some_and(|state| state.count > 0) {
                ids.push(server.id.clone());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn purchase_promotion(&self, purchase: PromotionPurchase<'_>) -> Result<bool> {
        let mut promotions = self.discover_promotions.lock().await;
        let existing = promotions.get(purchase.id).cloned();
        let same = existing
            .as_ref()
            .is_some_and(|item| item.server_id == purchase.server_id);
        if purchase.only_if_free
            && existing
                .as_ref()
                .is_some_and(|item| !same && item.ends_at > purchase.now_ms && item.test == purchase.test)
        {
            return Ok(false);
        }

        let (started_at, from, views, clicks, joins, mut orders) = match existing.filter(|_| same)
        {
            Some(item) => (
                item.started_at,
                item.ends_at.max(purchase.now_ms),
                item.views,
                item.clicks,
                item.joins,
                item.orders,
            ),
            None => (purchase.now_ms, purchase.now_ms, 0, 0, 0, vec![]),
        };
        orders.push(purchase.order_id.to_string());
        promotions.insert(
            purchase.id.to_string(),
            DiscoverPromotion {
                id: purchase.id.to_string(),
                kind: purchase.kind.to_string(),
                target_kind: purchase.target_kind.to_string(),
                server_id: purchase.server_id.to_string(),
                buyer_id: purchase.buyer_id.to_string(),
                started_at,
                ends_at: from + purchase.add_ms,
                test: purchase.test,
                views,
                clicks,
                joins,
                orders,
            },
        );
        Ok(true)
    }

    async fn fetch_active_promotions(&self, now_ms: i64) -> Result<Vec<DiscoverPromotion>> {
        Ok(self
            .discover_promotions
            .lock()
            .await
            .values()
            .filter(|item| item.ends_at > now_ms)
            .cloned()
            .collect())
    }

    async fn insert_waitlist_entry(&self, entry: &DiscoverWaitlistEntry) -> Result<()> {
        self.discover_waitlist
            .lock()
            .await
            .insert(entry.id.clone(), entry.clone());
        Ok(())
    }

    async fn fetch_waitlist(&self) -> Result<Vec<DiscoverWaitlistEntry>> {
        let mut entries: Vec<_> = self.discover_waitlist.lock().await.values().cloned().collect();
        entries.sort_by_key(|entry| entry.created_at);
        Ok(entries)
    }

    async fn delete_waitlist_entry(&self, id: &str) -> Result<()> {
        self.discover_waitlist.lock().await.remove(id);
        Ok(())
    }

    async fn delete_promotions_for_target(&self, target_id: &str) -> Result<()> {
        self.discover_promotions
            .lock()
            .await
            .retain(|_, item| item.server_id != target_id);
        self.discover_waitlist
            .lock()
            .await
            .retain(|_, entry| entry.target_id != target_id);
        Ok(())
    }

    async fn insert_order(&self, order: &BillingOrder) -> Result<bool> {
        let mut orders = self.billing_orders.lock().await;
        if orders.contains_key(&order.id) {
            return Ok(false);
        }
        orders.insert(order.id.clone(), order.clone());
        Ok(true)
    }

    async fn fetch_order(&self, id: &str) -> Result<BillingOrder> {
        self.billing_orders
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_orders_by_user(&self, user_id: &str, limit: i64) -> Result<Vec<BillingOrder>> {
        let mut orders: Vec<BillingOrder> = self
            .billing_orders
            .lock()
            .await
            .values()
            .filter(|order| order.user_id == user_id)
            .cloned()
            .collect();
        orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        orders.truncate(limit.max(0) as usize);
        Ok(orders)
    }

    async fn transition_order(
        &self,
        id: &str,
        from: OrderStatus,
        to: OrderStatus,
        paid_at: Option<i64>,
        failure: Option<String>,
    ) -> Result<bool> {
        let mut orders = self.billing_orders.lock().await;
        match orders.get_mut(id) {
            Some(order) if order.status == from => {
                order.status = to;
                if paid_at.is_some() {
                    order.paid_at = paid_at;
                }
                if failure.is_some() {
                    order.failure = failure;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn cancel_stale_orders(&self, before_ms: i64) -> Result<()> {
        for order in self.billing_orders.lock().await.values_mut() {
            if order.status == OrderStatus::Pending
                && order.provider == "test"
                && order.created_at < before_ms
            {
                order.status = OrderStatus::Cancelled;
            }
        }
        Ok(())
    }

    async fn increment_promotion_stat(&self, id: &str, stat: PromotionStat) -> Result<()> {
        if let Some(item) = self.discover_promotions.lock().await.get_mut(id) {
            match stat {
                PromotionStat::Views => item.views += 1,
                PromotionStat::Clicks => item.clicks += 1,
                PromotionStat::Joins => item.joins += 1,
            }
        }
        Ok(())
    }

    async fn move_order(
        &self,
        id: &str,
        to: OrderStatus,
        changes: &OrderChanges,
    ) -> Result<bool> {
        let mut orders = self.billing_orders.lock().await;
        match orders.get_mut(id) {
            Some(order) if order.status.can_move_to(to) => {
                order.status = to;
                apply_changes(order, changes);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn update_order(&self, id: &str, changes: &OrderChanges) -> Result<()> {
        if let Some(order) = self.billing_orders.lock().await.get_mut(id) {
            apply_changes(order, changes);
        }
        Ok(())
    }

    async fn fetch_order_by_provider(
        &self,
        reference: ProviderRef,
        value: &str,
    ) -> Result<Option<BillingOrder>> {
        if value.is_empty() {
            return Ok(None);
        }
        let mut found: Vec<BillingOrder> = self
            .billing_orders
            .lock()
            .await
            .values()
            .filter(|order| provider_ref(order, reference) == Some(value))
            .cloned()
            .collect();
        found.sort_by_key(|order| order.created_at);
        Ok(found.into_iter().next())
    }

    async fn claim_fulfilment(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool> {
        let mut orders = self.billing_orders.lock().await;
        let Some(order) = orders.get_mut(id) else {
            return Ok(false);
        };
        let free = match order.fulfilment {
            Fulfilment::None | Fulfilment::Failed => true,
            Fulfilment::Processing => order.fulfilment_lease.is_some_and(|lease| lease < now_ms),
            Fulfilment::Done => false,
        };
        if order.status != OrderStatus::Paid || !free {
            return Ok(false);
        }
        order.fulfilment = Fulfilment::Processing;
        order.fulfilment_lease = Some(lease_until);
        order.fulfilment_attempts += 1;
        Ok(true)
    }

    async fn finish_fulfilment(&self, id: &str, ok: bool, error: Option<String>) -> Result<()> {
        if let Some(order) = self.billing_orders.lock().await.get_mut(id) {
            if order.fulfilment == Fulfilment::Processing {
                order.fulfilment = if ok { Fulfilment::Done } else { Fulfilment::Failed };
                order.fulfilment_lease = None;
                order.fulfilment_error = error;
            }
        }
        Ok(())
    }

    async fn fetch_undelivered_orders(
        &self,
        now_ms: i64,
        max_attempts: u32,
    ) -> Result<Vec<BillingOrder>> {
        Ok(self
            .billing_orders
            .lock()
            .await
            .values()
            .filter(|order| {
                order.status == OrderStatus::Paid
                    && order.fulfilment_attempts < max_attempts
                    && match order.fulfilment {
                        Fulfilment::None | Fulfilment::Failed => true,
                        Fulfilment::Processing => {
                            order.fulfilment_lease.is_some_and(|lease| lease < now_ms)
                        }
                        Fulfilment::Done => false,
                    }
            })
            .cloned()
            .collect())
    }

    async fn fetch_open_provider_orders(
        &self,
        before_ms: i64,
        limit: i64,
    ) -> Result<Vec<BillingOrder>> {
        let mut found: Vec<BillingOrder> = self
            .billing_orders
            .lock()
            .await
            .values()
            .filter(|order| {
                order.status.is_open() && order.provider != "test" && order.created_at < before_ms
            })
            .cloned()
            .collect();
        found.sort_by_key(|order| order.created_at);
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn mark_order_synced(&self, id: &str, now_ms: i64, not_since: i64) -> Result<bool> {
        let mut orders = self.billing_orders.lock().await;
        match orders.get_mut(id) {
            Some(order) if order.synced_at.is_none_or(|at| at < not_since) => {
                order.synced_at = Some(now_ms);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn search_orders(&self, search: &OrderSearch, limit: i64) -> Result<Vec<BillingOrder>> {
        let mut found: Vec<BillingOrder> = self
            .billing_orders
            .lock()
            .await
            .values()
            .filter(|order| {
                search.user_id.as_ref().is_none_or(|user| &order.user_id == user)
                    && search.status.is_none_or(|status| order.status == status)
                    && (!search.fulfilment_failed || order.fulfilment == Fulfilment::Failed)
                    && search.before_ms.is_none_or(|before| order.created_at < before)
                    && search.reference.as_ref().is_none_or(|reference| {
                        &order.id == reference
                            || [
                                &order.provider_checkout_id,
                                &order.provider_payment_id,
                                &order.provider_subscription_id,
                                &order.provider_invoice_id,
                                &order.provider_customer_id,
                            ]
                            .iter()
                            .any(|value| value.as_ref() == Some(reference))
                    })
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn count_orders_by_status(&self) -> Result<Vec<(OrderStatus, u64)>> {
        let mut counts: std::collections::HashMap<OrderStatus, u64> = Default::default();
        for order in self.billing_orders.lock().await.values() {
            *counts.entry(order.status).or_default() += 1;
        }
        Ok(counts.into_iter().collect())
    }

    async fn insert_webhook_event(&self, event: &BillingWebhookEvent) -> Result<bool> {
        let mut events = self.billing_webhook_events.lock().await;
        if events.contains_key(&event.id) {
            return Ok(false);
        }
        events.insert(event.id.clone(), event.clone());
        Ok(true)
    }

    async fn fetch_webhook_event(&self, id: &str) -> Result<Option<BillingWebhookEvent>> {
        Ok(self.billing_webhook_events.lock().await.get(id).cloned())
    }

    async fn claim_webhook_event(&self, id: &str, now_ms: i64, lease_until: i64) -> Result<bool> {
        let mut events = self.billing_webhook_events.lock().await;
        match events.get_mut(id) {
            Some(event)
                if event.status == WebhookEventStatus::Received
                    && event.next_attempt_at <= now_ms
                    && event.lease.is_none_or(|lease| lease < now_ms) =>
            {
                event.lease = Some(lease_until);
                event.attempts += 1;
                Ok(true)
            }
            _ => Ok(false),
        }
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
        if let Some(event) = self.billing_webhook_events.lock().await.get_mut(id) {
            event.status = status;
            event.next_attempt_at = next_attempt_at;
            event.lease = None;
            if status != WebhookEventStatus::Received {
                event.processed_at = Some(now_ms);
            }
            if error.is_some() {
                event.last_error = error;
            }
            if order_id.is_some() {
                event.order_id = order_id;
            }
        }
        Ok(())
    }

    async fn fetch_due_webhook_events(
        &self,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<BillingWebhookEvent>> {
        let mut found: Vec<BillingWebhookEvent> = self
            .billing_webhook_events
            .lock()
            .await
            .values()
            .filter(|event| {
                event.status == WebhookEventStatus::Received
                    && event.next_attempt_at <= now_ms
                    && event.lease.is_none_or(|lease| lease < now_ms)
            })
            .cloned()
            .collect();
        found.sort_by_key(|event| event.created);
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn fetch_webhook_events(
        &self,
        status: Option<WebhookEventStatus>,
        limit: i64,
    ) -> Result<Vec<BillingWebhookEvent>> {
        let mut found: Vec<BillingWebhookEvent> = self
            .billing_webhook_events
            .lock()
            .await
            .values()
            .filter(|event| status.is_none_or(|status| event.status == status))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.received_at.cmp(&a.received_at));
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn count_webhook_events(&self, status: WebhookEventStatus) -> Result<u64> {
        Ok(self
            .billing_webhook_events
            .lock()
            .await
            .values()
            .filter(|event| event.status == status)
            .count() as u64)
    }

    async fn fetch_billing_customer(&self, id: &str) -> Result<Option<BillingCustomer>> {
        Ok(self.billing_customers.lock().await.get(id).cloned())
    }

    async fn insert_billing_customer(&self, customer: &BillingCustomer) -> Result<bool> {
        let mut customers = self.billing_customers.lock().await;
        if customers.contains_key(&customer.id) {
            return Ok(false);
        }
        customers.insert(customer.id.clone(), customer.clone());
        Ok(true)
    }

    async fn insert_billing_audit(&self, entry: &BillingAuditEntry) -> Result<()> {
        self.billing_audit
            .lock()
            .await
            .insert(entry.id.clone(), entry.clone());
        Ok(())
    }

    async fn fetch_billing_audit(
        &self,
        order_id: Option<&str>,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BillingAuditEntry>> {
        let mut found: Vec<BillingAuditEntry> = self
            .billing_audit
            .lock()
            .await
            .values()
            .filter(|entry| {
                order_id.is_none_or(|id| entry.order_id.as_deref() == Some(id))
                    && user_id.is_none_or(|id| entry.user_id.as_deref() == Some(id))
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn save_reconciliation_issue(&self, issue: &ReconciliationIssue) -> Result<()> {
        self.billing_issues
            .lock()
            .await
            .insert(issue.id.clone(), issue.clone());
        Ok(())
    }

    async fn fetch_reconciliation_issues(
        &self,
        resolved: bool,
        limit: i64,
    ) -> Result<Vec<ReconciliationIssue>> {
        let mut found: Vec<ReconciliationIssue> = self
            .billing_issues
            .lock()
            .await
            .values()
            .filter(|issue| issue.resolved == resolved)
            .cloned()
            .collect();
        found.sort_by(|a, b| b.found_at.cmp(&a.found_at));
        found.truncate(limit.max(0) as usize);
        Ok(found)
    }

    async fn count_open_reconciliation_issues(&self) -> Result<u64> {
        Ok(self
            .billing_issues
            .lock()
            .await
            .values()
            .filter(|issue| !issue.resolved)
            .count() as u64)
    }

    async fn fetch_plus_by_provider_subscription(
        &self,
        id: &str,
    ) -> Result<Option<PlusSubscription>> {
        Ok(self
            .plus_subscriptions
            .lock()
            .await
            .values()
            .find(|sub| !sub.ended && sub.provider_subscription_id.as_deref() == Some(id))
            .cloned())
    }

    async fn set_plus_provider_state(
        &self,
        id: &str,
        status: SubscriptionStatus,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()> {
        if let Some(sub) = self.plus_subscriptions.lock().await.get_mut(id) {
            if !sub.ended {
                sub.status = status;
                sub.cancel_at_period_end = cancel_at_period_end;
                sub.current_period_end = from_epoch_ms(period_end_ms + sub.extra_ms);
            }
        }
        Ok(())
    }

    async fn add_plus_time(&self, id: &str, order_id: &str, add_ms: i64) -> Result<bool> {
        let mut subscriptions = self.plus_subscriptions.lock().await;
        match subscriptions.get_mut(id) {
            Some(sub) if !sub.ended && !sub.applied_orders.iter().any(|o| o == order_id) => {
                sub.current_period_end = sub.current_period_end + Duration::milliseconds(add_ms);
                sub.extra_ms += add_ms;
                sub.applied_orders.push(order_id.to_string());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn remove_plus_time(&self, id: &str, order_id: &str, remove_ms: i64) -> Result<bool> {
        let mut subscriptions = self.plus_subscriptions.lock().await;
        match subscriptions.get_mut(id) {
            Some(sub) if !sub.ended && sub.applied_orders.iter().any(|o| o == order_id) => {
                sub.current_period_end =
                    sub.current_period_end - Duration::milliseconds(remove_ms);
                sub.extra_ms -= remove_ms;
                sub.applied_orders.retain(|o| o != order_id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_supercharges_by_order(&self, order_id: &str) -> Result<Vec<Supercharge>> {
        Ok(self
            .supercharges
            .lock()
            .await
            .values()
            .filter(|item| item.order_id.as_deref() == Some(order_id))
            .cloned()
            .collect())
    }

    async fn fetch_supercharges_by_provider_subscription(
        &self,
        id: &str,
    ) -> Result<Vec<Supercharge>> {
        Ok(self
            .supercharges
            .lock()
            .await
            .values()
            .filter(|item| item.provider_subscription_id.as_deref() == Some(id))
            .cloned()
            .collect())
    }

    async fn set_supercharges_provider_state(
        &self,
        subscription_id: &str,
        period_end_ms: i64,
        cancel_at_period_end: bool,
    ) -> Result<()> {
        for item in self.supercharges.lock().await.values_mut() {
            if item.provider_subscription_id.as_deref() == Some(subscription_id) {
                item.current_period_end = Some(from_epoch_ms(period_end_ms));
                item.cancel_at_period_end = cancel_at_period_end;
            }
        }
        Ok(())
    }

    async fn fetch_promotion_by_order(&self, order_id: &str) -> Result<Option<DiscoverPromotion>> {
        Ok(self
            .discover_promotions
            .lock()
            .await
            .values()
            .find(|item| item.orders.iter().any(|o| o == order_id))
            .cloned())
    }

    async fn remove_promotion_time(
        &self,
        id: &str,
        order_id: &str,
        remove_ms: i64,
    ) -> Result<bool> {
        let mut promotions = self.discover_promotions.lock().await;
        match promotions.get_mut(id) {
            Some(item) if item.orders.iter().any(|o| o == order_id) => {
                item.ends_at -= remove_ms;
                item.orders.retain(|o| o != order_id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn provider_ref(order: &BillingOrder, reference: ProviderRef) -> Option<&str> {
    match reference {
        ProviderRef::Checkout => order.provider_checkout_id.as_deref(),
        ProviderRef::Payment => order.provider_payment_id.as_deref(),
        ProviderRef::Subscription => order.provider_subscription_id.as_deref(),
        ProviderRef::Invoice => order.provider_invoice_id.as_deref(),
    }
}

fn apply_changes(order: &mut BillingOrder, changes: &OrderChanges) {
    let c = changes.clone();
    if c.paid_at.is_some() {
        order.paid_at = c.paid_at;
    }
    if c.failure.is_some() {
        order.failure = c.failure;
    }
    if c.provider_checkout_id.is_some() {
        order.provider_checkout_id = c.provider_checkout_id;
    }
    if c.checkout_url.is_some() {
        order.checkout_url = c.checkout_url;
    }
    if c.checkout_expires_at.is_some() {
        order.checkout_expires_at = c.checkout_expires_at;
    }
    if c.provider_customer_id.is_some() {
        order.provider_customer_id = c.provider_customer_id;
    }
    if c.provider_payment_id.is_some() {
        order.provider_payment_id = c.provider_payment_id;
    }
    if c.provider_subscription_id.is_some() {
        order.provider_subscription_id = c.provider_subscription_id;
    }
    if c.provider_invoice_id.is_some() {
        order.provider_invoice_id = c.provider_invoice_id;
    }
    if c.invoice_url.is_some() {
        order.invoice_url = c.invoice_url;
    }
    if c.receipt_url.is_some() {
        order.receipt_url = c.receipt_url;
    }
    if c.dispute_status.is_some() {
        order.dispute_status = c.dispute_status;
    }
    if let Some(refunded) = c.amount_refunded {
        order.amount_refunded = refunded;
    }
    if let Some(revoked) = c.revoked {
        order.revoked = revoked;
    }
}
