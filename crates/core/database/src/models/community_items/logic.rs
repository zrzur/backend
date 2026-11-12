//! Review rules, platform permissions and image checks for community
//! designs. Pure functions, so every route and service uses the same rules.

use std::collections::BTreeSet;

use crate::{
    util::{age::owner_band, cdn_purge::purge_files_later},
    CommunityItemQuery, CommunityItemStatus, CommunityReviewAction, CreatorStanding,
    PlatformPermission, PlatformRole, User,
};

/// The status an item (or pending revision) moves to, or `None` if the
/// action is not allowed from `from`. This is the only place transitions
/// are defined.
pub fn community_transition(
    from: &CommunityItemStatus,
    action: &CommunityReviewAction,
) -> Option<CommunityItemStatus> {
    use CommunityItemStatus::*;
    use CommunityReviewAction as A;
    match (from, action) {
        // creators are trusted team members: they publish their own drafts,
        // fixed or rejected designs, and anything still waiting from before
        // review was dropped, straight away
        (Draft | ChangesRequested | Rejected | PendingReview, A::Publish) => Some(Published),
        (PendingReview, A::Approve) => Some(Published),
        (PendingReview, A::Reject) => Some(Rejected),
        (PendingReview, A::RequestChanges) => Some(ChangesRequested),
        // reviewers take a published design down, or put it back
        (Published | PendingReview, A::Suspend) => Some(Suspended),
        (Suspended, A::Approve) => Some(Published),
        // a reviewer keeps a reported design: it stays published
        (Published, A::Approve) => Some(Published),
        (Archived, A::Archive) => None,
        (_, A::Archive) => Some(Archived),
        // an admin can reopen any decision; it goes back to the queue
        (Published | Rejected | Suspended | Archived, A::Override) => Some(PendingReview),
        _ => None,
    }
}

/// Effective platform permissions of a user, read from `platform_roles`
pub async fn platform_permissions(
    db: &crate::Database,
    user: &User,
) -> revolt_result::Result<PlatformPermissions> {
    let role = db.fetch_platform_role(&user.id).await?;
    let mut perms = PlatformPermissions::resolve(user, role.as_ref());
    // Whoever is under 18 now, or never said, cannot publish, whatever rank
    // they hold: every route and the upload server ask here
    if !user.privileged
        && perms.has(PlatformPermission::CosmeticsSubmit)
        && !is_declared_adult(db, user).await
    {
        perms.0.remove(&PlatformPermission::CosmeticsSubmit);
    }
    Ok(perms)
}

/// The creator rank is 18+ (owner, 2026-11-06). A declared age is not proof,
/// and no declared age counts as a minor.
pub async fn is_declared_adult(db: &crate::Database, user: &User) -> bool {
    owner_band(db, user).await == revolt_models::v0::AgeBand::AdultDeclared
}

/// One design goes for good, with every file it ever had. Wearers come off
/// first, so no profile points at nothing; a file kept as Safety evidence
/// stays until its case is over. Safe to run twice.
pub async fn erase_community_item(db: &crate::Database, id: &str) -> revolt_result::Result<()> {
    db.unset_community_item_from_users(id).await?;
    let files = db.fetch_community_item_files(id).await?;
    let ids: Vec<String> = files.iter().map(|file| file.id.clone()).collect();
    db.mark_attachments_as_deleted(&ids).await?;
    purge_files_later(files);
    db.delete_community_item(id).await
}

/// An account is erased: everything its owner made on Designs goes, whatever
/// its state (owner, 2026-11-06). Safe to run twice; a failure leaves the rest
/// for the next try. Wearers come off first, so no profile points at nothing,
/// and a file kept as Safety evidence stays until its case is over.
pub async fn erase_community_items_of(
    db: &crate::Database,
    creator_id: &str,
) -> revolt_result::Result<()> {
    loop {
        let items = db
            .fetch_community_items(CommunityItemQuery {
                creator_id: Some(creator_id),
                limit: 100,
                ..Default::default()
            })
            .await?;
        if items.is_empty() {
            break;
        }
        for item in items {
            erase_community_item(db, &item.id).await?;
        }
    }
    // no rank and no public name left behind on a deleted user
    if let Some(mut role) = db.fetch_platform_role(creator_id).await? {
        role.permissions.clear();
        role.creator = None;
        role.show_public_name = false;
        role.updated_at = crate::epoch_ms(iso8601_timestamp::Timestamp::now_utc());
        role.updated_by = creator_id.to_string();
        db.save_platform_role(&role).await?;
    }
    Ok(())
}

/// Who may download a `decorations` file
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommunityFileAccess {
    /// The current image of a published design: anyone, cacheable
    Public,
    /// Not public, but this viewer is its creator or a reviewer
    Private,
    /// Not public and not this viewer's
    Denied,
}

/// Whether this creator's designs are hidden: true for as long as the
/// creator is suspended, and nothing has to be restored on reinstatement
pub async fn creator_is_suspended(
    db: &crate::Database,
    creator_id: &str,
) -> revolt_result::Result<bool> {
    Ok(db
        .fetch_platform_role(creator_id)
        .await?
        .is_some_and(|role| role.creator == Some(CreatorStanding::Suspended)))
}

/// Whether this person is a creator in good standing right now: a suspended
/// or revoked creator is not, whatever they published
pub async fn creator_is_active(
    db: &crate::Database,
    creator_id: &str,
) -> revolt_result::Result<bool> {
    Ok(db
        .fetch_platform_role(creator_id)
        .await?
        .is_some_and(|role| role.creator == Some(CreatorStanding::Active)))
}

/// Whether a `decorations` file may be served. Drafts, designs in review,
/// rejected or suspended ones, designs of a suspended creator and old or
/// pending revisions are only shown to their creator and to reviewers.
pub async fn community_file_access(
    db: &crate::Database,
    file: &crate::File,
    viewer: Option<&User>,
) -> revolt_result::Result<CommunityFileAccess> {
    let Some(used_for) = &file.used_for else {
        return Ok(CommunityFileAccess::Denied);
    };
    if used_for.object_type != crate::FileUsedForType::CommunityItem {
        return Ok(CommunityFileAccess::Denied);
    }
    let Ok(item) = db.fetch_community_item(&used_for.id).await else {
        return Ok(CommunityFileAccess::Denied);
    };
    if item.status == CommunityItemStatus::Published
        && item.file.id == file.id
        && !creator_is_suspended(db, &item.creator_id).await?
    {
        return Ok(CommunityFileAccess::Public);
    }
    let Some(viewer) = viewer else {
        return Ok(CommunityFileAccess::Denied);
    };
    let perms = platform_permissions(db, viewer).await?;
    // decisions are made in the admin console: the staff who make them look at
    // what they decide on
    let staff = db.fetch_staff_grant(&viewer.id).await?.is_some_and(|grant| {
        grant
            .effective(crate::staff_now())
            .contains(&crate::StaffPermission::ReportsRead)
    });
    let allowed = staff
        || perms.has(PlatformPermission::CosmeticsReview)
        || (item.creator_id == viewer.id && perms.has(PlatformPermission::CosmeticsManageOwn));
    Ok(if allowed {
        CommunityFileAccess::Private
    } else {
        CommunityFileAccess::Denied
    })
}

/// Statuses shown in public lists, search and selectors
pub const COMMUNITY_PUBLIC: CommunityItemStatus = CommunityItemStatus::Published;

/// Effective platform permissions of a user
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformPermissions(pub BTreeSet<PlatformPermission>);

impl PlatformPermissions {
    /// Work out what a user may do. `privileged` users can do everything.
    /// A creator who is suspended or revoked can't submit or revise, but
    /// still sees their own designs.
    pub fn resolve(user: &User, role: Option<&PlatformRole>) -> Self {
        use PlatformPermission::*;
        if user.privileged {
            return Self(
                [
                    CosmeticsSubmit,
                    CosmeticsManageOwn,
                    CosmeticsReview,
                    CosmeticsManageCreators,
                    CosmeticsManageAll,
                    BadgesGrant,
                    BetaManage,
                    BugsReview,
                    BugsSecurity,
                    SafetyReview,
                    SafetyAction,
                    SafetyManage,
                    PluginsReview,
                ]
                .into_iter()
                .collect(),
            );
        }
        if user.bot.is_some() {
            return Self::default();
        }

        let Some(role) = role else {
            return Self::default();
        };
        let mut set: BTreeSet<_> = role.permissions.iter().copied().collect();
        if set.contains(&CosmeticsManageAll) {
            set.extend([CosmeticsReview, CosmeticsManageCreators, CosmeticsManageOwn]);
        }
        // a senior reviewer can do what reviewers do; acting needs review access
        if set.contains(&SafetyManage) {
            set.extend([SafetyReview, SafetyAction]);
        }
        if set.contains(&SafetyAction) {
            set.insert(SafetyReview);
        }
        match role.creator {
            Some(CreatorStanding::Active) => {
                set.insert(CosmeticsManageOwn);
            }
            Some(CreatorStanding::Suspended | CreatorStanding::Revoked) => {
                set.remove(&CosmeticsSubmit);
                set.insert(CosmeticsManageOwn);
            }
            None => {
                // submitting needs an active creator record
                set.remove(&CosmeticsSubmit);
            }
        }
        Self(set)
    }

    /// Whether a permission is about community designs (the only ones a
    /// `cosmetics.manage_all` admin may hand out; the rest need a
    /// privileged account)
    pub fn is_cosmetics(permission: PlatformPermission) -> bool {
        use PlatformPermission::*;
        matches!(
            permission,
            CosmeticsSubmit
                | CosmeticsManageOwn
                | CosmeticsReview
                | CosmeticsManageCreators
                | CosmeticsManageAll
        )
    }

    pub fn has(&self, permission: PlatformPermission) -> bool {
        self.0.contains(&permission)
    }

    /// Names as sent to clients (`cosmetics.submit`, ...)
    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|p| p.name().to_string()).collect()
    }
}

impl PlatformPermission {
    pub fn name(&self) -> &'static str {
        match self {
            PlatformPermission::CosmeticsSubmit => "cosmetics.submit",
            PlatformPermission::CosmeticsManageOwn => "cosmetics.manage_own",
            PlatformPermission::CosmeticsReview => "cosmetics.review",
            PlatformPermission::CosmeticsManageCreators => "cosmetics.manage_creators",
            PlatformPermission::CosmeticsManageAll => "cosmetics.manage_all",
            PlatformPermission::BadgesGrant => "badges.grant",
            PlatformPermission::BetaManage => "beta.manage",
            PlatformPermission::BugsReview => "bugs.review",
            PlatformPermission::BugsSecurity => "bugs.security",
            PlatformPermission::SafetyReview => "safety.review",
            PlatformPermission::SafetyAction => "safety.action",
            PlatformPermission::SafetyManage => "safety.manage",
            PlatformPermission::PluginsReview => "plugins.review",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "cosmetics.submit" => PlatformPermission::CosmeticsSubmit,
            "cosmetics.manage_own" => PlatformPermission::CosmeticsManageOwn,
            "cosmetics.review" => PlatformPermission::CosmeticsReview,
            "cosmetics.manage_creators" => PlatformPermission::CosmeticsManageCreators,
            "cosmetics.manage_all" => PlatformPermission::CosmeticsManageAll,
            "badges.grant" => PlatformPermission::BadgesGrant,
            "beta.manage" => PlatformPermission::BetaManage,
            "bugs.review" => PlatformPermission::BugsReview,
            "bugs.security" => PlatformPermission::BugsSecurity,
            "safety.review" => PlatformPermission::SafetyReview,
            "safety.action" => PlatformPermission::SafetyAction,
            "safety.manage" => PlatformPermission::SafetyManage,
            "plugins.review" => PlatformPermission::PluginsReview,
            _ => return None,
        })
    }
}

/// Limits for a community image
#[derive(Debug, Clone, Copy)]
pub struct CommunityImageLimits {
    pub max_bytes: usize,
    /// Largest width or height in pixels
    pub max_dimension: u32,
    pub max_frames: u32,
    /// Total length of one loop of an animation
    pub max_duration_ms: u32,
}

/// What was found in a community image
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityImageInfo {
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub duration_ms: u32,
    /// How many times it plays; 0 is for ever
    pub plays: u32,
    /// Shortest time one frame shows; `u32::MAX` for a still image
    pub shortest_delay_ms: u32,
}

/// Why a community image was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommunityImageError {
    TooLarge,
    /// Not PNG / APNG / GIF / WebP by its bytes (SVG, HTML, JS, ...)
    BadSignature,
    /// The file is cut short or malformed
    Corrupt,
    TooManyPixels,
    TooManyFrames,
    TooLong,
    /// An effect has to move
    NotAnimated,
    /// An effect plays once
    Repeats,
    /// Frames faster than the limit: 0 ms delays are how strobes are made
    FramesTooFast,
    /// Not the 9:16 shape of a profile
    WrongShape,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as u32)
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 3)
        .map(|s| s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// PNG / APNG: IHDR size, acTL frame count, fcTL delays
fn inspect_png(b: &[u8]) -> Option<CommunityImageInfo> {
    let mut at = 8;
    let (mut width, mut height) = (0, 0);
    let mut frames = 1;
    let mut controls = 0;
    let mut duration_ms: u64 = 0;
    let mut animated = false;
    let mut seen_end = false;
    let mut plays = 1;
    let mut shortest_delay_ms = u32::MAX;
    while at + 8 <= b.len() {
        let len = be32(b, at)? as usize;
        let kind = b.get(at + 4..at + 8)?;
        let data = b.get(at + 8..at + 8 + len)?;
        match kind {
            b"IHDR" => {
                width = be32(data, 0)?;
                height = be32(data, 4)?;
            }
            b"acTL" => {
                animated = true;
                frames = be32(data, 0)?;
                plays = be32(data, 4)?;
            }
            b"fcTL" => {
                controls += 1;
                let num = u16::from_be_bytes([*data.get(20)?, *data.get(21)?]) as u64;
                let mut den = u16::from_be_bytes([*data.get(22)?, *data.get(23)?]) as u64;
                if den == 0 {
                    den = 100;
                }
                let delay = num * 1000 / den;
                duration_ms += delay;
                shortest_delay_ms = shortest_delay_ms.min(delay.min(u32::MAX as u64) as u32);
            }
            b"IEND" => {
                seen_end = true;
                break;
            }
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 || height == 0 || !seen_end {
        return None;
    }
    Some(CommunityImageInfo {
        mime: if animated { "image/apng" } else { "image/png" },
        width,
        height,
        // the file may say fewer frames than it has
        frames: frames.max(controls).max(1),
        duration_ms: duration_ms.min(u32::MAX as u64) as u32,
        plays,
        shortest_delay_ms,
    })
}

/// GIF: logical screen size, image descriptors and graphic-control delays
fn inspect_gif(b: &[u8]) -> Option<CommunityImageInfo> {
    let width = le16(b, 6)?;
    let height = le16(b, 8)?;
    let flags = *b.get(10)?;
    let mut at = 13;
    if flags & 0x80 != 0 {
        at += 3 * (1 << ((flags & 0x07) + 1));
    }
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let size = *b.get(at)? as usize;
            at += 1;
            if size == 0 {
                return Some(at);
            }
            at += size;
        }
    };
    let mut frames = 0u32;
    let mut duration_ms: u64 = 0;
    let mut plays = 1;
    let mut shortest_delay_ms = u32::MAX;
    loop {
        match *b.get(at)? {
            0x3B => break,
            0x21 => {
                let label = *b.get(at + 1)?;
                if label == 0xF9 {
                    let delay = le16(b, at + 4)? * 10;
                    duration_ms += delay as u64;
                    shortest_delay_ms = shortest_delay_ms.min(delay);
                }
                // browsers play a counted loop that many times more
                if label == 0xFF && b.get(at + 3..at + 14) == Some(b"NETSCAPE2.0".as_slice()) {
                    let count = le16(b, at + 16)?;
                    plays = if count == 0 { 0 } else { count + 1 };
                }
                at = skip_blocks(at + 2)?;
            }
            0x2C => {
                frames += 1;
                let local = *b.get(at + 9)?;
                at += 10;
                if local & 0x80 != 0 {
                    at += 3 * (1 << ((local & 0x07) + 1));
                }
                at = skip_blocks(at + 1)?;
            }
            _ => return None,
        }
    }
    if width == 0 || height == 0 || frames == 0 {
        return None;
    }
    Some(CommunityImageInfo {
        mime: "image/gif",
        width,
        height,
        frames,
        duration_ms: duration_ms.min(u32::MAX as u64) as u32,
        plays,
        shortest_delay_ms,
    })
}

/// WebP: VP8X canvas or VP8 / VP8L size, ANMF frames and durations
fn inspect_webp(b: &[u8]) -> Option<CommunityImageInfo> {
    let mut at = 12;
    let (mut width, mut height) = (0, 0);
    let mut frames = 0u32;
    let mut duration_ms: u64 = 0;
    let mut plays = 1;
    let mut shortest_delay_ms = u32::MAX;
    while at + 8 <= b.len() {
        let kind = b.get(at..at + 4)?;
        let len = le32(b, at + 4)? as usize;
        let data = b.get(at + 8..at + 8 + len)?;
        match kind {
            b"VP8X" => {
                width = le24(data, 4)? + 1;
                height = le24(data, 7)? + 1;
            }
            b"ANMF" => {
                frames += 1;
                let delay = le24(data, 12)?;
                duration_ms += delay as u64;
                shortest_delay_ms = shortest_delay_ms.min(delay);
            }
            b"ANIM" => {
                plays = le16(data, 4)?;
            }
            b"VP8 " if width == 0 => {
                width = le16(data, 6)? & 0x3FFF;
                height = le16(data, 8)? & 0x3FFF;
            }
            b"VP8L" if width == 0 => {
                if *data.first()? != 0x2F {
                    return None;
                }
                let bits = le32(data, 1)?;
                width = (bits & 0x3FFF) + 1;
                height = ((bits >> 14) & 0x3FFF) + 1;
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    if width == 0 || height == 0 {
        return None;
    }
    Some(CommunityImageInfo {
        mime: "image/webp",
        width,
        height,
        frames: frames.max(1),
        duration_ms: duration_ms.min(u32::MAX as u64) as u32,
        plays,
        shortest_delay_ms,
    })
}

/// Check a community image by its bytes, never its name or claimed type.
/// Only PNG, APNG, GIF and WebP pass; SVG, HTML, scripts and everything
/// else is refused.
pub fn inspect_community_image(
    bytes: &[u8],
    limits: &CommunityImageLimits,
) -> Result<CommunityImageInfo, CommunityImageError> {
    if bytes.len() > limits.max_bytes {
        return Err(CommunityImageError::TooLarge);
    }
    let info = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        inspect_png(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        inspect_gif(bytes)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        inspect_webp(bytes)
    } else {
        return Err(CommunityImageError::BadSignature);
    }
    .ok_or(CommunityImageError::Corrupt)?;

    if info.width > limits.max_dimension || info.height > limits.max_dimension {
        return Err(CommunityImageError::TooManyPixels);
    }
    if info.frames > limits.max_frames {
        return Err(CommunityImageError::TooManyFrames);
    }
    if info.duration_ms > limits.max_duration_ms {
        return Err(CommunityImageError::TooLong);
    }
    Ok(info)
}

/// The rules only an effect has, on top of `inspect_community_image`: it
/// moves, plays once, has no strobing delays and has the shape of a profile.
pub fn check_effect_image(
    info: &CommunityImageInfo,
    min_frame_ms: u32,
) -> Result<(), CommunityImageError> {
    if info.frames < 2 {
        return Err(CommunityImageError::NotAnimated);
    }
    if info.plays != 1 {
        return Err(CommunityImageError::Repeats);
    }
    if info.shortest_delay_ms < min_frame_ms {
        return Err(CommunityImageError::FramesTooFast);
    }
    // 9:16, within 2%
    let (wide, tall) = (info.width as u64 * 16, info.height as u64 * 9);
    if wide.abs_diff(tall) * 50 > tall {
        return Err(CommunityImageError::WrongShape);
    }
    Ok(())
}

/// Lowercase, without Arabic marks and with its letter forms joined, so a
/// name can't slip past the word check by spelling
fn plain_words(text: &str) -> Vec<String> {
    let folded: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !('\u{64B}'..='\u{65F}').contains(c) && *c != '\u{640}')
        .map(|c| match c {
            'أ' | 'إ' | 'آ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            'ä' => 'a',
            'ö' => 'o',
            'ü' => 'u',
            'é' | 'è' => 'e',
            other => other,
        })
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Names that say Zrzur stands behind an effect, or that belong to someone
/// else. A filter, not a clearance: reviewers still look (T-06). These are
/// matched inside longer words too.
const BLOCKED_ANYWHERE: &[&str] = &["zrzur", "zarzour", "زرزور", "discord", "ديسكورد"];
/// Matched as whole words, so "pineapple" stays fine
const BLOCKED_WORDS: &[&str] = &[
    "official", "offiziell", "رسمي", "الرسمي", "staff", "طاقم", "verified", "verifiziert",
    "موثق", "الموثق", "moderator", "mod", "مشرف", "admin", "مدير", "nintendo", "pokemon",
    "disney", "marvel", "netflix", "spotify", "youtube", "twitch", "tiktok", "instagram",
    "facebook", "whatsapp", "telegram", "playstation", "xbox", "steam", "apple", "google",
    "microsoft", "fifa", "uefa",
];

/// Whether a text an effect's creator wrote uses a blocked name
pub fn community_text_blocked(text: &str) -> bool {
    let words = plain_words(text);
    words.iter().any(|word| {
        BLOCKED_WORDS.contains(&word.as_str())
            || BLOCKED_ANYWHERE.iter().any(|name| word.contains(name))
    }) || {
        // spaced out: z r z u r
        let joined = words.concat();
        BLOCKED_ANYWHERE.iter().any(|name| joined.contains(name))
    }
}

/// Normalise tags: trimmed, lowercase, unique, at most 8 of up to 24 chars
pub fn community_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag: String = tag.trim().to_lowercase().chars().take(24).collect();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use CommunityItemStatus::*;
    use CommunityReviewAction as A;

    const LIMITS: CommunityImageLimits = CommunityImageLimits {
        max_bytes: 2_500_000,
        max_dimension: 1024,
        max_frames: 300,
        max_duration_ms: 15_000,
    };

    fn effect(frames: u32, plays: u32, delay: u32, width: u32, height: u32) -> CommunityImageInfo {
        CommunityImageInfo {
            mime: "image/gif",
            width,
            height,
            frames,
            duration_ms: frames * delay,
            plays,
            shortest_delay_ms: delay,
        }
    }

    #[test]
    fn effects_have_their_own_rules() {
        use CommunityImageError as E;
        assert_eq!(check_effect_image(&effect(40, 1, 50, 576, 1024), 33), Ok(()));
        assert_eq!(check_effect_image(&effect(1, 1, 50, 576, 1024), 33), Err(E::NotAnimated));
        assert_eq!(check_effect_image(&effect(40, 0, 50, 576, 1024), 33), Err(E::Repeats));
        assert_eq!(check_effect_image(&effect(40, 2, 50, 576, 1024), 33), Err(E::Repeats));
        assert_eq!(check_effect_image(&effect(40, 1, 0, 576, 1024), 33), Err(E::FramesTooFast));
        assert_eq!(check_effect_image(&effect(40, 1, 20, 576, 1024), 33), Err(E::FramesTooFast));
        assert_eq!(check_effect_image(&effect(40, 1, 50, 1024, 1024), 33), Err(E::WrongShape));
    }

    #[test]
    fn an_apng_says_how_often_it_plays() {
        let mut once = png(576, 1024, Some((4, 100)));
        assert_eq!(inspect_community_image(&once, &LIMITS).unwrap().plays, 0);
        // acTL: length, kind, frames, then plays
        let at = once.windows(4).position(|w| w == b"acTL").unwrap() + 8;
        once[at + 3] = 1;
        let info = inspect_community_image(&once, &LIMITS).unwrap();
        assert_eq!((info.plays, info.shortest_delay_ms), (1, 100));
    }

    #[test]
    fn blocked_names_stay_out_of_effects() {
        for name in [
            "Zrzur snow", "زرزور", "زَرْزُور", "OFFICIAL rain", "Official-Rain", "z r z u r",
            "الرسمي", "رسميّ", "Staff sparkle", "Verifiziert", "Pokémon", "Discord glow", "ديسكورد",
        ] {
            assert!(community_text_blocked(name), "{name}");
        }
        for name in ["Snow", "ثلج", "Schnee", "pineapple rain", "Modern", "steamed"] {
            assert!(!community_text_blocked(name), "{name}");
        }
    }

    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]); // crc is not checked here
        out
    }

    pub fn png(width: u32, height: u32, frames: Option<(u32, u16)>) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend(height.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        out.extend(chunk(b"IHDR", &ihdr));
        if let Some((count, delay_ms)) = frames {
            let mut actl = count.to_be_bytes().to_vec();
            actl.extend(0u32.to_be_bytes());
            out.extend(chunk(b"acTL", &actl));
            for _ in 0..count {
                let mut fctl = vec![0u8; 20];
                fctl.extend(delay_ms.to_be_bytes());
                fctl.extend(1000u16.to_be_bytes());
                fctl.extend([0, 0]);
                out.extend(chunk(b"fcTL", &fctl));
            }
        }
        out.extend(chunk(b"IDAT", &[0; 4]));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn transitions_follow_the_publish_flow() {
        // creators publish directly
        assert_eq!(community_transition(&Draft, &A::Publish), Some(Published));
        assert_eq!(community_transition(&ChangesRequested, &A::Publish), Some(Published));
        assert_eq!(community_transition(&Rejected, &A::Publish), Some(Published));
        assert_eq!(community_transition(&PendingReview, &A::Publish), Some(Published));
        // the old "send for review" step no longer moves anything
        assert_eq!(community_transition(&Draft, &A::Submit), None);
        // reviewers still take designs down; only they put them back
        assert_eq!(community_transition(&Published, &A::Suspend), Some(Suspended));
        assert_eq!(community_transition(&Suspended, &A::Approve), Some(Published));
        assert_eq!(community_transition(&Suspended, &A::Publish), None);
        assert_eq!(community_transition(&Published, &A::Publish), None);
        assert_eq!(community_transition(&Archived, &A::Publish), None);
        assert_eq!(community_transition(&Archived, &A::Archive), None);
        // older review decisions on anything still waiting keep working
        assert_eq!(community_transition(&PendingReview, &A::Approve), Some(Published));
        assert_eq!(community_transition(&PendingReview, &A::Reject), Some(Rejected));
        assert_eq!(community_transition(&Draft, &A::Approve), None);
    }

    #[test]
    fn legacy_statuses_still_read() {
        #[derive(serde::Deserialize)]
        struct S {
            status: CommunityItemStatus,
        }
        let read = |s: &str| {
            serde_json::from_str::<S>(&format!("{{\"status\":\"{s}\"}}"))
                .unwrap()
                .status
        };
        assert_eq!(read("Active"), Published);
        assert_eq!(read("Hidden"), Suspended);
        assert_eq!(read("Removed"), Archived);
        assert_eq!(read("Published"), Published);
    }

    #[test]
    fn accepts_png_apng_gif_webp() {
        let still = inspect_community_image(&png(256, 256, None), &LIMITS).unwrap();
        assert_eq!((still.mime, still.frames), ("image/png", 1));

        let apng = inspect_community_image(&png(128, 128, Some((10, 100))), &LIMITS).unwrap();
        assert_eq!((apng.mime, apng.frames, apng.duration_ms), ("image/apng", 10, 1000));

        // 2x1 GIF, two frames of 50ms
        let mut gif = b"GIF89a\x02\x00\x01\x00\x00\x00\x00".to_vec();
        for _ in 0..2 {
            gif.extend([0x21, 0xF9, 4, 0, 5, 0, 0, 0]);
            gif.extend([0x2C, 0, 0, 0, 0, 2, 0, 1, 0, 0, 2, 1, 0, 0]);
        }
        gif.push(0x3B);
        let info = inspect_community_image(&gif, &LIMITS).unwrap();
        assert_eq!((info.width, info.height, info.frames, info.duration_ms), (2, 1, 2, 100));

        // animated WebP canvas 300x200, three frames of 40ms
        let mut body = b"WEBP".to_vec();
        let mut vp8x = vec![0x02, 0, 0, 0];
        vp8x.extend(&299u32.to_le_bytes()[..3]);
        vp8x.extend(&199u32.to_le_bytes()[..3]);
        body.extend(b"VP8X");
        body.extend((vp8x.len() as u32).to_le_bytes());
        body.extend(&vp8x);
        for _ in 0..3 {
            let mut anmf = vec![0u8; 12];
            anmf.extend(&40u32.to_le_bytes()[..3]);
            anmf.push(0);
            body.extend(b"ANMF");
            body.extend((anmf.len() as u32).to_le_bytes());
            body.extend(&anmf);
        }
        let mut webp = b"RIFF".to_vec();
        webp.extend((body.len() as u32).to_le_bytes());
        webp.extend(body);
        let info = inspect_community_image(&webp, &LIMITS).unwrap();
        assert_eq!((info.width, info.height, info.frames, info.duration_ms), (300, 200, 3, 120));
    }

    #[test]
    fn rejects_bad_signatures_svg_and_html() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        assert_eq!(
            inspect_community_image(svg, &LIMITS),
            Err(CommunityImageError::BadSignature)
        );
        assert_eq!(
            inspect_community_image(b"<html><body>hi</body></html>", &LIMITS),
            Err(CommunityImageError::BadSignature)
        );
        // a JPEG renamed to .png
        assert_eq!(
            inspect_community_image(b"\xFF\xD8\xFF\xE0\x00\x10JFIF", &LIMITS),
            Err(CommunityImageError::BadSignature)
        );
        // PNG header but cut short
        let mut cut = png(64, 64, None);
        cut.truncate(30);
        assert_eq!(inspect_community_image(&cut, &LIMITS), Err(CommunityImageError::Corrupt));
    }

    #[test]
    fn rejects_oversized_files_dimensions_and_animations() {
        let mut big = png(64, 64, None);
        big.resize(LIMITS.max_bytes + 1, 0);
        assert_eq!(inspect_community_image(&big, &LIMITS), Err(CommunityImageError::TooLarge));
        assert_eq!(
            inspect_community_image(&png(4096, 64, None), &LIMITS),
            Err(CommunityImageError::TooManyPixels)
        );
        assert_eq!(
            inspect_community_image(&png(64, 64, Some((301, 10))), &LIMITS),
            Err(CommunityImageError::TooManyFrames)
        );
        assert_eq!(
            inspect_community_image(&png(64, 64, Some((20, 1000))), &LIMITS),
            Err(CommunityImageError::TooLong)
        );
    }

    #[test]
    fn tags_are_normalised() {
        let tags = community_tags(&[" Fire ".into(), "fire".into(), "".into(), "ICE".into()]);
        assert_eq!(tags, vec!["fire".to_string(), "ice".to_string()]);
    }
}
