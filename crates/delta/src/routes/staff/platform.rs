//! Platform statistics, user and server directories, and system health for
//! the admin console. Read-only, except ending a user's sessions. Counts
//! use the `_id` index (ULID ids carry their creation time) or estimated
//! collection sizes, never a scan of messages.

use std::sync::OnceLock;
use std::time::Instant;

use redis_kiss::{get_connection, AsyncCommands};
use revolt_database::{
    record_platform_audit, staff_now, AuditOutcome, Channel, Database, Server, StaffCounted,
    StaffDbStats, StaffPermission, User,
};
use revolt_models::v0::{SafetyTargetKind, UserFlags};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Serialize;
use ulid::Ulid;

use super::{reason, DataReason, StaffContext};

static STARTED_AT: OnceLock<i64> = OnceLock::new();

/// Remember when the API started (called when routes are mounted)
pub fn mark_started() {
    STARTED_AT.get_or_init(staff_now);
}

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;

/// The smallest ULID created at `ms`: `_id >= ulid_at(t)` means "created at or after t"
fn ulid_at(ms: i64) -> String {
    Ulid::from_parts(ms.max(0) as u64, 0).to_string()
}

/// Creation time of a ULID id
fn created_at(id: &str) -> Option<i64> {
    Ulid::from_string(id).ok().map(|u| u.timestamp_ms() as i64)
}

/// `YYYY-MM-DD` of a day number since 1970-01-01 (proleptic Gregorian)
fn utc_date(days: i64) -> String {
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod date_test {
    #[test]
    fn utc_dates() {
        assert_eq!(super::utc_date(0), "1970-01-01");
        assert_eq!(super::utc_date(20_392), "2025-10-31");
        assert_eq!(super::utc_date(11_016), "2000-02-29");
    }
}

async fn count_since(db: &Database, what: StaffCounted, since: i64) -> Result<u64> {
    db.staff_count(what, Some(&ulid_at(since)), None).await
}

/// Distinct users with at least one live connection (Redis `online` set)
async fn online_users() -> Option<u64> {
    let conn = get_connection().await.ok()?;
    let mut conn = conn.into_inner();
    conn.scard::<_, u64>("online").await.ok()
}

// ---- dashboard ----

#[derive(Serialize)]
pub struct CountWindow {
    pub total: u64,
    pub last_24h: u64,
    pub last_7d: u64,
    pub last_30d: u64,
}

#[derive(Serialize)]
pub struct KindCount {
    pub kind: String,
    pub count: u64,
}

#[derive(Serialize)]
pub struct PlatformStats {
    pub generated_at: i64,
    /// All windows are rolling, in UTC
    pub users: CountWindow,
    /// `None` when the presence store can't be reached
    pub online_users: Option<u64>,
    pub servers: CountWindow,
    pub channels_total: u64,
    pub channels_by_type: Vec<KindCount>,
    pub messages_last_hour: u64,
    pub messages_last_24h: u64,
    /// Only for staff who may read reports
    pub open_reports: Option<u64>,
}

async fn window(db: &Database, what: StaffCounted, now: i64) -> Result<CountWindow> {
    Ok(CountWindow {
        total: db.staff_count(what, None, None).await?,
        last_24h: count_since(db, what, now - DAY).await?,
        last_7d: count_since(db, what, now - 7 * DAY).await?,
        last_30d: count_since(db, what, now - 30 * DAY).await?,
    })
}

/// # Platform statistics
#[get("/stats")]
pub async fn stats(db: &State<Database>, staff: StaffContext) -> Result<Json<PlatformStats>> {
    staff.require(db, StaffPermission::DashboardRead).await?;
    let now = staff_now();
    let channels_by_type: Vec<KindCount> = db
        .staff_channel_types()
        .await?
        .into_iter()
        .map(|(kind, count)| KindCount { kind, count })
        .collect();
    Ok(Json(PlatformStats {
        generated_at: now,
        users: window(db, StaffCounted::Users, now).await?,
        online_users: online_users().await,
        servers: window(db, StaffCounted::Servers, now).await?,
        channels_total: channels_by_type.iter().map(|k| k.count).sum(),
        channels_by_type,
        messages_last_hour: count_since(db, StaffCounted::Messages, now - HOUR).await?,
        messages_last_24h: count_since(db, StaffCounted::Messages, now - DAY).await?,
        open_reports: if staff.has(StaffPermission::ReportsRead) {
            Some(db.staff_open_safety_cases().await?)
        } else {
            None
        },
    }))
}

#[derive(Serialize)]
pub struct DayCount {
    /// UTC day, `YYYY-MM-DD`
    pub day: String,
    pub start: i64,
    pub count: u64,
}

/// # New users, servers or messages per UTC day
#[get("/stats/daily?<metric>&<days>")]
pub async fn daily(
    db: &State<Database>,
    staff: StaffContext,
    metric: &str,
    days: Option<u32>,
) -> Result<Json<Vec<DayCount>>> {
    staff.require(db, StaffPermission::AnalyticsRead).await?;
    let what = match metric {
        "users" => StaffCounted::Users,
        "servers" => StaffCounted::Servers,
        "messages" => StaffCounted::Messages,
        _ => {
            return Err(create_error!(FailedValidation {
                error: "metric must be users, servers or messages".to_string()
            }))
        }
    };
    let days = days.unwrap_or(30).clamp(1, 90) as i64;
    let today = staff_now() / DAY * DAY;
    let mut out = Vec::with_capacity(days as usize);
    for i in (0..days).rev() {
        let start = today - i * DAY;
        let count = db
            .staff_count(what, Some(&ulid_at(start)), Some(&ulid_at(start + DAY)))
            .await?;
        out.push(DayCount {
            day: utc_date(start / DAY),
            start,
            count,
        });
    }
    Ok(Json(out))
}

// ---- users ----

#[derive(Serialize)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub discriminator: String,
    pub display_name: Option<String>,
    pub created_at: Option<i64>,
    pub bot: bool,
    pub suspended: bool,
    pub banned: bool,
    pub deleted: bool,
    /// Old upstream flag; grants nothing in the console
    pub privileged: bool,
    /// Official Zrzur account
    pub official: bool,
    /// Only with `platform.users.pii.read`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

fn user_row(user: &User) -> UserRow {
    let flags = user.flags.unwrap_or_default();
    UserRow {
        id: user.id.clone(),
        username: user.username.clone(),
        discriminator: user.discriminator.clone(),
        display_name: user.display_name.clone(),
        created_at: created_at(&user.id),
        bot: user.bot.is_some(),
        suspended: flags & UserFlags::SuspendedUntil as i32 != 0,
        banned: flags & UserFlags::Banned as i32 != 0,
        deleted: flags & UserFlags::Deleted as i32 != 0,
        privileged: user.privileged,
        official: user.official,
        email: None,
    }
}

fn limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(50).clamp(1, 100)
}

/// # User directory
///
/// Newest first (`before` is a user id cursor), or a search by id, username
/// or display name. Emails only with `platform.users.pii.read`.
#[get("/users?<q>&<before>&<limit>")]
pub async fn users(
    db: &State<Database>,
    staff: StaffContext,
    q: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Json<Vec<UserRow>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let limit = self::limit(limit);
    let found = match q.map(str::trim).filter(|q| !q.is_empty()) {
        Some(query) => {
            if query.chars().count() > 64 {
                return Err(create_error!(FailedValidation {
                    error: "search is limited to 64 characters".to_string()
                }));
            }
            db.search_users_by_name(query.trim_start_matches('@'), limit.min(50))
                .await?
        }
        None => db.staff_list_users(before.filter(|b| b.len() == 26), limit).await?,
    };
    let pii = staff.has(StaffPermission::UsersPiiRead);
    let mut rows = Vec::with_capacity(found.len());
    for user in &found {
        let mut row = user_row(user);
        if pii && user.bot.is_none() {
            row.email = db.fetch_account(&user.id).await.ok().map(|a| a.email);
        }
        rows.push(row);
    }
    if pii && !rows.is_empty() {
        let mut record = staff.record("staff.user.list_pii", Some(StaffPermission::UsersPiiRead));
        record.summary = Some(format!("{} users with email", rows.len()));
        record_platform_audit(db, record, AuditOutcome::Success).await?;
    }
    Ok(Json(rows))
}

#[derive(Serialize)]
pub struct UserAccountState {
    pub verified: bool,
    pub disabled: bool,
    pub mfa_enabled: bool,
    pub deletion_pending: bool,
}

#[derive(Serialize)]
pub struct UserDetail {
    #[serde(flatten)]
    pub user: UserRow,
    pub online: bool,
    pub servers: usize,
    pub sessions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<UserAccountState>,
    pub reports_about: usize,
    pub suspended_until: Option<String>,
}

/// # One user
#[get("/users/<id>")]
pub async fn user(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<UserDetail>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let user = db.fetch_user(id).await?;
    let mut row = user_row(&user);
    let account = if user.bot.is_none() {
        db.fetch_account(&user.id).await.ok()
    } else {
        None
    };
    if staff.has(StaffPermission::UsersPiiRead) {
        row.email = account.as_ref().map(|a| a.email.clone());
        if row.email.is_some() {
            let mut record = staff.record("staff.user.view_pii", Some(StaffPermission::UsersPiiRead));
            record.target_kind = Some("user".to_string());
            record.target_id = Some(user.id.clone());
            record_platform_audit(db, record, AuditOutcome::Success).await?;
        }
    }
    Ok(Json(UserDetail {
        online: revolt_presence::is_online(&user.id).await,
        servers: db.fetch_server_count(&user.id).await.unwrap_or(0),
        sessions: db.fetch_sessions(&user.id).await.map(|s| s.len()).unwrap_or(0),
        account: account.map(|a| UserAccountState {
            verified: matches!(a.verification, revolt_database::EmailVerification::Verified),
            disabled: a.disabled,
            mfa_enabled: a.mfa.is_active(),
            deletion_pending: a.deletion.is_some(),
        }),
        reports_about: db
            .fetch_safety_cases_for_target(SafetyTargetKind::User, &user.id, 100)
            .await
            .map(|c| c.len())
            .unwrap_or(0),
        suspended_until: user.suspended_until.map(|t| t.format().to_string()),
        user: row,
    }))
}

/// # Sign a user out everywhere
///
/// For a compromised account. Ends every session of the user (and their
/// staff sessions). An Owner's sessions can only be ended by an Owner.
#[post("/users/<id>/revoke-sessions", data = "<data>")]
pub async fn revoke_user_sessions(
    db: &State<Database>,
    staff: StaffContext,
    id: &str,
    data: Json<DataReason>,
) -> Result<Json<usize>> {
    staff
        .require(db, StaffPermission::UsersSessionsRevoke)
        .await?;
    let reason = reason(&data.reason)?;
    let user = db.fetch_user(id).await?;
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let mut record = staff.record("staff.user.revoke_sessions", Some(StaffPermission::UsersSessionsRevoke));
    record.target_kind = Some("user".to_string());
    record.target_id = Some(user.id.clone());
    record.reason = Some(reason.clone());
    let target_is_owner = db
        .fetch_staff_grant(&user.id)
        .await?
        .is_some_and(|g| g.is_owner(staff.now));
    let actor_is_owner = staff.grant.is_owner(staff.now);
    if target_is_owner && !actor_is_owner {
        record_platform_audit(db, record, AuditOutcome::Denied).await?;
        return Err(create_error!(NotPrivileged));
    }
    let count = db.fetch_sessions(&user.id).await.map(|s| s.len()).unwrap_or(0);
    // fail closed: the record comes first
    record.summary = Some(format!("{count} sessions"));
    record_platform_audit(db, record, AuditOutcome::Success).await?;
    let account = db.fetch_account(&user.id).await?;
    account.delete_all_sessions(db, None).await?;
    db.revoke_staff_sessions_for_user(&user.id, staff.now).await?;
    crate::util::official::sessions_ended(db, &user.id, &reason).await;
    Ok(Json(count))
}

// ---- servers ----

#[derive(Serialize)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub owner_username: Option<String>,
    pub created_at: Option<i64>,
    pub channels: usize,
    pub roles: usize,
    pub nsfw: bool,
    pub flags: i32,
}

fn server_row(server: &Server, owners: &[User]) -> ServerRow {
    ServerRow {
        id: server.id.clone(),
        name: server.name.clone(),
        owner_id: server.owner.clone(),
        owner_username: owners
            .iter()
            .find(|u| u.id == server.owner)
            .map(|u| format!("{}#{}", u.username, u.discriminator)),
        created_at: created_at(&server.id),
        channels: server.channels.len(),
        roles: server.roles.len(),
        nsfw: server.nsfw,
        flags: server.flags.unwrap_or_default(),
    }
}

/// # Server directory
#[get("/servers?<q>&<before>&<limit>")]
pub async fn servers(
    db: &State<Database>,
    staff: StaffContext,
    q: Option<&str>,
    before: Option<&str>,
    limit: Option<usize>,
) -> Result<Json<Vec<ServerRow>>> {
    staff.require(db, StaffPermission::ServersRead).await?;
    if q.is_some_and(|q| q.chars().count() > 64) {
        return Err(create_error!(FailedValidation {
            error: "search is limited to 64 characters".to_string()
        }));
    }
    let list = db
        .staff_list_servers(q, before.filter(|b| b.len() == 26), self::limit(limit))
        .await?;
    let owner_ids: Vec<String> = list.iter().map(|s| s.owner.clone()).collect();
    let owners = db.fetch_users(&owner_ids).await.unwrap_or_default();
    Ok(Json(list.iter().map(|s| server_row(s, &owners)).collect()))
}

#[derive(Serialize)]
pub struct ServerDetail {
    #[serde(flatten)]
    pub server: ServerRow,
    pub description: Option<String>,
    pub members: usize,
    pub channels_by_type: Vec<KindCount>,
    pub reports_about: usize,
    /// 1 official, 2 verified, otherwise no mark
    pub flags: i32,
    /// Marked 18+
    pub nsfw: bool,
}

fn channel_kind(channel: &Channel) -> &'static str {
    match channel {
        Channel::TextChannel { .. } if channel.voice().is_some() => "VoiceChannel",
        Channel::TextChannel { .. } => "TextChannel",
        _ => "Other",
    }
}

/// # One server
///
/// Metadata only: no messages or member lists.
#[get("/servers/<id>")]
pub async fn server(db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<ServerDetail>> {
    staff.require(db, StaffPermission::ServersRead).await?;
    let server = db.fetch_server(id).await?;
    let owners = db.fetch_users(std::slice::from_ref(&server.owner)).await.unwrap_or_default();
    let channels = db.fetch_channels(&server.channels).await.unwrap_or_default();
    let mut kinds = std::collections::BTreeMap::<&str, u64>::new();
    for channel in &channels {
        *kinds.entry(channel_kind(channel)).or_default() += 1;
    }
    Ok(Json(ServerDetail {
        members: db.fetch_member_count(&server.id).await.unwrap_or(0),
        channels_by_type: kinds
            .into_iter()
            .map(|(kind, count)| KindCount { kind: kind.to_string(), count })
            .collect(),
        reports_about: db
            .fetch_safety_cases_for_target(SafetyTargetKind::Server, &server.id, 100)
            .await
            .map(|c| c.len())
            .unwrap_or(0),
        description: server.description.clone(),
        flags: server.flags.unwrap_or(0),
        nsfw: server.nsfw,
        server: server_row(&server, &owners),
    }))
}

// ---- health ----

#[derive(Serialize)]
pub struct ServiceHealth {
    pub ok: bool,
    pub latency_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct SystemHealth {
    pub generated_at: i64,
    pub api_version: &'static str,
    pub started_at: Option<i64>,
    pub database: ServiceHealth,
    pub database_stats: Option<StaffDbStats>,
    pub presence: ServiceHealth,
    pub online_users: Option<u64>,
}

/// # System health
///
/// API, database (with sizes) and the presence store. No hosts, keys or
/// configuration values.
#[get("/health")]
pub async fn health(db: &State<Database>, staff: StaffContext) -> Result<Json<SystemHealth>> {
    staff.require(db, StaffPermission::OperationsRead).await?;
    let start = Instant::now();
    let stats = db.staff_db_stats().await.ok();
    let db_ms = start.elapsed().as_millis() as u64;
    let start = Instant::now();
    let online = online_users().await;
    let redis_ms = start.elapsed().as_millis() as u64;
    Ok(Json(SystemHealth {
        generated_at: staff_now(),
        api_version: env!("CARGO_PKG_VERSION"),
        started_at: STARTED_AT.get().copied(),
        database: ServiceHealth {
            ok: stats.is_some(),
            latency_ms: stats.is_some().then_some(db_ms),
        },
        database_stats: stats,
        presence: ServiceHealth {
            ok: online.is_some(),
            latency_ms: online.is_some().then_some(redis_ms),
        },
        online_users: online,
    }))
}
