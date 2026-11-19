use super::{Message, User};

auto_derived!(
    /// One entry of a person's private saved list
    pub struct SavedMessage {
        /// Id of the saved message
        pub message: String,
        /// Channel the message is in
        pub channel: String,
        /// When it was saved, in milliseconds since the epoch
        pub saved_at: i64,
        /// Whether the person can still read the message. When false (the
        /// message is gone or they lost access) the answer carries nothing
        /// of the message.
        pub available: bool,
    }

    /// A person's saved list, newest first
    pub struct SavedMessagesResponse {
        /// The entries of this page
        pub entries: Vec<SavedMessage>,
        /// The messages of the available entries
        pub messages: Vec<Message>,
        /// The authors of those messages
        pub users: Vec<User>,
        /// How many messages the person has saved in all
        pub total: usize,
        /// How many they may keep saved
        pub max: usize,
    }

    /// A reminder a person set on a message
    pub struct MessageReminder {
        /// Reminder id
        #[serde(rename = "_id")]
        pub id: String,
        /// Id of the message
        pub message: String,
        /// Channel the message is in
        pub channel: String,
        /// When it is due, in milliseconds since the epoch
        pub due_at: i64,
        /// When it was set
        pub created_at: i64,
        /// When it was delivered; absent while it waits
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub delivered_at: Option<i64>,
        /// Whether the person can still read the message (see `SavedMessage`)
        pub available: bool,
    }

    /// A person's reminders: waiting ones soonest first, then delivered ones
    /// newest first
    pub struct MessageRemindersResponse {
        pub reminders: Vec<MessageReminder>,
        /// The messages of the available reminders
        pub messages: Vec<Message>,
        /// The authors of those messages
        pub users: Vec<User>,
        /// How many reminders this person may have waiting at once
        pub max_active: usize,
        /// The same number with Feather
        pub max_active_plus: usize,
    }

    /// Set a reminder
    pub struct DataCreateReminder {
        /// Id of the message
        pub message: String,
        /// When to remind, in milliseconds since the epoch
        pub due_at: i64,
    }

    /// Move a waiting reminder
    pub struct DataEditReminder {
        /// When to remind, in milliseconds since the epoch
        pub due_at: i64,
    }

    /// What a delivered reminder carries to the person's sessions
    pub struct MessageReminderDue {
        pub reminder: MessageReminder,
        /// The message, when it can still be read
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub message: Option<Message>,
        /// Its author, when the message is there
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub user: Option<User>,
    }
);
