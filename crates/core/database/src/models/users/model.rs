use std::{
    collections::{BTreeMap, HashSet},
    str::FromStr,
    time::Duration,
};

use crate::{
    events::client::EventV1,
    util::email::{email_templates, send_email},
    Database, File, RatelimitEvent, UserPlus, AMQP,
};

use futures::future::join_all;
use iso8601_timestamp::Timestamp;
use once_cell::sync::Lazy;
use rand::seq::SliceRandom;
use regex::{Regex, RegexBuilder};
use revolt_config::{config, FeaturesLimits};
use revolt_models::v0::{self, UserBadges, UserFlags};
use revolt_presence::filter_online;
use revolt_result::{create_error, Result};
use serde_json::json;
use ulid::Ulid;

auto_derived_partial!(
    /// # User
    pub struct User {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Username
        pub username: String,
        /// Discriminator
        pub discriminator: String,
        /// Display name
        #[serde(skip_serializing_if = "Option::is_none")]
        pub display_name: Option<String>,
        /// User's pronouns
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub pronouns: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        /// Avatar attachment
        pub avatar: Option<File>,
        /// Relationships with other users
        #[serde(skip_serializing_if = "Option::is_none")]
        pub relations: Option<Vec<Relationship>>,

        /// Bitfield of user badges
        #[serde(skip_serializing_if = "Option::is_none")]
        pub badges: Option<i32>,
        /// User's current status
        #[serde(skip_serializing_if = "Option::is_none")]
        pub status: Option<UserStatus>,
        /// User's profile page
        #[serde(skip_serializing_if = "Option::is_none")]
        pub profile: Option<UserProfile>,

        /// Enum of user flags
        #[serde(skip_serializing_if = "Option::is_none")]
        pub flags: Option<i32>,
        /// Whether this user is privileged
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub privileged: bool,
        /// Whether this is an official Zrzur account. Written only by the
        /// seed and the two-person staff route, never by a public API
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub official: bool,
        /// Whether this is a verified official bot: a bot Zrzur runs itself.
        /// Not the same as `official`; people use it like any bot. Written
        /// only by `zrzur-staff` on the server, never by an API
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub official_bot: bool,
        /// Name of an official account per language (`ar`, `en`, `de`);
        /// `display_name` is the fallback
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub official_names: Option<BTreeMap<String, String>>,
        /// Bot information
        #[serde(skip_serializing_if = "Option::is_none")]
        pub bot: Option<BotInformation>,
        /// Profile look (theme, name style, decoration)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub style: Option<v0::UserStyle>,
        /// Zrzur Feather subscription state (never sent to other users)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub plus: Option<UserPlus>,
        /// Who can message this user or add them to groups (never sent to other users)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub privacy: Option<v0::UserPrivacy>,
        /// Language the person uses the app in, e.g. `ar` or `de` (never
        /// sent to other users); messages from Zrzur are written in it
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub locale: Option<String>,

        /// Time until user is unsuspended
        #[serde(skip_serializing_if = "Option::is_none")]
        pub suspended_until: Option<Timestamp>,
        /// Last acknowledged policy change
        pub last_acknowledged_policy_change: Timestamp,
    },
    "PartialUser"
);

auto_derived!(
    /// Optional fields on user object
    pub enum FieldsUser {
        Avatar,
        StatusText,
        StatusPresence,
        ProfileContent,
        ProfileBackground,
        DisplayName,
        Pronouns,
        Style,

        // internal fields
        Suspension,
        None,
    }

    /// User's relationship with another user (or themselves)
    pub enum RelationshipStatus {
        None,
        User,
        Friend,
        Outgoing,
        Incoming,
        Blocked,
        BlockedOther,
    }

    /// Relationship entry indicating current status with other user
    pub struct Relationship {
        #[serde(rename = "_id")]
        pub id: String,
        pub status: RelationshipStatus,
    }

    /// Presence status
    pub enum Presence {
        /// User is online
        Online,
        /// User is not currently available
        Idle,
        /// User is focusing / will only receive mentions
        Focus,
        /// User is busy / will not receive any notifications
        Busy,
        /// User appears to be offline
        Invisible,
    }

    /// User's active status
    #[derive(Default)]
    pub struct UserStatus {
        /// Custom status text
        #[serde(skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
        /// Emoji before the text: unicode or a custom emoji id
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub emoji: Option<String>,
        /// When the text and emoji run out
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub expires_at: Option<Timestamp>,
        /// Current presence option
        #[serde(skip_serializing_if = "Option::is_none")]
        pub presence: Option<Presence>,
    }

    /// User's profile
    #[derive(Default)]
    pub struct UserProfile {
        /// Text content on user's profile
        #[serde(skip_serializing_if = "Option::is_none")]
        pub content: Option<String>,
        /// Background visible on user's profile
        #[serde(skip_serializing_if = "Option::is_none")]
        pub background: Option<File>,
        /// Links to other sites, checked on the way in
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub links: Vec<String>,
        /// Interest ids from the fixed list, for showing only
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub interests: Vec<String>,
        /// Section ids in the order the person chose
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub sections: Option<Vec<String>>,
        /// Section ids the person turned off
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        pub hidden_sections: Vec<String>,
    }

    /// Bot information for if the user is a bot
    pub struct BotInformation {
        /// Id of the owner of this bot
        pub owner: String,
        /// Verified by Trust & Safety (shown on the profile)
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub verified: bool,
    }

    /// Enumeration providing a hint to the type of user we are handling
    pub enum UserHint {
        /// Could be either a user or a bot
        Any,
        /// Only match bots
        Bot,
        /// Only match users
        User,
    }
);

pub static DISCRIMINATOR_SEARCH_SPACE: Lazy<HashSet<String>> = Lazy::new(|| {
    let mut set = (2..9999)
        .map(|v| format!("{:0>4}", v))
        .collect::<HashSet<String>>();

    for discrim in [
        123, 1234, 1111, 2222, 3333, 4444, 5555, 6666, 7777, 8888, 9999, 1488,
    ] {
        set.remove(&format!("{:0>4}", discrim));
    }

    set.into_iter().collect()
});

static BLOCKED_USERNAME_PATTERNS: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new("`{3}|(discord|rvlt|guilded|stt)\\.gg|(revolt|stoat)\\.chat|https?:\\/\\/")
        .case_insensitive(true)
        .build()
        .unwrap()
});

static USERNAME_CODEPOINT_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new("^(\\p{L}|[\\d_.-])+$")
        .case_insensitive(true)
        .build()
        .unwrap()
});

#[allow(clippy::derivable_impls)]
impl Default for User {
    fn default() -> Self {
        Self {
            id: Default::default(),
            username: Default::default(),
            discriminator: Default::default(),
            display_name: Default::default(),
            pronouns: Default::default(),
            avatar: Default::default(),
            relations: Default::default(),
            badges: Default::default(),
            status: Default::default(),
            profile: Default::default(),
            flags: Default::default(),
            privileged: Default::default(),
            official: Default::default(),
            official_bot: Default::default(),
            official_names: Default::default(),
            bot: Default::default(),
            style: Default::default(),
            plus: None,
            privacy: None,
            locale: None,
            suspended_until: Default::default(),
            last_acknowledged_policy_change: Timestamp::UNIX_EPOCH,
        }
    }
}

#[allow(clippy::disallowed_methods)]
impl User {
    /// Create a new user
    pub async fn create<I, D>(
        db: &Database,
        username: String,
        account_id: I,
        data: D,
    ) -> Result<User>
    where
        I: Into<Option<String>>,
        D: Into<Option<PartialUser>>,
    {
        let new_username = User::sanitise_username(&username).await?;
        User::validate_username(&new_username)?;

        let mut user = User {
            id: account_id.into().unwrap_or_else(|| Ulid::new().to_string()),
            discriminator: User::find_discriminator(db, &new_username, None).await?,
            username: new_username.clone(),
            last_acknowledged_policy_change: Timestamp::now_utc(),
            ..Default::default()
        };

        if let Some(data) = data.into() {
            user.apply_options(data);
        }

        db.insert_user(&user).await?;
        Ok(user)
    }

    /// Whether Plus is active for this user (and billing is available)
    pub async fn has_plus(&self) -> bool {
        let billing = config().await.features.billing;
        // sandbox or simulated Plus never counts on a live server, and
        // live Plus never on a test server
        self.plus
            .as_ref()
            .is_some_and(|plus| plus.active() && billing.counts(!plus.live))
    }

    /// Get limits for this user
    pub async fn limits(&self) -> FeaturesLimits {
        let config = config().await;
        if self.has_plus().await {
            if let Some(plus) = config.features.limits.roles.get("plus") {
                return plus.clone();
            }
        }

        if ulid::Ulid::from_str(&self.id)
            .expect("`ulid`")
            .datetime()
            .elapsed()
            .expect("time went backwards")
            <= Duration::from_secs(3600u64 * config.features.limits.global.new_user_hours as u64)
        {
            config.features.limits.new_user
        } else {
            config.features.limits.default
        }
    }

    /// Get the relationship with another user
    pub fn relationship_with(&self, user_b: &str) -> RelationshipStatus {
        if self.id == user_b {
            return RelationshipStatus::User;
        }

        if let Some(relations) = &self.relations {
            if let Some(relationship) = relations.iter().find(|x| x.id == user_b) {
                return relationship.status.clone();
            }
        }

        RelationshipStatus::None
    }

    pub fn is_friends_with(&self, user_b: &str) -> bool {
        matches!(
            self.relationship_with(user_b),
            RelationshipStatus::Friend | RelationshipStatus::User
        )
    }

    /// Check whether two users have a mutual connection
    ///
    /// This will check if user and user_b share a server or a group.
    pub async fn has_mutual_connection(&self, db: &Database, user_b: &str) -> Result<bool> {
        Ok(!db
            .fetch_mutual_server_ids(&self.id, user_b)
            .await?
            .is_empty()
            || !db
                .fetch_mutual_channel_ids(&self.id, user_b)
                .await?
                .is_empty())
    }

    /// Check whether two users share a server or a group
    ///
    /// Unlike [User::has_mutual_connection], a direct message between
    /// them does not count.
    pub async fn shares_server_or_group(&self, db: &Database, user_b: &str) -> Result<bool> {
        if !db
            .fetch_mutual_server_ids(&self.id, user_b)
            .await?
            .is_empty()
        {
            return Ok(true);
        }

        // Two users have at most one direct message, so any second
        // channel is a group
        let channels = db.fetch_mutual_channel_ids(&self.id, user_b).await?;
        match channels.len() {
            0 => Ok(false),
            1 => Ok(matches!(
                db.fetch_channel(&channels[0]).await?,
                crate::Channel::Group { .. }
            )),
            _ => Ok(true),
        }
    }

    /// This user's stored privacy settings (defaults if never set)
    pub fn privacy(&self) -> v0::UserPrivacy {
        self.privacy.clone().unwrap_or_default()
    }

    /// This user's privacy settings as enforced: until they choose
    /// something themselves, people under 18 only get messages from friends
    pub async fn effective_privacy(&self, db: &Database) -> v0::UserPrivacy {
        if let Some(privacy) = &self.privacy {
            return privacy.clone();
        }

        let mut privacy = v0::UserPrivacy::default();
        if let Ok(account) = db.fetch_account(&self.id).await {
            if matches!(
                account.age_band(),
                v0::AgeBand::Teen | v0::AgeBand::UnderMinimum
            ) {
                privacy.direct_messages = v0::DirectMessagePrivacy::Friends;
            }
        }

        privacy
    }

    /// Whether `by` may start a direct message with this user
    ///
    /// Friendship, blocks and bots are handled by the permission
    /// calculation; this only looks at the privacy setting.
    pub async fn accepts_direct_messages_from(&self, db: &Database, by: &User) -> bool {
        match self.effective_privacy(db).await.direct_messages {
            v0::DirectMessagePrivacy::Everyone => true,
            v0::DirectMessagePrivacy::Mutual => self
                .shares_server_or_group(db, &by.id)
                .await
                .unwrap_or_default(),
            v0::DirectMessagePrivacy::Friends => self.is_friends_with(&by.id),
        }
    }

    /// Check whether `by` may add this user to a group chat
    pub fn can_be_added_to_group_by(&self, by: &User) -> Result<()> {
        if self.id == by.id {
            return Ok(());
        }

        if matches!(
            by.relationship_with(&self.id),
            RelationshipStatus::Blocked | RelationshipStatus::BlockedOther
        ) {
            return Err(create_error!(GroupInvitesRestricted));
        }

        let allowed = match self.privacy().group_invites {
            v0::GroupInvitePrivacy::Everyone => true,
            v0::GroupInvitePrivacy::Friends => by.is_friends_with(&self.id),
            v0::GroupInvitePrivacy::Nobody => false,
        };

        if allowed {
            Ok(())
        } else {
            Err(create_error!(GroupInvitesRestricted))
        }
    }

    /// Check if this user can acquire another server
    pub async fn can_acquire_server(&self, db: &Database) -> Result<()> {
        if db.fetch_server_count(&self.id).await? <= self.limits().await.servers {
            Ok(())
        } else {
            Err(create_error!(TooManyServers {
                max: self.limits().await.servers
            }))
        }
    }

    /// Validate a username
    ///
    /// This will check if the username is a blocked name or contains a blocked pattern.
    fn validate_username(username: &str) -> Result<()> {
        if !USERNAME_CODEPOINT_REGEX.is_match(username)
            || !validator::validate_length(username, Some(2), Some(32), None)
        {
            return Err(create_error!(InvalidUsername));
        }

        let username_lowercase = username.to_lowercase();

        const BLOCKED_USERNAMES: &[&str] = &["admin", "revolt", "stoat", "zrzur"];
        if BLOCKED_USERNAMES.contains(&username_lowercase.as_str())
            // names that pass for Zrzur
            || crate::passes_for_official(username)
            || BLOCKED_USERNAME_PATTERNS.is_match(username)
        {
            return Err(create_error!(InvalidUsername));
        }

        Ok(())
    }

    /// Sanitise a username
    ///
    /// This will clean up Unicode homoglyphs and pad to the min username length with underscores.
    async fn sanitise_username(username: &str) -> Result<String> {
        // the cure below turns Arabic letters into something else, so the
        // name rule looks at what the person typed as well
        if crate::passes_for_official(username) {
            return Err(create_error!(InvalidUsername));
        }

        let options = decancer::Options::default().retain_capitalization();
        let mut username = decancer::cure(username, options)
            .map_err(|_| create_error!(InvalidUsername))?
            .to_string();

        let config = revolt_config::config().await;
        let username_length_diff = config
            .api
            .users
            .min_username_length
            .saturating_sub(username.len());
        if username_length_diff > 0 {
            username.push_str(&"_".repeat(username_length_diff))
        }

        Ok(username)
    }

    /// What a failed token lookup means to the caller: a token nothing
    /// matches is an invalid session; anything else (the database is down)
    /// stays the error it is, so a good session is never reported as refused
    pub(crate) fn token_lookup_error(error: revolt_result::Error) -> revolt_result::Error {
        match error.error_type {
            revolt_result::ErrorType::NotFound => create_error!(InvalidSession),
            _ => error,
        }
    }

    /// Find a user and session ID from a given token and hint
    #[async_recursion]
    pub async fn from_token(db: &Database, token: &str, hint: UserHint) -> Result<(User, String)> {
        match hint {
            UserHint::Bot => {
                let bot = db
                    .fetch_bot_by_token(token)
                    .await
                    .map_err(User::token_lookup_error)?;
                if crate::bot_suspended(&bot) {
                    return Err(create_error!(InvalidSession));
                }
                Ok((db.fetch_user(&bot.id).await?, String::new()))
            }
            UserHint::User => {
                let session = crate::Session::from_token(db, token).await?;
                Ok((db.fetch_user(&session.user_id).await?, session.id))
            }
            UserHint::Any => {
                match User::from_token(db, token, UserHint::User).await {
                    Ok(result) => Ok(result),
                    // not a user session: it may be a bot token
                    Err(error) if User::may_be_bot_token(&error) => {
                        User::from_token(db, token, UserHint::Bot).await
                    }
                    // the lookup itself failed: say so instead of guessing
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// Whether a failed user lookup only means "no such session", so a bot
    /// token is worth trying (a database failure is not)
    pub(crate) fn may_be_bot_token(error: &revolt_result::Error) -> bool {
        matches!(
            error.error_type,
            revolt_result::ErrorType::InvalidSession | revolt_result::ErrorType::NotFound
        )
    }

    /// Helper function to fetch many users as a mutually connected user
    /// (while optimising the online ID query)
    pub async fn fetch_many_ids_as_mutuals(
        db: &Database,
        perspective: &User,
        ids: &[String],
    ) -> Result<Vec<v0::User>> {
        let online_ids = filter_online(ids).await;

        let mut users = join_all(db.fetch_users(ids).await?.into_iter().map(|user| async {
            let is_online = online_ids.contains(&user.id);
            user.into_known(perspective, is_online).await
        }))
        .await;

        // a custom status emoji shows only from a server the viewer shares:
        // the viewer's servers are looked up once, and only when needed
        use crate::util::status_emoji;
        if status_emoji::any_custom(users.iter().map(|user| user.status.as_ref())) {
            let ours: HashSet<String> = db
                .fetch_all_memberships(&perspective.id)
                .await?
                .into_iter()
                .map(|member| member.id.server)
                .collect();
            for user in &mut users {
                let owner = user.id.clone();
                status_emoji::hide_for_member_of(
                    db,
                    &mut user.status,
                    &owner,
                    &perspective.id,
                    &|server| ours.contains(server),
                )
                .await;
            }
        }

        Ok(users)
    }

    /// Find a free discriminator for a given username
    pub async fn find_discriminator(
        db: &Database,
        username: &str,
        preferred: Option<(String, String)>,
    ) -> Result<String> {
        let search_space: &HashSet<String> = &DISCRIMINATOR_SEARCH_SPACE;
        let used_discriminators: HashSet<String> = db
            .fetch_discriminators_in_use(username)
            .await?
            .into_iter()
            .collect();

        let available_discriminators: Vec<&String> =
            search_space.difference(&used_discriminators).collect();

        if available_discriminators.is_empty() {
            return Err(create_error!(UsernameTaken));
        }

        if let Some((preferred, target_id)) = preferred {
            if available_discriminators.contains(&&preferred) {
                return Ok(preferred);
            } else {
                if db
                    .has_ratelimited(
                        &target_id,
                        crate::RatelimitEventType::DiscriminatorChange,
                        Duration::from_secs(60 * 60 * 24),
                        1,
                    )
                    .await?
                {
                    return Err(create_error!(DiscriminatorChangeRatelimited));
                }

                RatelimitEvent::create(
                    db,
                    target_id,
                    crate::RatelimitEventType::DiscriminatorChange,
                )
                .await?;
            }
        }

        let mut rng = rand::thread_rng();
        Ok(available_discriminators
            .choose(&mut rng)
            .expect("we can assert this has an element")
            .to_string())
    }

    /// Update a user's username
    pub async fn update_username(&mut self, db: &Database, username: String) -> Result<()> {
        let new_username = User::sanitise_username(&username).await?;
        User::validate_username(&new_username)?;

        if self.username.to_lowercase() == new_username.to_lowercase() {
            self.update(
                db,
                PartialUser {
                    username: Some(new_username),
                    ..Default::default()
                },
                vec![],
            )
            .await
        } else {
            self.update(
                db,
                PartialUser {
                    discriminator: Some(
                        User::find_discriminator(
                            db,
                            &new_username,
                            Some((self.discriminator.to_string(), self.id.clone())),
                        )
                        .await?,
                    ),
                    username: Some(new_username),
                    ..Default::default()
                },
                vec![],
            )
            .await
        }
    }

    /// Set a relationship to another user
    pub async fn set_relationship(
        &mut self,
        db: &Database,
        user_b: &User,
        status: RelationshipStatus,
    ) -> Result<()> {
        db.set_relationship(&self.id, &user_b.id, &status).await?;

        if let RelationshipStatus::None | RelationshipStatus::User = status {
            if let Some(relations) = &mut self.relations {
                relations.retain(|relation| relation.id != user_b.id);
            }
        } else {
            let relation = Relationship {
                id: user_b.id.to_string(),
                status,
            };

            if let Some(relations) = &mut self.relations {
                relations.retain(|relation| relation.id != user_b.id);
                relations.push(relation);
            } else {
                self.relations = Some(vec![relation]);
            }
        }

        Ok(())
    }

    /// Apply a certain relationship between two users
    pub async fn apply_relationship(
        &mut self,
        db: &Database,
        target: &mut User,
        local: RelationshipStatus,
        remote: RelationshipStatus,
    ) -> Result<()> {
        target.set_relationship(db, self, remote).await?;
        self.set_relationship(db, target, local).await?;

        EventV1::UserRelationship {
            id: target.id.clone(),
            user: self.clone().into(db, Some(&*target)).await,
        }
        .p(target.id.clone())
        .await;

        EventV1::UserRelationship {
            id: self.id.clone(),
            user: target.clone().into(db, Some(&*self)).await,
        }
        .p(self.id.clone())
        .await;

        Ok(())
    }

    /// Add another user as a friend
    pub async fn add_friend(
        &mut self,
        db: &Database,
        amqp: &AMQP,
        target: &mut User,
    ) -> Result<()> {
        // official accounts take no friends
        if target.official || self.official {
            return Err(create_error!(OfficialAccount));
        }
        match self.relationship_with(&target.id) {
            RelationshipStatus::User => Err(create_error!(NoEffect)),
            RelationshipStatus::Friend => Err(create_error!(AlreadyFriends)),
            RelationshipStatus::Outgoing => Err(create_error!(AlreadySentRequest)),
            RelationshipStatus::Blocked => Err(create_error!(Blocked)),
            RelationshipStatus::BlockedOther => Err(create_error!(BlockedByOther)),
            RelationshipStatus::Incoming => {
                // Accept incoming friend request
                _ = amqp.friend_request_accepted(self, target).await;

                self.apply_relationship(
                    db,
                    target,
                    RelationshipStatus::Friend,
                    RelationshipStatus::Friend,
                )
                .await
            }
            RelationshipStatus::None => {
                // Get this user's current count of outgoing friend requests
                let count = self
                    .relations
                    .as_ref()
                    .map(|relations| {
                        relations
                            .iter()
                            .filter(|r| matches!(r.status, RelationshipStatus::Outgoing))
                            .count()
                    })
                    .unwrap_or_default();

                // If we're over the limit, don't allow creating more requests
                if count >= self.limits().await.outgoing_friend_requests {
                    return Err(create_error!(TooManyPendingFriendRequests {
                        max: self.limits().await.outgoing_friend_requests
                    }));
                }

                _ = amqp.friend_request_received(target, self).await;

                // Send the friend request
                self.apply_relationship(
                    db,
                    target,
                    RelationshipStatus::Outgoing,
                    RelationshipStatus::Incoming,
                )
                .await
            }
        }
    }

    /// Remove another user as a friend
    pub async fn remove_friend(&mut self, db: &Database, target: &mut User) -> Result<()> {
        match self.relationship_with(&target.id) {
            RelationshipStatus::Friend
            | RelationshipStatus::Outgoing
            | RelationshipStatus::Incoming => {
                self.apply_relationship(
                    db,
                    target,
                    RelationshipStatus::None,
                    RelationshipStatus::None,
                )
                .await
            }
            _ => Err(create_error!(NoEffect)),
        }
    }

    /// Block another user
    pub async fn block_user(&mut self, db: &Database, target: &mut User) -> Result<()> {
        // official messages always reach the person (they can mute them)
        if target.official {
            return Err(create_error!(OfficialAccount));
        }
        match self.relationship_with(&target.id) {
            RelationshipStatus::User | RelationshipStatus::Blocked => Err(create_error!(NoEffect)),
            RelationshipStatus::BlockedOther => {
                self.apply_relationship(
                    db,
                    target,
                    RelationshipStatus::Blocked,
                    RelationshipStatus::Blocked,
                )
                .await
            }
            RelationshipStatus::None
            | RelationshipStatus::Friend
            | RelationshipStatus::Incoming
            | RelationshipStatus::Outgoing => {
                self.apply_relationship(
                    db,
                    target,
                    RelationshipStatus::Blocked,
                    RelationshipStatus::BlockedOther,
                )
                .await
            }
        }
    }

    /// Unblock another user
    pub async fn unblock_user(&mut self, db: &Database, target: &mut User) -> Result<()> {
        match self.relationship_with(&target.id) {
            RelationshipStatus::Blocked => match target.relationship_with(&self.id) {
                RelationshipStatus::Blocked => {
                    self.apply_relationship(
                        db,
                        target,
                        RelationshipStatus::BlockedOther,
                        RelationshipStatus::Blocked,
                    )
                    .await
                }
                RelationshipStatus::BlockedOther => {
                    self.apply_relationship(
                        db,
                        target,
                        RelationshipStatus::None,
                        RelationshipStatus::None,
                    )
                    .await
                }
                _ => Err(create_error!(InternalError)),
            },
            _ => Err(create_error!(NoEffect)),
        }
    }

    /// Update user data
    pub async fn update(
        &mut self,
        db: &Database,
        partial: PartialUser,
        remove: Vec<FieldsUser>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field);
        }

        self.apply_options(partial.clone());
        db.update_user(&self.id, &partial, remove.clone()).await?;

        EventV1::UserUpdate {
            id: self.id.clone(),
            data: partial.into(),
            clear: remove.into_iter().map(|v| v.into()).collect(),
            event_id: Some(Ulid::new().to_string()),
        }
        .p(self.id.clone())
        .await;

        Ok(())
    }

    /// Clear custom statuses whose expiry has passed, at most 1000 a pass
    /// (crond runs it every minute), and tell the gateway about each one
    /// this pass really cleared, so other clients drop it too. Reads already
    /// hide an expired status, so this only tidies storage and clients that
    /// hold the old value. Safe to run twice.
    pub async fn clear_expired_statuses(db: &Database) -> Result<usize> {
        let now = Timestamp::now_utc();
        let mut cleared = 0;
        for id in db.fetch_expired_status_ids(now, 1000).await? {
            // only a status this pass really cleared is announced: one set
            // between the look and the write stays, and so does its event
            if !db.clear_expired_status(&id, now).await? {
                continue;
            }
            cleared += 1;
            EventV1::UserUpdate {
                id: id.clone(),
                data: v0::PartialUser::default(),
                clear: vec![v0::FieldsUser::StatusText],
                event_id: Some(Ulid::new().to_string()),
            }
            .p(id)
            .await;
        }
        Ok(cleared)
    }

    /// Remove a field from User object
    pub fn remove_field(&mut self, field: &FieldsUser) {
        match field {
            FieldsUser::Avatar => self.avatar = None,
            // the emoji and expiry belong to the text and go with it
            FieldsUser::StatusText => {
                if let Some(x) = self.status.as_mut() {
                    x.text = None;
                    x.emoji = None;
                    x.expires_at = None;
                }
            }
            FieldsUser::StatusPresence => {
                if let Some(x) = self.status.as_mut() {
                    x.presence = None;
                }
            }
            FieldsUser::ProfileContent => {
                if let Some(x) = self.profile.as_mut() {
                    x.content = None;
                }
            }
            FieldsUser::ProfileBackground => {
                if let Some(x) = self.profile.as_mut() {
                    x.background = None;
                }
            }
            FieldsUser::DisplayName => self.display_name = None,
            FieldsUser::Pronouns => self.pronouns = None,
            FieldsUser::Style => self.style = None,
            FieldsUser::Suspension => self.suspended_until = None,
            FieldsUser::None => {}
        }
    }

    /// Suspend the user
    ///
    /// - If a duration is specified, the user will be automatically unsuspended after the given time.
    /// - If a reason is specified, an email will be sent.
    pub async fn suspend(
        &mut self,
        db: &Database,
        duration_days: Option<usize>,
        reason: Option<Vec<String>>,
    ) -> Result<()> {
        let mut account = db.fetch_account(&self.id).await?;

        account.disable(db).await?;

        account.delete_all_sessions(db, None).await?;

        self.update(
            db,
            PartialUser {
                flags: Some(UserFlags::SuspendedUntil as i32),
                suspended_until: duration_days.and_then(|dur| {
                    Timestamp::now_utc().checked_add(iso8601_timestamp::Duration::days(dur as i64))
                }),
                ..Default::default()
            },
            vec![],
        )
        .await?;

        if let Some(reason) = reason {
            let config = config().await;

            if !config.api.smtp.host.is_empty() {
                let templates = email_templates().await;

                send_email(
                    &config.api.smtp,
                    account.email.clone(),
                    &templates.suspension,
                    json!({
                        "email": account.email,
                        "list": reason.join(", "),
                        "duration": duration_days,
                        "duration_display": if duration_days.is_some() {
                            "block"
                        } else {
                            "none"
                        }
                    }),
                )
                .map_err(|_| create_error!(InternalError))?;
            }
        }

        Ok(())
    }

    /// Unsuspend the user
    pub async fn unsuspend(&mut self, db: &Database) -> Result<()> {
        let mut account = db.fetch_account(&self.id).await?;
        if account.disabled {
            account.disabled = false;
            account.save(db).await?;
        }

        self.update(
            db,
            PartialUser {
                flags: Some(self.flags.unwrap_or(0) & !(UserFlags::SuspendedUntil as i32)),
                suspended_until: None,
                ..Default::default()
            },
            vec![FieldsUser::Suspension],
        )
        .await
    }

    /// Permanently ban the user
    ///
    /// - If a reason is specified, an email will be sent.
    pub async fn ban(&mut self, _db: &Database, _reason: Option<String>) -> Result<()> {
        // Send ban email (if reason provided)
        unimplemented!()
    }

    /// Mark as deleted
    pub async fn mark_deleted(&mut self, db: &Database) -> Result<()> {
        // The picture and banner files go to the file clean-up (crond)
        let files: Vec<String> = self
            .avatar
            .iter()
            .chain(self.profile.as_ref().and_then(|p| p.background.as_ref()))
            .map(|file| file.id.clone())
            .collect();
        for id in files {
            db.mark_attachment_as_deleted(&id).await.ok();
        }

        self.update(
            db,
            PartialUser {
                username: Some(format!("Deleted User {}", self.id)),
                discriminator: Some("0000".to_string()),
                flags: Some(2),
                relations: Some(Vec::new()),
                ..Default::default()
            },
            vec![
                FieldsUser::Avatar,
                FieldsUser::StatusText,
                FieldsUser::StatusPresence,
                FieldsUser::ProfileContent,
                FieldsUser::ProfileBackground,
                FieldsUser::DisplayName,
                FieldsUser::Pronouns,
                FieldsUser::Style,
                FieldsUser::Suspension,
            ],
        )
        .await?;

        // Own settings, language, privacy choices, Feather state and badges.
        // A failure must surface: the account then stays due and crond retries.
        db.delete_user_settings(&self.id).await?;
        db.scrub_user_fields(&self.id).await?;
        self.locale = None;
        self.privacy = None;
        self.profile = None;
        self.plus = None;
        self.badges = None;

        Ok(())
    }

    /// Gets the user's badges along with calculating any dynamic badges
    pub async fn get_badges(&self) -> u32 {
        let config = config().await;
        let mut badges = self.badges.unwrap_or_default() as u32 & !(UserBadges::Plus as u32);

        if self.has_plus().await && !self.plus.as_ref().is_some_and(|plus| plus.hide_badge) {
            badges |= UserBadges::Plus as u32;
        }

        if let Some(cutoff) = config.api.users.early_adopter_cutoff {
            if Ulid::from_string(&self.id).unwrap().timestamp_ms() < cutoff {
                return badges | UserBadges::EarlyAdopter as u32;
            };
        };

        badges
    }

    /// Removes all relationships which include the user
    pub async fn clear_relationships(&self, db: &Database) -> Result<()> {
        let user_ids = self
            .relations
            .iter()
            .flatten()
            .map(|relation| relation.id.clone())
            .collect();

        db.clear_user_relationships(&self.id, user_ids).await
    }

    /// Removes user from all joined groups
    pub async fn remove_from_all_groups(&self, db: &Database) -> Result<()> {
        let mut generator = db.find_group_message_channels(&self.id).await?;

        while let Some(groups) = generator.next_n(100).await? {
            let ids = groups
                .into_iter()
                .map(|channel| channel.id().to_string())
                .collect();

            db.remove_user_from_groups(ids, &self.id).await?;
        }

        Ok(())
    }

    /// Deletes the user along with:
    /// - deletes owned bots, servers and messages
    /// - removes user from all groups
    /// - clears relationships
    pub async fn delete(&mut self, db: &Database) -> Result<()> {
        for bot in db.fetch_bots_by_user(&self.id).await? {
            bot.delete(db).await?;
        }

        for server in db.fetch_owned_servers(&self.id).await? {
            server.delete(db).await?;
        }

        self.remove_from_all_groups(db).await?;
        // Plus ends and supercharges come off their servers
        crate::end_billing_for_user(db, &self.id).await?;
        db.clear_memberships(&self.id).await?;
        self.clear_relationships(db).await?;
        // their files first, as each message would on its own (forwarded
        // copies of them included), then the messages
        crate::Message::remove_files_of_author(db, &self.id, None, None, 500).await?;
        db.delete_messages_by_user(&self.id).await?;
        self.mark_deleted(db).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{User, UserHint};
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use revolt_result::{create_database_error, create_error, ErrorType};

    #[test]
    fn only_a_missing_token_is_an_invalid_session() {
        let missing = User::token_lookup_error(create_error!(NotFound));
        assert!(matches!(missing.error_type, ErrorType::InvalidSession));

        // a database failure is not the client's fault: it must not read as
        // a refused token (the gateway would then drop a good session)
        let broken = User::token_lookup_error(create_database_error!("find_one", "bots"));
        assert!(matches!(broken.error_type, ErrorType::DatabaseError { .. }));
    }

    #[test]
    fn a_failing_database_does_not_fall_through_to_bot_tokens() {
        assert!(User::may_be_bot_token(&create_error!(InvalidSession)));
        assert!(User::may_be_bot_token(&create_error!(NotFound)));
        assert!(!User::may_be_bot_token(&create_database_error!("find_one", "sessions")));
        assert!(!User::may_be_bot_token(&create_error!(InternalError)));
    }

    #[tokio::test]
    async fn unknown_tokens_are_invalid_sessions() {
        database_test!(|db| async move {
            for hint in [UserHint::Any, UserHint::Bot, UserHint::User] {
                let error = User::from_token(&db, "no-such-token", hint)
                    .await
                    .unwrap_err();
                assert!(matches!(error.error_type, ErrorType::InvalidSession));
            }
        });
    }

    #[tokio::test]
    async fn the_gateway_refuses_an_expired_session() {
        database_test!(|db| async move {
            let account = crate::Account::new(
                &db,
                format!("{}@zrzur.cc", ulid::Ulid::new()),
                "password_insecure".to_owned(),
                false,
            )
            .await
            .unwrap();
            let mut session = account.create_session(&db, String::new()).await.unwrap();
            session.last_seen =
                iso8601_timestamp::Timestamp::now_utc() + iso8601_timestamp::Duration::days(-31);
            db.save_session(&session).await.unwrap();

            for hint in [UserHint::Any, UserHint::User] {
                let error = User::from_token(&db, &session.token, hint).await.unwrap_err();
                assert!(matches!(error.error_type, ErrorType::InvalidSession));
            }
        });
    }

    #[test]
    fn username_validation_blocked_names() {
        let username_admin = "Admin";
        let username_revolt = "Revolt";
        let username_stoat = "Stoat";
        let username_allowed = "Allowed";

        assert!(User::validate_username(username_admin).is_err());
        assert!(User::validate_username(username_revolt).is_err());
        assert!(User::validate_username(username_stoat).is_err());
        assert!(User::validate_username(username_allowed).is_ok());
    }

    #[test]
    fn username_validation_blocked_patterns() {
        let username_grave = "```_test";
        let username_discord = "discord.gg_test";
        let username_rvlt = "rvlt.gg_test";
        let username_guilded = "guilded.gg_test";
        let username_stt = "stt.gg_test";
        let username_revolt = "revolt.chat_test";
        let username_stoat = "stoat.chat_test";
        let username_http = "http://_test";
        let username_https = "https://_test";

        assert!(User::validate_username(username_grave).is_err());
        assert!(User::validate_username(username_discord).is_err());
        assert!(User::validate_username(username_rvlt).is_err());
        assert!(User::validate_username(username_guilded).is_err());
        assert!(User::validate_username(username_stt).is_err());
        assert!(User::validate_username(username_revolt).is_err());
        assert!(User::validate_username(username_stoat).is_err());
        assert!(User::validate_username(username_http).is_err());
        assert!(User::validate_username(username_https).is_err());
    }

    #[tokio::test]
    async fn username_sanitisation_clean() {
        let username_clean = "Test";

        let username_clean_sanitised = User::sanitise_username(username_clean).await;

        assert!(username_clean_sanitised.is_ok());
        assert_eq!(username_clean, username_clean_sanitised.unwrap());
    }

    #[tokio::test]
    async fn username_sanitisation_homoglyphs() {
        let username_homoglyphs = "𝔽𝕌Ňℕｙ";

        let username_homoglyphs_sanitised =
            User::sanitise_username(username_homoglyphs).await.unwrap();

        assert_ne!(username_homoglyphs, username_homoglyphs_sanitised);
        assert_eq!("funny", username_homoglyphs_sanitised);
    }

    #[tokio::test]
    async fn username_sanitisation_padding() {
        let username_padding = "a";

        let username = User::sanitise_username(username_padding).await.unwrap();

        assert_eq!("a_", username);
    }

    #[tokio::test]
    async fn create_user() {
        use revolt_result::Result;
        use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};

        database_test!(|db| async move {
            let mut created_clean = User::create(&db, "Test".to_string(), None, None)
                .await
                .unwrap();

            assert_eq!("Test", created_clean.username);

            created_clean
                .update_username(&db, "Test2".to_string())
                .await
                .unwrap();

            assert_eq!("Test2", created_clean.username);

            let created_invalid_result: Result<_> =
                User::create(&db, "stoat.chat".to_string(), None, None).await;

            assert!(created_invalid_result.is_err());

            let mut updated_invalid = User::create(&db, "Test".to_string(), None, None)
                .await
                .unwrap();

            let updated_invalid_update_result = updated_invalid
                .update_username(&db, "http://test".to_string())
                .await;

            assert!(updated_invalid_update_result.is_err());
        });
    }

    #[tokio::test]
    async fn remove_profile_background() {
        use crate::{FieldsUser, File, Metadata, PartialUser, UserProfile};
        use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};

        database_test!(|db| async move {
            let mut user = User::create(&db, "Test".to_string(), None, None)
                .await
                .unwrap();

            let background = File {
                id: "banner_id".to_string(),
                tag: "banners".to_string(),
                filename: "banner.png".to_string(),
                hash: None,
                uploaded_at: None,
                uploader_id: Some(user.id.clone()),
                used_for: None,
                deleted: None,
                reported: None,
                purged: None,
                metadata: Metadata::Image {
                    width: 100,
                    height: 100,
                    thumbhash: None,
                    animated: None,
                },
                content_type: "image/png".to_string(),
                size: 1,
                message_id: None,
                user_id: None,
                server_id: None,
                object_id: None,
                forwarded_from: None,
            };

            user.update(
                &db,
                PartialUser {
                    profile: Some(UserProfile {
                        background: Some(background),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();

            assert!(user.profile.as_ref().unwrap().background.is_some());

            // Removing the banner without any other field changes used to
            // send MongoDB an empty $set, which it rejects outright.
            user.update(&db, PartialUser::default(), vec![FieldsUser::ProfileBackground])
                .await
                .unwrap();

            assert!(user.profile.as_ref().unwrap().background.is_none());
        });
    }
}
