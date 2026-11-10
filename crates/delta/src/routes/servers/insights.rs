//! Server insights: daily counts of a server for the people who run it.
//!
//! Who: Manage Server, the permission of the server settings the page sits
//! in (the same rule in the SDK and the web app). Others get
//! MissingPermission; people outside the server NotFound.
//!
//! Privacy: messages count only in channels the viewer can see, daily
//! totals included, so a hidden channel never shows up; every count but the
//! member total is `null` when it is 1 to 4.
//!
//! Cost: one indexed find on `server_insights` (server, day) for at most 92
//! rows, the member count from its cache, reads (1,000 ids each) of the
//! channels that have messages in those rows (to check what the viewer can
//! see; a thread follows its parent), and the start of counting from a
//! ten-minute memory cache. Nothing reads messages or
//! members here; crond and the join and leave paths keep the counts (see
//! `ServerInsightDay`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iso8601_timestamp::Timestamp;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::util::reference::Reference;
use revolt_database::{
    day_label, existing_channels, insight_channels, shown_count, summarise_insights, Channel,
    Database, InsightZone, ServerEvent, ServerInsightDay, User, INSIGHT_RANGES, TOP_CHANNELS,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// Of these channel ids, the ones the viewer can see. Read in batches;
/// a thread takes its parent's result, worked out once per parent.
async fn visible_channels(
    db: &Database,
    user: &User,
    server: &revolt_database::Server,
    member: &revolt_database::Member,
    ids: &[String],
) -> Result<HashSet<String>> {
    let channels = existing_channels(db, ids).await?;
    // the channels whose permissions decide: a thread's parent, or itself
    let mut deciders: HashMap<String, Option<Channel>> = HashMap::new();
    for channel in &channels {
        if channel.server() != Some(server.id.as_str()) {
            continue;
        }
        let decider = thread_parent(channel).unwrap_or_else(|| channel.id().to_string());
        deciders.entry(decider).or_insert(None);
    }
    for channel in &channels {
        if let Some(slot) = deciders.get_mut(channel.id()) {
            *slot = Some(channel.clone());
        }
    }
    let missing: Vec<String> = deciders
        .iter()
        .filter(|(_, channel)| channel.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    for parent in existing_channels(db, &missing).await? {
        deciders.insert(parent.id().to_string(), Some(parent));
    }

    let mut allowed: HashSet<String> = HashSet::new();
    for (id, channel) in &deciders {
        if let Some(channel) = channel {
            if ServerEvent::channel_visible(db, user, server, member, channel).await {
                allowed.insert(id.clone());
            }
        }
    }
    Ok(channels
        .iter()
        .filter(|channel| channel.server() == Some(server.id.as_str()))
        .filter(|channel| {
            let decider = thread_parent(channel).unwrap_or_else(|| channel.id().to_string());
            allowed.contains(&decider)
        })
        .map(|channel| channel.id().to_string())
        .collect())
}

fn thread_parent(channel: &Channel) -> Option<String> {
    match channel {
        Channel::TextChannel {
            parent: Some(parent),
            ..
        } => Some(parent.clone()),
        _ => None,
    }
}

/// The first hour crond rolled up, read at most every ten minutes
async fn counted_since(db: &Database) -> Option<i64> {
    static CACHE: Mutex<Option<(Instant, Option<i64>)>> = Mutex::new(None);
    if let Some((at, since)) = *CACHE.lock().ok()? {
        if at.elapsed() < Duration::from_secs(600) {
            return since;
        }
    }
    let since = db.fetch_server_insight_since().await.ok().flatten();
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((Instant::now(), since));
    }
    since
}

/// # Fetch Server Insights
///
/// Daily joins, leaves, members, messages, active members and voice minutes
/// of a server for the last 7, 30 or 90 days, counted in the viewer's time
/// zone, with where joins came from and the busiest channels. Needs Manage
/// Server. Counts are aggregated; messages count only in channels the viewer
/// can see; channel counts, active members and voice minutes of 1 to 4 are
/// `null`.
#[openapi(tag = "Server Information")]
#[get("/<target>/insights?<options..>")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsServerInsights,
) -> Result<Json<v0::ServerInsights>> {
    options
        .validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let days = options.days.unwrap_or(30);
    if !INSIGHT_RANGES.contains(&days) {
        return Err(create_error!(FailedValidation {
            error: "days: 7, 30 or 90".to_string()
        }));
    }

    let server = target.as_server(db).await?;
    let member = db
        .fetch_member(&server.id, &user.id)
        .await
        .map_err(|_| create_error!(NotFound))?;
    let mut query = DatabasePermissionQuery::new(db, &user)
        .server(&server)
        .member(&member);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    let zone = InsightZone::pick(options.time_zone.as_deref(), options.utc_offset);
    let now = Timestamp::now_utc();
    let stored = ServerInsightDay::fetch_page(db, &server.id, days, &zone, now).await?;

    // the channels with messages that the viewer can see (threads too)
    let ids = insight_channels(&stored);
    let visible = visible_channels(db, &user, &server, &member, &ids).await?;

    let members_now = server.get_approximate_member_count(db).await as u32;
    let summary = summarise_insights(&stored, days, &zone, now, members_now, &|channel| {
        visible.contains(channel)
    });

    // the busiest of the server's own channels (not threads)
    let top_channels = summary
        .channels
        .into_iter()
        .filter(|(id, _)| server.channels.contains(id))
        .take(TOP_CHANNELS)
        .map(|(channel, count)| v0::ServerInsightsChannel {
            channel,
            messages: shown_count(count),
        })
        .collect();

    Ok(Json(v0::ServerInsights {
        days,
        time_zone: zone.label(),
        rows: summary.rows,
        totals: summary.totals,
        join_sources: summary.join_sources,
        top_channels,
        counted_since: counted_since(db)
            .await
            .map(|hour| day_label(zone.local_day(hour))),
    }))
}
