//! Real sample payloads through the real routes. Sources and licences of the
//! fixtures: `fixtures/README.md`.

use rocket::http::{ContentType, Header, Status};
use serde_json::{json, Value};

use super::test_support::*;
use crate::util::test::TestHarness;

macro_rules! fixture {
    ($file:literal) => {
        ($file, include_str!(concat!("fixtures/github/", $file, ".json")))
    };
}

/// Header value, sample, whether a message is expected
const GITHUB: &[(&str, (&str, &str), bool)] = &[
    ("commit_comment", fixture!("commit_comment.created"), true),
    ("create", fixture!("create"), true),
    ("delete", fixture!("delete"), true),
    ("fork", fixture!("fork"), true),
    ("issue_comment", fixture!("issue_comment.created"), true),
    ("issues", fixture!("issues.opened"), true),
    ("issues", fixture!("issues.reopened"), true),
    // the action upstream refused with a 400
    ("issues", fixture!("issues.labeled"), false),
    ("member", fixture!("member.added"), true),
    ("public", fixture!("public"), true),
    ("pull_request", fixture!("pull_request.opened"), true),
    ("pull_request", fixture!("pull_request.closed"), true),
    ("pull_request", fixture!("pull_request.synchronize"), false),
    ("pull_request_review", fixture!("pull_request_review.submitted"), true),
    ("pull_request_review_comment", fixture!("pull_request_review_comment.created"), true),
    // a deleted branch has no commits: `delete` tells it
    ("push", fixture!("push.deleted-branch"), false),
    ("push", fixture!("push.with-commit"), true),
    ("release", fixture!("release.published"), true),
    ("watch", fixture!("watch.started"), true),
    ("check_run", fixture!("check_run.completed"), true),
    ("check_suite", fixture!("check_suite.completed"), true),
    ("discussion", fixture!("discussion.created"), true),
    ("discussion", fixture!("discussion.answered"), true),
    ("discussion_comment", fixture!("discussion_comment.created"), true),
    ("star", fixture!("star.created"), true),
    ("ping", fixture!("ping"), false),
];

async fn github(harness: &TestHarness, id: &str, token: &str, event: &str, body: String) -> Status {
    harness
        .client
        .post(format!("/webhooks/{id}/{token}/github"))
        .header(ContentType::JSON)
        .header(Header::new("X-GitHub-Event", event.to_string()))
        .body(body)
        .dispatch()
        .await
        .status()
}

#[rocket::async_test]
async fn every_github_sample_is_taken() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;

    for (event, (name, sample), shows) in GITHUB {
        // one webhook each: 5 in 2 seconds is not what is tested here
        let (id, token) = webhook(&harness, &scene, "GitHub").await;
        let status = github(&harness, &id, &token, event, sample.to_string()).await;
        assert_eq!(status, Status::NoContent, "{name}");

        let shown = harness
            .db
            .fetch_webhook(&id)
            .await
            .unwrap()
            .last_used
            .is_some();
        assert_eq!((*name, shown), (*name, *shows));
        harness.db.delete_webhook(&id).await.unwrap();
    }
}

#[rocket::async_test]
async fn the_text_of_a_github_message_reads_right() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "GitHub").await;

    let (_, (_, sample), _) = GITHUB.iter().find(|(event, _, _)| *event == "issues").unwrap();
    assert_eq!(github(&harness, &id, &token, "issues", sample.to_string()).await, Status::NoContent);

    let messages = call(
        &harness,
        Get,
        &format!("/channels/{}/messages", scene.channel),
        Some(&scene.session),
        None,
        None,
    )
    .await;
    let description = messages.body[0]["embeds"][0]["description"].as_str().unwrap();
    assert!(description.contains("Issue Opened #"), "{description}");
    assert!(description.contains("https://github.com/"), "{description}");
    assert_eq!(messages.body[0]["webhook"]["name"], json!("GitHub"));
}

#[rocket::async_test]
async fn unknown_github_events_answer_204() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "GitHub").await;
    for event in ["workflow_run", "deployment_status", "no_such_event"] {
        assert_eq!(github(&harness, &id, &token, event, "{}".to_string()).await, Status::NoContent, "{event}");
    }
    assert_eq!(github(&harness, &id, &token, "push", "not json".to_string()).await, Status::BadRequest);
}

#[rocket::async_test]
async fn arabic_github_text_does_not_break_the_route() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "GitHub").await;
    let title = "إصلاح ".repeat(200);
    let body = json!({
        "ref": "refs/heads/main",
        "compare": "https://github.com/a/b/compare/1...2",
        "commits": [{ "id": "1234567890", "url": "https://github.com/a/b/commit/1", "message": title, "author": { "name": "ماهر" } }],
        "sender": { "login": "m", "avatar_url": "https://avatars.githubusercontent.com/u/1", "html_url": "https://github.com/m" },
        "repository": { "full_name": "a/b", "html_url": "https://github.com/a/b" },
    });
    assert_eq!(github(&harness, &id, &token, "push", body.to_string()).await, Status::NoContent);
}

#[rocket::async_test]
async fn nothing_on_github_pings_anyone_here() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "GitHub").await;
    let body = json!({
        "action": "created",
        "issue": { "number": 1, "title": "@everyone" },
        "comment": { "body": format!("<@{}> @everyone", scene.user.id), "html_url": "https://github.com/a/b/issues/1#c" },
        "sender": { "login": "m" },
        "repository": { "full_name": "a/b" },
    });
    assert_eq!(github(&harness, &id, &token, "issue_comment", body.to_string()).await, Status::NoContent);
    let messages = call(
        &harness,
        Get,
        &format!("/channels/{}/messages", scene.channel),
        Some(&scene.session),
        None,
        None,
    )
    .await;
    assert!(messages.body[0].get("mentions").is_none(), "{}", messages.text);
    assert_eq!(messages.body[0]["flags"].as_u64().unwrap_or(0), 0, "{}", messages.text);
}

const SLACK: &[(&str, &str)] = &[
    ("text", include_str!("fixtures/slack/text.json")),
    ("attachments", include_str!("fixtures/slack/attachments.json")),
    ("blocks", include_str!("fixtures/slack/blocks.json")),
    ("legacy", include_str!("fixtures/slack/legacy-fields.json")),
];

#[rocket::async_test]
async fn every_slack_sample_is_taken() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;

    for (name, sample) in SLACK {
        let (id, token) = webhook(&harness, &scene, "Slack").await;
        let reply = call(
            &harness,
            Post,
            &format!("/webhooks/{id}/{token}/slack"),
            None,
            Some(serde_json::from_str::<Value>(sample).unwrap()),
            None,
        )
        .await;
        // the message is the default answer here
        assert_eq!(reply.status, Status::Ok, "{name}: {}", reply.text);
        assert_eq!(reply.body["webhook"]["name"], json!("Slack"), "{name}");
        harness.db.delete_webhook(&id).await.unwrap();
    }
}

#[rocket::async_test]
async fn slack_forms_and_wait() {
    let harness = TestHarness::new().await;
    let scene = scene(&harness).await;
    let (id, token) = webhook(&harness, &scene, "Slack").await;

    let response = harness
        .client
        .post(format!("/webhooks/{id}/{token}/slack"))
        .header(ContentType::Form)
        .body("payload=%7B%22text%22%3A%22form+text%22%7D")
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    let text = response.into_string().await.unwrap();
    assert!(text.contains("form text"), "{text}");

    let quiet = call(&harness, Post, &format!("/webhooks/{id}/{token}/slack?wait=false"), None, Some(json!({ "text": "x" })), None).await;
    assert_eq!(quiet.status, Status::NoContent);

    let empty = call(&harness, Post, &format!("/webhooks/{id}/{token}/slack"), None, Some(json!({})), None).await;
    assert_eq!(empty.status, Status::UnprocessableEntity, "{}", empty.text);
    assert_eq!(empty.body["type"], json!("EmptyMessage"));
}
