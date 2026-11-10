use std::io::Cursor;

use rocket::{
    http::{ContentType, Status},
    response::{self, Responder},
    Request, Response,
};

use crate::{Error, ErrorType};

/// The error a request guard refused with, for the catcher that answers
/// (Rocket gives catchers only the status)
#[derive(Default)]
pub struct GuardRefusal(pub Option<Error>);

/// HTTP response builder for Error enum
impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(mut self, request: &'r Request<'_>) -> response::Result<'static> {
        self.log();

        // Callers of webhook routes are outside services: they get no source
        // paths, on any error
        if request.uri().path().as_str().contains("/webhooks") {
            self.location = None;
        }

        let status = match self.error_type {
            ErrorType::LabelMe => Status::InternalServerError,

            ErrorType::ContactSupport { .. } => Status::BadRequest,

            ErrorType::AlreadyOnboarded => Status::Forbidden,
            ErrorType::AgeNotDeclared => Status::Forbidden,

            ErrorType::UnknownUser => Status::NotFound,
            ErrorType::InvalidUsername => Status::BadRequest,
            ErrorType::UsernameTaken => Status::Conflict,
            ErrorType::DiscriminatorChangeRatelimited => Status::TooManyRequests,
            ErrorType::AlreadyFriends => Status::Conflict,
            ErrorType::AlreadySentRequest => Status::Conflict,
            ErrorType::Blocked => Status::Conflict,
            ErrorType::BlockedByOther => Status::Forbidden,
            ErrorType::NotFriends => Status::Forbidden,
            ErrorType::DirectMessagesRestricted => Status::Forbidden,
            ErrorType::GroupInvitesRestricted => Status::Forbidden,
            ErrorType::TooManyPendingFriendRequests { .. } => Status::BadRequest,

            ErrorType::UnknownChannel => Status::NotFound,
            ErrorType::UnknownMessage => Status::NotFound,
            ErrorType::UnknownAttachment => Status::BadRequest,
            ErrorType::CannotDeleteMessage => Status::Forbidden,
            ErrorType::CannotEditMessage => Status::Forbidden,
            ErrorType::CannotJoinCall => Status::BadRequest,
            ErrorType::TooManyAttachments { .. } => Status::BadRequest,
            ErrorType::TooManyReplies { .. } => Status::BadRequest,
            ErrorType::EmptyMessage => Status::UnprocessableEntity,
            ErrorType::PayloadTooLarge => Status::UnprocessableEntity,
            ErrorType::CannotRemoveYourself => Status::BadRequest,
            ErrorType::GroupTooLarge { .. } => Status::Forbidden,
            ErrorType::AlreadyInGroup => Status::Conflict,
            ErrorType::NotInGroup => Status::NotFound,
            ErrorType::AlreadyPinned => Status::BadRequest,
            ErrorType::NotPinned => Status::BadRequest,
            ErrorType::InSlowmode { retry_after: _ } => Status::TooManyRequests,
            ErrorType::InvalidFlagValue => Status::BadRequest,

            ErrorType::CantCreateServers => Status::Forbidden,
            ErrorType::UnknownServer => Status::NotFound,
            ErrorType::InvalidRole => Status::NotFound,
            ErrorType::Banned => Status::Forbidden,
            ErrorType::AlreadyInServer => Status::Conflict,
            ErrorType::CannotTimeoutYourself => Status::BadRequest,
            ErrorType::VanityUnavailable => Status::Forbidden,
            ErrorType::VanityTaken => Status::Conflict,
            ErrorType::VanityReserved => Status::BadRequest,
            ErrorType::VanityPaused => Status::Gone,

            ErrorType::TooManyServers { .. } => Status::BadRequest,
            ErrorType::TooManyEmbeds { .. } => Status::BadRequest,
            ErrorType::TooManyChannelWebhooks { .. } => Status::BadRequest,
            ErrorType::TooManyServerWebhooks { .. } => Status::BadRequest,
            ErrorType::RateLimited { .. } => Status::TooManyRequests,
            ErrorType::ForumThreadRequired => Status::BadRequest,
            ErrorType::ThreadsDisabled => Status::Forbidden,
            ErrorType::ForumsDisabled => Status::Forbidden,
            ErrorType::PluginsDisabled => Status::Forbidden,
            ErrorType::TooManyEmoji { .. } => Status::BadRequest,
            ErrorType::TooManySounds { .. } => Status::BadRequest,
            ErrorType::SoundAlreadyAdded => Status::Conflict,
            ErrorType::SoundLibraryUnavailable => Status::ServiceUnavailable,
            ErrorType::TooManySpeakers { .. } => Status::BadRequest,
            ErrorType::TooManyCommunityItems { .. } => Status::BadRequest,
            ErrorType::MissingPlatformPermission { .. } => Status::Forbidden,
            ErrorType::CommunityItemConflict => Status::Conflict,
            ErrorType::ProfileTextFiltered { .. } => Status::UnprocessableEntity,
            ErrorType::TooManyReports { .. } => Status::TooManyRequests,
            ErrorType::SafetyCaseConflict => Status::Conflict,
            ErrorType::UnknownApplication => Status::NotFound,
            ErrorType::ApplicationNotInstalled => Status::Forbidden,
            ErrorType::ApplicationOffline => Status::ServiceUnavailable,
            ErrorType::SignedDeliveryRejected { .. } => Status::BadRequest,
            ErrorType::UnknownCommand => Status::NotFound,
            ErrorType::CommandAlreadyExists => Status::Conflict,
            ErrorType::TooManyCommands { .. } => Status::BadRequest,
            ErrorType::UnknownInteraction => Status::NotFound,
            ErrorType::InteractionExpired => Status::Gone,
            ErrorType::InteractionAlreadyAcknowledged => Status::Conflict,
            ErrorType::TooManyFollowups { .. } => Status::BadRequest,
            ErrorType::TooManySupercharges { .. } => Status::BadRequest,
            ErrorType::TooManyBotCreations { .. } => Status::TooManyRequests,
            ErrorType::PromotionSlotsFull => Status::Conflict,
            ErrorType::NotOnDiscover => Status::BadRequest,
            ErrorType::TooManyChannels { .. } => Status::BadRequest,
            ErrorType::TooManyServerEvents { .. } => Status::BadRequest,
            ErrorType::TooManyRoles { .. } => Status::BadRequest,

            ErrorType::ReachedMaximumBots => Status::BadRequest,
            ErrorType::IsBot => Status::BadRequest,
            ErrorType::IsNotBot => Status::BadRequest,
            ErrorType::BotIsPrivate => Status::Forbidden,

            ErrorType::CannotReportYourself => Status::BadRequest,

            ErrorType::MissingPermission { .. } => Status::Forbidden,
            ErrorType::MissingUserPermission { .. } => Status::Forbidden,
            ErrorType::MissingIntent { .. } => Status::Forbidden,
            ErrorType::MasqueradeNameTaken => Status::BadRequest,
            ErrorType::OfficialAccount => Status::Forbidden,
            ErrorType::VerificationRequired { .. } => Status::Forbidden,
            ErrorType::InvitesPaused => Status::Forbidden,
            ErrorType::RulesNotAccepted => Status::Forbidden,
            ErrorType::AutoModBlocked { .. } => Status::Forbidden,
            ErrorType::DirectMessagesPaused => Status::Forbidden,
            ErrorType::BotNeedsVerification => Status::Forbidden,
            ErrorType::NotElevated => Status::Forbidden,
            ErrorType::NotPrivileged => Status::Forbidden,
            ErrorType::CannotGiveMissingPermissions => Status::Forbidden,
            ErrorType::NotOwner => Status::Forbidden,
            ErrorType::IsElevated => Status::Forbidden,

            ErrorType::DatabaseError { .. } => Status::InternalServerError,
            ErrorType::InternalError => Status::InternalServerError,
            ErrorType::InvalidOperation => Status::BadRequest,
            ErrorType::InvalidCredentials => Status::Unauthorized,
            ErrorType::InvalidProperty => Status::BadRequest,
            ErrorType::InvalidSession => Status::Unauthorized,
            ErrorType::NotAuthenticated => Status::Unauthorized,
            ErrorType::DuplicateNonce => Status::Conflict,
            ErrorType::NotFound => Status::NotFound,
            ErrorType::NoEffect => Status::BadRequest,
            ErrorType::FailedValidation { .. } => Status::BadRequest,
            ErrorType::LiveKitUnavailable => Status::BadRequest,
            ErrorType::NotAVoiceChannel => Status::BadRequest,
            ErrorType::AlreadyConnected => Status::BadRequest,
            ErrorType::NotConnected => Status::BadRequest,
            ErrorType::UnknownNode => Status::BadRequest,
            ErrorType::FeatureDisabled { .. } => Status::BadRequest,
            ErrorType::HeaderTooLarge => Status::BadRequest,

            ErrorType::ProxyError => Status::BadRequest,
            ErrorType::FileTooSmall => Status::UnprocessableEntity,
            ErrorType::FileTooLarge { .. } => Status::UnprocessableEntity,
            ErrorType::FileTypeNotAllowed => Status::BadRequest,
            ErrorType::ImageProcessingFailed => Status::InternalServerError,
            ErrorType::NoEmbedData => Status::BadRequest,
            ErrorType::VosoUnavailable => Status::BadRequest,

            ErrorType::RenderFail => Status::InternalServerError,
            ErrorType::MissingHeaders => Status::BadRequest,
            ErrorType::CaptchaFailed => Status::BadRequest,
            ErrorType::BlockedByShield => Status::BadRequest,
            ErrorType::UnverifiedAccount => Status::Forbidden,
            ErrorType::EmailFailed => Status::InternalServerError,
            ErrorType::InvalidToken => Status::Unauthorized,
            ErrorType::MissingInvite => Status::BadRequest,
            ErrorType::InvalidInvite => Status::BadRequest,
            ErrorType::CompromisedPassword => Status::BadRequest,
            ErrorType::ShortPassword => Status::BadRequest,
            ErrorType::Blacklisted => {
                // Fail blacklisted email addresses.
                const RESP: &str = "{\"type\":\"DisallowedContactSupport\", \"note\":\"If you see this messages right here, you're probably doing something you shouldn't be.\"}";

                return Response::build()
                    .status(Status::Unauthorized)
                    .sized_body(RESP.len(), std::io::Cursor::new(RESP))
                    .ok();
            }
            ErrorType::LockedOut => Status::Forbidden,
            ErrorType::TotpAlreadyEnabled => Status::BadRequest,
            ErrorType::DisallowedMFAMethod => Status::BadRequest,
            ErrorType::AgeRequirementNotMet => Status::Forbidden,
            ErrorType::AgeChangeNotAllowed => Status::Forbidden,
            ErrorType::AgeRestricted => Status::Forbidden,
            ErrorType::ForwardMatureContent => Status::Forbidden,
            ErrorType::PollEnded => Status::Conflict,
            ErrorType::OperationFailed => Status::InternalServerError,
            ErrorType::IncorrectData { .. } => Status::BadRequest,
        };

        // Serialize the error data structure into JSON.
        let string = serde_json::to_string(&self).unwrap();

        // Build and send the request.
        Response::build()
            .sized_body(string.len(), Cursor::new(string))
            .header(ContentType::new("application", "json"))
            .status(status)
            .ok()
    }
}
