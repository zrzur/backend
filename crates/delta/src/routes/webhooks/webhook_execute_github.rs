//! GitHub's webhook format. Events are read field by field from the JSON, not
//! into fixed structs: GitHub adds fields and leaves some out per event, and a
//! missing one must cost a blank in the message, never a refused delivery.

use revolt_database::{
    util::{idempotency::IdempotencyKey, reference::Reference},
    Database, AMQP,
};
use revolt_models::v0::{self, SendableEmbed};
use revolt_result::{create_error, Error, Result};
use revolt_rocket_okapi::{
    r#gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
    revolt_okapi::openapi3::{MediaType, Parameter, ParameterValue},
};
use rocket::{http::Status, request::FromRequest, Request, State};
use rocket_empty::EmptyResponse;
use schemars::schema::SchemaObject;
use serde_json::Value;

use super::delivery::{deliver, Delivery};
use crate::util::webhooks::WebhookCaller;

#[derive(Debug, JsonSchema)]
pub struct EventHeader<'r>(pub &'r str);

impl std::ops::Deref for EventHeader<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

#[async_trait]
impl<'r> FromRequest<'r> for EventHeader<'r> {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> rocket::request::Outcome<Self, Self::Error> {
        let headers = request.headers();
        let Some(event) = headers.get_one("X-GitHub-Event") else {
            return rocket::request::Outcome::Error((
                Status::BadRequest,
                create_error!(InvalidOperation),
            ));
        };

        rocket::request::Outcome::Success(Self(event))
    }
}

impl<'r> OpenApiFromRequest<'r> for EventHeader<'r> {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        let mut content = schemars::Map::new();
        content.insert(
            "X-Github-Event".to_string(),
            MediaType {
                schema: Some(SchemaObject {
                    string: Some(Box::default()),
                    ..Default::default()
                }),
                example: None,
                examples: None,
                encoding: schemars::Map::new(),
                extensions: schemars::Map::new(),
            },
        );

        Ok(RequestHeaderInput::Parameter(Parameter {
            name: "X-Github-Event".to_string(),
            location: "header".to_string(),
            required: true,
            description: Some("The name of the github event".to_string()),
            deprecated: false,
            allow_empty_value: false,
            value: ParameterValue::Content { content },
            extensions: schemars::Map::new(),
        }))
    }
}

const GREEN: &str = "#0f890f";
const RED: &str = "#e73e3e";
const GREY: &str = "#202224";
const BLUE: &str = "#279adc";
const ORANGE: &str = "#d76a34";
const LIGHT_ORANGE: &str = "#d9916d";

/// Longest text taken from a comment, issue or commit list
const BODY_LENGTH: usize = 450;
const COMMITS_LENGTH: usize = 1000;
const COMMIT_LINE_LENGTH: usize = 50;

/// At most `length` characters. Counted in characters: slicing bytes would
/// panic in the middle of an Arabic letter.
fn shorten(text: &str, length: usize) -> String {
    if text.chars().count() <= length {
        return text.to_string();
    }
    let cut: String = text.chars().take(length.saturating_sub(3)).collect();
    format!("{cut}...")
}

fn first_line(text: &str, length: usize) -> String {
    shorten(text.lines().next().unwrap_or_default(), length)
}

/// A string in the payload, or nothing
fn get<'a>(body: &'a Value, path: &str) -> &'a str {
    body.pointer(path).and_then(Value::as_str).unwrap_or_default()
}

fn number(body: &Value, path: &str) -> String {
    body.pointer(path).map(Value::to_string).unwrap_or_default()
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// `Issue #12: title` as a line, with the person who did it on top
fn line(actor: &Value, headline: String, text: Option<String>, colour: &str) -> SendableEmbed {
    let description = match text {
        Some(text) if !text.is_empty() => format!("#### {headline}\n{text}"),
        _ => format!("#### {headline}"),
    };
    SendableEmbed {
        title: Some(shorten(get(actor, "/login"), 256)).filter(|login| !login.is_empty()),
        icon_url: Some(get(actor, "/avatar_url").to_string())
            .filter(|url| !url.is_empty() && url.len() <= 256),
        url: Some(get(actor, "/html_url").to_string())
            .filter(|url| !url.is_empty() && url.len() <= 256),
        description: Some(shorten(&description, 4096)),
        colour: Some(colour.to_string()),
        ..Default::default()
    }
}

/// The message for an event, or `None` for events and actions nothing is
/// shown for: unknown ones too, GitHub adds new ones all the time
fn render(event: &str, body: &Value) -> Option<SendableEmbed> {
    let null = Value::Null;
    let sender = body.get("sender").unwrap_or(&null);
    let repo = get(body, "/repository/full_name");
    let repo_url = get(body, "/repository/html_url");
    let action = get(body, "/action");
    let excerpt = |path: &str| Some(shorten(get(body, path), BODY_LENGTH));

    Some(match (event, action) {
        ("star", "created") | ("watch", "started") => {
            line(sender, format!("[[{repo}] New star added]({repo_url})"), None, GREY)
        }

        ("push", _) => {
            let branch = get(body, "/ref").split('/').skip(2).collect::<Vec<_>>().join("/");
            let compare = get(body, "/compare");
            let commits = body.get("commits").and_then(Value::as_array)?;
            if body.get("forced").and_then(Value::as_bool).unwrap_or(false) {
                let after = short_sha(get(body, "/after"));
                line(
                    sender,
                    format!("[{repo}] Branch {branch} was force-pushed to {after}"),
                    Some(format!("[compare changes]({compare})")),
                    RED,
                )
            } else if commits.is_empty() {
                // new or deleted branch: `create` and `delete` tell that
                return None;
            } else {
                let list = commits
                    .iter()
                    .map(|commit| {
                        format!(
                            "[`{}`]({}) {} - {}",
                            short_sha(get(commit, "/id")),
                            get(commit, "/url"),
                            first_line(get(commit, "/message"), COMMIT_LINE_LENGTH),
                            get(commit, "/author/name")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let count = commits.len();
                let word = if count == 1 { "commit" } else { "commits" };
                line(
                    sender,
                    format!("[[{repo}:{branch}] {count} new {word}]({compare})"),
                    Some(shorten(&list, COMMITS_LENGTH)),
                    BLUE,
                )
            }
        }

        ("commit_comment", _) => line(
            sender,
            format!(
                "[[{repo}] New comment on commit `{}`]({})",
                short_sha(get(body, "/comment/commit_id")),
                get(body, "/comment/html_url")
            ),
            excerpt("/comment/body"),
            GREY,
        ),

        ("create", _) => line(
            sender,
            format!("[{repo}] New {} created: {}", get(body, "/ref_type"), get(body, "/ref")),
            None,
            GREY,
        ),
        ("delete", _) => line(
            sender,
            format!("[{repo}] {} deleted: {}", get(body, "/ref_type"), get(body, "/ref")),
            None,
            GREY,
        ),
        ("fork", _) => line(
            sender,
            format!(
                "[[{repo}] Fork created: {}]({})",
                get(body, "/forkee/full_name"),
                get(body, "/forkee/html_url")
            ),
            None,
            GREY,
        ),
        ("public", _) => line(
            sender,
            format!("[[{repo}] The repository is now public]({repo_url})"),
            None,
            GREEN,
        ),
        ("member", "added") => line(
            sender,
            format!(
                "[[{repo}] New collaborator: {}]({})",
                get(body, "/member/login"),
                get(body, "/member/html_url")
            ),
            None,
            GREY,
        ),

        ("issues", "opened" | "closed" | "reopened") => {
            let (word, colour) = match action {
                "opened" => ("Opened", ORANGE),
                "closed" => ("Closed", GREY),
                _ => ("Reopened", GREEN),
            };
            line(
                sender,
                format!(
                    "[[{repo}] Issue {word} #{}: {}]({})",
                    number(body, "/issue/number"),
                    get(body, "/issue/title"),
                    get(body, "/issue/html_url")
                ),
                (action == "opened").then(|| shorten(get(body, "/issue/body"), BODY_LENGTH)),
                colour,
            )
        }

        ("issue_comment", "created") => line(
            sender,
            format!(
                "[[{repo}] New comment on issue #{}: {}]({})",
                number(body, "/issue/number"),
                get(body, "/issue/title"),
                get(body, "/comment/html_url")
            ),
            excerpt("/comment/body"),
            LIGHT_ORANGE,
        ),

        ("pull_request", "opened" | "closed" | "reopened") => {
            let merged = body
                .pointer("/pull_request/merged")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let (word, colour) = match action {
                "opened" => ("Opened", ORANGE),
                "closed" if merged => ("Merged", GREEN),
                "closed" => ("Closed", GREY),
                _ => ("Reopened", GREEN),
            };
            line(
                sender,
                format!(
                    "[[{repo}] Pull Request {word} #{}: {}]({})",
                    number(body, "/pull_request/number"),
                    get(body, "/pull_request/title"),
                    get(body, "/pull_request/html_url")
                ),
                (action == "opened").then(|| shorten(get(body, "/pull_request/body"), BODY_LENGTH)),
                colour,
            )
        }

        ("pull_request_review", "submitted") => {
            let (word, colour) = match get(body, "/review/state").to_ascii_lowercase().as_str() {
                "approved" => ("approved", GREEN),
                "changes_requested" => ("changes requested on", RED),
                _ => ("reviewed", LIGHT_ORANGE),
            };
            line(
                sender,
                format!(
                    "[[{repo}] Pull Request {word} #{}: {}]({})",
                    number(body, "/pull_request/number"),
                    get(body, "/pull_request/title"),
                    get(body, "/review/html_url")
                ),
                excerpt("/review/body"),
                colour,
            )
        }

        ("pull_request_review_comment", "created") => line(
            sender,
            format!(
                "[[{repo}] New review comment on pull request #{}: {}]({})",
                number(body, "/pull_request/number"),
                get(body, "/pull_request/title"),
                get(body, "/comment/html_url")
            ),
            excerpt("/comment/body"),
            LIGHT_ORANGE,
        ),

        ("release", "published") => {
            let tag = get(body, "/release/tag_name");
            let name = get(body, "/release/name");
            line(
                sender,
                format!(
                    "[[{repo}] Release published: {}]({})",
                    if name.is_empty() { tag } else { name },
                    get(body, "/release/html_url")
                ),
                excerpt("/release/body"),
                GREEN,
            )
        }

        ("check_run" | "check_suite", "completed") => {
            let (object, label) = if event == "check_run" {
                ("/check_run", get(body, "/check_run/name"))
            } else {
                ("/check_suite", "Check suite")
            };
            let conclusion = get(body, &format!("{object}/conclusion"));
            let colour = match conclusion {
                "success" => GREEN,
                "failure" | "timed_out" | "startup_failure" => RED,
                _ => GREY,
            };
            let branch = get(body, &format!("{object}/head_branch"));
            let branch = if branch.is_empty() {
                get(body, "/check_run/check_suite/head_branch")
            } else {
                branch
            };
            line(
                sender,
                format!("[{repo}:{branch}] {label}: {conclusion}"),
                Some(get(body, &format!("{object}/html_url")).to_string()),
                colour,
            )
        }

        ("discussion", "created") => line(
            sender,
            format!(
                "[[{repo}] New discussion #{}: {}]({})",
                number(body, "/discussion/number"),
                get(body, "/discussion/title"),
                get(body, "/discussion/html_url")
            ),
            excerpt("/discussion/body"),
            LIGHT_ORANGE,
        ),
        ("discussion", "answered") => line(
            body.pointer("/answer/user").unwrap_or(sender),
            format!(
                "[[{repo}] Discussion #{} marked answered: {}]({})",
                number(body, "/discussion/number"),
                get(body, "/discussion/title"),
                get(body, "/answer/html_url")
            ),
            excerpt("/answer/body"),
            LIGHT_ORANGE,
        ),
        ("discussion_comment", "created") => line(
            sender,
            format!(
                "[[{repo}] New comment on discussion #{}: {}]({})",
                number(body, "/discussion/number"),
                get(body, "/discussion/title"),
                get(body, "/comment/html_url")
            ),
            excerpt("/comment/body"),
            LIGHT_ORANGE,
        ),

        _ => return None,
    })
}

/// GitHub sends JSON, or `payload=<json>` when a hook is set to form encoding
pub fn json_from_body(data: &str) -> Result<Value> {
    let invalid = || {
        create_error!(FailedValidation {
            error: "the body is not valid JSON".to_string()
        })
    };
    if let Ok(value) = serde_json::from_str(data) {
        return Ok(value);
    }
    let payload = url::form_urlencoded::parse(data.as_bytes())
        .find(|(key, _)| key == "payload")
        .ok_or_else(invalid)?;
    serde_json::from_str(&payload.1).map_err(|_| invalid())
}

/// # Executes a webhook specific to github
///
/// Takes a GitHub webhook delivery and posts what happened. Events and
/// actions that show nothing answer 204 too, so GitHub keeps the hook healthy.
#[openapi(tag = "Webhooks")]
#[post("/<webhook_id>/<token>/github?<thread_id>", data = "<data>")]
#[allow(clippy::too_many_arguments)]
pub async fn webhook_execute_github(
    db: &State<Database>,
    amqp: &State<AMQP>,
    caller: WebhookCaller<'_>,
    webhook_id: Reference<'_>,
    token: String,
    thread_id: Option<String>,
    event: EventHeader<'_>,
    data: String,
) -> Result<EmptyResponse> {
    let webhook = caller.authorise(db, webhook_id, &token).await?;
    let body = json_from_body(&data)?;

    let Some(embed) = render(&event, &body) else {
        return Ok(EmptyResponse);
    };

    let message = v0::DataMessageSend {
        nonce: None,
        content: None,
        attachments: None,
        replies: None,
        embeds: Some(vec![embed]),
        masquerade: None,
        interactions: None,
        components: None,
        interaction: None,
        flags: None,
    };
    let delivery = Delivery {
        thread_id,
        // the text comes from strangers on GitHub: nothing in it pings anyone
        allowed_mentions: Some(Default::default()),
        ..Default::default()
    };
    let key = IdempotencyKey::unchecked_from_string(ulid::Ulid::new().to_string());
    deliver(db, amqp, &caller, &webhook, message, delivery, key).await?;
    Ok(EmptyResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortening_counts_characters() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("exactly 10", 10), "exactly 10");
        assert_eq!(shorten("one too long", 10), "one too...");
        // bytes would cut this in the middle of a letter
        let arabic = "مرحبا ".repeat(100);
        assert_eq!(shorten(&arabic, 50).chars().count(), 50);
    }

    #[test]
    fn first_line_ignores_the_rest() {
        assert_eq!(first_line("Fix\n\nlong body", 50), "Fix");
        assert_eq!(first_line("", 50), "");
    }

    #[test]
    fn short_shas_never_panic() {
        assert_eq!(short_sha("1234567890"), "12345678");
        assert_eq!(short_sha("12"), "12");
        assert_eq!(short_sha(""), "");
    }

    #[test]
    fn unknown_events_and_actions_show_nothing() {
        let body = serde_json::json!({ "action": "whatever", "sender": { "login": "a" } });
        for event in ["workflow_run", "gollum", "made_up", "issues", "pull_request", "ping"] {
            assert!(render(event, &body).is_none(), "{event}");
        }
    }

    #[test]
    fn a_form_encoded_delivery_is_understood() {
        let value = json_from_body("payload=%7B%22a%22%3A1%7D").unwrap();
        assert_eq!(value["a"], 1);
        assert!(json_from_body("not json").is_err());
    }
}
