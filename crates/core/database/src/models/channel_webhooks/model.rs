use iso8601_timestamp::Timestamp;
use revolt_result::Result;

use crate::events::client::EventV1;
use crate::{Database, File};

auto_derived_partial!(
    /// Webhook
    pub struct Webhook {
        /// Webhook Id
        #[serde(rename = "_id")]
        pub id: String,

        /// The name of the webhook
        pub name: String,

        /// The avatar of the webhook
        #[serde(skip_serializing_if = "Option::is_none")]
        pub avatar: Option<File>,

        /// User that created this webhook
        pub creator_id: String,

        /// The channel this webhook belongs to
        pub channel_id: String,

        /// The permissions of the webhook
        pub permissions: u64,

        /// The token as stored before hashing. New webhooks never have it,
        /// a migration removes it from old ones
        #[serde(skip_serializing_if = "Option::is_none")]
        pub token: Option<String>,

        /// SHA-256 of the token (hex)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub token_hash: Option<String>,

        /// When the webhook last sent something, to the minute
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub last_used: Option<Timestamp>,
    },
    "PartialWebhook"
);

auto_derived!(
    /// Optional fields on webhook object
    pub enum FieldsWebhook {
        Avatar,
        /// The plain token of a webhook from before hashing
        Token,
    }
);

#[allow(clippy::derivable_impls)]
impl Default for Webhook {
    fn default() -> Self {
        Self {
            id: Default::default(),
            name: Default::default(),
            avatar: None,
            creator_id: Default::default(),
            channel_id: Default::default(),
            permissions: Default::default(),
            token: Default::default(),
            token_hash: Default::default(),
            last_used: Default::default(),
        }
    }
}

/// Webhooks one channel may have
pub const MAX_CHANNEL_WEBHOOKS: usize = 15;

/// Webhooks one server may have
pub const MAX_SERVER_WEBHOOKS: usize = 1000;

/// How often `last_used` is written
const LAST_USED_EVERY_SECONDS: i64 = 60;

#[allow(clippy::disallowed_methods)]
impl Webhook {
    /// A new token and its hash. The prefix lets secret scanners find leaked
    /// tokens; after it, 43 characters of 6 bits, so 258 random bits.
    pub fn new_token() -> (String, String) {
        let token = format!("zrzur_wh_{}", nanoid::nanoid!(43));
        let hash = crate::hash_token(&token);
        (token, hash)
    }

    /// Save a new webhook. The token is never stored or sent in the event.
    pub async fn create(&self, db: &Database) -> Result<()> {
        let mut webhook = self.clone();
        webhook.token = None;

        db.insert_webhook(&webhook).await?;

        EventV1::WebhookCreate(webhook.into())
            .p(self.channel_id.clone())
            .await;

        Ok(())
    }

    /// Check a token: against the hash, or, for a webhook the migration has
    /// not reached, against the hash of its stored token. Both sides are
    /// hashes of equal length, so timing tells nothing about the token.
    pub fn assert_token(&self, token: &str) -> Result<()> {
        let given = crate::hash_token(token);
        let matches = match (&self.token_hash, &self.token) {
            (Some(hash), _) => crate::same_token_hash(hash, &given),
            (None, Some(plain)) => crate::same_token_hash(&crate::hash_token(plain), &given),
            (None, None) => false,
        };
        if matches {
            Ok(())
        } else {
            Err(create_error!(NotAuthenticated))
        }
    }

    /// Replace the token; the old one stops working with this write.
    /// Returns the new token, which is not kept anywhere.
    pub async fn regenerate_token(&mut self, db: &Database) -> Result<String> {
        let (token, hash) = Webhook::new_token();
        db.update_webhook(
            &self.id,
            &PartialWebhook {
                token_hash: Some(hash.clone()),
                ..Default::default()
            },
            &[FieldsWebhook::Token],
        )
        .await?;
        self.token = None;
        self.token_hash = Some(hash);
        Ok(token)
    }

    /// Whether this webhook may send files
    pub fn can_upload_files(&self) -> bool {
        revolt_permissions::PermissionValue::from(self.permissions)
            .has_channel_permission(revolt_permissions::ChannelPermission::UploadFiles)
    }

    /// Note that the webhook was used, at most once a minute
    pub async fn touch(&self, db: &Database) {
        let now = Timestamp::now_utc();
        let fresh = self.last_used.is_some_and(|last| {
            now.duration_since(last).whole_seconds() < LAST_USED_EVERY_SECONDS
        });
        if !fresh {
            let partial = PartialWebhook {
                last_used: Some(now),
                ..Default::default()
            };
            if let Err(error) = db.update_webhook(&self.id, &partial, &[]).await {
                revolt_config::capture_error(&error);
            }
        }
    }

    pub async fn update(
        &mut self,
        db: &Database,
        mut partial: PartialWebhook,
        remove: Vec<FieldsWebhook>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field)
        }

        self.apply_options(partial.clone());

        db.update_webhook(&self.id, &partial, &remove).await?;

        // Secrets stay out of the event
        partial.token = None;
        partial.token_hash = None;

        EventV1::WebhookUpdate {
            id: self.id.clone(),
            channel_id: self.channel_id.clone(),
            data: partial.into(),
            remove: remove
                .into_iter()
                .filter(|field| !matches!(field, FieldsWebhook::Token))
                .map(|v| v.into())
                .collect(),
        }
        .p(self.channel_id.clone())
        .await;

        Ok(())
    }

    pub fn remove_field(&mut self, field: &FieldsWebhook) {
        match field {
            FieldsWebhook::Avatar => self.avatar = None,
            FieldsWebhook::Token => self.token = None,
        }
    }

    pub async fn delete(&self, db: &Database) -> Result<()> {
        db.delete_webhook(&self.id).await?;

        EventV1::WebhookDelete {
            id: self.id.clone(),
            channel_id: self.channel_id.clone(),
        }
        .p(self.channel_id.clone())
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{FieldsWebhook, PartialWebhook, Webhook};
    use lapin::{ExchangeKind, options::ExchangeDeclareOptions, types::FieldTable};

    #[test]
    fn tokens_have_the_prefix_and_enough_entropy() {
        let (token, hash) = Webhook::new_token();
        assert_eq!(token.len(), 9 + 43);
        assert!(token.starts_with("zrzur_wh_"));
        assert_eq!(hash, crate::hash_token(&token));
        assert_ne!(Webhook::new_token().0, token);
    }

    #[test]
    fn only_the_right_token_passes() {
        let (token, hash) = Webhook::new_token();
        let webhook = Webhook { token_hash: Some(hash), ..Default::default() };
        assert!(webhook.assert_token(&token).is_ok());
        assert!(webhook.assert_token("zrzur_wh_x").is_err());
        assert!(webhook.assert_token(&crate::hash_token(&token)).is_err());
        assert!(webhook.assert_token("").is_err());

        // from before hashing: the old token, in clear, until the migration
        let old = Webhook { token: Some("old".to_string()), ..Default::default() };
        assert!(old.assert_token("old").is_ok());
        assert!(old.assert_token("older").is_err());

        // a hash wins over a leftover plain token
        let both = Webhook {
            token: Some("old".to_string()),
            token_hash: Some(crate::hash_token("new")),
            ..Default::default()
        };
        assert!(both.assert_token("new").is_ok());
        assert!(both.assert_token("old").is_err());

        assert!(Webhook::default().assert_token("").is_err());
    }

    #[tokio::test]
    async fn crud() {
        database_test!(|db| async move {
            let webhook_id = "webhook";
            let channel_id = "channel";

            let webhook = Webhook {
                id: webhook_id.to_string(),
                name: "Webhook Name".to_string(),
                channel_id: channel_id.to_string(),
                avatar: None,
                ..Default::default()
            };

            webhook.create(&db).await.unwrap();

            let mut updated_webhook = webhook.clone();
            updated_webhook
                .update(
                    &db,
                    PartialWebhook {
                        name: Some("New Name".to_string()),
                        ..Default::default()
                    },
                    vec![FieldsWebhook::Avatar],
                )
                .await
                .unwrap();

            let fetched_webhook = db.fetch_webhook(webhook_id).await.unwrap();
            let fetched_webhooks = db.fetch_webhooks_for_channel(channel_id).await.unwrap();

            assert_eq!(updated_webhook, fetched_webhook);
            assert_ne!(webhook, fetched_webhook);
            assert_eq!(1, fetched_webhooks.len());
            assert_eq!(fetched_webhook, fetched_webhooks[0]);

            let (other_id, third_id) = ("webhook2".to_string(), "webhook3".to_string());
            for (id, channel) in [(&other_id, "channel2"), (&third_id, "channel3")] {
                db.insert_webhook(&Webhook {
                    id: id.clone(),
                    channel_id: channel.to_string(),
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            let several = db
                .fetch_webhooks_for_channels(&["channel".to_string(), "channel2".to_string()])
                .await
                .unwrap();
            assert_eq!(2, several.len());
            db.delete_webhooks_for_channel("channel3").await.unwrap();
            assert!(db.fetch_webhook(&third_id).await.is_err());
            assert!(db.fetch_webhook(&other_id).await.is_ok());

            webhook.delete(&db).await.unwrap();
            assert!(db.fetch_webhook(webhook_id).await.is_err());
            assert_eq!(
                0,
                db.fetch_webhooks_for_channel(channel_id)
                    .await
                    .unwrap()
                    .len()
            )
        });
    }
}
