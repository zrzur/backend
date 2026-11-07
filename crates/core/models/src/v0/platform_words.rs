#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// A word or phrase on the platform-wide list
    pub struct PlatformWord {
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Same spelling rules as server AutoMod: a word, `*` at either end
        /// to match part of a word
        pub pattern: String,
        /// Reason shown to the writer
        pub category: PlatformWordCategory,
        /// On the allow-list: never counts as a hit (false positives)
        pub allow: bool,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub note: Option<String>,
        pub added_by: String,
        pub created_at: i64,
    }

    /// Why a word is on the list
    #[derive(Copy)]
    pub enum PlatformWordCategory {
        Harassment,
        HateOrThreats,
        SexualContent,
        Spam,
        Other,
    }

    /// Add a word to the platform-wide list
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataPlatformWord {
        #[cfg_attr(feature = "validator", validate(length(min = 2, max = 64)))]
        pub pattern: String,
        pub category: PlatformWordCategory,
        #[cfg_attr(feature = "serde", serde(default))]
        pub allow: bool,
        #[cfg_attr(feature = "validator", validate(length(max = 200)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub note: Option<String>,
    }
);
