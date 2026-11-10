//! Server insights: daily counts of a server for the people who run it.
//!
//! Who: Manage Server, the permission of the server settings the page sits
//! in (the same rule in the SDK and the web app). Others get
//! MissingPermission; people outside the server NotFound.
//!
//! Cost: one indexed find on `server_insights` (server, day) for at most 92
//! rows, the member count from its one-hour cache, and one read by id of
//! the candidate channels to drop those the viewer can not see. Nothing
//! reads messages or members here; crond and the join and leave paths keep
//! the counts (see `ServerInsightDay`).

use iso8601_timestamp::Timestamp;
use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::util::reference::Reference;
use revolt_database::{
    shown_count, summarise_insights, Database, InsightZone, ServerEvent, ServerInsightDay, User,
    INSIGHT_RANGES, TOP_CHANNELS,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// Channels checked for the top list at most (the busiest ones)
const CHANNEL_CANDIDATES: usize = 40;

/// # Fetch Server Insights
///
/// Daily joins, leaves, members, messages, active members and voice minutes
/// of a server for the last 7, 30 or 90 days, counted in the viewer's time
/// zone, with where joins came from and the busiest channels. Needs Manage
/// Server. Counts are aggregated; channel counts of 1 to 4 are `null`.
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
    let members_now = server.get_approximate_member_count(db).await as u32;
    let summary = summarise_insights(&stored, days, &zone, now, members_now);

    // the busiest channels that still exist and that the viewer can see
    let candidates: Vec<(String, u32)> = summary
        .channels
        .into_iter()
        .filter(|(id, _)| server.channels.contains(id))
        .take(CHANNEL_CANDIDATES)
        .collect();
    let mut top_channels = vec![];
    if !candidates.is_empty() {
        let ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
        let channels = db.fetch_channels(&ids).await?;
        for (id, count) in candidates {
            if top_channels.len() == TOP_CHANNELS {
                break;
            }
            let Some(channel) = channels.iter().find(|channel| channel.id() == id) else {
                continue;
            };
            if ServerEvent::channel_visible(db, &user, &server, &member, channel).await {
                top_channels.push(v0::ServerInsightsChannel {
                    channel: id,
                    messages: shown_count(count),
                });
            }
        }
    }

    Ok(Json(v0::ServerInsights {
        days,
        time_zone: zone.label(),
        rows: summary.rows,
        join_sources: summary.join_sources,
        top_channels,
    }))
}
