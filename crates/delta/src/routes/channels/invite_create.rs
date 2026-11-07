use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, Invite, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};

use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use revolt_config::config;
use crate::util::audit_log_reason::AuditLogReason;
use validator::Validate;

/// # Create Invite
///
/// Creates an invite to this channel.
///
/// Channel must be a `TextChannel`.
#[openapi(tag = "Channel Invites")]
#[post("/<target>/invites", data = "<data>")]
pub async fn create_invite(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    // TODO[v1]: make this non-optional; currently silently ignores malformed body
    data: Option<Json<v0::DataCreateInvite>>,
) -> Result<Json<v0::Invite>> {
    let mut data = data.map(Json::into_inner).unwrap_or(v0::DataCreateInvite {
        max_uses: None,
        expires: None,
        never_expires: false,
    });

    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // Bots may create invites too, with the same Invite Others permission
    let limits = config().await.features.limits.global;
    let max_invite_duration_days = Duration::days(limits.max_invite_duration_days as i64);

    // An invite expires after the default unless "never" was chosen
    if data.never_expires {
        if data.expires.is_some() {
            return Err(create_error!(InvalidOperation));
        }
    } else if data.expires.is_none() {
        data.expires =
            Some(Timestamp::now_utc() + Duration::days(limits.default_invite_duration_days as i64));
    }

    if let Some(expires) = data.expires {
        let now = Timestamp::now_utc();
        if expires <= now || expires > now + max_invite_duration_days {
            return Err(create_error!(InvalidOperation));
        }
    }

    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::InviteOthers)?;

    let invite = Invite::create_channel_invite(db, &user, &channel, data.max_uses, data.expires).await?;

    if let Some(server_id) = channel.server() {
        AuditLogEntryAction::InviteCreate {
            invite: invite.code().to_string(),
            channel: channel.id().to_string(),
        }
        .insert(db, server_id.to_string(), reason, user.id, None)
        .await;
    }

    Ok(Json(invite.into()))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::Member;

    use rocket::http::{ContentType, Header, Status};

    /// Bots create invites like people do (Invite Others), as on Discord
    #[rocket::async_test]
    async fn bots_create_invites() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (server, channels) = harness.new_server(&owner).await;
        let (bot, bot_user) = harness.new_bot(&owner).await;
        Member::create(&harness.db, &server, &bot_user, None).await.unwrap();

        let response = harness
            .client
            .post(format!("/channels/{}/invites", channels[0].id()))
            .header(Header::new("x-bot-token", bot.token.clone()))
            .header(ContentType::JSON)
            .body("{}")
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let invite: serde_json::Value = response.into_json().await.unwrap();
        let code = invite["_id"].as_str().unwrap().to_string();

        // someone joins with the bot's invite
        let (_, guest, _) = harness.new_user().await;
        let response = harness
            .client
            .post(format!("/invites/{code}"))
            .header(Header::new("x-session-token", guest.token.clone()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
    }
}
