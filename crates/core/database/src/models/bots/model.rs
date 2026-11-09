use revolt_result::Result;
use ulid::Ulid;

use crate::{
    events::client::EventV1, BotInformation, Database, PartialUser, RatelimitEvent,
    RatelimitEventType, RemovalIntention, User,
};

auto_derived_partial!(
    /// Bot
    pub struct Bot {
        /// Bot Id
        ///
        /// This equals the associated bot user's id.
        #[serde(rename = "_id")]
        pub id: String,
        /// User Id of the bot owner
        pub owner: String,
        /// The token, only right after it was made (creation or reset) so it
        /// can be shown once; never stored
        #[serde(skip)]
        pub token: String,
        /// SHA-256 of the token (hex); the only form that is stored
        #[serde(default)]
        pub token_hash: String,
        /// Whether the bot is public
        /// (may be invited by anyone)
        pub public: bool,
        /// The permissions the bot will ask to have upon being invited to a server,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_permissions: Option<i64>,

        /// Whether to enable analytics
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub analytics: bool,
        /// Whether this bot should be publicly discoverable
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub discoverable: bool,
        /// Reserved; URL for handling interactions
        #[serde(skip_serializing_if = "String::is_empty", default)]
        pub interactions_url: String,
        /// URL for terms of service
        #[serde(skip_serializing_if = "String::is_empty", default)]
        pub terms_of_service_url: String,
        /// URL for privacy policy
        #[serde(skip_serializing_if = "String::is_empty", default)]
        pub privacy_policy_url: String,

        /// Enum of bot flags
        #[serde(skip_serializing_if = "Option::is_none")]
        pub flags: Option<i32>,

        /// Privileged gateway intents the owner has enabled
        /// (`v0::GatewayIntents`); bots from before intents get all three
        /// in migration 66
        #[serde(default)]
        pub privileged_intents: i64,
        /// Privileged intents Trust & Safety approved; past
        /// [UNVERIFIED_BOT_SERVER_LIMIT] servers only these stay on
        #[serde(default)]
        pub approved_intents: i64,

        /// Checked HTTPS address that receives this bot's interactions
        /// instead of the gateway
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub interactions_endpoint: Option<String>,
        /// Generation of the signing secret, 0 for none yet. The secret
        /// itself is derived (`interactions_secret`), never stored.
        #[serde(default)]
        pub interactions_secret_version: i32,

        /// The owner chose to show who made the bot on its public page
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub show_creator: bool,
    },
    "PartialBot"
);

/// Servers an unverified bot may be in
pub const UNVERIFIED_BOT_SERVER_LIMIT: usize = 100;

auto_derived!(
    /// Optional fields on bot object
    pub enum FieldsBot {
        Token,
        InteractionsURL,
        InteractionsEndpoint,
    }
);

#[allow(clippy::derivable_impls)]
impl Default for Bot {
    fn default() -> Self {
        Self {
            id: Default::default(),
            owner: Default::default(),
            token: Default::default(),
            token_hash: Default::default(),
            public: Default::default(),
            default_permissions: Default::default(),
            analytics: Default::default(),
            discoverable: Default::default(),
            interactions_url: Default::default(),
            terms_of_service_url: Default::default(),
            privacy_policy_url: Default::default(),
            flags: Default::default(),
            privileged_intents: Default::default(),
            approved_intents: Default::default(),
            interactions_endpoint: Default::default(),
            interactions_secret_version: Default::default(),
            show_creator: Default::default(),
        }
    }
}

#[allow(clippy::disallowed_methods)]
impl Bot {
    /// Create a new bot
    pub async fn create<D>(
        db: &Database,
        username: String,
        owner: &User,
        data: D,
    ) -> Result<(Bot, User)>
    where
        D: Into<Option<PartialBot>>,
    {
        if owner.bot.is_some() {
            return Err(create_error!(IsBot));
        }

        if db.get_number_of_bots_by_user(&owner.id).await? >= owner.limits().await.bots {
            return Err(create_error!(ReachedMaximumBots));
        }

        // Everyone can have as many bots as they like, but only make a few
        // a week (deleting one doesn't give a creation back)
        let per_week = revolt_config::config()
            .await
            .features
            .limits
            .global
            .bot_creations_per_week;
        const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
        if per_week > 0
            && db
                .has_ratelimited(&owner.id, RatelimitEventType::BotCreation, WEEK, per_week)
                .await?
        {
            // a creation counts for a week, so the oldest one frees a place first
            let resets_at = db
                .oldest_ratelimit_event(&owner.id, RatelimitEventType::BotCreation, WEEK)
                .await?
                .and_then(|id| Ulid::from_string(&id).ok())
                .map(|id| id.timestamp_ms() as i64 + WEEK.as_millis() as i64);
            return Err(create_error!(TooManyBotCreations {
                max: per_week,
                resets_at
            }));
        }
        RatelimitEvent::create(db, owner.id.clone(), RatelimitEventType::BotCreation).await?;

        let id = Ulid::new().to_string();

        let user = User::create(
            db,
            username,
            Some(id.to_string()),
            Some(PartialUser {
                bot: Some(BotInformation {
                    owner: owner.id.to_string(),
                    verified: false,
                }),
                ..Default::default()
            }),
        )
        .await?;

        let (token, token_hash) = Bot::new_token(&id);
        let mut bot = Bot {
            id,
            owner: owner.id.to_string(),
            token,
            token_hash,
            ..Default::default()
        };

        if let Some(data) = data.into() {
            bot.apply_options(data);
        }

        db.insert_bot(&bot).await?;
        Ok((bot, user))
    }

    /// A new token and its hash. The token starts with `zrzur_bot_` and the
    /// bot's id, so a leaked one is easy to recognise (and scanners can
    /// match it); the secret part is 48 random characters.
    pub fn new_token(id: &str) -> (String, String) {
        let token = format!("zrzur_bot_{id}.{}", nanoid::nanoid!(48));
        let hash = hash_token(&token);
        (token, hash)
    }

    /// Remove a field from this object
    pub fn remove_field(&mut self, field: &FieldsBot) {
        match field {
            // a new token is made in `update`
            FieldsBot::Token => {}
            FieldsBot::InteractionsURL => {
                self.interactions_url = String::new();
            }
            FieldsBot::InteractionsEndpoint => {
                self.interactions_endpoint = None;
            }
        }
    }

    /// Update this bot
    pub async fn update(
        &mut self,
        db: &Database,
        mut partial: PartialBot,
        remove: Vec<FieldsBot>,
    ) -> Result<()> {
        let reset = remove.contains(&FieldsBot::Token);
        if reset {
            let (token, hash) = Bot::new_token(&self.id);
            self.token = token;
            partial.token_hash = Some(hash);
        }

        for field in &remove {
            self.remove_field(field);
        }
        let remove: Vec<FieldsBot> = remove.into_iter().filter(|f| f != &FieldsBot::Token).collect();

        db.update_bot(&self.id, &partial, remove).await?;

        // the old token stops working everywhere at once
        if reset {
            EventV1::Logout.p(self.id.clone()).await;
        }

        self.apply_options(partial);
        Ok(())
    }

    /// Delete this bot
    pub async fn delete(&self, db: &Database) -> Result<()> {
        for member in db.fetch_all_memberships(&self.id).await? {
            let server = db.fetch_server(&member.id.server).await?;

            member
                .remove(db, &server, RemovalIntention::Leave, true)
                .await?;

            server.cleanup_managed_bot_role(db, &self.id).await?;
        }
        
        db.fetch_user(&self.id).await?.mark_deleted(db).await?;
        // its paid Discover places go to whoever waits next
        db.delete_promotions_for_target(&self.id).await?;
        // its slash commands go with it, in every server
        db.delete_all_application_commands(&self.id).await?;
        db.delete_oauth_application(&self.id).await?;
        db.delete_bot(&self.id).await
    }
}

#[cfg(test)]
mod tests {

use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};
use crate::{Bot, FieldsBot, PartialBot, User};

    #[tokio::test]
    async fn crud() {
        database_test!(|db| async move {
            let owner = User::create(&db, "Owner".to_string(), None, None)
                .await
                .unwrap();

            let (bot, _) = Bot::create(
                &db,
                "Bot-Name".to_string(),
                &owner,
                PartialBot {
                    token: Some("my token".to_string()),
                    interactions_url: Some("some url".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            assert!(!bot.interactions_url.is_empty());

            let mut updated_bot = bot.clone();
            updated_bot
                .update(
                    &db,
                    PartialBot {
                        public: Some(true),
                        ..Default::default()
                    },
                    vec![FieldsBot::Token, FieldsBot::InteractionsURL],
                )
                .await
                .unwrap();

            let fetched_bot1 = db.fetch_bot(&bot.id).await.unwrap();
            // only the hash is stored: the new token is known right after the reset
            let fetched_bot2 = db.fetch_bot_by_token(&updated_bot.token).await.unwrap();
            assert!(fetched_bot1.token.is_empty());
            let fetched_bots = db.fetch_bots_by_user(&owner.id).await.unwrap();

            assert!(!bot.public);
            assert!(fetched_bot1.public);
            assert!(!bot.interactions_url.is_empty());
            assert!(fetched_bot1.interactions_url.is_empty());
            assert_ne!(bot.token, updated_bot.token);
            assert!(db.fetch_bot_by_token(&bot.token).await.is_err());
            let mut stored = updated_bot.clone();
            stored.token = String::new();
            assert_eq!(stored, fetched_bot1);
            assert_eq!(fetched_bot1, fetched_bot2);
            assert_eq!(fetched_bot1, fetched_bots[0]);
            assert_eq!(1, db.get_number_of_bots_by_user(&owner.id).await.unwrap());

            bot.delete(&db).await.unwrap();
            assert!(db.fetch_bot(&bot.id).await.is_err());
            assert_eq!(0, db.get_number_of_bots_by_user(&owner.id).await.unwrap());
            assert_eq!(db.fetch_user(&bot.id).await.unwrap().flags, Some(2))
        });
    }

    #[tokio::test]
    async fn past_the_server_limit_a_connection_gets_approved_intents_only() {
        use crate::{Member, MemberCompositeKey};
        use revolt_models::v0::GatewayIntents as I;

        database_test!(|db| async move {
            let owner = User::create(&db, "Owner".to_string(), None, None)
                .await
                .unwrap();
            let (mut bot, user) = Bot::create(&db, "Bot-Name".to_string(), &owner, None)
                .await
                .unwrap();
            let wanted = I::DEFAULT | I::MESSAGE_CONTENT;
            bot.update(
                &db,
                PartialBot {
                    privileged_intents: Some(I::MESSAGE_CONTENT as i64),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
            assert_eq!(
                crate::connection_intents(&db, &user, Some(wanted)).await,
                Ok(wanted)
            );

            for n in 0..=crate::UNVERIFIED_BOT_SERVER_LIMIT {
                db.insert_or_merge_member(&Member {
                    id: MemberCompositeKey {
                        server: format!("server{n}"),
                        user: user.id.clone(),
                    },
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            // left out, not refused: a running bot keeps its connection
            assert_eq!(
                crate::connection_intents(&db, &user, Some(wanted)).await,
                Ok(I::DEFAULT)
            );
            // an intent the owner never switched on still closes it
            assert_eq!(
                crate::connection_intents(&db, &user, Some(I::PRESENCE)).await,
                Err(vec!["PRESENCE"])
            );

            bot.update(
                &db,
                PartialBot {
                    approved_intents: Some(I::MESSAGE_CONTENT as i64),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
            assert_eq!(
                crate::connection_intents(&db, &user, Some(wanted)).await,
                Ok(wanted)
            );
        });
    }

    #[tokio::test]
    async fn presence_is_hidden_from_bots_without_the_intent() {
        use revolt_models::v0::GatewayIntents as I;

        database_test!(|db| async move {
            let owner = User::create(&db, "Owner".to_string(), None, None)
                .await
                .unwrap();
            let (mut bot, reader) = Bot::create(&db, "Bot-Name".to_string(), &owner, None)
                .await
                .unwrap();

            let seen = |id: &str| -> revolt_models::v0::User {
                serde_json::from_value(serde_json::json!({
                    "_id": id,
                    "username": "someone",
                    "discriminator": "0001",
                    "relationship": "None",
                    "online": true,
                    "status": { "text": "away" },
                }))
                .unwrap()
            };

            let mut users = [seen("someone"), seen(&reader.id)];
            crate::hide_presence(&db, &reader, &mut users).await;
            assert!(!users[0].online && users[0].status.is_none());
            assert!(users[1].online, "a bot always sees itself");

            // people always see it
            let mut users = [seen("someone")];
            crate::hide_presence(&db, &owner, &mut users).await;
            assert!(users[0].online);

            bot.update(
                &db,
                PartialBot {
                    privileged_intents: Some(I::PRESENCE as i64),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .unwrap();
            let mut users = [seen("someone")];
            crate::hide_presence(&db, &reader, &mut users).await;
            assert!(users[0].online && users[0].status.is_some());
        });
    }
}

/// Privileged gateway intents a user's requests may use: every one for
/// people, the ones the owner enabled for bots
pub async fn enabled_intents(db: &Database, user: &User) -> u64 {
    if user.bot.is_none() {
        return revolt_models::v0::GatewayIntents::ALL;
    }
    let Ok(bot) = db.fetch_bot(&user.id).await else {
        return 0;
    };
    let mut intents = bot.privileged_intents as u64;
    // past the limit only what Trust & Safety approved stays on
    let unapproved = intents
        & !(bot.approved_intents as u64)
        & revolt_models::v0::GatewayIntents::PRIVILEGED;
    if unapproved != 0
        && db.fetch_server_count(&bot.id).await.unwrap_or(usize::MAX) > UNVERIFIED_BOT_SERVER_LIMIT
    {
        intents &= !unapproved;
    }
    intents
}

/// The intents a bot's gateway connection gets: what it asked for (the free
/// ones if nothing), or the names of the privileged ones its owner has not
/// switched on. Past the server limit an unapproved intent is left out, not
/// refused: a running bot gets less, it is not cut off.
pub async fn connection_intents(
    db: &Database,
    user: &User,
    requested: Option<u64>,
) -> std::result::Result<u64, Vec<&'static str>> {
    use revolt_models::v0::GatewayIntents;
    let switched_on = db
        .fetch_bot(&user.id)
        .await
        .map(|bot| bot.privileged_intents)
        .unwrap_or(0);
    let intents = GatewayIntents::resolve(requested, switched_on)?;
    Ok(intents & (!GatewayIntents::PRIVILEGED | enabled_intents(db, user).await))
}

/// Hide who is online, and their status, from a bot without the Presence
/// intent; a bot always sees itself. The gateway does the same to events.
pub async fn hide_presence(db: &Database, reader: &User, users: &mut [revolt_models::v0::User]) {
    if reader.bot.is_none()
        || enabled_intents(db, reader).await & revolt_models::v0::GatewayIntents::PRESENCE != 0
    {
        return;
    }
    for user in users.iter_mut().filter(|user| user.id != reader.id) {
        user.online = false;
        user.status = None;
    }
}

impl Bot {
    /// Whether Trust & Safety verified the bot
    pub fn is_verified(&self) -> bool {
        self.flags.unwrap_or_default() & revolt_models::v0::BotFlags::Verified as i32 != 0
    }
}

/// Hide what a bot without the Message Content intent may not read: every
/// message except its own, the ones in its DMs and the ones that mention it
pub fn hide_message_content(
    messages: &mut [revolt_models::v0::Message],
    reader: &str,
    direct_message: bool,
    intents: u64,
) {
    if direct_message || intents & revolt_models::v0::GatewayIntents::MESSAGE_CONTENT != 0 {
        return;
    }
    for message in messages {
        let mentioned = message
            .mentions
            .as_ref()
            .is_some_and(|m| m.iter().any(|id| id == reader));
        if message.author != reader && !mentioned {
            message.hide_content();
        }
    }
}

/// SHA-256 of a bot token (hex), as stored
pub fn hash_token(token: &str) -> String {
    crate::sha256_hex(token.as_bytes())
}

/// Compare two token hashes without leaking where they differ
pub fn same_token_hash(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
