//! Who may show as typing. Clients send `BeginTyping` every few seconds, so one
//! connection keeps the answer for a moment instead of asking the database each
//! time. The rule itself is `assert_can_type`, shared with the REST route.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use revolt_database::{util::threads::assert_can_type, Database};

/// How long an answer holds. A right taken away ends within this time, which
/// is fine for a typing indicator.
const HOLD: Duration = Duration::from_secs(10);
/// One connection rarely types in more than a few channels
const MOST_CHANNELS: usize = 32;

#[derive(Default)]
pub struct TypingRights {
    answers: HashMap<String, (Instant, bool)>,
}

impl TypingRights {
    /// Whether `user_id` may show as typing in `channel_id`. Anything that
    /// can't be looked up counts as no.
    pub async fn allowed(&mut self, db: &Database, user_id: &str, channel_id: &str) -> bool {
        let now = Instant::now();
        if let Some(answer) = self.known(channel_id, now) {
            return answer;
        }

        let answer = match (db.fetch_user(user_id).await, db.fetch_channel(channel_id).await) {
            (Ok(user), Ok(channel)) => assert_can_type(db, &user, &channel).await.is_ok(),
            _ => false,
        };
        self.remember(channel_id, answer, now);
        answer
    }

    fn known(&self, channel_id: &str, now: Instant) -> Option<bool> {
        self.answers
            .get(channel_id)
            .filter(|(until, _)| now < *until)
            .map(|(_, answer)| *answer)
    }

    fn remember(&mut self, channel_id: &str, answer: bool, now: Instant) {
        if self.answers.len() >= MOST_CHANNELS {
            self.answers.retain(|_, (until, _)| now < *until);
        }
        if self.answers.len() >= MOST_CHANNELS {
            self.answers.clear();
        }
        self.answers
            .insert(channel_id.to_string(), (now + HOLD, answer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_holds_for_a_moment_only() {
        let now = Instant::now();
        let mut rights = TypingRights::default();
        assert_eq!(rights.known("a", now), None);

        rights.remember("a", false, now);
        assert_eq!(rights.known("a", now + HOLD / 2), Some(false));
        assert_eq!(rights.known("a", now + HOLD), None);
    }

    #[test]
    fn the_cache_stays_small() {
        let now = Instant::now();
        let mut rights = TypingRights::default();
        for channel in 0..(MOST_CHANNELS * 3) {
            rights.remember(&channel.to_string(), true, now);
        }
        assert!(rights.answers.len() <= MOST_CHANNELS);
    }
}
