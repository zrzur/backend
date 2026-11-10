use std::collections::{BTreeMap, HashMap};

use revolt_result::Result;

use crate::ReferenceDb;
use crate::{InsightCounter, InsightHour, MessageTally, ServerInsightDay};

use super::AbstractServerInsights;

fn add(map: &mut BTreeMap<String, u32>, key: impl Into<String>, by: u32) {
    let slot = map.entry(key.into()).or_default();
    *slot = slot.saturating_add(by);
}

fn new_row(server: &str, day: i64) -> ServerInsightDay {
    ServerInsightDay {
        id: ServerInsightDay::key(server, day),
        server: server.to_string(),
        day,
        ..Default::default()
    }
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
            .or_insert_with(|| new_row(server, day));
        match counter {
            InsightCounter::Join(source) => {
                add(&mut row.joins, slot, 1);
                add(&mut row.sources, source.key(), 1);
            }
            InsightCounter::Leave => add(&mut row.leaves, slot, 1),
            InsightCounter::VoiceSeconds(seconds) => add(&mut row.voice, slot, *seconds),
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
        let mut rows = self.server_insights.lock().await;
        let row = rows
            .entry(ServerInsightDay::key(server, day))
            .or_insert_with(|| new_row(server, day));
        let of_day = hour.rem_euclid(24);
        if row.hours.contains(&of_day) {
            return Ok(false);
        }
        row.hours.push(of_day);
        row.open = true;
        let per_channel = row.messages.entry(of_day.to_string()).or_default();
        for (channel, n) in &counts.channels {
            add(per_channel, channel.clone(), *n);
        }
        for (channel, n) in &counts.channels {
            add(&mut row.channels, channel.clone(), *n);
        }
        Ok(true)
    }

    async fn set_server_insight_active(&self, server: &str, day: i64, active: u32) -> Result<()> {
        if let Some(row) = self
            .server_insights
            .lock()
            .await
            .get_mut(&ServerInsightDay::key(server, day))
        {
            row.active = active;
        }
        Ok(())
    }

    async fn fetch_open_server_insight_days(
        &self,
        day: i64,
        limit: i64,
    ) -> Result<Vec<(String, i64)>> {
        let rows = self.server_insights.lock().await;
        Ok(rows
            .values()
            .filter(|row| row.open && row.day < day)
            .take(limit.max(0) as usize)
            .map(|row| (row.server.clone(), row.day))
            .collect())
    }

    async fn close_server_insight_day(
        &self,
        server: &str,
        day: i64,
        active: Option<u32>,
    ) -> Result<()> {
        let mut rows = self.server_insights.lock().await;
        if let Some(row) = rows.get_mut(&ServerInsightDay::key(server, day)) {
            row.open = false;
            if let Some(active) = active {
                row.active = active;
            }
        }
        Ok(())
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
        Ok(self.server_insights_cursor.lock().await.map(|(hour, _)| hour))
    }

    async fn set_server_insight_cursor(&self, hour: i64, since: Option<i64>) -> Result<()> {
        *self.server_insights_attempts.lock().await = None;
        let mut cursor = self.server_insights_cursor.lock().await;
        let kept = cursor.and_then(|(_, kept)| kept).or(since);
        *cursor = Some((hour, kept));
        Ok(())
    }

    async fn fetch_server_insight_since(&self) -> Result<Option<i64>> {
        Ok(self.server_insights_cursor.lock().await.and_then(|(_, since)| since))
    }

    async fn note_server_insight_attempt(&self, hour: i64) -> Result<u32> {
        let mut attempts = self.server_insights_attempts.lock().await;
        let count = match *attempts {
            Some((failed, count)) if failed == hour => count + 1,
            _ => 1,
        };
        *attempts = Some((hour, count));
        Ok(count)
    }

    async fn fetch_server_ids_among(&self, ids: &[String]) -> Result<Vec<String>> {
        let servers = self.servers.lock().await;
        Ok(ids.iter().filter(|id| servers.contains_key(*id)).cloned().collect())
    }

    async fn fetch_bot_ids_among(&self, ids: &[String]) -> Result<Vec<String>> {
        let users = self.users.lock().await;
        Ok(ids
            .iter()
            .filter(|id| users.get(*id).is_some_and(|user| user.bot.is_some()))
            .cloned()
            .collect())
    }
}
