//! Usernames nobody may take.
//!
//! Three lists, one name a line, in `reserved/`:
//! - `zrzur.txt`: Zrzur's own names, its owner's names, role words
//! - `companies.txt`: companies, products and services
//! - `words.txt`: words that read as a page or a function of a site
//!   (`admin`, `support`, `login`, …), from The Big Username Blocklist (MIT)
//!
//! The first two hold for people and bots. The word list holds for people
//! only: a bot is found by name and tag and is often named after what it
//! does (`music`, `help`).
//!
//! A name is compared in lower case with dots, dashes and underscores taken
//! out, so `Dis.cord` and `zrzur_app` are refused too. Accounts that already
//! carry a listed name keep it; the rule is asked only when a name is taken
//! or changed (see `User::create` and `User::update_username`).

use std::collections::HashSet;

use once_cell::sync::Lazy;

fn squash(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '.' | '-' | '_'))
        .collect()
}

fn list(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(squash)
        .filter(|name| !name.is_empty())
        .collect()
}

static FOR_EVERYONE: Lazy<HashSet<String>> = Lazy::new(|| {
    let mut names = list(include_str!("reserved/zrzur.txt"));
    names.extend(list(include_str!("reserved/companies.txt")));
    names
});

static FOR_PEOPLE: Lazy<HashSet<String>> =
    Lazy::new(|| list(include_str!("reserved/words.txt")));

/// Whether a username is on a reserved list (for a bot: the Zrzur and
/// company lists only)
pub fn is_reserved_username(username: &str, is_bot: bool) -> bool {
    let name = squash(username);
    FOR_EVERYONE.contains(&name) || (!is_bot && FOR_PEOPLE.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::is_reserved_username;

    #[test]
    fn own_and_company_names_are_reserved_for_everyone() {
        for name in ["Maher", "ZRZUR", "zrzur_app", "Dis.cord", "google", "Steam", "زرزور", "Play-Station"] {
            assert!(is_reserved_username(name, false), "{name} for a person");
            assert!(is_reserved_username(name, true), "{name} for a bot");
        }
    }

    #[test]
    fn site_words_are_reserved_for_people_only() {
        for name in ["admin", "Support", "login", "help"] {
            assert!(is_reserved_username(name, false), "{name} for a person");
            assert!(!is_reserved_username(name, true), "{name} for a bot");
        }
    }

    #[test]
    fn ordinary_names_are_free() {
        for name in ["Layla", "omar_k", "maher2001", "discordfan", "ليلى", "五条悟"] {
            assert!(!is_reserved_username(name, false), "{name}");
        }
    }

    #[test]
    fn the_lists_are_not_empty() {
        assert!(super::FOR_EVERYONE.len() > 150);
        assert!(super::FOR_PEOPLE.len() > 400);
    }
}
