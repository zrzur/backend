//! Badge programs people apply for: Zrzur Partner, Verified Server (with
//! Verified Server Owner), Verified Bot Developer and Content Creator.
//!
//! Behind `[features.badge_applications] enabled` (off by default): off,
//! every route here answers `FeatureDisabled`; the team's review in the
//! admin console keeps working for applications already made.
//!
//! The numbers of a program only let someone apply. The applicant writes a
//! draft, proves control of a domain where the program needs it (Verified
//! Server: a DNS TXT record or a file on the website), and sends it; the
//! checklist is measured again then. A person on the team decides in the
//! admin console (`routes/staff/badge_applications.rs`), with reasons that
//! reach the applicant in their language. A refusal can be appealed once
//! within 30 days; a new application waits 90 days.
//!
//! Evidence and the application text are private: only the applicant and
//! reviewers see them. Nothing here is public.

use revolt_config::config;
use revolt_database::{
    add_evidence, appeal_application, application_blocker, confirm_challenge, create_application,
    edit_application, end_programs_for_target, issue_challenge, own_application, program_eligibility,
    submit_application, sync_verified_server_owner, withdraw_application, ApplicationInput,
    BadgeApplication, BadgeProgram, CreatorChannel, Database, EligibilityItem, OwnershipMethod,
    PartialServer, Server, User, DNS_RECORD_PREFIX, SERVER_FLAG_OFFICIAL, SERVER_FLAG_PARTNER,
    SERVER_FLAG_VERIFIED, WEBSITE_PATH,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::{no_bots, now_ms};
use crate::util::ownership_probe;

/// The applicant routes are open only with `[features.badge_applications]`
pub(crate) async fn applications_open() -> Result<()> {
    if config().await.features.badge_applications.enabled {
        Ok(())
    } else {
        Err(create_error!(FeatureDisabled {
            feature: "badge_applications".to_string()
        }))
    }
}

/// The channel from the API shape
pub(crate) fn channel_in(data: v0::CreatorChannelData) -> CreatorChannel {
    CreatorChannel {
        platform: data.platform,
        platform_name: data.platform_name,
        url: data.url,
        category: data.category,
        works: data.works,
        schedule: data.schedule,
    }
}

/// The channel in the API shape
pub(crate) fn channel_view(channel: &CreatorChannel) -> v0::CreatorChannelData {
    v0::CreatorChannelData {
        platform: channel.platform.clone(),
        platform_name: channel.platform_name.clone(),
        url: channel.url.clone(),
        category: channel.category.clone(),
        works: channel.works.clone(),
        schedule: channel.schedule.clone(),
    }
}

fn program_of(name: &str) -> Result<BadgeProgram> {
    BadgeProgram::from_name(name).ok_or_else(|| create_error!(FailedValidation { error: "program".to_string() }))
}

pub(crate) fn check_view(item: &EligibilityItem) -> v0::ProgramCheck {
    v0::ProgramCheck {
        code: item.code.clone(),
        met: item.met,
        value: item.value,
        needed: item.needed,
    }
}

/// Name of the server or bot (Content Creator: the applicant), when it
/// still exists
pub(crate) async fn target_name(db: &Database, application: &BadgeApplication) -> Option<String> {
    if application.program.targets_bot() || application.program.targets_person() {
        db.fetch_user(&application.target_id).await.ok().map(|user| user.username)
    } else {
        db.fetch_server(&application.target_id).await.ok().map(|server| server.name)
    }
}

pub(crate) fn ownership_view(application: &BadgeApplication) -> Option<v0::ApplicationOwnership> {
    application.ownership.as_ref().map(|o| {
        let profile = o.method == OwnershipMethod::Profile;
        v0::ApplicationOwnership {
            method: o.method.name().to_string(),
            domain: o.domain.clone(),
            record_name: if profile { String::new() } else { format!("{DNS_RECORD_PREFIX}.{}", o.domain) },
            file_url: if profile { String::new() } else { format!("https://{}{WEBSITE_PATH}", o.domain) },
            expires_at: o.expires_at,
            verified_at: o.verified_at,
        }
    })
}

/// The applicant's view: no reviewer is ever named
pub(crate) async fn application_view(db: &Database, application: &BadgeApplication) -> Result<v0::BadgeApplicationView> {
    let evidence = if application.evidence_purged {
        vec![]
    } else {
        db.fetch_badge_evidence(&application.id).await?
    };
    Ok(v0::BadgeApplicationView {
        id: application.id.clone(),
        program: application.program.name().to_string(),
        target_id: application.target_id.clone(),
        target_name: target_name(db, application).await,
        status: application.status.name().to_string(),
        exception: application.exception,
        statement: application.statement.clone(),
        links: application.links.clone(),
        authorized: application.authorized,
        ownership: ownership_view(application),
        creator: application.creator.as_ref().map(channel_view),
        checks: application.eligibility.iter().map(check_view).collect(),
        created_at: application.created_at,
        submitted_at: application.submitted_at,
        decided_at: application.decided_at,
        reasons: application.reasons.clone(),
        note: application.note.clone(),
        reapply_after: application.reapply_after,
        appeal_until: application.appeal_until,
        appeal: application.appeal.as_ref().map(|a| v0::ApplicationAppealView {
            status: a.status.name().to_string(),
            created_at: a.created_at,
            decided_at: a.decided_at,
            reasons: a.reasons.clone(),
            note: a.note.clone(),
        }),
        next_review_at: application.next_review_at,
        improve_until: application.improve_until,
        ended_at: application.ended_at,
        end_reason: application.end_reason.clone(),
        evidence: evidence
            .into_iter()
            .map(|e| v0::ApplicationEvidence {
                id: e.id,
                kind: e.kind,
                text: e.text,
                created_at: e.created_at,
            })
            .collect(),
    })
}

/// A server changed hands: Partner and Verified Server end and their marks
/// go (the new owner applies again; the official mark stays), the old
/// applicant hears why, and both owners' Verified Server Owner badges
/// follow. Returns the server's flags after. Failures are logged and never
/// undo the transfer.
pub(crate) async fn after_server_transfer(db: &Database, server: &Server, previous_owner: &str) -> Option<i32> {
    let mut flags = server.flags;
    let current = server.flags.unwrap_or_default();
    if current & (SERVER_FLAG_VERIFIED | SERVER_FLAG_PARTNER) != 0 && current & SERVER_FLAG_OFFICIAL == 0 {
        let next = current & !(SERVER_FLAG_VERIFIED | SERVER_FLAG_PARTNER);
        let mut server = server.clone();
        match server
            .update(db, PartialServer { flags: Some(next), ..Default::default() }, vec![])
            .await
        {
            Ok(_) => flags = Some(next),
            Err(error) => log::error!("marks of transferred server {}: {:?}", server.id, error.error_type),
        }
    }
    let programs = config().await.api.badge_programs;
    match end_programs_for_target(db, &programs, &server.id, None, "ownership_transferred", now_ms()).await {
        Ok(ended) => {
            for application in ended {
                crate::util::official::program_ended(
                    db,
                    &application.applicant_id,
                    application.program.name(),
                    "ownership_transferred",
                    None,
                )
                .await;
            }
        }
        Err(error) => log::error!("programs of transferred server {}: {:?}", server.id, error.error_type),
    }
    for owner in [previous_owner, server.owner.as_str()] {
        if let Err(error) = sync_verified_server_owner(db, owner).await {
            log::error!("verified server owner badge for {owner}: {:?}", error.error_type);
        }
    }
    flags
}

/// # Servers and bots you can apply for
///
/// The servers and bots you own. Applying is open only to the owner.
#[openapi(tag = "Badges")]
#[get("/programs/targets")]
pub async fn targets(db: &State<Database>, user: User) -> Result<Json<Vec<v0::ProgramTarget>>> {
    applications_open().await?;
    no_bots(&user)?;
    let mut list: Vec<v0::ProgramTarget> = db
        .fetch_owned_servers(&user.id)
        .await?
        .into_iter()
        .map(|server| v0::ProgramTarget {
            id: server.id,
            name: server.name,
            kind: "server".to_string(),
        })
        .collect();
    for bot in db.fetch_bots_by_user(&user.id).await? {
        let name = db.fetch_user(&bot.id).await.map(|u| u.username).unwrap_or_default();
        list.push(v0::ProgramTarget {
            id: bot.id,
            name,
            kind: "bot".to_string(),
        });
    }
    Ok(Json(list))
}

/// # Program checklist
///
/// What the server measured for one of your servers or bots, line by line,
/// and whether you may start an application. Lines without `met` are
/// checked by a person. Meeting the numbers only lets you apply. Content
/// Creator: `target` is `@me` (or your own id).
#[openapi(tag = "Badges")]
#[get("/programs/<program>/eligibility?<target>&<exception>")]
pub async fn eligibility(
    db: &State<Database>,
    user: User,
    program: &str,
    target: &str,
    exception: Option<bool>,
) -> Result<Json<v0::ProgramEligibility>> {
    applications_open().await?;
    no_bots(&user)?;
    let program = program_of(program)?;
    let target = if program.targets_person() && target == "@me" { user.id.as_str() } else { target };
    let exception = exception.unwrap_or(false) && program == BadgeProgram::VerifiedServer;
    let now = now_ms();
    let programs = config().await.api.badge_programs;
    let checklist = program_eligibility(db, &programs, program, &user, target, exception, false, now).await?;
    // only the owner sees a target's checklist
    if checklist.owner_id.as_deref() != Some(user.id.as_str()) {
        return Err(create_error!(NotFound));
    }
    let blocked = application_blocker(db, &user.id, program, target, now).await?;
    // the ownership check is done inside the application
    let missing: Vec<String> = checklist.missing().into_iter().filter(|code| code != "ownership").collect();
    Ok(Json(v0::ProgramEligibility {
        program: program.name().to_string(),
        target_id: target.to_string(),
        checks: checklist.items.iter().map(check_view).collect(),
        can_apply: missing.is_empty() && blocked.is_none(),
        blocked: blocked.map(|(code, _)| code.to_string()),
        blocked_until: blocked.and_then(|(_, until)| until),
    }))
}

/// # Your applications
///
/// Your badge program applications, newest first, with decisions, reasons
/// and appeals. Reviewers are never named.
#[openapi(tag = "Badges")]
#[get("/applications/@me")]
pub async fn my_applications(db: &State<Database>, user: User) -> Result<Json<Vec<v0::BadgeApplicationView>>> {
    applications_open().await?;
    no_bots(&user)?;
    let mut list = vec![];
    for application in db.fetch_badge_applications_by_applicant(&user.id).await? {
        list.push(application_view(db, &application).await?);
    }
    Ok(Json(list))
}

/// # Start an application
///
/// A draft for one of your servers or bots. One open application per
/// program and target; after a refusal the target waits before a new one.
#[openapi(tag = "Badges")]
#[post("/applications", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreateBadgeApplication>,
) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    no_bots(&user)?;
    let data = data.into_inner();
    let program = program_of(&data.program)?;
    let target = if program.targets_person() && data.target_id == "@me" { user.id.clone() } else { data.target_id };
    let application = create_application(
        db,
        &user,
        program,
        &target,
        data.exception,
        ApplicationInput {
            statement: data.statement,
            links: data.links,
            authorized: data.authorized,
            creator: data.creator.map(channel_in),
        },
        now_ms(),
    )
    .await?;
    application_view(db, &application).await.map(Json)
}

/// # Change a draft
#[openapi(tag = "Badges")]
#[patch("/applications/<id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataEditBadgeApplication>,
) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    let data = data.into_inner();
    let application = own_application(db, &user, id).await?;
    let application = edit_application(
        db,
        application,
        ApplicationInput {
            statement: data.statement,
            links: data.links,
            authorized: data.authorized,
            creator: data.creator.map(channel_in),
        },
        now_ms(),
    )
    .await?;
    application_view(db, &application).await.map(Json)
}

/// # Add a link or a note
///
/// Private: only you and the reviewers see it. Never send identity
/// documents.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/evidence", data = "<data>")]
pub async fn evidence(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataApplicationEvidence>,
) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    let application = own_application(db, &user, id).await?;
    add_evidence(db, &application, &data.kind, &data.text, now_ms()).await?;
    application_view(db, &application).await.map(Json)
}

/// # Start an ownership check
///
/// Verified Server: put the value that comes back in a TXT record at
/// `_zrzur-verification.<domain>` or in the file
/// `https://<domain>/.well-known/zrzur-verification.txt`. It is shown only
/// now and accepted for 7 days. Content Creator (`profile`): put it in the
/// description or a pinned post of the channel you named; a reviewer looks
/// for it by hand (Zrzur never calls the platform). Accepted for 90 days.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/ownership", data = "<data>")]
pub async fn ownership(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataOwnershipChallenge>,
) -> Result<Json<v0::OwnershipChallengeIssued>> {
    applications_open().await?;
    let method = OwnershipMethod::from_name(&data.method)
        .ok_or_else(|| create_error!(FailedValidation { error: "method".to_string() }))?;
    let application = own_application(db, &user, id).await?;
    let (application, value) = issue_challenge(db, application, method, &data.domain, now_ms()).await?;
    Ok(Json(v0::OwnershipChallengeIssued {
        application: application_view(db, &application).await?,
        value,
    }))
}

/// # Check ownership now
///
/// Reads the TXT record or the file and compares it with the code. A
/// Content Creator profile code is checked by a reviewer instead.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/ownership/verify")]
pub async fn verify_ownership(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    let application = own_application(db, &user, id).await?;
    let Some(challenge) = application.ownership.clone() else {
        return Err(create_error!(FailedValidation { error: "no_challenge".to_string() }));
    };
    let found = match challenge.method {
        OwnershipMethod::Dns => ownership_probe::lookup_txt(&format!("{DNS_RECORD_PREFIX}.{}", challenge.domain)).await,
        OwnershipMethod::Website => ownership_probe::fetch_website_file(&challenge.domain, WEBSITE_PATH).await,
        // nothing is fetched: `confirm_challenge` refuses it
        OwnershipMethod::Profile => vec![],
    };
    let application = confirm_challenge(db, application, &found, now_ms()).await?;
    application_view(db, &application).await.map(Json)
}

/// # Send an application
///
/// The checklist is measured again now; every measurable line must be met.
/// A person on the team reviews it when a volunteer is free: no date is
/// promised.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/submit")]
pub async fn submit(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    no_bots(&user)?;
    let application = own_application(db, &user, id).await?;
    let programs = config().await.api.badge_programs;
    let (application, _) = submit_application(db, &programs, &user, application, now_ms()).await?;
    application_view(db, &application).await.map(Json)
}

/// # Withdraw an application
///
/// A draft is deleted; a sent application is kept for 30 days as withdrawn.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/withdraw")]
pub async fn withdraw(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    let application = own_application(db, &user, id).await?;
    let application = withdraw_application(db, application, now_ms()).await?;
    application_view(db, &application).await.map(Json)
}

/// # Appeal a refusal
///
/// Once per decision, within 30 days of it.
#[openapi(tag = "Badges")]
#[post("/applications/<id>/appeal", data = "<data>")]
pub async fn appeal(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataApplicationAppeal>,
) -> Result<Json<v0::BadgeApplicationView>> {
    applications_open().await?;
    let application = own_application(db, &user, id).await?;
    let application = appeal_application(db, application, &data.text, now_ms()).await?;
    application_view(db, &application).await.map(Json)
}
