//! Stage channels: speakers talk, the audience listens and can raise a hand.
//!
//! Moderators are the members with Mute Members on the channel: they set the
//! topic, invite people to speak, move them back to the audience and end the
//! stage. The live state is in Redis (see `revolt_database::voice::stage`),
//! and every change goes to the channel as one `StageUpdate` with the whole
//! (small, bounded) state, so a client never has to merge partial updates.
use iso8601_timestamp::Timestamp;
use revolt_database::{
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    voice::{
        add_stage_speaker, clear_stage_state, get_channel_node, get_stage_state,
        get_voice_channel_members, is_in_voice_channel, is_stage_channel, is_stage_speaker,
        lower_stage_hand, raise_stage_hand, remove_stage_speaker, remove_user_from_voice_channel,
        revoke_voice_reentry, set_stage_topic, sync_user_voice_permissions, UserVoiceChannel,
        VoiceClient, MAX_STAGE_SPEAKERS,
    },
    Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use validator::Validate;

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

fn is_moderator(permissions: &PermissionValue) -> bool {
    permissions.has(ChannelPermission::MuteMembers as u64)
}

/// Send the new state to everyone who follows the channel
async fn publish(channel_id: &str) -> Result<v0::StageState> {
    let state = get_stage_state(channel_id).await?;
    EventV1::StageUpdate {
        id: channel_id.to_string(),
        data: state.clone(),
    }
    .p(channel_id.to_string())
    .await;
    Ok(state)
}

/// Give LiveKit the user's new publish rights. A stage is usable without a
/// voice server in the room (nobody joined yet, or no LiveKit at all), so a
/// failure here is logged, not returned: the state change itself stands and
/// the next join token carries the right grants.
async fn sync_livekit(db: &Database, voice_client: &VoiceClient, channel: &Channel, user_id: &str) {
    if !voice_client.is_enabled() {
        return;
    }
    let Ok(Some(node)) = get_channel_node(channel.id()).await else {
        return;
    };
    let result = async {
        let user = Reference::from_unchecked(user_id).as_user(db).await?;
        let server = match channel.server() {
            Some(id) => Some(db.fetch_server(id).await?),
            None => None,
        };
        sync_user_voice_permissions(db, voice_client, &node, &user, channel, server.as_ref(), None)
            .await
    }
    .await;
    if let Err(error) = result {
        log::warn!("stage: could not update voice rights of {user_id}: {error:?}");
    }
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
    if !is_moderator(&permissions) {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::MuteMembers.to_string()
        }));
    }

    let topic = data.topic.as_deref().map(str::trim).filter(|topic| !topic.is_empty());
    set_stage_topic(channel.id(), topic).await?;
    Ok(Json(publish(channel.id()).await?))
}

/// # End Stage
///
/// Ends the stage: everyone leaves the call and the topic, speakers and
/// raised hands are cleared (needs Mute Members).
#[openapi(tag = "Voice")]
#[delete("/<target>/stage")]
pub async fn end_stage(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::StageState>> {
    let (channel, permissions) = stage_channel(db, &user, &target).await?;
    if !is_moderator(&permissions) {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::MuteMembers.to_string()
        }));
    }

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
    Ok(Json(publish(channel.id()).await?))
}

/// # Raise Hand
///
/// Ask to speak on a stage you are listening to.
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
    if !raise_stage_hand(channel.id(), &user.id, now).await? {
        return Err(create_error!(NoEffect));
    }
    Ok(Json(publish(channel.id()).await?))
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
    if member_id != user.id && !is_moderator(&permissions) {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::MuteMembers.to_string()
        }));
    }

    if !lower_stage_hand(channel.id(), member_id).await? {
        return Ok(Json(get_stage_state(channel.id()).await?));
    }
    Ok(Json(publish(channel.id()).await?))
}

/// # Invite To Speak
///
/// Make someone in the stage a speaker (needs Mute Members). Their raised
/// hand goes down.
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
    if !is_moderator(&permissions) {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::MuteMembers.to_string()
        }));
    }
    let member_id = if member.id == "@me" { user.id.as_str() } else { member.id };
    if !is_in_voice_channel(member_id, &UserVoiceChannel::from_channel(&channel)).await? {
        return Err(create_error!(NotConnected));
    }

    if !add_stage_speaker(channel.id(), member_id).await? {
        return Err(create_error!(TooManySpeakers {
            max: MAX_STAGE_SPEAKERS
        }));
    }
    sync_livekit(db, voice_client, &channel, member_id).await;
    Ok(Json(publish(channel.id()).await?))
}

/// # Move To Audience
///
/// Stop speaking yourself, or (with Mute Members) move a speaker back to
/// the audience.
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
    if member_id != user.id && !is_moderator(&permissions) {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::MuteMembers.to_string()
        }));
    }

    if !remove_stage_speaker(channel.id(), member_id).await? {
        return Ok(Json(get_stage_state(channel.id()).await?));
    }
    sync_livekit(db, voice_client, &channel, member_id).await;
    Ok(Json(publish(channel.id()).await?))
}
