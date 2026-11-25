//! Community decorations, nameplates, profile effects and profile frames.
//! Only creators granted by staff can submit. Decorations, nameplates and
//! frames go live at once;
//! effects play by themselves for people who did not ask, so a reviewer
//! approves each one first. Everyone signed in can browse, favourite and wear
//! published designs.
//!
//! Every route checks platform permissions itself (see
//! `PlatformPermissions`); a hidden button in the client is not security.

pub(crate) mod admin;

use std::collections::HashSet;

use iso8601_timestamp::Timestamp;
use revolt_config::config;
use revolt_database::{
    community_tags, community_text_blocked, community_transition, creator_is_active, is_declared_adult,
    creator_is_suspended, epoch_ms, erase_community_item, record_platform_audit, AuditOutcome,
    AuditRecord, platform_permissions, util::cdn_purge::purge_files_later, CommunityItem, CommunityItemKind,
    CommunityItemQuery, CommunityItemStatus, CommunityReview, CommunityReviewAction,
    CommunityRevision, CommunityRights, CommunitySort, CreatorStanding, Database, File,
    PlatformPermission, PlatformPermissions, PlatformRole, User, COMMUNITY_REPORTS_TO_HIDE,
    CREATOR_TERMS_VERSION,
};
use revolt_database::util::{
    age::{may_see_personal_sections, owner_band},
    permissions::DatabasePermissionQuery,
};
use revolt_models::v0;
use revolt_permissions::{calculate_user_permissions, UserPermission};
use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::{serde::json::Json, Route, State};
use rocket_empty::EmptyResponse;
use ulid::Ulid;
use validator::Validate;

pub use admin::routes as admin_routes;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        permissions,
        set_public_name,
        creator_standing,
        list,
        create,
        mine,
        review_queue,
        creator_profile,
        creations,
        details,
        download,
        edit,
        submit,
        approve,
        reject,
        request_changes,
        suspend,
        archive,
        restore,
        delete_item,
        erase,
        override_review,
        favourite,
        unfavourite,
        report
    ]
}

/// Actor id used when the system acts (reports reaching the limit)
const SYSTEM_ACTOR: &str = "00000000000000000000000000";

fn kind_of(kind: &str) -> Result<CommunityItemKind> {
    match kind {
        "decorations" => Ok(CommunityItemKind::Decoration),
        "nameplates" => Ok(CommunityItemKind::Nameplate),
        "effects" => Ok(CommunityItemKind::Effect),
        "frames" => Ok(CommunityItemKind::Frame),
        _ => Err(create_error!(InvalidOperation)),
    }
}

/// Whether creators' items of this kind are open under these settings.
/// Effects are off until the machine check is trusted, community frames until
/// they are approved; decorations and nameplates are always open.
pub(crate) fn kind_open(
    kind: &CommunityItemKind,
    community: &revolt_config::FeaturesCommunity,
) -> bool {
    match kind {
        CommunityItemKind::Effect => community.effects_enabled,
        CommunityItemKind::Frame => community.frames_enabled,
        CommunityItemKind::Decoration | CommunityItemKind::Nameplate => true,
    }
}

/// Refuses a kind that is switched off (effects, community frames)
async fn effects_open(kind: &CommunityItemKind) -> Result<()> {
    if !kind_open(kind, &config().await.features.community) {
        return Err(create_error!(NotFound));
    }
    Ok(())
}

/// Where an item's file lives
fn tag_of(kind: &CommunityItemKind) -> &'static str {
    match kind {
        CommunityItemKind::Effect => "effects",
        CommunityItemKind::Decoration | CommunityItemKind::Nameplate | CommunityItemKind::Frame => {
            "decorations"
        }
    }
}

/// A community frame is one still picture of the template's size: it is laid
/// over the card as it is, and a moving one could not be stopped for people
/// who ask for less motion. The media server has already checked that the
/// card's content area is transparent (autumn `check_frame_safe_area`).
fn check_frame_file(file: &File) -> Result<()> {
    let refused = |code: &str| {
        Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
            "file", code,
        )]))
    };
    let (width, height) = v0::PROFILE_FRAME_FILE_SIZE;
    match &file.metadata {
        revolt_database::Metadata::Image { width: w, height: h, animated, .. } => {
            if (*w as usize, *h as usize) != (width, height) {
                return refused("frame_size");
            }
            if animated.unwrap_or(false) {
                return refused("frame_still");
            }
            Ok(())
        }
        _ => refused("frame_size"),
    }
}

/// Names, descriptions and tags of effects keep clear of Zrzur and of other
/// people's names (T-06)
fn check_effect_words(
    kind: &CommunityItemKind,
    name: Option<&str>,
    description: Option<&str>,
    tags: Option<&[String]>,
) -> Result<()> {
    if *kind != CommunityItemKind::Effect {
        return Ok(());
    }
    let refused = |field: &str| {
        Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
            field, "blocked",
        )]))
    };
    if name.is_some_and(community_text_blocked) {
        return refused("name");
    }
    if description.is_some_and(community_text_blocked) {
        return refused("description");
    }
    if tags.is_some_and(|tags| tags.iter().any(|tag| community_text_blocked(tag))) {
        return refused("tags");
    }
    Ok(())
}

/// The creator's promise, checked and dated (L-20). It is never assumed: the
/// box starts unticked, and the version must be the terms in force.
fn rights_of(data: Option<v0::DataCommunityRights>) -> Result<Option<CommunityRights>> {
    let Some(data) = data else {
        return Ok(None);
    };
    if !data.confirmed || data.terms_version != CREATOR_TERMS_VERSION {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("rights", "required"),
        ]));
    }
    Ok(Some(CommunityRights {
        terms_version: data.terms_version,
        confirmed_at: now_ms(),
        third_party: clean_description(data.third_party),
        ai_parts: clean_description(data.ai_parts),
    }))
}

/// Nothing a creator publishes goes out without their promise on it: own
/// work, parts by others, parts made with AI tools, and the Creator Terms in
/// force (Creator Terms, "What these terms cover" and "Only original work")
fn require_rights(item: &CommunityItem) -> Result<()> {
    if !promise_stands(item) {
        return Err(rights_required());
    }
    Ok(())
}

/// The promise on a design counts for a new publication only if it was given
/// under the Creator Terms in force; after new terms the creator is asked
/// again (L-42). What is already public is not touched.
fn promise_stands(item: &CommunityItem) -> bool {
    item.rights
        .as_ref()
        .is_some_and(|rights| rights.terms_version == CREATOR_TERMS_VERSION)
}

/// A reviewer's approval: an effect needs the promise. A decoration or
/// nameplate published before the promise was asked of every design can
/// still be kept or reinstated.
fn require_rights_to_approve(item: &CommunityItem) -> Result<()> {
    if item.kind == CommunityItemKind::Effect && item.rights.is_none() {
        return Err(rights_required());
    }
    Ok(())
}

fn rights_required() -> revolt_result::Error {
    revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new(
        "rights", "required",
    )])
}

fn validated<T: Validate>(data: T) -> Result<T> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    Ok(data)
}

fn now_ms() -> i64 {
    epoch_ms(Timestamp::now_utc())
}

pub(crate) fn require(perms: &PlatformPermissions, permission: PlatformPermission) -> Result<()> {
    if perms.has(permission) {
        Ok(())
    } else {
        Err(create_error!(MissingPlatformPermission {
            permission: permission.name().to_string()
        }))
    }
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim().to_string();
    if name.chars().count() < 2 {
        return Err(revolt_result::failed_validation(vec![revolt_result::ValidationProblem {
            min: Some(2),
            ..revolt_result::ValidationProblem::new("name", "length")
        }]));
    }
    Ok(name)
}

fn clean_description(description: Option<String>) -> Option<String> {
    description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Claim an uploaded image for a design. Only the uploader can use it.
async fn claim_file(
    db: &Database,
    file_id: &str,
    kind: &CommunityItemKind,
    item_id: &str,
    user: &User,
) -> Result<File> {
    let tag = tag_of(kind);
    let file = db.fetch_attachment(tag, file_id).await?;
    if file.uploader_id.as_deref() != Some(user.id.as_str()) {
        return Err(create_error!(NotFound));
    }
    if *kind == CommunityItemKind::Frame {
        check_frame_file(&file)?;
    }
    File::use_community_item(db, file_id, tag, item_id, &user.id).await
}

/// Statuses that count against a creator's quota
const ACTIVE_STATUSES: [CommunityItemStatus; 5] = [
    CommunityItemStatus::Draft,
    CommunityItemStatus::PendingReview,
    CommunityItemStatus::ChangesRequested,
    CommunityItemStatus::Published,
    CommunityItemStatus::Suspended,
];

/// Publish straight away. Creators are trusted Zrzur team members, so
/// their designs go live without review; reviewers can still suspend them.
/// An effect goes to the review queue instead: a reviewer publishes it.
fn publish(after: &mut CommunityItem, from: &CommunityItemStatus) -> Result<()> {
    let published = community_transition(from, &CommunityReviewAction::Publish)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    after.review_note = None;
    require_rights(after)?;
    if after.kind == CommunityItemKind::Effect {
        after.status = CommunityItemStatus::PendingReview;
        return Ok(());
    }
    after.status = published;
    after.published_at.get_or_insert_with(now_ms);
    Ok(())
}

/// What a creator's "publish" is called in the log
fn sent_action(item: &CommunityItem) -> CommunityReviewAction {
    if item.kind == CommunityItemKind::Effect {
        CommunityReviewAction::Submit
    } else {
        CommunityReviewAction::Publish
    }
}

/// Wearers told about a takedown, at most: the item leaves everyone anyway
const WEARERS_TOLD: usize = 2000;

/// A design left the public against its creator's will. It gets the same
/// statement of reasons and appeal as any Safety action, and its wearers are
/// told; never who reported it (D-23). This also takes it off the wearers.
async fn took_down(db: &Database, actor: &str, item: &CommunityItem, reason: &str, automated: bool) -> Result<()> {
    crate::routes::safety::actions::record_takedown(db, actor, item, reason, automated).await?;
    crate::routes::safety::actions::take_off_wearers(db, item, WEARERS_TOLD).await
}

/// Make a revision the public version. Returns the new file id when the
/// image changed, so everyone wearing it can be pointed at it.
fn apply_revision(after: &mut CommunityItem, revision: CommunityRevision) -> Option<String> {
    let new_file = (revision.file.id != after.file.id).then(|| revision.file.id.clone());
    after.name = revision.name;
    after.description = revision.description;
    after.tags = revision.tags;
    after.file = revision.file;
    after.revision = revision.revision;
    new_file
}

async fn log(
    db: &Database,
    item: &CommunityItem,
    actor: &str,
    action: CommunityReviewAction,
    reason: Option<String>,
) -> Result<()> {
    db.insert_community_review(&CommunityReview {
        id: Ulid::new().to_string(),
        item_id: item.id.clone(),
        revision: item
            .pending
            .as_ref()
            .map(|p| p.revision)
            .unwrap_or(item.revision),
        actor_id: actor.to_string(),
        action,
        reason,
        created_at: now_ms(),
    })
    .await
}

/// Save a changed item if nobody changed it since `before` was read
async fn save(db: &Database, before: &CommunityItem, mut after: CommunityItem, clear_reports: bool) -> Result<CommunityItem> {
    after.version = before.version + 1;
    if !db
        .update_community_item(&after, before.version, clear_reports)
        .await?
    {
        return Err(create_error!(CommunityItemConflict));
    }
    if before.status != after.status {
        sync_creator_badge(db, &after.creator_id).await;
    }
    if let Some(file) = left_public(before, &after) {
        purge_files_later(vec![file]);
    }
    Ok(after)
}

/// The image the public could see until now, if this change took it away
fn left_public(before: &CommunityItem, after: &CommunityItem) -> Option<File> {
    (before.status == CommunityItemStatus::Published
        && (after.status != CommunityItemStatus::Published || after.file.id != before.file.id))
        .then(|| before.file.clone())
}

/// Creator badge follows published designs; a failure here never undoes
/// the design change
async fn sync_creator_badge(db: &Database, creator_id: &str) {
    if let Err(error) = revolt_database::sync_creator_badge(db, creator_id).await {
        log::error!("creator badge for {creator_id}: {error:?}");
    }
}

/// Users and their platform records, once each
async fn people(
    db: &Database,
    ids: impl IntoIterator<Item = String>,
) -> Result<Vec<(User, Option<PlatformRole>)>> {
    let ids: Vec<String> = ids
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let roles = db.fetch_platform_roles(&ids).await?;
    Ok(db
        .fetch_users(&ids)
        .await?
        .into_iter()
        .map(|user| {
            let role = roles.iter().find(|r| r.id == user.id).cloned();
            (user, role)
        })
        .collect())
}

fn is_active(role: Option<&PlatformRole>) -> bool {
    role.is_some_and(|r| r.creator == Some(CreatorStanding::Active))
}

/// Public creator cards for a list of user ids: name and avatar, never
/// `username#tag`
pub(crate) async fn creators_for(
    db: &Database,
    ids: impl IntoIterator<Item = String>,
) -> Result<Vec<v0::CommunityCreator>> {
    let mut cards = vec![];
    for (user, role) in people(db, ids).await? {
        // the rank is 18+: it is not shown on anyone who is not a declared adult
        let verified = is_active(role.as_ref()) && is_declared_adult(db, &user).await;
        let open_page = verified && role.is_some_and(|r| r.show_public_name);
        cards.push(user.into_community_creator(verified, open_page));
    }
    Ok(cards)
}

/// The same people for staff views, with their handles
pub(crate) async fn accounts_for(
    db: &Database,
    ids: impl IntoIterator<Item = String>,
) -> Result<Vec<v0::CreatorAccount>> {
    Ok(people(db, ids)
        .await?
        .into_iter()
        .map(|(user, role)| {
            let verified = is_active(role.as_ref());
            user.into_creator_account(verified)
        })
        .collect())
}

/// # Community permissions
///
/// What the signed-in user may do: submit, review, manage creators.
#[openapi(tag = "Community")]
#[get("/permissions")]
pub async fn permissions(db: &State<Database>, user: User) -> Result<Json<v0::CommunityPermissions>> {
    let role = db.fetch_platform_role(&user.id).await?;
    let perms = platform_permissions(db, &user).await?;
    let can_delete_any = may_delete_any(db, &user, &perms).await?;
    Ok(Json(v0::CommunityPermissions {
        permissions: perms.names(),
        show_public_name: role.as_ref().map(|r| r.show_public_name).unwrap_or(false),
        creator: role.and_then(|r| r.creator).map(|c| format!("{c:?}")),
        can_delete_any,
    }))
}

/// Ranks that may delete any creator's design for good. The owner decided
/// (2026-11-25) that these ranks grant this power; ids from the badge
/// catalogue (`badges/logic.rs`).
const DELETE_ANY_RANKS: [&str; 3] = ["founder", "staff", "platform_moderator"];

/// Whether this person may delete anyone's design: a design admin
/// (`cosmetics.manage_all`, privileged staff included), or someone who holds
/// one of [`DELETE_ANY_RANKS`] now (not revoked, not expired). Bots never.
async fn may_delete_any(db: &Database, user: &User, perms: &PlatformPermissions) -> Result<bool> {
    if perms.has(PlatformPermission::CosmeticsManageAll) {
        return Ok(true);
    }
    if user.bot.is_some() {
        return Ok(false);
    }
    let held = revolt_database::held_badges(db, &user.id).await?;
    Ok(DELETE_ANY_RANKS.iter().any(|rank| held.contains(*rank)))
}

/// # Show my name on the public Designs site
///
/// Creators only. Off by default: visitors without an account see designs
/// without any creator name, avatar or id, and nobody but you sees a page of
/// all your work, until you switch this on.
#[openapi(tag = "Community")]
#[put("/creators/@me/public-name", data = "<data>")]
pub async fn set_public_name(
    db: &State<Database>,
    user: User,
    data: Json<v0::DataCreatorPublicName>,
) -> Result<EmptyResponse> {
    let mut role = db
        .fetch_platform_role(&user.id)
        .await?
        .filter(|r| r.creator.is_some())
        .ok_or_else(|| create_error!(NotFound))?;
    role.show_public_name = data.show_public_name;
    role.updated_at = now_ms();
    role.updated_by = user.id.clone();
    db.save_platform_role(&role).await?;
    Ok(EmptyResponse)
}

/// Ids among `ids` whose creator agreed to be named publicly, is still an
/// active creator and is a declared adult
async fn public_creators(db: &Database, ids: &[String]) -> Result<HashSet<String>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let named = db
        .fetch_platform_roles(ids)
        .await?
        .into_iter()
        .filter(|r| r.show_public_name && r.creator == Some(CreatorStanding::Active))
        .map(|r| r.id);
    let mut open = HashSet::new();
    for (user, _) in people(db, named).await? {
        if is_declared_adult(db, &user).await {
            open.insert(user.id);
        }
    }
    Ok(open)
}

/// # List community items
///
/// Published decorations, nameplates or frames only, without those of suspended
/// creators. Sort by `new` or `popular`
/// (unique users who applied it), search by name, tag or creator, filter
/// by creator or your favourites.
#[openapi(tag = "Community")]
#[get("/<kind>?<options..>")]
pub async fn list(
    db: &State<Database>,
    user: Option<User>,
    kind: &str,
    options: v0::OptionsCommunityQuery,
) -> Result<Json<v0::CommunityPage>> {
    let options = validated(options)?;
    let kind = kind_of(kind)?;
    // switched off: the section is simply empty, for old clients too
    if effects_open(&kind).await.is_err() {
        return Ok(Json(v0::CommunityPage {
            items: vec![],
            creators: vec![],
        }));
    }
    let favourites = favourites_of(db, user.as_ref()).await?;

    let search = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let mut search_creators: Vec<String> = match search {
        Some(q) => db
            .search_users_by_name(q.trim_start_matches('@'), 20)
            .await?
            .into_iter()
            .map(|u| u.id)
            .collect(),
        None => vec![],
    };
    // nobody finds a creator by name, or lists all their work, unless the
    // creator agreed to it
    let allowed = public_creators(db, &search_creators).await?;
    search_creators.retain(|id| allowed.contains(id));
    if let Some(creator) = options.creator.as_deref() {
        let own = user.as_ref().is_some_and(|user| user.id == creator);
        if !own && !public_creators(db, &[creator.to_string()]).await?.contains(creator) {
            return Err(create_error!(NotFound));
        }
    }

    let items = db
        .fetch_community_items(CommunityItemQuery {
            kind: Some(kind),
            search,
            search_creators,
            creator_id: options.creator.as_deref(),
            hidden_creators: db.fetch_suspended_creators().await?,
            ids: options
                .favourites
                .unwrap_or(false)
                .then(|| favourites.clone()),
            statuses: vec![CommunityItemStatus::Published],
            sort: match options.sort.as_deref() {
                Some("popular") => CommunitySort::Popular,
                _ => CommunitySort::New,
            },
            limit: options.limit.unwrap_or(48).clamp(1, 100) as usize,
            offset: options.offset.unwrap_or(0).max(0) as usize,
            ..Default::default()
        })
        .await?;

    let ids: Vec<String> = items.iter().map(|i| i.creator_id.clone()).collect();
    let mut creators = creators_for(db, ids.clone()).await?;
    let mut items: Vec<v0::CommunityItem> = items
        .into_iter()
        .map(|item| {
            let favourite = favourites.contains(&item.id);
            item.into_v0(false, favourite)
        })
        .collect();
    if user.is_none() {
        let allowed = public_creators(db, &ids).await?;
        creators.retain(|c| allowed.contains(&c.id));
        for item in items.iter_mut() {
            if !allowed.contains(&item.creator_id) {
                item.creator_id.clear();
            }
        }
    }
    Ok(Json(v0::CommunityPage { items, creators }))
}

/// # Submit community item
///
/// Creators only. The image must be uploaded to the `decorations` tag by
/// you. It starts as a draft, or is published straight away with `submit`.
#[openapi(tag = "Community")]
#[post("/<kind>", data = "<data>")]
pub async fn create(
    db: &State<Database>,
    user: User,
    kind: &str,
    data: Json<v0::DataCreateCommunityItem>,
) -> Result<Json<v0::CommunityItem>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsSubmit)?;

    let kind = kind_of(kind)?;
    effects_open(&kind).await?;
    let data = validated(data.into_inner())?;
    let name = clean_name(&data.name)?;
    let submit = data.submit.unwrap_or(false);
    let tags = community_tags(&data.tags.unwrap_or_default());
    let description = clean_description(data.description);
    check_effect_words(&kind, Some(&name), description.as_deref(), Some(&tags))?;
    let rights = rights_of(data.rights)?;
    if submit && rights.is_none() {
        return Err(rights_required());
    }

    let max = config().await.features.community.max_items_per_creator;
    if db
        .count_community_items_by_creator(&user.id, &ACTIVE_STATUSES)
        .await?
        >= max
    {
        return Err(create_error!(TooManyCommunityItems { max }));
    }

    let id = Ulid::new().to_string();
    let file = claim_file(db, &data.file, &kind, &id, &user).await?;
    let review_first = submit && kind == CommunityItemKind::Effect;
    let item = CommunityItem {
        id,
        kind,
        name,
        creator_id: user.id.clone(),
        file,
        status: if review_first {
            CommunityItemStatus::PendingReview
        } else if submit {
            CommunityItemStatus::Published
        } else {
            CommunityItemStatus::Draft
        },
        description,
        tags,
        revision: 1,
        published_at: (submit && !review_first).then(now_ms),
        reviewed_by: None,
        review_note: None,
        pending: None,
        popularity: 0,
        version: 0,
        reports: vec![],
        rights,
    };
    db.insert_community_item(&item).await?;
    if submit {
        log(db, &item, &user.id, sent_action(&item), None).await?;
        sync_creator_badge(db, &item.creator_id).await;
    }
    Ok(Json(item.into_v0(true, false)))
}

/// # My community items
///
/// Your own designs in every state, with review notes. Also works for
/// creators who were suspended or revoked.
#[openapi(tag = "Community")]
#[get("/mine")]
pub async fn mine(db: &State<Database>, user: User) -> Result<Json<Vec<v0::CommunityItem>>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageOwn)?;
    let items = db
        .fetch_community_items(CommunityItemQuery {
            creator_id: Some(&user.id),
            limit: 100,
            ..Default::default()
        })
        .await?;
    Ok(Json(
        items
            .into_iter()
            .map(|item| item.into_v0(true, false))
            .collect(),
    ))
}

/// # Review queue
///
/// Designs waiting for review, new versions of published designs and
/// reported designs. Reviewers only.
#[openapi(tag = "Community")]
#[get("/review")]
pub async fn review_queue(
    db: &State<Database>,
    user: User,
) -> Result<Json<v0::CommunityReviewQueue>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsReview)?;

    let fetch = |statuses: Vec<CommunityItemStatus>, reported: bool, pending_revision: bool| {
        db.fetch_community_items(CommunityItemQuery {
            statuses,
            reported,
            pending_revision,
            limit: 100,
            ..Default::default()
        })
    };
    let mut pending = fetch(vec![CommunityItemStatus::PendingReview], false, false).await?;
    // oldest first: first come, first served
    pending.reverse();
    let revisions = fetch(vec![CommunityItemStatus::Published], false, true).await?;
    let reported = fetch(
        vec![
            CommunityItemStatus::Published,
            CommunityItemStatus::Suspended,
        ],
        true,
        false,
    )
    .await?;

    let creators = creators_for(
        db,
        pending
            .iter()
            .chain(&revisions)
            .chain(&reported)
            .map(|i| i.creator_id.clone()),
    )
    .await?;
    let api = |list: Vec<CommunityItem>| {
        list.into_iter()
            .map(|item| item.into_v0(true, false))
            .collect()
    };
    Ok(Json(v0::CommunityReviewQueue {
        pending: api(pending),
        revisions: api(revisions),
        reported: api(reported),
        creators,
    }))
}

/// # Creator profile
///
/// A creator and their published designs, if they opened their page and are
/// an active creator. They always see their own.
#[openapi(tag = "Community")]
#[get("/creators/<id>")]
pub async fn creator_profile(
    db: &State<Database>,
    user: Option<User>,
    id: &str,
) -> Result<Json<v0::CommunityCreatorProfile>> {
    let own = user.as_ref().is_some_and(|user| user.id == id);
    if !own && !public_creators(db, &[id.to_string()]).await?.contains(id) {
        return Err(create_error!(NotFound));
    }
    let creator = creators_for(db, [id.to_string()])
        .await?
        .pop()
        .ok_or_else(|| create_error!(NotFound))?;
    let favourites = favourites_of(db, user.as_ref()).await?;
    let items = db
        .fetch_community_items(CommunityItemQuery {
            creator_id: Some(id),
            statuses: vec![CommunityItemStatus::Published],
            limit: 100,
            ..Default::default()
        })
        .await?;
    Ok(Json(v0::CommunityCreatorProfile {
        creator,
        items: items
            .into_iter()
            .map(|item| {
                let favourite = favourites.contains(&item.id);
                item.into_v0(false, favourite)
            })
            .collect(),
    }))
}

/// The designs someone saved; none for a visitor without an account
async fn favourites_of(db: &Database, user: Option<&User>) -> Result<Vec<String>> {
    match user {
        Some(user) => db.fetch_community_favourites(&user.id).await,
        None => Ok(vec![]),
    }
}

/// # Their creations
///
/// What an active creator published, for the section on their profile. It
/// carries no name or avatar and works with their page closed; it answers
/// whoever may see that section of that profile, and nobody else.
#[openapi(tag = "Community")]
#[get("/creators/<id>/designs")]
pub async fn creations(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<Vec<v0::CommunityItem>>> {
    let target = db.fetch_user(id).await?;
    if target.id != user.id {
        let mut query = DatabasePermissionQuery::new(db, &user).user(&target);
        calculate_user_permissions(&mut query)
            .await
            .throw_if_lacking_user_permission(UserPermission::ViewProfile)?;
        let band = owner_band(db, &target).await;
        if !may_see_personal_sections(band, false, target.is_friends_with(&user.id))
            || !target.shows_section("creations")
        {
            return Err(create_error!(NotFound));
        }
    }
    if !creator_is_active(db, id).await? {
        return Err(create_error!(NotFound));
    }
    let items = db
        .fetch_community_items(CommunityItemQuery {
            creator_id: Some(id),
            statuses: vec![CommunityItemStatus::Published],
            limit: 100,
            ..Default::default()
        })
        .await?;
    Ok(Json(
        items
            .into_iter()
            .map(|item| item.into_v0(false, false))
            .collect(),
    ))
}

/// Who may see what of an item
struct Access {
    own: bool,
    reviewer: bool,
}

async fn access(db: &Database, user: &User, item: &CommunityItem) -> Result<(Access, PlatformPermissions)> {
    let perms = platform_permissions(db, user).await?;
    let access = Access {
        own: item.creator_id == user.id && perms.has(PlatformPermission::CosmeticsManageOwn),
        reviewer: perms.has(PlatformPermission::CosmeticsReview),
    };
    Ok((access, perms))
}

/// # Community item details
///
/// A published design, or one of your own, or any design for reviewers.
#[openapi(tag = "Community")]
#[get("/items/<id>")]
pub async fn details(
    db: &State<Database>,
    user: Option<User>,
    id: &str,
) -> Result<Json<v0::CommunityItemDetails>> {
    let item = db.fetch_community_item(id).await?;
    let access = match user.as_ref() {
        Some(user) => access(db, user, &item).await?.0,
        None => Access {
            own: false,
            reviewer: false,
        },
    };
    let private = access.own || access.reviewer;
    if !private
        && (item.status != CommunityItemStatus::Published
            || creator_is_suspended(db, &item.creator_id).await?)
    {
        return Err(create_error!(NotFound));
    }

    let history = if private {
        db.fetch_community_reviews(id)
            .await?
            .into_iter()
            // creators don't see who reported them
            .filter(|r| access.reviewer || r.action != CommunityReviewAction::Report)
            .map(|mut r| {
                // ...nor which member of staff decided: their own entries keep
                // their id, every other one goes without (P-89)
                let own = user.as_ref().is_some_and(|user| user.id == r.actor_id);
                if !access.reviewer && !own {
                    r.actor_id.clear();
                }
                r.into()
            })
            .collect()
    } else {
        vec![]
    };
    let reporters = if access.reviewer {
        item.reports.clone()
    } else {
        vec![]
    };
    let mut creator = creators_for(db, [item.creator_id.clone()]).await?.pop();
    let favourite = favourites_of(db, user.as_ref()).await?.contains(&item.id);
    let version = item.version;
    let mut item = item.into_v0(private, favourite);
    if user.is_none()
        && !public_creators(db, &[item.creator_id.clone()])
            .await?
            .contains(&item.creator_id)
    {
        creator = None;
        item.creator_id.clear();
    }
    Ok(Json(v0::CommunityItemDetails {
        item,
        creator,
        version,
        history,
        reporters,
    }))
}

/// # My creator standing
///
/// Whether you are an active creator and, if you are suspended, why, since
/// when, and whether you can appeal. An appeal goes through
/// `POST /safety/standing/{action_id}/appeal`.
#[openapi(tag = "Community")]
#[get("/creators/@me/standing")]
pub async fn creator_standing(
    db: &State<Database>,
    user: User,
) -> Result<Json<v0::CommunityCreatorStanding>> {
    let role = db
        .fetch_platform_role(&user.id)
        .await?
        .filter(|role| role.creator.is_some())
        .ok_or_else(|| create_error!(NotFound))?;
    let standing = role.creator.as_ref().map(|c| format!("{c:?}")).unwrap_or_default();
    let mut answer = v0::CommunityCreatorStanding {
        standing,
        reason: None,
        since: None,
        action_id: None,
        can_appeal: false,
        appeal_status: None,
        appeal_note: None,
    };
    if role.creator != Some(CreatorStanding::Suspended) {
        return Ok(Json(answer));
    }

    let action = db
        .fetch_safety_actions_for_user(&user.id)
        .await?
        .into_iter()
        .filter(|a| a.kind == v0::SafetyActionKind::SuspendCreator && a.revoked.is_none())
        .max_by_key(|a| a.created_at);
    if let Some(action) = action {
        let appeal = db
            .fetch_safety_appeals_for_user(&user.id)
            .await?
            .into_iter()
            .find(|a| a.action_id == action.id);
        answer.reason = Some(action.public_reason);
        answer.since = Some(action.created_at);
        answer.can_appeal = action.appealable && appeal.is_none();
        answer.appeal_status = appeal.as_ref().map(|a| a.status);
        answer.appeal_note = appeal.and_then(|a| a.decision_note);
        answer.action_id = Some(action.id);
    } else if let Some(entry) = db
        .fetch_creator_logs(&user.id)
        .await?
        .into_iter()
        .filter(|entry| entry.action == revolt_database::CreatorAction::Suspend)
        .max_by_key(|entry| entry.created_at)
    {
        // suspended before reasons were recorded as actions
        answer.reason = entry.reason;
        answer.since = Some(entry.created_at);
    }
    Ok(Json(answer))
}

/// # Download my design
///
/// Where to fetch your own copy of a design, in any state and also while
/// suspended. The file answers your session only, unless it is public.
#[openapi(tag = "Community")]
#[get("/items/<id>/download")]
pub async fn download(
    db: &State<Database>,
    user: User,
    id: &str,
) -> Result<Json<v0::CommunityItemDownload>> {
    let item = db.fetch_community_item(id).await?;
    let (access, _) = access(db, &user, &item).await?;
    if !access.own {
        return Err(create_error!(NotFound));
    }
    let base = config().await.hosts.autumn.clone();
    Ok(Json(v0::CommunityItemDownload {
        url: format!(
            "{}/{}/{}/original",
            base.trim_end_matches('/'),
            tag_of(&item.kind),
            item.file.id
        ),
        filename: item.file.filename,
    }))
}

/// # Edit community item
///
/// Change your own design. Drafts and designs sent back to you change in
/// place. On a published design, changes are kept as a draft revision, or
/// go live at once with `submit`.
#[openapi(tag = "Community")]
#[patch("/items/<id>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataEditCommunityItem>,
) -> Result<Json<v0::CommunityItem>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsSubmit)?;
    let data = validated(data.into_inner())?;
    let before = db.fetch_community_item(id).await?;
    if before.creator_id != user.id {
        return Err(create_error!(NotFound));
    }
    let submit = data.submit.unwrap_or(false);

    let name = data.name.as_deref().map(clean_name).transpose()?;
    let tags = data.tags.as_deref().map(community_tags);
    let description = data.description.map(|d| clean_description(Some(d)));
    check_effect_words(
        &before.kind,
        name.as_deref(),
        description.as_ref().and_then(|d| d.as_deref()),
        tags.as_deref(),
    )?;
    let rights = rights_of(data.rights)?;
    // before the file is claimed: a refused change leaves the upload usable
    if submit && rights.is_none() && (data.file.is_some() || !promise_stands(&before)) {
        return Err(rights_required());
    }
    let file = match &data.file {
        Some(file) => Some(claim_file(db, file, &before.kind, id, &user).await?),
        None => None,
    };

    let mut after = before.clone();
    // a new file needs a new promise; the old one was about the old file
    if file.is_some() {
        after.rights = None;
    }
    if rights.is_some() {
        after.rights = rights;
    }
    let review_first = before.kind == CommunityItemKind::Effect;
    let mut new_file = None;
    match before.status {
        CommunityItemStatus::Draft
        | CommunityItemStatus::ChangesRequested
        | CommunityItemStatus::Rejected
        | CommunityItemStatus::PendingReview => {
            if let Some(name) = name {
                after.name = name;
            }
            if let Some(file) = file {
                after.file = file;
            }
            if let Some(tags) = tags {
                after.tags = tags;
            }
            if let Some(description) = description {
                after.description = description;
            }
            // a reviewer must not approve what they did not see
            if before.status == CommunityItemStatus::PendingReview {
                after.status = CommunityItemStatus::Draft;
            }
            if submit {
                publish(&mut after, &before.status)?;
            }
        }
        CommunityItemStatus::Published => {
            let base = before.pending.clone().unwrap_or(CommunityRevision {
                revision: before.revision + 1,
                name: before.name.clone(),
                description: before.description.clone(),
                tags: before.tags.clone(),
                file: before.file.clone(),
                status: CommunityItemStatus::Draft,
                review_note: None,
                submitted_at: now_ms(),
            });
            let revision = CommunityRevision {
                name: name.unwrap_or(base.name),
                description: description.unwrap_or(base.description),
                tags: tags.unwrap_or(base.tags),
                file: file.unwrap_or(base.file),
                // as above: changed, it waits to be sent again
                status: if base.status == CommunityItemStatus::PendingReview {
                    CommunityItemStatus::Draft
                } else {
                    base.status
                },
                ..base
            };
            if submit && review_first {
                require_rights(&after)?;
                after.pending = Some(CommunityRevision {
                    status: CommunityItemStatus::PendingReview,
                    ..revision
                });
            } else if submit {
                require_rights(&after)?;
                after.pending = None;
                after.review_note = None;
                new_file = apply_revision(&mut after, revision);
            } else {
                after.pending = Some(revision);
            }
        }
        CommunityItemStatus::Suspended | CommunityItemStatus::Archived => {
            return Err(create_error!(InvalidOperation));
        }
    }

    let after = save(db, &before, after, false).await?;
    if let Some(file) = new_file {
        db.set_community_item_file_on_users(id, &file).await?;
    }
    if submit {
        log(db, &after, &user.id, sent_action(&after), None).await?;
    }
    Ok(Json(after.into_v0(true, false)))
}

/// # Publish
///
/// Publish your draft, a design that was sent back, or the new version of
/// a published design. It goes live straight away; an effect goes to the
/// reviewers instead and goes live when one approves it.
#[openapi(tag = "Community")]
#[post("/items/<id>/submit")]
pub async fn submit(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::CommunityItem>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsSubmit)?;
    let before = db.fetch_community_item(id).await?;
    if before.creator_id != user.id {
        return Err(create_error!(NotFound));
    }

    let mut after = before.clone();
    let mut new_file = None;
    match (&before.status, after.pending.take()) {
        (CommunityItemStatus::Published, Some(revision))
            if before.kind == CommunityItemKind::Effect =>
        {
            require_rights(&after)?;
            after.pending = Some(CommunityRevision {
                status: CommunityItemStatus::PendingReview,
                ..revision
            });
        }
        (CommunityItemStatus::Published, Some(revision)) => {
            require_rights(&after)?;
            after.review_note = None;
            new_file = apply_revision(&mut after, revision);
        }
        (status, pending) => {
            after.pending = pending;
            publish(&mut after, status)?;
        }
    }
    let after = save(db, &before, after, false).await?;
    if let Some(file) = new_file {
        db.set_community_item_file_on_users(id, &file).await?;
    }
    log(db, &after, &user.id, sent_action(&after), None).await?;
    Ok(Json(after.into_v0(true, false)))
}

/// A reviewer decision on an item or its pending revision
async fn decide(
    db: &Database,
    user: &User,
    id: &str,
    action: CommunityReviewAction,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    let perms = platform_permissions(db, user).await?;
    require(&perms, PlatformPermission::CosmeticsReview)?;
    let data = match data {
        Some(data) => validated(data.into_inner())?,
        None => v0::DataCommunityDecision {
            reason: None,
            version: None,
        },
    };
    // nobody approves their own work, unless they can override anything
    let before = db.fetch_community_item(id).await?;
    if before.creator_id == user.id && !perms.has(PlatformPermission::CosmeticsManageAll) {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsManageAll.name().to_string()
        }));
    }
    decide_as(db, user, id, action, data).await.map(Json)
}

/// The decision itself, for the reviewer routes and the admin console
pub(crate) async fn decide_as(
    db: &Database,
    user: &User,
    id: &str,
    action: CommunityReviewAction,
    data: v0::DataCommunityDecision,
) -> Result<v0::CommunityItem> {
    let reason = clean_description(data.reason);
    if reason.is_none()
        && matches!(
            action,
            CommunityReviewAction::Reject
                | CommunityReviewAction::RequestChanges
                | CommunityReviewAction::Suspend
        )
    {
        return Err(revolt_result::failed_validation(vec![
            revolt_result::ValidationProblem::new("reason", "required"),
        ]));
    }

    let before = db.fetch_community_item(id).await?;
    if data.version.is_some_and(|v| v != before.version) {
        return Err(create_error!(CommunityItemConflict));
    }
    if action == CommunityReviewAction::Approve {
        require_rights_to_approve(&before)?;
    }

    let mut after = before.clone();
    after.reviewed_by = Some(user.id.clone());
    let mut clear_reports = false;
    let mut new_file = None;

    let revision_waiting = before.status == CommunityItemStatus::Published
        && before
            .pending
            .as_ref()
            .is_some_and(|p| p.status == CommunityItemStatus::PendingReview);

    if revision_waiting && action != CommunityReviewAction::Suspend {
        let revision = after.pending.take().expect("pending revision");
        let status = community_transition(&revision.status, &action)
            .ok_or_else(|| create_error!(InvalidOperation))?;
        if status == CommunityItemStatus::Published {
            new_file = apply_revision(&mut after, revision);
            after.review_note = reason.clone();
        } else {
            after.pending = Some(CommunityRevision {
                status,
                review_note: reason.clone(),
                ..revision
            });
        }
    } else {
        // "keep" only means something for a reported design; a second
        // approval of something already published is refused
        if before.status == CommunityItemStatus::Published
            && action == CommunityReviewAction::Approve
            && before.reports.is_empty()
        {
            return Err(create_error!(InvalidOperation));
        }
        after.status = community_transition(&before.status, &action)
            .ok_or_else(|| create_error!(InvalidOperation))?;
        after.review_note = reason.clone();
        if after.status == CommunityItemStatus::Published {
            after.published_at.get_or_insert_with(now_ms);
            // keeping a reported design clears its reports
            clear_reports = true;
        }
    }

    let after = save(db, &before, after, clear_reports).await?;
    if let Some(file) = new_file {
        db.set_community_item_file_on_users(id, &file).await?;
    }
    if after.status == CommunityItemStatus::Suspended {
        took_down(db, &user.id, &after, reason.as_deref().unwrap_or_default(), false).await?;
    }
    log(db, &after, &user.id, action, reason).await?;
    Ok(after.into_v0(true, false))
}

/// # Approve
///
/// Publish a design (or its new version), reinstate a suspended one or
/// keep a reported one. Reviewers only. Two reviewers acting at once: only
/// the first decision counts, the other gets a conflict.
#[openapi(tag = "Community")]
#[post("/items/<id>/approve", data = "<data>")]
pub async fn approve(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    decide(db, &user, id, CommunityReviewAction::Approve, data).await
}

/// # Reject
///
/// Reviewers only; a reason is required.
#[openapi(tag = "Community")]
#[post("/items/<id>/reject", data = "<data>")]
pub async fn reject(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    decide(db, &user, id, CommunityReviewAction::Reject, data).await
}

/// # Request changes
///
/// Send a design back to its creator; a reason is required.
#[openapi(tag = "Community")]
#[post("/items/<id>/request-changes", data = "<data>")]
pub async fn request_changes(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    decide(db, &user, id, CommunityReviewAction::RequestChanges, data).await
}

/// # Suspend
///
/// Take a published design out of the gallery and off everyone wearing it;
/// a reason is required.
#[openapi(tag = "Community")]
#[post("/items/<id>/suspend", data = "<data>")]
pub async fn suspend(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    decide(db, &user, id, CommunityReviewAction::Suspend, data).await
}

/// # Archive
///
/// The creator (or an admin) withdraws a design. It leaves the gallery and
/// everyone wearing it. The image is kept. A withdrawal by an admin is a
/// takedown: it may carry a `reason`, and the creator gets a statement of
/// reasons and can appeal, whether the design was public or not.
#[openapi(tag = "Community")]
#[post("/items/<id>/archive", data = "<data>")]
pub async fn archive(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Option<Json<v0::DataCommunityDecision>>,
) -> Result<Json<v0::CommunityItem>> {
    // staff may say why; the creator reads it with the statement of reasons
    let reason = match data {
        Some(data) => clean_description(validated(data.into_inner())?.reason),
        None => None,
    };
    let before = db.fetch_community_item(id).await?;
    let (access, perms) = access(db, &user, &before).await?;
    if !access.own && !perms.has(PlatformPermission::CosmeticsManageAll) {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsManageOwn.name().to_string()
        }));
    }
    // read-only while suspended: nothing changes until a decision is made
    if !perms.has(PlatformPermission::CosmeticsManageAll)
        && creator_is_suspended(db, &user.id).await?
    {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsSubmit.name().to_string()
        }));
    }
    let mut after = before.clone();
    after.status = community_transition(&before.status, &CommunityReviewAction::Archive)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    after.pending = None;
    let after = save(db, &before, after, false).await?;
    let by_staff = after.creator_id != user.id;
    if by_staff {
        // published or not: the creator gets a statement and can appeal (D-49)
        let said = reason.as_deref().unwrap_or("Withdrawn by Zrzur staff");
        took_down(db, &user.id, &after, said, false).await?;
    } else {
        db.unset_community_item_from_users(id).await?;
    }
    let logged = if by_staff { reason } else { None };
    log(db, &after, &user.id, CommunityReviewAction::Archive, logged).await?;
    Ok(Json(after.into_v0(true, false)))
}

/// # Restore
///
/// The creator takes a withdrawn design back. It becomes a draft again, with
/// its image, name and tags; nobody sees it until it is published like any
/// other draft. It counts against the creator's limit again. A design that
/// staff withdrew is not the creator's to take back.
#[openapi(tag = "Community")]
#[post("/items/<id>/restore")]
pub async fn restore(db: &State<Database>, user: User, id: &str) -> Result<Json<v0::CommunityItem>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsSubmit)?;
    let before = db.fetch_community_item(id).await?;
    if before.creator_id != user.id {
        return Err(create_error!(NotFound));
    }
    effects_open(&before.kind).await?;
    let mut after = before.clone();
    after.status = community_transition(&before.status, &CommunityReviewAction::Restore)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    // only what the creator withdrew themself: a design staff withdrew is a
    // takedown, and comes back through an appeal, never by this button. With
    // no record of who withdrew it, it stays where it is.
    let withdrawn_by = db
        .fetch_community_reviews(id)
        .await?
        .into_iter()
        .filter(|entry| entry.action == CommunityReviewAction::Archive)
        .max_by_key(|entry| entry.created_at)
        .map(|entry| entry.actor_id);
    if withdrawn_by.as_deref() != Some(user.id.as_str()) {
        return Err(create_error!(InvalidOperation));
    }
    let max = config().await.features.community.max_items_per_creator;
    if db
        .count_community_items_by_creator(&user.id, &ACTIVE_STATUSES)
        .await?
        >= max
    {
        return Err(create_error!(TooManyCommunityItems { max }));
    }
    after.review_note = None;
    after.pending = None;
    let after = save(db, &before, after, false).await?;
    log(db, &after, &user.id, CommunityReviewAction::Restore, None).await?;
    Ok(Json(after.into_v0(true, false)))
}

/// # Delete
///
/// The creator deletes one of their own designs for good, in one step,
/// published ones included (owner, 2026-11-25: "anything they publish, they
/// can delete"). It leaves the gallery and everyone wearing it, and its
/// files are deleted and leave the CDN; a file held as Safety evidence stays
/// until its case is over. A design taken down by a reviewer stays until the
/// review is over, and so does everything while the creator is suspended.
/// Staff delete other people's designs with `POST /community/items/{id}/erase`.
#[openapi(tag = "Community")]
#[delete("/items/<id>")]
pub async fn delete_item(db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    let item = db.fetch_community_item(id).await?;
    // deleting is the creator's own right, with or without a creator rank
    if item.creator_id != user.id {
        return Err(create_error!(NotFound));
    }
    if creator_is_suspended(db, &user.id).await? {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsSubmit.name().to_string()
        }));
    }
    if item.status == CommunityItemStatus::Suspended {
        return Err(create_error!(InvalidOperation));
    }
    // the record first, so staff can see who deleted what after the design
    // and its history are gone: no record, no deletion
    let mut record = AuditRecord {
        actor_id: user.id.clone(),
        action: "community.item.delete".to_string(),
        target_kind: Some("community_item".to_string()),
        target_id: Some(item.id.clone()),
        // ids and states only: the design's name and text are the creator's
        summary: Some(format!("{:?} by its creator, was {:?}", item.kind, item.status)),
        ..Default::default()
    };
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    // a reviewer acting now must not lose to the deletion: the design is
    // claimed (its version moves on) before anything is erased
    let mut claimed = item.clone();
    claimed.version = item.version + 1;
    let erased = async {
        if !db.update_community_item(&claimed, item.version, false).await? {
            return Err(create_error!(CommunityItemConflict));
        }
        erase_community_item(db, &item.id).await
    }
    .await;
    if let Err(error) = erased {
        record.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(error);
    }
    if item.status == CommunityItemStatus::Published {
        sync_creator_badge(db, &item.creator_id).await;
    }
    Ok(EmptyResponse)
}

/// # Erase (staff)
///
/// Staff delete any creator's design for good, in any state, one a reviewer
/// already took down included. Needs a held Founder, Zrzur Staff or Platform
/// Moderator rank (the owner decided, 2026-11-25, that these ranks grant this
/// power) or `cosmetics.manage_all`, and a `reason`; send the `version` you
/// looked at to fail if the design changed since.
/// It leaves the gallery and everyone wearing it, and its files leave the
/// CDN. The creator gets a statement of reasons and can appeal; the image
/// is kept only as evidence for that case. The platform audit log records
/// who erased what and why. Creators delete their own designs with
/// `DELETE /community/items/{id}`.
#[openapi(tag = "Community")]
#[post("/items/<id>/erase", data = "<data>")]
pub async fn erase(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataCommunityDecision>,
) -> Result<EmptyResponse> {
    let perms = platform_permissions(db, &user).await?;
    if !may_delete_any(db, &user, &perms).await? {
        return Err(create_error!(MissingPlatformPermission {
            permission: PlatformPermission::CosmeticsManageAll.name().to_string()
        }));
    }
    let data = validated(data.into_inner())?;
    let reason = clean_description(data.reason).ok_or_else(|| {
        revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new("reason", "required")])
    })?;
    let item = db.fetch_community_item(id).await?;
    if data.version.is_some_and(|version| version != item.version) {
        return Err(create_error!(CommunityItemConflict));
    }

    // the record first: no record, no erasure
    let mut record = AuditRecord {
        actor_id: user.id.clone(),
        action: "community.item.erase".to_string(),
        target_kind: Some("community_item".to_string()),
        target_id: Some(item.id.clone()),
        reason: Some(reason.clone()),
        // ids and states only: the design's name and text are the creator's
        summary: Some(format!(
            "{:?} by {}, was {:?}",
            item.kind, item.creator_id, item.status
        )),
        ..Default::default()
    };
    record_platform_audit(db, record.clone(), AuditOutcome::Success).await?;
    let erased = async {
        // statement of reasons, appeal and evidence first: the case keeps the
        // image (as reported) once the design is gone; wearers are told
        took_down(db, &user.id, &item, &reason, false).await?;
        erase_community_item(db, &item.id).await
    }
    .await;
    if let Err(error) = erased {
        record.summary = Some(format!("{:?}", error.error_type).chars().take(200).collect());
        // best effort: the first record already shows the attempt
        let _ = record_platform_audit(db, record, AuditOutcome::Failed).await;
        return Err(error);
    }
    if item.status == CommunityItemStatus::Published {
        sync_creator_badge(db, &item.creator_id).await;
    }
    Ok(EmptyResponse)
}

/// # Override review
///
/// Admins reopen any decision: the design goes back to the review queue.
/// Logged with the reason.
#[openapi(tag = "Community")]
#[post("/items/<id>/override", data = "<data>")]
pub async fn override_review(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataCommunityDecision>,
) -> Result<Json<v0::CommunityItem>> {
    let perms = platform_permissions(db, &user).await?;
    require(&perms, PlatformPermission::CosmeticsManageAll)?;
    let reason = clean_description(validated(data.into_inner())?.reason).ok_or_else(|| {
        revolt_result::failed_validation(vec![revolt_result::ValidationProblem::new("reason", "required")])
    })?;
    let before = db.fetch_community_item(id).await?;
    let mut after = before.clone();
    after.status = community_transition(&before.status, &CommunityReviewAction::Override)
        .ok_or_else(|| create_error!(InvalidOperation))?;
    after.reviewed_by = Some(user.id.clone());
    after.review_note = Some(reason.clone());
    let after = save(db, &before, after, false).await?;
    if before.status == CommunityItemStatus::Published {
        took_down(db, &user.id, &after, &reason, false).await?;
    }
    log(db, &after, &user.id, CommunityReviewAction::Override, Some(reason)).await?;
    Ok(Json(after.into_v0(true, false)))
}

/// # Favourite
///
/// Save a published design to your favourites.
#[openapi(tag = "Community")]
#[put("/items/<id>/favourite")]
pub async fn favourite(db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    let item = db.fetch_community_item(id).await?;
    if item.status != CommunityItemStatus::Published
        || creator_is_suspended(db, &item.creator_id).await?
    {
        return Err(create_error!(NotFound));
    }
    db.add_community_favourite(&user.id, id).await?;
    Ok(EmptyResponse)
}

/// # Unfavourite
#[openapi(tag = "Community")]
#[delete("/items/<id>/favourite")]
pub async fn unfavourite(db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    db.remove_community_favourite(&user.id, id).await?;
    Ok(EmptyResponse)
}

/// # Report community item
///
/// Each user counts once and every report puts the design in front of the
/// reviewers. After several reports it is suspended until a reviewer looks.
/// You can't report your own design.
#[openapi(tag = "Community")]
#[post("/items/<id>/report")]
pub async fn report(db: &State<Database>, user: User, id: &str) -> Result<EmptyResponse> {
    let item = db.fetch_community_item(id).await?;
    if item.creator_id == user.id {
        return Err(create_error!(CannotReportYourself));
    }
    if item.status != CommunityItemStatus::Published || item.reports.contains(&user.id) {
        return Ok(EmptyResponse);
    }

    let reports = db.report_community_item(id, &user.id).await?;
    log(db, &item, &user.id, CommunityReviewAction::Report, None).await?;
    if reports >= COMMUNITY_REPORTS_TO_HIDE {
        let before = db.fetch_community_item(id).await?;
        let mut after = before.clone();
        if let Some(status) = community_transition(&before.status, &CommunityReviewAction::Suspend) {
            after.status = status;
            after.review_note = Some("Suspended after reports".to_string());
            // someone else acting first is fine
            if let Ok(after) = save(db, &before, after, false).await {
                // no person has looked yet: the record says so, and a reviewer
                // gets it as an open case
                took_down(db, SYSTEM_ACTOR, &after, "Taken out after reports", true).await?;
                log(
                    db,
                    &after,
                    SYSTEM_ACTOR,
                    CommunityReviewAction::Suspend,
                    Some(format!("{reports} reports")),
                )
                .await?;
            }
        }
    }
    Ok(EmptyResponse)
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod kind_open_tests {
    use super::*;

    #[test]
    fn community_frames_follow_their_own_switch() {
        let mut community = revolt_config::FeaturesCommunity::default();
        // off by default: community frames are not approved yet
        assert!(!community.frames_enabled);
        assert!(!kind_open(&CommunityItemKind::Frame, &community));
        assert!(kind_open(&CommunityItemKind::Decoration, &community));
        assert!(kind_open(&CommunityItemKind::Nameplate, &community));
        community.frames_enabled = true;
        assert!(kind_open(&CommunityItemKind::Frame, &community));
        community.effects_enabled = false;
        assert!(!kind_open(&CommunityItemKind::Effect, &community));
        assert!(kind_open(&CommunityItemKind::Frame, &community));
    }
}
