//! Voice clips and recorded rooms.
//!
//! The server records nothing. The app of the person who clips or records
//! puts the audio together on their own device, from the voices of the
//! people who agreed, and uploads the finished file like any attachment.
//! These routes keep what the room has to know and what the law asks for:
//!
//! - everyone who sees the room is told when clips are on or a recording
//!   runs (`VoiceCaptureUpdate`, the whole state each time);
//! - a voice is included only by its owner's choice (`PUT .../consent`;
//!   with `consent = "opt_in"`, the default, nobody is included before they
//!   said yes), and the choice can be changed at any moment;
//! - a clip or an episode that names someone who did not agree is refused,
//!   and the people in it are stored with it, so each of them can delete it;
//! - what a month allows: clips per person (more with Feather), recording
//!   time per server by Wings level.
//!
//! Server voice rooms only: nothing here works in direct or group calls.
use revolt_config::{config, FeaturesVoiceCapture, VoiceCaptureConsent};
use revolt_database::{
    remove_voice_capture,
    util::{idempotency::IdempotencyKey, permissions::perms, reference::Reference},
    voice::{
        clear_capture_recording, get_capture_recording, get_capture_state, get_recording_voices,
        is_in_voice_channel, note_recording_voices, publish_capture_state, set_capture_clips,
        set_capture_consent, start_capture_recording, UserVoiceChannel,
    },
    voice_capture_allowance, Channel, Database, File, Server, User, AMQP,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use ulid::Ulid;
use validator::Validate;

/// Seconds a client's clock and upload may be off by
const SLACK_SECS: u32 = 5;
/// Most clips and episodes one answer lists
const LIST_LIMIT: usize = 50;

struct Room {
    channel: Channel,
    server: Server,
    permissions: PermissionValue,
    seat: UserVoiceChannel,
}

/// The voice room of a server, if the user can see it
async fn room_of(db: &Database, user: &User, target: &Reference<'_>) -> Result<Room> {
    let channel = target.as_channel(db).await?;
    let mut query = perms(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    let Some(server_id) = channel.server() else {
        // direct and group calls are never clipped or recorded
        return Err(create_error!(InvalidOperation));
    };
    if channel.voice().is_none() {
        return Err(create_error!(NotAVoiceChannel));
    }
    let server = db.fetch_server(server_id).await?;
    let seat = UserVoiceChannel::from_channel(&channel);
    Ok(Room {
        channel,
        server,
        permissions,
        seat,
    })
}

fn feature_off(feature: &str) -> revolt_result::Error {
    create_error!(FeatureDisabled {
        feature: feature.to_string()
    })
}

async fn settings() -> (bool, bool, FeaturesVoiceCapture) {
    let config = config().await;
    (
        config.features.voice_clips_enabled,
        config.features.voice_recordings_enabled,
        config.features.voice_capture,
    )
}

fn opt_in(settings: &FeaturesVoiceCapture) -> bool {
    settings.consent == VoiceCaptureConsent::OptIn
}

async fn require_in_room(user: &User, room: &Room) -> Result<()> {
    if is_in_voice_channel(&user.id, &room.seat).await? {
        Ok(())
    } else {
        Err(create_error!(NotConnected))
    }
}

/// The uploaded audio: the user's own, not used yet, audio, within `max`
async fn audio_file(db: &Database, user: &User, file_id: &str, max: Option<usize>) -> Result<File> {
    let file = db.fetch_attachment("attachments", file_id).await?;
    if file.uploader_id.as_deref() != Some(user.id.as_str())
        || file.used_for.is_some()
        || file.deleted == Some(true)
    {
        return Err(create_error!(NotFound));
    }
    let kind = file.content_type.as_str();
    if !(kind.starts_with("audio/") || kind == "application/ogg" || kind == "video/webm") {
        return Err(create_error!(FileTypeNotAllowed));
    }
    if let Some(max) = max {
        if file.size as usize > max {
            return Err(create_error!(FileTooLarge { max }));
        }
    }
    Ok(file)
}

/// Post the file as a message of the user, with every check a message has
async fn share(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    channel_id: &str,
    file_id: &str,
    content: Option<String>,
) -> Result<v0::Message> {
    let send = v0::DataMessageSend {
        nonce: None,
        content,
        attachments: Some(vec![file_id.to_string()]),
        replies: None,
        embeds: None,
        masquerade: None,
        interactions: None,
        components: None,
        interaction: None,
        poll: None,
        flags: None,
    };
    super::message_send::send_inner(
        db,
        amqp,
        user,
        Reference::from_unchecked(channel_id),
        send,
        IdempotencyKey::unchecked_from_string(Ulid::new().to_string()),
        false,
        None,
    )
    .await
    .map(|message| message.into_inner())
}

/// # Fetch Capture State
///
/// Whether clips are on or a recording runs in this voice room, who is
/// included, and what you and the server may still do this month.
#[openapi(tag = "Voice")]
#[get("/<target>/capture")]
pub async fn fetch_capture(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::VoiceCaptureInfo>> {
    let (clips, recordings, settings) = settings().await;
    if !clips && !recordings {
        return Err(feature_off("voice_capture"));
    }
    let room = room_of(db, &user, &target).await?;
    Ok(Json(v0::VoiceCaptureInfo {
        state: get_capture_state(&room.seat, opt_in(&settings)).await?,
        allowance: voice_capture_allowance(db, &user, &room.server).await?,
    }))
}

/// # Switch Clips
///
/// Turn clips on or off in a voice room you are in (needs Manage Channel).
/// Everyone who sees the room is told.
#[openapi(tag = "Voice")]
#[put("/<target>/capture/clips", data = "<data>")]
pub async fn switch_clips(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataVoiceClipsSwitch>,
) -> Result<Json<v0::VoiceCaptureState>> {
    let (clips, _, settings) = settings().await;
    if !clips {
        return Err(feature_off("voice_clips"));
    }
    let room = room_of(db, &user, &target).await?;
    room.permissions
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;
    // turning clips off works from outside the room too
    if data.on {
        require_in_room(&user, &room).await?;
    }

    set_capture_clips(room.channel.id(), data.on.then_some(user.id.as_str())).await?;
    let state = get_capture_state(&room.seat, opt_in(&settings)).await?;
    publish_capture_state(room.channel.id(), state.clone()).await;
    Ok(Json(state))
}

/// # Set Capture Consent
///
/// Choose whether your voice may be in clips and in the recording of the
/// voice room you are in. You can change it at any moment; it holds until
/// you leave the room.
#[openapi(tag = "Voice")]
#[put("/<target>/capture/consent", data = "<data>")]
pub async fn set_consent(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataVoiceCaptureConsent>,
) -> Result<Json<v0::VoiceCaptureState>> {
    let (clips, recordings, settings) = settings().await;
    if !clips && !recordings {
        return Err(feature_off("voice_capture"));
    }
    let room = room_of(db, &user, &target).await?;
    require_in_room(&user, &room).await?;

    set_capture_consent(room.channel.id(), &user.id, data.include).await?;
    if data.include && get_capture_recording(room.channel.id()).await?.is_some() {
        note_recording_voices(room.channel.id(), std::slice::from_ref(&user.id)).await?;
    }
    let state = get_capture_state(&room.seat, opt_in(&settings)).await?;
    publish_capture_state(room.channel.id(), state.clone()).await;
    Ok(Json(state))
}

/// # Share Clip
///
/// Share a clip of the voice room you are in. Upload the audio first (tag
/// `attachments`), then name it here with the people whose voice is in it:
/// every one of them must be included right now. Clips must be on in the
/// room. The clip is posted as your message in the room's chat or in
/// another channel of the same server.
#[openapi(tag = "Voice")]
#[post("/<target>/capture/clips", data = "<data>")]
pub async fn share_clip(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataVoiceClip>,
) -> Result<Json<v0::VoiceCapture>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (clips, _, settings) = settings().await;
    if !clips {
        return Err(feature_off("voice_clips"));
    }
    let room = room_of(db, &user, &target).await?;
    require_in_room(&user, &room).await?;

    let state = get_capture_state(&room.seat, opt_in(&settings)).await?;
    if state.clips.is_none() {
        return Err(create_error!(InvalidOperation));
    }
    if data.duration == 0 || data.duration > settings.clip_seconds + SLACK_SECS {
        return Err(create_error!(FailedValidation {
            error: format!("a clip is at most {} seconds long", settings.clip_seconds)
        }));
    }
    if data.included.iter().any(|id| !state.included.contains(id)) {
        return Err(create_error!(VoiceNotAgreed));
    }

    let allowance = voice_capture_allowance(db, &user, &room.server).await?;
    if allowance.clips_left == 0 {
        return Err(create_error!(TooManyVoiceClips {
            max: allowance.clips_per_month
        }));
    }

    // only inside the server the room is in
    let share_in = match &data.share_in {
        Some(id) if id != room.channel.id() => {
            let other = Reference::from_unchecked(id).as_channel(db).await?;
            if other.server() != Some(room.server.id.as_str()) {
                return Err(create_error!(InvalidOperation));
            }
            other.id().to_string()
        }
        _ => room.channel.id().to_string(),
    };

    let file = audio_file(db, &user, &data.file, Some(settings.clip_max_bytes)).await?;
    let author = user.id.clone();
    let message = share(db, amqp, user, &share_in, &file.id, None).await?;

    let mut included = data.included;
    included.sort();
    included.dedup();
    let capture = v0::VoiceCapture {
        id: Ulid::new().to_string(),
        kind: v0::VoiceCaptureKind::Clip,
        channel: room.channel.id().to_string(),
        server: room.server.id.clone(),
        author,
        file: file.id,
        shared_in: share_in,
        message: message.id,
        duration: data.duration.min(settings.clip_seconds),
        included,
        title: None,
        removed: false,
    };
    db.insert_voice_capture(&capture).await?;
    Ok(Json(capture))
}

/// # Start Recording
///
/// Start recording the voice room you are in as an episode (needs Manage
/// Channel). Everyone who sees the room is told; a voice is included only
/// by its owner's choice. The server needs recording time left this month
/// (by its Wings level).
#[openapi(tag = "Voice")]
#[post("/<target>/capture/recording", data = "<data>")]
pub async fn start_recording(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataVoiceRecordingStart>,
) -> Result<Json<v0::VoiceCaptureState>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (_, recordings, settings) = settings().await;
    if !recordings {
        return Err(feature_off("voice_recordings"));
    }
    let room = room_of(db, &user, &target).await?;
    room.permissions
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;
    require_in_room(&user, &room).await?;

    let allowance = voice_capture_allowance(db, &user, &room.server).await?;
    if allowance.recording_seconds_left == 0 {
        return Err(create_error!(RecordingTimeUsedUp {
            max: allowance.recording_seconds_per_month
        }));
    }

    let title = data
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);
    let longest = allowance
        .recording_seconds_left
        .min(u64::from(allowance.episode_max_seconds));
    let recording = v0::VoiceRecordingLive {
        by: user.id.clone(),
        started_at: now_ms(),
        title,
    };
    // kept a little longer than the episode may be, so it can be saved
    if !start_capture_recording(room.channel.id(), &recording, longest + 600).await? {
        return Err(create_error!(InvalidOperation));
    }

    let state = get_capture_state(&room.seat, opt_in(&settings)).await?;
    note_recording_voices(room.channel.id(), &state.included).await?;
    publish_capture_state(room.channel.id(), state.clone()).await;
    Ok(Json(state))
}

/// # Stop Recording
///
/// End the recording without saving it. The person who started it or
/// anyone with Manage Channel can.
#[openapi(tag = "Voice")]
#[delete("/<target>/capture/recording")]
pub async fn stop_recording(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::VoiceCaptureState>> {
    let (_, recordings, settings) = settings().await;
    if !recordings {
        return Err(feature_off("voice_recordings"));
    }
    let room = room_of(db, &user, &target).await?;
    let Some(recording) = get_capture_recording(room.channel.id()).await? else {
        return Err(create_error!(InvalidOperation));
    };
    if recording.by != user.id {
        room.permissions
            .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;
    }

    clear_capture_recording(room.channel.id()).await?;
    let state = get_capture_state(&room.seat, opt_in(&settings)).await?;
    publish_capture_state(room.channel.id(), state.clone()).await;
    Ok(Json(state))
}

/// # Finish Recording
///
/// End the recording you started and save it as an episode. Upload the
/// audio first (tag `attachments`), then name it here with everyone whose
/// voice is in it: each of them must have been included while it ran. The
/// episode is posted as your message in the room's chat.
#[openapi(tag = "Voice")]
#[post("/<target>/capture/recording/finish", data = "<data>")]
pub async fn finish_recording(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataVoiceRecordingFinish>,
) -> Result<Json<v0::VoiceCapture>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let (_, recordings, settings) = settings().await;
    if !recordings {
        return Err(feature_off("voice_recordings"));
    }
    let room = room_of(db, &user, &target).await?;
    let Some(recording) = get_capture_recording(room.channel.id()).await? else {
        return Err(create_error!(InvalidOperation));
    };
    if recording.by != user.id {
        return Err(create_error!(InvalidOperation));
    }

    // no longer than it ran, than an episode may be, or than the month has left
    let ran = ((now_ms() - recording.started_at).max(0) / 1000) as u64;
    let allowance = voice_capture_allowance(db, &user, &room.server).await?;
    let longest = ran
        .min(u64::from(allowance.episode_max_seconds))
        .min(allowance.recording_seconds_left);
    if data.duration == 0 || u64::from(data.duration) > longest + u64::from(SLACK_SECS) {
        return Err(create_error!(FailedValidation {
            error: format!("this recording can be at most {longest} seconds long")
        }));
    }

    let (ever, no) = get_recording_voices(room.channel.id()).await?;
    let agreed = |id: &String| {
        if opt_in(&settings) {
            ever.contains(id)
        } else {
            !no.contains(id)
        }
    };
    if !data.included.iter().all(agreed) {
        return Err(create_error!(VoiceNotAgreed));
    }

    let file = audio_file(db, &user, &data.file, None).await?;
    let author = user.id.clone();
    let message = share(
        db,
        amqp,
        user,
        room.channel.id(),
        &file.id,
        recording.title.clone(),
    )
    .await?;

    let mut included = data.included;
    included.sort();
    included.dedup();
    let capture = v0::VoiceCapture {
        id: Ulid::new().to_string(),
        kind: v0::VoiceCaptureKind::Episode,
        channel: room.channel.id().to_string(),
        server: room.server.id.clone(),
        author,
        file: file.id,
        shared_in: room.channel.id().to_string(),
        message: message.id,
        // the length checked above (it may run a few seconds past the
        // server's own clock); clamping to `longest` counted a short episode as 0
        duration: data.duration.min((longest + u64::from(SLACK_SECS)) as u32),
        included,
        title: recording.title,
        removed: false,
    };
    db.insert_voice_capture(&capture).await?;

    clear_capture_recording(room.channel.id()).await?;
    publish_capture_state(
        room.channel.id(),
        get_capture_state(&room.seat, opt_in(&settings)).await?,
    )
    .await;
    Ok(Json(capture))
}

/// # List Clips and Episodes
///
/// The clips and episodes made in this voice room, newest first.
#[openapi(tag = "Voice")]
#[get("/<target>/capture/items")]
pub async fn list_captures(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<v0::VoiceCapture>>> {
    let (clips, recordings, _) = settings().await;
    if !clips && !recordings {
        return Err(feature_off("voice_capture"));
    }
    let room = room_of(db, &user, &target).await?;
    let mut items = db
        .fetch_voice_captures_in_channel(room.channel.id(), LIST_LIMIT)
        .await?;

    // a clip whose message was deleted the usual way is gone as well
    let message_ids: Vec<String> = items.iter().map(|item| item.message.clone()).collect();
    let alive: std::collections::HashSet<String> = db
        .fetch_messages_by_id(&message_ids)
        .await?
        .into_iter()
        .map(|message| message.id)
        .collect();
    for item in items.iter().filter(|item| !alive.contains(&item.message)) {
        db.remove_voice_capture(&item.id).await?;
    }
    items.retain(|item| alive.contains(&item.message));

    // one shared in a channel this person cannot read is not theirs to see
    let mut readable = std::collections::HashMap::new();
    readable.insert(room.channel.id().to_string(), true);
    let mut kept = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        let can_read = match readable.get(&item.shared_in) {
            Some(can_read) => *can_read,
            None => {
                let can_read = match db.fetch_channel(&item.shared_in).await {
                    Ok(channel) => {
                        let mut query = perms(db, &user).channel(&channel);
                        calculate_channel_permissions(&mut query)
                            .await
                            .has_channel_permission(ChannelPermission::ViewChannel)
                    }
                    Err(_) => false,
                };
                readable.insert(item.shared_in.clone(), can_read);
                can_read
            }
        };
        if can_read || item.author == user.id || item.included.contains(&user.id) {
            kept.push(item);
        }
    }
    Ok(Json(kept))
}

/// # Delete Clip or Episode
///
/// Remove a clip or an episode with its message and its file. Whoever
/// saved it, everyone whose voice is in it, and people with Manage
/// Messages in the room can.
#[openapi(tag = "Voice")]
#[delete("/<target>/capture/items/<id>")]
pub async fn delete_capture(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    id: String,
) -> Result<EmptyResponse> {
    let room = room_of(db, &user, &target).await?;
    let capture = db.fetch_voice_capture(&id).await?;
    if capture.channel != room.channel.id() || capture.removed {
        return Err(create_error!(NotFound));
    }
    if capture.author != user.id && !capture.included.contains(&user.id) {
        room.permissions
            .throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
    }
    remove_voice_capture(db, &capture).await?;
    Ok(EmptyResponse)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}
