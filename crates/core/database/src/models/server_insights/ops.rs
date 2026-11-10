use revolt_result::Result;

use crate::{InsightCounter, InsightHour, MessageTally, ServerInsightDay};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractServerInsights: Sync + Send {
    /// Add to a counter of a server in a UTC hour (creates the day row)
    async fn add_server_insight(&self, server: &str, hour: i64, counter: &InsightCounter)
        -> Result<()>;

    /// Messages with ids in `[from, to)` grouped by channel and author
    /// (system messages left out)
    async fn tally_messages_between(&self, from: &str, to: &str) -> Result<Vec<MessageTally>>;

    /// Fold one hour's messages into a server's day row, once, and mark the
    /// day open. Returns false when that hour was in already.
    async fn add_server_insight_hour(
        &self,
        server: &str,
        hour: i64,
        counts: &InsightHour,
    ) -> Result<bool>;

    /// Open days before `day` (server, day), at most `limit`
    async fn fetch_open_server_insight_days(&self, day: i64, limit: i64)
        -> Result<Vec<(String, i64)>>;

    /// Close a day: its active count is final (`None` keeps the last one)
    async fn close_server_insight_day(&self, server: &str, day: i64, active: Option<u32>)
        -> Result<()>;

    /// Delete rows of days before `day`
    async fn delete_server_insights_before(&self, day: i64) -> Result<u64>;

    /// Delete every row of a server (the server is deleted)
    async fn delete_server_insights_of(&self, server: &str) -> Result<()>;

    /// Rows of a server from day `first` to day `last`, both included
    async fn fetch_server_insights(
        &self,
        server: &str,
        first: i64,
        last: i64,
    ) -> Result<Vec<ServerInsightDay>>;

    /// The last hour crond rolled up, if it ever did
    async fn fetch_server_insight_cursor(&self) -> Result<Option<i64>>;

    /// Note the last hour rolled up; `since` is the first hour ever rolled
    /// up, kept from the first call on
    async fn set_server_insight_cursor(&self, hour: i64, since: Option<i64>) -> Result<()>;

    /// The first hour crond ever rolled up, if known
    async fn fetch_server_insight_since(&self) -> Result<Option<i64>>;
}
