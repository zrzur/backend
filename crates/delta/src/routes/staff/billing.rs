//! Billing in the admin console: Feather subscriptions, Wings and Discover
//! purchases. Same functions as `/billing/admin` (which stays limited to
//! `privileged` accounts with an MFA ticket), through a staff session:
//!
//! - `platform.billing.read`: summary, orders, billing audit, webhooks,
//!   reconciliation issues
//! - `platform.billing.manage` + a fresh step-up: refund, grant Feather,
//!   deliver again, retry a webhook, reconcile now
//!
//! Every change is written to the platform audit log first (and the
//! billing functions keep writing their own billing audit). No payment is
//! ever created here.

use iso8601_timestamp::Timestamp;
use revolt_database::{
    admin_summary, epoch_ms, fulfil_order, grant_plus, process_webhook_event, reconcile,
    record_platform_audit, request_refund, AuditOutcome, AuditRecord, Database, OrderSearch,
    StaffPermission, WebhookEventStatus,
};
use revolt_models::v0::{self, OrderStatus};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use super::StaffContext;

fn limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

/// Record, then run a change; a change that fails afterwards gets a second
/// record saying so
async fn audited<T>(
    db: &Database,
    record: AuditRecord,
    change: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    match change.await {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failed = record;
            failed.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
            let _ = record_platform_audit(db, failed, AuditOutcome::Failed).await;
            Err(error)
        }
    }
}

fn validated<T: Validate>(data: T) -> Result<T> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    Ok(data)
}

/// # Billing summary
#[get("/billing/summary")]
pub async fn summary(db: &State<Database>, staff: StaffContext) -> Result<Json<v0::BillingAdminSummary>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    Ok(Json(admin_summary(db).await?))
}

/// # Orders
///
/// Newest first; `reference` matches an order id or any provider id.
#[get("/billing/orders?<member>&<status>&<reference>&<failed>&<before>&<limit>")]
#[allow(clippy::too_many_arguments)]
pub async fn orders(
    db: &State<Database>,
    staff: StaffContext,
    member: Option<&str>,
    status: Option<&str>,
    reference: Option<&str>,
    failed: Option<bool>,
    before: Option<i64>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::AdminOrder>>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    if reference.is_some_and(|r| r.len() > 128) || member.is_some_and(|m| m.len() > 64) {
        return Err(create_error!(FailedValidation {
            error: "query too long".to_string()
        }));
    }
    let status = match status {
        Some(status) => Some(
            serde_json::from_value::<OrderStatus>(serde_json::Value::String(status.to_string()))
                .map_err(|_| create_error!(InvalidOperation))?,
        ),
        None => None,
    };
    let search = OrderSearch {
        user_id: member.map(String::from),
        status,
        reference: reference.map(String::from),
        fulfilment_failed: failed.unwrap_or(false),
        before_ms: before,
    };
    Ok(Json(
        db.search_orders(&search, self::limit(limit))
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Order
#[get("/billing/orders/<id>")]
pub async fn order(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<v0::AdminOrder>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    Ok(Json(db.fetch_order(id).await?.into()))
}

/// # Billing audit log
#[get("/billing/audit?<order>&<member>&<limit>")]
pub async fn audit(
    db: &State<Database>,
    staff: StaffContext,
    order: Option<&str>,
    member: Option<&str>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::BillingAuditEntry>>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    Ok(Json(db.fetch_billing_audit(order, member, self::limit(limit)).await?))
}

/// # Webhook events
#[get("/billing/webhooks?<status>&<limit>")]
pub async fn webhooks(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::WebhookEventInfo>>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    let status = match status {
        Some("Received") => Some(WebhookEventStatus::Received),
        Some("Processed") => Some(WebhookEventStatus::Processed),
        Some("Ignored") => Some(WebhookEventStatus::Ignored),
        Some("Failed") => Some(WebhookEventStatus::Failed),
        Some(_) => return Err(create_error!(InvalidOperation)),
        None => None,
    };
    Ok(Json(
        db.fetch_webhook_events(status, self::limit(limit))
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Reconciliation issues
#[get("/billing/issues?<resolved>&<limit>")]
pub async fn issues(
    db: &State<Database>,
    staff: StaffContext,
    resolved: Option<bool>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::ReconciliationIssue>>> {
    staff.require(db, StaffPermission::BillingRead).await?;
    Ok(Json(
        db.fetch_reconciliation_issues(resolved.unwrap_or(false), self::limit(limit))
            .await?,
    ))
}

fn order_record(staff: &StaffContext, action: &str, id: &str) -> AuditRecord {
    let mut record = staff.record(action, Some(StaffPermission::BillingManage));
    record.target_kind = Some("order".to_string());
    record.target_id = Some(id.to_string());
    record
}

/// # Refund an order
///
/// Gives money back at the provider (all that is left when no amount is
/// given). A full refund takes the perks back; a partial one keeps them
/// unless `revoke` is set.
#[post("/billing/orders/<id>/refund", data = "<data>")]
pub async fn refund(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<v0::DataRefundOrder>,
) -> Result<Json<v0::AdminOrder>> {
    staff.require_step_up(db, StaffPermission::BillingManage).await?;
    let data = validated(data.into_inner())?;
    db.fetch_order(id).await?;
    let mut record = order_record(&staff, "staff.billing.refund", id);
    record.reason = Some(data.reason.trim().chars().take(500).collect());
    record.summary = Some(match data.amount {
        Some(amount) => format!("amount={amount} revoke={}", data.revoke.unwrap_or(false)),
        None => format!("full revoke={}", data.revoke.unwrap_or(false)),
    });
    let order = audited(
        db,
        record,
        request_refund(db, id, data.amount, data.revoke, &staff.user.id, &data.reason),
    )
    .await?;
    let minor = data.amount.map(|a| a as f64).unwrap_or(order.amount as f64);
    let amount = format!("{:.2} {}", minor / 100.0, order.currency.to_uppercase());
    crate::util::official::refunded(db, &order.user_id, &amount, &data.reason).await;
    Ok(Json(order.into()))
}

/// # Deliver a paid order again
///
/// Delivery is keyed by the order, so nothing is given twice.
#[post("/billing/orders/<id>/deliver")]
pub async fn deliver(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<v0::AdminOrder>> {
    staff.require_step_up(db, StaffPermission::BillingManage).await?;
    db.fetch_order(id).await?;
    let record = order_record(&staff, "staff.billing.deliver", id);
    audited(db, record, fulfil_order(db, id)).await?;
    Ok(Json(db.fetch_order(id).await?.into()))
}

/// # Retry a failed webhook event
#[post("/billing/webhooks/<id>/retry")]
pub async fn retry_webhook(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
) -> Result<Json<v0::WebhookEventInfo>> {
    staff.require_step_up(db, StaffPermission::BillingManage).await?;
    let event = db
        .fetch_webhook_event(id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if event.status != WebhookEventStatus::Failed {
        return Err(create_error!(InvalidOperation));
    }
    let mut record = staff.record("staff.billing.webhook_retry", Some(StaffPermission::BillingManage));
    record.target_kind = Some("webhook".to_string());
    record.target_id = Some(id.to_string());
    audited(db, record, async {
        let now = epoch_ms(Timestamp::now_utc());
        db.finish_webhook_event(id, WebhookEventStatus::Received, None, now, None, now)
            .await?;
        process_webhook_event(db, id).await
    })
    .await?;
    Ok(Json(
        db.fetch_webhook_event(id)
            .await?
            .ok_or_else(|| create_error!(NotFound))?
            .into(),
    ))
}

/// # Reconcile now
///
/// Compares orders and subscriptions with the provider (safe policy only:
/// never creates payments or refunds) and returns the open issues.
#[post("/billing/reconcile")]
pub async fn reconcile_now(db: &State<Database>, staff: StaffContext) -> Result<Json<Vec<v0::ReconciliationIssue>>> {
    staff.require_step_up(db, StaffPermission::BillingManage).await?;
    let record = staff.record("staff.billing.reconcile", Some(StaffPermission::BillingManage));
    audited(db, record, reconcile(db)).await?;
    Ok(Json(db.fetch_reconciliation_issues(false, 200).await?))
}

/// # Grant Feather
///
/// Feather for some days without payment, recorded with the reason.
#[post("/billing/grants", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    staff: StaffContext,
    data: Json<v0::DataGrantPlus>,
) -> Result<Json<v0::BillingOverview>> {
    staff.require_step_up(db, StaffPermission::BillingManage).await?;
    let data = validated(data.into_inner())?;
    let subject = db.fetch_user(&data.user).await?;
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    // nobody gives themselves a free subscription
    if subject.id == staff.user.id {
        return Err(create_error!(NotPrivileged));
    }
    let mut record = staff.record("staff.billing.grant", Some(StaffPermission::BillingManage));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(subject.id.clone());
    record.reason = Some(data.reason.trim().chars().take(500).collect());
    record.summary = Some(format!("{} days", data.days));
    audited(db, record, grant_plus(db, &staff.user.id, &data.user, data.days, &data.reason)).await?;
    crate::util::official::feather_granted(db, &data.user, data.days as i64).await;
    Ok(Json(crate::routes::billing::overview_of(db, &data.user).await?))
}
