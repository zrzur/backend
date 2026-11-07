//! Profile badges: the catalogue, your own badges, and the staff tools to
//! grant and revoke them. Every grant goes through
//! `revolt_database::grant_badge`, which writes the audit log; nothing
//! here trusts the client about who may do what.

pub mod beta;
pub mod bugs;
#[cfg(test)]
mod test;

use iso8601_timestamp::Timestamp;
use revolt_database::{
    badge_definition, badge_eligibility, beta_features_for, bug_hunter_stats, creator_eligibility,
    epoch_ms, grant_badge, platform_permissions, revoke_badge, set_badge_hidden,
    sync_creator_badge, sync_user_badges, BadgeDefinition, BadgeGrant, BadgeGrantRule, BadgeSource, BetaMember,
    Database, PlatformPermission, User, BADGE_CATALOGUE, ELITE_MIN_VERIFIED,
    SUPER_CREATOR_MIN_DESIGNS,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use super::community::{accounts_for, require};

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        catalogue,
        mine,
        visibility,
        user_badges,
        admin_search,
        admin_user,
        admin_grant,
        admin_revoke
    ]
}

pub(crate) fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

pub(crate) fn validated<T: Validate>(data: T) -> Result<T> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    Ok(data)
}

/// Bots never use staff tools or receive badges
pub(crate) fn no_bots(user: &User) -> Result<()> {
    if user.bot.is_some() {
        Err(create_error!(IsBot))
    } else {
        Ok(())
    }
}

fn rule_name(rule: BadgeGrantRule) -> &'static str {
    match rule {
        BadgeGrantRule::Dynamic => "dynamic",
        BadgeGrantRule::Automatic => "automatic",
        BadgeGrantRule::Staff => "staff",
        BadgeGrantRule::Privileged => "privileged",
    }
}

fn source_name(source: &BadgeSource) -> &'static str {
    match source {
        BadgeSource::Admin => "admin",
        BadgeSource::System => "system",
        BadgeSource::Legacy => "legacy",
    }
}

fn own(grant: BadgeGrant) -> v0::OwnBadge {
    v0::OwnBadge {
        source: source_name(&grant.source).to_string(),
        badge: grant.badge,
        granted_at: grant.granted_at,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
        revoke_reason: grant.revoke_reason,
        hidden: grant.hidden,
    }
}

fn admin_grant_view(grant: BadgeGrant, now: i64) -> v0::AdminBadgeGrant {
    v0::AdminBadgeGrant {
        held: grant.held(now),
        source: source_name(&grant.source).to_string(),
        badge: grant.badge,
        source_ref: grant.source_ref,
        granted_by: grant.granted_by,
        granted_at: grant.granted_at,
        reason: grant.reason,
        expires_at: grant.expires_at,
        revoked_at: grant.revoked_at,
        revoked_by: grant.revoked_by,
        revoke_reason: grant.revoke_reason,
        hidden: grant.hidden,
    }
}

pub(crate) fn beta_view(member: &BetaMember, features: Vec<String>) -> v0::BetaMembership {
    v0::BetaMembership {
        status: format!("{:?}", member.status),
        features,
        joined_at: member.joined_at,
        ended_at: member.ended_at,
        contributions: member.contributions,
    }
}

/// # Badge catalogue
///
/// Every badge, its bit in `User.badges`, tier and how it is given out.
#[openapi(tag = "Badges")]
#[get("/catalogue")]
pub async fn catalogue() -> Json<Vec<v0::BadgeInfo>> {
    Json(
        BADGE_CATALOGUE
            .iter()
            .map(|b| v0::BadgeInfo {
                id: b.id.to_string(),
                bit: b.bit,
                category: b.category.to_string(),
                tier: b.tier,
                priority: b.priority,
                rule: rule_name(b.rule).to_string(),
                retired: b.retired,
            })
            .collect(),
    )
}

/// # My badges
///
/// Your badges (held, expired and revoked) and how close you are to the
/// ones you can earn.
#[openapi(tag = "Badges")]
#[get("/@me")]
pub async fn mine(db: &State<Database>, user: User) -> Result<Json<v0::MyBadges>> {
    no_bots(&user)?;
    // creators who published before badges existed get theirs here
    sync_creator_badge(db, &user.id).await?;
    let user = sync_user_badges(db, &user.id).await?;
    let mut badges: Vec<v0::OwnBadge> = db
        .fetch_badge_grants(&user.id)
        .await?
        .into_iter()
        .map(own)
        .collect();
    badges.sort_by_key(|b| badge_definition(&b.badge).map(|d| d.priority).unwrap_or(u16::MAX));

    let role = db.fetch_platform_role(&user.id).await?;
    let perms = platform_permissions(db, &user).await?;
    let (eligible, published) = creator_eligibility(db, &user.id).await?;
    let (verified, security_verified) = bug_hunter_stats(db, &user.id).await?;
    let beta = match db.fetch_beta_member(&user.id).await? {
        Some(member) => {
            let features = beta_features_for(db, &member).await?;
            Some(beta_view(&member, features))
        }
        None => None,
    };
    let plus = user.has_plus().await;
    Ok(Json(v0::MyBadges {
        badges,
        creator: v0::CreatorProgress {
            standing: role.and_then(|r| r.creator).map(|c| format!("{c:?}")),
            can_submit: perms.has(PlatformPermission::CosmeticsSubmit),
            published,
            eligible,
            super_creator_designs: SUPER_CREATOR_MIN_DESIGNS,
        },
        bugs: v0::BugProgress {
            verified,
            security_verified,
            elite_verified: ELITE_MIN_VERIFIED,
        },
        beta,
        plus,
        permissions: perms.names(),
        privileged: user.privileged,
    }))
}

/// # Show or hide a badge
///
/// Hide one of your badges from your profile, or show it again.
#[openapi(tag = "Badges")]
#[patch("/@me/<badge>", data = "<data>")]
pub async fn visibility(
    db: &State<Database>,
    user: User,
    badge: &str,
    data: Json<v0::DataBadgeVisibility>,
) -> Result<EmptyResponse> {
    no_bots(&user)?;
    set_badge_hidden(db, &user.id, badge, data.hidden).await?;
    Ok(EmptyResponse)
}

/// # A user's badges
///
/// When each shown badge was earned. Never says why or who granted it.
#[openapi(tag = "Badges")]
#[get("/users/<id>")]
pub async fn user_badges(
    db: &State<Database>,
    _user: User,
    id: &str,
) -> Result<Json<Vec<v0::PublicBadge>>> {
    let now = now_ms();
    Ok(Json(
        db.fetch_badge_grants(id)
            .await?
            .into_iter()
            .filter(|g| g.held(now) && !g.hidden)
            .map(|g| v0::PublicBadge {
                badge: g.badge,
                granted_at: g.granted_at,
            })
            .collect(),
    ))
}

/// Which hand-given badges someone may grant and revoke
#[derive(Clone, Copy)]
pub(crate) struct BadgeRights {
    /// `Staff`-rule badges
    pub staff: bool,
    /// `Privileged`-rule badges (Staff badge, old official badges)
    pub privileged: bool,
}

async fn rights_of(db: &Database, actor: &User) -> Result<BadgeRights> {
    let perms = platform_permissions(db, actor).await?;
    Ok(BadgeRights {
        staff: perms.has(PlatformPermission::BadgesGrant),
        privileged: actor.privileged,
    })
}

/// Can someone with `rights` grant or revoke `definition` on `subject`?
pub(crate) fn check_rule(
    actor_id: &str,
    rights: BadgeRights,
    subject: &User,
    definition: &BadgeDefinition,
) -> Result<()> {
    if subject.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    // nobody grants or revokes their own badges
    if subject.id == actor_id {
        return Err(create_error!(NotPrivileged));
    }
    match definition.rule {
        // Plus comes from the subscription, Creator from designs, Beta
        // badges from the beta program: change the record, not the badge
        BadgeGrantRule::Dynamic | BadgeGrantRule::Automatic => {
            Err(create_error!(InvalidOperation))
        }
        BadgeGrantRule::Privileged if rights.privileged => Ok(()),
        BadgeGrantRule::Privileged => Err(create_error!(NotPrivileged)),
        BadgeGrantRule::Staff if rights.staff => Ok(()),
        BadgeGrantRule::Staff => Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::BadgesGrant.name().to_string()
        })),
    }
}

/// Staff need `badges.grant` (or a privileged account) to look
async fn staff_only(db: &Database, actor: &User) -> Result<()> {
    no_bots(actor)?;
    if actor.privileged {
        return Ok(());
    }
    let perms = platform_permissions(db, actor).await?;
    require(&perms, PlatformPermission::BadgesGrant)
}

/// # Find badge holders
///
/// Users matching a name or id, or everyone holding one badge.
#[openapi(tag = "Badges")]
#[get("/admin/users?<options..>")]
pub async fn admin_search(
    db: &State<Database>,
    user: User,
    options: v0::OptionsBadgeSearch,
) -> Result<Json<Vec<v0::BadgeHolder>>> {
    // beta staff search here too, to invite testers
    no_bots(&user)?;
    let perms = platform_permissions(db, &user).await?;
    if !perms.has(PlatformPermission::BadgesGrant) && !perms.has(PlatformPermission::BetaManage) {
        require(&perms, PlatformPermission::BadgesGrant)?;
    }
    let options = validated(options)?;
    let ids: Vec<String> = match (
        options.query.as_deref().map(str::trim).filter(|q| !q.is_empty()),
        options.badge.as_deref(),
    ) {
        (Some(query), _) => db
            .search_users_by_name(query.trim_start_matches('@'), 25)
            .await?
            .into_iter()
            .filter(|u| u.bot.is_none())
            .map(|u| u.id)
            .collect(),
        (None, Some(badge)) => db
            .fetch_badge_holders(badge, 100)
            .await?
            .into_iter()
            .map(|g| g.user_id)
            .collect(),
        (None, None) => vec![],
    };
    let now = now_ms();
    let grants = db.fetch_badge_grants_for(&ids).await?;
    let users = accounts_for(db, ids.clone()).await?;
    Ok(Json(
        ids.iter()
            .filter_map(|id| users.iter().find(|u| &u.id == id).cloned())
            .map(|u| v0::BadgeHolder {
                badges: grants
                    .iter()
                    .filter(|g| g.user_id == u.id && g.held(now))
                    .map(|g| g.badge.clone())
                    .collect(),
                user: u,
            })
            .collect(),
    ))
}

/// # A user's badge record
///
/// Grants, audit log and which badges staff can give now.
#[openapi(tag = "Badges")]
#[get("/admin/users/<id>")]
pub async fn admin_user(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::AdminUserBadges>> {
    staff_only(db, &user).await?;
    let rights = rights_of(db, &user).await?;
    user_record(db, &user.id, rights, id).await.map(Json)
}

/// A user's badge record, with what `rights` may give them now
pub(crate) async fn user_record(
    db: &Database,
    actor_id: &str,
    rights: BadgeRights,
    id: &str,
) -> Result<v0::AdminUserBadges> {
    let subject = db.fetch_user(id).await?;
    no_bots(&subject)?;
    sync_user_badges(db, &subject.id).await?;
    let now = now_ms();
    let grants = db.fetch_badge_grants(&subject.id).await?;
    let mut eligibility = vec![];
    for definition in BADGE_CATALOGUE.iter().filter(|b| !b.retired) {
        let reason = match check_rule(actor_id, rights, &subject, definition) {
            Err(_) => Some("not allowed for you".to_string()),
            Ok(()) => badge_eligibility(db, &subject.id, definition.id).await?,
        };
        eligibility.push(v0::BadgeEligibility {
            badge: definition.id.to_string(),
            allowed: reason.is_none(),
            reason,
        });
    }
    Ok(v0::AdminUserBadges {
        user: accounts_for(db, [subject.id.clone()])
            .await?
            .pop()
            .ok_or_else(|| create_error!(NotFound))?,
        grants: grants.into_iter().map(|g| admin_grant_view(g, now)).collect(),
        log: db
            .fetch_badge_logs(&subject.id)
            .await?
            .into_iter()
            .map(|e| v0::BadgeLogEntry {
                id: e.id,
                badge: e.badge,
                actor_id: e.actor_id,
                action: format!("{:?}", e.action),
                reason: e.reason,
                created_at: e.created_at,
            })
            .collect(),
        eligibility,
    })
}

/// # Grant a badge
///
/// Staff only, never to yourself, never to a bot. Staff (and the old
/// official badges) need a privileged account; Plus, Creator and the Beta
/// badges can't be granted by hand.
#[openapi(tag = "Badges")]
#[post("/admin/grant", data = "<data>")]
pub async fn admin_grant(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataGrantBadge>,
) -> Result<Json<v0::AdminBadgeGrant>> {
    no_bots(&user)?;
    let rights = rights_of(db, &user).await?;
    grant_with(db, &user.id, rights, data.into_inner()).await.map(Json)
}

/// Grant a hand-given badge (rules checked against `rights`)
pub(crate) async fn grant_with(
    db: &Database,
    actor_id: &str,
    rights: BadgeRights,
    data: v0::DataGrantBadge,
) -> Result<v0::AdminBadgeGrant> {
    let data = validated(data)?;
    let definition = badge_definition(&data.badge).ok_or_else(|| create_error!(NotFound))?;
    let subject = db.fetch_user(&data.user).await?;
    check_rule(actor_id, rights, &subject, definition)?;
    if definition.retired {
        return Err(create_error!(InvalidOperation));
    }
    if let Some(reason) = badge_eligibility(db, &subject.id, definition.id).await? {
        return Err(create_error!(FailedValidation { error: reason }));
    }
    let grant = grant_badge(
        db,
        &subject.id,
        definition.id,
        BadgeSource::Admin,
        None,
        actor_id,
        Some(data.reason.trim().to_string()),
        data.expires_at,
    )
    .await?;
    Ok(admin_grant_view(grant, now_ms()))
}

/// # Revoke a badge
#[openapi(tag = "Badges")]
#[post("/admin/revoke", data = "<data>")]
pub async fn admin_revoke(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataRevokeBadge>,
) -> Result<Json<v0::AdminBadgeGrant>> {
    no_bots(&user)?;
    let rights = rights_of(db, &user).await?;
    revoke_with(db, &user.id, rights, data.into_inner()).await.map(Json)
}

/// Revoke a hand-given badge (rules checked against `rights`)
pub(crate) async fn revoke_with(
    db: &Database,
    actor_id: &str,
    rights: BadgeRights,
    data: v0::DataRevokeBadge,
) -> Result<v0::AdminBadgeGrant> {
    let data = validated(data)?;
    let definition = badge_definition(&data.badge).ok_or_else(|| create_error!(NotFound))?;
    let subject = db.fetch_user(&data.user).await?;
    check_rule(actor_id, rights, &subject, definition)?;
    let grant = revoke_badge(
        db,
        &subject.id,
        definition.id,
        actor_id,
        Some(data.reason.trim().to_string()),
    )
    .await?;
    Ok(admin_grant_view(grant, now_ms()))
}
