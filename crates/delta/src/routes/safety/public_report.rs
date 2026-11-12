//! `POST /safety/public-reports`: report illegal content without an
//! account (DSA Art. 16(1)-(2)). No session is needed, so it is guarded by a
//! per-IP limit, a total cap, size limits and, when sign-up uses it, Cloudflare Turnstile.
//! The report goes into the same case queue as in-app reports, marked as
//! public (`reporter_id == "public"`).
//!
//! Nothing is read from the reported content here: an anonymous caller must
//! not be able to make us copy private messages. Reviewers open the content
//! themselves; the link or id the reporter gave is kept on the case.

use std::{collections::HashMap, sync::Mutex};

use revolt_database::{
    safety_now,
    util::{captcha::check_captcha, email::validate_email, ip::rocket::to_real_ip},
    Database, File, FileUsedForType,
};
use revolt_models::v0::{
    safety_is_csam, safety_priority, safety_public_categories, safety_reference,
    safety_subcategories, DataCreatePublicReport, SafetyCase, SafetyPriority, SafetyPublicReporter, SafetyReceipt,
    SafetyRoute, SafetyStatus, SafetyTarget, SafetyTargetKind, PUBLIC_REPORTER_ID,
    SAFETY_POLICY_VERSION,
};
use revolt_result::{create_error, Result};
use rocket::{
    request::{FromRequest, Outcome},
    serde::json::Json,
    Request, State,
};
use revolt_config::{config, CorsOrigins};
use ulid::Ulid;

use super::{clean_text, log, reports::receipt, targets, validate, SYSTEM_ACTOR};

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
/// Each report one address may send can take this many tries at the captcha
const TRIES_PER_REPORT: usize = 4;

/// Sliding window of request times per key
#[derive(Default)]
pub(crate) struct WindowLimiter {
    hits: HashMap<String, Vec<i64>>,
}

impl WindowLimiter {
    /// Whether `key` is under `max` in the last `window` ms (counts nothing)
    pub(crate) fn has_room(&mut self, key: &str, max: usize, window: i64, now: i64) -> bool {
        // forget idle keys so the map cannot grow without bound
        if self.hits.len() > 10_000 {
            self.hits.retain(|_, times| times.last().is_some_and(|t| now - t < window));
        }
        let times = self.hits.entry(key.to_string()).or_default();
        times.retain(|t| now - t < window);
        times.len() < max
    }

    pub(crate) fn record(&mut self, key: &str, now: i64) {
        self.hits.entry(key.to_string()).or_default().push(now);
    }
}

/// Limits of the public report route and of its e-mails, kept as Rocket
/// state (one per server, so tests and instances never share counters).
/// Set with `ZRZUR_PUBLIC_REPORTS_PER_IP_HOUR` (5), `ZRZUR_PUBLIC_REPORTS_PER_HOUR`
/// (300) and `ZRZUR_PUBLIC_REPORT_MAILS_PER_HOUR` (60). Counters live in
/// memory per API instance; behind several instances use one proxy-level limit too.
pub struct PublicReportGuard {
    per_ip: usize,
    global: usize,
    mails_per_hour: usize,
    reports: Mutex<WindowLimiter>,
    tries: Mutex<WindowLimiter>,
    mails: Mutex<WindowLimiter>,
}

fn env_limit(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

impl PublicReportGuard {
    pub fn new(per_ip: usize, global: usize, mails_per_hour: usize) -> Self {
        Self {
            per_ip,
            global,
            mails_per_hour,
            reports: Mutex::default(),
            tries: Mutex::default(),
            mails: Mutex::default(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            env_limit("ZRZUR_PUBLIC_REPORTS_PER_IP_HOUR", 5),
            env_limit("ZRZUR_PUBLIC_REPORTS_PER_HOUR", 300),
            env_limit("ZRZUR_PUBLIC_REPORT_MAILS_PER_HOUR", 60),
        )
    }

    pub fn per_ip(&self) -> usize {
        self.per_ip
    }

    /// May this address try now? Asked before the captcha, which costs a call
    /// to Cloudflare Turnstile: an address over its limit is stopped here, and failed tries
    /// count too, so wrong tokens cannot keep us calling out.
    pub(crate) fn may_try(&self, ip: &str, now: i64) -> bool {
        let key = if ip.is_empty() { "unknown" } else { ip };
        let room = self
            .reports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_room(key, self.per_ip, HOUR_MS, now);
        let mut tries = self.tries.lock().unwrap_or_else(|e| e.into_inner());
        if !room || !tries.has_room(key, self.per_ip.saturating_mul(TRIES_PER_REPORT), HOUR_MS, now) {
            return false;
        }
        tries.record(key, now);
        true
    }

    /// Admit one report: the address is checked first, the platform-wide cap
    /// only for requests that pass it, so one client cannot use it up for everyone
    pub(crate) fn admit(&self, ip: &str, now: i64) -> bool {
        let mut limiter = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        // an unknown address shares one bucket: stricter, never open
        let key = if ip.is_empty() { "unknown" } else { ip };
        if !limiter.has_room(key, self.per_ip, HOUR_MS, now)
            || !limiter.has_room("*", self.global, HOUR_MS, now)
        {
            return false;
        }
        limiter.record(key, now);
        limiter.record("*", now);
        true
    }

    /// Whether a receipt may go to this address now: once a day per
    /// recipient and a platform-wide cap per hour
    pub(crate) fn may_mail(&self, email: &str, now: i64) -> bool {
        let mut limiter = self.mails.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("to:{}", email.to_lowercase());
        if !limiter.has_room(&key, 1, DAY_MS, now) || !limiter.has_room("*", self.mails_per_hour, HOUR_MS, now) {
            return false;
        }
        limiter.record(&key, now);
        limiter.record("*", now);
        true
    }
}

/// The caller's address (used for the limit only, never stored)
pub struct ClientAddress(pub String);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ClientAddress {
    type Error = revolt_result::Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        // one /64 is one address for the limit (an IPv6 host can pick any address in it)
        Outcome::Success(ClientAddress(revolt_ratelimits::ratelimiter::address_group(
            &to_real_ip(request).await,
        )))
    }
}

impl<'r> revolt_rocket_okapi::request::OpenApiFromRequest<'r> for ClientAddress {
    fn from_request_input(
        _gen: &mut revolt_rocket_okapi::r#gen::OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<revolt_rocket_okapi::request::RequestHeaderInput> {
        Ok(revolt_rocket_okapi::request::RequestHeaderInput::None)
    }
}

fn is_id(token: &str) -> bool {
    token.len() == 26 && token.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Files are named by a 42-character id (letters, digits, `_` and `-`)
fn is_file_id(token: &str) -> bool {
    token.len() == 42 && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The object id inside a link (`.../server/<id>/channel/<id>/<message>`)
/// or a bare id. A message is the last id in the link, a server the one
/// after `server`, a file its 42-character id, anything else the first.
pub(crate) fn id_from_location(kind: SafetyTargetKind, location: &str) -> Option<String> {
    if kind == SafetyTargetKind::Attachment {
        return location
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .find(|t| is_file_id(t))
            .map(str::to_string);
    }
    let tokens: Vec<&str> = location.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let ids: Vec<&str> = tokens.iter().copied().filter(|t| is_id(t)).collect();
    let pick = match kind {
        SafetyTargetKind::Message => ids.last().copied(),
        SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => tokens
            .windows(2)
            .find(|w| w[0].eq_ignore_ascii_case("server") && is_id(w[1]))
            .map(|w| w[1])
            .or(ids.first().copied()),
        _ => ids.first().copied(),
    };
    pick.map(str::to_uppercase)
}

/// The custom link name in a location: a bare name, or the last path segment
/// of a link on one of our own sites. Another site's page names nothing here.
fn link_name<'a>(path: &'a str, origins: &CorsOrigins) -> Option<&'a str> {
    let path = path.trim().trim_end_matches('/');
    let (scheme, rest) = path.split_once("://").unwrap_or(("https", path));
    let name = match rest.split_once('/') {
        Some((host, _)) => {
            let ours = host == "zrzur.cc" || origins.allows(&format!("{scheme}://{host}"));
            ours.then(|| rest.rsplit('/').next()).flatten()?
        }
        None if rest.len() == path.len() => rest,
        None => return None,
    };
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then_some(name)
}

/// The server a report names: an id that exists, else a custom link. A link
/// name can look like an id, and an id in the query names nothing.
async fn server_target(db: &Database, location: &str) -> Option<String> {
    let path = location.split(['?', '#']).next()?;
    if let Some(id) = id_from_location(SafetyTargetKind::Server, path) {
        if db.fetch_server(&id).await.is_ok() {
            return Some(id);
        }
    }
    let config = config().await;
    let origins = CorsOrigins::from_settings(&config.hosts.app, &config.api.security.staff_origins);
    let name = link_name(path, &origins)?;
    crate::routes::discover::list_servers::server_by_id_or_link(db, name)
        .await
        .ok()
        .map(|server| server.id)
}

/// The message a file was attached to. `used_for` is what uploads write;
/// `message_id` only exists on old files.
fn file_message(file: &File) -> Option<String> {
    file.used_for
        .as_ref()
        .filter(|used| matches!(used.object_type, FileUsedForType::Message))
        .map(|used| used.id.clone())
        .or_else(|| file.message_id.clone())
}

/// Who is responsible for the target, and for a file the message it belongs
/// to, as far as the database knows. Nothing is copied here.
pub(crate) async fn resolve_owner(
    db: &Database,
    kind: SafetyTargetKind,
    id: &str,
) -> (Option<String>, Option<String>) {
    match kind {
        SafetyTargetKind::Message => match db.fetch_message(id).await {
            Ok(message) => (targets::message_owner(db, &message).await, None),
            Err(_) => (None, None),
        },
        SafetyTargetKind::Webhook => {
            (db.fetch_webhook(id).await.ok().map(|w| w.creator_id), None)
        }
        SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => {
            (db.fetch_server(id).await.ok().map(|s| s.owner), None)
        }
        SafetyTargetKind::User => (db.fetch_user(id).await.ok().map(|u| u.id), None),
        SafetyTargetKind::Bot | SafetyTargetKind::Application | SafetyTargetKind::DiscoverBot => {
            (db.fetch_bot(id).await.ok().map(|b| b.owner), None)
        }
        SafetyTargetKind::CommunityItem => {
            (db.fetch_community_item(id).await.ok().map(|i| i.creator_id), None)
        }
        SafetyTargetKind::Attachment => {
            let Ok(file) = db.fetch_attachment("attachments", id).await else {
                return (None, None);
            };
            // not every file `use_attachment` claims hangs on a message (bug
            // reports and interaction options name their own ids), and a
            // message can be gone: then the uploader answers for the file
            match file_message(&file) {
                Some(message_id) => match db.fetch_message(&message_id).await {
                    Ok(message) => (targets::message_owner(db, &message).await, Some(message.id)),
                    Err(_) => (file.uploader_id, None),
                },
                None => (file.uploader_id, None),
            }
        }
        _ => (None, None),
    }
}

/// What the captcha settings ask of a report, before Turnstile is called. With
/// a site key the form shows the check, so a token is needed; a site key without
/// a secret cannot verify one, so no token is good enough.
pub(crate) fn captcha_settings_allow(site_key: &str, secret: &str, token: Option<&str>) -> bool {
    site_key.is_empty() || (!secret.is_empty() && token.is_some_and(|token| !token.is_empty()))
}

/// Check everything about the request except the captcha and the limit
pub(crate) fn check_public_report(data: &DataCreatePublicReport) -> Result<()> {
    validate(data)?;
    let bad = |error: &str| create_error!(FailedValidation { error: error.to_string() });
    if !data.good_faith {
        return Err(bad("good_faith must be confirmed"));
    }
    if !safety_public_categories().contains(&data.category) {
        return Err(bad("category is not offered here"));
    }
    let sub = data.subcategory.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(sub) = sub
        && !safety_subcategories(data.category).contains(&sub)
    {
        return Err(bad("unknown subcategory"));
    }
    if clean_text(&data.location).is_empty() || clean_text(&data.description).chars().count() < 10 {
        return Err(bad("location and description are required"));
    }
    // name and e-mail are optional only for child sexual abuse material
    if !safety_is_csam(data.category, sub) {
        let name = data.name.as_deref().map(clean_text).unwrap_or_default();
        let email = data.email.as_deref().map(str::trim).unwrap_or_default();
        if name.is_empty() || email.is_empty() {
            return Err(bad("name and email are required"));
        }
    }
    if let Some(email) = data.email.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        validate_email(email)?;
    }
    Ok(())
}

/// # Report without an account
///
/// Report illegal content to Zrzur without signing in (DSA Art. 16). Name
/// and e-mail are required unless the report is about child sexual abuse
/// material. If an e-mail address is given, a receipt is sent to it in the
/// page language.
#[openapi(tag = "User Safety")]
#[post("/public-reports", data = "<data>")]
pub async fn create_public(
    db: &State<Database>,
    guard: &State<PublicReportGuard>,
    ip: ClientAddress,
    data: Json<DataCreatePublicReport>,
) -> Result<Json<SafetyReceipt>> {
    let data = data.into_inner();
    check_public_report(&data)?;
    let now = safety_now();
    if !guard.may_try(&ip.0, now) {
        return Err(create_error!(TooManyReports { max: guard.per_ip() }));
    }
    let captcha = config().await.api.security.captcha;
    if !captcha_settings_allow(&captcha.turnstile_sitekey, &captcha.turnstile_secret, data.captcha.as_deref()) {
        return Err(create_error!(CaptchaFailed));
    }
    check_captcha(data.captcha.as_deref()).await?;

    if !guard.admit(&ip.0, now) {
        return Err(create_error!(TooManyReports { max: guard.per_ip() }));
    }

    let sub = data
        .subcategory
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let location = clean_text(&data.location);
    let id = Ulid::new().to_string();
    // a link we cannot read gets its own id, so unrelated reports never group together
    let found = match data.target_kind {
        SafetyTargetKind::Server | SafetyTargetKind::DiscoverServer => {
            server_target(db, &location).await
        }
        kind => id_from_location(kind, &location),
    };
    let target_id = found.clone().unwrap_or_else(|| format!("unresolved:{id}"));
    let (owner, message_id) = match &found {
        Some(found) => resolve_owner(db, data.target_kind, found).await,
        None => (None, None),
    };

    let email = data
        .email
        .as_deref()
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty());
    let name = data.name.as_deref().map(clean_text).filter(|n| !n.is_empty());
    let language = match data
        .language
        .as_deref()
        .unwrap_or("en")
        .to_lowercase()
        .split(['-', '_'])
        .next()
    {
        Some(l @ ("ar" | "de")) => l.to_string(),
        _ => "en".to_string(),
    };
    let law = data.law.as_deref().map(clean_text).filter(|l| !l.is_empty());

    let mut description = clean_text(&data.description);
    if let Some(law) = &law {
        description = format!("{description}\n\nLaw named by the reporter: {law}");
    }

    let mut priority = safety_priority(data.category, sub.as_deref());
    // a notice of illegal content is never "low", whatever the box ticked
    if priority < SafetyPriority::Normal {
        priority = SafetyPriority::Normal;
    }
    let case = SafetyCase {
        reference: safety_reference(&id),
        id,
        reporter_id: PUBLIC_REPORTER_ID.to_string(),
        public_reporter: Some(SafetyPublicReporter {
            name,
            email: email.clone(),
            location: location.clone(),
            law,
            language: language.clone(),
            good_faith: true,
        }),
        target: SafetyTarget {
            kind: data.target_kind,
            id: target_id,
            facet: None,
            message_id,
        },
        target_owner_id: owner,
        server_id: None,
        channel_id: None,
        channel_type: None,
        route: SafetyRoute::Platform,
        category: data.category,
        subcategory: sub.clone(),
        description,
        evidence_ids: vec![],
        status: SafetyStatus::Queued,
        priority,
        assignee_id: None,
        resolution: None,
        duplicate_of: None,
        info_request: None,
        policy_version: SAFETY_POLICY_VERSION.to_string(),
        created_at: now,
        updated_at: now,
        closed_at: None,
        version: 0,
        legal_hold: false,
    };
    db.insert_safety_case(&case).await?;
    log(
        db,
        &case.id,
        SYSTEM_ACTOR,
        "Created",
        None,
        Some("Queued".into()),
        Some("Public report".into()),
    )
    .await?;

    // the receipt carries the reference only, never what the caller wrote, and
    // is limited per recipient: the address could belong to someone else
    if let Some(email) = email
        && guard.may_mail(&email, now)
    {
        let reference = case.reference.clone();
        tokio::spawn(async move {
            crate::util::official::public_report_receipt(&email, &language, &reference).await;
        });
    }
    Ok(Json(receipt(&case, false)))
}

#[cfg(test)]
mod tests {
    use revolt_models::v0::SafetyCategory;

    use super::*;

    fn good() -> DataCreatePublicReport {
        DataCreatePublicReport {
            target_kind: SafetyTargetKind::Message,
            location: "https://app.zrzur.com/channel/01HZZZZZZZZZZZZZZZZZZZZZZZ/01HYYYYYYYYYYYYYYYYYYYYYYY".into(),
            description: "This message offers stolen goods".into(),
            category: SafetyCategory::IllegalContent,
            subcategory: None,
            law: Some("StGB 259".into()),
            name: Some("Sam".into()),
            email: Some("sam@example.org".into()),
            good_faith: true,
            language: Some("de".into()),
            captcha: None,
        }
    }

    #[test]
    fn a_complete_report_passes() {
        check_public_report(&good()).unwrap();
    }

    #[test]
    fn good_faith_name_and_email_are_required() {
        let mut data = good();
        data.good_faith = false;
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.name = None;
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.email = Some("  ".into());
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.email = Some("not an address".into());
        assert!(check_public_report(&data).is_err());
    }

    #[test]
    fn child_abuse_material_needs_no_name_or_email() {
        let mut data = good();
        data.category = SafetyCategory::SexualContent;
        data.subcategory = Some("minor_safety".into());
        data.name = None;
        data.email = None;
        check_public_report(&data).unwrap();
        // other sexual content still needs them
        data.subcategory = Some("unsolicited".into());
        assert!(check_public_report(&data).is_err());
    }

    #[test]
    fn sizes_and_categories_are_limited() {
        let mut data = good();
        data.description = "x".repeat(2001);
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.description = "short".into();
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.location = "y".repeat(501);
        assert!(check_public_report(&data).is_err());
        let mut data = good();
        data.category = SafetyCategory::Spam;
        assert!(check_public_report(&data).is_err());
    }

    #[test]
    fn ids_are_found_in_links() {
        let link = "https://app.zrzur.com/server/01HAAAAAAAAAAAAAAAAAAAAAAA/channel/01HBBBBBBBBBBBBBBBBBBBBBBB/01HCCCCCCCCCCCCCCCCCCCCCCC";
        assert_eq!(
            id_from_location(SafetyTargetKind::Message, link).unwrap(),
            "01HCCCCCCCCCCCCCCCCCCCCCCC"
        );
        assert_eq!(
            id_from_location(SafetyTargetKind::Server, link).unwrap(),
            "01HAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            id_from_location(SafetyTargetKind::User, "01HDDDDDDDDDDDDDDDDDDDDDDD").unwrap(),
            "01HDDDDDDDDDDDDDDDDDDDDDDD"
        );
        assert!(id_from_location(SafetyTargetKind::User, "@somebody").is_none());
        let id = "Zx9_Zx9-Zx9_Zx9-Zx9_Zx9-Zx9_Zx9-Zx9_Zx9-Zx";
        assert_eq!(id.len(), 42);
        assert_eq!(
            id_from_location(SafetyTargetKind::Attachment, &format!("https://cdn.zrzur.com/attachments/{id}/cat.png")).as_deref(),
            Some(id)
        );
        assert!(id_from_location(SafetyTargetKind::Attachment, "https://cdn.zrzur.com/attachments/short/cat.png").is_none());
    }

    #[test]
    fn the_rate_limit_is_per_address_and_per_hour() {
        let guard = PublicReportGuard::new(5, 300, 60);
        for i in 0..5 {
            assert!(guard.admit("1.2.3.4", i));
        }
        assert!(!guard.admit("1.2.3.4", 10));
        // another address is not affected
        assert!(guard.admit("5.6.7.8", 10));
        // an hour later it opens again
        assert!(guard.admit("1.2.3.4", HOUR_MS + 100));
    }

    #[test]
    fn one_client_cannot_use_up_the_platform_cap() {
        let guard = PublicReportGuard::new(2, 3, 60);
        // the flood from one address is stopped by its own limit...
        for _ in 0..50 {
            guard.admit("9.9.9.9", 1);
        }
        // ...so the platform cap still has room for others (2 used so far)
        assert!(guard.admit("a", 2));
        assert!(!guard.admit("b", 2), "the cap counts passing requests only: 2 + 1");
    }

    #[test]
    fn an_address_over_its_limit_never_reaches_the_captcha() {
        let guard = PublicReportGuard::new(2, 300, 60);
        assert!(guard.may_try("1.2.3.4", 1));
        assert!(guard.admit("1.2.3.4", 1));
        assert!(guard.admit("1.2.3.4", 2));
        assert!(!guard.may_try("1.2.3.4", 3), "its two reports are used up");
        assert!(guard.may_try("5.6.7.8", 3), "another address is not affected");
    }

    #[test]
    fn wrong_tokens_cannot_keep_calling_the_captcha_service() {
        let guard = PublicReportGuard::new(2, 300, 60);
        // two reports allow eight tries, whether or not any report gets through
        for i in 0..8 {
            assert!(guard.may_try("1.2.3.4", i));
        }
        assert!(!guard.may_try("1.2.3.4", 9));
        assert!(guard.may_try("1.2.3.4", HOUR_MS + 100), "an hour later it opens again");
    }

    #[test]
    fn a_site_key_without_a_secret_accepts_no_token() {
        assert!(!captcha_settings_allow("site", "", Some("any token")));
        assert!(!captcha_settings_allow("site", "", None));
        assert!(!captcha_settings_allow("site", "secret", None));
        assert!(!captcha_settings_allow("site", "secret", Some("")));
        assert!(captcha_settings_allow("site", "secret", Some("token")));
        // no check configured
        assert!(captcha_settings_allow("", "", None));
    }

    #[test]
    fn a_receipt_goes_to_an_address_once_a_day() {
        let guard = PublicReportGuard::new(5, 300, 1);
        assert!(guard.may_mail("Someone@Example.org", 0));
        assert!(!guard.may_mail("someone@example.org", 1000));
        assert!(guard.may_mail("someone@example.org", DAY_MS + 1));
        // the hourly cap for all receipts
        assert!(!guard.may_mail("d@example.org", DAY_MS + 3));
    }
}
