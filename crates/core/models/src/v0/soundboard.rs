#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// A sound of a server's soundboard
    pub struct SoundboardSound {
        /// Unique id (also the id of its file, tag `sounds`)
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Server it belongs to
        pub server: String,
        /// Who added it
        pub creator_id: String,
        /// Name
        pub name: String,
        /// Emoji shown with it (a unicode emoji or a custom emoji id)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
        /// Length in milliseconds
        pub duration_ms: u32,
        /// Volume it plays at, 1-100
        pub volume: u8,
    }

    /// Add a sound to a server's soundboard
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateSoundboardSound {
        /// Name (1-32 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Emoji shown with it (a unicode emoji or a custom emoji id)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
        /// Length in milliseconds, as measured when it was picked (at most 5200)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 5200)))]
        pub duration_ms: u32,
        /// Volume it plays at, 1-100 (default 100)
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub volume: Option<u8>,
    }

    /// Change a soundboard sound
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataEditSoundboardSound {
        /// New name (1-32 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub name: Option<String>,
        /// New emoji
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub emoji: Option<String>,
        /// New volume, 1-100
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub volume: Option<u8>,
    }

    /// A server's soundboard
    pub struct SoundboardList {
        /// Its sounds, oldest first
        pub sounds: Vec<SoundboardSound>,
        /// Most sounds this server may have (more with Wings)
        pub limit: usize,
    }
);
