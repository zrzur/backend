use iso8601_timestamp::Timestamp;

use super::{Member, User};

#[cfg(feature = "rocket")]
use rocket::FromForm;

auto_derived!(
    /// Poll carried by a message
    ///
    /// Answers show only how many people chose them; who chose what is read
    /// with `GET /channels/{target}/messages/{msg}/polls/answers/{answer}/voters`.
    pub struct Poll {
        /// The question asked
        pub question: String,
        /// Answers in the order the author gave them
        pub answers: Vec<PollAnswer>,
        /// Whether people may choose more than one answer
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub allow_multiselect: bool,
        /// When voting closes
        pub expires_at: Timestamp,
        /// Whether the poll is over: ended early, or closed after
        /// `expires_at` (a poll past `expires_at` is over even before this is
        /// set)
        #[serde(skip_serializing_if = "crate::if_false", default)]
        pub ended: bool,
        /// Ids of the answers the user reading this chose (only in answers
        /// made for that user: message fetches, queries and votes)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub me: Option<Vec<u32>>,
        /// Goes up with every vote and with closing; poll events carry the
        /// same number, so an event older than this can be dropped
        #[serde(skip_serializing_if = "crate::if_zero_u64", default)]
        pub version: u64,
    }

    /// One answer of a poll
    pub struct PollAnswer {
        /// Answer id, 1 to 10 in the order given
        pub id: u32,
        /// Answer text
        pub text: String,
        /// Unicode emoji or custom emoji id shown with the answer
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub emoji: Option<String>,
        /// How many people chose it
        #[serde(default)]
        pub count: u32,
    }

    /// Vote count of one answer
    pub struct PollAnswerCount {
        /// Answer id
        pub id: u32,
        /// How many people chose it
        pub count: u32,
    }

    /// Poll to send with a message
    pub struct DataPoll {
        /// The question asked (1 to 300 characters)
        pub question: String,
        /// Answers (2 to 10)
        pub answers: Vec<DataPollAnswer>,
        /// How long voting stays open, in hours: 1, 4, 8, 24, 72, 168 or 336
        pub duration: u32,
        /// Whether people may choose more than one answer
        #[serde(default)]
        pub allow_multiselect: bool,
    }

    /// Answer of a poll to send
    pub struct DataPollAnswer {
        /// Answer text (1 to 55 characters)
        pub text: String,
        /// Unicode emoji or the id of a custom emoji the author can use
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub emoji: Option<String>,
    }

    /// Page of the people who chose an answer
    pub struct PollVoters {
        /// User ids of this page, oldest vote first
        pub ids: Vec<String>,
        /// Those users
        pub users: Vec<User>,
        /// Their memberships (server channels only)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub members: Option<Vec<Member>>,
        /// Total number of people who chose this answer
        pub count: u32,
    }
);

auto_derived!(
    /// Options for listing voters
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsPollVoters {
        /// Number of voters to return (1 to 100, default 50)
        pub limit: Option<u32>,
        /// User id after which to continue (the last one of the page before)
        pub after: Option<String>,
    }
);

impl DataPoll {
    /// Allowed durations in hours: 1 h, 4 h, 8 h, 24 h, 3 days, 1 week, 2 weeks
    pub const DURATIONS: [u32; 7] = [1, 4, 8, 24, 72, 168, 336];
    /// Longest question in characters
    pub const QUESTION_MAX: usize = 300;
    /// Longest answer in characters
    pub const ANSWER_MAX: usize = 55;
    /// Fewest answers
    pub const ANSWERS_MIN: usize = 2;
    /// Most answers
    pub const ANSWERS_MAX: usize = 10;

    /// Check the question, the answers and the duration. Lengths count
    /// characters, not bytes, and text that is only spaces counts as empty.
    pub fn validate_poll(&self) -> Result<(), String> {
        fn text(what: &str, value: &str, max: usize) -> Result<(), String> {
            let len = value.trim().chars().count();
            if len == 0 || value.chars().count() > max {
                Err(format!("{what} must be 1 to {max} characters"))
            } else {
                Ok(())
            }
        }

        text("question", &self.question, Self::QUESTION_MAX)?;

        if self.answers.len() < Self::ANSWERS_MIN || self.answers.len() > Self::ANSWERS_MAX {
            return Err(format!(
                "a poll has {} to {} answers",
                Self::ANSWERS_MIN,
                Self::ANSWERS_MAX
            ));
        }

        for answer in &self.answers {
            text("answer", &answer.text, Self::ANSWER_MAX)?;
            if let Some(emoji) = &answer.emoji {
                let len = emoji.chars().count();
                if len == 0 || len > 128 || emoji.chars().any(char::is_whitespace) {
                    return Err("answer emoji is not valid".to_string());
                }
            }
        }

        if !Self::DURATIONS.contains(&self.duration) {
            return Err("duration must be 1, 4, 8, 24, 72, 168 or 336 hours".to_string());
        }

        Ok(())
    }
}

impl Poll {
    /// Whether voting is closed at `now`
    pub fn is_closed_at(&self, now: Timestamp) -> bool {
        self.ended || now >= self.expires_at
    }

    /// Short text of the poll for places that show text only (a forwarded
    /// copy, a notification): the question, then one line per answer
    pub fn summary(&self) -> String {
        let mut lines = vec![format!("📊 {}", self.question)];
        for answer in &self.answers {
            match &answer.emoji {
                Some(emoji) if !is_custom_emoji(emoji) => {
                    lines.push(format!("• {emoji} {}", answer.text))
                }
                Some(emoji) => lines.push(format!("• :{emoji}: {}", answer.text)),
                None => lines.push(format!("• {}", answer.text)),
            }
        }
        lines.join("\n")
    }
}

/// Custom emoji are referred to by their id (a ULID)
fn is_custom_emoji(emoji: &str) -> bool {
    emoji.len() == 26 && emoji.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll() -> DataPoll {
        DataPoll {
            question: "Where do we meet?".to_string(),
            answers: vec![
                DataPollAnswer {
                    text: "Library".to_string(),
                    emoji: Some("📚".to_string()),
                },
                DataPollAnswer {
                    text: "Café".to_string(),
                    emoji: None,
                },
            ],
            duration: 24,
            allow_multiselect: false,
        }
    }

    #[test]
    fn a_plain_poll_is_valid() {
        assert_eq!(poll().validate_poll(), Ok(()));
    }

    #[test]
    fn lengths_count_characters_not_bytes() {
        let mut p = poll();
        p.question = "س".repeat(300);
        p.answers[0].text = "ج".repeat(55);
        assert_eq!(p.validate_poll(), Ok(()), "Arabic at the limits");
        p.question = "س".repeat(301);
        assert!(p.validate_poll().is_err(), "question over 300");
        p.question = "q".to_string();
        p.answers[0].text = "ج".repeat(56);
        assert!(p.validate_poll().is_err(), "answer over 55");
    }

    #[test]
    fn blank_text_is_refused() {
        let mut p = poll();
        p.question = "   ".to_string();
        assert!(p.validate_poll().is_err());
        let mut p = poll();
        p.answers[1].text = String::new();
        assert!(p.validate_poll().is_err());
    }

    #[test]
    fn two_to_ten_answers() {
        let mut p = poll();
        p.answers.truncate(1);
        assert!(p.validate_poll().is_err(), "one answer");
        let mut p = poll();
        p.answers = (0..10)
            .map(|n| DataPollAnswer {
                text: format!("answer {n}"),
                emoji: None,
            })
            .collect();
        assert_eq!(p.validate_poll(), Ok(()), "ten answers");
        p.answers.push(DataPollAnswer {
            text: "eleven".to_string(),
            emoji: None,
        });
        assert!(p.validate_poll().is_err(), "eleven answers");
    }

    #[test]
    fn only_the_offered_durations() {
        for hours in DataPoll::DURATIONS {
            let mut p = poll();
            p.duration = hours;
            assert_eq!(p.validate_poll(), Ok(()), "{hours} h");
        }
        for hours in [0, 2, 25, 337, 10_000] {
            let mut p = poll();
            p.duration = hours;
            assert!(p.validate_poll().is_err(), "{hours} h");
        }
    }

    #[test]
    fn summary_lists_question_and_answers() {
        let p = Poll {
            question: "Where?".to_string(),
            answers: vec![
                PollAnswer {
                    id: 1,
                    text: "Library".to_string(),
                    emoji: Some("📚".to_string()),
                    count: 3,
                },
                PollAnswer {
                    id: 2,
                    text: "Café".to_string(),
                    emoji: Some("01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string()),
                    count: 0,
                },
            ],
            allow_multiselect: false,
            expires_at: Timestamp::UNIX_EPOCH,
            ended: false,
            me: None,
            version: 0,
        };
        assert_eq!(
            p.summary(),
            "📊 Where?\n• 📚 Library\n• :01HZZZZZZZZZZZZZZZZZZZZZZZ: Café"
        );
    }
}
