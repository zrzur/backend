//! Playing a soundboard sound sends one small `SoundboardPlay` event to the
//! call; every listener's client plays it from its own cache, at its own
//! volume, or not at all when they muted the soundboard. Nothing goes
//! through the voice server.
use revolt_database::{
    events::client::EventV1,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{
        get_voice_channel_members, is_in_voice_channel, is_stage_channel, is_stage_speaker,
        UserVoiceChannel,
    },
    Database, User,
};
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Play Sound
///
/// Play a sound of this server into the voice channel you are in. Needs Use
/// Soundboard and Speak there, and not being server-muted; on a stage only
/// speakers play sounds. Limited to a few sounds every ten seconds.
#[openapi(tag = "Voice")]
#[post("/<target>/soundboard/<sound_id>")]
pub async fn play_sound(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    sound_id: String,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;
    let Some(server_id) = channel.server() else {
        return Err(create_error!(InvalidOperation));
    };
    if channel.voice().is_none() {
        return Err(create_error!(NotAVoiceChannel));
    }

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::UseSoundboard)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::Speak)?;

    if !is_in_voice_channel(&user.id, &UserVoiceChannel::from_channel(&channel)).await? {
        return Err(create_error!(NotConnected));
    }

    // server-muted members and the audience of a stage stay silent
    let member = Reference::from_unchecked(&user.id)
        .as_member(db, server_id)
        .await?;
    if !member.can_publish
        || (is_stage_channel(&channel) && !is_stage_speaker(channel.id(), &user.id).await?)
    {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::Speak.to_string()
        }));
    }

    let sound = db.fetch_soundboard_sound(&sound_id).await?;
    if sound.server != server_id {
        return Err(create_error!(NotFound));
    }

    // to the people in the call only, not everyone who sees the channel
    let listeners = get_voice_channel_members(&UserVoiceChannel::from_channel(&channel))
        .await?
        .unwrap_or_default();
    EventV1::SoundboardPlay {
        channel_id: channel.id().to_string(),
        sound_id: sound.id,
        user: user.id.clone(),
    }
    .p_broadcast(listeners)
    .await;
    Ok(EmptyResponse)
}
