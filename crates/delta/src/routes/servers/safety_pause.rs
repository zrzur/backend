use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, PartialServer, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// Hours a pause may last; 0 ends it
const HOURS: [u8; 5] = [0, 1, 6, 12, 24];

fn until(hours: u8) -> Result<Timestamp> {
    if !HOURS.contains(&hours) {
        return Err(create_error!(InvalidProperty));
    }
    // ending a pause stores "now": the pause is over at once
    Ok(Timestamp::now_utc() + Duration::hours(hours as i64))
}

/// # Pause invites or DMs
///
/// Security actions: refuse every invite and Discover join, and / or stop
/// DMs between members who aren't friends, for 1, 6, 12 or 24 hours
/// (0 ends a pause). Needs Manage Server.
#[openapi(tag = "Server Information")]
#[put("/<target>/safety/pause", data = "<data>")]
pub async fn pause(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataServerPause>,
) -> Result<Json<v0::Server>> {
    let data = data.into_inner();
    let mut server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    if data.invites_hours.is_none() && data.dms_hours.is_none() {
        return Err(create_error!(InvalidOperation));
    }

    let partial = PartialServer {
        invites_paused_until: data.invites_hours.map(until).transpose()?,
        dms_paused_until: data.dms_hours.map(until).transpose()?,
        ..Default::default()
    };
    let before = server.generate_diff(&partial, &[]);
    server.update(db, partial.clone(), vec![]).await?;

    AuditLogEntryAction::ServerEdit {
        before,
        after: partial,
    }
    .insert(db, server.id.clone(), reason, user.id, None)
    .await;

    Ok(Json(server.into(db).await))
}
