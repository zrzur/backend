//! Stream previews: the sharer's client sends a tiny still of its screen
//! share at most every 25 seconds; others fetch it only when they hover the
//! share before joining. No gateway event: nobody pays for a preview they do
//! not look at.
use revolt_database::{
    util::{permissions::perms, reference::Reference},
    voice::{get_stream_preview, get_voice_state, set_stream_preview, UserVoiceChannel},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

/// # Send Stream Preview
///
/// Replace the preview of your screen share in this voice channel (a JPEG or
/// WebP of at most 20 KB, base64). Only while you share your screen there;
/// a new one is taken at most every 25 seconds (`NoEffect` before that).
#[openapi(tag = "Voice")]
#[put("/<target>/stream_preview", data = "<data>")]
pub async fn set_preview(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataStreamPreview>,
) -> Result<EmptyResponse> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let channel = target.as_channel(db).await?;
    if channel.voice().is_none() {
        return Err(create_error!(NotAVoiceChannel));
    }

    let sharing = get_voice_state(&UserVoiceChannel::from_channel(&channel), &user.id)
        .await?
        .is_some_and(|state| state.screensharing);
    if !sharing {
        return Err(create_error!(NotConnected));
    }

    if !set_stream_preview(channel.id(), &user.id, &data.image).await? {
        return Err(create_error!(NoEffect));
    }
    Ok(EmptyResponse)
}

/// # Fetch Stream Preview
///
/// The latest still of someone's screen share in a voice channel you can see.
#[openapi(tag = "Voice")]
#[get("/<target>/stream_preview/<member>")]
pub async fn fetch_preview(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<Json<v0::StreamPreview>> {
    let channel = target.as_channel(db).await?;
    let mut query = perms(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    // seeing someone's screen is like being in the call: Listen, and not
    // server-deafened
    permissions.throw_if_lacking_channel_permission(ChannelPermission::Listen)?;
    if let Some(server) = channel.server() {
        if !db.fetch_member(server, &user.id).await?.can_receive {
            return Err(create_error!(MissingPermission {
                permission: ChannelPermission::Listen.to_string()
            }));
        }
    }
    // a mature channel's screen is for adults only, like the channel
    revolt_database::util::age::assert_can_view_channel(db, &user, &channel).await?;

    // only while they still share their screen there
    let sharing = get_voice_state(&UserVoiceChannel::from_channel(&channel), member.id)
        .await?
        .is_some_and(|state| state.screensharing);
    if !sharing {
        return Err(create_error!(NotFound));
    }

    get_stream_preview(channel.id(), member.id)
        .await?
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}
