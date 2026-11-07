//! The body of `POST /webhooks/{id}/{token}`. It takes Zrzur's own fields and
//! the field names common webhook tools send, so a tool works by changing
//! only the host: `username`, `avatar_url`, `allowed_mentions`, `color`,
//! `image.url`, `thumbnail.url`, `attachments`, `thread_name`, `applied_tags`.

use iso8601_timestamp::Timestamp;

use super::{
    ComponentBlock, EmbedAuthor, EmbedField, EmbedFooter, Interactions, Masquerade, ReplyIntent,
    SendableEmbed,
};

auto_derived!(
    /// Kinds of mention a request lets through
    #[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
    pub enum AllowedMentionType {
        Everyone,
        Roles,
        Users,
    }

    /// Which mentions in this message may notify anyone. Left out, every
    /// mention counts; sent, only what it lists. Either way `@everyone`,
    /// `@online` and role mentions also need the webhook's own permissions.
    #[derive(Default)]
    pub struct AllowedMentions {
        /// Kinds of mention to take from the text
        #[cfg_attr(feature = "serde", serde(default))]
        pub parse: Vec<AllowedMentionType>,
        /// These roles only (with `roles` not in `parse`)
        #[cfg_attr(feature = "serde", serde(default))]
        pub roles: Vec<String>,
        /// These users only (with `users` not in `parse`)
        #[cfg_attr(feature = "serde", serde(default))]
        pub users: Vec<String>,
        /// Notify the author of a replied-to message
        #[cfg_attr(feature = "serde", serde(default))]
        pub replied_user: bool,
    }

    /// An embed image: the id of an uploaded file, or `{ "url": ... }`, which
    /// is a web address or `attachment://name` for a file of this request
    #[cfg_attr(feature = "serde", serde(untagged))]
    pub enum WebhookEmbedMedia {
        Id(String),
        Url { url: String },
    }

    /// Embed as a webhook sends it
    #[derive(Default)]
    pub struct WebhookEmbed {
        pub title: Option<String>,
        pub description: Option<String>,
        pub url: Option<String>,
        /// Time shown next to the footer
        pub timestamp: Option<Timestamp>,
        /// Colour as a number (`0xE0195F`)
        pub color: Option<u32>,
        /// Colour as CSS text
        pub colour: Option<String>,
        pub icon_url: Option<String>,
        pub author: Option<EmbedAuthor>,
        pub fields: Option<Vec<EmbedField>>,
        pub footer: Option<EmbedFooter>,
        /// Large image
        pub image: Option<WebhookEmbedMedia>,
        /// Id of an uploaded file for the large image
        pub media: Option<String>,
        /// Small image
        pub thumbnail: Option<WebhookEmbedMedia>,
    }

    /// A file named in the body: the id of an uploaded file, or an entry
    /// like `{ "id": 0, "filename": "a.png" }`, which refers to a file of the
    /// multipart request and adds nothing here
    #[cfg_attr(feature = "serde", serde(untagged))]
    pub enum WebhookAttachment {
        Id(String),
        Described {
            #[cfg_attr(feature = "serde", serde(default))]
            filename: Option<String>,
        },
    }

    /// Message to send through a webhook
    #[derive(Default)]
    pub struct DataWebhookExecute {
        /// Message text, up to 2000 characters
        pub content: Option<String>,
        /// Name shown on this message instead of the webhook's (1 to 80
        /// characters). The webhook mark stays.
        pub username: Option<String>,
        /// Avatar shown on this message, a web address. Clients load it
        /// through the media proxy.
        pub avatar_url: Option<String>,
        /// Accepted and ignored
        pub tts: Option<bool>,
        /// Up to 10
        pub embeds: Option<Vec<WebhookEmbed>>,
        /// Which mentions may notify anyone
        pub allowed_mentions: Option<AllowedMentions>,
        /// Components; interactive ones only on webhooks a bot created
        pub components: Option<Vec<ComponentBlock>>,
        /// Files: ids from an upload, or descriptions of multipart files
        pub attachments: Option<Vec<WebhookAttachment>>,
        /// `4` hides link previews, `4096` sends without a notification
        pub flags: Option<u32>,
        /// Forum: start a thread with this name
        pub thread_name: Option<String>,
        /// Forum: tags of the new thread
        pub applied_tags: Option<Vec<String>>,

        /// Name and avatar override in Zrzur's own form
        pub masquerade: Option<Masquerade>,
        /// Messages to reply to
        pub replies: Option<Vec<ReplyIntent>>,
        /// How this message may be interacted with
        pub interactions: Option<Interactions>,
    }

    /// A test message from a manager
    pub struct DataWebhookTest {
        /// Text to post, up to 200 characters
        pub content: String,
    }

    /// Changes to a message a webhook sent
    #[derive(Default)]
    pub struct DataWebhookEditMessage {
        /// New text. Mentions in it notify nobody: an edit never does.
        pub content: Option<String>,
        /// New embeds, replacing the old ones
        pub embeds: Option<Vec<WebhookEmbed>>,
        /// New components; an empty list removes them
        pub components: Option<Vec<ComponentBlock>>,
        /// Accepted and ignored: an edit notifies nobody anyway
        pub allowed_mentions: Option<AllowedMentions>,
    }
);

/// Discord-style `flags` bit: no link previews
pub const WEBHOOK_FLAG_SUPPRESS_EMBEDS: u32 = 1 << 2;
/// Discord-style `flags` bit: no notifications
pub const WEBHOOK_FLAG_SILENT: u32 = 1 << 12;

impl WebhookEmbed {
    /// The embed in Zrzur's form. `file` finds the id of a file of this
    /// request by its name.
    pub fn into_sendable(
        self,
        file: impl Fn(&str) -> Option<String>,
    ) -> Result<SendableEmbed, String> {
        let colour = match (self.color, self.colour) {
            (Some(color), _) if color > 0xFF_FF_FF => {
                return Err("color must be at most 0xFFFFFF".to_string())
            }
            (Some(color), _) => Some(format!("#{color:06X}")),
            (None, colour) => colour,
        };

        // (file id, web address)
        let resolve = |media: Option<WebhookEmbedMedia>| -> Result<(Option<String>, Option<String>), String> {
            match media {
                None => Ok((None, None)),
                Some(WebhookEmbedMedia::Id(id)) => Ok((Some(id), None)),
                Some(WebhookEmbedMedia::Url { url }) => match url.strip_prefix("attachment://") {
                    Some(name) => file(name)
                        .map(|id| (Some(id), None))
                        .ok_or_else(|| format!("no file named {name} in this request")),
                    None => Ok((None, Some(url))),
                },
            }
        };
        let (image_file, image_url) = resolve(self.image)?;
        let (thumbnail, thumbnail_url) = resolve(self.thumbnail)?;

        Ok(SendableEmbed {
            icon_url: self.icon_url,
            url: self.url,
            title: self.title,
            description: self.description,
            media: self.media.or(image_file),
            colour,
            author: self.author,
            fields: self.fields,
            thumbnail,
            footer: self.footer,
            timestamp: self.timestamp,
            image_url,
            thumbnail_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_tool_written_for_another_platform_is_understood() {
        let embed: WebhookEmbed = serde_json::from_value(serde_json::json!({
            "type": "rich",
            "title": "Build",
            "color": 14686559,
            "timestamp": "2026-11-04T10:00:00+00:00",
            "image": { "url": "https://example.com/i.png" },
            "thumbnail": { "url": "https://example.com/t.png" },
            "footer": { "text": "CI", "icon_url": "https://example.com/f.png" },
            "author": { "name": "bot" },
            "provider": { "name": "ignored" },
            "fields": [{ "name": "a", "value": "b", "inline": true }],
        }))
        .unwrap();
        let sendable = embed.into_sendable(none).unwrap();
        assert_eq!(sendable.colour.as_deref(), Some("#E0195F"));
        assert_eq!(sendable.image_url.as_deref(), Some("https://example.com/i.png"));
        assert_eq!(sendable.thumbnail_url.as_deref(), Some("https://example.com/t.png"));
        assert_eq!(sendable.media, None);
    }

    #[test]
    fn zrzur_embeds_keep_working() {
        let embed: WebhookEmbed = serde_json::from_value(serde_json::json!({
            "colour": "#E0195F", "media": "FILE", "thumbnail": "THUMB", "icon_url": "https://e.com/i"
        }))
        .unwrap();
        let sendable = embed.into_sendable(none).unwrap();
        assert_eq!(sendable.colour.as_deref(), Some("#E0195F"));
        assert_eq!(sendable.media.as_deref(), Some("FILE"));
        assert_eq!(sendable.thumbnail.as_deref(), Some("THUMB"));
    }

    #[test]
    fn colours_stay_in_range() {
        let embed = WebhookEmbed { color: Some(0xFF_FF_FF), ..Default::default() };
        assert_eq!(embed.into_sendable(none).unwrap().colour.as_deref(), Some("#FFFFFF"));
        let embed = WebhookEmbed { color: Some(0x1_00_00_00), ..Default::default() };
        assert!(embed.into_sendable(none).is_err());
        let embed = WebhookEmbed { color: Some(0), ..Default::default() };
        assert_eq!(embed.into_sendable(none).unwrap().colour.as_deref(), Some("#000000"));
    }

    #[test]
    fn attachment_addresses_point_at_files_of_the_request() {
        let find = |name: &str| (name == "a.png").then(|| "FILE".to_string());
        let embed = WebhookEmbed {
            image: Some(WebhookEmbedMedia::Url { url: "attachment://a.png".to_string() }),
            ..Default::default()
        };
        assert_eq!(embed.into_sendable(find).unwrap().media.as_deref(), Some("FILE"));
        let embed = WebhookEmbed {
            image: Some(WebhookEmbedMedia::Url { url: "attachment://b.png".to_string() }),
            ..Default::default()
        };
        assert!(embed.into_sendable(find).is_err());
    }

    #[test]
    fn attachments_are_ids_or_descriptions() {
        let list: Vec<WebhookAttachment> = serde_json::from_value(serde_json::json!([
            "FILE", { "id": 0, "filename": "a.png", "description": "x" }
        ]))
        .unwrap();
        assert_eq!(list[0], WebhookAttachment::Id("FILE".to_string()));
        assert_eq!(list[1], WebhookAttachment::Described { filename: Some("a.png".to_string()) });
    }

    #[test]
    fn allowed_mentions_parse() {
        let allowed: AllowedMentions = serde_json::from_value(serde_json::json!({
            "parse": ["everyone", "roles", "users"], "replied_user": true
        }))
        .unwrap();
        assert_eq!(allowed.parse.len(), 3);
        assert!(allowed.replied_user);
        let empty: AllowedMentions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(empty.parse.is_empty());
    }
}
