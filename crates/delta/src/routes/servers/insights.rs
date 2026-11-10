//! Server insights: daily counts of a server for the people who run it.
//!
//! Who: Manage Server, the permission of the server settings the page sits
//! in (the same rule in the SDK and the web app). Others get
//! MissingPermission; people outside the server NotFound.
//!
//! Privacy: messages count only in channels the viewer can see, daily
//! totals included, so a hidden channel never shows up; per-channel counts,
//! active members and voice minutes of 1 to 4 are `null`.
//!
//! Cost: one indexed find on `server_insights` (server, day) for at most 92
//! rows, the member count from its cache, one read of the channels that have
//! messages in those rows (to check what the viewer can see), and the start
//! of counting from a ten-minute memory cache. Nothing reads messages or
//! members here; crond and the join and leave paths keep the counts (see
//! `ServerInsightDay`).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iso8601_timestamp::Timestamp;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::util::reference::Reference;
use revolt_database::{
    day_label, insight_channels, shown_count, summarise_insights, Database, InsightZone,
    ServerEvent, ServerInsightDay, User, INSIGHT_RANGES, TOP_CHANNELS,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// Channels checked for what the viewer can see at most (a server with more
/// channels with messages in the range counts only the first ones)
const CHANNELS_CHECKED: usize = 500;

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
    let mut ids = insight_channels(&stored);
    ids.truncate(CHANNELS_CHECKED);
    let mut visible: HashSet<String> = HashSet::new();
    if !ids.is_empty() {
        for channel in revolt_database::existing_channels(db, &ids).await? {
            if channel.server() == Some(server.id.as_str())
                && ServerEvent::channel_visible(db, &user, &server, &member, &channel).await
            {
                visible.insert(channel.id().to_string());
            }
        }
    }

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
