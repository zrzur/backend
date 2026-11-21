//! Custom invite links (zrzur.cc/<name>): who may hold one, which channel
//! it opens, and what happens when a server stops meeting the rule.
//!
//! Owner rule of 2026-11-21: official servers (flag 1) and partners (flag 4)
//! always may; any other server needs `features.vanity.min_members` members
//! and an active community (`active_messages` messages from `active_authors`
//! different members in the last `active_days` days; bots, webhooks and
//! system messages not counted). With billing on, the Wings level that
//! unlocks the link is a second way in.
//!
//! A server that stops meeting the rule is marked (`below_level_since`),
//! its owner gets a notice from the official Zrzur account, the link keeps
//! working `notice_days` (30) days and the name is then released. A server
//! that held the link by its Wings level keeps the older timeline: the link
//! works for 7 days, then pauses; the name stays reserved for 90 days.
//! Meeting the rule again clears the mark.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration as StdDuration, Instant, SystemTime},
};

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_models::v0::{VanityEligibility, VanityMissing, VanityPath};
use revolt_permissions::ChannelPermission;
use revolt_result::{create_error, Result};
use ulid::Ulid;

use crate::{
    server_supercharge_level, Channel, Database, FieldsServer, Invite, MessageActivity, PartialServer, Server,
    AMQP,
};

/// Server flag bits (`Server::flags`)
pub const SERVER_FLAG_OFFICIAL: i32 = 1;
pub const SERVER_FLAG_VERIFIED: i32 = 2;
/// A Zrzur partner: set and removed by platform staff only
/// (`POST /staff/servers/{id}/actions`, `partner` / `unpartner`)
pub const SERVER_FLAG_PARTNER: i32 = 4;

/// Days a Wings link keeps working after the server falls below the level
pub const VANITY_GRACE_DAYS: i64 = 7;
/// Days a Wings name stays reserved for the server after the drop
pub const VANITY_RESERVE_DAYS: i64 = 90;

/// Run by the Zrzur team
pub fn server_is_official(server: &Server) -> bool {
    server.flags.unwrap_or_default() & SERVER_FLAG_OFFICIAL != 0
}

/// A Zrzur partner
pub fn server_is_partner(server: &Server) -> bool {
    server.flags.unwrap_or_default() & SERVER_FLAG_PARTNER != 0
}

/// Where a custom link stands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VanityState {
    /// The server meets the rule
    Active,
    /// Not meeting the rule, link still works until `ends`
    Grace { ends: Timestamp },
    /// Below the Wings level, link paused, name kept until `released_at`
    Paused { released_at: Timestamp },
    /// Time is up: the name goes back to everyone
    Release,
}

/// Days the link works and days the name is kept after a drop, by the way
/// the server last held it: the Wings level keeps its 7 and 90 days; every
/// other way (and links from before the rule) gets the notice period, after
/// which the name is released at once
pub fn vanity_timeline(via: Option<VanityPath>, notice_days: i64) -> (i64, i64) {
    match via {
        Some(VanityPath::Wings) => (VANITY_GRACE_DAYS, VANITY_RESERVE_DAYS),
        _ => (notice_days, notice_days),
    }
}

/// State of a link from when its server stopped meeting the rule (None =
/// meets it), on the timeline of `via`
pub fn vanity_state_with(
    below_level_since: Option<Timestamp>,
    via: Option<VanityPath>,
    notice_days: i64,
    now: Timestamp,
) -> VanityState {
    let Some(since) = below_level_since else {
        return VanityState::Active;
    };
    let (grace, reserve) = vanity_timeline(via, notice_days);
    let ends = since + Duration::days(grace);
    if now < ends {
        return VanityState::Grace { ends };
    }
    let released_at = since + Duration::days(reserve);
    if now < released_at {
        VanityState::Paused { released_at }
    } else {
        VanityState::Release
    }
}

/// [`vanity_state_with`] with the notice period from the config
pub async fn vanity_state(below_level_since: Option<Timestamp>, via: Option<VanityPath>, now: Timestamp) -> VanityState {
    let notice_days = config().await.features.vanity.notice_days as i64;
    vanity_state_with(below_level_since, via, notice_days, now)
}

/// Recent activity counts, per server, with when they were counted
static ACTIVITY_CACHE: LazyLock<Mutex<HashMap<String, (Instant, MessageActivity)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Messages people wrote in the server in the last `active_days` days, and
/// how many people wrote them. Reused for `activity_cache_hours` unless
/// `fresh` (crond's daily check counts again).
pub async fn server_activity(db: &Database, server: &Server, fresh: bool) -> Result<MessageActivity> {
    let settings = config().await.features.vanity;
    let keep = StdDuration::from_secs(settings.activity_cache_hours as u64 * 3600);
    if !fresh {
        let cache = ACTIVITY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, activity)) = cache.get(&server.id) {
            if at.elapsed() < keep {
                return Ok(*activity);
            }
        }
    }
    let since = SystemTime::now() - StdDuration::from_secs(settings.active_days as u64 * 86_400);
    let since_id = Ulid::from_datetime(since).to_string();
    let activity = db.count_message_activity(&server.channels, &since_id).await?;
    let mut cache = ACTIVITY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // a cache that only grows would keep every server ever asked about
    if cache.len() > 50_000 {
        cache.retain(|_, (at, _)| at.elapsed() < keep);
    }
    cache.insert(server.id.clone(), (Instant::now(), activity));
    Ok(activity)
}

/// Whether the server's Wings level unlocks a custom link (billing on only)
async fn has_wings_level(server: &Server) -> bool {
    let billing = config().await.features.billing;
    if !billing.available() {
        return false;
    }
    match billing.vanity_level() {
        Some(required) => server_supercharge_level(server).await >= required,
        None => false,
    }
}

/// Whether the server may hold a custom invite link, and the numbers behind
/// it. Official and partner servers are not counted.
pub async fn vanity_eligibility(db: &Database, server: &Server, fresh: bool) -> Result<VanityEligibility> {
    let settings = config().await.features.vanity;
    let mut out = VanityEligibility {
        eligible: false,
        via: None,
        missing: None,
        members: 0,
        min_members: settings.min_members,
        messages: 0,
        min_messages: settings.active_messages,
        authors: 0,
        min_authors: settings.active_authors,
        days: settings.active_days,
        link_ends_at: None,
    };
    if let Some(vanity) = &server.vanity_invite {
        if let VanityState::Grace { ends } = vanity_state_with(
            vanity.below_level_since,
            vanity.via,
            settings.notice_days as i64,
            Timestamp::now_utc(),
        ) {
            out.link_ends_at = Some(ends);
        }
    }

    let flagged = if server_is_official(server) {
        Some(VanityPath::Official)
    } else if server_is_partner(server) {
        Some(VanityPath::Partner)
    } else {
        None
    };
    if flagged.is_some() {
        out.eligible = true;
        out.via = flagged;
        return Ok(out);
    }

    out.members = db.fetch_member_count(&server.id).await?;
    let activity = server_activity(db, server, fresh).await?;
    out.messages = activity.messages;
    out.authors = activity.authors;
    let enough_members = out.members >= settings.min_members;
    let active = activity.messages >= settings.active_messages && activity.authors >= settings.active_authors;

    if enough_members && active {
        out.eligible = true;
        out.via = Some(VanityPath::Community);
    } else if has_wings_level(server).await {
        out.eligible = true;
        out.via = Some(VanityPath::Wings);
    } else {
        out.missing = Some(match (enough_members, active) {
            (false, false) => VanityMissing::Both,
            (false, true) => VanityMissing::Members,
            _ => VanityMissing::Activity,
        });
    }
    Ok(out)
}

/// The error a claim gets when the server does not meet the rule
pub fn vanity_not_eligible(e: &VanityEligibility) -> revolt_result::Error {
    let missing = match e.missing {
        Some(VanityMissing::Members) => "Members",
        Some(VanityMissing::Activity) => "Activity",
        _ => "Both",
    }
    .to_string();
    create_error!(VanityNotEligible {
        missing,
        members: e.members,
        min_members: e.min_members,
        messages: e.messages,
        min_messages: e.min_messages,
        authors: e.authors,
        min_authors: e.min_authors,
        days: e.days,
    })
}

/// Refuse a custom link that is paused or past its time (fetch and join);
/// other invites pass. Only the stored mark and the cheap checks run here,
/// never the activity count: claims and the sync keep the mark up to date.
pub async fn check_vanity_open(db: &Database, invite: &Invite) -> Result<()> {
    let Invite::Server { vanity: true, server, channel, .. } = invite else {
        return Ok(());
    };
    let server = db.fetch_server(server).await?;
    // no readable channel left to land in: paused until the owner picks one
    if !server.channels.iter().any(|c| c == channel) {
        return Err(create_error!(VanityPaused));
    }
    let Some(vanity) = &server.vanity_invite else {
        return Ok(());
    };
    if vanity.below_level_since.is_none()
        || server_is_official(&server)
        || server_is_partner(&server)
        || has_wings_level(&server).await
    {
        return Ok(());
    }
    match vanity_state(vanity.below_level_since, vanity.via, Timestamp::now_utc()).await {
        VanityState::Active | VanityState::Grace { .. } => Ok(()),
        _ => Err(create_error!(VanityPaused)),
    }
}

/// What one pass of [`sync_vanity_invites`] changed
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VanitySyncReport {
    /// Links that stopped meeting the rule and were marked
    pub marked: usize,
    /// Marked links whose server meets the rule again
    pub recovered: usize,
    /// Names given back
    pub released: usize,
}

/// Keep every custom link in step with the rule: mark the ones whose server
/// stopped meeting it, unmark the ones that meet it again, release names
/// whose time ran out. `fresh` counts activity again instead of reusing a
/// recent count (crond's daily check).
pub async fn sync_vanity_invites(db: &Database, now: Timestamp, fresh: bool) -> Result<VanitySyncReport> {
    let mut report = VanitySyncReport::default();
    for mut server in db.fetch_servers_with_vanity().await? {
        let Some(mut vanity) = server.vanity_invite.clone() else {
            continue;
        };
        let check = match vanity_eligibility(db, &server, fresh).await {
            Ok(check) => check,
            Err(error) => {
                log::warn!("vanity check of {}: {:?}", server.id, error.error_type);
                continue;
            }
        };
        let before = vanity.clone();
        if check.eligible {
            if vanity.below_level_since.is_some() {
                report.recovered += 1;
            }
            vanity.below_level_since = None;
            vanity.notice_sent_at = None;
            vanity.via = check.via;
        } else if vanity.below_level_since.is_none() {
            vanity.below_level_since = Some(now);
            vanity.notice_sent_at = None;
            report.marked += 1;
        }

        if vanity_state(vanity.below_level_since, vanity.via, now).await == VanityState::Release {
            db.delete_invite(&vanity.code).await.ok();
            server
                .update(db, PartialServer::default(), vec![FieldsServer::VanityInvite])
                .await?;
            report.released += 1;
            continue;
        }
        if vanity != before {
            server
                .update(
                    db,
                    PartialServer {
                        vanity_invite: Some(vanity),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;
        }
    }
    Ok(report)
}

/// Official message files of delta, which crond cannot reach; delta's tests
/// check that every file has every key with the same placeholders
const NOTICE_TEXTS: &[(&str, &str)] = &[
    ("en", include_str!("../../../../../delta/assets/official/en.json")),
    ("ar", include_str!("../../../../../delta/assets/official/ar.json")),
    ("de", include_str!("../../../../../delta/assets/official/de.json")),
];

#[derive(serde::Deserialize)]
struct NoticeText {
    #[serde(default)]
    title: String,
    body: String,
}

static NOTICES: LazyLock<HashMap<&'static str, HashMap<String, NoticeText>>> = LazyLock::new(|| {
    NOTICE_TEXTS
        .iter()
        .map(|(code, json)| (*code, serde_json::from_str(json).expect("valid official message file")))
        .collect()
});

fn notice_language(locale: Option<&str>) -> &'static str {
    let wanted = locale.unwrap_or_default().to_lowercase();
    let base = wanted.split(['-', '_']).next().unwrap_or_default();
    NOTICE_TEXTS
        .iter()
        .map(|(code, _)| *code)
        .find(|code| *code == base)
        .unwrap_or("en")
}

/// The notice that a link will end, in `language`, signed like every
/// formal message of the official account
pub fn vanity_notice_text(language: &str, vars: &[(&str, String)]) -> String {
    let texts = NOTICES.get(language).unwrap_or(&NOTICES["en"]);
    let fill = |template: &str| {
        let mut out = template.to_string();
        for (name, value) in vars {
            out = out.replace(&format!("{{{name}}}"), value);
        }
        out
    };
    let notice = &texts["vanity.ending"];
    format!(
        "**{}**\n\n{}\n\n{}",
        fill(&notice.title),
        fill(&notice.body),
        texts["signature"].body
    )
}

/// Tell the owner of every link that stopped meeting the rule, once per
/// drop, from the official Zrzur account: the link keeps working until its
/// end date, then it is released. Wings links (their older timeline) get no
/// notice here. Returns how many owners were told.
pub async fn send_vanity_notices(db: &Database, amqp: Option<&AMQP>, now: Timestamp) -> Result<usize> {
    let settings = config().await.features.vanity;
    let mut sent = 0;
    for mut server in db.fetch_servers_with_vanity().await? {
        let Some(mut vanity) = server.vanity_invite.clone() else {
            continue;
        };
        if vanity.notice_sent_at.is_some() || vanity.via == Some(VanityPath::Wings) {
            continue;
        }
        let VanityState::Grace { ends } =
            vanity_state_with(vanity.below_level_since, vanity.via, settings.notice_days as i64, now)
        else {
            continue;
        };
        let locale = db.fetch_user(&server.owner).await.ok().and_then(|u| u.locale);
        let date: String = ends.format().to_string().chars().take(10).collect();
        let content = vanity_notice_text(
            notice_language(locale.as_deref()),
            &[
                ("name", crate::util::team_texts::plain(&server.name)),
                ("code", vanity.code.clone()),
                ("date", date),
                ("members", settings.min_members.to_string()),
                ("messages", settings.active_messages.to_string()),
                ("authors", settings.active_authors.to_string()),
                ("days", settings.active_days.to_string()),
            ],
        );
        if let Err(error) =
            crate::send_official_message(db, amqp, crate::OfficialAccount::Zrzur, &server.owner, content).await
        {
            log::warn!("vanity notice to the owner of {} not sent: {:?}", server.id, error.error_type);
        }
        // marked as told either way: a failed message is not retried daily
        vanity.notice_sent_at = Some(now);
        server
            .update(
                db,
                PartialServer {
                    vanity_invite: Some(vanity),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
        sent += 1;
    }
    Ok(sent)
}

/// A channel was deleted (its invites with it): move the server's custom
/// link to a channel everyone can read, or keep the name reserved on the old
/// channel id, which answers "paused" until the owner picks a channel
pub async fn repoint_vanity_after_channel_delete(db: &Database, server_id: &str, deleted: &str) -> Result<()> {
    let mut server = db.fetch_server(server_id).await?;
    let Some(mut vanity) = server.vanity_invite.clone() else {
        return Ok(());
    };
    if vanity.channel != deleted {
        return Ok(());
    }
    let target = landing_channel(db, &server, None)
        .await
        .unwrap_or_else(|_| deleted.to_string());
    db.insert_invite(&Invite::Server {
        code: vanity.code.clone(),
        server: server.id.clone(),
        creator: server.owner.clone(),
        channel: target.clone(),
        max_uses: None,
        uses: 0,
        expires: None,
        vanity: true,
    })
    .await
    .ok();
    vanity.channel = target;
    server
        .update(
            db,
            PartialServer {
                vanity_invite: Some(vanity),
                ..Default::default()
            },
            vec![],
        )
        .await
}

/// The chosen channel, or the first one, that new members (@everyone) can read
pub async fn landing_channel(db: &Database, server: &Server, wanted: Option<&str>) -> Result<String> {
    let candidates = match wanted {
        Some(id) if server.channels.iter().any(|c| c == id) => vec![id.to_string()],
        Some(_) => return Err(create_error!(NotFound)),
        None => server.channels.clone(),
    };
    for id in candidates {
        if let Ok(channel) = db.fetch_channel(&id).await {
            if everyone_can_read(server, &channel) {
                return Ok(id);
            }
        }
    }
    Err(create_error!(InvalidOperation))
}

/// A text channel the server's default role may view
pub fn everyone_can_read(server: &Server, channel: &Channel) -> bool {
    let Channel::TextChannel { voice, default_permissions, .. } = channel else {
        return false;
    };
    if voice.is_some() {
        return false;
    }
    let view = ChannelPermission::ViewChannel as i64;
    let mut allowed = server.default_permissions & view != 0;
    if let Some(over) = default_permissions {
        if over.d & view != 0 {
            allowed = false;
        }
        if over.a & view != 0 {
            allowed = true;
        }
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanity_state_timeline() {
        let now = Timestamp::now_utc();
        let ago = |days: i64| Some(now - Duration::days(days));
        let wings = Some(VanityPath::Wings);
        let state = |since| vanity_state_with(since, wings, 30, now);
        assert_eq!(state(None), VanityState::Active);
        assert_eq!(
            state(ago(3)),
            VanityState::Grace { ends: now - Duration::days(3) + Duration::days(VANITY_GRACE_DAYS) }
        );
        assert!(matches!(state(ago(8)), VanityState::Paused { .. }));
        assert!(matches!(state(ago(89)), VanityState::Paused { .. }));
        assert_eq!(state(ago(91)), VanityState::Release);
    }

    #[test]
    fn rule_links_work_30_days_then_are_released() {
        let now = Timestamp::now_utc();
        let ago = |days: i64| Some(now - Duration::days(days));
        for via in [None, Some(VanityPath::Community), Some(VanityPath::Official), Some(VanityPath::Partner)] {
            assert_eq!(
                vanity_state_with(ago(29), via, 30, now),
                VanityState::Grace { ends: now - Duration::days(29) + Duration::days(30) },
                "{via:?}"
            );
            // no paused stretch: released right after the 30 days
            assert_eq!(vanity_state_with(ago(31), via, 30, now), VanityState::Release, "{via:?}");
        }
    }

    #[test]
    fn notice_is_in_three_languages_with_the_date() {
        let vars = [
            ("name", "Arab Gamers".to_string()),
            ("code", "arab-gamers".to_string()),
            ("date", "2026-12-21".to_string()),
            ("members", "1000".to_string()),
            ("messages", "100".to_string()),
            ("authors", "20".to_string()),
            ("days", "30".to_string()),
        ];
        for language in ["en", "ar", "de"] {
            let text = vanity_notice_text(language, &vars);
            for wanted in ["zrzur.cc/arab-gamers", "2026-12-21", "1000", "Arab Gamers"] {
                assert!(text.contains(wanted), "{language}: {wanted}");
            }
            assert!(!text.contains('{'), "{language}: blank left");
        }
        assert!(vanity_notice_text("ar", &vars).contains("السيرفر"));
        assert!(vanity_notice_text("de", &vars).contains("Mitglieder"));
        assert_eq!(notice_language(Some("de-AT")), "de");
        assert_eq!(notice_language(Some("fr")), "en");
    }
}
