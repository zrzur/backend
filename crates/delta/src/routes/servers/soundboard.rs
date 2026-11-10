//! A server's soundboard: short sounds members play into a voice call.
//!
//! Files come through autumn (tag `sounds`, audio only, size-limited there),
//! like emojis. Playing one is `channels/soundboard_play.rs`.
use revolt_database::{
    server_supercharge_level, soundboard_limit,
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    AuditLogEntryAction, Database, File, Metadata, PartialSoundboardSound, SoundboardSound, User,
    SOUNDBOARD_MAX_DURATION_MS,
};
use revolt_models::v0;
use revolt_permissions::{calculate_server_permissions, ChannelPermission, PermissionQuery};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

async fn assert_can_manage(db: &Database, user: &User, server: &revolt_database::Server) -> Result<()> {
    let mut query = DatabasePermissionQuery::new(db, user).server(server);
    calculate_server_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageSoundboard)
}

/// # Fetch Soundboard
///
/// The sounds of a server and how many it may have.
#[openapi(tag = "Server Customisation")]
#[get("/<target>/soundboard")]
pub async fn list_sounds(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::SoundboardList>> {
    let server = target.as_server(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
    if !query.are_we_a_member().await {
        return Err(create_error!(NotFound));
    }

    let sounds = db.fetch_soundboard_sounds(&server.id).await?;
    Ok(Json(v0::SoundboardList {
        sounds: sounds.into_iter().map(Into::into).collect(),
        limit: soundboard_limit(server_supercharge_level(&server).await),
    }))
}

/// # Add Sound
///
/// Add a sound by its Autumn upload id (tag `sounds`), which becomes its id. Needs Manage
/// Soundboard; a server has 8 sounds, and 8 more per Wings level.
#[openapi(tag = "Server Customisation")]
#[put("/<target>/soundboard/<sound_id>", data = "<data>")]
pub async fn create_sound(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    sound_id: String,
    data: Json<v0::DataCreateSoundboardSound>,
) -> Result<Json<v0::SoundboardSound>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let server = target.as_server(db).await?;
    assert_can_manage(db, &user, &server).await?;

    let max = soundboard_limit(server_supercharge_level(&server).await);
    if db.count_soundboard_sounds(&server.id).await? >= max {
        return Err(create_error!(TooManySounds { max }));
    }

    let file = File::use_soundboard_sound(db, &sound_id, &server.id, &user.id).await?;

    // the length autumn measured wins over the one the client sends
    let measured = match file.metadata {
        Metadata::Audio {
            duration_ms: Some(duration_ms),
        } => Some(duration_ms),
        _ => None,
    };
    if measured.is_some_and(|ms| ms > SOUNDBOARD_MAX_DURATION_MS) {
        db.mark_attachment_as_deleted(&sound_id).await?;
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("duration_ms", "too_long"),
        ]));
    }

    let sound = SoundboardSound {
        id: sound_id,
        server: server.id.clone(),
        creator_id: user.id.clone(),
        name: data.name.trim().to_string(),
        emoji: data.emoji,
        duration_ms: measured.unwrap_or(data.duration_ms).min(SOUNDBOARD_MAX_DURATION_MS),
        volume: data.volume.unwrap_or(100),
    };
    if !sound.create(db, max).await? {
        db.mark_attachment_as_deleted(&sound.id).await?;
        return Err(create_error!(TooManySounds { max }));
    }
    AuditLogEntryAction::SoundboardSoundCreate {
        sound: sound.id.clone(),
        name: sound.name.clone(),
    }
    .insert(db, server.id.clone(), reason, user.id, None)
    .await;
    Ok(Json(sound.into()))
}

/// # Edit Sound
///
/// Rename a sound, change its emoji or volume (needs Manage Soundboard).
#[openapi(tag = "Server Customisation")]
#[patch("/<target>/soundboard/<sound_id>", data = "<data>")]
pub async fn edit_sound(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    sound_id: String,
    data: Json<v0::DataEditSoundboardSound>,
) -> Result<Json<v0::SoundboardSound>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let server = target.as_server(db).await?;
    assert_can_manage(db, &user, &server).await?;

    let mut sound = db.fetch_soundboard_sound(&sound_id).await?;
    if sound.server != server.id {
        return Err(create_error!(NotFound));
    }
    sound
        .update(
            db,
            PartialSoundboardSound {
                name: data.name.map(|name| name.trim().to_string()),
                emoji: data.emoji,
                volume: data.volume,
                ..Default::default()
            },
        )
        .await?;
    Ok(Json(sound.into()))
}

/// # Remove Sound
///
/// Remove a sound and its file (needs Manage Soundboard).
#[openapi(tag = "Server Customisation")]
#[delete("/<target>/soundboard/<sound_id>")]
pub async fn delete_sound(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    sound_id: String,
) -> Result<EmptyResponse> {
    let server = target.as_server(db).await?;
    assert_can_manage(db, &user, &server).await?;

    let sound = db.fetch_soundboard_sound(&sound_id).await?;
    if sound.server != server.id {
        return Err(create_error!(NotFound));
    }
    let (id, name) = (sound.id.clone(), sound.name.clone());
    sound.delete(db).await?;
    AuditLogEntryAction::SoundboardSoundDelete { sound: id, name }
        .insert(db, server.id.clone(), reason, user.id, None)
        .await;
    Ok(EmptyResponse)
}
