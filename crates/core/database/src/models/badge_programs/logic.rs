//! The application lifecycle (owner-approved audit C.2):
//!
//! `Draft → Submitted → InReview → Approved | Refused | MoreInfo`, plus
//! `Withdrawn` by the applicant and `Ended` when an approved mark or badge
//! ends. `MoreInfo → Submitted` starts the review again. A refusal can be
//! appealed once within `appeal_days`; an upheld appeal sends the
//! application back to the queue. After a refusal the same target waits
//! `reapply_days`.
//!
//! Every step here is server-side, written to the platform audit log (ids,
//! program, status and reason codes; never the statement, evidence or an
//! ownership code) and, for reviewers, to `badge_reviews`. Approvals that
//! give a badge go through the badge gate (`mutate_badge`); delta does the
//! marks. Nobody reviews an application they made, or one for a server or
//! bot they own.

use revolt_config::ApiBadgePrograms;
use revolt_models::v0::BotFlags;
use revolt_result::{create_error, ErrorType, Result};
use ulid::Ulid;

use crate::{
    application_checklist, mutate_badge, record_platform_audit, sha256_hex, AppealStatus,
    ApplicationAppeal, ApplicationStatus, AuditOutcome, AuditRecord, BadgeActor, BadgeApplication,
    BadgeChange, BadgeEvidence, BadgeMutation, BadgeProgram, BadgeReview, CreatorChannel, CreatorFigures,
    Database, OwnershipChallenge, OwnershipMethod, ProgramEligibility, StaffPermission, User, CREATOR_QUALITY,
    DAY_MS,
};


/// Statement length (what the server or bot is, and why it fits)
pub const STATEMENT_MIN: usize = 20;
pub const STATEMENT_MAX: usize = 2000;
/// Links an applicant may name
pub const LINKS_MAX: usize = 5;
pub const LINK_MAX: usize = 300;
/// Evidence items per application, and the length of a note
pub const EVIDENCE_MAX: usize = 10;
pub const NOTE_MAX: usize = 1000;
/// Open applications one person may have at once
pub const OPEN_MAX: usize = 3;
/// How long an ownership code is accepted
pub const CHALLENGE_TTL_MS: i64 = 7 * DAY_MS;
/// After a recheck warning: days to improve
pub const IMPROVE_DAYS: i64 = 30;
/// A withdrawn application stays this long (it was sent; the record shows it)
pub const WITHDRAWN_KEEP_DAYS: i64 = 30;
/// Reason codes a reviewer may give at once
pub const REASONS_MAX: usize = 5;
/// Content Creator: links to works at most, and the length of a schedule
pub const CREATOR_WORKS_MAX: usize = 10;
pub const SCHEDULE_MAX: usize = 500;
/// Content Creator: platforms named in plain text (never a logo or brand)
pub const CREATOR_PLATFORMS: [&str; 8] = ["youtube", "tiktok", "twitch", "instagram", "x", "kick", "facebook", "other"];
/// Content Creator: kinds of content (a genre is never a reason to refuse)
pub const CREATOR_CATEGORIES: [&str; 11] = [
    "education",
    "technology",
    "gaming",
    "livestream",
    "tutorials",
    "creative",
    "comedy",
    "art",
    "film",
    "commentary",
    "other",
];
/// The TXT record name and the file path of the ownership challenge
pub const DNS_RECORD_PREFIX: &str = "_zrzur-verification";
pub const WEBSITE_PATH: &str = "/.well-known/zrzur-verification.txt";

/// Reason codes a reviewer may give, per program. The apps and the
/// official message show them in the reader's language.
pub fn reason_codes(program: BadgeProgram) -> &'static [&'static str] {
    match program {
        BadgeProgram::Partner => &[
            "criteria_met",
            "more_information",
            "purpose_unclear",
            "rules_missing",
            "moderation",
            "security_measures",
            "safety_violations",
            "fake_engagement",
            "metrics_not_met",
            "other",
        ],
        BadgeProgram::VerifiedServer => &[
            "criteria_met",
            "more_information",
            "ownership_not_proven",
            "not_official",
            "branding",
            "moderation",
            "safety_violations",
            "fake_engagement",
            "metrics_not_met",
            "other",
        ],
        BadgeProgram::VerifiedBot => &[
            "criteria_met",
            "more_information",
            "description_inaccurate",
            "no_working_feature",
            "privacy_policy",
            "permissions",
            "malicious",
            "inflated_installs",
            "safety_violations",
            "other",
        ],
        // the published criteria, one code each (owner's brief, section 6);
        // a genre is never one of them
        BadgeProgram::ContentCreator => &[
            "criteria_met",
            "more_information",
            "ownership_not_proven",
            "followers_not_met",
            "activity_not_met",
            "works_not_met",
            "originality",
            "effort",
            "consistency",
            "authenticity",
            "safety",
            "honesty",
            "rights",
            "other",
        ],
    }
}

fn invalid(code: &str) -> revolt_result::Error {
    create_error!(FailedValidation { error: code.to_string() })
}

/// Plain text: no control characters but new lines, trimmed
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .to_string()
}

fn bounded(text: &str, min: usize, max: usize, field: &str) -> Result<String> {
    let text = clean_text(text);
    let length = text.chars().count();
    if !(min..=max).contains(&length) {
        return Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem::length(
            field, min, max,
        )]));
    }
    Ok(text)
}

/// A public https link: a real host, no password, no spaces
pub fn clean_link(value: &str) -> Result<String> {
    let value = value.trim();
    let valid = value.chars().count() <= LINK_MAX
        && !value.contains(char::is_whitespace)
        && reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| host.contains('.'))
                && url.username().is_empty()
                && url.password().is_none()
        });
    if !valid {
        return Err(invalid("https_link"));
    }
    Ok(value.to_string())
}

fn clean_links(links: &[String]) -> Result<Vec<String>> {
    if links.len() > LINKS_MAX {
        return Err(invalid("too_many_links"));
    }
    links.iter().filter(|l| !l.trim().is_empty()).map(|l| clean_link(l)).collect()
}

/// A domain name for an ownership challenge: lower case, letters, digits
/// and hyphens in dot-separated labels, at least two labels, no IP address,
/// no scheme, port or path
pub fn clean_domain(value: &str) -> Option<String> {
    let domain = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').collect();
    let valid = domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        // the last label is never all digits (no IPv4 address)
        && !labels.last().unwrap_or(&"").chars().all(|c| c.is_ascii_digit());
    valid.then_some(domain)
}

/// Content Creator's channel as the applicant named it: a known platform
/// (with its name for `other`), a public https address, a kind of content,
/// at most ten https links to works and a short schedule
pub fn clean_creator(channel: &CreatorChannel) -> Result<CreatorChannel> {
    let platform = channel.platform.trim().to_ascii_lowercase();
    if !CREATOR_PLATFORMS.contains(&platform.as_str()) {
        return Err(invalid("platform"));
    }
    let platform_name = match channel.platform_name.as_deref().map(clean_text).filter(|n| !n.is_empty()) {
        Some(name) if name.chars().count() > 40 => return Err(invalid("platform_name")),
        Some(name) if platform == "other" => Some(name),
        None if platform == "other" => return Err(invalid("platform_name")),
        _ => None,
    };
    let category = channel.category.trim().to_ascii_lowercase();
    if !CREATOR_CATEGORIES.contains(&category.as_str()) {
        return Err(invalid("category"));
    }
    let url = if channel.url.trim().is_empty() { String::new() } else { clean_link(&channel.url)? };
    if channel.works.len() > CREATOR_WORKS_MAX {
        return Err(invalid("too_many_works"));
    }
    let works = channel
        .works
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| clean_link(l))
        .collect::<Result<Vec<String>>>()?;
    let schedule = match channel.schedule.as_deref().map(clean_text).filter(|s| !s.is_empty()) {
        Some(text) if text.chars().count() > SCHEDULE_MAX => return Err(invalid("schedule")),
        other => other,
    };
    Ok(CreatorChannel {
        platform,
        platform_name,
        url,
        category,
        works,
        schedule,
    })
}

/// Whether `reviewer` must not review this application: they made it, or
/// they own the server or bot now
pub fn reviewer_conflict(application: &BadgeApplication, owner_id: Option<&str>, reviewer: &str) -> bool {
    application.applicant_id == reviewer || owner_id == Some(reviewer)
}

/// Write one step to the platform audit log: ids, program, status and
/// codes only
pub async fn application_audit(
    db: &Database,
    actor_id: &str,
    staff_session: Option<String>,
    action: &str,
    application: &BadgeApplication,
    outcome: AuditOutcome,
    detail: &str,
) -> Result<()> {
    record_platform_audit(
        db,
        AuditRecord {
            actor_id: actor_id.to_string(),
            action: action.to_string(),
            permission: staff_session.as_ref().map(|_| StaffPermission::BadgesManage),
            target_kind: Some("badge_application".to_string()),
            target_id: Some(application.id.clone()),
            reason: None,
            staff_session_id: staff_session,
            summary: Some(
                format!(
                    "{} {} {} {}",
                    application.program.name(),
                    application.target_id,
                    application.status.name(),
                    detail
                )
                .trim()
                .to_string(),
            ),
        },
        outcome,
    )
    .await
    .map(|_| ())
}

/// Save a changed application; refused if someone else changed it first
pub async fn save_application(db: &Database, application: &mut BadgeApplication, before: i64, now: i64) -> Result<()> {
    application.updated_at = now.max(before + 1);
    if !db.save_badge_application(application, before).await? {
        return Err(invalid("changed_meanwhile"));
    }
    Ok(())
}

/// Current owner of the server or bot an application is for (Content
/// Creator: the applicant themself, while the account exists)
pub async fn target_owner(db: &Database, program: BadgeProgram, target_id: &str) -> Result<Option<String>> {
    let owner = if program.targets_person() {
        db.fetch_user(target_id).await.map(|user| user.id)
    } else if program.targets_bot() {
        db.fetch_bot(target_id).await.map(|bot| bot.owner)
    } else {
        db.fetch_server(target_id).await.map(|server| server.owner)
    };
    match owner {
        Ok(owner) => Ok(Some(owner)),
        Err(error) if matches!(error.error_type, ErrorType::NotFound | ErrorType::UnknownServer | ErrorType::UnknownUser) => Ok(None),
        Err(error) => Err(error),
    }
}

/// What someone starts an application with
#[derive(Debug, Clone, Default)]
pub struct ApplicationInput {
    pub statement: Option<String>,
    pub links: Option<Vec<String>>,
    pub authorized: Option<bool>,
    /// Content Creator: the channel or profile
    pub creator: Option<CreatorChannel>,
}

/// What keeps someone from starting an application now, if anything:
/// `already_approved`, `open_application`, `reapply_wait` (with the date
/// it ends) or `too_many_open`
pub async fn application_blocker(
    db: &Database,
    applicant_id: &str,
    program: BadgeProgram,
    target_id: &str,
    now: i64,
) -> Result<Option<(&'static str, Option<i64>)>> {
    let earlier = db.fetch_badge_applications_for_target(target_id).await?;
    let earlier: Vec<&BadgeApplication> = earlier.iter().filter(|a| a.program == program).collect();
    if earlier.iter().any(|a| a.status == ApplicationStatus::Approved) {
        return Ok(Some(("already_approved", None)));
    }
    if earlier.iter().any(|a| a.status.is_open()) {
        return Ok(Some(("open_application", None)));
    }
    if let Some(until) = earlier
        .iter()
        .filter(|a| a.status == ApplicationStatus::Refused)
        .filter_map(|a| a.reapply_after)
        .filter(|at| *at > now)
        .max()
    {
        return Ok(Some(("reapply_wait", Some(until))));
    }
    let open = db
        .fetch_badge_applications_by_applicant(applicant_id)
        .await?
        .iter()
        .filter(|a| a.status.is_open())
        .count();
    if open >= OPEN_MAX {
        return Ok(Some(("too_many_open", None)));
    }
    Ok(None)
}

/// Start an application (a draft). Only the current owner of the server or
/// bot; one open application per program and target; not while a refusal's
/// wait runs; not for a target that already holds the mark.
pub async fn create_application(
    db: &Database,
    applicant: &User,
    program: BadgeProgram,
    target_id: &str,
    exception: bool,
    input: ApplicationInput,
    now: i64,
) -> Result<BadgeApplication> {
    if applicant.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    if exception && program != BadgeProgram::VerifiedServer {
        return Err(invalid("exception_verified_server_only"));
    }
    if input.creator.is_some() && !program.targets_person() {
        return Err(invalid("creator_content_creator_only"));
    }
    // the target exists and is theirs; otherwise it is as if it didn't exist
    if target_owner(db, program, target_id).await?.as_deref() != Some(applicant.id.as_str()) {
        return Err(create_error!(NotFound));
    }
    let already = if program.targets_person() {
        crate::held_badges(db, &applicant.id).await?.contains("content_creator")
    } else if program.targets_bot() {
        db.fetch_bot(target_id).await?.flags.unwrap_or_default() & BotFlags::Verified as i32 != 0
    } else if program == BadgeProgram::VerifiedServer {
        db.fetch_server(target_id).await?.flags.unwrap_or_default() & (crate::SERVER_FLAG_VERIFIED | crate::SERVER_FLAG_OFFICIAL) != 0
    } else {
        false
    };
    if already {
        return Err(invalid("already_approved"));
    }
    if let Some((code, _)) = application_blocker(db, &applicant.id, program, target_id, now).await? {
        return Err(invalid(code));
    }
    let application = BadgeApplication {
        id: Ulid::new().to_string(),
        program,
        applicant_id: applicant.id.clone(),
        target_id: target_id.to_string(),
        status: ApplicationStatus::Draft,
        exception,
        statement: bounded(input.statement.as_deref().unwrap_or_default(), 0, STATEMENT_MAX, "statement")?,
        links: clean_links(input.links.as_deref().unwrap_or_default())?,
        authorized: input.authorized.unwrap_or(false),
        ownership: None,
        creator: input.creator.as_ref().map(clean_creator).transpose()?,
        creator_figures: None,
        eligibility: vec![],
        created_at: now,
        updated_at: now,
        submitted_at: None,
        reviewer_id: None,
        first_approval_by: None,
        badge_request_id: None,
        decided_at: None,
        decided_by: None,
        reasons: vec![],
        note: None,
        reapply_after: None,
        appeal_until: None,
        appeal: None,
        next_review_at: None,
        recheck_due: false,
        improve_until: None,
        ended_at: None,
        end_reason: None,
        evidence_purge_at: None,
        evidence_purged: false,
        purge_at: None,
    };
    db.insert_badge_application(&application).await?;
    application_audit(db, &applicant.id, None, "badge.application.create", &application, AuditOutcome::Success, "").await?;
    Ok(application)
}

/// An application of `user` (anyone else: not found)
pub async fn own_application(db: &Database, user: &User, id: &str) -> Result<BadgeApplication> {
    let application = db.fetch_badge_application(id).await?;
    if application.applicant_id != user.id {
        return Err(create_error!(NotFound));
    }
    Ok(application)
}

fn editable(application: &BadgeApplication) -> Result<()> {
    if matches!(application.status, ApplicationStatus::Draft | ApplicationStatus::MoreInfo) {
        Ok(())
    } else {
        Err(invalid("not_editable"))
    }
}

/// Change a draft (or an application the reviewer asked more of)
pub async fn edit_application(db: &Database, mut application: BadgeApplication, input: ApplicationInput, now: i64) -> Result<BadgeApplication> {
    editable(&application)?;
    let before = application.updated_at;
    if let Some(statement) = input.statement {
        application.statement = bounded(&statement, 0, STATEMENT_MAX, "statement")?;
    }
    if let Some(links) = input.links {
        application.links = clean_links(&links)?;
    }
    if let Some(authorized) = input.authorized {
        application.authorized = authorized;
    }
    if let Some(channel) = input.creator {
        if !application.program.targets_person() {
            return Err(invalid("creator_content_creator_only"));
        }
        let channel = clean_creator(&channel)?;
        // another channel: the code placed on the old one proves nothing
        if application.creator.as_ref().is_some_and(|old| old.url != channel.url) {
            application.ownership = None;
        }
        application.creator = Some(channel);
    }
    save_application(db, &mut application, before, now).await?;
    Ok(application)
}

/// Add a link or a short note. Private: the applicant and reviewers only.
pub async fn add_evidence(db: &Database, application: &BadgeApplication, kind: &str, text: &str, now: i64) -> Result<BadgeEvidence> {
    if !application.status.is_open() {
        return Err(invalid("not_editable"));
    }
    let text = match kind {
        "link" => clean_link(text)?,
        "note" => bounded(text, 3, NOTE_MAX, "text")?,
        _ => return Err(invalid("evidence_kind")),
    };
    if db.fetch_badge_evidence(&application.id).await?.len() >= EVIDENCE_MAX {
        return Err(invalid("too_much_evidence"));
    }
    let evidence = BadgeEvidence {
        id: Ulid::new().to_string(),
        application_id: application.id.clone(),
        applicant_id: application.applicant_id.clone(),
        kind: kind.to_string(),
        text,
        created_at: now,
    };
    db.insert_badge_evidence(&evidence).await?;
    // an opaque reference only: never the evidence itself
    application_audit(
        db,
        &application.applicant_id,
        None,
        "badge.application.evidence",
        application,
        AuditOutcome::Success,
        &format!("evidence:{}", evidence.id),
    )
    .await?;
    Ok(evidence)
}

/// The value the TXT record or the file must hold
pub fn challenge_value(code: &str) -> String {
    format!("zrzur-verification={code}")
}

/// Start an ownership challenge (Verified Server: DNS or website; Content
/// Creator: the profile of the named channel). The code comes back once;
/// only its hash is kept. A new challenge replaces the old one.
pub async fn issue_challenge(
    db: &Database,
    mut application: BadgeApplication,
    method: OwnershipMethod,
    domain: &str,
    now: i64,
) -> Result<(BadgeApplication, String)> {
    editable(&application)?;
    let (domain, ttl) = match (application.program, method) {
        (BadgeProgram::VerifiedServer, OwnershipMethod::Dns | OwnershipMethod::Website) => {
            (clean_domain(domain).ok_or_else(|| invalid("domain"))?, CHALLENGE_TTL_MS)
        }
        (BadgeProgram::ContentCreator, OwnershipMethod::Profile) => {
            // the host of the named channel: the code goes on that page
            let url = application
                .creator
                .as_ref()
                .map(|c| c.url.clone())
                .filter(|u| !u.is_empty())
                .ok_or_else(|| invalid("channel_needed"))?;
            let host = reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .ok_or_else(|| invalid("https_link"))?;
            let days = revolt_config::config().await.api.badge_programs.creator_code_days;
            (host, days * DAY_MS)
        }
        _ => return Err(invalid("no_ownership_check")),
    };
    // a profile code is short enough for a person to read and type
    let code = if method == OwnershipMethod::Profile {
        format!("zrzur-{}", nanoid::nanoid!(12, &PROFILE_ALPHABET))
    } else {
        nanoid::nanoid!(32, &nanoid::alphabet::SAFE)
    };
    let value = if method == OwnershipMethod::Profile { code.clone() } else { challenge_value(&code) };
    let before = application.updated_at;
    application.ownership = Some(OwnershipChallenge {
        method,
        domain,
        code_hash: sha256_hex(value.as_bytes()),
        issued_at: now,
        expires_at: now + ttl,
        verified_at: None,
    });
    save_application(db, &mut application, before, now).await?;
    application_audit(
        db,
        &application.applicant_id,
        None,
        "badge.application.challenge",
        &application,
        AuditOutcome::Success,
        method.name(),
    )
    .await?;
    Ok((application, value))
}

/// Letters and digits a person reads without mistakes (no 0/o, 1/l/i)
const PROFILE_ALPHABET: [char; 31] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'm', 'n', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x',
    'y', 'z', '2', '3', '4', '5', '6', '7', '8', '9',
];

/// The code a reviewer saw, as it is compared: trimmed, lower case
pub fn profile_code(seen: &str) -> String {
    seen.trim().trim_matches('"').to_ascii_lowercase()
}

/// Content Creator: a reviewer opened the named channel and types the code
/// they saw in its description or a pinned post. Only the hash is compared;
/// each try is written to the audit log (result only, never the code).
pub async fn reviewer_confirm_profile(
    db: &Database,
    mut application: BadgeApplication,
    reviewer: &str,
    staff_session: Option<String>,
    seen: &str,
    now: i64,
) -> Result<BadgeApplication> {
    if !matches!(application.status, ApplicationStatus::Submitted | ApplicationStatus::InReview) {
        return Err(invalid("not_waiting"));
    }
    let Some(challenge) = application.ownership.clone().filter(|o| o.method == OwnershipMethod::Profile) else {
        return Err(invalid("no_challenge"));
    };
    let fresh = now < challenge.expires_at && !challenge.code_hash.is_empty();
    let matched = fresh && sha256_hex(profile_code(seen).as_bytes()) == challenge.code_hash;
    let detail = format!("profile {}", if matched { "matched" } else if fresh { "not_found" } else { "expired" });
    application_audit(
        db,
        reviewer,
        staff_session,
        "staff.badge.application.ownership",
        &application,
        if matched { AuditOutcome::Success } else { AuditOutcome::Denied },
        &detail,
    )
    .await?;
    if !matched {
        return Err(invalid(if fresh { "challenge_not_found" } else { "challenge_expired" }));
    }
    let before = application.updated_at;
    if let Some(ownership) = application.ownership.as_mut() {
        ownership.verified_at = Some(now);
    }
    if application.reviewer_id.is_none() {
        application.reviewer_id = Some(reviewer.to_string());
    }
    save_application(db, &mut application, before, now).await?;
    record_review(db, &application, reviewer, "ownership_seen", &[], None, now).await?;
    Ok(application)
}

/// What a reviewer records about a Content Creator channel
#[derive(Debug, Clone, Default)]
pub struct FiguresInput {
    pub followers: i64,
    pub seen_at: i64,
    pub active_days: i64,
    pub works: i64,
    pub schedule: bool,
    pub quality: Vec<String>,
}

/// Content Creator: the reviewer records what they saw on the public page
/// (the figure with its day, days active, works counted) and the quality
/// criteria they found met. Recording again replaces it. Nothing is given:
/// the decision is a separate step, and the first approval needs a second
/// person.
pub async fn record_creator_figures(
    db: &Database,
    mut application: BadgeApplication,
    reviewer: &str,
    staff_session: Option<String>,
    input: FiguresInput,
    now: i64,
) -> Result<BadgeApplication> {
    if !application.program.targets_person() {
        return Err(invalid("not_content_creator"));
    }
    if !matches!(application.status, ApplicationStatus::Submitted | ApplicationStatus::InReview) {
        return Err(invalid("not_waiting"));
    }
    // plain numbers a person read off a page; a day within the last year,
    // never in the future
    let sane = (0..=10_000_000_000).contains(&input.followers)
        && (0..=36_500).contains(&input.active_days)
        && (0..=10_000).contains(&input.works)
        && input.seen_at <= now + DAY_MS
        && input.seen_at >= now - 365 * DAY_MS;
    if !sane {
        return Err(invalid("figures"));
    }
    let mut quality: Vec<String> = vec![];
    for code in &input.quality {
        let Some(known) = CREATOR_QUALITY.iter().find(|c| **c == code.as_str()) else {
            return Err(invalid("quality"));
        };
        if !quality.iter().any(|q| q == known) {
            quality.push(known.to_string());
        }
    }
    let before = application.updated_at;
    application.creator_figures = Some(CreatorFigures {
        followers: input.followers,
        seen_at: input.seen_at,
        active_days: input.active_days,
        works: input.works,
        schedule: input.schedule,
        quality,
        recorded_by: reviewer.to_string(),
        recorded_at: now,
    });
    if application.reviewer_id.is_none() {
        application.reviewer_id = Some(reviewer.to_string());
    }
    save_application(db, &mut application, before, now).await?;
    record_review(db, &application, reviewer, "figures_recorded", &[], None, now).await?;
    // nothing about the channel in the log: ids and the program only
    application_audit(db, reviewer, staff_session, "staff.badge.application.figures", &application, AuditOutcome::Success, "").await?;
    Ok(application)
}

/// Check what the DNS record or the file held against the challenge. Each
/// try is written to the audit log (method and result only).
pub async fn confirm_challenge(db: &Database, mut application: BadgeApplication, found: &[String], now: i64) -> Result<BadgeApplication> {
    editable(&application)?;
    let Some(challenge) = application.ownership.clone() else {
        return Err(invalid("no_challenge"));
    };
    // a profile code is looked for by a reviewer, never by the server
    if challenge.method == OwnershipMethod::Profile {
        return Err(invalid("checked_by_reviewer"));
    }
    let fresh = now < challenge.expires_at && !challenge.code_hash.is_empty();
    let matched = fresh
        && found
            .iter()
            .any(|value| sha256_hex(value.trim().trim_matches('"').as_bytes()) == challenge.code_hash);
    let outcome = if matched { AuditOutcome::Success } else { AuditOutcome::Denied };
    let detail = format!(
        "{} {}",
        challenge.method.name(),
        if matched { "matched" } else if fresh { "not_found" } else { "expired" }
    );
    application_audit(db, &application.applicant_id, None, "badge.application.ownership", &application, outcome, &detail).await?;
    if !matched {
        return Err(invalid(if fresh { "challenge_not_found" } else { "challenge_expired" }));
    }
    let before = application.updated_at;
    if let Some(ownership) = application.ownership.as_mut() {
        ownership.verified_at = Some(now);
    }
    save_application(db, &mut application, before, now).await?;
    Ok(application)
}

/// Send an application: the checklist is measured again now, and every
/// measurable line must be met. Lines the server can't measure go to the
/// reviewer. Nothing is given: a person decides.
pub async fn submit_application(
    db: &Database,
    programs: &ApiBadgePrograms,
    applicant: &User,
    mut application: BadgeApplication,
    now: i64,
) -> Result<(BadgeApplication, ProgramEligibility)> {
    editable(&application)?;
    bounded(&application.statement, STATEMENT_MIN, STATEMENT_MAX, "statement")?;
    if !application.authorized {
        return Err(invalid("not_authorized"));
    }
    let eligibility = application_checklist(db, programs, applicant, &application, now).await?;
    let missing = eligibility.missing();
    if !missing.is_empty() {
        return Err(invalid(&format!("not_eligible:{}", missing.join(","))));
    }
    let before = application.updated_at;
    application.status = ApplicationStatus::Submitted;
    application.submitted_at = Some(now);
    application.eligibility = eligibility.items.clone();
    // a fresh review: nobody holds it, no first approval stands
    application.reviewer_id = None;
    application.first_approval_by = None;
    application.badge_request_id = None;
    save_application(db, &mut application, before, now).await?;
    application_audit(db, &applicant.id, None, "badge.application.submit", &application, AuditOutcome::Success, "").await?;
    Ok((application, eligibility))
}

/// The applicant takes an open application back
pub async fn withdraw_application(db: &Database, mut application: BadgeApplication, now: i64) -> Result<BadgeApplication> {
    if !application.status.is_open() {
        return Err(invalid("not_open"));
    }
    if application.status == ApplicationStatus::Draft {
        // never sent: nothing to keep
        db.delete_badge_application(&application.id).await?;
        application_audit(db, &application.applicant_id, None, "badge.application.discard", &application, AuditOutcome::Success, "").await?;
        application.status = ApplicationStatus::Withdrawn;
        return Ok(application);
    }
    let before = application.updated_at;
    application.status = ApplicationStatus::Withdrawn;
    application.first_approval_by = None;
    application.evidence_purge_at = Some(now);
    application.purge_at = Some(now + WITHDRAWN_KEEP_DAYS * DAY_MS);
    save_application(db, &mut application, before, now).await?;
    application_audit(db, &application.applicant_id, None, "badge.application.withdraw", &application, AuditOutcome::Success, "").await?;
    Ok(application)
}

/// Appeal a refusal, once, within the window
pub async fn appeal_application(db: &Database, mut application: BadgeApplication, text: &str, now: i64) -> Result<BadgeApplication> {
    if application.status != ApplicationStatus::Refused {
        return Err(invalid("not_refused"));
    }
    if application.appeal.is_some() {
        return Err(invalid("already_appealed"));
    }
    if application.appeal_until.is_none_or(|until| now > until) {
        return Err(invalid("appeal_window_closed"));
    }
    let text = bounded(text, STATEMENT_MIN, STATEMENT_MAX, "text")?;
    let before = application.updated_at;
    application.appeal = Some(ApplicationAppeal {
        text,
        created_at: now,
        status: AppealStatus::Open,
        decided_by: None,
        decided_at: None,
        reasons: vec![],
        note: None,
    });
    // the evidence stays until the appeal is decided
    application.evidence_purge_at = None;
    save_application(db, &mut application, before, now).await?;
    application_audit(db, &application.applicant_id, None, "badge.application.appeal", &application, AuditOutcome::Success, "").await?;
    Ok(application)
}

/// Reason codes and the reviewer's note, checked
pub fn checked_decision(program: BadgeProgram, reasons: &[String], note: Option<&str>) -> Result<(Vec<String>, Option<String>)> {
    if reasons.is_empty() || reasons.len() > REASONS_MAX {
        return Err(invalid("reasons"));
    }
    let allowed = reason_codes(program);
    let mut codes: Vec<String> = vec![];
    for reason in reasons {
        let Some(code) = allowed.iter().find(|code| **code == reason.as_str()) else {
            return Err(invalid("reasons"));
        };
        if !codes.iter().any(|c| c == code) {
            codes.push(code.to_string());
        }
    }
    let note = match note.map(clean_text).filter(|n| !n.is_empty()) {
        Some(note) if note.chars().count() > NOTE_MAX => return Err(invalid("note")),
        other => other,
    };
    if codes.iter().any(|c| c == "other") && note.is_none() {
        return Err(invalid("note_needed"));
    }
    Ok((codes, note))
}

/// Write a reviewer's step to `badge_reviews`
pub async fn record_review(
    db: &Database,
    application: &BadgeApplication,
    reviewer: &str,
    action: &str,
    reasons: &[String],
    note: Option<&str>,
    now: i64,
) -> Result<()> {
    db.insert_badge_review(&BadgeReview {
        id: Ulid::new().to_string(),
        application_id: application.id.clone(),
        reviewer_id: reviewer.to_string(),
        action: action.to_string(),
        reasons: reasons.to_vec(),
        note: note.map(str::to_string),
        created_at: now,
    })
    .await
}

/// A refusal's dates: the wait before a new application, the appeal window
/// and when the evidence and the record go
pub fn refuse_dates(application: &mut BadgeApplication, programs: &ApiBadgePrograms, now: i64) {
    // Content Creator: when only information was missing there is no wait
    // (the published rule: "sooner if only information was missing")
    let only_information = application.program == BadgeProgram::ContentCreator
        && !application.reasons.is_empty()
        && application.reasons.iter().all(|r| r == "more_information");
    application.reapply_after = Some(if only_information { now } else { now + programs.reapply_days * DAY_MS });
    application.appeal_until = Some(now + programs.appeal_days * DAY_MS);
    application.evidence_purge_at = Some(now + (programs.appeal_days + programs.evidence_days) * DAY_MS);
    application.purge_at = Some(now + programs.record_days.max(programs.reapply_days) * DAY_MS);
}

/// An approval's dates: evidence goes after `evidence_days`; Partner is
/// checked again after `partner_review_days`; the record stays while the
/// badge stands
pub fn approve_dates(application: &mut BadgeApplication, programs: &ApiBadgePrograms, now: i64) {
    application.evidence_purge_at = Some(now + programs.evidence_days * DAY_MS);
    application.purge_at = None;
    application.next_review_at =
        (application.program == BadgeProgram::Partner).then_some(now + programs.partner_review_days * DAY_MS);
    application.recheck_due = false;
    application.improve_until = None;
}

/// Whether a program's badge (Partner, Content Creator) may be given to
/// this person: an application of theirs that a reviewer approved (the
/// badge waits for the second person) or that is approved. No path gives
/// either badge without one.
pub async fn program_application_backs(db: &Database, user_id: &str, program: BadgeProgram) -> Result<bool> {
    Ok(db.fetch_badge_applications_by_applicant(user_id).await?.iter().any(|a| {
        a.program == program
            && (a.status == ApplicationStatus::Approved
                || (a.status == ApplicationStatus::InReview && a.first_approval_by.is_some()))
    }))
}

/// [`program_application_backs`] for Partner
pub async fn partner_application_backs(db: &Database, user_id: &str) -> Result<bool> {
    program_application_backs(db, user_id, BadgeProgram::Partner).await
}

/// A person took a program's badge away in the badge console (Content
/// Creator): the approved application behind it ends with `badge_removed`.
/// The gate calls it after the change; failures are logged.
pub async fn program_badge_removed(db: &Database, user_id: &str, badge: &str, actor: &str) {
    let program = match badge {
        "content_creator" => BadgeProgram::ContentCreator,
        _ => return,
    };
    let programs = revolt_config::config().await.api.badge_programs;
    let applications = match db.fetch_badge_applications_by_applicant(user_id).await {
        Ok(list) => list,
        Err(error) => {
            log::error!("applications of {user_id}: {:?}", error.error_type);
            return;
        }
    };
    for application in applications
        .into_iter()
        .filter(|a| a.program == program && a.status == ApplicationStatus::Approved)
    {
        // the badge is gone already: only the record ends
        if let Err(error) = mark_ended(db, &programs, application, actor, "badge_removed", now_ms()).await {
            log::error!("ending the {badge} application of {user_id}: {:?}", error.error_type);
        }
    }
}

/// Take a badge away because its condition ended (the system, through the
/// gate). Already gone: nothing to do.
async fn system_revoke(db: &Database, user_id: &str, badge: &str, source_ref: &str, reason: &str) -> Result<()> {
    if !crate::held_badges(db, user_id).await?.contains(badge) {
        return Ok(());
    }
    match mutate_badge(
        db,
        &BadgeActor::system(),
        BadgeMutation {
            user_id: user_id.to_string(),
            badge: badge.to_string(),
            change: BadgeChange::Revoke,
            reason: reason.to_string(),
            expires_at: None,
            source_ref: Some(source_ref.to_string()),
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) if matches!(error.error_type, ErrorType::NoEffect) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The record of an approved application ends (no badge is touched here)
async fn mark_ended(
    db: &Database,
    programs: &ApiBadgePrograms,
    mut application: BadgeApplication,
    actor: &str,
    reason: &str,
    now: i64,
) -> Result<BadgeApplication> {
    let before = application.updated_at;
    application.status = ApplicationStatus::Ended;
    application.ended_at = Some(now);
    application.end_reason = Some(reason.to_string());
    application.recheck_due = false;
    application.next_review_at = None;
    application.purge_at = Some(now + programs.record_days * DAY_MS);
    save_application(db, &mut application, before, now).await?;
    application_audit(db, actor, None, "badge.application.end", &application, AuditOutcome::Success, reason).await?;
    Ok(application)
}

/// End an approved application (its mark or badge went), with the Partner
/// badge when it was one. The caller deals with the marks and sends the
/// notice.
pub async fn end_application(
    db: &Database,
    programs: &ApiBadgePrograms,
    application: BadgeApplication,
    actor: &str,
    reason: &str,
    now: i64,
) -> Result<BadgeApplication> {
    if application.status != ApplicationStatus::Approved {
        return Ok(application);
    }
    let application = mark_ended(db, programs, application, actor, reason, now).await?;
    if application.program == BadgeProgram::Partner {
        system_revoke(
            db,
            &application.applicant_id,
            "partner",
            &format!("application:{}", application.id),
            &format!("partnership ended: {reason}"),
        )
        .await?;
    }
    Ok(application)
}

/// A server or bot changed hands, was deleted or suspended: its approved
/// applications end, open ones are closed (the old owner can't continue
/// them). Returns the ended ones (for notices).
pub async fn end_programs_for_target(
    db: &Database,
    programs: &ApiBadgePrograms,
    target_id: &str,
    only: Option<BadgeProgram>,
    reason: &str,
    now: i64,
) -> Result<Vec<BadgeApplication>> {
    let mut ended = vec![];
    for application in db.fetch_badge_applications_for_target(target_id).await? {
        if only.is_some_and(|program| program != application.program) {
            continue;
        }
        if application.status == ApplicationStatus::Approved {
            ended.push(end_application(db, programs, application, crate::BADGE_SYSTEM_ACTOR, reason, now).await?);
        } else if application.status.is_open() && application.status != ApplicationStatus::Draft {
            let mut closed = application;
            let before = closed.updated_at;
            closed.status = ApplicationStatus::Withdrawn;
            closed.end_reason = Some(reason.to_string());
            closed.first_approval_by = None;
            closed.evidence_purge_at = Some(now);
            closed.purge_at = Some(now + WITHDRAWN_KEEP_DAYS * DAY_MS);
            save_application(db, &mut closed, before, now).await?;
            application_audit(db, crate::BADGE_SYSTEM_ACTOR, None, "badge.application.close", &closed, AuditOutcome::Success, reason).await?;
        } else if application.status == ApplicationStatus::Draft {
            db.delete_badge_application(&application.id).await?;
        }
    }
    Ok(ended)
}

/// What a clean-up pass did
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProgramPurgeReport {
    pub evidence: usize,
    pub records: usize,
    pub rechecks: usize,
}

/// Retention: evidence and ownership codes past their date, records past
/// theirs, untouched drafts. And rechecks: approved partnerships past their
/// date are flagged for a reviewer (never ended automatically). Safe to run
/// again.
pub async fn program_upkeep(db: &Database, programs: &ApiBadgePrograms, now: i64) -> Result<ProgramPurgeReport> {
    let mut report = ProgramPurgeReport::default();
    let draft_before = now - programs.draft_days * DAY_MS;
    for mut application in db.fetch_badge_applications_expired(now, draft_before).await? {
        let whole = application.purge_at.is_some_and(|at| at <= now)
            || (application.status == ApplicationStatus::Draft && application.updated_at < draft_before);
        if whole {
            db.delete_badge_application(&application.id).await?;
            report.records += 1;
            continue;
        }
        db.delete_badge_evidence(&application.id).await?;
        let before = application.updated_at;
        if let Some(ownership) = application.ownership.as_mut() {
            ownership.code_hash = String::new();
        }
        // the links to works are evidence too; the channel and the
        // recorded figures stay with the decision record
        if let Some(channel) = application.creator.as_mut() {
            channel.works.clear();
            channel.schedule = None;
        }
        application.evidence_purged = true;
        save_application(db, &mut application, before, now).await?;
        report.evidence += 1;
    }
    for mut application in db.fetch_badge_applications_due(now).await? {
        let before = application.updated_at;
        application.recheck_due = true;
        save_application(db, &mut application, before, now).await?;
        application_audit(db, crate::BADGE_SYSTEM_ACTOR, None, "badge.application.recheck_due", &application, AuditOutcome::Success, "").await?;
        report.rechecks += 1;
    }
    Ok(report)
}

/// The account is erased: its applications, evidence and reviews go with
/// it (legal BV-09)
pub async fn erase_applications_of(db: &Database, user_id: &str) -> Result<usize> {
    let applications = db.fetch_badge_applications_by_applicant(user_id).await?;
    for application in &applications {
        db.delete_badge_application(&application.id).await?;
    }
    Ok(applications.len())
}

fn now_ms() -> i64 {
    crate::epoch_ms(iso8601_timestamp::Timestamp::now_utc())
}

/// A server is gone (deleted): its programs end. No badge follows (the
/// owner's Verified Server Owner badge was cancelled; holders keep it).
/// Failures are logged, never stop the deletion.
pub async fn server_programs_gone(db: &Database, server_id: &str) {
    let programs = revolt_config::config().await.api.badge_programs;
    if let Err(error) = end_programs_for_target(db, &programs, server_id, None, "server_deleted", now_ms()).await {
        log::error!("programs of deleted server {server_id}: {:?}", error.error_type);
    }
}

/// A bot was deleted or suspended (`reason` `bot_deleted` or
/// `bot_suspended`), or its mark taken away (`mark_removed`): its Verified
/// Bot application ends and its owner's Verified Bot Developer badge follows.
/// Failures are logged.
pub async fn bot_programs_ended(db: &Database, bot_id: &str, owner: &str, reason: &str) -> Vec<BadgeApplication> {
    let programs = revolt_config::config().await.api.badge_programs;
    let ended = match end_programs_for_target(db, &programs, bot_id, Some(BadgeProgram::VerifiedBot), reason, now_ms()).await {
        Ok(ended) => ended,
        Err(error) => {
            log::error!("programs of bot {bot_id}: {:?}", error.error_type);
            vec![]
        }
    };
    if let Err(error) = crate::sync_verified_bot_developer(db, owner).await {
        log::error!("verified developer badge for {owner}: {:?}", error.error_type);
    }
    ended
}
