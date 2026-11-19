//! Texts of "Join the Zrzur Team" in the app's seven complete languages:
//! the messages a person receives about their application, and the name,
//! description and rules of each kind of team role with the two standard
//! questions of the form. A person reads them in the language they use the
//! app in (`User::locale`), English when we do not have theirs.
//!
//! The texts live in `assets/team/<language>.json`, one file per language
//! with the same keys; the tests check that every file has every key with
//! the same placeholders, and that form texts fit the form's limits.
//!
//! A role of the settings takes its texts from its kind: the key itself
//! (`support`) or the part before a suffix (`moderator-lounge` is a
//! `moderator`). Words the owner writes on a role in the settings (`name`,
//! `description`, `rules`) replace the English text; a role of no known
//! kind shows those words in every language.

use std::{collections::HashMap, sync::LazyLock};

use revolt_config::{TeamQuestion, TeamRole};
use revolt_database::{send_official_message, Database, OfficialAccount};

/// Languages we have texts for, the first is the fallback
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", include_str!("../../assets/team/en.json")),
    ("ar", include_str!("../../assets/team/ar.json")),
    ("de", include_str!("../../assets/team/de.json")),
    ("fr", include_str!("../../assets/team/fr.json")),
    ("es", include_str!("../../assets/team/es.json")),
    ("ru", include_str!("../../assets/team/ru.json")),
    ("zh-Hans", include_str!("../../assets/team/zh-Hans.json")),
];

static TEXTS: LazyLock<HashMap<&'static str, HashMap<String, String>>> = LazyLock::new(|| {
    LANGUAGES
        .iter()
        .map(|(code, json)| (*code, serde_json::from_str(json).expect("valid team text file")))
        .collect()
});

/// Our language code for a locale such as `de-DE` or `zh-CN`, or English
pub fn language_of(locale: Option<&str>) -> &'static str {
    let wanted = locale.unwrap_or_default().to_lowercase().replace('_', "-");
    let base = wanted.split('-').next().unwrap_or_default();
    if base == "zh" {
        return "zh-Hans";
    }
    LANGUAGES
        .iter()
        .map(|(code, _)| *code)
        .find(|code| code.to_lowercase() == wanted || *code == base)
        .unwrap_or(LANGUAGES[0].0)
}

/// The text `key` in `language`, in English when that language lacks it
pub fn text(language: &str, key: &str) -> String {
    TEXTS
        .get(language)
        .and_then(|texts| texts.get(key))
        .or_else(|| TEXTS[LANGUAGES[0].0].get(key))
        .cloned()
        .unwrap_or_default()
}

/// Words that come from a person (a name, a rejection reason) made harmless
/// for a message of the official account: no link that can be pressed, no
/// mention, no formatting, nothing that reads as a blank of the template
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.trim().chars() {
        match c {
            // without its square brackets a link is plain text; round
            // brackets stay as they are (they are common in names)
            '\\' | '*' | '_' | '~' | '`' | '>' | '|' | '[' | ']' | '<' => {
                out.push('\\');
                out.push(c);
            }
            // a zero-width space breaks mentions and blanks without changing how it reads
            '@' | '{' => {
                out.push(c);
                out.push('\u{200b}');
            }
            c if c.is_control() && c != '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    let out = out.replace("://", ":\u{200b}//");
    // a line that starts with `#` would be a heading
    out.split('\n')
        .map(|line| {
            if line.trim_start().starts_with('#') {
                format!("\\{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fill the `{blanks}` of a template in one pass: a value is never read
/// again, so words a person wrote cannot pull in another blank
fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match vars.iter().find(|(known, _)| *known == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // a value left empty must not leave a gap
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

/// The kind a role key belongs to: the key itself or its part before a
/// `-suffix`, the longest that fits
pub fn kind_of(key: &str) -> Option<&'static str> {
    TeamRole::KINDS
        .iter()
        .copied()
        .filter(|kind| key == *kind || key.starts_with(&format!("{kind}-")))
        .max_by_key(|kind| kind.len())
}

#[derive(Clone, Copy)]
pub enum RoleField {
    Name,
    Description,
    Rules,
}

/// One text of a role in one language
pub fn role_text(role: &TeamRole, language: &str, field: RoleField) -> String {
    let (own, word) = match field {
        RoleField::Name => (&role.name, "name"),
        RoleField::Description => (&role.description, "description"),
        RoleField::Rules => (&role.rules, "rules"),
    };
    // the owner's own words are the English text
    if language == LANGUAGES[0].0 && !own.trim().is_empty() {
        return own.clone();
    }
    let shipped = kind_of(&role.key)
        .map(|kind| text(language, &format!("role.{kind}.{word}")))
        .unwrap_or_default();
    if shipped.is_empty() {
        own.clone()
    } else {
        shipped
    }
}

/// The free-text questions of a role's form in one language: the role's
/// own as written in the settings, or the two standard ones
pub fn questions(role: &TeamRole, language: &str) -> Vec<TeamQuestion> {
    if !role.questions.is_empty() {
        return role.questions.clone();
    }
    ["experience", "why"]
        .iter()
        .map(|id| TeamQuestion {
            id: id.to_string(),
            label: text(language, &format!("question.{id}.label")),
            hint: text(language, &format!("question.{id}.hint")),
        })
        .collect()
}

/// Send the message `key` about an application to one person, in their
/// language, from the official Zrzur account. `{role}`, `{rules}` and
/// `{terms}` are filled here; `role_key` names the role when the settings
/// no longer have it. Best effort: the decision is already saved.
pub async fn send(
    db: &Database,
    user_id: &str,
    key: &str,
    role: Option<&TeamRole>,
    role_key: &str,
    vars: &[(&str, &str)],
) {
    let locale = db.fetch_user(user_id).await.ok().and_then(|user| user.locale);
    let language = language_of(locale.as_deref());
    let name = role
        .map(|role| role_text(role, language, RoleField::Name))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| role_key.to_string());
    let rules = role
        .map(|role| role_text(role, language, RoleField::Rules))
        .unwrap_or_default();
    // what the caller passes comes from people (names, a reason) or from
    // the settings (a channel mention, a link): the first is made harmless
    let mut all: Vec<(&str, String)> = vars
        .iter()
        .map(|(name, value)| {
            let value = match *name {
                "channel" | "privacy" | "days" => value.trim().to_string(),
                _ => plain(value),
            };
            (*name, value)
        })
        .collect();
    all.push(("role", name));
    all.push(("rules", rules));
    all.push(("terms", text(language, "terms")));
    let message = format!(
        "**{}**\n\n{}\n\n{}",
        fill(&text(language, &format!("{key}.title")), &all),
        fill(&text(language, &format!("{key}.body")), &all),
        text(language, "signature")
    );
    if let Err(error) =
        send_official_message(db, None, OfficialAccount::Zrzur, user_id, message).await
    {
        log::warn!("team message to {user_id} not sent: {:?}", error.error_type);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name.to_string()))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn every_language_has_every_text() {
        let base = &TEXTS[LANGUAGES[0].0];
        assert_eq!(LANGUAGES.len(), 7);
        for (code, _) in LANGUAGES {
            let texts = &TEXTS[code];
            let mut keys: Vec<_> = texts.keys().collect();
            let mut expected: Vec<_> = base.keys().collect();
            keys.sort();
            expected.sort();
            assert_eq!(keys, expected, "{code} has different keys");
            for (key, text) in base {
                assert!(!texts[key].trim().is_empty(), "{code} {key} is empty");
                assert_eq!(placeholders(&texts[key]), placeholders(text), "{code} {key}");
            }
        }
    }

    #[test]
    fn every_kind_has_its_texts_and_form_texts_fit_the_form() {
        for (code, _) in LANGUAGES {
            for kind in TeamRole::KINDS {
                for word in ["name", "description", "rules"] {
                    assert!(!text(code, &format!("role.{kind}.{word}")).is_empty(), "{code} {kind} {word}");
                }
                // a role's name is an entry of a menu, its description the line under it
                assert!(text(code, &format!("role.{kind}.name")).chars().count() <= 60, "{code} {kind} name");
                assert!(text(code, &format!("role.{kind}.description")).chars().count() <= 100, "{code} {kind} description");
            }
            for id in ["experience", "why"] {
                assert!(text(code, &format!("question.{id}.label")).chars().count() <= 45, "{code} {id} label");
                assert!(text(code, &format!("question.{id}.hint")).chars().count() <= 100, "{code} {id} hint");
            }
        }
    }

    #[test]
    fn it_is_a_volunteer_project_in_every_language_and_never_a_job() {
        // the word for "volunteer" as each language writes it in the terms
        let volunteer = [
            ("en", "volunteer project"),
            ("ar", "مشروع تطوعي"),
            ("de", "ehrenamtliches Projekt"),
            ("fr", "projet bénévole"),
            ("es", "proyecto voluntario"),
            ("ru", "волонтёрский проект"),
            ("zh-Hans", "志愿项目"),
        ];
        for (code, word) in volunteer {
            assert!(text(code, "terms").contains(word), "{code} terms");
        }
        // no text asks for or keeps an age
        for (code, _) in LANGUAGES {
            for (key, value) in &TEXTS[code] {
                assert!(!value.contains("18"), "{code} {key} names an age");
            }
        }
        let english = &TEXTS["en"];
        for (key, value) in english {
            // "unpaid" is the one place the word may stand
            let lower = value.to_lowercase().replace("unpaid", "");
            for word in ["salary", "hire", "shift", "paid", "position", "wage"] {
                assert!(!lower.contains(word), "{key} says {word}");
            }
        }
        assert!(english["terms"].contains("It is not a job and not employment"));
    }

    #[test]
    fn words_from_people_are_made_harmless_and_cannot_pull_in_a_blank() {
        let reason = "[Re-verify your account](https://evil.example) @everyone <@01X> {terms} {rules}";
        let safe = plain(reason);
        assert!(!safe.contains("](https://"));
        assert!(!safe.contains("://evil"));
        assert!(!safe.contains("@everyone"));
        // the mention is broken apart: nothing reads as `<@id>` any more
        assert!(!safe.contains("<@01X"));
        let filled = fill(
            "A {reason} B {terms}",
            &[("reason", safe), ("terms", "TERMS".to_string())],
        );
        // the template's own blank is filled; the one inside the reason is not
        assert_eq!(filled.matches("TERMS").count(), 1);
        assert!(filled.ends_with("B TERMS"));
        assert_eq!(fill("{a}{b} {c}", &[("a", "{b}".to_string()), ("b", "x".to_string())]), "{b}x {c}");
    }

    #[test]
    fn locales_and_kinds() {
        assert_eq!(language_of(Some("de-DE")), "de");
        assert_eq!(language_of(Some("zh-CN")), "zh-Hans");
        assert_eq!(language_of(Some("zh_Hans")), "zh-Hans");
        assert_eq!(language_of(Some("ar")), "ar");
        assert_eq!(language_of(Some("ja")), "en");
        assert_eq!(language_of(None), "en");
        assert_eq!(kind_of("moderator-lounge"), Some("moderator"));
        assert_eq!(kind_of("discover-reviewer"), Some("discover-reviewer"));
        assert_eq!(kind_of("support"), Some("support"));
        assert_eq!(kind_of("supporter"), None);
        assert_eq!(kind_of("helper"), None);

        let mut role = TeamRole {
            key: "moderator-designs".to_string(),
            ..Default::default()
        };
        assert_eq!(role_text(&role, "en", RoleField::Name), "Moderator");
        assert_eq!(role_text(&role, "fr", RoleField::Name), "Modérateur");
        // the owner's own words are the English text; other languages keep theirs
        role.name = "Guardian".to_string();
        assert_eq!(role_text(&role, "en", RoleField::Name), "Guardian");
        assert_eq!(role_text(&role, "fr", RoleField::Name), "Modérateur");
        // a role of no known kind shows the owner's words everywhere
        role.key = "helper".to_string();
        assert_eq!(role_text(&role, "ru", RoleField::Name), "Guardian");
        assert_eq!(questions(&role, "es")[1].label, "¿Por qué quieres este rol?");
    }
}
