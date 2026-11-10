use std::collections::{BTreeMap, HashMap};

use revolt_result::Result;

use crate::ReferenceDb;
use crate::{InsightCounter, InsightHour, MessageTally, ServerInsightDay};

use super::AbstractServerInsights;

fn add(map: &mut BTreeMap<String, u32>, key: impl Into<String>, by: u32) {
    let slot = map.entry(key.into()).or_default();
    *slot = slot.saturating_add(by);
}

#[async_trait]
impl AbstractServerInsights for ReferenceDb {
    async fn add_server_insight(
        &self,
        server: &str,
        hour: i64,
        counter: &InsightCounter,
    ) -> Result<()> {
        let day = hour.div_euclid(24);
        let slot = hour.rem_euclid(24).to_string();
        let mut rows = self.server_insights.lock().await;
        let row = rows
            .entry(ServerInsightDay::key(server, day))
            .or_insert_with(|| ServerInsightDay {
                id: ServerInsightDay::key(server, day),
                server: server.to_string(),
                day,
                ..Default::default()
            });
        match counter {
            InsightCounter::Join(source) => {
                add(&mut row.joins, slot, 1);
                add(&mut row.sources, source.key(), 1);
            }
            InsightCounter::Leave => add(&mut row.leaves, slot, 1),
            InsightCounter::VoiceMinutes(minutes) => add(&mut row.voice, slot, *minutes),
        }
        Ok(())
    }

    async fn tally_messages_between(&self, from: &str, to: &str) -> Result<Vec<MessageTally>> {
        let messages = self.messages.lock().await;
        let mut tallies: HashMap<(String, Option<String>), u32> = HashMap::new();
        for message in messages.values() {
            if message.id.as_str() < from || message.id.as_str() >= to || message.system.is_some()
            {
                continue;
            }
            let author = message.webhook.is_none().then(|| message.author.clone());
            *tallies.entry((message.channel.clone(), author)).or_default() += 1;
        }
        Ok(tallies
            .into_iter()
            .map(|((channel, author), count)| MessageTally {
                channel,
                author,
                count,
            })
            .collect())
    }

    async fn add_server_insight_hour(
        &self,
        server: &str,
        hour: i64,
        counts: &InsightHour,
    ) -> Result<bool> {
        let day = hour.div_euclid(24);
        let key = ServerInsightDay::key(server, day);
        let mut rows = self.server_insights.lock().await;
        let row = rows.entry(key.clone()).or_insert_with(|| ServerInsightDay {
            id: key.clone(),
            server: server.to_string(),
            day,
            ..Default::default()
        });
        let of_day = hour.rem_euclid(24);
        if row.hours.contains(&of_day) {
            return Ok(false);
        }
        row.hours.push(of_day);
        add(&mut row.messages, of_day.to_string(), counts.messages);
        for (channel, n) in &counts.channels {
            add(&mut row.channels, channel.clone(), *n);
        }
        // author ids of open days (in MongoDB an `authors` field of the row
        // that is never read back)
        let mut open = self.server_insight_authors.lock().await;
        let ids = open.entry(key).or_default();
        for author in &counts.authors {
            if !ids.contains(author) {
                ids.push(author.clone());
            }
        }
        row.active = ids.len() as u32;
        Ok(true)
    }

    async fn close_server_insight_days(&self, day: i64) -> Result<u64> {
        let rows = self.server_insights.lock().await;
        let mut open = self.server_insight_authors.lock().await;
        let before = open.len();
        open.retain(|key, _| rows.get(key).is_none_or(|row| row.day >= day));
        Ok((before - open.len()) as u64)
    }

    async fn delete_server_insights_before(&self, day: i64) -> Result<u64> {
        let mut rows = self.server_insights.lock().await;
        let before = rows.len();
        rows.retain(|_, row| row.day >= day);
        Ok((before - rows.len()) as u64)
    }

    async fn delete_server_insights_of(&self, server: &str) -> Result<()> {
        self.server_insights
            .lock()
            .await
            .retain(|_, row| row.server != server);
        Ok(())
    }

    async fn fetch_server_insights(
        &self,
        server: &str,
        first: i64,
        last: i64,
    ) -> Result<Vec<ServerInsightDay>> {
        let rows = self.server_insights.lock().await;
        let mut list: Vec<ServerInsightDay> = rows
            .values()
            .filter(|row| row.server == server && row.day >= first && row.day <= last)
            .cloned()
            .collect();
        list.sort_by_key(|row| row.day);
        Ok(list)
    }

    async fn fetch_server_insight_cursor(&self) -> Result<Option<i64>> {
        Ok(*self.server_insights_cursor.lock().await)
    }

    async fn set_server_insight_cursor(&self, hour: i64) -> Result<()> {
        *self.server_insights_cursor.lock().await = Some(hour);
        Ok(())
    }
}
