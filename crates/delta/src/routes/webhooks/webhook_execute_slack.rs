//! Slack's incoming webhook format, so tools that post to Slack work by
//! changing the address.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_database::{
    util::{idempotency::IdempotencyKey, reference::Reference},
    Database, AMQP,
};
use revolt_models::v0::{self, EmbedAuthor, EmbedField, EmbedFooter, SendableEmbed};
use revolt_result::Result;
use rocket::State;
use serde_json::Value;

use super::delivery::{deliver, Delivery, MaybeMessage};
use super::webhook_execute_github::json_from_body;
use crate::util::webhooks::WebhookCaller;

fn shorten(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

fn text_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn owned(value: &Value, key: &str) -> Option<String> {
    text_of(value, key).map(str::to_string)
}

/// Slack's mrkdwn in Zrzur's markdown. `<url|label>` becomes a link, `*bold*`
/// bold, `~gone~` struck through. Slack's mass mentions become plain words:
/// nothing a Slack payload says may ping anyone here.
fn from_mrkdwn(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push('<');
            rest = after;
            continue;
        };
        let inner = &after[..end];
        rest = &after[end + 1..];
        match inner.split_once('|') {
            _ if inner.starts_with("!channel") => out.push_str("(channel)"),
            _ if inner.starts_with("!here") => out.push_str("(here)"),
            _ if inner.starts_with("!everyone") => out.push_str("(all)"),
            _ if inner.starts_with('!') => {}
            Some((target, label))
                if target.starts_with('@') || target.starts_with('#') =>
            {
                out.push_str(label);
            }
            _ if inner.starts_with('@') || inner.starts_with('#') => out.push_str(inner),
            Some((url, label)) => out.push_str(&format!("[{label}]({url})")),
            None => out.push_str(inner),
        }
    }
    out.push_str(rest);

    // inline markers: *bold* to **bold**, ~gone~ to ~~gone~~
    let mut styled = String::with_capacity(out.len());
    for (index, part) in out.split('`').enumerate() {
        if index > 0 {
            styled.push('`');
        }
        // code stays as it is
        if index % 2 == 1 {
            styled.push_str(part);
        } else {
            styled.push_str(&double_markers(&double_markers(part, '*'), '~'));
        }
    }

    styled.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Double a marker around a word group: `*a b*` to `**a b**`
fn double_markers(text: &str, marker: char) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == marker && chars.get(index + 1).is_some_and(|c| !c.is_whitespace()) {
            if let Some(length) = chars[index + 1..].iter().position(|c| *c == marker) {
                if length > 0 && !chars[index + length].is_whitespace() {
                    let inner: String = chars[index + 1..=index + length].iter().collect();
                    out.extend([marker, marker]);
                    out.push_str(&inner);
                    out.extend([marker, marker]);
                    index += length + 2;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// Slack's colour names, or a hex colour
fn colour(attachment: &Value) -> Option<String> {
    match text_of(attachment, "color")? {
        "good" => Some("#2EB67D".to_string()),
        "warning" => Some("#ECB22E".to_string()),
        "danger" => Some("#E01E5A".to_string()),
        hex if hex.starts_with('#') => Some(hex.to_string()),
        hex if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) => Some(format!("#{hex}")),
        _ => None,
    }
}

fn web(value: &Value, key: &str) -> Option<String> {
    owned(value, key).filter(|url| url.len() <= 256 && url.starts_with("http"))
}

/// One Slack attachment as an embed, cut to the embed limits
fn embed(attachment: &Value) -> SendableEmbed {
    let body = [text_of(attachment, "pretext"), text_of(attachment, "text")]
        .into_iter()
        .flatten()
        .map(from_mrkdwn)
        .collect::<Vec<_>>()
        .join("\n");
    let fields: Vec<EmbedField> = attachment
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| {
            let name = shorten(&from_mrkdwn(text_of(field, "title")?), 256);
            let value = shorten(&from_mrkdwn(text_of(field, "value")?), 1024);
            Some(EmbedField {
                name,
                value,
                inline: field.get("short").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .take(25)
        .collect();

    SendableEmbed {
        title: text_of(attachment, "title").map(|title| shorten(&from_mrkdwn(title), 256)),
        url: web(attachment, "title_link"),
        description: Some(shorten(&body, 4096)).filter(|text| !text.is_empty()),
        colour: colour(attachment),
        author: text_of(attachment, "author_name").map(|name| EmbedAuthor {
            name: shorten(name, 256),
            icon_url: web(attachment, "author_icon"),
            url: web(attachment, "author_link"),
        }),
        fields: (!fields.is_empty()).then_some(fields),
        footer: text_of(attachment, "footer").map(|text| EmbedFooter {
            text: shorten(text, 2048),
            icon_url: web(attachment, "footer_icon"),
        }),
        timestamp: attachment.get("ts").and_then(Value::as_i64).map(|seconds| {
            Timestamp::UNIX_EPOCH + Duration::seconds(seconds)
        }),
        image_url: owned(attachment, "image_url"),
        thumbnail_url: owned(attachment, "thumb_url"),
        ..Default::default()
    }
}

/// The readable text of Block Kit blocks
fn blocks_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(|block| {
            let text = block.pointer("/text/text").and_then(Value::as_str);
            let parts = block
                .get("fields")
                .or_else(|| block.get("elements"))
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|text| !text.is_empty());
            match (block.get("type").and_then(Value::as_str), text) {
                (Some("header"), Some(text)) => Some(format!("**{}**", from_mrkdwn(text))),
                (_, Some(text)) => Some(from_mrkdwn(text)),
                (_, None) => parts.as_deref().map(from_mrkdwn),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Slack payload as a webhook body
fn body_from_slack(payload: &Value) -> (v0::DataMessageSend, Option<String>, Option<String>) {
    let mut lines = vec![];
    if let Some(text) = text_of(payload, "text") {
        lines.push(from_mrkdwn(text));
    }
    if let Some(blocks) = payload.get("blocks").and_then(Value::as_array) {
        let text = blocks_text(blocks);
        // Slack shows blocks in place of `text`, which is the fallback for them
        if !text.is_empty() {
            lines = vec![text];
        }
    }
    let content = shorten(&lines.join("\n"), 2000);

    let embeds: Vec<SendableEmbed> = payload
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(embed)
        .filter(|embed| embed.text_length() > 0 || embed.image_url.is_some())
        .take(10)
        .collect();

    (
        v0::DataMessageSend {
            nonce: None,
            content: (!content.is_empty()).then_some(content),
            attachments: None,
            replies: None,
            embeds: (!embeds.is_empty()).then_some(embeds),
            masquerade: None,
            interactions: None,
            components: None,
            interaction: None,
            flags: None,
        },
        owned(payload, "username"),
        owned(payload, "icon_url"),
    )
}

/// # Executes a webhook in Slack's format
///
/// Takes what a Slack incoming webhook takes (`text`, `attachments`, `blocks`,
/// `username`, `icon_url`), as JSON or as `payload=<json>`. Returns the message
/// unless `wait=false`, which answers 204.
#[openapi(tag = "Webhooks")]
#[post("/<webhook_id>/<token>/slack?<wait>&<thread_id>", data = "<data>")]
#[allow(clippy::too_many_arguments)]
pub async fn webhook_execute_slack(
    db: &State<Database>,
    amqp: &State<AMQP>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    wait: Option<bool>,
    thread_id: Option<String>,
    data: String,
) -> Result<MaybeMessage> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let payload = json_from_body(&data)?;
    let (mut message, username, icon_url) = body_from_slack(&payload);
    if username.is_some() || icon_url.is_some() {
        message.masquerade = Some(v0::Masquerade {
            name: username,
            avatar: icon_url,
            colour: None,
        });
    }

    let delivery = Delivery {
        thread_id,
        allowed_mentions: Some(Default::default()),
        ..Default::default()
    };
    let key = IdempotencyKey::unchecked_from_string(ulid::Ulid::new().to_string());
    let sent = deliver(db, amqp, &caller, &webhook, message, delivery, key).await?;
    Ok(MaybeMessage(
        wait.unwrap_or(true).then(|| sent.into_model(None, None)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn links_and_styles() {
        for (slack, ours) in [
            ("<https://zrzur.com|Zrzur>", "[Zrzur](https://zrzur.com)"),
            ("<https://zrzur.com>", "https://zrzur.com"),
            ("*bold* and _italic_", "**bold** and _italic_"),
            ("~gone~", "~~gone~~"),
            ("two * stars * apart", "two * stars * apart"),
            ("`*code*`", "`*code*`"),
            ("a &amp; b &lt;c&gt;", "a & b <c>"),
            ("3 < 4 and no closing", "3 < 4 and no closing"),
        ] {
            assert_eq!(from_mrkdwn(slack), ours, "{slack}");
        }
    }

    #[test]
    fn slack_mass_mentions_ping_nobody() {
        assert_eq!(from_mrkdwn("<!channel> deploy"), "(channel) deploy");
        assert_eq!(from_mrkdwn("<!here>"), "(here)");
        assert_eq!(from_mrkdwn("<!everyone>"), "(all)");
        assert_eq!(from_mrkdwn("<!date^1392734382^{date}|Feb 18>"), "");
        assert_eq!(from_mrkdwn("<@U123|sami> hi"), "sami hi");
        assert_eq!(from_mrkdwn("<@U123> hi"), "@U123 hi");
        assert_eq!(from_mrkdwn("<#C123|general>"), "general");
    }

    #[test]
    fn colours() {
        assert_eq!(colour(&json!({ "color": "good" })).as_deref(), Some("#2EB67D"));
        assert_eq!(colour(&json!({ "color": "#36a64f" })).as_deref(), Some("#36a64f"));
        assert_eq!(colour(&json!({ "color": "36a64f" })).as_deref(), Some("#36a64f"));
        assert_eq!(colour(&json!({ "color": "nonsense" })), None);
    }

    #[test]
    fn an_attachment_is_cut_to_the_limits() {
        let long = "ب".repeat(9000);
        let embed = embed(&json!({
            "title": long, "text": long,
            "fields": (0..40).map(|n| json!({ "title": format!("f{n}"), "value": long })).collect::<Vec<_>>(),
        }));
        assert_eq!(embed.title.unwrap().chars().count(), 256);
        assert_eq!(embed.description.unwrap().chars().count(), 4096);
        assert_eq!(embed.fields.as_ref().unwrap().len(), 25);
        assert_eq!(embed.fields.unwrap()[0].value.chars().count(), 1024);
    }

    #[test]
    fn empty_payloads_stay_empty() {
        let (message, _, _) = body_from_slack(&json!({}));
        assert_eq!(message.content, None);
        assert_eq!(message.embeds, None);
    }
}
