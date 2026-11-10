use std::fmt::Display;
use std::panic::Location;

use strum_macros::IntoStaticStr;

#[cfg(feature = "serde")]
#[macro_use]
extern crate serde;

#[cfg(feature = "schemas")]
#[macro_use]
extern crate schemars;

#[cfg(feature = "utoipa")]
#[macro_use]
extern crate utoipa;

#[cfg(feature = "rocket")]
pub mod rocket;

#[cfg(feature = "axum")]
pub mod axum;

#[cfg(feature = "okapi")]
pub mod okapi;

/// Result type with custom Error
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error information
#[cfg_attr(feature = "serde", derive(Deserialize))]
#[cfg_attr(feature = "schemas", derive(JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[derive(Debug, Clone)]
pub struct Error {
    /// Type of error and additional information
    #[cfg_attr(feature = "serde", serde(flatten))]
    pub error_type: ErrorType,

    /// Where this error occurred
    pub location: Option<String>,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} occurred in {:?}", self.error_type, self.location)
    }
}

impl std::error::Error for Error {}

/// A build for production. Technical detail stays in the log there: source
/// paths and database names never go into an answer.
const PRODUCTION: bool = !cfg!(debug_assertions);

impl Error {
    /// The source path an answer may carry
    #[cfg(feature = "serde")]
    fn public_location(&self, production: bool) -> Option<&str> {
        match self.error_type {
            _ if production => None,
            ErrorType::InvalidCredentials
                | ErrorType::InvalidSession
                | ErrorType::InvalidToken
                | ErrorType::UnverifiedAccount
                | ErrorType::LockedOut
                | ErrorType::DisallowedMFAMethod => None,
            _ => self.location.as_deref(),
        }
    }

    /// The type an answer may carry: `type` stays, a database error loses
    /// the names of the operation and the collection
    #[cfg(feature = "serde")]
    fn public_type(&self, production: bool) -> ErrorType {
        match self.error_type {
            ErrorType::DatabaseError { .. } if production => ErrorType::DatabaseError {
                operation: String::new(),
                collection: String::new(),
            },
            _ => self.error_type.clone(),
        }
    }

    /// Whether the fault is ours: these are what an operator has to see
    pub fn is_server_fault(&self) -> bool {
        matches!(
            self.error_type,
            ErrorType::LabelMe
                | ErrorType::InternalError
                | ErrorType::DatabaseError { .. }
                | ErrorType::OperationFailed
                | ErrorType::ProxyError
        )
    }

    /// Put what an answer leaves out into the log
    pub fn log(&self) {
        if self.is_server_fault() {
            log::error!("{self}");
        } else {
            log::debug!("{self}");
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Body<'a> {
            #[serde(flatten)]
            error_type: ErrorType,
            #[serde(skip_serializing_if = "Option::is_none")]
            location: Option<&'a str>,
        }

        Body {
            error_type: self.public_type(PRODUCTION),
            location: self.public_location(PRODUCTION),
        }
        .serialize(serializer)
    }
}

/// One field that failed validation, for a message in the reader's language
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schemas", derive(JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationProblem {
    /// The field as it was sent, `embeds[0].title` when nested
    pub field: String,
    /// `length`, `range`, `count`, `https`, `required`, `format`, `taken`,
    /// `per_day`, `image`, `adult` or `invalid`
    pub rule: String,
    /// Smallest value allowed: characters for `length`, items for `count`
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
    pub min: Option<i64>,
    /// Largest value allowed
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
    pub max: Option<i64>,
}

impl ValidationProblem {
    pub fn new(field: impl Into<String>, rule: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            rule: rule.into(),
            min: None,
            max: None,
        }
    }

    /// Between `min` and `max` characters
    pub fn length(field: impl Into<String>, min: usize, max: usize) -> Self {
        Self {
            min: Some(min as i64),
            max: Some(max as i64),
            ..Self::new(field, "length")
        }
    }

    /// At most `max` items
    pub fn count(field: impl Into<String>, max: usize) -> Self {
        Self {
            max: Some(max as i64),
            ..Self::new(field, "count")
        }
    }

    fn bounds(&self, unit: &str) -> String {
        match (self.min, self.max) {
            (Some(min), Some(max)) if min == max => format!("must be {min}{unit}"),
            (Some(min), Some(max)) => format!("must be between {min} and {max}{unit}"),
            (Some(min), None) => format!("must be at least {min}{unit}"),
            (None, Some(max)) => format!("must be at most {max}{unit}"),
            (None, None) => "is not valid".to_string(),
        }
    }

    /// One plain English line, for the `error` field of the answer
    pub fn sentence(&self) -> String {
        let rule = match self.rule.as_str() {
            "length" => self.bounds(" characters"),
            "range" => self.bounds(""),
            "count" => self.bounds(" items"),
            "https" => "must be a https:// link".to_string(),
            "required" => "is required".to_string(),
            "format" => "has the wrong format".to_string(),
            "taken" => "is already used".to_string(),
            "per_day" => self.bounds(" a day"),
            "image" => "is not an image that can be used".to_string(),
            "adult" => "must have declared an age of 18 or over".to_string(),
            _ => "is not valid".to_string(),
        };
        format!("{} {rule}", self.field)
    }
}

/// A `FailedValidation` naming the fields: `error` is one plain line, and the
/// app shows each problem in the reader's language
#[track_caller]
pub fn failed_validation(fields: Vec<ValidationProblem>) -> Error {
    let loc = Location::caller();
    let error = fields
        .iter()
        .map(ValidationProblem::sentence)
        .collect::<Vec<_>>()
        .join("; ");
    Error {
        error_type: ErrorType::FailedValidation { error, fields },
        location: format!("{}:{}:{}", loc.file(), loc.line(), loc.column()).into(),
    }
}

/// Possible error types
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
#[cfg_attr(feature = "schemas", derive(JsonSchema))]
#[cfg_attr(feature = "utoipa", derive(ToSchema))]
#[derive(Debug, Clone, IntoStaticStr)]
pub enum ErrorType {
    /// This error was not labeled :(
    LabelMe,

    // ? Support Errors
    ContactSupport {
        locale: String,
        msg: String,
    },

    // ? Onboarding related errors
    AlreadyOnboarded,
    /// The account has no declared year and month of birth yet: everything
    /// but declaring it (PATCH /auth/account/age) waits for it
    AgeNotDeclared,

    // ? User related errors
    UsernameTaken,
    InvalidUsername,
    DiscriminatorChangeRatelimited,
    UnknownUser,
    AlreadyFriends,
    AlreadySentRequest,
    Blocked,
    BlockedByOther,
    NotFriends,
    /// Their privacy settings don't let you start a direct message
    DirectMessagesRestricted,
    /// Their privacy settings don't let you add them to a group chat
    GroupInvitesRestricted,
    TooManyPendingFriendRequests {
        max: usize,
    },

    // ? Channel related errors
    UnknownChannel,
    UnknownAttachment,
    UnknownMessage,
    CannotDeleteMessage,
    CannotEditMessage,
    CannotJoinCall,
    TooManyAttachments {
        max: usize,
    },
    TooManyEmbeds {
        max: usize,
    },
    TooManyReplies {
        max: usize,
    },
    /// A server has this many scheduled and active events already
    TooManyServerEvents {
        max: usize,
    },
    TooManyChannels {
        max: usize,
    },
    /// This channel has as many webhooks as it may have
    TooManyChannelWebhooks {
        max: usize,
    },
    /// This server has as many webhooks as it may have
    TooManyServerWebhooks {
        max: usize,
    },
    /// Too many requests. `retry_after` is in milliseconds, like every 429
    RateLimited {
        retry_after: u64,
        global: bool,
    },
    /// A webhook posting to a forum must name a thread: `thread_id` for an
    /// existing one or `thread_name` for a new one
    ForumThreadRequired,
    /// This server switched threads off: existing ones can be read, nothing new
    ThreadsDisabled,
    /// This server switched forums off: existing ones can be read, nothing new
    ForumsDisabled,
    /// Plugins are switched off on this server (`features.plugins_enabled`)
    PluginsDisabled,
    EmptyMessage,
    PayloadTooLarge,
    CannotRemoveYourself,
    GroupTooLarge {
        max: usize,
    },
    AlreadyInGroup,
    NotInGroup,
    AlreadyPinned,
    NotPinned,
    InSlowmode {
        retry_after: u64,
    },

    // ? Server related errors
    CantCreateServers,
    UnknownServer,
    InvalidRole,
    Banned,
    TooManyServers {
        max: usize,
    },
    TooManyEmoji {
        max: usize,
    },
    TooManyCommunityItems {
        max: usize,
    },
    /// Needs a platform permission (creator, reviewer, admin)
    MissingPlatformPermission {
        permission: String,
    },
    /// Someone else changed the community design first
    CommunityItemConflict,
    /// The platform word list stopped this text (`category` is the reason)
    ProfileTextFiltered {
        category: String,
    },
    /// Daily report limit reached (urgent reports are never limited)
    TooManyReports {
        max: usize,
    },
    /// Someone else changed the Trust & Safety case first
    SafetyCaseConflict,
    // ? Developer platform errors
    UnknownApplication,
    ApplicationNotInstalled,
    ApplicationOffline,
    /// An address Zrzur sends signed requests to failed its checks: `reason`
    /// says which. Any feature that delivers to third parties uses it.
    SignedDeliveryRejected {
        reason: String,
    },
    UnknownCommand,
    CommandAlreadyExists,
    TooManyCommands {
        max: usize,
    },
    UnknownInteraction,
    InteractionExpired,
    InteractionAlreadyAcknowledged,
    TooManyFollowups {
        max: usize,
    },
    TooManySupercharges {
        max: usize,
    },
    /// Made too many bots this week
    TooManyBotCreations {
        max: usize,
        /// When the account can make a bot again, in milliseconds since 1970
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
        resets_at: Option<i64>,
    },
    /// The top row of Discover is full
    PromotionSlotsFull,
    /// The server has to be approved for Discover first
    NotOnDiscover,
    TooManyRoles {
        max: usize,
    },
    AlreadyInServer,
    CannotTimeoutYourself,
    /// The server is below the Wings level that unlocks a custom invite link
    VanityUnavailable,
    /// Another server already uses this custom invite link
    VanityTaken,
    /// This name cannot be used as a custom invite link
    VanityReserved,
    /// The server dropped below the required level; its custom link is paused
    VanityPaused,

    // ? Bot related errors
    ReachedMaximumBots,
    IsBot,
    IsNotBot,
    BotIsPrivate,

    // ? User safety related errors
    CannotReportYourself,

    // ? Permission errors
    MissingPermission {
        permission: String,
    },
    MissingUserPermission {
        permission: String,
    },
    /// A bot asked for, or acted without, a gateway intent it may not use
    MissingIntent {
        intent: String,
    },
    /// A bot tried to masquerade under the name of someone in the server
    MasqueradeNameTaken,
    /// Official accounts can't be answered, reacted to, pinned, reported,
    /// befriended or blocked; a reply to Zrzur Support goes through a ticket
    OfficialAccount,
    /// An unverified bot is already in as many servers as it may be
    BotNeedsVerification,
    /// The server's verification level asks for more before talking:
    /// `email` (verify your email), `account_age` or `member_age`
    /// (wait `wait_seconds`)
    VerificationRequired {
        requirement: String,
        wait_seconds: Option<u64>,
    },
    /// AutoMod blocked the message; `rule` is the rule's name
    AutoModBlocked {
        rule: String,
    },
    /// Rules screening: accept the server rules before talking
    RulesNotAccepted,
    /// The server paused invites for now (security action)
    InvitesPaused,
    /// A server you share paused DMs between members who aren't friends
    DirectMessagesPaused,
    NotElevated,
    NotPrivileged,
    CannotGiveMissingPermissions,
    NotOwner,
    IsElevated,

    // ? General errors
    DatabaseError {
        operation: String,
        collection: String,
    },
    InternalError,
    InvalidOperation,
    InvalidCredentials,
    InvalidProperty,
    InvalidSession,
    InvalidFlagValue,
    NotAuthenticated,
    DuplicateNonce,
    NotFound,
    NoEffect,
    FailedValidation {
        /// One plain English line for developers
        error: String,
        /// Which fields failed and how. Absent when no single field did.
        #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Vec::is_empty"))]
        fields: Vec<ValidationProblem>,
    },
    HeaderTooLarge,
    OperationFailed,
    IncorrectData {
        with: String,
    },

    // ? Voice errors
    LiveKitUnavailable,
    NotAVoiceChannel,
    AlreadyConnected,
    NotConnected,
    UnknownNode,
    // ? Micro-service errors
    ProxyError,
    FileTooSmall,
    FileTooLarge {
        max: usize,
    },
    FileTypeNotAllowed,
    ImageProcessingFailed,
    NoEmbedData,

    // ? Legacy errors
    VosoUnavailable,

    // ? Feature flag disabled in the config
    FeatureDisabled {
        feature: String,
    },

    // ? Authentication
    RenderFail,
    MissingHeaders,
    CaptchaFailed,
    BlockedByShield,
    UnverifiedAccount,
    EmailFailed,
    InvalidToken,
    MissingInvite,
    InvalidInvite,

    CompromisedPassword,
    ShortPassword,
    Blacklisted,
    LockedOut,

    TotpAlreadyEnabled,
    DisallowedMFAMethod,

    // ? Age rules
    AgeRequirementNotMet,
    AgeChangeNotAllowed,
    AgeRestricted,
    /// A message from a mature channel can only be forwarded into another
    /// mature channel
    ForwardMatureContent,
}

#[macro_export]
macro_rules! create_error {
    // A validation error that names no field
    ( FailedValidation { error: $value: expr $(,)? } ) => {
        $crate::create_error!(FailedValidation { error: $value, fields: Vec::new() })
    };
    ( FailedValidation { $value: ident } ) => {
        $crate::create_error!(FailedValidation { error: $value, fields: Vec::new() })
    };
    ( $error: ident $( $tt:tt )? ) => {
        $crate::Error {
            error_type: $crate::ErrorType::$error $( $tt )?,
            location: format!("{}:{}:{}", file!(), line!(), column!()).into(),
        }
    };
}

#[macro_export]
macro_rules! create_database_error {
    ( $operation: expr, $collection: expr ) => {
        $crate::create_error!(DatabaseError {
            operation: $operation.to_string(),
            collection: $collection.to_string()
        })
    };
}

#[macro_export]
#[cfg(debug_assertions)]
macro_rules! query {
    ( $self: ident, $type: ident, $collection: expr, $($rest:expr),+ ) => {
        Ok($self.$type($collection, $($rest),+).await.unwrap())
    };
}

#[macro_export]
#[cfg(not(debug_assertions))]
macro_rules! query {
    ( $self: ident, $type: ident, $collection: expr, $($rest:expr),+ ) => {
        $self.$type($collection, $($rest),+).await
            .map_err(|_| create_database_error!(stringify!($type), $collection))
    };
}

pub trait ToRevoltError<T> {
    #[track_caller]
    fn to_internal_error(self) -> Result<T, Error>;
}

impl<T, E: std::fmt::Debug + std::error::Error> ToRevoltError<T> for Result<T, E> {
    #[track_caller]
    fn to_internal_error(self) -> Result<T, Error> {
        let loc = Location::caller();

        self.map_err(|e| {
            log::error!("{e:?}");
            #[cfg(feature = "sentry")]
            sentry::capture_error(&e);

            Error {
                error_type: ErrorType::InternalError,
                location: format!("{}:{}:{}", loc.file(), loc.line(), loc.column()).into(),
            }
        })
    }
}

impl<T> ToRevoltError<T> for Option<T> {
    #[track_caller]
    fn to_internal_error(self) -> Result<T, Error> {
        let loc = Location::caller();

        self.ok_or_else(|| Error {
            error_type: ErrorType::InternalError,
            location: format!("{}:{}:{}", loc.file(), loc.line(), loc.column()).into(),
        })
    }
}

#[cfg(all(test, feature = "serde"))]
mod public_answer {
    use super::*;

    #[test]
    fn production_answers_carry_no_source_path_and_no_database_names() {
        let error = create_database_error!("find_one", "accounts");
        assert_eq!(error.public_location(true), None);
        assert!(error.public_location(false).is_some());
        let ErrorType::DatabaseError { operation, collection } = error.public_type(true) else {
            panic!("the type has to stay");
        };
        assert_eq!((operation.as_str(), collection.as_str()), ("", ""));
        let ErrorType::DatabaseError { collection, .. } = error.public_type(false) else {
            panic!("the type has to stay");
        };
        assert_eq!(collection, "accounts");
    }

    #[test]
    fn a_field_problem_reads_as_one_plain_line() {
        let error = failed_validation(vec![ValidationProblem::length("statement", 20, 1000)]);
        let ErrorType::FailedValidation { error, fields } = error.error_type else {
            panic!("wrong type");
        };
        assert_eq!(error, "statement must be between 20 and 1000 characters");
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn the_old_form_of_the_macro_names_no_field() {
        let error = "plain";
        let built = [
            create_error!(FailedValidation { error: error.to_string() }),
            {
                let error = error.to_string();
                create_error!(FailedValidation { error })
            },
        ];
        for one in built {
            assert!(matches!(
                one.error_type,
                ErrorType::FailedValidation { ref fields, .. } if fields.is_empty()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ErrorType;

    #[test]
    fn use_macro_to_construct_error() {
        let error = create_error!(LabelMe);
        assert!(matches!(error.error_type, ErrorType::LabelMe));
    }

    #[test]
    fn use_macro_to_construct_complex_error() {
        let error = create_error!(LabelMe);
        assert!(matches!(error.error_type, ErrorType::LabelMe));
    }
}

#[cfg(test)]
mod stable_codes {
    use std::collections::BTreeSet;

    /// The variant names of `ErrorType`, read from this very file
    fn variants() -> BTreeSet<String> {
        let source = include_str!("lib.rs");
        let body = source
            .split_once("pub enum ErrorType {")
            .expect("ErrorType in lib.rs")
            .1;
        let mut names = BTreeSet::new();
        for line in body.lines() {
            if line.starts_with('}') {
                break;
            }
            // variants sit at one level of indentation
            let Some(rest) = line.strip_prefix("    ") else { continue };
            if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
                names.insert(name);
            }
        }
        names
    }

    #[test]
    fn error_codes_are_stable() {
        let listed: BTreeSet<String> = include_str!("../error-codes.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect();
        let actual = variants();
        assert!(actual.len() > 100, "parsing broke: {}", actual.len());

        let removed: Vec<_> = listed.difference(&actual).collect();
        assert!(removed.is_empty(), "error codes are stable, these went away: {removed:?}");
        let added: Vec<_> = actual.difference(&listed).collect();
        assert!(added.is_empty(), "add these to error-codes.txt and the docs: {added:?}");
    }
}
