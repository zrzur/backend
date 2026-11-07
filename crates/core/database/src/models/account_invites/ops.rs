use revolt_result::Result;

use crate::AccountInvite;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractAccountInvites: Sync + Send {
    /// Find invite by id
    async fn fetch_account_invite(&self, id: &str) -> Result<AccountInvite>;

    /// Atomically mark an unused invite as used; `InvalidInvite` when missing or already used
    async fn claim_account_invite(&self, id: &str) -> Result<AccountInvite>;

    /// Give a claimed invite back (account creation failed after the claim)
    async fn release_account_invite(&self, id: &str) -> Result<()>;

    /// Save invite
    async fn save_account_invite(&self, invite: &AccountInvite) -> Result<()>;
}
