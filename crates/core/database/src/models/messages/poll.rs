//! Polls carried by messages: votes, closing and the "poll ended" reply.
//!
//! Votes are stored in the message (each answer keeps its voters), so
//! deleting the message deletes its votes. Everyone in the channel is told
//! only the counts; who chose what is read on request.
//!
//! Every vote is one conditional write on the database: it only applies
//! while the poll is open, so a vote and the poll closing can't both win.
//! Each change raises `version`; events carry it so clients drop older ones.
use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0;
use revolt_result::Result;
use ulid::Ulid;

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
            version: poll.version,
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
            ended_at: None,
            replied: false,
            version: 0,
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

    /// Votes of every answer together
    pub fn total(&self) -> u32 {
        self.answers.iter().map(|a| a.voters.len() as u32).sum()
    }

    /// Whether choosing `answer` would change nothing for the user
    pub fn vote_changes_nothing(&self, user: &str, answer: u32) -> bool {
        let mine = self.choices_of(user);
        if self.allow_multiselect {
            mine.contains(&answer)
        } else {
            mine == [answer]
        }
    }

    /// What the "poll ended" reply says: the answers with the most votes
    /// (none when nobody voted), their votes and the votes of all answers
    pub fn outcome(&self) -> (Vec<String>, u32, u32) {
        let total = self.total();
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

    /// Choose `answer` for `user`: on a single-choice poll it replaces their
    /// earlier choice, on a multiple-choice poll only this answer is added
    /// (whatever this copy of the message says about their other answers).
    /// `PollEnded` when the poll closed meanwhile. Returns the message after
    /// the vote.
    pub async fn vote_poll(&self, db: &Database, user: &str, answer: u32) -> Result<Message> {
        let poll = self.poll.as_ref().ok_or_else(|| create_error!(NotFound))?;
        let updated = db
            .add_poll_vote(
                &self.id,
                user,
                answer,
                !poll.allow_multiselect,
                poll.answers.len(),
                Timestamp::now_utc(),
            )
            .await?
            .ok_or_else(|| create_error!(PollEnded))?;
        self.announce_vote(&updated, user, answer, true).await;
        Ok(updated)
    }

    /// Take back `user`'s choice of `answer`; `PollEnded` when it closed
    pub async fn unvote_poll(&self, db: &Database, user: &str, answer: u32) -> Result<Message> {
        let updated = db
            .remove_poll_vote(&self.id, user, answer, Timestamp::now_utc())
            .await?
            .ok_or_else(|| create_error!(PollEnded))?;
        self.announce_vote(&updated, user, answer, false).await;
        Ok(updated)
    }

    /// Counts to the channel, the user's own choice to their sessions
    async fn announce_vote(&self, updated: &Message, user: &str, answer: u32, added: bool) {
        let Some(poll) = updated.poll.as_ref() else {
            return;
        };
        let (answers, total, version) = (poll.counts(), poll.total(), poll.version);
        if added {
            EventV1::MessagePollVoteAdd {
                id: self.id.clone(),
                channel_id: self.channel.clone(),
                answer_id: answer,
                answers,
                total,
                version,
            }
        } else {
            EventV1::MessagePollVoteRemove {
                id: self.id.clone(),
                channel_id: self.channel.clone(),
                answer_id: answer,
                answers,
                total,
                version,
            }
        }
        .p(self.channel.clone())
        .await;

        EventV1::MessagePollChoice {
            id: self.id.clone(),
            channel_id: self.channel.clone(),
            answers: poll.choices_of(user),
            version,
        }
        .p(user.to_string())
        .await;
    }

    /// Close this message's poll for good. Only the first call closes it
    /// (crond and an early end may meet); returns whether this one did. The
    /// result reply follows at once; if posting it fails, crond posts it on
    /// its next pass, so a closed poll never stays without its reply.
    pub async fn end_poll(&self, db: &Database) -> Result<bool> {
        if self.poll.is_none() || !db.end_poll(&self.id, Timestamp::now_utc()).await? {
            return Ok(false);
        }
        if let Err(error) = Message::finish_poll(db, &self.id).await {
            revolt_config::capture_error(&error);
        }
        Ok(true)
    }

    /// Id of a poll's "poll ended" reply: the time it closed, with the random
    /// part of the poll's own id. The same poll always gets the same id, so
    /// posting the reply twice stores it once.
    pub fn poll_reply_id(poll_message: &str, ended_at: Timestamp) -> String {
        let random = Ulid::from_string(poll_message)
            .map(|ulid| ulid.random())
            .unwrap_or_default();
        let millis = ended_at
            .duration_since(Timestamp::UNIX_EPOCH)
            .whole_milliseconds()
            .max(0) as u64;
        Ulid::from_parts(millis, random).to_string()
    }

    /// For a closed poll: tell the channel the final counts, post the reply
    /// once (a second try finds it stored) and note that it is posted
    pub async fn finish_poll(db: &Database, id: &str) -> Result<()> {
        let message = db.fetch_message(id).await?;
        let Some(poll) = message.poll.as_ref() else {
            return Ok(());
        };
        if !poll.ended || poll.replied {
            return Ok(());
        }
        let ended_at = poll.ended_at.unwrap_or(poll.expires_at);
        let reply_id = Message::poll_reply_id(&message.id, ended_at);

        if db.fetch_message(&reply_id).await.is_err() {
            EventV1::MessagePollEnd {
                id: message.id.clone(),
                channel_id: message.channel.clone(),
                answers: poll.counts(),
                total: poll.total(),
                version: poll.version,
            }
            .p(message.channel.clone())
            .await;

            let (winners, votes, total) = poll.outcome();
            let mut reply = SystemMessage::PollEnded {
                id: message.id.clone(),
                question: poll.question.clone(),
                winners,
                votes,
                total,
            }
            .into_message(message.channel.clone());
            reply.id = reply_id.clone();
            reply.replies = Some(vec![message.id.clone()]);

            let is_dm = matches!(
                db.fetch_channel(&message.channel).await,
                Ok(Channel::DirectMessage { .. })
            );
            if let Err(error) = reply
                .send_without_notifications(db, None, None, is_dm, false, true)
                .await
            {
                // stored by another try in the meantime: that one counts
                if db.fetch_message(&reply_id).await.is_err() {
                    return Err(error);
                }
            }
        }

        db.mark_poll_replied(&message.id).await
    }

    /// crond's pass: close every poll past its end and finish every closed
    /// poll whose reply is missing. One message failing is logged and the
    /// pass goes on. Returns how many were finished.
    pub async fn close_due_polls(db: &Database, now: Timestamp, limit: i64) -> Result<usize> {
        let mut finished = 0;
        for message in db.fetch_polls_to_close(now, limit).await? {
            let result = async {
                db.end_poll(&message.id, now).await?;
                Message::finish_poll(db, &message.id).await
            }
            .await;
            match result {
                Ok(()) => finished += 1,
                Err(error) => {
                    log::error!("poll {} not finished: {error:?}", message.id);
                    revolt_config::capture_error(&error);
                }
            }
        }
        Ok(finished)
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
    fn a_vote_that_changes_nothing_is_seen() {
        let mut single = poll(false);
        single.answers[0].voters.insert("u".to_string());
        assert!(single.vote_changes_nothing("u", 1));
        assert!(!single.vote_changes_nothing("u", 2));

        let mut multi = poll(true);
        multi.answers[0].voters.insert("u".to_string());
        multi.answers[1].voters.insert("u".to_string());
        assert!(multi.vote_changes_nothing("u", 2));
        assert!(!multi.vote_changes_nothing("u", 3));
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

    #[test]
    fn the_reply_id_is_the_same_for_the_same_poll() {
        let at = Timestamp::UNIX_EPOCH + Duration::hours(1);
        let a = Message::poll_reply_id("01HZZZZZZZZZZZZZZZZZZZZZZZ", at);
        assert_eq!(a, Message::poll_reply_id("01HZZZZZZZZZZZZZZZZZZZZZZZ", at));
        assert_ne!(a, Message::poll_reply_id("01HZZZZZZZZZZZZZZZZZZZZZZY", at));
        assert!(Ulid::from_string(&a).is_ok());
    }

    fn stored(id: &str, ends: Timestamp, multi: bool) -> Message {
        Message {
            id: id.to_string(),
            channel: "C".to_string(),
            author: "A".to_string(),
            poll: Some(Poll {
                expires_at: ends,
                ..poll(multi)
            }),
            ..Default::default()
        }
    }

    /// Both drivers: polls to close are found; a single-choice vote replaces
    /// the earlier one; every change raises the version
    #[tokio::test]
    async fn stored_polls_are_found_voted_and_closed() {
        use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

        database_test!(|db| async move {
            let now = Timestamp::now_utc();
            const PAST: &str = "01HPOLLPAST000000000000000";
            const FUTURE: &str = "01HPOLLFUTURE0000000000000";
            db.insert_message(&stored(PAST, now - Duration::hours(1), false))
                .await
                .unwrap();
            db.insert_message(&stored(FUTURE, now + Duration::hours(1), false))
                .await
                .unwrap();

            let found = db.fetch_polls_to_close(now, 10).await.unwrap();
            assert_eq!(found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), [PAST]);

            // an expired poll takes no vote even before it is closed
            assert!(db
                .add_poll_vote(PAST, "u", 1, true, 3, now)
                .await
                .unwrap()
                .is_none());

            let first = db.add_poll_vote(FUTURE, "u", 1, true, 3, now).await.unwrap().unwrap();
            let second = db.add_poll_vote(FUTURE, "u", 2, true, 3, now).await.unwrap().unwrap();
            assert_eq!(second.poll.as_ref().unwrap().choices_of("u"), vec![2]);
            assert!(second.poll.as_ref().unwrap().version > first.poll.as_ref().unwrap().version);

            assert!(db.end_poll(PAST, now).await.unwrap());
            assert!(!db.end_poll(PAST, now).await.unwrap());
            // closed but not answered: still to finish
            assert_eq!(db.fetch_polls_to_close(now, 10).await.unwrap().len(), 1);
            db.mark_poll_replied(PAST).await.unwrap();
            assert!(db.fetch_polls_to_close(now, 10).await.unwrap().is_empty());
        });
    }

    /// Both drivers: on a multiple-choice poll two votes made from the same
    /// (stale) copy of the message both count
    #[tokio::test]
    async fn multiple_choice_adds_only_the_one_answer() {
        use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

        database_test!(|db| async move {
            const ID: &str = "01HPOLLMULTI00000000000000";
            let message = stored(ID, Timestamp::now_utc() + Duration::hours(1), true);
            db.insert_message(&message).await.unwrap();

            message.vote_poll(&db, "u", 1).await.unwrap();
            let after = message.vote_poll(&db, "u", 3).await.unwrap();
            assert_eq!(after.poll.as_ref().unwrap().choices_of("u"), vec![1, 3]);

            let after = message.unvote_poll(&db, "u", 1).await.unwrap();
            assert_eq!(after.poll.as_ref().unwrap().choices_of("u"), vec![3]);
        });
    }

    /// Both drivers: a vote that loses the race with the poll closing is
    /// refused with PollEnded and changes nothing
    #[tokio::test]
    async fn a_vote_after_the_close_is_refused() {
        use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

        database_test!(|db| async move {
            const ID: &str = "01HPOLLRACE000000000000000";
            let stale = stored(ID, Timestamp::now_utc() + Duration::hours(1), false);
            db.insert_message(&stale).await.unwrap();

            assert!(db.end_poll(ID, Timestamp::now_utc()).await.unwrap());
            let error = stale.vote_poll(&db, "u", 1).await.unwrap_err();
            assert!(
                matches!(error.error_type, revolt_result::ErrorType::PollEnded),
                "{error:?}"
            );
            let error = stale.unvote_poll(&db, "u", 1).await.unwrap_err();
            assert!(matches!(error.error_type, revolt_result::ErrorType::PollEnded));
            let stored = db.fetch_message(ID).await.unwrap();
            assert!(stored.poll.unwrap().choices_of("u").is_empty());
        });
    }

    /// Both drivers: the user's own votes are found for their data export
    #[tokio::test]
    async fn the_polls_a_user_voted_on_are_found() {
        use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

        database_test!(|db| async move {
            let later = Timestamp::now_utc() + Duration::hours(1);
            db.insert_message(&stored("01HPOLLVOTEDA0000000000000", later, false))
                .await
                .unwrap();
            db.insert_message(&stored("01HPOLLVOTEDB0000000000000", later, false))
                .await
                .unwrap();
            db.add_poll_vote("01HPOLLVOTEDA0000000000000", "u", 2, true, 3, Timestamp::now_utc())
                .await
                .unwrap();
            let voted = db.fetch_messages_voted_by("u", 10).await.unwrap();
            assert_eq!(
                voted.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
                ["01HPOLLVOTEDA0000000000000"]
            );
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
