//! A server's soundboard: short sounds members play into a voice call.
//!
//! Files come through autumn (tag `sounds`, audio only, size-limited there),
//! like emojis. Playing one is `channels/soundboard_play.rs`.
use revolt_database::{
    server_supercharge_level, sound_library::library_sound, soundboard_limit,
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

    add_to_soundboard(
        db,
        user,
        reason,
        &server,
        max,
        sound_id,
        NewSound {
            name: data.name.trim().to_string(),
            emoji: data.emoji,
            volume: data.volume,
            library_id: None,
        },
    )
    .await
    .map(Json)
}

/// What a new sound is called and how it plays
struct NewSound {
    name: String,
    emoji: Option<String>,
    volume: Option<u8>,
    library_id: Option<String>,
}

/// Make a file (tag `sounds`, not used yet) a sound of the server: the
/// measured length is checked, the limit is checked again after saving, and
/// the server and the audit log hear of it. Uploads and the library both
/// come through here.
async fn add_to_soundboard(
    db: &Database,
    user: User,
    reason: AuditLogReason,
    server: &revolt_database::Server,
    max: usize,
    sound_id: String,
    new: NewSound,
) -> Result<v0::SoundboardSound> {
    let file = File::use_soundboard_sound(db, &sound_id, &server.id, &user.id).await?;

    // the length autumn measured wins over the one the client sends
    let measured = match file.metadata {
        Metadata::Audio {
            duration_ms: Some(duration_ms),
        } => Some(duration_ms),
        _ => None,
    };
    // a sound autumn could not measure is refused, whatever the client says
    let refusal = match measured {
        None => Some("not_measured"),
        Some(ms) if ms > SOUNDBOARD_MAX_DURATION_MS => Some("too_long"),
        Some(_) => None,
    };
    if let Some(code) = refusal {
        db.mark_attachment_as_deleted(&sound_id).await?;
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("duration_ms", code),
        ]));
    }
    let measured = measured.unwrap_or_default();

    let sound = SoundboardSound {
        id: sound_id,
        server: server.id.clone(),
        creator_id: user.id.clone(),
        name: new.name,
        emoji: new.emoji,
        duration_ms: measured,
        volume: new.volume.unwrap_or(100),
        library_id: new.library_id,
    };
    if !sound.create(db, max).await? {
        db.mark_attachment_as_deleted(&sound.id).await?;
        return Err(create_error!(TooManySounds { max }));
    }

    // two people adding the same library sound at once: the first one stays
    if let Some(library_id) = &sound.library_id {
        let first = db
            .fetch_soundboard_sounds(&server.id)
            .await?
            .into_iter()
            .find(|other| other.library_id.as_ref() == Some(library_id))
            .map(|other| other.id);
        if first.as_ref() != Some(&sound.id) {
            sound.delete(db).await?;
            return Err(create_error!(SoundAlreadyAdded));
        }
    }

    AuditLogEntryAction::SoundboardSoundCreate {
        sound: sound.id.clone(),
        name: sound.name.clone(),
    }
    .insert(db, server.id.clone(), reason, user.id, None)
    .await;
    Ok(sound.into())
}

/// # Add Sound from the Library
///
/// Add a ready-made sound of the sound library (`GET /soundboard/library`).
/// Needs Manage Soundboard and counts like an upload; each library sound is
/// on a server at most once. The name is the library name in your language
/// unless you give one.
#[openapi(tag = "Server Customisation")]
#[put("/<target>/soundboard/library/<library_id>", data = "<data>")]
pub async fn add_library_sound(
    db: &State<Database>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    library_id: String,
    data: Json<v0::DataAddLibrarySound>,
) -> Result<Json<v0::SoundboardSound>> {
    let data = data.into_inner();
    data.validate()
        .map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let server = target.as_server(db).await?;
    assert_can_manage(db, &user, &server).await?;
    let library = library_sound(&library_id).ok_or_else(|| create_error!(NotFound))?;

    // checked before a file is made, so a refusal leaves nothing behind
    let max = soundboard_limit(server_supercharge_level(&server).await);
    let sounds = db.fetch_soundboard_sounds(&server.id).await?;
    if sounds
        .iter()
        .any(|sound| sound.library_id.as_deref() == Some(library.id.as_str()))
    {
        return Err(create_error!(SoundAlreadyAdded));
    }
    if sounds.len() >= max {
        return Err(create_error!(TooManySounds { max }));
    }

    // autumn stores every library sound at start
    let hash = db
        .fetch_attachment_hash(&library.sha256)
        .await
        .ok()
        .filter(|hash| !hash.iv.is_empty())
        .ok_or_else(|| {
            log::error!("library sound {} is not stored yet: is autumn running?", library.id);
            create_error!(InternalError)
        })?;
    let file = library.new_file(&hash, &user.id);
    db.insert_attachment(&file).await?;

    let name = data
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| library.name_in(user.locale.as_deref()).to_string());
    add_to_soundboard(
        db,
        user,
        reason,
        &server,
        max,
        file.id,
        NewSound {
            name,
            emoji: data.emoji.or_else(|| Some(library.emoji.clone())),
            volume: data.volume,
            library_id: Some(library.id.clone()),
        },
    )
    .await
    .map(Json)
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
