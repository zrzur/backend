//! Names nobody may masquerade as: those of the other people in the server.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use revolt_result::Result;

use crate::Database;

/// How long the names of a server's members are kept before reloading
const NAMES_TTL: Duration = Duration::from_secs(30);

/// server id → (loaded at, normalised name → ids of the members using it)
type NameCache = HashMap<String, (Instant, HashMap<String, Vec<String>>)>;

static NAMES: LazyLock<Mutex<NameCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Lower case, without spaces or invisible characters, so "Night Owl",
/// "nightowl" and "night\u{200b}owl" are the same name
pub fn normalise_name(name: &str) -> String {
    name.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
                )
        })
        .flat_map(char::to_lowercase)
        .collect()
}

async fn load_names(db: &Database, server_id: &str) -> Result<HashMap<String, Vec<String>>> {
    let members = db.fetch_all_members(server_id).await?;
    let ids: Vec<String> = members.iter().map(|m| m.id.user.clone()).collect();
    let users = db.fetch_users(&ids).await?;

    let mut names: HashMap<String, Vec<String>> = HashMap::new();
    let mut add = |name: &str, id: &str| {
        let key = normalise_name(name);
        if !key.is_empty() {
            names.entry(key).or_default().push(id.to_string());
        }
    };
    for member in &members {
        if let Some(nickname) = &member.nickname {
            add(nickname, &member.id.user);
        }
    }
    for user in &users {
        add(&user.username, &user.id);
        if let Some(display_name) = &user.display_name {
            add(display_name, &user.id);
        }
    }
    Ok(names)
}

/// Whether `name` is (ignoring case, spaces and invisible characters) the
/// username, display name or nickname of a member of the server other
/// than the author. Names are cached per server for a short time.
pub async fn masquerade_name_taken(
    db: &Database,
    server_id: &str,
    author_id: &str,
    name: &str,
) -> Result<bool> {
    let key = normalise_name(name);
    if key.is_empty() {
        return Ok(false);
    }

    let cached = {
        let cache = NAMES.lock().unwrap();
        cache
            .get(server_id)
            .filter(|(at, _)| at.elapsed() < NAMES_TTL)
            .map(|(_, names)| names.get(&key).cloned().unwrap_or_default())
    };
    let holders = match cached {
        Some(holders) => holders,
        None => {
            let names = load_names(db, server_id).await?;
            let holders = names.get(&key).cloned().unwrap_or_default();
            let mut cache = NAMES.lock().unwrap();
            cache.retain(|_, (at, _)| at.elapsed() < NAMES_TTL);
            cache.insert(server_id.to_string(), (Instant::now(), names));
            holders
        }
    };

    Ok(holders.iter().any(|id| id != author_id))
}

#[cfg(test)]
mod tests {
    use super::normalise_name;

    #[test]
    fn names_match_without_case_spaces_or_invisible_characters() {
        assert_eq!(normalise_name("Night Owl"), "nightowl");
        assert_eq!(normalise_name("  NIGHT\tOWL "), "nightowl");
        assert_eq!(normalise_name("night\u{200b}owl\u{feff}"), "nightowl");
        assert_eq!(normalise_name("ليلى حداد"), "ليلىحداد");
        assert_eq!(normalise_name(" \u{200b} "), "");
    }
}
