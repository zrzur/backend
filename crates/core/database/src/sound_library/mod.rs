//! The sound library: ready-made soundboard sounds (Kenney, CC0) that any
//! server can add. The files and their names are built into the binary
//! (`assets/sound-library`, about 1.2 MB); autumn stores each file once as a
//! file hash at start, and adding one to a server makes a new file of that
//! hash, so the sound then works like any uploaded one.
use std::{collections::BTreeMap, sync::LazyLock};

use iso8601_timestamp::Timestamp;
use serde::Deserialize;
use sha2::Digest;

use crate::{File, FileHash, Metadata};

mod files;

/// Languages the library names are written in
pub const LIBRARY_LANGUAGES: [&str; 7] = ["en", "ar", "de", "fr", "es", "ru", "zh-Hans"];

/// A sound of the library
#[derive(Debug, Deserialize)]
pub struct LibrarySound {
    /// Stable id, e.g. `voice-go`
    pub id: String,
    /// Category id, e.g. `voice`
    pub category: String,
    /// Emoji shown with it
    pub emoji: String,
    /// Name per language (`LIBRARY_LANGUAGES`)
    pub names: BTreeMap<String, String>,
    /// Length measured by ffprobe when the library was built
    pub duration_ms: u32,
    /// Size of the file in bytes
    pub size: usize,
    /// Sha256 of the file (also the id of its file hash)
    pub sha256: String,
    /// The Ogg Vorbis file
    #[serde(skip)]
    pub bytes: &'static [u8],
}

/// A category of the library
#[derive(Debug, Deserialize)]
pub struct LibraryCategory {
    /// Category id
    pub id: String,
    /// Name per language
    pub names: BTreeMap<String, String>,
}

/// The whole library
#[derive(Debug, Deserialize)]
pub struct SoundLibrary {
    /// Categories, in the order they are shown
    pub categories: Vec<LibraryCategory>,
    /// Sounds, grouped by category
    pub sounds: Vec<LibrarySound>,
}

static LIBRARY: LazyLock<SoundLibrary> = LazyLock::new(|| {
    let mut library: SoundLibrary =
        serde_json::from_str(include_str!("../../assets/sound-library/library.json"))
            .expect("the built-in sound library is valid JSON");
    for sound in &mut library.sounds {
        sound.bytes = files::FILES
            .iter()
            .find(|(id, _)| *id == sound.id)
            .map(|(_, bytes)| *bytes)
            .expect("every library sound has a file");
    }
    library
});

/// The built-in library
pub fn sound_library() -> &'static SoundLibrary {
    &LIBRARY
}

/// A library sound by id
pub fn library_sound(id: &str) -> Option<&'static LibrarySound> {
    LIBRARY.sounds.iter().find(|sound| sound.id == id)
}

/// Whether a file hash holds a library sound (those are kept when the last
/// server sound using one is removed: the next server adds it again)
pub fn is_library_hash(hash: &str) -> bool {
    LIBRARY.sounds.iter().any(|sound| sound.sha256 == hash)
}

/// Short version of a file's hash, for addresses that never change
pub fn short_hash(sha256: &str) -> &str {
    &sha256[..12.min(sha256.len())]
}

impl LibrarySound {
    /// Its name in a language (`ar`, `de-AT`, `zh-Hans` …), English when
    /// the library has no name in it
    pub fn name_in(&self, locale: Option<&str>) -> &str {
        let locale = locale.unwrap_or("en");
        let base = locale.split(['-', '_']).next().unwrap_or("en");
        self.names
            .get(locale)
            .or_else(|| {
                // `zh`, `zh-CN`, `zh-Hans-CN`: the one Chinese name there is
                self.names
                    .iter()
                    .find(|(lang, _)| lang.split('-').next() == Some(base))
                    .map(|(_, name)| name)
            })
            .or_else(|| self.names.get("en"))
            .map(String::as_str)
            .unwrap_or(&self.id)
    }

    /// Sha256 of the bytes built into this binary
    pub fn computed_sha256(&self) -> String {
        format!("{:02x}", sha2::Sha256::digest(self.bytes))
    }

    /// The file hash autumn stores this sound as (`iv` from the upload)
    pub fn file_hash(&self, bucket_id: String, iv: String) -> FileHash {
        FileHash {
            id: self.sha256.clone(),
            processed_hash: self.sha256.clone(),
            created_at: Timestamp::now_utc(),
            bucket_id,
            path: self.sha256.clone(),
            iv,
            metadata: Metadata::Audio {
                duration_ms: Some(self.duration_ms),
            },
            content_type: "audio/ogg".to_string(),
            size: self.size as isize,
        }
    }

    /// A new file (tag `sounds`) of this sound for one person, as if they
    /// had uploaded it
    pub fn new_file(&self, hash: &FileHash, uploader_id: &str) -> File {
        hash.into_file(
            ulid::Ulid::new().to_string(),
            "sounds".to_string(),
            format!("{}.ogg", self.id),
            uploader_id.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_complete() {
        let library = sound_library();
        assert!((80..=120).contains(&library.sounds.len()), "{}", library.sounds.len());
        assert_eq!(files::FILES.len(), library.sounds.len());
        for sound in &library.sounds {
            assert!(
                library.categories.iter().any(|c| c.id == sound.category),
                "{} has no category",
                sound.id
            );
            for lang in LIBRARY_LANGUAGES {
                let name = sound.names.get(lang).unwrap_or_else(|| panic!("{} {lang}", sound.id));
                assert!((1..=32).contains(&name.chars().count()), "{} {lang}", sound.id);
            }
            assert!(sound.duration_ms > 0 && sound.duration_ms <= 5_000, "{}", sound.id);
            assert_eq!(sound.bytes.len(), sound.size, "{}", sound.id);
            assert_eq!(sound.computed_sha256(), sound.sha256, "{}", sound.id);
            assert!(sound.bytes.starts_with(b"OggS"), "{}", sound.id);
        }
        for category in &library.categories {
            for lang in LIBRARY_LANGUAGES {
                assert!(category.names.contains_key(lang), "{} {lang}", category.id);
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let library = sound_library();
        let mut ids: Vec<&str> = library.sounds.iter().map(|s| s.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), library.sounds.len());
    }

    #[test]
    fn names_follow_the_language() {
        let go = library_sound("voice-go").expect("voice-go");
        assert_eq!(go.name_in(Some("en")), "\u{201c}Go\u{201d}");
        assert!(go.name_in(Some("ar")).contains("انطلق"));
        assert!(go.name_in(Some("de-AT")).contains("Los"));
        assert!(go.name_in(Some("zh-CN")).contains("出发"));
        assert_eq!(go.name_in(Some("xx")), go.name_in(None));
        assert!(library_sound("nothing-here").is_none());
        assert!(is_library_hash(&go.sha256));
        assert!(!is_library_hash("00"));
    }
}
