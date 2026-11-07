//! Bot verification: an owner asks for the verified mark, Zrzur staff
//! review it in the admin console. Asking never changes the bot; only an
//! approval sets `BotFlags::Verified`.

use revolt_database::{
    util::reference::Reference, BotVerification, BotVerificationStatus, Database, PartialBot, User,
    BOT_VERIFICATION_RETRY_MS, BOT_VERIFICATION_STATEMENT_MAX,
};
use revolt_models::v0::{self, BotFlags, GatewayIntents};
use revolt_result::{create_error, failed_validation, Result, ValidationProblem};
use rocket::{serde::json::Json, State};
use ulid::Ulid;
use validator::Validate;

pub(crate) const VERIFIED: i32 = BotFlags::Verified as i32;
const SUSPENDED: i32 = BotFlags::Suspended as i32;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub(crate) fn status_name(status: BotVerificationStatus) -> &'static str {
    match status {
        BotVerificationStatus::Pending => "Pending",
        BotVerificationStatus::Approved => "Approved",
        BotVerificationStatus::Denied => "Denied",
        BotVerificationStatus::Revoked => "Revoked",
    }
}

/// The owner's view: no reviewer identity
pub(crate) fn owner_view(request: BotVerification) -> v0::BotVerificationRequest {
    v0::BotVerificationRequest {
        id: request.id,
        bot_id: request.bot_id,
        statement: request.statement,
        website: request.website,
        support: request.support,
        privacy_policy_url: request.privacy_policy_url,
        terms_of_service_url: request.terms_of_service_url,
        intents: request.intents,
        status: status_name(request.status).to_string(),
        created_at: request.created_at,
        decided_at: request.decided_at,
        decision_note: request.decision_note,
    }
}

/// Plain text, trimmed, no control characters
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .to_string()
}

fn optional_link(value: Option<&str>, field: &str) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if !value.starts_with("https://") || value.contains(char::is_whitespace) {
        return Err(failed_validation(vec![ValidationProblem::new(field, "https")]));
    }
    Ok(Some(value.to_string()))
}

/// A public https link: no other scheme, a real host, no password in it
fn required_link(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    let valid = reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
    });
    if !valid || value.contains(char::is_whitespace) {
        return Err(failed_validation(vec![ValidationProblem::new(field, "https")]));
    }
    Ok(value.to_string())
}

async fn state(db: &Database, bot_id: &str, verified: bool) -> Result<v0::BotVerificationState> {
    let latest = db.fetch_bot_verifications_for_bot(bot_id).await?.into_iter().next();
    let can_apply_at = latest.as_ref().and_then(|r| match r.status {
        BotVerificationStatus::Denied | BotVerificationStatus::Revoked => r
            .decided_at
            .map(|at| at + BOT_VERIFICATION_RETRY_MS)
            .filter(|at| *at > now_ms()),
        _ => None,
    });
    Ok(v0::BotVerificationState {
        verified,
        request: latest.map(owner_view),
        can_apply_at,
    })
}

/// # Bot verification state
///
/// Whether the bot is verified and the latest request. Owner only.
#[openapi(tag = "Bots")]
#[get("/<target>/verification")]
pub async fn verification_state(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::BotVerificationState>> {
    let bot = target.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    let verified = bot.flags.unwrap_or_default() & VERIFIED != 0;
    state(db, &bot.id, verified).await.map(Json)
}

/// # Apply for verification
///
/// Ask Zrzur to verify your bot, and to approve the privileged intents it
/// needs past 100 servers. Links to its privacy policy and terms are
/// required, https only, and also go on the bot. One open request at a time;
/// after a denial you can apply again a week later.
#[openapi(tag = "Bots")]
#[post("/<target>/verification", data = "<data>")]
pub async fn apply_verification(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataBotVerification>,
) -> Result<Json<v0::BotVerificationState>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    let intents = data.intents.unwrap_or_default();
    if intents < 0 || intents as u64 & !GatewayIntents::PRIVILEGED != 0 {
        return Err(create_error!(InvalidOperation));
    }
    let mut bot = target.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    let flags = bot.flags.unwrap_or_default();
    if flags & VERIFIED != 0 {
        return Err(create_error!(NoEffect));
    }
    if flags & SUSPENDED != 0 {
        return Err(create_error!(InvalidOperation));
    }
    let current = state(db, &bot.id, false).await?;
    if current.request.as_ref().is_some_and(|r| r.status == "Pending") || current.can_apply_at.is_some() {
        return Err(create_error!(InvalidOperation));
    }
    let statement = clean(&data.statement);
    let length = statement.chars().count();
    if !(20..=BOT_VERIFICATION_STATEMENT_MAX).contains(&length) {
        return Err(failed_validation(vec![ValidationProblem::length(
            "statement",
            20,
            BOT_VERIFICATION_STATEMENT_MAX,
        )]));
    }
    let privacy_policy_url = required_link(&data.privacy_policy_url, "privacy_policy_url")?;
    let terms_of_service_url = required_link(&data.terms_of_service_url, "terms_of_service_url")?;
    let request = BotVerification {
        id: Ulid::new().to_string(),
        bot_id: bot.id.clone(),
        owner_id: user.id.clone(),
        statement,
        website: optional_link(data.website.as_deref(), "website")?,
        support: optional_link(data.support.as_deref(), "support")?,
        privacy_policy_url: Some(privacy_policy_url.clone()),
        terms_of_service_url: Some(terms_of_service_url.clone()),
        intents,
        status: BotVerificationStatus::Pending,
        created_at: now_ms(),
        decided_at: None,
        decided_by: None,
        decision_note: None,
    };
    db.insert_bot_verification(&request).await?;
    bot.update(
        db,
        PartialBot {
            privacy_policy_url: Some(privacy_policy_url),
            terms_of_service_url: Some(terms_of_service_url),
            ..Default::default()
        },
        vec![],
    )
    .await?;
    state(db, &bot.id, false).await.map(Json)
}
