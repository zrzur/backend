use revolt_result::Result;

use crate::DeskPending;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractDeskPending: Sync + Send {
    /// Store a new pending action
    async fn insert_desk_pending(&self, pending: &DeskPending) -> Result<()>;

    /// A pending action by id (expired ones may still be returned until the
    /// TTL index removes them: check `is_expired`)
    async fn fetch_desk_pending(&self, id: &str) -> Result<Option<DeskPending>>;

    /// Bind the card message once. `true` when it is bound now or was
    /// already bound to this message; `false` when bound to another one.
    async fn bind_desk_pending_card(&self, id: &str, message_id: &str) -> Result<bool>;

    /// Mark as executed; `false` if it already was (or is gone)
    async fn claim_desk_pending(&self, id: &str) -> Result<bool>;

    /// Undo a claim when the action did not run (a step-up was missing)
    async fn release_desk_pending(&self, id: &str) -> Result<()>;
}
