//! The sound library: ready-made soundboard sounds (Kenney, CC0) built into
//! the server. Listing it needs a signed-in person; a preview is a plain
//! file under an address that never changes. Adding one to a server is
//! `servers/soundboard.rs`.
use std::sync::LazyLock;

use revolt_database::{
    sound_library::{library_sound, short_hash, sound_library},
    User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::{
    r#gen::OpenApiGenerator,
    response::OpenApiResponderInner,
    revolt_okapi::openapi3::{self, MediaType, OpenApi, RefOr},
    OpenApiError,
};
use rocket::{
    http::ContentType,
    response::{self, Responder},
    serde::json::Json,
    Request, Response, Route,
};
use schemars::schema::{InstanceType, SchemaObject, SingleOrVec};

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![list_library, preview]
}

/// The list changes only with a new server version
const LIST_CACHE_CONTROL: &str = "private, max-age=3600";
/// A preview address holds the file's hash, so the file behind it never changes
const PREVIEW_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The list as the API sends it, written once
static LIST_JSON: LazyLock<String> = LazyLock::new(|| {
    let library = sound_library();
    let list = v0::SoundLibrary {
        categories: library
            .categories
            .iter()
            .map(|category| v0::SoundLibraryCategory {
                id: category.id.clone(),
                names: category.names.clone(),
            })
            .collect(),
        sounds: library
            .sounds
            .iter()
            .map(|sound| v0::SoundLibrarySound {
                id: sound.id.clone(),
                category: sound.category.clone(),
                emoji: sound.emoji.clone(),
                names: sound.names.clone(),
                duration_ms: sound.duration_ms,
                size: sound.size,
                version: short_hash(&sound.sha256).to_string(),
            })
            .collect(),
    };
    serde_json::to_string(&list).expect("the library serialises")
});

/// The library list with its cache header
pub struct LibraryList;

impl<'r> Responder<'r, 'static> for LibraryList {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        Response::build_from((ContentType::JSON, LIST_JSON.as_str()).respond_to(req)?)
            .raw_header("Cache-Control", LIST_CACHE_CONTROL)
            .ok()
    }
}

impl OpenApiResponderInner for LibraryList {
    fn responses(r#gen: &mut OpenApiGenerator) -> std::result::Result<openapi3::Responses, OpenApiError> {
        <Json<v0::SoundLibrary> as OpenApiResponderInner>::responses(r#gen)
    }
}

/// A library file with its cache header
pub struct LibraryAudio(&'static [u8]);

impl<'r> Responder<'r, 'static> for LibraryAudio {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        Response::build_from((ContentType::new("audio", "ogg"), self.0).respond_to(req)?)
            .raw_header("Cache-Control", PREVIEW_CACHE_CONTROL)
            .ok()
    }
}

impl OpenApiResponderInner for LibraryAudio {
    fn responses(_gen: &mut OpenApiGenerator) -> std::result::Result<openapi3::Responses, OpenApiError> {
        let mut content = schemars::Map::new();
        content.insert(
            "audio/ogg".to_owned(),
            MediaType {
                schema: Some(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::String))),
                    format: Some("binary".to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let mut responses = schemars::Map::new();
        responses.insert(
            "200".to_string(),
            RefOr::Object(openapi3::Response {
                description: "The sound (Ogg Vorbis)".to_string(),
                content,
                ..Default::default()
            }),
        );
        Ok(openapi3::Responses {
            responses,
            ..Default::default()
        })
    }
}

/// # Fetch Sound Library
///
/// The ready-made sounds any server can add to its soundboard, with their
/// names in every language the app has.
#[openapi(tag = "Server Customisation")]
#[get("/library")]
pub async fn list_library(_user: User) -> LibraryList {
    LibraryList
}

/// # Preview Library Sound
///
/// The file of a library sound. Add `?v=<version>` from the list: the
/// answer is cached for good.
#[openapi(tag = "Server Customisation")]
#[get("/library/<id>/audio?<v>")]
pub async fn preview(id: String, v: Option<String>) -> Result<LibraryAudio> {
    let _ = v;
    library_sound(&id)
        .map(|sound| LibraryAudio(sound.bytes))
        .ok_or_else(|| create_error!(NotFound))
}
