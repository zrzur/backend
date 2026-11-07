//! Which names a webhook may post under. A webhook shows any name its token
//! holder picks, so the rules are stricter than for people: nothing that
//! passes for Zrzur, its staff or a verified bot, no invisible characters.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use revolt_result::{create_error, Result};

use crate::{passes_for_official, Database};

/// Longest webhook name, in characters
pub const WEBHOOK_NAME_MAX: usize = 80;

/// Roles people trust at sight, after squashing (see [`squash`])
const RESERVED_NAMES: &[&str] = &[
    "system", "systemmessage", "admin", "administrator", "moderator", "staff", "support", "official",
    "trustandsafety", "نظام", "النظام", "مدير", "المدير", "مشرف", "المشرف", "ادارة", "الادارة",
    "دعم", "الدعم", "رسمي",
];

/// These may not appear anywhere in the name
const RESERVED_WORDS: &[&str] = &["zrzur", "زرزور"];

/// Refused anywhere: controls, and characters that reorder or hide text
/// (RLO, LRO, isolates, ZWJ, ZWNJ, ZWSP, word joiner, BOM, soft hyphen,
/// Arabic letter mark, Mongolian vowel separator, tag characters)
fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202F}'
                | '\u{205F}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

fn squash(text: &str) -> String {
    let folded = crate::util::automod::normalise(text);
    folded.chars().filter(|c| c.is_alphanumeric() && *c != 'ـ').collect()
}

/// Lookalike Latin letters become plain ones; the cure changes Arabic, so the
/// original counts as well
fn spellings(name: &str) -> [String; 2] {
    let cured = decancer::cure(name, decancer::Options::default())
        .map(|cured| cured.to_string())
        .unwrap_or_default();
    [squash(&cured), squash(name)]
}

/// What the name breaks, if anything. No database: length, characters and
/// the fixed lists.
pub fn webhook_name_problem(name: &str) -> Option<&'static str> {
    let length = name.chars().count();
    if name.trim().is_empty() || length > WEBHOOK_NAME_MAX {
        return Some("a webhook name has 1 to 80 characters");
    }
    if name.chars().any(is_hidden) {
        return Some("a webhook name cannot have hidden characters");
    }
    if passes_for_official(name)
        || spellings(name).iter().any(|spelling| {
            RESERVED_NAMES.iter().any(|name| squash(name) == *spelling)
                || RESERVED_WORDS.iter().any(|word| spelling.contains(word))
        })
    {
        return Some("this name is reserved");
    }
    None
}

/// Names of platform staff and verified bots, kept for a few minutes: a name
/// override is checked on every message.
static PROTECTED: Mutex<Option<(Instant, HashSet<String>)>> = Mutex::new(None);
const PROTECTED_FOR: Duration = Duration::from_secs(300);

async fn protected_names(db: &Database) -> Result<HashSet<String>> {
    if let Ok(cache) = PROTECTED.lock() {
        if let Some((at, names)) = cache.as_ref() {
            if at.elapsed() < PROTECTED_FOR {
                return Ok(names.clone());
            }
        }
    }

    let mut ids: Vec<String> = db
        .fetch_staff_grants()
        .await?
        .into_iter()
        .filter(|grant| !grant.disabled)
        .map(|grant| grant.id)
        .collect();
    ids.extend(db.fetch_verified_bot_ids().await?);

    let mut names = HashSet::new();
    for user in db.fetch_users(&ids).await? {
        names.insert(squash(&user.username));
        if let Some(display_name) = &user.display_name {
            names.insert(squash(display_name));
        }
    }
    names.remove("");

    if let Ok(mut cache) = PROTECTED.lock() {
        *cache = Some((Instant::now(), names.clone()));
    }
    Ok(names)
}

/// Refuse a name this webhook may not use
pub async fn assert_webhook_name(db: &Database, name: &str) -> Result<()> {
    if webhook_name_problem(name).is_some() {
        return Err(create_error!(InvalidUsername));
    }
    let protected = protected_names(db).await?;
    if spellings(name).iter().any(|spelling| protected.contains(spelling)) {
        return Err(create_error!(InvalidUsername));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_limits() {
        assert_eq!(webhook_name_problem(""), Some("a webhook name has 1 to 80 characters"));
        assert_eq!(webhook_name_problem("   "), Some("a webhook name has 1 to 80 characters"));
        assert_eq!(webhook_name_problem("a"), None);
        assert_eq!(webhook_name_problem(&"x".repeat(80)), None);
        assert!(webhook_name_problem(&"x".repeat(81)).is_some());
        // characters, not bytes: Arabic letters take two bytes
        assert_eq!(webhook_name_problem(&"ب".repeat(80)), None);
        assert!(webhook_name_problem(&"ب".repeat(81)).is_some());
    }

    #[test]
    fn hidden_characters() {
        for c in [
            '\u{202E}', '\u{202D}', '\u{2066}', '\u{2069}', '\u{200B}', '\u{200C}', '\u{200D}',
            '\u{200E}', '\u{200F}', '\u{061C}', '\u{FEFF}', '\u{00AD}', '\u{2060}', '\u{180E}',
            '\u{0007}', '\n', '\u{E0041}',
        ] {
            assert!(webhook_name_problem(&format!("build{c}bot")).is_some(), "{c:?}");
        }
    }

    #[test]
    fn reserved_names_in_every_disguise() {
        for name in [
            "zrzur", "Zrzur", "ZRZUR", "Z R Z U R", "z.r.z.u.r", "ⓩⓡⓩⓤⓡ", "Ｚｒｚｕｒ", "zrzur bot",
            "my zrzur news", "زرزور", "زَرْزُور", "زرزور الرسمي", "ز ر ز و ر", "زـرـزـور",
            "system", "SYSTEM", "Sуstem", "admin", "Admin", "ad-min", "administrator", "Moderator",
            "staff", "Support", "نظام", "النظام", "مشرف", "الإدارة", "إدارة", "الدعم",
        ] {
            assert_eq!((name, webhook_name_problem(name).is_some()), (name, true));
        }
    }

    #[test]
    fn ordinary_names_pass() {
        for name in [
            "GitHub", "Deploys", "Build bot", "نشر تلقائي", "إشعارات المتجر", "Grafana-Alerts",
            "systems status", "admin panel alerts",
        ] {
            assert_eq!(webhook_name_problem(name), None, "{name}");
        }
    }
}
