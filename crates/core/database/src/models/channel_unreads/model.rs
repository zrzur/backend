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

    /// Mentions are added by a debounced task (`tasks/ack.rs`) seconds after
    /// the message; these cases are that task writing before or after the
    /// reader's ack. A read must never come back as a mention.
    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_mention_read_before_the_task_runs_stays_read() {
        database_test!(|db| async move {
            let (channel, user) = (Ulid::new().to_string(), Ulid::new().to_string());
            let mention = Ulid::new().to_string();

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
            let (channel, user) = (Ulid::new().to_string(), Ulid::new().to_string());
            let mention = Ulid::new().to_string();

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
            let (channel, user) = (Ulid::new().to_string(), Ulid::new().to_string());
            let first = Ulid::new().to_string();
            let second = Ulid::new().to_string();
            assert!(first < second);

            // both written by one task run, then only the first is read
            db.add_mention_to_unread(&channel, &user, &[first.clone(), second.clone()])
                .await
                .unwrap();
            db.acknowledge_message(&channel, &user, &first).await.unwrap();
            let unread = db.fetch_unread(&user, &channel).await.unwrap().unwrap();
            assert_eq!(unread.mentions, Some(vec![second.clone()]));

            // and the same when the read lands before the task writes them
            let (channel, user) = (Ulid::new().to_string(), Ulid::new().to_string());
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
            let channel = Ulid::new().to_string();
            let (reader, other) = (Ulid::new().to_string(), Ulid::new().to_string());
            let mention = Ulid::new().to_string();

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
}
