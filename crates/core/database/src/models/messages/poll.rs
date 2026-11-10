//! Polls carried by messages: votes, closing and the "poll ended" reply.
//!
//! Votes are stored in the message (each answer keeps its voters), so
//! deleting the message deletes its votes. Everyone in the channel is told
//! only the counts; who chose what is read on request.
use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0;
use revolt_result::Result;

use crate::{events::client::EventV1, Channel, Database, Message, Poll, PollAnswer, SystemMessage};

impl From<Poll> for v0::Poll {
    fn from(poll: Poll) -> Self {
        v0::Poll {
            question: poll.question,
            answers: poll
                .answers
                .into_iter()
                .map(|answer| v0::PollAnswer {
                    id: answer.id,
                    text: answer.text,
                    emoji: answer.emoji,
                    count: answer.voters.len() as u32,
                })
                .collect(),
            allow_multiselect: poll.allow_multiselect,
            expires_at: poll.expires_at,
            ended: poll.ended,
            me: None,
        }
    }
}

impl Poll {
    /// A new poll from what the author sent, open from `now`
    pub fn from_data(data: v0::DataPoll, now: Timestamp) -> Poll {
        Poll {
            question: data.question.trim().to_string(),
            answers: data
                .answers
                .into_iter()
                .enumerate()
                .map(|(index, answer)| PollAnswer {
                    id: index as u32 + 1,
                    text: answer.text.trim().to_string(),
                    emoji: answer.emoji,
                    voters: Default::default(),
                })
                .collect(),
            allow_multiselect: data.allow_multiselect,
            expires_at: now + Duration::hours(data.duration as i64),
            ended: false,
        }
    }

    /// Whether voting is closed at `now`
    pub fn is_closed_at(&self, now: Timestamp) -> bool {
        self.ended || now >= self.expires_at
    }

    /// The answer with this id
    pub fn answer(&self, id: u32) -> Option<&PollAnswer> {
        self.answers.iter().find(|answer| answer.id == id)
    }

    /// Ids of the answers a user chose
    pub fn choices_of(&self, user: &str) -> Vec<u32> {
        self.answers
            .iter()
            .filter(|answer| answer.voters.contains(user))
            .map(|answer| answer.id)
            .collect()
    }

    /// Counts of every answer
    pub fn counts(&self) -> Vec<v0::PollAnswerCount> {
        self.answers
            .iter()
            .map(|answer| v0::PollAnswerCount {
                id: answer.id,
                count: answer.voters.len() as u32,
            })
            .collect()
    }

    /// The user's whole choice after choosing `answer`: only it on a
    /// single-choice poll, added to the others on a multiple-choice one
    pub fn choice_with(&self, user: &str, answer: u32) -> Vec<u32> {
        if self.allow_multiselect {
            let mut choice = self.choices_of(user);
            if !choice.contains(&answer) {
                choice.push(answer);
            }
            choice
        } else {
            vec![answer]
        }
    }

    /// The user's whole choice after taking back `answer`
    pub fn choice_without(&self, user: &str, answer: u32) -> Vec<u32> {
        self.choices_of(user)
            .into_iter()
            .filter(|id| *id != answer)
            .collect()
    }

    /// What the "poll ended" reply says: the answers with the most votes
    /// (none when nobody voted), their votes and the votes of all answers
    pub fn outcome(&self) -> (Vec<String>, u32, u32) {
        let total: u32 = self.answers.iter().map(|a| a.voters.len() as u32).sum();
        let best = self
            .answers
            .iter()
            .map(|a| a.voters.len() as u32)
            .max()
            .unwrap_or_default();
        if best == 0 {
            return (vec![], 0, total);
        }
        let winners = self
            .answers
            .iter()
            .filter(|a| a.voters.len() as u32 == best)
            .map(|a| a.text.clone())
            .collect();
        (winners, best, total)
    }
}

impl Message {
    /// The message as `viewer` sees it: their own choice on its poll is marked
    pub fn into_model_for(
        self,
        viewer: &str,
        user: Option<v0::User>,
        member: Option<v0::Member>,
    ) -> v0::Message {
        let me = self.poll.as_ref().map(|poll| poll.choices_of(viewer));
        let mut model = self.into_model(user, member);
        if let (Some(poll), Some(me)) = (model.poll.as_mut(), me) {
            poll.me = Some(me);
        }
        model
    }

    /// The poll of this message as `viewer` sees it
    pub fn poll_for(&self, viewer: &str) -> Option<v0::Poll> {
        self.poll.as_ref().map(|poll| {
            let mut model: v0::Poll = poll.clone().into();
            model.me = Some(poll.choices_of(viewer));
            model
        })
    }

    /// Make `choice` the user's whole choice on this message's poll and tell
    /// the channel the new counts (and the user's other sessions their
    /// choice). `answer` is the answer the request was about; `added` says
    /// whether it was chosen or taken back. Returns the message as stored
    /// after the change.
    pub async fn set_poll_choice(
        &self,
        db: &Database,
        user: &str,
        choice: Vec<u32>,
        answer: u32,
        added: bool,
    ) -> Result<Message> {
        let poll = self.poll.as_ref().ok_or_else(|| create_error!(NotFound))?;
        db.set_poll_choice(&self.id, user, &choice, poll.answers.len())
            .await?;
        let message = db.fetch_message(&self.id).await?;
        let counts = message
            .poll
            .as_ref()
            .map(Poll::counts)
            .unwrap_or_default();

        if added {
            EventV1::MessagePollVoteAdd {
                id: self.id.clone(),
                channel_id: self.channel.clone(),
                answer_id: answer,
                answers: counts,
            }
        } else {
            EventV1::MessagePollVoteRemove {
                id: self.id.clone(),
                channel_id: self.channel.clone(),
                answer_id: answer,
                answers: counts,
            }
        }
        .p(self.channel.clone())
        .await;

        EventV1::MessagePollChoice {
            id: self.id.clone(),
            channel_id: self.channel.clone(),
            answers: message
                .poll
                .as_ref()
                .map(|poll| poll.choices_of(user))
                .unwrap_or_default(),
        }
        .p(user.to_string())
        .await;

        Ok(message)
    }

    /// Close this message's poll for good, tell the channel the final counts
    /// and reply to it with the result. Only the first call does anything
    /// (crond and an early end may meet); returns whether this one did.
    pub async fn end_poll(&self, db: &Database) -> Result<bool> {
        if self.poll.is_none() || !db.end_poll(&self.id).await? {
            return Ok(false);
        }

        // the votes as they stand now, not as this copy last saw them
        let current = db.fetch_message(&self.id).await?;
        let Some(poll) = current.poll else {
            return Ok(true);
        };

        EventV1::MessagePollEnd {
            id: self.id.clone(),
            channel_id: self.channel.clone(),
            answers: poll.counts(),
        }
        .p(self.channel.clone())
        .await;

        let (winners, votes, total) = poll.outcome();
        let mut reply = SystemMessage::PollEnded {
            id: self.id.clone(),
            question: poll.question.clone(),
            winners,
            votes,
            total,
        }
        .into_message(self.channel.clone());
        reply.replies = Some(vec![self.id.clone()]);

        let is_dm = matches!(
            db.fetch_channel(&self.channel).await,
            Ok(Channel::DirectMessage { .. })
        );
        reply
            .send_without_notifications(db, None, None, is_dm, false, true)
            .await?;

        Ok(true)
    }

    /// Close every poll past its end (crond, and a vote that finds one).
    /// Returns how many were closed.
    pub async fn end_expired_polls(db: &Database, now: Timestamp, limit: i64) -> Result<usize> {
        let mut closed = 0;
        for message in db.fetch_expired_open_polls(now, limit).await? {
            if message.end_poll(db).await? {
                closed += 1;
            }
        }
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(multi: bool) -> Poll {
        Poll::from_data(
            v0::DataPoll {
                question: "  Lunch?  ".to_string(),
                answers: vec![
                    v0::DataPollAnswer {
                        text: "Soup".to_string(),
                        emoji: None,
                    },
                    v0::DataPollAnswer {
                        text: "Salad".to_string(),
                        emoji: Some("🥗".to_string()),
                    },
                    v0::DataPollAnswer {
                        text: "Bread".to_string(),
                        emoji: None,
                    },
                ],
                duration: 4,
                allow_multiselect: multi,
            },
            Timestamp::UNIX_EPOCH,
        )
    }

    #[test]
    fn answers_are_numbered_from_one_and_trimmed() {
        let p = poll(false);
        assert_eq!(p.question, "Lunch?");
        assert_eq!(p.answers.iter().map(|a| a.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(p.expires_at, Timestamp::UNIX_EPOCH + Duration::hours(4));
        assert!(!p.is_closed_at(Timestamp::UNIX_EPOCH));
        assert!(p.is_closed_at(Timestamp::UNIX_EPOCH + Duration::hours(4)));
    }

    #[test]
    fn single_choice_replaces_multiple_choice_adds() {
        let mut single = poll(false);
        single.answers[0].voters.insert("u".to_string());
        assert_eq!(single.choice_with("u", 2), vec![2]);

        let mut multi = poll(true);
        multi.answers[0].voters.insert("u".to_string());
        assert_eq!(multi.choice_with("u", 2), vec![1, 2]);
        assert_eq!(multi.choice_with("u", 1), vec![1]);
        assert_eq!(multi.choice_without("u", 1), Vec::<u32>::new());
    }

    #[test]
    fn outcome_names_winners_and_ties() {
        let mut p = poll(true);
        assert_eq!(p.outcome(), (vec![], 0, 0), "nobody voted");

        p.answers[1].voters.insert("a".to_string());
        p.answers[1].voters.insert("b".to_string());
        p.answers[0].voters.insert("a".to_string());
        assert_eq!(p.outcome(), (vec!["Salad".to_string()], 2, 3));

        p.answers[2].voters.insert("a".to_string());
        p.answers[2].voters.insert("c".to_string());
        assert_eq!(
            p.outcome(),
            (vec!["Salad".to_string(), "Bread".to_string()], 2, 5)
        );
    }

    /// Both drivers: open polls past their end are found, and only once
    /// closed; a choice replaces the user's earlier one
    #[tokio::test]
    async fn stored_polls_are_found_voted_and_closed() {
        use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

        database_test!(|db| async move {
            let now = Timestamp::now_utc();
            let message = |id: &str, ends: Timestamp| Message {
                id: id.to_string(),
                channel: "C".to_string(),
                author: "A".to_string(),
                poll: Some(Poll {
                    expires_at: ends,
                    ..poll(false)
                }),
                ..Default::default()
            };
            db.insert_message(&message("01HPOLLPAST000000000000000", now - Duration::hours(1)))
                .await
                .unwrap();
            db.insert_message(&message("01HPOLLFUTURE0000000000000", now + Duration::hours(1)))
                .await
                .unwrap();

            let found = db.fetch_expired_open_polls(now, 10).await.unwrap();
            assert_eq!(
                found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                ["01HPOLLPAST000000000000000"]
            );

            db.set_poll_choice("01HPOLLPAST000000000000000", "u", &[1], 3)
                .await
                .unwrap();
            db.set_poll_choice("01HPOLLPAST000000000000000", "u", &[2], 3)
                .await
                .unwrap();
            let stored = db.fetch_message("01HPOLLPAST000000000000000").await.unwrap();
            assert_eq!(stored.poll.as_ref().unwrap().choices_of("u"), vec![2]);

            assert!(db.end_poll("01HPOLLPAST000000000000000").await.unwrap());
            assert!(!db.end_poll("01HPOLLPAST000000000000000").await.unwrap());
            assert!(db.fetch_expired_open_polls(now, 10).await.unwrap().is_empty());

            // votes stop once it is closed
            db.set_poll_choice("01HPOLLPAST000000000000000", "v", &[1], 3)
                .await
                .unwrap();
            let stored = db.fetch_message("01HPOLLPAST000000000000000").await.unwrap();
            assert!(stored.poll.unwrap().choices_of("v").is_empty());
        });
    }

    #[test]
    fn readers_see_counts_not_voters() {
        let mut p = poll(false);
        p.answers[0].voters.insert("a".to_string());
        let model: v0::Poll = p.into();
        assert_eq!(model.answers[0].count, 1);
        let json = serde_json::to_string(&model).unwrap();
        assert!(!json.contains("\"a\""), "{json}");
    }
}
