use crate::v0::{Member, PartialChannel, PartialEmoji, PartialMember, PartialRole, PartialServer, User};
use revolt_permissions::Override;

auto_derived!(
    /// Audit log entry
    pub struct AuditLogEntry {
        /// Unique ID
        #[serde(rename = "_id")]
        pub id: String,

        /// The server the entry happened in
        pub server: String,
        /// User provided reason
        pub reason: Option<String>,
        /// User who ran the action
        pub user: String,
        /// User this action is targetting
        pub target: Option<String>,
        /// The action ran
        pub action: AuditLogEntryAction,
    }

    /// Indivual action stored on the audit log
    #[serde(tag = "type")]
    #[allow(clippy::large_enum_variant)]
    pub enum AuditLogEntryAction {
        MessageDelete {
            author: String,
            channel: String,
        },
        MessageBulkDelete {
            channel: String,
            count: usize,
        },
        MessagePin {
            message: String,
            author: String,
            channel: String,
        },
        MessageUnpin {
            message: String,
            author: String,
            channel: String,
        },
        BanCreate {
            user: String,
        },
        BanDelete {
            user: String,
        },
        ChannelCreate {
            channel: String,
            name: String,
        },
        ChannelEdit {
            channel: String,
            before: PartialChannel,
            after: PartialChannel,
        },
        ChannelRolePermissionsEdit {
            channel: String,
            role: String,
            permissions: Override,
        },
        ChannelMemberPermissionsEdit {
            channel: String,
            member: String,
            permissions: Override,
        },
        ChannelDelete {
            channel: String,
            name: String,
        },
        MemberEdit {
            user: String,
            before: PartialMember,
            after: PartialMember,
        },
        MemberKick {
            user: String,
        },
        ServerEdit {
            before: PartialServer,
            after: PartialServer,
        },
        RoleEdit {
            role: String,
            before: PartialRole,
            after: PartialRole,
        },
        RoleCreate {
            role: String,
            name: String,
        },
        RoleDelete {
            role: String,
            name: String,
        },
        RolesReorder {
            before: Vec<String>,
            after: Vec<String>,
        },
        InviteCreate {
            invite: String,
            channel: String,
        },
        InviteDelete {
            invite: String,
            channel: String,
        },
        WebhookCreate {
            webhook: String,
            name: String,
            channel: String,
        },
        WebhookDelete {
            webhook: String,
            name: String,
            channel: String,
        },
        WebhookUpdate {
            webhook: String,
            name: String,
            channel: String,
        },
        WebhookTokenReset {
            webhook: String,
            name: String,
            channel: String,
        },
        EmojiCreate {
            emoji: String,
            name: String,
        },
        EmojiUpdate {
            emoji: String,
            before: PartialEmoji,
            after: PartialEmoji,
        },
        EmojiDelete {
            emoji: String,
            name: String,
        },
        /// A soundboard sound was added
        SoundboardSoundCreate {
            sound: String,
            name: String,
        },
        /// A soundboard sound was removed
        SoundboardSoundDelete {
            sound: String,
            name: String,
        },
        /// A stage was closed (everyone left the call)
        StageEnd {
            channel: String,
        },
        /// A bot was added to the server
        BotAdd {
            bot: String,
            /// Permissions granted through its managed role (0 for none)
            permissions: i64,
            scopes: Vec<String>,
        },
        /// A bot was removed from the server
        BotRemove {
            bot: String,
        },
        /// Who may use a bot's commands changed (`command` absent: the whole bot)
        CommandPermissionsUpdate {
            bot: String,
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
            command: Option<String>,
            /// The rules before the change
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            before: Vec<super::CommandPermission>,
            /// The rules after it
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty", default))]
            after: Vec<super::CommandPermission>,
        },
        /// AutoMod acted on a message (actions: block, alert, timeout)
        AutoModAction {
            rule: String,
            user: String,
            channel: String,
            actions: Vec<String>,
        },
        /// A scheduled event was created
        ServerEventCreate {
            event: String,
            title: String,
        },
        /// A scheduled event was changed
        ServerEventEdit {
            event: String,
            title: String,
        },
        /// A scheduled event was called off
        ServerEventCancel {
            event: String,
            title: String,
        },
        /// A scheduled event was deleted
        ServerEventDelete {
            event: String,
            title: String,
        },
    }

    /// Audit log query filters
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    #[cfg_attr(feature = "rocket", derive(rocket::FromForm))]
    pub struct OptionsAuditLogQuery {
        /// Filter by who ran the action
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub user: Option<String>,
        /// Filter by who the action is targetting
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub target: Option<String>,
        /// Filter by the action type
        pub r#type: Option<Vec<String>>,
        /// Entries before a certain entry id
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub before: Option<String>,
        /// Entries after a certain entry id
        #[cfg_attr(feature = "validator", validate(length(min = 26, max = 26)))]
        pub after: Option<String>,
        /// Maximum number of entries to fetch
        #[cfg_attr(feature = "validator", validate(range(min = 1, max = 100)))]
        pub limit: Option<i64>,
    }

    /// Response containing the audit log entries and the users involved
    pub struct AuditLogQueryResponse {
        /// List of audit logs
        pub audit_logs: Vec<AuditLogEntry>,
        /// List of users
        pub users: Vec<User>,
        /// List of members
        pub members: Vec<Member>,
    }
);
