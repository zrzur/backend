//! Stage channels: speakers talk, the audience listens and can ask to speak.
//!
//! Moderators are the members with Mute Members on the channel: they set the
//! topic, give the floor, send speakers back to the listeners, and (with Move
//! Members too, since everyone leaves the call) close the stage. Acting on
//! someone else needs a higher rank than theirs, like editing a member. The
//! live state is in Redis (see `revolt_database::voice::stage`); each change
//! goes to the channel as a small `StageUpdate` with what changed, and the
//! answers carry the whole state for the person acting.
use iso8601_timestamp::Timestamp;
use revolt_database::{
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    voice::{
        add_stage_speaker, clear_stage_state, get_channel_node, get_stage_state,
        get_voice_channel_members, is_in_voice_channel, is_stage_channel, is_stage_speaker,
        lower_stage_hand, raise_stage_hand, remove_stage_speaker, remove_user_from_voice_channel,
        revoke_voice_reentry, set_stage_topic, sync_user_voice_permissions, HandRaised,
        SpeakerAdded, UserVoiceChannel, VoiceClient, MAX_STAGE_SPEAKERS,
    },
    AuditLogEntryAction, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// The channel, if it is a stage the user can see, with their permissions
async fn stage_channel(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
) -> Result<(Channel, PermissionValue)> {
    let channel = target.as_channel(db).await?;
    let mut query = perms(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    if !is_stage_channel(&channel) {
        return Err(create_error!(InvalidOperation));
    }
    Ok((channel, permissions))
}

fn require(permissions: &PermissionValue, permission: ChannelPermission) -> Result<()> {
    if permissions.has(permission as u64) {
        Ok(())
    } else {
        Err(create_error!(MissingPermission {
            permission: permission.to_string()
        }))
    }
}

/// Acting on someone else on a stage needs a higher rank than theirs
async fn assert_outranks(db: &Database, user: &User, channel: &Channel, target_id: &str) -> Result<()> {
    if target_id == user.id {
        return Ok(());
    }
    let Some(server_id) = channel.server() else {
        return Ok(());
    };
    let server = db.fetch_server(server_id).await?;
    if server.owner == user.id {
        return Ok(());
    }
    if server.owner == target_id {
        return Err(create_error!(NotElevated));
    }
    let ours = db.fetch_member(&server.id, &user.id).await?.get_ranking(&server);
    let theirs = db
        .fetch_member(&server.id, target_id)
        .await
        .map(|member| member.get_ranking(&server))
        .unwrap_or(i64::MAX);
    if theirs <= ours {
        return Err(create_error!(NotElevated));
    }
    Ok(())
}

/// Tell the channel what changed
async fn publish(channel_id: &str, change: v0::StageChange) {
    EventV1::StageUpdate {
        id: channel_id.to_string(),
        data: change,
    }
    .p(channel_id.to_string())
    .await;
}

/// Give LiveKit the user's new publish rights. A stage works without a voice
/// server in the room (nobody joined yet, or no LiveKit at all).
async fn sync_livekit(
    db: &Database,
    voice_client: &VoiceClient,
    channel: &Channel,
    user_id: &str,
) -> Result<()> {
    if !voice_client.is_enabled() {
        return Ok(());
    }
    let Some(node) = get_channel_node(channel.id()).await? else {
        return Ok(());
    };
    let user = Reference::from_unchecked(user_id).as_user(db).await?;
    let server = match channel.server() {
        Some(id) => Some(db.fetch_server(id).await?),
        None => None,
    };
    sync_user_voice_permissions(db, voice_client, &node, &user, channel, server.as_ref(), None)
        .await
}

/// # Fetch Stage
///
/// Topic, speakers and raised hands of a stage channel.
#[openapi(tag = "Voice")]
#[get("/<target>/stage")]
pub async fn fetch_stage(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, _) = stage_channel(db, &user, &target).await?;
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # Edit Stage
///
/// Set or clear the topic of a stage (needs Mute Members).
#[openapi(tag = "Voice")]
#[patch("/<target>/stage", data = "<data>")]
pub async fn edit_stage(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditStage>,
) -> Result<Json<v0::StageState>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    require(&permissions, ChannelPermission::MuteMembers)?;

    let topic = data.topic.as_deref().map(str::trim).filter(|topic| !topic.is_empty());
    set_stage_topic(channel.id(), topic).await?;
    publish(
        channel.id(),
        v0::StageChange {
            topic: Some(topic.unwrap_or_default().to_string()),
            ..Default::default()
        },
    )
    .await;
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # End Stage
///
/// Closes the stage: everyone leaves the call and the topic, speakers and
/// raised hands are cleared. Needs Mute Members and Move Members.
#[openapi(tag = "Voice")]
#[delete("/<target>/stage")]
pub async fn end_stage(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    require(&permissions, ChannelPermission::MuteMembers)?;
    require(&permissions, ChannelPermission::MoveMembers)?;

    let seat = UserVoiceChannel::from_channel(&channel);
    for member in get_voice_channel_members(&seat).await?.unwrap_or_default() {
        revoke_voice_reentry(channel.id(), &member).await?;
        remove_user_from_voice_channel(voice_client, &seat, &member).await?;
        EventV1::VoiceChannelLeave {
            id: channel.id().to_string(),
            user: member,
        }
        .p(channel.id().to_string())
        .await;
    }

    clear_stage_state(channel.id()).await?;
    publish(
        channel.id(),
        v0::StageChange {
            ended: true,
            ..Default::default()
        },
    )
    .await;
    if let Some(server) = channel.server() {
        AuditLogEntryAction::StageEnd {
            channel: channel.id().to_string(),
        }
        .insert(db, server.to_string(), reason, user.id.clone(), None)
        .await;
    }
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # Raise Hand
///
/// Ask to speak on a stage you are listening to (once every 20 seconds).
#[openapi(tag = "Voice")]
#[put("/<target>/stage/hand")]
pub async fn raise_hand(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::Connect)?;
    if !is_in_voice_channel(&user.id, &UserVoiceChannel::from_channel(&channel)).await? {
        return Err(create_error!(NotConnected));
    }
    if is_stage_speaker(channel.id(), &user.id).await? {
        return Err(create_error!(NoEffect));
    }

    let now = Timestamp::now_utc()
        .duration_since(Timestamp::UNIX_EPOCH)
        .whole_milliseconds() as i64;
    match raise_stage_hand(channel.id(), &user.id, now).await? {
        HandRaised::Raised => {
            publish(
                channel.id(),
                v0::StageChange {
                    hands_raised: vec![user.id.clone()],
                    ..Default::default()
                },
            )
            .await
        }
        HandRaised::Already => {}
        HandRaised::TooSoon(retry_after) => {
            return Err(create_error!(RateLimited {
                retry_after,
                global: false
            }))
        }
        HandRaised::Full => return Err(create_error!(NoEffect)),
    }
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # Lower Hand
///
/// Lower your own raised hand, or (with Mute Members) someone else's.
#[openapi(tag = "Voice")]
#[delete("/<target>/stage/hands/<member>")]
pub async fn lower_hand(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    let member_id = if member.id == "@me" { user.id.as_str() } else { member.id };
    if member_id != user.id {
        require(&permissions, ChannelPermission::MuteMembers)?;
    }

    if lower_stage_hand(channel.id(), member_id).await? {
        publish(
            channel.id(),
            v0::StageChange {
                hands_lowered: vec![member_id.to_string()],
                ..Default::default()
            },
        )
        .await;
    }
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # Invite To Speak
///
/// Give someone in the stage the floor (needs Mute Members, and a higher
/// rank than theirs). Their raised hand goes down.
#[openapi(tag = "Voice")]
#[put("/<target>/stage/speakers/<member>")]
pub async fn add_speaker(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    require(&permissions, ChannelPermission::MuteMembers)?;
    let member_id = if member.id == "@me" { user.id.as_str() } else { member.id };
    assert_outranks(db, &user, &channel, member_id).await?;
    if !is_in_voice_channel(member_id, &UserVoiceChannel::from_channel(&channel)).await? {
        return Err(create_error!(NotConnected));
    }

    match add_stage_speaker(channel.id(), member_id).await? {
        SpeakerAdded::Full => {
            return Err(create_error!(TooManySpeakers {
                max: MAX_STAGE_SPEAKERS
            }))
        }
        SpeakerAdded::Already => {}
        SpeakerAdded::Added { hand_lowered } => {
            if let Err(error) = sync_livekit(db, voice_client, &channel, member_id).await {
                // their join token still can't publish: they speak after a rejoin
                log::warn!("stage: could not open the microphone of {member_id}: {error:?}");
            }
            publish(
                channel.id(),
                v0::StageChange {
                    speakers_added: vec![member_id.to_string()],
                    hands_lowered: if hand_lowered {
                        vec![member_id.to_string()]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
            )
            .await;
        }
    }
    Ok(Json(get_stage_state(channel.id()).await?))
}

/// # Move To Audience
///
/// Stop speaking yourself, or (with Mute Members and a higher rank) send a
/// speaker back to the listeners. When the voice server can't be told, the
/// speaker is taken out of the call rather than left able to talk.
#[openapi(tag = "Voice")]
#[delete("/<target>/stage/speakers/<member>")]
pub async fn remove_speaker(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    let member_id = if member.id == "@me" { user.id.as_str() } else { member.id };
    if member_id != user.id {
        require(&permissions, ChannelPermission::MuteMembers)?;
        assert_outranks(db, &user, &channel, member_id).await?;
    }

    if remove_stage_speaker(channel.id(), member_id).await? {
        // try twice, then take them out of the call: a speaker sent back
        // must not keep a microphone
        let mut result = sync_livekit(db, voice_client, &channel, member_id).await;
        if result.is_err() {
            result = sync_livekit(db, voice_client, &channel, member_id).await;
        }
        if let Err(error) = result {
            log::warn!("stage: could not close the microphone of {member_id}: {error:?}");
            remove_user_from_voice_channel(
                voice_client,
                &UserVoiceChannel::from_channel(&channel),
                member_id,
            )
            .await?;
            EventV1::VoiceChannelLeave {
                id: channel.id().to_string(),
                user: member_id.to_string(),
            }
            .p(channel.id().to_string())
            .await;
        }
        publish(
            channel.id(),
            v0::StageChange {
                speakers_removed: vec![member_id.to_string()],
                ..Default::default()
            },
        )
        .await;
    }
    Ok(Json(get_stage_state(channel.id()).await?))
}
