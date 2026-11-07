//! Messages from Zrzur's official accounts to a person's private messages,
//! written in the language that person uses the app in (`User::locale`,
//! reported by the app), English when we don't have their language yet.
//! Tickets come from Zrzur Support, moderation from Zrzur Safety, news from
//! Zrzur Updates and everything else from Zrzur ([`sender_of`]).
//!
//! The texts live in `assets/official/<language>.json`, one file per
//! language with the same keys. To add a language, add its file and one
//! line to `LANGUAGES`; the tests check that every file has every key with
//! the same placeholders.
//!
//! Sending is best effort: the decision or answer is already saved and
//! shown in the app, so a failure here is logged and never undoes it.

use std::{collections::HashMap, sync::LazyLock};

use revolt_database::{send_official_message, Database, OfficialAccount};
use revolt_models::v0::{
    safety_reference, SafetyAction, SafetyActionBasis, SafetyActionKind, SafetyCase, SafetyCategory,
    SafetyGround, SafetyTargetKind, PUBLIC_REPORTER_ID,
};
use serde::Deserialize;

/// Languages we have texts for, the first is the fallback
const LANGUAGES: &[(&str, &str)] = &[
    ("en", include_str!("../../assets/official/en.json")),
    ("ar", include_str!("../../assets/official/ar.json")),
    ("de", include_str!("../../assets/official/de.json")),
];

#[derive(Deserialize, Clone)]
struct Text {
    title: String,
    body: String,
}

static TEXTS: LazyLock<HashMap<&'static str, HashMap<String, Text>>> = LazyLock::new(|| {
    LANGUAGES
        .iter()
        .map(|(code, json)| (*code, serde_json::from_str(json).expect("valid official message file")))
        .collect()
});

/// Our language code for a locale such as `de-DE`, or the fallback
fn language_of(locale: Option<&str>) -> &'static str {
    let wanted = locale.unwrap_or_default().to_lowercase();
    let base = wanted.split(['-', '_']).next().unwrap_or_default();
    LANGUAGES
        .iter()
        .map(|(code, _)| *code)
        .find(|code| *code == wanted || *code == base)
        .unwrap_or(LANGUAGES[0].0)
}

fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    // a value left empty must not leave a gap
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

fn text(language: &str, key: &str) -> Text {
    TEXTS
        .get(language)
        .and_then(|texts| texts.get(key))
        .or_else(|| TEXTS[LANGUAGES[0].0].get(key))
        .cloned()
        .unwrap_or_else(|| panic!("missing official message `{key}`"))
}

/// The message for `key` in `language` (the app shows who sent it and
/// the password warning around the conversation, not in every message)
fn render(language: &str, key: &str, vars: &[(&str, String)]) -> String {
    let t = text(language, key);
    sign(language, format!("**{}**\n\n{}", fill(&t.title, vars), fill(&t.body, vars)))
}

/// A formal message ends with the team's name
fn sign(language: &str, message: String) -> String {
    format!("{message}\n\n{}", text(language, "signature").body)
}

async fn language_for(db: &Database, user_id: &str) -> &'static str {
    let locale = db.fetch_user(user_id).await.ok().and_then(|u| u.locale);
    language_of(locale.as_deref())
}

/// Which account sends the message `key`
fn sender_of(key: &str) -> OfficialAccount {
    let area = key.split('.').next().unwrap_or_default();
    match (area, key) {
        ("ticket", _) | ("change_request", _) => OfficialAccount::Support,
        ("action", _) | ("appeal", _) | ("report", _) | ("reporter", _) => OfficialAccount::Safety,
        (
            _,
            "bot.disabled" | "bot.unverified" | "plugin.revoked" | "plugin.suspended" | "plugin.removed"
            | "publisher.suspended" | "creator.suspended" | "creator.revoked" | "discover.removed"
            | "server.unverified" | "server.nsfw" | "sessions.ended",
        ) => OfficialAccount::Safety,
        _ => OfficialAccount::Zrzur,
    }
}

async fn deliver(db: &Database, from: OfficialAccount, user_id: &str, content: String) {
    if let Err(error) = send_official_message(db, None, from, user_id, content).await {
        log::warn!("official message to {user_id} not sent: {:?}", error.error_type);
    }
}

/// Send the message `key` to one person, in their language
pub async fn send(db: &Database, user_id: &str, key: &str, vars: &[(&str, &str)]) {
    let language = language_for(db, user_id).await;
    let vars: Vec<(&str, String)> = vars
        .iter()
        .map(|(k, v)| (*k, if *k == "note" || *k == "answer" || *k == "reason" { quote(v) } else { v.to_string() }))
        .collect();
    deliver(db, sender_of(key), user_id, render(language, key, &vars)).await;
}

/// Text written by staff, as plain text (no quote boxes in a formal message)
fn quote(text: &str) -> String {
    text.trim().to_string()
}

// ---- one function per kind of event ----

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The one address people write to about decisions, appeals and reports.
/// Set `ZRZUR_CONTACT_EMAIL`; until then the interim address.
pub fn contact_email() -> String {
    std::env::var("ZRZUR_CONTACT_EMAIL")
        .ok()
        .map(|address| address.trim().to_string())
        .filter(|address| !address.is_empty())
        .unwrap_or_else(|| "support@zrzur.com".to_string())
}

/// Page with the rules a decision refers to
const RULES_URL: &str = "https://zrzur.com/legal/community-guidelines";

/// A rule stored as `rule:<Category>` is shown by its translated name; any
/// other text is a reviewer's own words and is shown as written.
pub const RULE_PREFIX: &str = "rule:";

/// Name of a Zrzur rule in a language, from the case category
pub fn rule_name(language: &str, category: SafetyCategory) -> String {
    text(language, &format!("rule.{category:?}")).body
}

/// Optional name of a certified out-of-court body Zrzur works with
/// (`ZRZUR_ODS_BODY`). Without it the notice only points to the European
/// Commission's list; no body is ever invented.
pub fn out_of_court_body() -> Option<String> {
    std::env::var("ZRZUR_ODS_BODY")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn action_key(action: &SafetyAction) -> &'static str {
    match action.kind {
        SafetyActionKind::Warning => "action.warning",
        SafetyActionKind::RemoveMessage => "action.remove_message",
        SafetyActionKind::SuspendAccount => "action.suspend",
        SafetyActionKind::BanAccount => "action.ban",
        SafetyActionKind::DisableBot => "action.disable_bot",
        SafetyActionKind::RemoveDiscoverListing => "action.remove_discover",
        SafetyActionKind::SuspendCommunityItem | SafetyActionKind::SuspendCreator => "action.suspend_design",
    }
}

/// "7 days." in the language, with the right form for Arabic (1, 2, 3-10, 11+)
fn days_text(language: &str, days: i64) -> String {
    let t = |key: &str| text(language, key).body;
    if language == "ar" {
        return match days {
            1 => t("statement.days_one"),
            2 => t("statement.days_two"),
            3..=10 => fill(&t("statement.days_few"), &[("days", days.to_string())]),
            _ => fill(&t("statement.days"), &[("days", days.to_string())]),
        };
    }
    match days {
        1 => t("statement.days_one"),
        2 => t("statement.days_two"),
        _ => fill(&t("statement.days_few"), &[("days", days.to_string())]),
    }
}

/// The statement of reasons (DSA Art. 17(3)) in one language: what it
/// followed, facts, reason, ground (law or terms, with the rule's name and a
/// link), automation, territorial scope, duration, how to appeal and the
/// out-of-court option.
pub fn statement_text(
    language: &str,
    action: &SafetyAction,
    app_url: &str,
    out_of_court: Option<&str>,
) -> String {
    let t = |key: &str| text(language, key).body;
    let contact = contact_email();
    let yes_no = |value: bool| t(if value { "statement.yes" } else { "statement.no" });
    let facts = if action.facts.trim().is_empty() { action.public_reason.clone() } else { action.facts.clone() };
    let mut rule_link = String::new();
    let ground = match &action.ground {
        SafetyGround::Law { law, provision } => fill(
            &t("statement.ground_law"),
            &[("law", law.clone()), ("provision", provision.clone())],
        ),
        SafetyGround::Terms { rule } => {
            let name = match rule.strip_prefix(RULE_PREFIX) {
                Some(category) => {
                    rule_link = fill(&t("statement.rule_link"), &[("link", RULES_URL.to_string())]);
                    let key = format!("rule.{category}");
                    if TEXTS[LANGUAGES[0].0].contains_key(&key) { t(&key) } else { category.to_string() }
                }
                // actions older than statements of reasons say only this
                None if rule == "Terms of Service" => t("rule.terms_of_service"),
                None => rule.clone(),
            };
            fill(&t("statement.ground_terms"), &[("rule", name)])
        }
    };
    let scope = if action.territorial_scope.trim().eq_ignore_ascii_case("worldwide") {
        t("statement.worldwide")
    } else {
        action.territorial_scope.clone()
    };
    let duration = match action.expires_at {
        Some(end) => days_text(language, ((end - action.created_at).max(0) + DAY_MS - 1) / DAY_MS),
        None => t("statement.open_ended"),
    };
    // an old action may not say what it followed: then nothing is claimed
    let basis = match action.basis {
        SafetyActionBasis::Report => t("statement.basis_report"),
        SafetyActionBasis::Notice => t("statement.basis_notice"),
        SafetyActionBasis::OwnInitiative => t("statement.basis_own"),
        SafetyActionBasis::Unknown => String::new(),
    };
    let parts = [
        basis,
        fill(&t("statement.facts"), &[("facts", facts)]),
        fill(&t("statement.reason"), &[("reason", quote(&action.public_reason))]),
        ground,
        rule_link,
        fill(
            &t("statement.automation"),
            &[("detection", yes_no(action.automated_detection)), ("decided", yes_no(action.automated_decision))],
        ),
        fill(&t("statement.scope"), &[("scope", scope)]),
        fill(&t("statement.duration"), &[("duration", duration)]),
        if action.appealable {
            fill(
                &t("statement.appeal"),
                &[
                    ("link", app_url.to_string()),
                    ("contact", contact.clone()),
                    ("reference", safety_reference(&action.id)),
                ],
            )
        } else {
            String::new()
        },
        t("statement.out_of_court"),
        out_of_court
            .map(|body| fill(&t("statement.out_of_court_body"), &[("body", body.to_string())]))
            .unwrap_or_default(),
        fill(&t("statement.contact"), &[("contact", contact)]),
    ];
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n")
}

/// The whole message for an action: headline and statement of reasons
pub fn action_message(language: &str, action: &SafetyAction, app_url: &str, out_of_court: Option<&str>) -> String {
    let statement = statement_text(language, action, app_url, out_of_court);
    render(language, action_key(action), &[("statement", statement)])
}

/// Send a plain message by e-mail in the person's language. Best effort,
/// like the private message: the decision is already saved.
pub async fn email_in_language(to: &str, language: &str, subject: &str, body: &str) {
    use revolt_database::util::email::{email_templates, send_email, validate_email};
    let smtp = revolt_config::config().await.api.smtp;
    if smtp.host.is_empty() || validate_email(to).is_err() {
        return;
    }
    let mut template = email_templates().await.statement;
    template.title = subject.chars().filter(|c| !c.is_control()).take(200).collect();
    let variables = serde_json::json!({
        "body": body.replace("**", ""),
        "dir": if language == "ar" { "rtl" } else { "ltr" },
    });
    let to = to.to_string();
    let result = tokio::task::spawn_blocking(move || send_email(&smtp, to, &template, variables)).await;
    if !matches!(result, Ok(Ok(()))) {
        log::warn!("e-mail notice not sent");
    }
}

/// Trust & Safety took an action on this person's account or content: tell
/// them in their private messages, and for a suspension or ban also by
/// e-mail (they can no longer sign in to read it), with the full statement
/// of reasons in their language.
pub async fn action_taken(db: &Database, user_id: &str, action: &SafetyAction) {
    let language = language_for(db, user_id).await;
    let config = revolt_config::config().await;
    let ods = out_of_court_body();
    let content = action_message(language, action, &config.hosts.app, ods.as_deref());
    deliver(db, OfficialAccount::Safety, user_id, content.clone()).await;
    if matches!(action.kind, SafetyActionKind::SuspendAccount | SafetyActionKind::BanAccount) {
        if let Ok(account) = db.fetch_account(user_id).await {
            let subject = text(language, action_key(action)).title;
            let body = content.replacen(&format!("**{subject}**\n\n"), "", 1);
            email_in_language(&account.email, language, &subject, &body).await;
        }
    }
}

/// How a case ended, for the person who reported it
pub enum ReportOutcome<'a> {
    /// An action was taken
    Action(&'a SafetyAction),
    NoViolation,
    Duplicate,
    Closed,
    /// More information is needed (the reviewer's question)
    Info(&'a str),
    /// The reviewer's note after a decision that was already told
    Note,
    /// An appeal reversed the earlier decision
    Overturned,
    /// A reviewer reversed the earlier decision without an appeal
    Revoked,
}

fn what_key(action: &SafetyAction) -> &'static str {
    match action.kind {
        SafetyActionKind::Warning => "reporter.what.warning",
        SafetyActionKind::RemoveMessage => "reporter.what.remove_message",
        SafetyActionKind::SuspendAccount => "reporter.what.suspend",
        SafetyActionKind::BanAccount => "reporter.what.ban",
        SafetyActionKind::DisableBot => "reporter.what.disable_bot",
        SafetyActionKind::RemoveDiscoverListing => "reporter.what.remove_discover",
        SafetyActionKind::SuspendCommunityItem | SafetyActionKind::SuspendCreator => "reporter.what.suspend_design",
    }
}

/// The notice to the person who reported (DSA Art. 16(5), 16(6)): what we
/// decided, the reviewer's note, whether automated means were used, and how to
/// challenge it. Returns `(subject, body)`.
pub fn reporter_notice(
    language: &str,
    reference: &str,
    outcome: &ReportOutcome<'_>,
    note: Option<&str>,
    public: bool,
) -> (String, String) {
    let t = |key: &str| text(language, key).body;
    let contact = contact_email();
    let vars = |extra: &[(&'static str, String)]| -> Vec<(&'static str, String)> {
        let mut all = vec![("reference", reference.to_string()), ("contact", contact.clone())];
        all.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        all
    };
    let subject = fill(&text(language, "reporter.subject").title, &vars(&[]));
    let mut parts = vec![fill(&t("reporter.opening"), &vars(&[]))];
    let (outcome_line, automation) = match outcome {
        ReportOutcome::Action(action) => (
            fill(&t("reporter.outcome.action"), &vars(&[("what", t(what_key(action)))])),
            Some((action.automated_detection, action.automated_decision)),
        ),
        ReportOutcome::NoViolation => (t("reporter.outcome.no_violation"), None),
        ReportOutcome::Duplicate => (t("reporter.outcome.duplicate"), None),
        ReportOutcome::Closed => (t("reporter.outcome.closed"), None),
        ReportOutcome::Note => (String::new(), None),
        ReportOutcome::Overturned => (t("reporter.outcome.overturned"), None),
        ReportOutcome::Revoked => (t("reporter.outcome.revoked"), None),
        ReportOutcome::Info(question) => (
            fill(&t("reporter.outcome.info"), &vars(&[("question", quote(question))])),
            None,
        ),
    };
    parts.push(outcome_line);
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        parts.push(fill(&t("reporter.note"), &[("note", note.to_string())]));
    }
    match outcome {
        // a public reporter has no account and our address is a no-reply one:
        // they write to the contact address; others answer in the app
        ReportOutcome::Info(_) => parts.push(fill(
            &t(if public { "reporter.info_reply_public" } else { "reporter.info_reply_app" }),
            &vars(&[]),
        )),
        ReportOutcome::Note => parts.push(fill(&t("reporter.redress"), &vars(&[]))),
        _ => {
            parts.push(match automation {
                Some((detected, decided)) if detected || decided => {
                    let yes_no = |v: bool| t(if v { "statement.yes" } else { "statement.no" });
                    fill(
                        &t("statement.automation"),
                        &[("detection", yes_no(detected)), ("decided", yes_no(decided))],
                    )
                }
                _ => t("reporter.human"),
            });
            parts.push(fill(&t("reporter.redress"), &vars(&[])));
        }
    }
    (subject, parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n"))
}

/// Tell the person who reported a case how it went, by e-mail if they
/// reported without an account (and gave an address), otherwise in their
/// private messages. Best effort.
pub async fn report_outcome(db: &Database, case: &SafetyCase, outcome: ReportOutcome<'_>, note: Option<&str>) {
    // cases a staff member opened on their own have no reporter to answer
    if case.subcategory.as_deref() == Some("staff") {
        return;
    }
    if let Some(public) = &case.public_reporter {
        if let Some(email) = &public.email {
            let language = language_of(Some(&public.language));
            let (subject, body) = reporter_notice(language, &case.reference, &outcome, note, true);
            email_in_language(email, language, &subject, &body).await;
        }
        return;
    }
    if case.reporter_id == PUBLIC_REPORTER_ID {
        return;
    }
    let language = language_for(db, &case.reporter_id).await;
    let (subject, body) = reporter_notice(language, &case.reference, &outcome, note, false);
    deliver(db, OfficialAccount::Safety, &case.reporter_id, sign(language, format!("**{subject}**\n\n{body}"))).await;
}

/// Receipt for a report without an account: the reference only, never text
/// the caller wrote (the address could belong to someone else)
pub async fn public_report_receipt(email: &str, language: &str, reference: &str) {
    let language = language_of(Some(language));
    let t = text(language, "public_report.receipt");
    let vars = [("reference", reference.to_string()), ("contact", contact_email())];
    email_in_language(email, language, &fill(&t.title, &vars), &fill(&t.body, &vars)).await;
}

pub async fn action_revoked(db: &Database, user_id: &str, note: &str) {
    send(db, user_id, "action.revoked", &[("note", note)]).await;
}

pub async fn reporter_answer(db: &Database, user_id: &str, reference: &str, note: &str) {
    send(db, user_id, "report.answer", &[("reference", reference), ("note", note)]).await;
}

pub async fn appeal_decided(db: &Database, user_id: &str, overturned: bool, note: &str) {
    let key = if overturned { "appeal.overturned" } else { "appeal.upheld" };
    send(db, user_id, key, &[("note", note)]).await;
}

pub async fn ticket_answered(db: &Database, user_id: &str, subject: &str, answer: &str, closed: bool) {
    let key = if closed { "ticket.answer_closed" } else { "ticket.answer" };
    send(db, user_id, key, &[("subject", subject), ("answer", answer)]).await;
}

/// A notice the team wrote in several languages; the person gets theirs
pub async fn notice(db: &Database, user_id: &str, title: &HashMap<String, String>, body: &HashMap<String, String>, link: Option<&str>) {
    let language = language_for(db, user_id).await;
    let pick = |map: &HashMap<String, String>| {
        map.get(language).or_else(|| map.get(LANGUAGES[0].0)).cloned().unwrap_or_default()
    };
    let more = link.map(|l| format!("\n\n{l}")).unwrap_or_default();
    deliver(db, OfficialAccount::Updates, user_id, sign(language, format!("**{}**\n\n{}{more}", pick(title), pick(body)))).await;
}

pub async fn bot(db: &Database, owner_id: &str, key: &str, name: &str, note: Option<&str>) {
    send(db, owner_id, key, &[("name", name), ("note", note.unwrap_or_default())]).await;
}

pub async fn discover_decided(db: &Database, owner_id: &str, key: &str, name: &str, note: Option<&str>) {
    send(db, owner_id, key, &[("name", name), ("note", note.unwrap_or_default())]).await;
}

pub async fn plugin(db: &Database, user_id: &str, key: &str, name: &str, version: &str, note: Option<&str>) {
    send(db, user_id, key, &[("name", name), ("version", version), ("note", note.unwrap_or_default())]).await;
}

pub async fn server(db: &Database, user_id: &str, key: &str, name: &str, note: &str) {
    send(db, user_id, key, &[("name", name), ("note", note)]).await;
}

pub async fn creator(db: &Database, user_id: &str, action: &str, note: &str) {
    let key = match action {
        "grant" => "creator.granted",
        "suspend" => "creator.suspended",
        "revoke" => "creator.revoked",
        _ => "creator.reinstated",
    };
    send(db, user_id, key, &[("note", note)]).await;
}

pub async fn badge(db: &Database, user_id: &str, granted: bool, badge_id: &str, note: &str) {
    let language = language_for(db, user_id).await;
    let key = format!("badge_name.{badge_id}");
    let name = TEXTS[language]
        .get(&key)
        .or_else(|| TEXTS[LANGUAGES[0].0].get(&key))
        .map(|t| t.title.clone())
        .unwrap_or_else(|| badge_id.to_string());
    let vars = [("name", name), ("note", quote(note))];
    deliver(db, OfficialAccount::Zrzur, user_id, render(language, if granted { "badge.granted" } else { "badge.revoked" }, &vars)).await;
}

/// A reviewer acted on a bug report (`action` is the review action)
pub async fn bug(db: &Database, user_id: &str, action: &str, title: &str, note: &str) {
    let key = format!("bug.{action}");
    if TEXTS[LANGUAGES[0].0].contains_key(&key) {
        send(db, user_id, &key, &[("title", title), ("note", note)]).await;
    }
}

pub async fn feather_granted(db: &Database, user_id: &str, days: i64) {
    send(db, user_id, "feather.granted", &[("days", &days.to_string())]).await;
}

pub async fn refunded(db: &Database, user_id: &str, amount: &str, note: &str) {
    send(db, user_id, "billing.refunded", &[("amount", amount), ("note", note)]).await;
}

pub async fn beta(db: &Database, user_id: &str, action: &str) {
    let key = match action {
        "invite" => "beta.invited",
        "approve" => "beta.approved",
        "remove" => "beta.removed",
        _ => return,
    };
    send(db, user_id, key, &[]).await;
}

pub async fn change_request(db: &Database, user_id: &str) {
    send(db, user_id, "change_request", &[]).await;
}

pub async fn sessions_ended(db: &Database, user_id: &str, note: &str) {
    send(db, user_id, "sessions.ended", &[("note", note)]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name.to_string()))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn every_language_has_every_message() {
        let base = &TEXTS[LANGUAGES[0].0];
        for (code, _) in LANGUAGES {
            let texts = &TEXTS[code];
            let mut keys: Vec<_> = texts.keys().collect();
            let mut expected: Vec<_> = base.keys().collect();
            keys.sort();
            expected.sort();
            assert_eq!(keys, expected, "{code} has different keys");
            for (key, t) in base {
                assert_eq!(placeholders(&texts[key].title), placeholders(&t.title), "{code} {key} title");
                assert_eq!(placeholders(&texts[key].body), placeholders(&t.body), "{code} {key} body");
            }
        }
    }

    #[test]
    fn grant_notices_carry_no_staff_reason() {
        let secret = "internal staff reason";
        let vars = [("note", secret.to_string()), ("name", "x".to_string()), ("version", "1".to_string())];
        for language in ["en", "ar", "de"] {
            for key in [
                "badge.granted",
                "creator.granted",
                "creator.reinstated",
                "bot.verified",
                "discover.approved",
                "plugin.approved",
                "plugin.reinstated",
                "server.verified",
                "server.not_nsfw",
                "server.transferred_to",
            ] {
                assert!(!render(language, key, &vars).contains(secret), "{language} {key}");
            }
            // the person must see why here
            for key in ["creator.suspended", "bot.denied", "plugin.rejected", "badge.revoked", "appeal.upheld"] {
                assert!(render(language, key, &vars).contains(secret), "{language} {key}");
            }
        }
    }

    #[test]
    fn notices_come_from_the_fitting_account() {
        assert_eq!(sender_of("ticket.answer"), OfficialAccount::Support);
        assert_eq!(sender_of("change_request"), OfficialAccount::Support);
        assert_eq!(sender_of("action.suspend"), OfficialAccount::Safety);
        assert_eq!(sender_of("appeal.upheld"), OfficialAccount::Safety);
        assert_eq!(sender_of("report.answer"), OfficialAccount::Safety);
        assert_eq!(sender_of("bot.disabled"), OfficialAccount::Safety);
        assert_eq!(sender_of("badge.granted"), OfficialAccount::Zrzur);
        assert_eq!(sender_of("feather.granted"), OfficialAccount::Zrzur);
    }

    #[test]
    fn the_persons_language_is_used() {
        assert_eq!(language_of(Some("de-DE")), "de");
        assert_eq!(language_of(Some("ar")), "ar");
        // not translated yet: English
        assert_eq!(language_of(Some("fr")), "en");
        assert_eq!(language_of(None), "en");
        let message = render("ar", "feather.granted", &[("days", "7".to_string())]);
        assert!(message.contains("7 يوماً"), "{message}");
        assert!(message.contains("منحك فريق زرزور"));
    }
}

#[cfg(test)]
mod statement_tests {
    use revolt_models::v0::{SafetyCategory, SafetyTarget};

    use super::*;

    fn action(ground: SafetyGround, expires_at: Option<i64>) -> SafetyAction {
        SafetyAction {
            id: "01HAAAAAAAAAAAAAAAAAAAAAAA".into(),
            case_id: None,
            actor_id: "reviewer".into(),
            kind: SafetyActionKind::SuspendAccount,
            target: SafetyTarget { kind: SafetyTargetKind::Message, id: "m".into(), facet: None, message_id: None },
            subject_user_id: Some("u".into()),
            category: SafetyCategory::HateOrThreats,
            public_reason: "Hate speech".into(),
            internal_note: Some("secret note".into()),
            created_at: 0,
            expires_at,
            revoked: None,
            appealable: true,
            automated_detection: true,
            automated_decision: false,
            ground,
            facts: "You posted a slur in #general".into(),
            territorial_scope: "Germany, Austria".into(),
            basis: SafetyActionBasis::Report,
        }
    }

    #[test]
    fn the_notice_has_every_part_in_every_language() {
        let law = SafetyGround::Law { law: "StGB".into(), provision: "section 130".into() };
        for (code, _) in LANGUAGES {
            let message = action_message(code, &action(law.clone(), Some(7 * DAY_MS)), "https://app.zrzur.com", None);
            for part in ["You posted a slur in #general", "StGB", "section 130", "Germany, Austria", "support@zrzur.com", "https://app.zrzur.com", "ZR-AAAAAAAA"] {
                assert!(message.contains(part), "{code}: missing {part}\n{message}");
            }
            assert!(!message.contains("secret note"), "{code} leaks the internal note");
            assert!(!message.contains('{'), "{code}: unfilled placeholder\n{message}");
            assert!(!message.contains("to be added"), "{code}: placeholder text");
        }
    }

    #[test]
    fn arabic_and_german_notices_are_in_their_language() {
        let terms = SafetyGround::Terms { rule: "Harassment".into() };
        let ar = action_message("ar", &action(terms.clone(), Some(7 * DAY_MS)), "", None);
        assert!(ar.contains("6 أشهر") && ar.contains("المادة 21") && ar.contains("7 أيام"), "{ar}");
        let de = action_message("de", &action(terms, None), "", None);
        assert!(de.contains("6 Monate") && de.contains("Artikel 21") && de.contains("ohne Enddatum"), "{de}");
    }

    #[test]
    fn arabic_day_numbers_use_the_right_form() {
        let one = days_text("ar", 1);
        let two = days_text("ar", 2);
        assert_eq!((one.as_str(), two.as_str()), ("يوم واحد.", "يومان."));
        assert_eq!(days_text("ar", 3), "3 أيام.");
        assert_eq!(days_text("ar", 10), "10 أيام.");
        assert_eq!(days_text("ar", 11), "11 يوماً.");
        assert_eq!(days_text("ar", 30), "30 يوماً.");
        assert_eq!(days_text("en", 1), "1 day.");
        assert_eq!(days_text("de", 30), "30 Tage.");
    }

    #[test]
    fn the_out_of_court_text_points_to_the_commission_list_and_names_no_body_it_does_not_have() {
        let terms = SafetyGround::Terms { rule: "Spam".into() };
        let none = statement_text("en", &action(terms.clone(), None), "", None);
        assert!(none.contains("European Commission publishes the list"), "{none}");
        assert!(!none.contains("also works with"), "{none}");
        let some = statement_text("en", &action(terms, None), "", Some("Example Settlement Body"));
        assert!(some.contains("also works with: Example Settlement Body"), "{some}");
    }

    #[test]
    fn rules_are_shown_by_translated_name_with_a_link_never_as_an_enum() {
        let terms = SafetyGround::Terms { rule: "rule:HateOrThreats".into() };
        for (code, expected) in [("en", "Hate or threats"), ("de", "Hass oder Drohungen"), ("ar", "الكراهية أو التهديد")] {
            let message = statement_text(code, &action(terms.clone(), None), "", None);
            assert!(message.contains(expected), "{code}: {message}");
            assert!(message.contains("zrzur.com/legal/community-guidelines"), "{code}: {message}");
            assert!(!message.contains("HateOrThreats") && !message.contains("rule:Hate"), "{code}: {message}");
        }
        // every category has a name in every language
        for category in [SafetyCategory::Other, SafetyCategory::IllegalContent, SafetyCategory::Underage] {
            for (code, _) in LANGUAGES {
                assert!(!rule_name(code, category).is_empty());
            }
        }
    }

    #[test]
    fn the_notice_says_what_the_decision_followed() {
        let terms = SafetyGround::Terms { rule: "Spam".into() };
        let mut a = action(terms, None);
        assert!(statement_text("en", &a, "", None).contains("followed a report from another person"));
        a.basis = SafetyActionBasis::Notice;
        assert!(statement_text("en", &a, "", None).contains("notice of illegal content"));
        a.basis = SafetyActionBasis::OwnInitiative;
        assert!(statement_text("de", &a, "", None).contains("eigenen Prüfung"));
    }

    #[test]
    fn terms_and_law_grounds_read_differently_and_automation_is_stated() {
        let terms = statement_text("en", &action(SafetyGround::Terms { rule: "Spam".into() }, None), "", None);
        assert!(terms.contains("breaks our rules (Spam)") && terms.contains("not a finding that it is illegal"), "{terms}");
        assert!(terms.contains("to find this: yes") && terms.contains("automated means: no"), "{terms}");
        let law = statement_text("en", &action(SafetyGround::Law { law: "StGB".into(), provision: "130".into() }, None), "", None);
        assert!(law.contains("illegal under StGB, 130"), "{law}");
    }

    #[test]
    fn an_action_saved_before_statements_still_reads_with_defaults() {
        let old = serde_json::json!({
            "_id": "a", "actor_id": "r", "kind": "Warning",
            "target": { "kind": "Message", "id": "m" },
            "category": "Spam", "public_reason": "Spam", "created_at": 1, "appealable": true
        });
        let action: SafetyAction = serde_json::from_value(old).unwrap();
        assert!(!action.automated_detection && !action.automated_decision);
        assert_eq!(action.territorial_scope, "worldwide");
        assert!(matches!(action.ground, SafetyGround::Terms { .. }));
        // with no facts on record the reason the person was shown is used
        let message = statement_text("en", &action, "", None);
        assert!(message.contains("What happened: Spam"), "{message}");
    }

    #[test]
    fn the_reporter_is_told_every_outcome_with_redress_and_automation() {
        let terms = SafetyGround::Terms { rule: "Spam".into() };
        let taken = action(terms, None);
        for (code, _) in LANGUAGES {
            for outcome in [
                ReportOutcome::Action(&taken),
                ReportOutcome::NoViolation,
                ReportOutcome::Duplicate,
                ReportOutcome::Closed,
            ] {
                let (subject, body) = reporter_notice(code, "ZR-ABCD1234", &outcome, Some("Checked twice"), true);
                assert!(subject.contains("ZR-ABCD1234"), "{code}: {subject}");
                for part in ["Checked twice", "support@zrzur.com", "ZR-ABCD1234"] {
                    assert!(body.contains(part), "{code}: missing {part}\n{body}");
                }
                assert!(!body.contains('{'), "{code}: unfilled placeholder\n{body}");
            }
        }
        // decisions without an action: "a person decided", and a way to challenge it
        let (_, body) = reporter_notice("en", "ZR-X", &ReportOutcome::NoViolation, None, true);
        assert!(body.contains("did not find a breach") && body.contains("A person made this decision") && body.contains("within 6 months"));
        // an action taken with automated detection says so
        let (_, body) = reporter_notice("en", "ZR-X", &ReportOutcome::Action(&taken), None, true);
        assert!(body.contains("to find this: yes"), "{body}");
        // an information request asks for a reply, it is not a decision
        let (_, body) = reporter_notice("de", "ZR-X", &ReportOutcome::Info("Welcher Kanal?"), None, true);
        assert!(body.contains("Welcher Kanal?") && body.contains("Aktenzeichen ZR-X") && body.contains("support@zrzur.com"));
        assert!(!body.contains("antworte auf diese Nachricht"), "no reply to a no-reply sender: {body}");
        // an account holder answers in the app
        let (_, app) = reporter_notice("en", "ZR-X", &ReportOutcome::Info("Which channel?"), None, false);
        assert!(app.contains("Settings → Feedback & reports") && !app.contains("write to"), "{app}");
        // an overturned decision and a closing note
        for (language, words) in [("en", "reversed our earlier"), ("ar", "ألغينا قرارنا"), ("de", "aufgehoben")] {
            let (_, revoked) = reporter_notice(language, "ZR-X", &ReportOutcome::Revoked, None, true);
            assert!(revoked.contains(words), "{revoked}");
        }
        let (_, over) = reporter_notice("en", "ZR-X", &ReportOutcome::Overturned, None, true);
        assert!(over.contains("reversed our earlier decision") && over.contains("within 6 months"), "{over}");
        let (_, note) = reporter_notice("en", "ZR-X", &ReportOutcome::Note, Some("Thank you for waiting"), true);
        assert!(note.contains("Thank you for waiting") && !note.contains("closed your report"), "{note}");
    }
}
