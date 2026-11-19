//! Links of an event.
//!
//! The share link is `<invite host>/e/<name>-<code>` (or `/e/<code>`): the
//! code is unique and finds the event, the name is the organiser's choice
//! and only makes the link readable. Someone outside the server learns
//! through it the title, the time, the cover and the server's name, and
//! only when an organiser opened the link (`link_public`); never a count
//! and never a person.
//!
//! An invitation link adds the member of the server who opens it to the
//! invited people of the event. It can run out by time or by uses.

use iso8601_timestamp::Timestamp;
use revolt_models::v0::{ServerEventLink, ServerEventStatus};

use crate::events::client::EventV1;
use crate::{ServerEvent, ServerEventInvitation};

/// Letters of a link code: no vowels (no words by accident) and nothing
/// that reads as another letter
const CODE_ALPHABET: [char; 28] = [
    '2', '3', '4', '5', '6', '7', '8', '9', 'b', 'c', 'd', 'f', 'g', 'h', 'j', 'k', 'm', 'n',
    'p', 'q', 'r', 's', 't', 'v', 'w', 'x', 'y', 'z',
];
/// Length of a share link code
pub const LINK_CODE_LENGTH: usize = 8;
/// Length of an invitation code: longer, it lets people in
pub const INVITATION_CODE_LENGTH: usize = 16;
/// Longest name in a link
pub const MAX_LINK_NAME: usize = 40;
/// Invitation links one event can have
pub const MAX_INVITATION_LINKS: usize = 10;

/// A new share link code
pub fn new_link_code() -> String {
    nanoid::nanoid!(LINK_CODE_LENGTH, &CODE_ALPHABET)
}

/// A new invitation code
pub fn new_invitation_code() -> String {
    nanoid::nanoid!(INVITATION_CODE_LENGTH, &CODE_ALPHABET)
}

/// The name of a link as it is kept: lower-case letters and digits of any
/// script, other characters become one `-`. Empty: no name.
pub fn clean_link_name(name: &str) -> Option<String> {
    let mut clean = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            clean.extend(c.to_lowercase());
        } else if !clean.is_empty() && !clean.ends_with('-') {
            clean.push('-');
        }
    }
    let clean: String = clean.trim_end_matches('-').chars().take(MAX_LINK_NAME).collect();
    let clean = clean.trim_end_matches('-').to_string();
    (!clean.is_empty()).then_some(clean)
}

/// The code in the last part of a link's path (`quiz-night-k7m2x9pq` or
/// `k7m2x9pq`)
pub fn code_of_link_path(path: &str) -> Option<&str> {
    let code = path.rsplit('-').next()?;
    (code.len() == LINK_CODE_LENGTH && code.chars().all(|c| CODE_ALPHABET.contains(&c)))
        .then_some(code)
}

/// The path of a link after `/e/`
pub fn link_path(link: &ServerEventLink) -> String {
    match &link.name {
        Some(name) => format!("{name}-{}", link.code),
        None => link.code.clone(),
    }
}

impl ServerEventInvitation {
    /// Whether it can still be used at `now`
    pub fn works(&self, now: Timestamp) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
            && self.max_uses.is_none_or(|max| self.uses < max)
    }
}

impl ServerEvent {
    /// Whether someone outside the server may see a little of it through
    /// its link: the organiser opened the link and it is not a draft
    pub fn open_to_link_holders(&self) -> bool {
        self.link.is_some() && self.link_public && self.status != ServerEventStatus::Draft
    }

    /// Show the event on the devices of one person who may see it now (they
    /// used an invitation link)
    pub async fn publish_to(&self, user: &str) {
        EventV1::ServerEventCreate(self.clone().into())
            .private(user.to_string())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_readable() {
        assert_eq!(clean_link_name("  Quiz Night! "), Some("quiz-night".to_string()));
        assert_eq!(clean_link_name("ليلة الألعاب"), Some("ليلة-الألعاب".to_string()));
        assert_eq!(clean_link_name("a--b__c"), Some("a-b-c".to_string()));
        assert_eq!(clean_link_name("  !!! "), None);
        assert_eq!(clean_link_name(&"x".repeat(80)).map(|name| name.len()), Some(40));
    }

    #[test]
    fn the_code_is_read_from_the_end_of_the_path() {
        let code = new_link_code();
        assert_eq!(code.len(), LINK_CODE_LENGTH);
        assert_eq!(code_of_link_path(&code), Some(code.as_str()));
        let path = format!("quiz-night-{code}");
        assert_eq!(code_of_link_path(&path), Some(code.as_str()));
        assert_eq!(code_of_link_path("quiz-night"), None);
        assert_eq!(code_of_link_path(""), None);
    }

    #[test]
    fn an_invitation_runs_out_by_time_or_by_uses() {
        let now = Timestamp::now_utc();
        let mut link = ServerEventInvitation {
            code: new_invitation_code(),
            creator: "a".to_string(),
            uses: 0,
            max_uses: Some(1),
            expires_at: None,
            created_at: now,
        };
        assert!(link.works(now));
        link.uses = 1;
        assert!(!link.works(now));
        link.max_uses = None;
        link.expires_at = Some(now);
        assert!(!link.works(now));
    }
}
