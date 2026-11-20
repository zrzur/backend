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

use revolt_config::{TeamQuestion, TeamRole};
use revolt_database::{send_official_message, Database, OfficialAccount};

// The text files and the helpers live in the database crate, so crond's
// volunteer job sends the same texts; the files stay in `assets/team/`.
pub use revolt_database::util::team_texts::{is_format, language_of, plain, text, LANGUAGES};
#[allow(unused_imports)]
pub(crate) use revolt_database::util::team_texts::{fill, message, TEXTS};

/// An age typed by a person: ASCII digits or Arabic-Indic digits (`١٧`),
/// trimmed, and nothing else (no words, signs, spaces inside or Persian
/// digits). `None` for anything that is not a plain number up to 255; the
/// range that counts is the caller's to check.
pub fn parse_age(text: &str) -> Option<u8> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 3 {
        return None;
    }
    let mut value: u32 = 0;
    for c in text.chars() {
        let digit = match c {
            '0'..='9' => c as u32 - '0' as u32,
            '\u{0660}'..='\u{0669}' => c as u32 - 0x0660,
            _ => return None,
        };
        value = value * 10 + digit;
    }
    u8::try_from(value).ok()
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
/// no longer have it. Best effort: the decision is already saved. Returns
/// whether the message went out.
pub async fn send(
    db: &Database,
    user_id: &str,
    key: &str,
    role: Option<&TeamRole>,
    role_key: &str,
    vars: &[(&str, &str)],
) -> bool {
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
    // `counting` asks for the counting notice (`volunteer.counting`, with
    // `{days}` and `{privacy}` of the same call); left out or empty, the
    // blank stays empty
    if let Some(at) = all.iter().position(|(name, _)| *name == "counting") {
        all[at].1 = if all[at].1.is_empty() {
            String::new()
        } else {
            fill(&text(language, "volunteer.counting"), &all)
        };
    } else {
        all.push(("counting", String::new()));
    }
    let message = message(language, key, &all);
    match send_official_message(db, None, OfficialAccount::Zrzur, user_id, message).await {
        Ok(_) => true,
        Err(error) => {
            log::warn!("team message to {user_id} not sent: {:?}", error.error_type);
            false
        }
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
    fn an_age_is_digits_and_nothing_else() {
        assert_eq!(parse_age("17"), Some(17));
        assert_eq!(parse_age(" 25 "), Some(25));
        assert_eq!(parse_age("\u{0661}\u{0667}"), Some(17));
        assert_eq!(parse_age("17 years"), None);
        assert_eq!(parse_age("1 7"), None);
        assert_eq!(parse_age("-5"), None);
        assert_eq!(parse_age("+17"), None);
        assert_eq!(parse_age("1.5"), None);
        assert_eq!(parse_age(""), None);
        assert_eq!(parse_age("256"), None);
        assert_eq!(parse_age("\u{06F1}\u{06F7}"), None);
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

    /// The texts a volunteer reads (Task 10): in each person's own words,
    /// never left in English; only the staff notice stays English
    #[test]
    fn every_language_has_every_volunteer_text_in_its_own_words() {
        let english = &TEXTS["en"];
        let volunteer_keys: Vec<&String> = english
            .keys()
            .filter(|key| {
                key.starts_with("volunteer.")
                    || key.starts_with("about_")
                    || key.starts_with("identity_missing.")
            })
            .collect();
        assert!(volunteer_keys.len() >= 24, "{} volunteer texts", volunteer_keys.len());
        for (code, _) in LANGUAGES.iter().skip(1) {
            let texts = &TEXTS[code];
            for key in &volunteer_keys {
                assert_ne!(texts[*key], english[*key], "{code} {key} is still English");
            }
            // the notice to the staff is English in every file (official servers are English)
            for key in ["staff.trial_ended.title", "staff.trial_ended.body"] {
                assert_eq!(texts[key], english[key], "{code} {key}");
            }
            // a button label of the leave question
            assert!(texts["volunteer.leave_confirm.label"].chars().count() <= 80, "{code} leave label");
        }
        let thanks = [
            ("en", "Thank you"),
            ("ar", "شكر"),
            ("de", "Danke"),
            ("fr", "Merci"),
            ("es", "Gracias"),
            ("ru", "Спасибо"),
            ("zh-Hans", "感谢"),
        ];
        for (code, word) in thanks {
            for key in ["volunteer.left.body", "volunteer.removed.body", "volunteer.reminder.body"] {
                assert!(text(code, key).contains(word), "{code} {key} says thank you");
            }
        }
        // a count reads right for any number: "Days away: 1", never "1 days"
        for (code, _) in LANGUAGES {
            for key in &volunteer_keys {
                let value = &TEXTS[code][*key];
                assert!(!value.contains("(s)"), "{code} {key}");
                assert!(!value.contains("{days} days"), "{code} {key}");
            }
        }
    }

    /// No text calls a volunteer role a job, a shift, a duty or a penalty, in
    /// any language. `terms` alone names a job, to say that it is not one.
    #[test]
    fn never_a_job_a_shift_a_duty_or_a_penalty_in_any_language() {
        // lower-case words, looked for in the lower-cased text
        let words: [(&str, &[&str]); 6] = [
            ("en", &["job", "shift", "duty", "duties", "penalt"]),
            ("ar", &["وظيف", "دوام", "عقوب", "مناوب"]),
            ("fr", &["emploi", "travail", "pénalit", "sanction"]),
            ("es", &["trabajo", "empleo", "turno", "penaliz", "sanción", "sancion"]),
            ("ru", &["работа", "смена", "смены", "смену", "дежурств", "штраф", "наказан"]),
            ("zh-Hans", &["工作", "上班", "值班", "惩罚", "处罚"]),
        ];
        for (code, list) in words {
            for (key, value) in &TEXTS[code] {
                if key == "terms" {
                    continue;
                }
                let lower = value.to_lowercase();
                for word in list {
                    assert!(!lower.contains(word), "{code} {key} says {word}");
                }
            }
        }
        // German nouns are written with a capital, so the case matters:
        // "Verpflichtung" (a commitment, said to be none) is fine
        for (key, value) in &TEXTS["de"] {
            if key == "terms" {
                continue;
            }
            for word in ["Job", "Schicht", "Pflicht", "Strafe"] {
                assert!(!value.contains(word), "de {key} says {word}");
            }
        }
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
    fn invisible_format_characters_from_people_are_dropped() {
        // a right-to-left override, isolates, marks and zero-width characters
        let sneaky = "Sara\u{202E}gnp.exe\u{2066}\u{2069}\u{200E}\u{200F}\u{200B}\u{FEFF}\u{2060} Haddad";
        let shown = plain(sneaky);
        assert_eq!(shown, "Saragnp.exe Haddad");
        for c in shown.chars() {
            assert!(!is_format(c), "{:?} left in", c);
        }
        // Arabic text itself stays as it is
        assert_eq!(plain("لينا حداد"), "لينا حداد");
        // the zero-width space plain() puts after `@` is its own, not the person's
        assert_eq!(plain("@everyone"), "@\u{200b}everyone");
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
