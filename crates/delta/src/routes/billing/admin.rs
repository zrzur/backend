//! Staff billing tools: read orders, webhooks, issues and the audit log;
//! refund, retry and grant. Only privileged accounts; refunds and grants
//! also need a validated MFA ticket and a reason, and are written to the
//! billing audit log. Nothing here is reachable by ordinary members, and
//! there is no test tool here: simulated payments only exist on local
//! development servers.

use iso8601_timestamp::Timestamp;
use revolt_database::{
    admin_summary, epoch_ms, fulfil_order, grant_plus, process_webhook_event, reconcile,
    request_refund, Database, OrderSearch, User, ValidatedTicket, WebhookEventStatus,
};
use revolt_models::v0::{self, OrderStatus};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

fn staff(user: &User) -> Result<()> {
    if user.bot.is_some() || !user.privileged {
        Err(create_error!(NotPrivileged))
    } else {
        Ok(())
    }
}

fn ticket(ticket: Option<ValidatedTicket>) -> Result<()> {
    // the guard only yields a validated, unexpired ticket of this account
    // and consumes it
    ticket
        .map(|_| ())
        .ok_or_else(|| create_error!(InvalidCredentials))
}

fn limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

/// # Billing summary (staff)
///
/// Mode, live gate checks (no secrets), orders per status, webhook and
/// reconciliation counts.
#[openapi(tag = "Billing Admin")]
#[get("/admin/summary")]
pub async fn summary(db: &State<Database>, user: User) -> Result<Json<v0::BillingAdminSummary>> {
    staff(&user)?;
    Ok(Json(admin_summary(db).await?))
}

/// # Search orders (staff)
///
/// Newest first. `reference` matches an order id or any provider id.
#[openapi(tag = "Billing Admin")]
#[get("/admin/orders?<member>&<status>&<reference>&<failed>&<before>&<limit>")]
#[allow(clippy::too_many_arguments)]
pub async fn orders(
    db: &State<Database>,
    user: User,
    member: Option<&str>,
    status: Option<&str>,
    reference: Option<&str>,
    failed: Option<bool>,
    before: Option<i64>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::AdminOrder>>> {
    staff(&user)?;
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

/// # Order (staff)
#[openapi(tag = "Billing Admin")]
#[get("/admin/orders/<id>")]
pub async fn order(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::AdminOrder>> {
    staff(&user)?;
    Ok(Json(db.fetch_order(id).await?.into()))
}

/// # Billing audit log (staff)
#[openapi(tag = "Billing Admin")]
#[get("/admin/audit?<order>&<member>&<limit>")]
pub async fn audit_log(
    db: &State<Database>,
    user: User,
    order: Option<&str>,
    member: Option<&str>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::BillingAuditEntry>>> {
    staff(&user)?;
    Ok(Json(
        db.fetch_billing_audit(order, member, self::limit(limit))
            .await?,
    ))
}

/// # Refund an order (staff, MFA)
///
/// Gives money back at the provider (all that is left when no amount is
/// given). A full refund takes the perks back; a partial one keeps them
/// unless `revoke` is set. Needs `X-MFA-Ticket`.
#[openapi(tag = "Billing Admin")]
#[post("/admin/orders/<id>/refund", data = "<data>")]
pub async fn refund(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    id: &str,
    data: Json<v0::DataRefundOrder>,
) -> Result<Json<v0::AdminOrder>> {
    staff(&user)?;
    ticket(validated_ticket)?;
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let order = request_refund(db, id, data.amount, data.revoke, &user.id, &data.reason).await?;
    Ok(Json(order.into()))
}

/// # Deliver a paid order again (staff)
///
/// Retries delivery of a paid order whose delivery failed. Delivery is
/// keyed by the order, so nothing is given twice.
#[openapi(tag = "Billing Admin")]
#[post("/admin/orders/<id>/deliver")]
pub async fn deliver(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::AdminOrder>> {
    staff(&user)?;
    fulfil_order(db, id).await?;
    Ok(Json(db.fetch_order(id).await?.into()))
}

/// # Webhook events (staff)
#[openapi(tag = "Billing Admin")]
#[get("/admin/webhooks?<status>&<limit>")]
pub async fn webhooks(
    db: &State<Database>,
    user: User,
    status: Option<&str>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::WebhookEventInfo>>> {
    staff(&user)?;
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

/// # Retry a failed webhook event (staff)
#[openapi(tag = "Billing Admin")]
#[post("/admin/webhooks/<id>/retry")]
pub async fn retry_webhook(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::WebhookEventInfo>> {
    staff(&user)?;
    let event = db
        .fetch_webhook_event(id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if event.status != WebhookEventStatus::Failed {
        return Err(create_error!(InvalidOperation));
    }
    let now = epoch_ms(Timestamp::now_utc());
    db.finish_webhook_event(id, WebhookEventStatus::Received, None, now, None, now)
        .await?;
    process_webhook_event(db, id).await?;
    Ok(Json(
        db.fetch_webhook_event(id)
            .await?
            .ok_or_else(|| create_error!(NotFound))?
            .into(),
    ))
}

/// # Reconcile now (staff)
///
/// Compares orders and subscriptions with the provider (safe policy only:
/// never creates payments or refunds) and returns the open issues.
#[openapi(tag = "Billing Admin")]
#[post("/admin/reconcile")]
pub async fn reconcile_now(
    db: &State<Database>,
    user: User,
) -> Result<Json<Vec<v0::ReconciliationIssue>>> {
    staff(&user)?;
    let report = reconcile(db).await?;
    log::info!("billing: reconciliation by {}: {report:?}", user.id);
    Ok(Json(db.fetch_reconciliation_issues(false, 200).await?))
}

/// # Reconciliation issues (staff)
#[openapi(tag = "Billing Admin")]
#[get("/admin/issues?<resolved>&<limit>")]
pub async fn issues(
    db: &State<Database>,
    user: User,
    resolved: Option<bool>,
    limit: Option<i64>,
) -> Result<Json<Vec<v0::ReconciliationIssue>>> {
    staff(&user)?;
    Ok(Json(
        db.fetch_reconciliation_issues(resolved.unwrap_or(false), self::limit(limit))
            .await?,
    ))
}

/// # Grant Plus (staff, MFA)
///
/// Gives Plus for some days without payment, recorded in the billing audit
/// log with the reason. Needs `X-MFA-Ticket`.
#[openapi(tag = "Billing Admin")]
#[post("/admin/grants", data = "<data>")]
pub async fn grant(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    data: Json<v0::DataGrantPlus>,
) -> Result<Json<v0::BillingOverview>> {
    staff(&user)?;
    ticket(validated_ticket)?;
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    grant_plus(db, &user.id, &data.user, data.days, &data.reason).await?;
    Ok(Json(super::overview_of(db, &data.user).await?))
}
