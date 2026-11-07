//! Checks for the profile fields people fill in themselves: links, interests
//! and section order. All of it is decided here, on the server.

use revolt_models::v0::{
    self, PROFILE_INTERESTS, PROFILE_LINK_MAX_LENGTH, PROFILE_MAX_INTERESTS, PROFILE_MAX_LINKS,
    PROFILE_SECTIONS,
};
use revolt_result::{create_error, Result};
use url::{Host, Url};

fn refuse<T>(error: &str) -> Result<T> {
    Err(create_error!(FailedValidation {
        error: error.to_string()
    }))
}

/// Characters that hide or reorder what the address really says
fn is_deceptive_char(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
        )
}

/// A profile link: `https`, a real site name, no credentials and nothing
/// that disguises the address. Returns the normalised address, which is
/// what people see, so a label can never point somewhere else.
pub fn clean_link(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().count() > PROFILE_LINK_MAX_LENGTH {
        return refuse("link length");
    }
    if raw.chars().any(is_deceptive_char) {
        return refuse("link has hidden characters");
    }
    let Ok(url) = Url::parse(raw) else {
        return refuse("link is not an address");
    };
    if url.scheme() != "https" {
        return refuse("link must use https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return refuse("link must not hold a name or password");
    }
    match url.host() {
        Some(Host::Domain(host)) if host.contains('.') && !host.ends_with('.') => {}
        _ => return refuse("link needs a site name"),
    }
    let clean = url.to_string();
    if clean.chars().count() > PROFILE_LINK_MAX_LENGTH {
        return refuse("link length");
    }
    Ok(clean)
}

/// The whole list of links
pub fn clean_links(raw: &[String]) -> Result<Vec<String>> {
    if raw.len() > PROFILE_MAX_LINKS {
        return refuse("too many links");
    }
    let mut links: Vec<String> = Vec::with_capacity(raw.len());
    for link in raw {
        let link = clean_link(link)?;
        if links.contains(&link) {
            return refuse("the same link twice");
        }
        links.push(link);
    }
    Ok(links)
}

/// Interests come from the fixed list only: free text could say something
/// about health, belief or politics
pub fn clean_interests(raw: &[String]) -> Result<Vec<String>> {
    if raw.len() > PROFILE_MAX_INTERESTS {
        return refuse("too many interests");
    }
    let mut interests: Vec<String> = Vec::with_capacity(raw.len());
    for interest in raw {
        if !PROFILE_INTERESTS.contains(&interest.as_str()) {
            return refuse("unknown interest");
        }
        if !interests.contains(interest) {
            interests.push(interest.clone());
        }
    }
    Ok(interests)
}

/// Known section ids, none twice, none both shown and hidden
pub fn check_sections(shown: Option<&[String]>, hidden: Option<&[String]>) -> Result<()> {
    let mut seen: Vec<&str> = vec![];
    for id in shown.into_iter().chain(hidden).flatten() {
        if !PROFILE_SECTIONS.contains(&id.as_str()) {
            return refuse("unknown profile section");
        }
        if seen.contains(&id.as_str()) {
            return refuse("a profile section is listed twice");
        }
        seen.push(id);
    }
    Ok(())
}

/// Bring a profile edit's checked fields into the stored profile
pub fn apply(profile: &mut revolt_database::UserProfile, data: &v0::DataUserProfile) -> Result<()> {
    if let Some(links) = &data.links {
        profile.links = clean_links(links)?;
    }
    if let Some(interests) = &data.interests {
        profile.interests = clean_interests(interests)?;
    }
    check_sections(data.sections.as_deref(), data.hidden_sections.as_deref())?;
    if let Some(sections) = &data.sections {
        profile.sections = Some(sections.clone());
    }
    if let Some(hidden) = &data.hidden_sections {
        profile.hidden_sections = hidden.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_site_address_passes_and_is_normalised() {
        assert_eq!(
            clean_link("https://Example.com/me").unwrap(),
            "https://example.com/me"
        );
    }

    #[test]
    fn only_https_is_accepted() {
        for link in [
            "http://example.com",
            "javascript:alert(1)",
            "data:text/html,hi",
            "ftp://example.com",
            "example.com",
        ] {
            assert!(clean_link(link).is_err(), "{link}");
        }
    }

    #[test]
    fn credentials_and_bare_hosts_are_refused() {
        for link in [
            "https://paypal.com@evil.example/",
            "https://user:pw@example.com/",
            "https://localhost/",
            "https://127.0.0.1/",
            "https://[::1]/",
        ] {
            assert!(clean_link(link).is_err(), "{link}");
        }
    }

    #[test]
    fn hidden_and_direction_characters_are_refused() {
        for link in [
            "https://exa\u{200B}mple.com",
            "https://example.com/\u{202E}gpj.exe",
            "https://example.com/\u{2066}a",
            "https://example.com/a b",
            "https://example.com/\u{00AD}",
        ] {
            assert!(clean_link(link).is_err(), "{link:?}");
        }
    }

    #[test]
    fn a_lookalike_host_shows_as_punycode() {
        let clean = clean_link("https://p\u{0430}ypal.com/").unwrap(); // Cyrillic a
        assert!(clean.contains("xn--"), "{clean}");
    }

    #[test]
    fn at_most_five_links_and_no_repeats() {
        let six: Vec<String> = (0..6).map(|n| format!("https://a{n}.example/")).collect();
        assert!(clean_links(&six).is_err());
        assert_eq!(clean_links(&six[..5]).unwrap().len(), 5);
        let twice = vec!["https://a.example/".to_string(); 2];
        assert!(clean_links(&twice).is_err());
    }

    #[test]
    fn a_long_address_is_refused() {
        let long = format!("https://example.com/{}", "a".repeat(300));
        assert!(clean_link(&long).is_err());
    }

    #[test]
    fn interests_come_from_the_fixed_list() {
        assert_eq!(
            clean_interests(&["music".into(), "music".into()]).unwrap(),
            vec!["music".to_string()]
        );
        assert!(clean_interests(&["my religion".into()]).is_err());
        let many = vec!["music".to_string(); PROFILE_MAX_INTERESTS + 1];
        assert!(clean_interests(&many).is_err());
    }

    #[test]
    fn sections_must_be_known_and_unique() {
        let about = "about".to_string();
        assert!(check_sections(Some(&[about.clone()]), Some(&["links".into()])).is_ok());
        assert!(check_sections(Some(&["crown".into()]), None).is_err());
        assert!(check_sections(Some(&[about.clone()]), Some(&[about.clone()])).is_err());
        assert!(check_sections(Some(&[about.clone(), about]), None).is_err());
    }
}
