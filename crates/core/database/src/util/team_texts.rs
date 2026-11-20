//! The text files of "Join the Zrzur Team" and the few helpers every sender
//! of those texts needs: delta's desk routes (`delta::util::team_texts`,
//! which builds on this) and crond's volunteer job, which cannot reach
//! delta. The files stay in `crates/delta/assets/team/<language>.json`, one
//! per language with the same keys; delta's tests check them.

use std::{collections::HashMap, sync::LazyLock};

/// Languages we have texts for, the first is the fallback
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", include_str!("../../../../delta/assets/team/en.json")),
    ("ar", include_str!("../../../../delta/assets/team/ar.json")),
    ("de", include_str!("../../../../delta/assets/team/de.json")),
    ("fr", include_str!("../../../../delta/assets/team/fr.json")),
    ("es", include_str!("../../../../delta/assets/team/es.json")),
    ("ru", include_str!("../../../../delta/assets/team/ru.json")),
    ("zh-Hans", include_str!("../../../../delta/assets/team/zh-Hans.json")),
];

pub static TEXTS: LazyLock<HashMap<&'static str, HashMap<String, String>>> =
    LazyLock::new(|| {
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
pub fn fill(template: &str, vars: &[(&str, String)]) -> String {
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

/// The whole message `key` (its `.title` and `.body`) in `language`, with
/// the signature, as the official account sends it. The caller makes words
/// from people harmless (`plain`) before passing them in `vars`.
pub fn message(language: &str, key: &str, vars: &[(&str, String)]) -> String {
    format!(
        "**{}**\n\n{}\n\n{}",
        fill(&text(language, &format!("{key}.title")), vars),
        fill(&text(language, &format!("{key}.body")), vars),
        text(language, "signature")
    )
}
