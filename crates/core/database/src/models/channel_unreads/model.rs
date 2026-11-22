auto_derived!(
    /// Channel Unread
    pub struct ChannelUnread {
        /// Composite key pointing to a user's view of a channel
        #[serde(rename = "_id")]
        pub id: ChannelCompositeKey,

        /// Id of the last message read in this channel by a user
        #[serde(skip_serializing_if = "Option::is_none")]
        pub last_id: Option<String>,
        /// Array of message ids that mention the user
        #[serde(skip_serializing_if = "Option::is_none")]
        pub mentions: Option<Vec<String>>,
    }

    /// Composite primary key consisting of channel and user id
    #[derive(Hash)]
    pub struct ChannelCompositeKey {
        /// Channel Id
        pub channel: String,
        /// User Id
        pub user: String,
    }
);

#[cfg(test)]
mod tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
    use ulid::Ulid;

    /// ULIDs in the order they are made: two from `Ulid::new()` in the same
    /// millisecond come in random order, and these tests compare them
    fn next_id() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut prev = LAST.load(Ordering::SeqCst);
        loop {
            let next = now.max(prev + 1);
            match LAST.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Ulid::from_parts(next, Ulid::new().random()).to_string(),
                Err(seen) => prev = seen,
            }
        }
    }

    /// Mentions are added by a debounced task (`tasks/ack.rs`) seconds after
    /// the message; these cases are that task writing before or after the
    /// reader's ack. A read must never come back as a mention.
    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_mention_read_before_the_task_runs_stays_read() {
        database_test!(|db| async move {
            let (channel, user) = (next_id(), next_id());
            let mention = next_id();

            // A reads the mention, then the debounced task writes it
            db.acknowledge_message(&channel, &user, &mention).await.unwrap();
            db.add_mention_to_unread(&channel, &user, &[mention.clone()])
                .await
                .unwrap();

            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert!(unread.mentions.unwrap_or_default().is_empty());
            assert_eq!(unread.last_id, Some(mention));
        });
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_mention_written_then_read_is_cleared() {
        database_test!(|db| async move {
            let (channel, user) = (next_id(), next_id());
            let mention = next_id();

            db.add_mention_to_unread(&channel, &user, &[mention.clone()])
                .await
                .unwrap();
            db.acknowledge_message(&channel, &user, &mention).await.unwrap();

            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert!(unread.mentions.unwrap_or_default().is_empty());
        });
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn reading_the_first_of_two_mentions_leaves_the_second() {
        database_test!(|db| async move {
            let (channel, user) = (next_id(), next_id());
            let first = next_id();
            let second = next_id();
            assert!(first < second);

            // both written by one task run, then only the first is read
            db.add_mention_to_unread(&channel, &user, &[first.clone(), second.clone()])
                .await
                .unwrap();
            db.acknowledge_message(&channel, &user, &first).await.unwrap();
            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![second.clone()]));

            // and the same when the read lands before the task writes them
            let (channel, user) = (next_id(), next_id());
            db.acknowledge_message(&channel, &user, &first).await.unwrap();
            db.add_mention_to_unread(&channel, &user, &[first.clone(), second.clone()])
                .await
                .unwrap();
            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![second]));
            // the task never moves the read mark
            assert_eq!(unread.last_id, Some(first));
        });
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn mass_mentions_skip_what_each_reader_has_read() {
        database_test!(|db| async move {
            let channel = next_id();
            let (reader, other) = (next_id(), next_id());
            let mention = next_id();

            db.acknowledge_message(&channel, &reader, &mention).await.unwrap();
            db.acknowledge_message(&channel, &other, &Ulid::nil().to_string())
                .await
                .unwrap();
            db.add_mention_to_many_unreads(
                &channel,
                &[reader.clone(), other.clone()],
                &[mention.clone()],
            )
            .await
            .unwrap();

            let read = db.fetch_unread(&reader, &channel).await.unwrap().unwrap();
            assert!(read.mentions.unwrap_or_default().is_empty());
            let unread = db.fetch_unread(&other, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![mention]));
        });
    }

    /// pushd's mass mention consumer, as it calls the database: the ids it
    /// adds must survive the read-mark check for a reader who read the
    /// channel before the @everyone message
    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn an_everyone_mention_after_a_read_shows() {
        database_test!(|db| async move {
            use crate::events::rabbit::mass_mention_ids;

            // the channel exists first, the reader reads a message, then
            // someone writes @everyone
            let channel = next_id();
            let reader = next_id();
            let read = next_id();
            let message = next_id();
            db.acknowledge_message(&channel, &reader, &read).await.unwrap();

            let push = revolt_models::v0::PushNotification {
                author: "author".to_string(),
                icon: String::new(),
                image: None,
                body: "@everyone".to_string(),
                raw_body: None,
                tag: channel.clone(),
                timestamp: 0,
                url: String::new(),
                message: crate::Message {
                    id: message.clone(),
                    channel: channel.clone(),
                    author: next_id(),
                    ..Default::default()
                }
                .into_model(None, None),
                channel: crate::Channel::SavedMessages {
                    id: channel.clone(),
                    user: reader.clone(),
                }
                .into(),
            };

            // what pushd passed before: the channel id, older than the read
            // mark, so the badge was dropped
            let before = vec![push.channel.id().to_string()];
            db.add_mention_to_many_unreads(&channel, &[reader.clone()], &before)
                .await
                .unwrap();
            let unread = db.fetch_unread(&reader, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, None, "the old ids are dropped");

            // what it passes now: the message, which shows
            db.add_mention_to_many_unreads(&channel, &[reader.clone()], &mass_mention_ids(&push))
                .await
                .unwrap();
            let unread = db.fetch_unread(&reader, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![message]));
        });
    }

    /// Both databases leave the same shape when nothing new is added: no
    /// mentions at all (not an empty list), and earlier ones untouched
    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn nothing_new_leaves_the_mentions_as_they_were() {
        database_test!(|db| async move {
            let (channel, user) = (next_id(), next_id());
            let old = next_id();
            let read = next_id();
            let kept = next_id();

            // a read mark and no mentions: an old id adds nothing
            db.acknowledge_message(&channel, &user, &read).await.unwrap();
            db.add_mention_to_unread(&channel, &user, &[old.clone()])
                .await
                .unwrap();
            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, None);

            // a mention after the mark, then an old id: the list stays
            db.add_mention_to_unread(&channel, &user, &[kept.clone()])
                .await
                .unwrap();
            db.add_mention_to_unread(&channel, &user, &[old]).await.unwrap();
            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![kept]));
        });
    }
}
