//! Keeps users, servers and HQ roles in step with Plus subscriptions and
//! supercharges, and renews or ends them when their period is over.

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_models::v0::{self, PlusSource, SuperchargeSource};
use revolt_result::Result;
use ulid::Ulid;

use crate::{
    billing_month, events::client::EventV1, Channel, Database, PartialMember, PartialServer, PartialUser,
    Server, Supercharge, User, UserPlus,
};

/// Supercharge level of a server that counts right now
pub async fn server_supercharge_level(server: &Server) -> usize {
    if !config().await.features.billing.available() {
        return 0;
    }
    server
        .supercharge
        .as_ref()
        .map(|state| state.level as usize)
        .unwrap_or_default()
}

/// Attachment size everyone in a server may send (0 without supercharges)
pub async fn server_supercharge_attachments(db: &Database, server_id: &str) -> usize {
    let Ok(server) = db.fetch_server(server_id).await else {
        return 0;
    };
    let level = server_supercharge_level(&server).await;
    config()
        .await
        .features
        .billing
        .perks(level)
        .map(|perks| perks.attachments)
        .unwrap_or_default()
}

/// Attachment size everyone in a channel's server may send (0 outside
/// servers or without supercharges)
pub async fn channel_supercharge_attachments(db: &Database, channel: &Channel) -> usize {
    match channel.server() {
        Some(server_id) => server_supercharge_attachments(db, server_id).await,
        None => 0,
    }
}

/// Stream resolution allowed in a server, if supercharges raise it
pub async fn server_supercharge_video(db: &Database, server_id: &str) -> Option<[u32; 2]> {
    let server = db.fetch_server(server_id).await.ok()?;
    let level = server_supercharge_level(&server).await;
    config()
        .await
        .features
        .billing
        .perks(level)
        .map(|perks| perks.video_resolution)
}

/// Recount a server's supercharges and tell its members if the level changed
pub async fn sync_server_supercharge(db: &Database, server_id: &str) -> Result<()> {
    let billing = config().await.features.billing;
    let Ok(mut server) = db.fetch_server(server_id).await else {
        // deleted servers keep nothing
        return Ok(());
    };

    // only feathers of this server's world count (sandbox vs live)
    let count = if billing.available() {
        db.fetch_supercharges_by_server(server_id)
            .await?
            .iter()
            .filter(|item| billing.counts(item.test))
            .count()
    } else {
        0
    };
    let next = v0::ServerSupercharge {
        count: count as u32,
        level: billing.level_for(count) as u32,
    };

    if server.supercharge.clone().unwrap_or_default() != next {
        server
            .update(
                db,
                PartialServer {
                    supercharge: Some(next),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }
    Ok(())
}

/// Give or take the Plus role in the official server
pub async fn sync_hq_role(db: &Database, user: &User) -> Result<()> {
    let billing = config().await.features.billing;
    if billing.hq_server.is_empty() || billing.plus_role.is_empty() {
        return Ok(());
    }
    let Ok(mut member) = db.fetch_member(&billing.hq_server, &user.id).await else {
        return Ok(());
    };

    let wanted = user.has_plus().await;
    let has = member.roles.contains(&billing.plus_role);
    if wanted == has {
        return Ok(());
    }

    let mut roles = member.roles.clone();
    if wanted {
        roles.push(billing.plus_role.clone());
    } else {
        roles.retain(|role| role != &billing.plus_role);
    }
    member
        .update(
            db,
            PartialMember {
                roles: Some(roles),
                ..Default::default()
            },
            vec![],
        )
        .await
}

/// Id of the n-th supercharge that comes with a member's Plus
pub fn included_supercharge_id(user_id: &str, n: usize) -> String {
    format!("{user_id}:plus:{n}")
}

/// End everything billing-related for an account being deleted
pub async fn end_billing_for_user(db: &Database, user_id: &str) -> Result<()> {
    if let Some(subscription) = db.fetch_plus_subscription(user_id).await? {
        db.end_plus_subscription(&subscription).await?;
    }
    for item in db.fetch_supercharges_by_user(user_id).await? {
        db.delete_supercharge(&item.id).await?;
        if let Some(server_id) = item.server_id {
            sync_server_supercharge(db, &server_id).await?;
        }
    }
    db.update_user(
        user_id,
        &PartialUser {
            plus: Some(UserPlus::default()),
            ..Default::default()
        },
        vec![],
    )
    .await
}

/// Copy the subscription onto the user, publish their badge, keep the
/// supercharges that come with Plus and the HQ role in step
pub async fn sync_user_plus(db: &Database, user_id: &str) -> Result<()> {
    let billing = config().await.features.billing;
    let subscription = db.fetch_plus_subscription(user_id).await?;
    let mut user = db.fetch_user(user_id).await?;

    let mut plus = user.plus.clone().unwrap_or_default();
    let until = subscription.as_ref().map(|sub| sub.current_period_end);
    let live = subscription.as_ref().is_some_and(|sub| !sub.test);
    if plus.until != until || plus.live != live {
        plus.until = until;
        plus.live = live;
        db.update_user(
            user_id,
            &PartialUser {
                plus: Some(plus.clone()),
                ..Default::default()
            },
            vec![],
        )
        .await?;
        user.plus = Some(plus);
    }

    publish_badges(&user).await;

    // supercharges that come with Plus
    let wanted = if user.has_plus().await {
        billing.plus_free_supercharges
    } else {
        0
    };
    // Included ones have fixed ids, so syncing twice at once can't create
    // extra ones
    let wanted_ids: Vec<String> = (0..wanted)
        .map(|n| included_supercharge_id(user_id, n))
        .collect();
    for id in &wanted_ids {
        db.ensure_supercharge(&Supercharge {
            id: id.clone(),
            user_id: user_id.to_string(),
            server_id: None,
            source: SuperchargeSource::Plus,
            created_at: Timestamp::now_utc(),
            current_period_end: None,
            cancel_at_period_end: false,
            test: subscription.as_ref().is_none_or(|sub| sub.test),
            order_id: None,
            provider_subscription_id: None,
        })
        .await?;
    }
    for item in db.fetch_supercharges_by_user(user_id).await? {
        if item.source == SuperchargeSource::Plus && !wanted_ids.contains(&item.id) {
            db.delete_supercharge(&item.id).await?;
            if let Some(server_id) = item.server_id {
                sync_server_supercharge(db, &server_id).await?;
            }
        }
    }

    sync_hq_role(db, &user).await
}

/// Tell the user and everyone who can see them about their badges
pub async fn publish_badges(user: &User) {
    EventV1::UserUpdate {
        id: user.id.clone(),
        data: v0::PartialUser {
            badges: Some(user.get_badges().await),
            ..Default::default()
        },
        clear: vec![],
        event_id: Some(Ulid::new().to_string()),
    }
    .p(user.id.clone())
    .await;
}

/// Change whether the Plus badge shows
pub async fn set_plus_badge_hidden(db: &Database, user: &mut User, hidden: bool) -> Result<()> {
    let mut plus: UserPlus = user.plus.clone().unwrap_or_default();
    plus.hide_badge = hidden;
    db.update_user(
        &user.id,
        &PartialUser {
            plus: Some(plus.clone()),
            ..Default::default()
        },
        vec![],
    )
    .await?;
    user.plus = Some(plus);
    publish_badges(user).await;
    Ok(())
}

/// How long past its period end a provider subscription keeps going while
/// we wait for the provider to report the renewal (late webhooks, outage).
/// After that it ends here too; reconciliation brings it back if the
/// provider says it was renewed.
fn provider_overdue(billing: &revolt_config::FeaturesBilling) -> iso8601_timestamp::Duration {
    iso8601_timestamp::Duration::days(billing.provider.past_due_grace_days as i64 + 2)
}

/// Renew or end subscriptions and supercharges whose period is over.
///
/// Only the built-in test provider renews here (simulated, nothing is
/// charged). Provider subscriptions renew through the provider's invoice
/// webhooks; here they only end once they are well overdue. Gifts never
/// renew. While billing is unavailable nothing renews.
pub async fn process_lapsed_billing(db: &Database) -> Result<()> {
    let billing = config().await.features.billing;
    let now = Timestamp::now_utc();
    let simulated = billing.mode() == revolt_config::BillingMode::InternalTest;

    for mut sub in db.fetch_lapsed_plus_subscriptions(now).await? {
        if sub.provider_subscription_id.is_some()
            && sub.current_period_end + provider_overdue(&billing) > now
        {
            continue;
        }
        let renew = simulated
            && sub.provider_subscription_id.is_none()
            && sub.test
            && matches!(sub.source, PlusSource::Purchase)
            && !sub.cancel_at_period_end;
        if renew {
            while sub.current_period_end <= now {
                sub.current_period_end = sub.current_period_end + billing_month();
            }
            db.save_plus_subscription(&sub).await?;
            record_renewal(db, &sub.user_id, v0::OrderProduct::Plus, billing.plus_price).await?;
        } else {
            db.end_plus_subscription(&sub).await?;
        }
        sync_user_plus(db, &sub.user_id).await?;
    }

    // unpaid orders don't wait forever
    db.cancel_stale_orders(crate::epoch_ms(now) - 30 * 60 * 1000)
        .await?;

    for mut item in db.fetch_lapsed_supercharges(now).await? {
        if item.provider_subscription_id.is_some()
            && item
                .current_period_end
                .is_some_and(|end| end + provider_overdue(&billing) > now)
        {
            continue;
        }
        if simulated
            && item.provider_subscription_id.is_none()
            && item.test
            && !item.cancel_at_period_end
        {
            let mut end = item.current_period_end.unwrap_or(now);
            while end <= now {
                end = end + billing_month();
            }
            item.current_period_end = Some(end);
            db.save_supercharge(&item).await?;
            record_renewal(
                db,
                &item.user_id,
                v0::OrderProduct::Supercharge {
                    server: item.server_id.clone().unwrap_or_default(),
                    quantity: None,
                },
                billing.supercharge_price,
            )
            .await?;
        } else {
            db.delete_supercharge(&item.id).await?;
            if let Some(server_id) = &item.server_id {
                sync_server_supercharge(db, server_id).await?;
            }
        }
    }

    Ok(())
}

/// Note a renewal in the member's billing history. In test mode nothing
/// is charged; a real provider charges and reports it through its webhook.
async fn record_renewal(
    db: &Database,
    user_id: &str,
    product: v0::OrderProduct,
    amount: u32,
) -> Result<()> {
    let billing = config().await.features.billing;
    let now = crate::epoch_ms(Timestamp::now_utc());
    let mut order = crate::BillingOrder::new(
        Ulid::new().to_string(),
        user_id.to_string(),
        product,
        amount,
        billing.currency.clone(),
        "test",
        true,
    );
    order.kind = v0::OrderKind::Renewal;
    order.status = v0::OrderStatus::Paid;
    order.paid_at = Some(now);
    order.fulfilment = crate::Fulfilment::Done;
    db.insert_order(&order).await.map(|_| ())
}

/// Recount every server that has or shows supercharges. Requests that
/// race each other can leave a count briefly off; this puts it right.
pub async fn reconcile_supercharge_levels(db: &Database) -> Result<()> {
    for server_id in db.fetch_supercharge_server_ids().await? {
        sync_server_supercharge(db, &server_id).await?;
    }
    Ok(())
}

/// Check for lapsed subscriptions and recount server levels every minute,
/// and compare with the payment provider every five minutes
pub async fn billing_worker(db: Database) {
    let mut round: u64 = 0;
    loop {
        if config().await.features.billing.available() {
            if let Err(error) = process_lapsed_billing(&db).await {
                log::error!("billing: {error:?}");
            }
            if let Err(error) = reconcile_supercharge_levels(&db).await {
                log::error!("billing reconcile: {error:?}");
            }
            if let Err(error) = crate::process_waitlist(&db).await {
                log::error!("discover waitlist: {error:?}");
            }
            if let Err(error) = crate::sync_vanity_invites(&db, iso8601_timestamp::Timestamp::now_utc()).await {
                log::error!("vanity links: {error:?}");
            }
            if round % 5 == 0 {
                match crate::reconcile(&db).await {
                    Ok(report) if report != crate::ReconcileReport::default() => {
                        log::info!("billing reconciliation: {report:?}")
                    }
                    Ok(_) => {}
                    Err(error) => log::error!("billing reconciliation: {error:?}"),
                }
            }
        } else if let Err(error) = reconcile_supercharge_levels(&db).await {
            // levels drop to 0 when billing is switched off
            log::error!("billing reconcile: {error:?}");
        } else if let Err(error) = crate::sync_vanity_invites(&db, iso8601_timestamp::Timestamp::now_utc()).await {
            log::error!("vanity links: {error:?}");
        }
        round += 1;
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// Handle webhook events that are due every few seconds (the receiver
/// starts new ones right away; this picks up retries and anything it
/// couldn't start)
pub async fn billing_webhook_worker(db: Database) {
    loop {
        if config().await.features.billing.mode().uses_provider() {
            if let Err(error) = crate::process_due_webhooks(&db).await {
                log::error!("billing webhooks: {error:?}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
