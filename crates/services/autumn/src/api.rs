use std::{
    io::{Cursor, Read, Write},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, Method, header},
    middleware::map_response,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_typed_multipart::{FieldData, TryFromMultipart, TypedMultipart};
use lazy_static::lazy_static;
use revolt_config::{config, report_internal_error, CorsOrigins, FilesLimit};
use revolt_database::{
    CommunityFileAccess, CommunityImageError, CommunityImageLimits, Database, FileHash, Metadata,
    PlatformPermission, User, check_effect_image, community_file_access, inspect_community_image,
    iso8601_timestamp::Timestamp,
};
use revolt_files::{
    AUTHENTICATION_TAG_SIZE_BYTES, create_thumbnail, decode_image, fetch_from_s3, is_animated,
    upload_to_s3,
};
use revolt_result::{Error, Result, ToRevoltError, create_error};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use stoat_otel::WideContextRepository;
use tempfile::NamedTempFile;
use tokio::time::Instant;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use url_escape::encode_component;
use utoipa::ToSchema;

use crate::{
    AppState,
    cache::{PRIVATE_CACHE_CONTROL, file_cache_control, seal_response},
    effects::{check_upload, still_of, strip_hidden_data},
    webm::{WebmLimits, convert_webm, is_webm},
    exif::strip_metadata, metadata::generate_metadata, mime_type::determine_mime_type,
    preview::PREVIEWS,
};

/// Which websites may read answers in a browser (S-02)
fn cors_layer(origins: CorsOrigins) -> CorsLayer {
    CorsLayer::new()
        .allow_methods([Method::GET, Method::HEAD, Method::POST])
        .allow_headers(AllowHeaders::mirror_request())
        .expose_headers(vec![
            "X-RateLimit-Limit".try_into().unwrap(),
            "X-RateLimit-Bucket".try_into().unwrap(),
            "X-RateLimit-Remaining".try_into().unwrap(),
            "X-RateLimit-Reset-After".try_into().unwrap(),
        ])
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin.to_str().is_ok_and(|origin| origins.allows(origin))
        }))
}

/// Build the API router
pub async fn router() -> Router<AppState> {
    let config = config().await;

    let cors = cors_layer(CorsOrigins::from_settings(
        &config.hosts.app,
        &config.api.security.staff_origins,
    ));

    Router::new()
        .route("/", get(root))
        .route(
            "/:tag",
            post(upload_file)
                .options(options)
                .layer(DefaultBodyLimit::max(
                    config.features.limits.global.body_limit_size,
                )),
        )
        .route(
            "/webhooks/:webhook_id/attachments",
            post(upload_webhook_file).layer(DefaultBodyLimit::max(
                config.features.limits.global.body_limit_size,
            )),
        )
        .route("/:tag/:file_id", get(fetch_preview))
        .route("/:tag/:file_id/:file_name", get(fetch_file))
        .route("/mod/:tag/:file_id/:file_name", get(fetch_file_mod))
        .layer(map_response(seal_response))
        .layer(cors)
}

lazy_static! {
    /// Short-lived file cache to allow us to populate different CDN regions without increasing bandwidth to S3 provider
    /// Uploads will also be stored here to prevent immediately queued downloads from doing the entire round-trip
    static ref S3_CACHE: moka::future::Cache<String, Result<Vec<u8>>> = moka::future::Cache::builder()
        .weigher(|_key, value: &Result<Vec<u8>>| -> u32 {
            std::mem::size_of::<Result<Vec<u8>>>() as u32 + if let Ok(vec) = value {
                vec.len().try_into().unwrap_or(u32::MAX)
            } else {
                std::mem::size_of::<Error>() as u32
            }
        })
        // TODO config
        // .max_capacity(1024 * 1024 * 1024) // Cache up to 1GiB in memory
        // .max_capacity(512 * 1024 * 1024) // Cache up to 512MiB in memory
        .max_capacity(2 * 1024 * 1024 * 1024) // Cache up to 2GiB in memory
        .time_to_live(Duration::from_secs(5 * 60)) // For up to 5 minutes
        .build();
}

/// Retrieve hash information and file data by given hash
async fn retrieve_file_by_hash(hash: &FileHash) -> Result<Vec<u8>> {
    if let Some(data) = S3_CACHE.get(&hash.id).await {
        data
    } else {
        let data = fetch_from_s3(&hash.bucket_id, &hash.path, &hash.iv).await;
        S3_CACHE.insert(hash.id.to_owned(), data.clone()).await;
        data
    }
}

/// Read an image and create its preview, away from the threads that answer requests
async fn render_preview(
    original: Vec<u8>,
    content_type: String,
    tag: &'static str,
) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let image = decode_image(&mut Cursor::new(original), &content_type)?;
        Ok(runtime.block_on(create_thumbnail(image, tag)))
    })
    .await
    .map_err(|_| create_error!(InternalError))?
}

/// Tags whose preview of an animated file is one still frame
pub(crate) fn previews_a_still(tag: &Tag) -> bool {
    matches!(tag, Tag::avatars | Tag::icons | Tag::effects | Tag::decorations)
}

/// To the original file. A design can be taken down, so nothing may remember
/// this answer for it.
pub(crate) fn to_original(tag: &Tag, path: &str) -> Redirect {
    if matches!(tag, Tag::effects | Tag::decorations) {
        Redirect::temporary(path)
    } else {
        Redirect::permanent(path)
    }
}

/// The frame of a design with the most on it, as a preview
pub(crate) async fn render_design_still(original: Vec<u8>, tag: &'static str) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let still = still_of(&original).ok_or_else(|| create_error!(InternalError))?;
        Ok(runtime.block_on(create_thumbnail(still, tag)))
    })
    .await
    .map_err(|_| create_error!(InternalError))?
}

/// Successful root response
#[derive(Serialize, Debug, ToSchema)]
pub struct RootResponse {
    autumn: &'static str,
    version: &'static str,
}

/// Capture crate version from Cargo
static CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root response from service
#[utoipa::path(
    get,
    path = "/",
    tag = "Files",
    responses(
        (status = 200, description = "Echo response", body = RootResponse)
    )
)]
async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        autumn: "Hello, I am a file server!",
        version: CRATE_VERSION,
    })
}

/// Empty handler for OPTIONS routes
async fn options() {}

/// Available tags to upload to
#[derive(Clone, Deserialize, Debug, ToSchema, strum_macros::IntoStaticStr)]
#[allow(non_camel_case_types)]
pub enum Tag {
    attachments,
    avatars,
    backgrounds,
    icons,
    banners,
    emojis,
    /// Community avatar decorations and nameplates
    decorations,
    /// Community profile effects
    effects,
    /// Soundboard sounds (short audio clips)
    sounds,
}

/// Request body for upload
#[derive(ToSchema, TryFromMultipart)]
pub struct UploadPayload {
    #[schema(format = Binary)]
    #[allow(dead_code)]
    #[form_data(limit = "unlimited")] // handled by axum
    file: FieldData<NamedTempFile>,
}

/// Successful upload response
#[derive(Serialize, Debug, ToSchema)]
pub struct UploadResponse {
    /// ID to attach uploaded file to object
    id: String,
}

/// Optional upload context
#[derive(Deserialize, Default)]
struct UploadQuery {
    /// Server the attachment is for: its supercharge level may allow a
    /// bigger file than your own limit
    server: Option<String>,
}

/// Upload a file
///
/// Available tags and restrictions:
///
/// | Tag | Size | Resolution | Type |
/// | :-: | --: | :-- | :-: |
/// | attachments | 20 MB | - | Any |
/// | avatars | 4 MB | 40 MP or 10,000px | Image |
/// | backgrounds | 6 MB | 40 MP or 10,000px | Image |
/// | icons | 2.5 MB | 40 MP or 10,000px | Image |
/// | banners | 6 MB | 40 MP or 10,000px | Image |
/// | emojis | 500 KB | 40 MP or 10,000px | Image |
/// | decorations | 2.5 MB | 40 MP or 10,000px | PNG / APNG / GIF / WebP |
/// | effects | 2 MB | 1024px | Animated, one play, transparent PNG / GIF / WebP |
/// | sounds | 512 KB | - | MP3 / OGG / WAV / WebM audio |
///
/// `decorations` and `effects` also take a WebM of up to 8 MB. It is turned
/// into an animated WebP here and checked as one; the WebM is never stored.
#[utoipa::path(
    post,
    path = "/{tag}",
    tag = "Files",
    responses(
        (status = 200, description = "Upload was successful", body = UploadResponse)
    ),
    params(
        ("tag" = Tag, Path, description = "Tag to upload to (e.g. attachments, icons, ...)")
    ),
    request_body(content_type = "multipart/form-data", content = UploadPayload),
    security(
        ("session_token" = []),
        ("bot_token" = [])
    )
)]
async fn upload_file(
    State(db): State<Database>,
    wide_context: WideContextRepository,
    user: User,
    Path(tag): Path<Tag>,
    Query(query): Query<UploadQuery>,
    TypedMultipart(UploadPayload { file }): TypedMultipart<UploadPayload>,
) -> Result<Json<UploadResponse>> {
    wide_context.add("user_id", user.id.clone());

    // Community designs: only creators granted by staff can upload
    if matches!(tag, Tag::effects) && !config().await.features.community.effects_enabled {
        return Err(create_error!(FileTypeNotAllowed));
    }
    if matches!(tag, Tag::decorations | Tag::effects)
        && !revolt_database::platform_permissions(&db, &user)
            .await?
            .has(PlatformPermission::CosmeticsSubmit)
    {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsSubmit.name().to_string()
        }));
    }

    // Get user's file upload limits
    let limits = user.limits().await;
    let mut size_limit = size_limit_for(&limits.file_upload_size_limit, &tag);

    // A supercharged server lets every member send bigger attachments
    if matches!(tag, Tag::attachments) {
        if let Some(server_id) = query.server.as_deref() {
            if db.fetch_member(server_id, &user.id).await.is_ok() {
                size_limit = size_limit
                    .max(revolt_database::server_supercharge_attachments(&db, server_id).await);
            }
        }
    }

    store_upload(&db, tag, file, &user.id, size_limit).await
}

/// Files a webhook sends with a message. The API forwards them here, with the
/// webhook's token in a header, so they get every check of a person's upload.
async fn upload_webhook_file(
    State(db): State<Database>,
    Path(webhook_id): Path<String>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Result<Json<UploadResponse>> {
    // The webhook and its token are checked before the body is read, so a
    // made-up webhook id costs no disk or memory. An unknown webhook answers
    // like a wrong token (401), which counts towards blocking the address.
    let token = headers
        .get("x-webhook-token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| create_error!(NotAuthenticated))?;
    let webhook = db
        .fetch_webhook(&webhook_id)
        .await
        .map_err(|_| create_error!(NotAuthenticated))?;
    webhook.assert_token(token)?;
    if !webhook.can_upload_files() {
        return Err(create_error!(MissingPermission {
            permission: "UploadFiles".to_string()
        }));
    }
    let TypedMultipart(UploadPayload { file }) =
        <TypedMultipart<UploadPayload> as axum::extract::FromRequest<()>>::from_request(request, &())
            .await
            .map_err(|_| {
                revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
                    "file", "invalid_upload",
                )])
            })?;

    let size_limit = *config()
        .await
        .features
        .limits
        .default
        .file_upload_size_limit
        .get("attachments")
        .expect("size limit");
    store_upload(&db, Tag::attachments, file, &webhook.id, size_limit).await
}

/// Size limit of a tag. A limits table written before soundboard sounds
/// existed has no `sounds` entry: those get the default instead of a panic.
fn size_limit_for(limits: &std::collections::HashMap<String, usize>, tag: &Tag) -> usize {
    let name: &'static str = tag.clone().into();
    match limits.get(name) {
        Some(limit) => *limit,
        None if matches!(tag, Tag::sounds) => SOUND_SIZE_LIMIT,
        None => panic!("size limit"),
    }
}

/// Size limit of a soundboard sound when the config names none
const SOUND_SIZE_LIMIT: usize = 512_000;

/// Audio a soundboard plays in every browser
pub(crate) fn is_sound_type(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "audio/mpeg" | "audio/ogg" | "audio/wav" | "audio/x-wav" | "audio/vnd.wave" | "audio/webm"
    )
}

/// Images whose header claims more pixels than the limits are refused before
/// anything decodes them: a small file can claim a huge picture
fn pixels_allowed(width: usize, height: usize, limit: &FilesLimit) -> bool {
    let side = limit.max_pixel_side;
    let pixels = limit.max_mega_pixels.saturating_mul(1_000_000);
    (side == 0 || (width <= side && height <= side))
        && (pixels == 0 || width.saturating_mul(height) <= pixels)
}

/// Uploads decoded at the same time; the rest wait their turn
const MAX_UPLOAD_DECODES: usize = 4;
static UPLOAD_DECODES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_UPLOAD_DECODES);

/// Check, clean and store one upload
async fn store_upload(
    db: &Database,
    tag: Tag,
    mut file: FieldData<NamedTempFile>,
    uploader: &str,
    size_limit: usize,
) -> Result<Json<UploadResponse>> {
    // Fetch configuration
    let config = config().await;

    // Keep track of processing time
    let now = Instant::now();

    // Extract the filename, or give it a generic name
    let mut filename = file.metadata.file_name.take().unwrap_or("unnamed-file".to_owned());

    // Refuse a file over the limit before it is read into memory (a design's
    // WebM has a limit of its own, checked again below)
    let design_upload = matches!(tag, Tag::decorations | Tag::effects);
    let largest = if design_upload {
        size_limit.max(config.features.community.webm_max_bytes)
    } else {
        size_limit
    };
    let on_disk = report_internal_error!(file.contents.as_file().metadata())?.len();
    if on_disk > largest as u64 {
        return Err(create_error!(FileTooLarge { max: largest }));
    }

    // Load file to memory
    let mut buf = Vec::<u8>::new();
    report_internal_error!(file.contents.read_to_end(&mut buf))?;

    // Take note of original file size
    let original_file_size = buf.len();

    // Ensure the file is not empty
    if original_file_size < config.files.limit.min_file_size {
        return Err(create_error!(FileTooSmall));
    }

    // A WebM for a community design: only the image made from it goes on. It
    // has a limit of its own going in; what comes out has the tag's limit, and
    // every check below sees an ordinary image.
    let community = &config.features.community;
    let design = matches!(tag, Tag::decorations | Tag::effects);
    if design && is_webm(&buf) {
        if original_file_size > community.webm_max_bytes {
            return Err(create_error!(FileTooLarge { max: community.webm_max_bytes }));
        }
        let effect = matches!(tag, Tag::effects);
        let limits = WebmLimits {
            max_dimension: if effect { community.effect_max_dimension } else { community.max_dimension },
            max_frames: if effect { community.effect_max_frames } else { community.max_frames },
            max_duration_ms: if effect { community.effect_max_duration_ms } else { community.max_duration_ms },
            max_output_bytes: size_limit,
            // effects play once; decorations and nameplates keep going
            plays: u16::from(effect),
            timeout: std::time::Duration::from_millis(community.webm_timeout_ms),
        };
        let image = convert_webm(file.contents.path(), &buf, &limits).await.map_err(|refusal| {
            tracing::debug!("webm refused: {refusal:?}");
            revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
                "file",
                refusal.code(),
            )])
        })?;
        let mut stored = report_internal_error!(NamedTempFile::new())?;
        report_internal_error!(stored.write_all(&image))?;
        report_internal_error!(stored.flush())?;
        file.contents = stored;
        filename = format!(
            "{}.webp",
            filename.rsplit_once('.').map_or(filename.as_str(), |(stem, _)| stem)
        );
        buf = image;
    } else if original_file_size > size_limit {
        return Err(create_error!(FileTooLarge { max: size_limit }));
    }
    let original_file_size = buf.len();
    if original_file_size > size_limit {
        return Err(create_error!(FileTooLarge { max: size_limit }));
    }

    // Effects go to everyone who opens a profile: data hidden in the file
    // leaves before anything else sees it, also before the hash, so the file
    // can't be matched to an earlier copy that still has it
    if matches!(tag, Tag::effects) {
        buf = strip_hidden_data(&buf).ok_or_else(|| create_error!(FileTypeNotAllowed))?;
    }

    // Generate sha256 hash
    let original_hash = {
        let mut hasher = sha2::Sha256::new();
        hasher.update(&buf);
        hasher.finalize()
    };

    // Generate an ID for this file (sorted by time for emojis and sounds)
    let id = if matches!(tag, Tag::emojis | Tag::sounds) {
        ulid::Ulid::new().to_string()
    } else {
        nanoid::nanoid!(42)
    };

    // Determine the mime type for the file
    let mime_type = determine_mime_type(&mut file.contents, &buf, &filename);

    // Check blocklist for mime type
    if config
        .files
        .blocked_mime_types
        .iter()
        .any(|m| m == mime_type)
    {
        return Err(create_error!(FileTypeNotAllowed));
    }

    // Read the size from the image header first; nothing decodes an image
    // over the limits. An attachment that big is still sent, as a plain file
    // (no preview); a picture for a profile, server or emoji is refused.
    let mut over_pixel_limit = false;
    if mime_type.starts_with("image/") && mime_type != "image/svg+xml" {
        if let Some((width, height)) = revolt_files::image_size(&file.contents) {
            if !pixels_allowed(width, height, &config.files.limit) {
                if !matches!(tag, Tag::attachments) {
                    return Err(revolt_result::failed_validation(vec![
                        revolt_result::ValidationProblem::new("file", "image_too_large"),
                    ]));
                }
                over_pixel_limit = true;
            }
        }
    }

    // Heavy work (decoding, re-encoding) waits for a free slot and runs away
    // from the threads that answer requests. The slot goes with the work into
    // the blocking task, so a request that is dropped doesn't free it early.
    let decode_slot = UPLOAD_DECODES
        .acquire()
        .await
        .map_err(|_| create_error!(InternalError))?;

    // Determine metadata for the file
    let (contents, metadata, decode_slot) = if over_pixel_limit {
        (file.contents, Metadata::File, decode_slot)
    } else {
        tokio::task::spawn_blocking(move || {
            let metadata = generate_metadata(&file.contents, mime_type);
            (file.contents, metadata, decode_slot)
        })
        .await
        .map_err(|_| create_error!(InternalError))?
    };

    // Soundboard sounds: audio a browser plays, nothing else
    if matches!(tag, Tag::sounds)
        && (!matches!(metadata, Metadata::Audio) || !is_sound_type(mime_type))
    {
        return Err(create_error!(FileTypeNotAllowed));
    }

    // Block non-images for non-attachment uploads
    if !matches!(tag, Tag::attachments | Tag::sounds) && !matches!(metadata, Metadata::Image { .. }) {
        return Err(create_error!(FileTypeNotAllowed));
    }

    // Decorations and nameplates: only formats that can be animated and
    // drawn over an avatar
    if matches!(tag, Tag::decorations | Tag::effects)
        && !matches!(mime_type, "image/png" | "image/apng" | "image/gif" | "image/webp")
    {
        return Err(create_error!(FileTypeNotAllowed));
    }

    // ...and check the real bytes: signature, size, frames and length
    if matches!(tag, Tag::decorations | Tag::effects) {
        let community = &config.features.community;
        let limits = if matches!(tag, Tag::effects) {
            CommunityImageLimits {
                max_bytes: size_limit,
                max_dimension: community.effect_max_dimension,
                max_frames: community.effect_max_frames,
                max_duration_ms: community.effect_max_duration_ms,
            }
        } else {
            CommunityImageLimits {
                max_bytes: size_limit,
                max_dimension: community.max_dimension,
                max_frames: community.max_frames,
                max_duration_ms: community.max_duration_ms,
            }
        };
        let inspected = inspect_community_image(&buf, &limits).and_then(|info| {
            if matches!(tag, Tag::effects) {
                check_effect_image(&info, community.effect_min_frame_ms)?;
            }
            Ok(info)
        });
        let info = match inspected {
            Ok(info) => info,
            Err(CommunityImageError::TooLarge) => {
                return Err(create_error!(FileTooLarge { max: size_limit }));
            }
            Err(CommunityImageError::BadSignature) => {
                return Err(create_error!(FileTypeNotAllowed));
            }
            Err(reason) => {
                tracing::debug!("community image refused: {reason:?}");
                return Err(revolt_result::failed_validation(vec![
                    revolt_result::ValidationProblem::new("file", effect_problem(&tag, &reason)),
                ]));
            }
        };

        // The machine check for flashing, and for an effect readable text.
        // Nothing goes on until it has passed; if it can't run, the file is
        // refused.
        let bytes = buf.clone();
        let upload_tag = tag.clone();
        let checked =
            tokio::task::spawn_blocking(move || check_upload(&upload_tag, &bytes, &info))
                .await
                .map_err(|_| create_error!(InternalError))?;
        if let Err(refusal) = checked {
            tracing::debug!("design refused: {refusal:?}");
            return Err(revolt_result::failed_validation(vec![
                revolt_result::ValidationProblem::new("file", refusal.code()),
            ]));
        }
    }

    // Find an existing hash and use that if possible
    let file_hash_exists = if let Ok(file_hash) = db
        .fetch_attachment_hash(&format!("{original_hash:02x}"))
        .await
    {
        if !file_hash.iv.is_empty() {
            let tag: &'static str = tag.into();
            db.insert_attachment(&file_hash.into_file(
                id.clone(),
                tag.to_owned(),
                filename,
                uploader.to_string(),
            ))
            .await?;

            return Ok(Json(UploadResponse { id }));
        }

        true
    } else {
        false
    };

    // Strip metadata. Decoration PNGs are kept byte for byte: re-encoding
    // would flatten animated PNGs (APNG) into a single frame.
    let (buf, metadata) = if matches!(tag, Tag::decorations | Tag::effects)
        && mime_type.starts_with("image/")
    {
        drop(decode_slot);
        (buf, metadata)
    } else {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _slot = decode_slot;
            runtime.block_on(strip_metadata(contents, buf, metadata, mime_type))
        })
        .await
        .map_err(|_| create_error!(InternalError))??
    };

    // Virus scan files if ClamAV is configured
    if matches!(metadata, Metadata::File)
        && (config.files.scan_mime_types.is_empty()
            || config.files.scan_mime_types.iter().any(|v| v == mime_type))
        && crate::clamav::is_malware(&buf).await?
    {
        return Err(create_error!(InternalError));
    }

    // Print file information for debug purposes
    let new_file_size = buf.len() + AUTHENTICATION_TAG_SIZE_BYTES;
    let processed_hash = {
        let mut hasher = sha2::Sha256::new();
        hasher.update(&buf);
        hasher.finalize()
    };
    let process_ratio = new_file_size as f32 / original_file_size as f32;
    let time_to_process = Instant::now() - now;

    tracing::info!(
        "Received file {filename}\nOriginal hash: {original_hash:02x}\nOriginal size: {original_file_size} bytes\nMime type: {mime_type}\nMetadata: {metadata:?}\nProcessed file size: {new_file_size} bytes ({:.2}%).\nProcessed hash: {processed_hash:02x}\nProcessing took {time_to_process:?}",
        process_ratio * 100.0
    );

    // Create hash entry in database
    let file_hash = FileHash {
        id: format!("{original_hash:02x}"),
        processed_hash: format!("{processed_hash:02x}"),

        created_at: Timestamp::now_utc(),

        bucket_id: config.files.s3.default_bucket,
        path: format!("{original_hash:02x}"),
        iv: String::new(), // indicates file is not uploaded yet

        metadata,
        content_type: mime_type.to_owned(),
        size: new_file_size as isize,
    };

    // Add attachment hash if it doesn't exist
    if !file_hash_exists {
        db.insert_attachment_hash(&file_hash).await?;
    }

    // Upload the file to S3 and commit nonce to database
    let upload_start = Instant::now();
    let nonce = upload_to_s3(&file_hash.bucket_id, &file_hash.id, &buf).await?;
    db.set_attachment_hash_nonce(&file_hash.id, &nonce).await?;

    // Debug information
    let time_to_upload = Instant::now() - upload_start;
    tracing::info!("Took {time_to_upload:?} to upload {new_file_size} bytes to S3.");

    // Finally, create the file and return its ID
    let tag: &'static str = tag.into();
    db.insert_attachment(&file_hash.into_file(
        id.clone(),
        tag.to_owned(),
        filename,
        uploader.to_string(),
    ))
    .await?;

    Ok(Json(UploadResponse { id }))
}

/// The code a refused community image is reported with: effects say why, so
/// the creator reads the reason in their own language
fn effect_problem(tag: &Tag, reason: &CommunityImageError) -> &'static str {
    if !matches!(tag, Tag::effects) {
        return "image";
    }
    match reason {
        CommunityImageError::NotAnimated => "not_animated",
        CommunityImageError::Repeats => "repeats",
        CommunityImageError::FramesTooFast => "too_fast",
        CommunityImageError::WrongShape => "shape",
        CommunityImageError::TooManyFrames => "too_many_frames",
        CommunityImageError::TooLong => "too_long",
        CommunityImageError::TooManyPixels => "too_many_pixels",
        _ => "image",
    }
}

/// Check who may download a `decorations` or `effects` file. Returns whether
/// the file is private (served, but not cacheable); other tags are always
/// public. With effects switched off no effect file is served at all.
async fn community_gate(
    db: &Database,
    tag: &str,
    file: &revolt_database::File,
    viewer: Option<&User>,
) -> Result<bool> {
    if tag != "decorations" && tag != "effects" {
        return Ok(false);
    }
    if tag == "effects" && !config().await.features.community.effects_enabled {
        return Err(create_error!(NotFound));
    }
    match community_file_access(db, file, viewer).await? {
        CommunityFileAccess::Public => Ok(false),
        CommunityFileAccess::Private => Ok(true),
        CommunityFileAccess::Denied => Err(create_error!(NotFound)),
    }
}

/// Fetch preview of file
///
/// This route will only return image content. <br>
/// For all other file types, please use the fetch route (you will receive a redirect if you try to use this route anyways!).
///
/// Depending on the given tag, the file will be re-processed to fit the criteria:
///
/// | Tag | Image Resolution <sup>†</sup> | Animations stripped by preview <sup>‡</sup> |
/// | :-: | --- | :-: |
/// | attachments | Up to 1280px on any axis | ❌ |
/// | avatars | Up to 256px on any axis | ✅ |
/// | backgrounds | Up to 1280x720px | ❌ |
/// | icons | Up to 256px on any axis | ✅ |
/// | banners | Up to 480px on any axis | ❌ |
/// | emojis | Up to 128px on any axis | ❌ |
/// | decorations | Up to 512px on any axis | ✅ |
/// | effects | Up to 256x456px | ✅ |
///
/// <sup>†</sup> aspect ratio will always be preserved
///
/// <sup>‡</sup> to fetch animated variant, suffix `/{file_name}` or `/original` to the path
#[utoipa::path(
    get,
    path = "/{tag}/{file_id}",
    tag = "Files",
    responses(
        (status = 200, description = "Generated preview", body = Vec<u8>)
    ),
    params(
        ("tag" = Tag, Path, description = "Tag to fetch from (e.g. attachments, icons, ...)"),
        ("file_id" = String, Path, description = "File identifier")
    ),
)]
async fn fetch_preview(
    State(db): State<Database>,
    viewer: Option<User>,
    Path((tag, file_id)): Path<(Tag, String)>,
) -> Result<Response> {
    let tag_str: &'static str = tag.clone().into();
    let file = db.fetch_attachment(tag_str, &file_id).await?;

    // Ignore deleted files
    if file.deleted.is_some_and(|v| v) {
        return Err(create_error!(NotFound));
    }

    // Ignore files that haven't been attached
    if file.used_for.is_none() {
        return Err(create_error!(NotFound));
    }

    // Community designs: only the current image of a published design is
    // public; the rest only for its creator and reviewers, never cached
    let private = community_gate(&db, tag_str, &file, viewer.as_ref()).await?;

    let hash = file.as_hash(&db).await?;

    let mut data = None;

    // If animated is unset, check the file contents to see if it is animated and update the filehash
    let is_animated = match &hash.metadata {
        Metadata::Image {
            animated: Some(value),
            ..
        } => *value,
        Metadata::Image { animated: None, .. } => {
            let file_data = retrieve_file_by_hash(&hash).await?;

            let mut named_file = NamedTempFile::new().to_internal_error()?;
            named_file.write(&file_data).to_internal_error()?;

            data = Some(file_data);

            // If it fails for some reason, set it to not be animated
            let animated = is_animated(&named_file, &hash.content_type).unwrap_or(false);
            db.set_attachment_hash_animated(&hash.id, animated).await?;

            animated
        }
        _ => false,
    };

    // Only process image files and don't process animations if not avatar or
    // icon. The preview of a design is one still frame: tiles, lists and anyone
    // who asked for no motion never play anything.
    if !matches!(hash.metadata, Metadata::Image { .. }) || (is_animated && !previews_a_still(&tag)) {
        return Ok(
            to_original(&tag, &format!("/{tag_str}/{file_id}/{}", file.filename)).into_response(),
        );
    }

    // Same file and tag, same preview: cached by content
    let preview = PREVIEWS
        .get_or_render(format!("{tag_str}:{}", hash.id), || async {
            // Original image data
            let original = if let Some(data) = data {
                data
            } else {
                retrieve_file_by_hash(&hash).await?
            };

            if tag_str == "effects" || (is_animated && tag_str == "decorations") {
                render_design_still(original, tag_str).await
            } else {
                render_preview(original, file.content_type.clone(), tag_str).await
            }
        })
        .await?;
    let data = preview.as_ref().clone();
    let cache_control =
        file_cache_control(tag_str, private, config().await.files.purge.configured());

    Ok((
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CONTENT_DISPOSITION, "inline"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            // files never run as a page, even if a browser is asked to show one
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox"),
            (header::CACHE_CONTROL, cache_control),
        ],
        data,
    )
        .into_response())
}

/// Fetch original file
///
/// Content disposition header will be set to 'attachment' to prevent browser from rendering anything.
///
/// Using `original` as the file name parameter will redirect you to the original file.
#[utoipa::path(
    get,
    path = "/{tag}/{file_id}/{file_name}",
    tag = "Files",
    responses(
        (status = 200, description = "Original file", body = Vec<u8>)
    ),
    params(
        ("tag" = Tag, Path, description = "Tag to fetch from (e.g. attachments, icons, ...)"),
        ("file_id" = String, Path, description = "File identifier"),
        ("file_name" = String, Path, description = "File name")
    ),
)]
async fn fetch_file(
    State(db): State<Database>,
    viewer: Option<User>,
    Path((tag, file_id, file_name)): Path<(Tag, String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let tag: &'static str = tag.clone().into();
    let file = db.fetch_attachment(tag, &file_id).await?;

    // Ignore deleted files
    if file.deleted.is_some_and(|v| v) {
        return Err(create_error!(NotFound));
    }

    // Ignore files that haven't been attached
    if file.used_for.is_none() {
        return Err(create_error!(NotFound));
    }

    // Community designs: only the current image of a published design is
    // public; the rest only for its creator and reviewers, never cached
    let private = community_gate(&db, tag, &file, viewer.as_ref()).await?;

    // Ensure filename is correct
    if file_name != file.filename {
        if file_name == "original" {
            let safe_filename = encode_component(&file.filename);

            return Ok(
                Redirect::permanent(&format!("/{tag}/{file_id}/{}", safe_filename)).into_response(),
            );
        }

        return Err(create_error!(NotFound));
    }

    let hash = file.as_hash(&db).await?;
    let data = retrieve_file_by_hash(&hash).await?;
    let cache_control = file_cache_control(tag, private, config().await.files.purge.configured());
    Ok(original_file_response(data, hash.content_type, cache_control, &headers))
}

/// Let browser media engines seek without repeatedly requesting the whole file.
fn original_file_response(
    data: Vec<u8>,
    content_type: String,
    cache_control: &str,
    headers: &HeaderMap,
) -> Response {
    use crate::byte_range::{ByteRange, parse_range};
    use axum::http::StatusCode;
    let size = data.len();
    // No entity validator is advertised, so If-Range requests receive the complete entity.
    let range = if headers.contains_key(header::IF_RANGE) {
        None
    } else {
        headers.get(header::RANGE).and_then(|v| v.to_str().ok())
    };
    let (status, body, content_range) = match parse_range(range, size) {
        ByteRange::Full => (StatusCode::OK, data, None),
        ByteRange::Partial(start, end) => (
            StatusCode::PARTIAL_CONTENT,
            data[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{size}")),
        ),
        ByteRange::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            Vec::new(),
            Some(format!("bytes */{size}")),
        ),
    };
    let length = body.len();
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, "attachment".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            // files never run as a page, even if a browser is asked to show one
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox".to_owned()),
            (header::CACHE_CONTROL, cache_control.to_owned()),
            (header::ACCEPT_RANGES, "bytes".to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        body,
    )
        .into_response();
    if let Some(value) = content_range {
        response
            .headers_mut()
            .insert(header::CONTENT_RANGE, value.parse().unwrap());
    }
    response
}

/// Fetch original file (Moderation)
///
/// This is intentionally left out of the OpenApi docs.
/// It uses the config key api.security.admin_keys to determine access. This is intended for server to server communication.
async fn fetch_file_mod(
    State(db): State<Database>,
    headers: HeaderMap,
    Path((tag, file_id, file_name)): Path<(Tag, String, String)>,
) -> Result<Response> {
    let config = revolt_config::config().await;

    let token = headers
        .get("X-Admin-Token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| create_error!(NotAuthenticated))?;

    if !config
        .api
        .security
        .admin_keys
        .iter()
        .any(|tok| tok == token)
    {
        return Err(create_error!(NotAuthenticated));
    }

    let tag: &'static str = tag.clone().into();
    let file = db.fetch_attachment(tag, &file_id).await?;

    // Ignore files that haven't been attached
    if file.used_for.is_none() {
        return Err(create_error!(NotFound));
    }

    // Ensure filename is correct
    if file_name != file.filename {
        if file_name == "original" {
            let safe_filename = encode_component(&file.filename);

            return Ok(
                Redirect::permanent(&format!("/{tag}/{file_id}/{}", safe_filename)).into_response(),
            );
        }

        return Err(create_error!(NotFound));
    }

    let hash = file.as_hash(&db).await?;
    retrieve_file_by_hash(&hash).await.map(|data| {
        (
            [
                (header::CONTENT_TYPE, hash.content_type),
                (header::CONTENT_DISPOSITION, "attachment".to_owned()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                // files never run as a page, even if a browser is asked to show one
                (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox".to_owned()),
                (header::CACHE_CONTROL, PRIVATE_CACHE_CONTROL.to_owned()),
            ],
            data,
        )
            .into_response()
    })
}

#[cfg(test)]
mod cors_tests {
    use std::time::Duration;

    use revolt_config::CorsOrigins;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    /// Ask a running service as a browser on `origin` does; the answer's headers
    async fn ask(port: u16, method: &str, origin: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!(
            "{method} / HTTP/1.1\r\nHost: test\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: GET\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8_lossy(&answer).to_lowercase()
    }

    #[tokio::test]
    async fn cors_answers_a_listed_origin_and_no_other() {
        let origins = CorsOrigins::new("https://app.zrzur.com", &[], Some("https://chat.example"));
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(super::cors_layer(origins));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for method in ["GET", "OPTIONS"] {
            for origin in ["https://discover.zrzur.com", "https://chat.example"] {
                let answer = ask(port, method, origin).await;
                let header = format!("access-control-allow-origin: {origin}");
                assert!(answer.contains(&header), "{method} {origin}: {answer}");
                // a cache must keep answers per origin
                assert!(answer.contains("vary: origin"), "{method} {origin}: {answer}");
            }
            for origin in ["https://evil.example", "http://localhost:3000"] {
                let answer = ask(port, method, origin).await;
                assert!(!answer.contains("access-control-allow-origin"), "{method} {origin}: {answer}");
                assert!(answer.contains("vary: origin"), "{method} {origin}: {answer}");
            }
        }
    }
}

#[cfg(test)]
mod pixel_limit_tests {
    use super::pixels_allowed;
    use revolt_config::FilesLimit;

    fn limit() -> FilesLimit {
        FilesLimit { min_file_size: 1, min_resolution: [1, 1], max_mega_pixels: 40, max_pixel_side: 10_000 }
    }

    #[test]
    fn ordinary_pictures_pass() {
        assert!(pixels_allowed(4032, 3024, &limit()));
        assert!(pixels_allowed(10_000, 4_000, &limit()));
    }

    #[test]
    fn a_huge_claimed_picture_is_refused() {
        assert!(!pixels_allowed(11_000, 11_000, &limit()));
        assert!(!pixels_allowed(10_001, 10, &limit()));
        // under the side limit, but over 40 megapixels
        assert!(!pixels_allowed(9_000, 9_000, &limit()));
        assert!(!pixels_allowed(usize::MAX, usize::MAX, &limit()));
    }

    #[test]
    fn zero_means_no_limit() {
        let none = FilesLimit { max_mega_pixels: 0, max_pixel_side: 0, ..limit() };
        assert!(pixels_allowed(50_000, 50_000, &none));
    }
}

#[cfg(test)]
mod sound_tests {
    use super::{is_sound_type, size_limit_for, Tag, SOUND_SIZE_LIMIT};
    use std::collections::HashMap;

    #[test]
    fn sounds_are_audio_a_browser_plays() {
        for mime in ["audio/mpeg", "audio/ogg", "audio/wav", "audio/x-wav"] {
            assert!(is_sound_type(mime), "{mime}");
        }
        for mime in ["video/webm", "audio/x-flac", "audio/midi", "image/png", "text/plain"] {
            assert!(!is_sound_type(mime), "{mime}");
        }
    }

    #[test]
    fn an_old_limits_table_still_takes_sounds() {
        let old = HashMap::from([("emojis".to_string(), 500_000)]);
        assert_eq!(size_limit_for(&old, &Tag::sounds), SOUND_SIZE_LIMIT);
        assert_eq!(size_limit_for(&old, &Tag::emojis), 500_000);
        let new = HashMap::from([("sounds".to_string(), 300_000)]);
        assert_eq!(size_limit_for(&new, &Tag::sounds), 300_000);
    }

    #[test]
    fn the_shipped_config_limits_sounds_everywhere() {
        let toml = include_str!("../../../core/config/Revolt.toml");
        assert_eq!(toml.matches("sounds = 512_000").count(), 3);
    }
}
