//! Plugins and community designs in the admin console, read-only: what
//! people published, with its status, size and reports. Changing anything
//! still goes through the existing review routes (`plugins.review`,
//! `cosmetics.*`); nothing here edits or approves.
//!
//! - `platform.users.read`: see plugins and designs

use revolt_database::{
    CommunityItemKind, CommunityItemQuery, CommunityItemStatus, Database, PluginQuery,
    PluginStatus, StaffPermission,
};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use serde::Serialize;

use super::StaffContext;
use crate::routes::plugins::PluginsOpen;

const PAGE: usize = 50;

async fn usernames(db: &Database, ids: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut ids = ids;
    ids.sort();
    ids.dedup();
    Ok(db
        .fetch_users(&ids)
        .await?
        .into_iter()
        .map(|u| (u.id, format!("{}#{}", u.username, u.discriminator)))
        .collect())
}

fn name_of(names: &[(String, String)], id: &str) -> Option<String> {
    names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone())
}

fn bad_query(q: Option<&str>) -> Result<()> {
    if q.is_some_and(|q| q.chars().count() > 64) {
        return Err(create_error!(FailedValidation {
            error: "query too long".to_string()
        }));
    }
    Ok(())
}

// ---- plugins ----

#[derive(Serialize)]
pub struct PluginRow {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub category: String,
    pub publisher_id: String,
    pub publisher: Option<String>,
    pub status: String,
    pub official: bool,
    pub latest_version: Option<String>,
    pub install_count: i64,
    pub reports: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

fn plugin_status(name: &str) -> Option<PluginStatus> {
    match name {
        "Draft" => Some(PluginStatus::Draft),
        "Listed" => Some(PluginStatus::Listed),
        "Suspended" => Some(PluginStatus::Suspended),
        "Removed" => Some(PluginStatus::Removed),
        _ => None,
    }
}

/// # Plugins
///
/// Newest first; `status` is Listed, Draft, Suspended, Removed or all.
#[get("/plugins?<status>&<q>&<reported>&<offset>")]
pub async fn plugins(
    _open: PluginsOpen,
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    q: Option<&str>,
    reported: Option<bool>,
    offset: Option<usize>,
) -> Result<Json<Vec<PluginRow>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    bad_query(q)?;
    let statuses = match status {
        None | Some("all") => vec![],
        Some(name) => vec![plugin_status(name).ok_or_else(|| create_error!(InvalidOperation))?],
    };
    let list = db
        .fetch_plugins(PluginQuery {
            search: q.map(str::trim).filter(|q| !q.is_empty()),
            statuses,
            reported: reported.unwrap_or(false),
            limit: PAGE,
            offset: offset.unwrap_or(0).min(10_000),
            ..Default::default()
        })
        .await?;
    let names = usernames(db, list.iter().map(|p| p.publisher_id.clone()).collect()).await?;
    Ok(Json(
        list.into_iter()
            .map(|p| PluginRow {
                publisher: name_of(&names, &p.publisher_id),
                status: format!("{:?}", p.status),
                reports: p.reports.len(),
                id: p.id,
                name: p.name,
                summary: p.summary,
                category: p.category,
                publisher_id: p.publisher_id,
                official: p.official,
                latest_version: p.latest_version,
                install_count: p.install_count,
                created_at: p.created_at,
                updated_at: p.updated_at,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct ReleaseRow {
    pub id: String,
    pub version: String,
    pub status: String,
    pub size: i64,
    pub permissions: Vec<String>,
    pub risk: String,
    pub platforms: Vec<String>,
    pub changelog: Option<String>,
    /// Automatic scan findings
    pub findings: usize,
    pub submitted_at: i64,
    pub reviewed_at: Option<i64>,
    pub review_note: Option<String>,
}

#[derive(Serialize)]
pub struct PluginDetail {
    #[serde(flatten)]
    pub plugin: PluginRow,
    pub status_reason: Option<String>,
    /// Newest first; the code itself is never sent here
    pub releases: Vec<ReleaseRow>,
}

/// # One plugin
#[get("/plugins/<id>")]
pub async fn plugin(_open: PluginsOpen, db: &State<Database>, staff: StaffContext, id: &str) -> Result<Json<PluginDetail>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    let p = db.fetch_plugin(id).await?;
    let names = usernames(db, vec![p.publisher_id.clone()]).await?;
    let releases = db
        .fetch_plugin_releases(&p.id)
        .await?
        .into_iter()
        .map(|r| ReleaseRow {
            status: format!("{:?}", r.status),
            risk: format!("{:?}", r.risk),
            findings: r.scan.len(),
            id: r.id,
            version: r.version,
            size: r.size,
            permissions: r.permissions,
            platforms: r.platforms,
            changelog: r.changelog,
            submitted_at: r.submitted_at,
            reviewed_at: r.reviewed_at,
            review_note: r.review_note,
        })
        .collect();
    Ok(Json(PluginDetail {
        status_reason: p.status_reason.clone(),
        plugin: PluginRow {
            publisher: name_of(&names, &p.publisher_id),
            status: format!("{:?}", p.status),
            reports: p.reports.len(),
            id: p.id,
            name: p.name,
            summary: p.summary,
            category: p.category,
            publisher_id: p.publisher_id,
            official: p.official,
            latest_version: p.latest_version,
            install_count: p.install_count,
            created_at: p.created_at,
            updated_at: p.updated_at,
        },
        releases,
    }))
}

// ---- community designs ----

#[derive(Serialize)]
pub struct DesignRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub creator_id: String,
    pub creator: Option<String>,
    pub status: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub revision: u32,
    pub published_at: Option<i64>,
    pub popularity: i64,
    pub reports: usize,
    /// A newer revision is waiting for review
    pub pending_revision: bool,
    pub review_note: Option<String>,
}

fn design_status(name: &str) -> Option<CommunityItemStatus> {
    match name {
        "Draft" => Some(CommunityItemStatus::Draft),
        "PendingReview" => Some(CommunityItemStatus::PendingReview),
        "ChangesRequested" => Some(CommunityItemStatus::ChangesRequested),
        "Published" => Some(CommunityItemStatus::Published),
        "Rejected" => Some(CommunityItemStatus::Rejected),
        "Suspended" => Some(CommunityItemStatus::Suspended),
        "Archived" => Some(CommunityItemStatus::Archived),
        _ => None,
    }
}

/// # Community designs
///
/// Newest first; `status` is Published, PendingReview, Draft,
/// ChangesRequested, Rejected, Suspended, Archived or all; `kind` is
/// Decoration, Nameplate or Effect.
#[get("/designs?<status>&<kind>&<q>&<reported>&<offset>")]
pub async fn designs(
    db: &State<Database>,
    staff: StaffContext,
    status: Option<&str>,
    kind: Option<&str>,
    q: Option<&str>,
    reported: Option<bool>,
    offset: Option<usize>,
) -> Result<Json<Vec<DesignRow>>> {
    staff.require(db, StaffPermission::UsersRead).await?;
    bad_query(q)?;
    let statuses = match status {
        None | Some("all") => vec![],
        Some(name) => vec![design_status(name).ok_or_else(|| create_error!(InvalidOperation))?],
    };
    let kind = match kind {
        None | Some("all") => None,
        Some("Decoration") => Some(CommunityItemKind::Decoration),
        Some("Nameplate") => Some(CommunityItemKind::Nameplate),
        Some("Effect") => Some(CommunityItemKind::Effect),
        _ => return Err(create_error!(InvalidOperation)),
    };
    let list = db
        .fetch_community_items(CommunityItemQuery {
            kind,
            search: q.map(str::trim).filter(|q| !q.is_empty()),
            statuses,
            reported: reported.unwrap_or(false),
            limit: PAGE,
            offset: offset.unwrap_or(0).min(10_000),
            ..Default::default()
        })
        .await?;
    let names = usernames(db, list.iter().map(|d| d.creator_id.clone()).collect()).await?;
    Ok(Json(
        list.into_iter()
            .map(|d| DesignRow {
                creator: name_of(&names, &d.creator_id),
                kind: format!("{:?}", d.kind),
                status: format!("{:?}", d.status),
                reports: d.reports.len(),
                pending_revision: d.pending.is_some(),
                id: d.id,
                name: d.name,
                creator_id: d.creator_id,
                description: d.description,
                tags: d.tags,
                revision: d.revision,
                published_at: d.published_at,
                popularity: d.popularity,
                review_note: d.review_note,
            })
            .collect(),
    ))
}
