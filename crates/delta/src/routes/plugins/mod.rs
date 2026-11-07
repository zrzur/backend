//! Zrzur plugins: store, developer submissions, review and installs.
//!
//! Every route checks who may act itself; a hidden button in the client
//! is not security. Publishing a plugin never makes it installable: a
//! reviewer with `plugins.review` has to approve each release, and code
//! from third-party publishers is refused until the sandbox has had an
//! external review (`PLUGIN_THIRD_PARTY_CODE_ENABLED`).
//!
//! Plugins never get a session or bot token from these routes; the client
//! host keeps them out of the sandbox.
//!
//! `features.plugins_enabled` (off by default) closes all of this, the
//! `/staff` plugin routes included: every route takes [`PluginsOpen`] as its
//! first guard, so nothing is read or written and no sign-in is asked for.
//! Stored plugins and installs stay as they are for when it is turned on.

use std::collections::{BTreeSet, HashMap};

use iso8601_timestamp::Timestamp;
use revolt_database::{
    clean_plugin_settings, compare_plugin_versions, counted_plugin_reports, epoch_ms, platform_permissions,
    plugin_release_transition, plugin_runnable, scan_plugin_package, sha256_hex,
    validate_plugin_manifest, Database, PlatformPermission, Plugin, PluginGrant, PluginInstall,
    PluginPublisher, PluginPublisherStatus, PluginQuery, PluginRelease, PluginReleaseStatus,
    PluginReviewAction, PluginReviewLog, PluginRisk, PluginScanFinding, PluginSettings,
    PluginStatus, User, PENDING_RELEASES_PER_PLUGIN, PLUGINS_PER_PUBLISHER,
    PLUGIN_INSTALLS_PER_USER, PLUGIN_OFFICIAL_PREFIX, PLUGIN_OFFICIAL_PREFIX_LEGACY, PLUGIN_REPORTS_TO_SUSPEND,
    PLUGIN_THIRD_PARTY_CODE_ENABLED,
};
use revolt_result::{create_error, Error, Result};
use revolt_rocket_okapi::{
    r#gen::OpenApiGenerator,
    request::{OpenApiFromRequest, RequestHeaderInput},
    revolt_okapi::openapi3::OpenApi,
};
use rocket::{
    http::Status,
    request::{FromRequest, Outcome},
    serde::json::Json,
    Request, Route, State,
};
use rocket_empty::EmptyResponse;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        access,
        list,
        details,
        log,
        create,
        validate,
        projects,
        submit,
        withdraw,
        review_queue,
        approve,
        reject,
        revoke,
        suspend,
        reinstate,
        remove,
        report,
        verify_publisher,
        suspend_publisher,
        reinstate_publisher,
        installs,
        install,
        set_enabled,
        uninstall,
        save_settings
    ]
}

/// Passes only while `features.plugins_enabled` is on. Taken first on every
/// plugin route, so it answers before the session and the body are looked at.
pub struct PluginsOpen;

#[rocket::async_trait]
impl<'r> FromRequest<'r> for PluginsOpen {
    type Error = Error;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        if revolt_config::config().await.features.plugins_enabled {
            return Outcome::Success(PluginsOpen);
        }
        let refused = create_error!(PluginsDisabled);
        // Rocket hands catchers only the status; this is what the 403 catcher answers
        request.local_cache(|| revolt_result::rocket::GuardRefusal(Some(refused.clone())));
        Outcome::Error((Status::Forbidden, refused))
    }
}

impl<'r> OpenApiFromRequest<'r> for PluginsOpen {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

/// Actor id used when the system acts (reports reaching the limit)
const SYSTEM_ACTOR: &str = "00000000000000000000000000";

fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

fn invalid(error: impl Into<String>) -> revolt_result::Error {
    create_error!(FailedValidation {
        error: error.into()
    })
}

fn status_name<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}")
}

async fn is_reviewer(db: &Database, user: &User) -> Result<bool> {
    Ok(platform_permissions(db, user)
        .await?
        .has(PlatformPermission::PluginsReview))
}

async fn require_reviewer(db: &Database, user: &User) -> Result<()> {
    if is_reviewer(db, user).await? {
        Ok(())
    } else {
        Err(create_error!(MissingPlatformPermission {
            permission: "plugins.review".to_string()
        }))
    }
}

/// Bots and suspended, banned or deleted accounts can't publish
fn account_in_good_standing(user: &User) -> bool {
    user.bot.is_none() && user.flags.unwrap_or(0) & (1 | 2 | 4 | 8) == 0
}

async fn publisher_of(db: &Database, user_id: &str) -> Result<PluginPublisher> {
    Ok(db
        .fetch_plugin_publisher(user_id)
        .await?
        .unwrap_or(PluginPublisher {
            id: user_id.to_string(),
            status: PluginPublisherStatus::Active,
            verified: false,
            reason: None,
            updated_at: 0,
            updated_by: SYSTEM_ACTOR.to_string(),
        }))
}

async fn require_publisher(db: &Database, user: &User) -> Result<PluginPublisher> {
    if !account_in_good_standing(user) {
        return Err(create_error!(IsBot));
    }
    let publisher = publisher_of(db, &user.id).await?;
    if publisher.status == PluginPublisherStatus::Suspended {
        return Err(create_error!(MissingPlatformPermission {
            permission: "plugins.publish".to_string()
        }));
    }
    Ok(publisher)
}

async fn audit(
    db: &Database,
    plugin_id: &str,
    release_id: Option<&str>,
    actor: &str,
    action: PluginReviewAction,
    reason: Option<String>,
) -> Result<()> {
    db.insert_plugin_log(&PluginReviewLog {
        id: Ulid::new().to_string(),
        plugin_id: plugin_id.to_string(),
        release_id: release_id.map(str::to_string),
        actor_id: actor.to_string(),
        action,
        reason,
        created_at: now_ms(),
    })
    .await
}

/// Save a changed plugin if nobody changed it since `before` was read
async fn save_plugin(db: &Database, before: &Plugin, mut after: Plugin) -> Result<Plugin> {
    after.version = before.version + 1;
    after.updated_at = now_ms();
    if !db.update_plugin(&after, before.version).await? {
        return Err(invalid("the plugin was changed by someone else; reload and try again"));
    }
    Ok(after)
}

fn clean_reason(reason: Option<String>, required: bool) -> Result<Option<String>> {
    let reason = reason
        .map(|r| r.trim().chars().take(1000).collect::<String>())
        .filter(|r| !r.is_empty());
    if required && reason.is_none() {
        return Err(invalid("a reason is required"));
    }
    Ok(reason)
}

// ---------------------------------------------------------------- views

/// # Plugin listing
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginCard {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub summary: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Draft, Listed, Suspended or Removed
    pub status: String,
    pub official: bool,
    pub publisher_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<String>,
    pub install_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Reviewers and the publisher only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reports: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

fn card(plugin: &Plugin, privileged_view: bool) -> PluginCard {
    PluginCard {
        id: plugin.id.clone(),
        name: plugin.name.clone(),
        summary: plugin.summary.clone(),
        category: plugin.category.clone(),
        icon: plugin.icon.clone(),
        status: status_name(&plugin.status),
        official: plugin.official,
        publisher_id: plugin.publisher_id.clone(),
        latest_version: plugin.latest_version.clone(),
        latest_release: plugin.latest_release.clone(),
        install_count: plugin.install_count,
        created_at: plugin.created_at,
        updated_at: plugin.updated_at,
        reports: privileged_view.then_some(plugin.reports.len()),
        status_reason: privileged_view.then(|| plugin.status_reason.clone()).flatten(),
    }
}

/// # Who publishes a plugin
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PublisherCard {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: String,
    pub discriminator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// A reviewer confirmed who this is
    pub verified: bool,
    pub suspended: bool,
}

async fn publishers_for(
    db: &Database,
    ids: impl IntoIterator<Item = String>,
) -> Result<Vec<PublisherCard>> {
    let ids: Vec<String> = ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let mut cards = vec![];
    for user in db.fetch_users(&ids).await? {
        let publisher = publisher_of(db, &user.id).await?;
        cards.push(PublisherCard {
            id: user.id.clone(),
            username: user.username.clone(),
            discriminator: user.discriminator.clone(),
            display_name: user.display_name.clone(),
            verified: publisher.verified,
            suspended: publisher.status == PluginPublisherStatus::Suspended,
        });
    }
    Ok(cards)
}

/// # Scanner finding
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ScanFinding {
    pub rule: String,
    pub block: bool,
    pub message: String,
    pub line: u32,
}

/// # One release of a plugin
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ReleaseView {
    #[serde(rename = "_id")]
    pub id: String,
    pub plugin_id: String,
    pub version: String,
    pub manifest: Value,
    /// Worker code; only sent when the caller may run or review it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_sha256: Option<String>,
    pub package_sha256: String,
    pub size: i64,
    pub permissions: Vec<String>,
    /// Low, Medium or High
    pub risk: String,
    /// `declarative` or `worker`
    pub entrypoint: String,
    pub platforms: Vec<String>,
    pub min_app_version: String,
    /// PendingReview, Approved, Rejected, Revoked or Withdrawn
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    pub scan: Vec<ScanFinding>,
    pub submitted_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_note: Option<String>,
}

fn release_view(release: &PluginRelease, with_code: bool, with_review: bool) -> ReleaseView {
    ReleaseView {
        id: release.id.clone(),
        plugin_id: release.plugin_id.clone(),
        version: release.version.clone(),
        manifest: serde_json::from_str(&release.manifest).unwrap_or(Value::Null),
        code: with_code.then(|| release.code.clone()).flatten(),
        code_sha256: release.code_sha256.clone(),
        package_sha256: release.package_sha256.clone(),
        size: release.size,
        permissions: release.permissions.clone(),
        risk: status_name(&release.risk),
        entrypoint: release.entrypoint.clone(),
        platforms: release.platforms.clone(),
        min_app_version: release.min_app_version.clone(),
        status: status_name(&release.status),
        changelog: release.changelog.clone(),
        scan: if with_review {
            release
                .scan
                .iter()
                .map(|f| ScanFinding {
                    rule: f.rule.clone(),
                    block: f.block,
                    message: f.message.clone(),
                    line: f.line,
                })
                .collect()
        } else {
            vec![]
        },
        submitted_at: release.submitted_at,
        reviewed_at: release.reviewed_at,
        review_note: with_review.then(|| release.review_note.clone()).flatten(),
    }
}

/// Code may leave the server only for releases a client is allowed to run
fn code_allowed(plugin: &Plugin) -> bool {
    plugin.official || PLUGIN_THIRD_PARTY_CODE_ENABLED
}

// ---------------------------------------------------------------- access

/// # What you may do with plugins
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginAccess {
    /// Can create plugins and submit releases
    pub can_publish: bool,
    /// Has `plugins.review`
    pub review: bool,
    /// Can create `zrzur.*` official plugins
    pub official: bool,
    pub publisher_verified: bool,
    pub publisher_suspended: bool,
    /// Whether code from third-party publishers is accepted at all
    pub third_party_code: bool,
}

/// # Plugin access
#[openapi(tag = "Plugins")]
#[get("/permissions")]
pub async fn access(_open: PluginsOpen, db: &State<Database>, user: User) -> Result<Json<PluginAccess>> {
    let publisher = publisher_of(db, &user.id).await?;
    let suspended = publisher.status == PluginPublisherStatus::Suspended;
    Ok(Json(PluginAccess {
        can_publish: account_in_good_standing(&user) && !suspended,
        review: is_reviewer(db, &user).await?,
        official: user.privileged,
        publisher_verified: publisher.verified,
        publisher_suspended: suspended,
        third_party_code: PLUGIN_THIRD_PARTY_CODE_ENABLED,
    }))
}

// ---------------------------------------------------------------- store

/// # Store query
#[derive(Deserialize, JsonSchema, FromForm)]
pub struct OptionsPluginQuery {
    /// Search by name or summary
    query: Option<String>,
    /// appearance, productivity, accessibility, notifications, integration, developer
    category: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// # Store page
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginPage {
    pub plugins: Vec<PluginCard>,
    pub publishers: Vec<PublisherCard>,
}

/// # List plugins in the store
///
/// Only listed plugins with an approved release.
#[openapi(tag = "Plugins")]
#[get("/?<options..>")]
pub async fn list(
    _open: PluginsOpen,
    db: &State<Database>,
    _user: User,
    options: OptionsPluginQuery,
) -> Result<Json<PluginPage>> {
    let search = options
        .query
        .as_deref()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .map(|q| q.chars().take(64).collect::<String>());
    let plugins = db
        .fetch_plugins(PluginQuery {
            search: search.as_deref(),
            category: options.category.as_deref(),
            statuses: vec![PluginStatus::Listed],
            limit: options.limit.unwrap_or(50).clamp(1, 100),
            offset: options.offset.unwrap_or(0).min(10_000),
            ..Default::default()
        })
        .await?;
    let publishers =
        publishers_for(db, plugins.iter().map(|p| p.publisher_id.clone())).await?;
    Ok(Json(PluginPage {
        plugins: plugins
            .iter()
            .filter(|p| p.latest_release.is_some())
            .map(|p| card(p, false))
            .collect(),
        publishers,
    }))
}

/// # Plugin details
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginDetails {
    pub plugin: PluginCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<PublisherCard>,
    /// Newest approved release
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseView>,
    /// Approved and revoked releases, newest first (changelog / history)
    pub versions: Vec<ReleaseView>,
    /// Every release, for the publisher and reviewers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_releases: Option<Vec<ReleaseView>>,
    pub can_manage: bool,
    pub can_review: bool,
}

/// # Fetch a plugin
#[openapi(tag = "Plugins")]
#[get("/<id>")]
pub async fn details(_open: PluginsOpen, db: &State<Database>, user: User, id: &str) -> Result<Json<PluginDetails>> {
    let plugin = db.fetch_plugin(id).await?;
    let reviewer = is_reviewer(db, &user).await?;
    let owner = plugin.publisher_id == user.id;
    if !owner && !reviewer && plugin.status != PluginStatus::Listed {
        return Err(create_error!(NotFound));
    }
    let releases = db.fetch_plugin_releases(&plugin.id).await?;
    let release = plugin
        .latest_release
        .as_ref()
        .and_then(|id| releases.iter().find(|r| &r.id == id))
        .map(|r| release_view(r, false, owner || reviewer));
    let versions = releases
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                PluginReleaseStatus::Approved | PluginReleaseStatus::Revoked
            )
        })
        .map(|r| release_view(r, false, owner || reviewer))
        .collect();
    let all_releases = (owner || reviewer).then(|| {
        releases
            .iter()
            .map(|r| release_view(r, reviewer && code_allowed(&plugin), true))
            .collect()
    });
    let publisher = publishers_for(db, [plugin.publisher_id.clone()])
        .await?
        .into_iter()
        .next();
    Ok(Json(PluginDetails {
        plugin: card(&plugin, owner || reviewer),
        publisher,
        release,
        versions,
        all_releases,
        can_manage: owner,
        can_review: reviewer,
    }))
}

/// # Review log entry
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginLogEntry {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_id: Option<String>,
    pub actor_id: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: i64,
}

/// # Review and security log of a plugin
///
/// Its publisher and reviewers only.
#[openapi(tag = "Plugins")]
#[get("/<id>/log")]
pub async fn log(_open: PluginsOpen, db: &State<Database>, user: User, id: &str) -> Result<Json<Vec<PluginLogEntry>>> {
    let plugin = db.fetch_plugin(id).await?;
    if plugin.publisher_id != user.id && !is_reviewer(db, &user).await? {
        return Err(create_error!(NotFound));
    }
    Ok(Json(
        db.fetch_plugin_logs(id)
            .await?
            .into_iter()
            .map(|l| PluginLogEntry {
                id: l.id,
                release_id: l.release_id,
                actor_id: l.actor_id,
                action: status_name(&l.action),
                reason: l.reason,
                created_at: l.created_at,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------- developers

/// # New plugin project
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct DataCreatePlugin {
    /// Manifest id, e.g. `dev.example.focus`
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    pub category: String,
}

/// # Create a plugin project
///
/// Creates a draft owned by you. Nothing is installable until a reviewer
/// approves a release.
#[openapi(tag = "Plugins")]
#[post("/", data = "<data>")]
pub async fn create(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    data: Json<DataCreatePlugin>,
) -> Result<Json<PluginCard>> {
    require_publisher(db, &user).await?;
    let data = data.into_inner();
    let id = data.id.trim().to_string();
    let id_re = regex::Regex::new(r"^[a-z][a-z0-9]*(\.[a-z][a-z0-9-]*){1,4}$").unwrap();
    if !id_re.is_match(&id) || id.len() > 64 {
        return Err(invalid("id must look like `publisher.plugin-name`"));
    }
    let official = id.starts_with(PLUGIN_OFFICIAL_PREFIX) || id.starts_with(PLUGIN_OFFICIAL_PREFIX_LEGACY);
    if official && !user.privileged {
        return Err(invalid("the `zrzur.` prefix is reserved for official plugins"));
    }
    let name = data.name.trim().to_string();
    if !(2..=48).contains(&name.chars().count()) {
        return Err(invalid("name must be 2-48 characters"));
    }
    if ![
        "appearance",
        "productivity",
        "accessibility",
        "notifications",
        "integration",
        "developer",
    ]
    .contains(&data.category.as_str())
    {
        return Err(invalid("unknown category"));
    }
    let owned = db
        .fetch_plugins(PluginQuery {
            publisher_id: Some(&user.id),
            statuses: vec![PluginStatus::Draft, PluginStatus::Listed, PluginStatus::Suspended],
            limit: 100,
            ..Default::default()
        })
        .await?;
    if owned.len() >= PLUGINS_PER_PUBLISHER {
        return Err(invalid(format!(
            "you can own at most {PLUGINS_PER_PUBLISHER} plugins"
        )));
    }
    if db.fetch_plugin(&id).await.is_ok() {
        return Err(invalid("that plugin id is taken"));
    }

    let now = now_ms();
    let plugin = Plugin {
        id: id.clone(),
        publisher_id: user.id.clone(),
        name,
        summary: data
            .summary
            .map(|s| s.trim().chars().take(300).collect())
            .unwrap_or_default(),
        category: data.category,
        icon: None,
        status: PluginStatus::Draft,
        official,
        latest_release: None,
        latest_version: None,
        install_count: 0,
        reports: vec![],
        status_reason: None,
        created_at: now,
        updated_at: now,
        version: 0,
    };
    db.insert_plugin(&plugin)
        .await
        .map_err(|_| invalid("that plugin id is taken"))?;
    audit(db, &id, None, &user.id, PluginReviewAction::Create, None).await?;
    Ok(Json(card(&plugin, true)))
}

/// # Plugin package
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct DataPluginPackage {
    pub manifest: Value,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
}

/// # Validation result
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginValidation {
    pub ok: bool,
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    pub scan: Vec<ScanFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_sha256: Option<String>,
}

/// Everything the server checks about a package, without saving it
fn check_package(package: &DataPluginPackage, official: bool) -> (PluginValidation, Option<revolt_database::ValidatedManifest>) {
    let code = package.code.as_deref().filter(|c| !c.is_empty());
    let manifest_text = serde_json::to_string(&package.manifest).unwrap_or_default();
    let findings = scan_plugin_package(&manifest_text, code);
    let scan: Vec<ScanFinding> = findings
        .iter()
        .map(|f| ScanFinding {
            rule: f.rule.clone(),
            block: f.block,
            message: f.message.clone(),
            line: f.line,
        })
        .collect();
    let mut issues = vec![];
    let validated = match validate_plugin_manifest(&package.manifest, code, official) {
        Ok(v) => Some(v),
        Err(errors) => {
            issues = errors;
            None
        }
    };
    if let Some(v) = &validated {
        if v.entrypoint == "worker" && !official && !PLUGIN_THIRD_PARTY_CODE_ENABLED {
            issues.push(
                "entrypoint: third-party plugin code is not enabled on Zrzur yet; publish a declarative plugin"
                    .to_string(),
            );
        }
    }
    for finding in findings.iter().filter(|f| f.block) {
        issues.push(format!("scan: {} (line {})", finding.message, finding.line));
    }
    let ok = issues.is_empty();
    let mut package_bytes = manifest_text.into_bytes();
    package_bytes.extend(code.unwrap_or_default().as_bytes());
    (
        PluginValidation {
            ok,
            issues,
            risk: validated.as_ref().map(|v| status_name(&v.risk)),
            scan,
            package_sha256: ok.then(|| sha256_hex(&package_bytes)),
        },
        if ok { validated } else { None },
    )
}

/// # Validate a package
///
/// Runs the same checks as a submission and saves nothing.
#[openapi(tag = "Plugins")]
#[post("/validate", data = "<data>")]
pub async fn validate(_open: PluginsOpen, user: User, data: Json<DataPluginPackage>) -> Result<Json<PluginValidation>> {
    Ok(Json(check_package(&data, user.privileged).0))
}

/// # A plugin project with all its releases
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginProject {
    pub plugin: PluginCard,
    pub releases: Vec<ReleaseView>,
}

/// # Your plugin projects
#[openapi(tag = "Plugins")]
#[get("/@me/projects")]
pub async fn projects(_open: PluginsOpen, db: &State<Database>, user: User) -> Result<Json<Vec<PluginProject>>> {
    let plugins = db
        .fetch_plugins(PluginQuery {
            publisher_id: Some(&user.id),
            limit: 100,
            ..Default::default()
        })
        .await?;
    let mut projects = vec![];
    for plugin in plugins {
        let releases = db.fetch_plugin_releases(&plugin.id).await?;
        projects.push(PluginProject {
            plugin: card(&plugin, true),
            releases: releases.iter().map(|r| release_view(r, false, true)).collect(),
        });
    }
    Ok(Json(projects))
}

/// # Submit a release for review
///
/// The package is validated and scanned. Invalid packages are refused with
/// every problem listed; valid ones wait for a reviewer.
#[openapi(tag = "Plugins")]
#[post("/<id>/releases", data = "<data>")]
pub async fn submit(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginPackage>,
) -> Result<Json<ReleaseView>> {
    require_publisher(db, &user).await?;
    let plugin = db.fetch_plugin(id).await?;
    if plugin.publisher_id != user.id {
        return Err(create_error!(NotFound));
    }
    if matches!(plugin.status, PluginStatus::Suspended | PluginStatus::Removed) {
        return Err(create_error!(InvalidOperation));
    }
    let package = data.into_inner();
    let (result, validated) = check_package(&package, plugin.official);
    let Some(validated) = validated else {
        audit(
            db,
            &plugin.id,
            None,
            &user.id,
            PluginReviewAction::AutoReject,
            Some(result.issues.join("\n").chars().take(1000).collect()),
        )
        .await?;
        return Err(invalid(result.issues.join("\n")));
    };
    if validated.id != plugin.id {
        return Err(invalid(format!(
            "id: the manifest says `{}` but this plugin is `{}`",
            validated.id, plugin.id
        )));
    }

    let releases = db.fetch_plugin_releases(&plugin.id).await?;
    if releases
        .iter()
        .filter(|r| r.status == PluginReleaseStatus::PendingReview)
        .count()
        >= PENDING_RELEASES_PER_PLUGIN
    {
        return Err(invalid(format!(
            "at most {PENDING_RELEASES_PER_PLUGIN} releases can wait for review"
        )));
    }
    if let Some(newest) = releases
        .iter()
        .filter(|r| {
            !matches!(
                r.status,
                PluginReleaseStatus::Rejected | PluginReleaseStatus::Withdrawn
            )
        })
        .map(|r| r.version.as_str())
        .max_by(|a, b| compare_plugin_versions(a, b))
    {
        if compare_plugin_versions(&validated.version, newest) != std::cmp::Ordering::Greater {
            return Err(invalid(format!(
                "version: must be newer than {newest}"
            )));
        }
    }

    let code = package.code.filter(|c| !c.is_empty());
    let manifest_text = serde_json::to_string(&package.manifest).unwrap_or_default();
    let size = (manifest_text.len() + code.as_ref().map(|c| c.len()).unwrap_or(0)) as i64;
    let release = PluginRelease {
        id: Ulid::new().to_string(),
        plugin_id: plugin.id.clone(),
        version: validated.version.clone(),
        manifest: manifest_text,
        code_sha256: code.as_ref().map(|c| sha256_hex(c.as_bytes())),
        code,
        package_sha256: result.package_sha256.clone().unwrap_or_default(),
        size,
        permissions: validated.permissions.clone(),
        risk: validated.risk.clone(),
        entrypoint: validated.entrypoint.clone(),
        platforms: validated.platforms.clone(),
        min_app_version: validated.min_app_version.clone(),
        status: PluginReleaseStatus::PendingReview,
        changelog: package
            .changelog
            .map(|c| c.trim().chars().take(2000).collect::<String>())
            .filter(|c| !c.is_empty()),
        // every finding (manifest and code) is kept for the reviewer
        scan: result
            .scan
            .iter()
            .map(|f| PluginScanFinding {
                rule: f.rule.clone(),
                block: f.block,
                message: f.message.clone(),
                line: f.line,
            })
            .collect(),
        submitted_by: user.id.clone(),
        submitted_at: now_ms(),
        reviewed_by: None,
        reviewed_at: None,
        review_note: None,
    };
    db.insert_plugin_release(&release).await?;
    audit(
        db,
        &plugin.id,
        Some(&release.id),
        &user.id,
        PluginReviewAction::Submit,
        None,
    )
    .await?;
    Ok(Json(release_view(&release, false, true)))
}

/// # Withdraw a release before review
#[openapi(tag = "Plugins")]
#[post("/releases/<id>/withdraw")]
pub async fn withdraw(_open: PluginsOpen, db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    let mut release = db.fetch_plugin_release(id).await?;
    let plugin = db.fetch_plugin(&release.plugin_id).await?;
    if plugin.publisher_id != user.id {
        return Err(create_error!(NotFound));
    }
    release.status = plugin_release_transition(&release.status, &PluginReviewAction::Withdraw)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    db.update_plugin_release(&release).await?;
    audit(db, &plugin.id, Some(id), &user.id, PluginReviewAction::Withdraw, None).await?;
    Ok(EmptyResponse)
}

// ---------------------------------------------------------------- review

/// # A release waiting for review
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct ReviewItem {
    pub plugin: PluginCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<PublisherCard>,
    /// With its code, so it can be read before approval
    pub release: ReleaseView,
    /// The currently approved release, to compare permissions
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<ReleaseView>,
    /// Permissions this release adds over the current one
    pub new_permissions: Vec<String>,
}

/// # Review queue
#[openapi(tag = "Plugins")]
#[get("/review/queue")]
pub async fn review_queue(_open: PluginsOpen, db: &State<Database>, user: User) -> Result<Json<Vec<ReviewItem>>> {
    require_reviewer(db, &user).await?;
    queue_items(db).await.map(Json)
}

/// Releases waiting for review, with what each adds over the current one
pub(crate) async fn queue_items(db: &Database) -> Result<Vec<ReviewItem>> {
    let pending = db
        .fetch_plugin_releases_by_status(PluginReleaseStatus::PendingReview, 100)
        .await?;
    let mut items = vec![];
    let mut plugins: HashMap<String, Plugin> = HashMap::new();
    for release in pending {
        let plugin = match plugins.get(&release.plugin_id) {
            Some(p) => p.clone(),
            None => {
                let p = db.fetch_plugin(&release.plugin_id).await?;
                plugins.insert(p.id.clone(), p.clone());
                p
            }
        };
        // a removed plugin never comes back; its releases aren't work
        if plugin.status == PluginStatus::Removed {
            continue;
        }
        let current = match &plugin.latest_release {
            Some(id) => db.fetch_plugin_release(id).await.ok(),
            None => None,
        };
        let new_permissions = release
            .permissions
            .iter()
            .filter(|p| current.as_ref().is_none_or(|c| !c.permissions.contains(p)))
            .cloned()
            .collect();
        let publisher = publishers_for(db, [plugin.publisher_id.clone()])
            .await?
            .into_iter()
            .next();
        items.push(ReviewItem {
            plugin: card(&plugin, true),
            publisher,
            release: release_view(&release, true, true),
            current: current.as_ref().map(|c| release_view(c, false, true)),
            new_permissions,
        });
    }
    Ok(items)
}

/// # Review decision
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone, Default)]
pub struct DataPluginReview {
    #[serde(default)]
    pub reason: Option<String>,
    /// Also mark the publisher as verified
    #[serde(default)]
    pub verify_publisher: bool,
    /// Required for high-risk releases: the reviewer read the code and
    /// every permission
    #[serde(default)]
    pub acknowledge_high_risk: bool,
}

/// Newest approved release of a plugin, ignoring `skip`
fn newest_approved<'a>(releases: &'a [PluginRelease], skip: &str) -> Option<&'a PluginRelease> {
    releases
        .iter()
        .filter(|r| r.status == PluginReleaseStatus::Approved && r.id != skip)
        .max_by(|a, b| compare_plugin_versions(&a.version, &b.version))
}

/// # Approve a release
#[openapi(tag = "Plugins")]
#[post("/releases/<id>/approve", data = "<data>")]
pub async fn approve(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<Json<ReleaseView>> {
    require_reviewer(db, &user).await?;
    approve_as(db, &user, id, data.into_inner()).await.map(Json)
}

/// Approve a release as `user`; the caller has checked their right to review
pub(crate) async fn approve_as(
    db: &Database,
    user: &User,
    id: &str,
    data: DataPluginReview,
) -> Result<ReleaseView> {
    let mut release = db.fetch_plugin_release(id).await?;
    let plugin = db.fetch_plugin(&release.plugin_id).await?;
    if plugin.publisher_id == user.id && !user.privileged {
        return Err(invalid("you can't review your own plugin"));
    }
    if plugin.status == PluginStatus::Removed {
        return Err(invalid("this plugin was removed"));
    }
    if release.risk == PluginRisk::High && !data.acknowledge_high_risk {
        return Err(invalid(
            "high-risk release: confirm you read the code and every permission",
        ));
    }
    if release.scan.iter().any(|f| f.block) {
        return Err(invalid("blocked by the scanner"));
    }
    release.status = plugin_release_transition(&release.status, &PluginReviewAction::Approve)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    release.reviewed_by = Some(user.id.clone());
    release.reviewed_at = Some(now_ms());
    release.review_note = clean_reason(data.reason, false)?;
    db.update_plugin_release(&release).await?;

    // the store shows the newest approved version
    let mut after = plugin.clone();
    let newer = after
        .latest_version
        .as_ref()
        .is_none_or(|v| compare_plugin_versions(&release.version, v) == std::cmp::Ordering::Greater);
    if newer {
        let manifest: Value = serde_json::from_str(&release.manifest).unwrap_or(Value::Null);
        after.latest_release = Some(release.id.clone());
        after.latest_version = Some(release.version.clone());
        if let Some(name) = manifest["name"].as_str() {
            after.name = name.trim().to_string();
        }
        if let Some(description) = manifest["description"].as_str() {
            after.summary = description.trim().to_string();
        }
        if let Some(category) = manifest["category"].as_str() {
            after.category = category.to_string();
        }
        after.icon = manifest["icon"].as_str().map(str::to_string);
    }
    if after.status == PluginStatus::Draft {
        after.status = PluginStatus::Listed;
    }
    save_plugin(db, &plugin, after).await?;
    audit(
        db,
        &plugin.id,
        Some(id),
        &user.id,
        PluginReviewAction::Approve,
        release.review_note.clone(),
    )
    .await?;

    if data.verify_publisher {
        let mut publisher = publisher_of(db, &plugin.publisher_id).await?;
        publisher.verified = true;
        publisher.updated_at = now_ms();
        publisher.updated_by = user.id.clone();
        db.save_plugin_publisher(&publisher).await?;
        audit(db, &plugin.id, None, &user.id, PluginReviewAction::VerifyPublisher, None).await?;
    }
    Ok(release_view(&release, false, true))
}

/// # Reject a release
#[openapi(tag = "Plugins")]
#[post("/releases/<id>/reject", data = "<data>")]
pub async fn reject(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<EmptyResponse> {
    require_reviewer(db, &user).await?;
    reject_as(db, &user, id, data.into_inner()).await?;
    Ok(EmptyResponse)
}

pub(crate) async fn reject_as(db: &Database, user: &User, id: &str, data: DataPluginReview) -> Result<()> {
    let mut release = db.fetch_plugin_release(id).await?;
    let reason = clean_reason(data.reason, true)?;
    release.status = plugin_release_transition(&release.status, &PluginReviewAction::Reject)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    release.reviewed_by = Some(user.id.clone());
    release.reviewed_at = Some(now_ms());
    release.review_note = reason.clone();
    db.update_plugin_release(&release).await?;
    audit(db, &release.plugin_id, Some(id), &user.id, PluginReviewAction::Reject, reason).await
}

/// # Revoke an approved release
///
/// It never runs again. The store falls back to the previous approved
/// release; users on the revoked one are asked to switch.
#[openapi(tag = "Plugins")]
#[post("/releases/<id>/revoke", data = "<data>")]
pub async fn revoke(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<EmptyResponse> {
    require_reviewer(db, &user).await?;
    revoke_as(db, &user, id, data.into_inner()).await?;
    Ok(EmptyResponse)
}

pub(crate) async fn revoke_as(db: &Database, user: &User, id: &str, data: DataPluginReview) -> Result<()> {
    let mut release = db.fetch_plugin_release(id).await?;
    let reason = clean_reason(data.reason, true)?;
    release.status = plugin_release_transition(&release.status, &PluginReviewAction::Revoke)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    release.reviewed_by = Some(user.id.clone());
    release.reviewed_at = Some(now_ms());
    release.review_note = reason.clone();
    db.update_plugin_release(&release).await?;

    let plugin = db.fetch_plugin(&release.plugin_id).await?;
    if plugin.latest_release.as_deref() == Some(id) {
        let releases = db.fetch_plugin_releases(&plugin.id).await?;
        let fallback = newest_approved(&releases, id);
        let mut after = plugin.clone();
        after.latest_release = fallback.map(|r| r.id.clone());
        after.latest_version = fallback.map(|r| r.version.clone());
        if fallback.is_none() && after.status == PluginStatus::Listed {
            // nothing safe left to install
            after.status = PluginStatus::Draft;
        }
        save_plugin(db, &plugin, after).await?;
    }
    audit(db, &release.plugin_id, Some(id), &user.id, PluginReviewAction::Revoke, reason).await
}

/// # Suspend a plugin
///
/// Out of the store, and installs stop running, until reinstated.
#[openapi(tag = "Plugins")]
#[post("/<id>/suspend", data = "<data>")]
pub async fn suspend(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<Json<PluginCard>> {
    require_reviewer(db, &user).await?;
    suspend_as(db, &user, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn suspend_as(db: &Database, user: &User, id: &str, data: DataPluginReview) -> Result<PluginCard> {
    let plugin = db.fetch_plugin(id).await?;
    if plugin.status == PluginStatus::Removed {
        return Err(create_error!(InvalidOperation));
    }
    let reason = clean_reason(data.reason, true)?;
    let mut after = plugin.clone();
    after.status = PluginStatus::Suspended;
    after.status_reason = reason.clone();
    let after = save_plugin(db, &plugin, after).await?;
    audit(db, id, None, &user.id, PluginReviewAction::Suspend, reason).await?;
    Ok(card(&after, true))
}

/// # Reinstate a suspended plugin
#[openapi(tag = "Plugins")]
#[post("/<id>/reinstate", data = "<data>")]
pub async fn reinstate(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<Json<PluginCard>> {
    require_reviewer(db, &user).await?;
    reinstate_as(db, &user, id, data.into_inner()).await.map(Json)
}

pub(crate) async fn reinstate_as(db: &Database, user: &User, id: &str, data: DataPluginReview) -> Result<PluginCard> {
    let plugin = db.fetch_plugin(id).await?;
    if plugin.status != PluginStatus::Suspended {
        return Err(create_error!(InvalidOperation));
    }
    let reason = clean_reason(data.reason, false)?;
    let mut after = plugin.clone();
    after.status = if after.latest_release.is_some() {
        PluginStatus::Listed
    } else {
        PluginStatus::Draft
    };
    after.status_reason = None;
    after.reports = vec![];
    let after = save_plugin(db, &plugin, after).await?;
    audit(db, id, None, &user.id, PluginReviewAction::Reinstate, reason).await?;
    Ok(card(&after, true))
}

/// # Remove a plugin for good
///
/// Its publisher or a reviewer. Installs stop running; users can only
/// uninstall it.
#[openapi(tag = "Plugins")]
#[post("/<id>/remove", data = "<data>")]
pub async fn remove(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<EmptyResponse> {
    let plugin = db.fetch_plugin(id).await?;
    if plugin.publisher_id != user.id {
        require_reviewer(db, &user).await?;
    }
    remove_as(db, &user, id, data.into_inner()).await?;
    Ok(EmptyResponse)
}

pub(crate) async fn remove_as(db: &Database, user: &User, id: &str, data: DataPluginReview) -> Result<()> {
    let plugin = db.fetch_plugin(id).await?;
    let reason = clean_reason(data.reason, false)?;
    let mut after = plugin.clone();
    after.status = PluginStatus::Removed;
    after.status_reason = reason.clone();
    save_plugin(db, &plugin, after).await?;
    audit(db, id, None, &user.id, PluginReviewAction::Remove, reason).await
}

/// # Report a plugin
///
/// Once per user. Enough reports from different users suspend it until a
/// reviewer looks at it.
#[openapi(tag = "Plugins")]
#[post("/<id>/report", data = "<data>")]
pub async fn report(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let plugin = db.fetch_plugin(id).await?;
    if plugin.publisher_id == user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if plugin.status != PluginStatus::Listed {
        return Err(create_error!(NotFound));
    }
    let reason = clean_reason(data.into_inner().reason, true)?;
    let already = plugin.reports.iter().any(|u| u == &user.id);
    db.report_plugin(id, &user.id).await?;
    if !already {
        audit(db, id, None, &user.id, PluginReviewAction::Report, reason).await?;
    }
    // only settled accounts count, so a few new accounts can't take a plugin
    // down; official plugins always wait for a reviewer
    let plugin = db.fetch_plugin(id).await?;
    let count = counted_plugin_reports(&plugin.reports, now_ms());
    if count >= PLUGIN_REPORTS_TO_SUSPEND && !plugin.official {
        if plugin.status == PluginStatus::Listed {
            let mut after = plugin.clone();
            after.status = PluginStatus::Suspended;
            after.status_reason = Some(format!("{count} user reports"));
            if save_plugin(db, &plugin, after).await.is_ok() {
                audit(
                    db,
                    id,
                    None,
                    SYSTEM_ACTOR,
                    PluginReviewAction::Suspend,
                    Some(format!("{count} user reports")),
                )
                .await?;
            }
        }
    }
    Ok(EmptyResponse)
}

async fn set_publisher(
    db: &Database,
    reviewer: &User,
    publisher_id: &str,
    change: impl FnOnce(&mut PluginPublisher),
    action: PluginReviewAction,
    reason: Option<String>,
) -> Result<EmptyResponse> {
    require_reviewer(db, reviewer).await?;
    change_publisher(db, reviewer, publisher_id, change, action, reason).await?;
    Ok(EmptyResponse)
}

/// `verify`, `suspend` (reason required) or `reinstate` a publisher as
/// `reviewer`; the caller has checked their right to review
pub(crate) async fn publisher_as(db: &Database, reviewer: &User, publisher_id: &str, action: &str, reason: Option<String>) -> Result<()> {
    match action {
        "verify" => change_publisher(db, reviewer, publisher_id, |p| p.verified = true, PluginReviewAction::VerifyPublisher, None).await,
        "suspend" => {
            let reason = clean_reason(reason, true)?;
            change_publisher(db, reviewer, publisher_id, |p| p.status = PluginPublisherStatus::Suspended, PluginReviewAction::SuspendPublisher, reason).await
        }
        "reinstate" => change_publisher(db, reviewer, publisher_id, |p| p.status = PluginPublisherStatus::Active, PluginReviewAction::ReinstatePublisher, None).await,
        _ => Err(create_error!(InvalidOperation)),
    }
}

async fn change_publisher(
    db: &Database,
    reviewer: &User,
    publisher_id: &str,
    change: impl FnOnce(&mut PluginPublisher),
    action: PluginReviewAction,
    reason: Option<String>,
) -> Result<()> {
    db.fetch_user(publisher_id).await?;
    let mut publisher = publisher_of(db, publisher_id).await?;
    change(&mut publisher);
    publisher.reason = reason.clone();
    publisher.updated_at = now_ms();
    publisher.updated_by = reviewer.id.clone();
    db.save_plugin_publisher(&publisher).await?;
    // log against every plugin they own so it shows in each history
    for plugin in db
        .fetch_plugins(PluginQuery {
            publisher_id: Some(publisher_id),
            limit: 100,
            ..Default::default()
        })
        .await?
    {
        audit(db, &plugin.id, None, &reviewer.id, action.clone(), reason.clone()).await?;
    }
    Ok(())
}

/// # Verify a publisher
#[openapi(tag = "Plugins")]
#[post("/publishers/<id>/verify")]
pub async fn verify_publisher(_open: PluginsOpen, db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    set_publisher(db, &user, id, |p| p.verified = true, PluginReviewAction::VerifyPublisher, None).await
}

/// # Suspend a publisher
///
/// They can't create or submit; their plugins stay as they are (suspend
/// those separately if they are unsafe).
#[openapi(tag = "Plugins")]
#[post("/publishers/<id>/suspend", data = "<data>")]
pub async fn suspend_publisher(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginReview>,
) -> Result<EmptyResponse> {
    let reason = clean_reason(data.into_inner().reason, true)?;
    set_publisher(
        db,
        &user,
        id,
        |p| p.status = PluginPublisherStatus::Suspended,
        PluginReviewAction::SuspendPublisher,
        reason,
    )
    .await
}

/// # Reinstate a publisher
#[openapi(tag = "Plugins")]
#[post("/publishers/<id>/reinstate")]
pub async fn reinstate_publisher(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<EmptyResponse> {
    set_publisher(
        db,
        &user,
        id,
        |p| p.status = PluginPublisherStatus::Active,
        PluginReviewAction::ReinstatePublisher,
        None,
    )
    .await
}

// ---------------------------------------------------------------- installs

/// # A newer release than the installed one
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct PluginUpdate {
    pub release_id: String,
    pub version: String,
    pub permissions: Vec<String>,
    /// Permissions the update adds; the user has to consent again
    pub new_permissions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    /// The installed release was revoked and this is the safe fallback
    pub fallback: bool,
}

/// # One of your installed plugins
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct InstallView {
    pub plugin: PluginCard,
    pub release: ReleaseView,
    pub enabled: bool,
    /// Permissions you granted to this release
    pub granted: Vec<String>,
    pub settings: Value,
    pub installed_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<PluginUpdate>,
    /// Whether the server allows it to run right now
    pub runnable: bool,
    /// Why not: `suspended`, `removed`, `revoked`, `not_approved`,
    /// `consent`, `third_party_code`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

async fn install_view(db: &Database, install: &PluginInstall) -> Result<Option<InstallView>> {
    let Ok(plugin) = db.fetch_plugin(&install.plugin_id).await else {
        return Ok(None);
    };
    let Ok(release) = db.fetch_plugin_release(&install.release_id).await else {
        return Ok(None);
    };
    let grant = db.fetch_plugin_grant(&install.user_id, &install.plugin_id).await?;
    let granted = grant
        .as_ref()
        .filter(|g| g.release_id == release.id)
        .map(|g| g.permissions.clone())
        .unwrap_or_default();
    let settings = db
        .fetch_plugin_settings(&install.user_id, &install.plugin_id)
        .await?
        .and_then(|s| serde_json::from_str(&s.values).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));

    let consented = release.permissions.iter().all(|p| granted.contains(p));
    let blocked = if plugin.status == PluginStatus::Suspended {
        Some("suspended")
    } else if plugin.status == PluginStatus::Removed {
        Some("removed")
    } else if release.status == PluginReleaseStatus::Revoked {
        Some("revoked")
    } else if release.status != PluginReleaseStatus::Approved || !plugin_runnable(&plugin.status) {
        Some("not_approved")
    } else if !consented {
        Some("consent")
    } else if release.entrypoint == "worker" && !code_allowed(&plugin) {
        Some("third_party_code")
    } else {
        None
    };

    let update = match &plugin.latest_release {
        Some(latest) if latest != &release.id && plugin.status == PluginStatus::Listed => {
            match db.fetch_plugin_release(latest).await {
                Ok(next)
                    if next.status == PluginReleaseStatus::Approved
                        && (release.status == PluginReleaseStatus::Revoked
                            || compare_plugin_versions(&next.version, &release.version)
                                == std::cmp::Ordering::Greater) =>
                {
                    Some(PluginUpdate {
                        new_permissions: next
                            .permissions
                            .iter()
                            .filter(|p| !granted.contains(p))
                            .cloned()
                            .collect(),
                        release_id: next.id.clone(),
                        version: next.version.clone(),
                        permissions: next.permissions.clone(),
                        changelog: next.changelog.clone(),
                        fallback: release.status == PluginReleaseStatus::Revoked,
                    })
                }
                _ => None,
            }
        }
        _ => None,
    };

    Ok(Some(InstallView {
        release: release_view(&release, blocked.is_none(), false),
        plugin: card(&plugin, false),
        enabled: install.enabled,
        granted,
        settings,
        installed_at: install.installed_at,
        updated_at: install.updated_at,
        update,
        runnable: blocked.is_none(),
        blocked: blocked.map(str::to_string),
    }))
}

/// # Your installed plugins
#[openapi(tag = "Plugins")]
#[get("/@me/installs")]
pub async fn installs(_open: PluginsOpen, db: &State<Database>, user: User) -> Result<Json<Vec<InstallView>>> {
    let mut views = vec![];
    for install in db.fetch_plugin_installs(&user.id).await? {
        if let Some(view) = install_view(db, &install).await? {
            views.push(view);
        }
    }
    Ok(Json(views))
}

/// # Install or update
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct DataInstallPlugin {
    /// Approved release to install
    pub release_id: String,
    /// Exactly the permissions the release asks for: consent is all or
    /// nothing
    pub permissions: Vec<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// # Install, update or switch version of a plugin
#[openapi(tag = "Plugins")]
#[put("/@me/installs/<id>", data = "<data>")]
pub async fn install(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataInstallPlugin>,
) -> Result<Json<InstallView>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let data = data.into_inner();
    let plugin = db.fetch_plugin(id).await?;
    if plugin.status != PluginStatus::Listed {
        return Err(create_error!(NotFound));
    }
    let release = db.fetch_plugin_release(&data.release_id).await?;
    if release.plugin_id != plugin.id || release.status != PluginReleaseStatus::Approved {
        return Err(create_error!(NotFound));
    }
    let requested: BTreeSet<&String> = release.permissions.iter().collect();
    let given: BTreeSet<&String> = data.permissions.iter().collect();
    if requested != given || data.permissions.len() != given.len() {
        return Err(invalid(
            "permissions: consent must cover exactly the permissions this release asks for",
        ));
    }

    let existing = db.fetch_plugin_install(&user.id, id).await?;
    if existing.is_none()
        && db.fetch_plugin_installs(&user.id).await?.len() >= PLUGIN_INSTALLS_PER_USER
    {
        return Err(invalid(format!(
            "you can install at most {PLUGIN_INSTALLS_PER_USER} plugins"
        )));
    }
    let now = now_ms();
    let install = PluginInstall {
        id: PluginInstall::key(&user.id, id),
        user_id: user.id.clone(),
        plugin_id: plugin.id.clone(),
        release_id: release.id.clone(),
        version: release.version.clone(),
        enabled: data
            .enabled
            .unwrap_or_else(|| existing.as_ref().map(|i| i.enabled).unwrap_or(true)),
        installed_at: existing.as_ref().map(|i| i.installed_at).unwrap_or(now),
        updated_at: now,
    };
    db.save_plugin_grant(&PluginGrant {
        id: install.id.clone(),
        user_id: user.id.clone(),
        plugin_id: plugin.id.clone(),
        release_id: release.id.clone(),
        permissions: release.permissions.clone(),
        granted_at: now,
    })
    .await?;
    db.save_plugin_install(&install).await?;
    if existing.is_none() {
        db.bump_plugin_installs(id, 1).await?;
    }
    install_view(db, &install)
        .await?
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}

/// # Turn on or off
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct DataPluginEnabled {
    pub enabled: bool,
}

/// # Enable or disable an installed plugin
#[openapi(tag = "Plugins")]
#[patch("/@me/installs/<id>", data = "<data>")]
pub async fn set_enabled(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginEnabled>,
) -> Result<Json<InstallView>> {
    let mut install = db
        .fetch_plugin_install(&user.id, id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    if data.enabled {
        // only something that may run can be switched on
        let view = install_view(db, &install)
            .await?
            .ok_or_else(|| create_error!(NotFound))?;
        if let Some(reason) = view.blocked.filter(|r| r != "third_party_code") {
            return Err(invalid(format!("this plugin can't be enabled: {reason}")));
        }
    }
    install.enabled = data.enabled;
    install.updated_at = now_ms();
    db.save_plugin_install(&install).await?;
    install_view(db, &install)
        .await?
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}

/// # Uninstall a plugin
///
/// Removes the install, its permission grant and its settings.
#[openapi(tag = "Plugins")]
#[delete("/@me/installs/<id>")]
pub async fn uninstall(_open: PluginsOpen, db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    if db.fetch_plugin_install(&user.id, id).await?.is_none() {
        return Err(create_error!(NotFound));
    }
    db.delete_plugin_install(&user.id, id).await?;
    db.bump_plugin_installs(id, -1).await?;
    Ok(EmptyResponse)
}

/// # Plugin settings
#[derive(Serialize, Deserialize, JsonSchema, Debug, Clone)]
pub struct DataPluginSettings {
    pub values: Value,
}

/// # Save your settings for a plugin
///
/// Checked against the settings the manifest declares; unknown keys are
/// dropped.
#[openapi(tag = "Plugins")]
#[put("/@me/installs/<id>/settings", data = "<data>")]
pub async fn save_settings(
    _open: PluginsOpen,
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<DataPluginSettings>,
) -> Result<Json<Value>> {
    let install = db
        .fetch_plugin_install(&user.id, id)
        .await?
        .ok_or_else(|| create_error!(NotFound))?;
    let release = db.fetch_plugin_release(&install.release_id).await?;
    let manifest: Value = serde_json::from_str(&release.manifest).unwrap_or(Value::Null);
    let plugin = db.fetch_plugin(id).await?;
    let validated = validate_plugin_manifest(&manifest, release.code.as_deref(), plugin.official)
        .map_err(|_| create_error!(InternalError))?;
    if !validated.permissions.iter().any(|p| p == "storage.settings") {
        return Err(create_error!(InvalidOperation));
    }
    let clean = clean_plugin_settings(&validated.settings, &data.values).map_err(invalid)?;
    let values = Value::Object(clean);
    db.save_plugin_settings(&PluginSettings {
        id: install.id.clone(),
        user_id: user.id.clone(),
        plugin_id: id.to_string(),
        values: values.to_string(),
        updated_at: now_ms(),
    })
    .await?;
    Ok(Json(values))
}

#[cfg(test)]
mod flag_test;
#[cfg(test)]
mod test;
