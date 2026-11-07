use revolt_database::{
    util::{automod::validate_rules, permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, PartialServer, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

use crate::util::audit_log_reason::AuditLogReason;

/// # Fetch AutoMod rules
///
/// The server's AutoMod rules. Needs Manage Server.
#[openapi(tag = "Server Information")]
#[get("/<target>/automod")]
pub async fn fetch(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::DataAutoModRules>> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    Ok(Json(v0::DataAutoModRules {
        rules: server.automod.unwrap_or_default(),
    }))
}

/// # Set AutoMod rules
///
/// Replace the server's AutoMod rules (at most 10). Needs Manage Server.
#[openapi(tag = "Server Information")]
#[put("/<target>/automod", data = "<data>")]
pub async fn set(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataAutoModRules>,
) -> Result<Json<v0::DataAutoModRules>> {
    let mut server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;

    let rules = data.into_inner().rules;
    validate_rules(&server, &rules)?;

    let partial = PartialServer {
        automod: Some(rules.clone()),
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

    Ok(Json(v0::DataAutoModRules { rules }))
}
