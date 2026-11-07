use revolt_result::Result;

use crate::{Account, DataExport, ExportSource};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractAccounts: Sync + Send {
    /// Find account by id
    async fn fetch_account(&self, id: &str) -> Result<Account>;

    /// Find account by normalised email
    async fn fetch_account_by_normalised_email(
        &self,
        normalised_email: &str,
    ) -> Result<Option<Account>>;

    /// Find account with active pending email verification
    async fn fetch_account_with_email_verification(&self, token: &str) -> Result<Account>;

    /// Find account with active password reset
    async fn fetch_account_with_password_reset(&self, token: &str) -> Result<Account>;

    /// Find account with active deletion token
    async fn fetch_account_with_deletion_token(&self, token: &str) -> Result<Account>;

    /// Find accounts which are due to be deleted
    async fn fetch_accounts_due_for_deletion(&self) -> Result<Vec<Account>>;

    // Save account
    async fn save_account(&self, account: &Account) -> Result<()>;

    async fn bump_lockout_count(&self, id: &str) -> Result<Account>;

    /// Whether the person owns a server or a bot or has sent a message
    /// (stops at the first hit)
    async fn user_has_content(&self, user_id: &str) -> Result<bool>;

    /// Remove the account document itself
    async fn remove_account(&self, id: &str) -> Result<()>;

    /// Unset what a plain save leaves behind (age, MFA secrets and recovery
    /// codes, reset token) and blank the password hash
    async fn scrub_account(&self, id: &str) -> Result<()>;

    /// Unset what only the deleted person's own record held: language,
    /// privacy settings, Feather state and badges on the user document
    async fn scrub_user_fields(&self, user_id: &str) -> Result<()>;

    /// The data export request of a person
    async fn fetch_data_export(&self, user_id: &str) -> Result<Option<DataExport>>;

    /// Requests waiting for crond
    async fn fetch_pending_data_exports(&self) -> Result<Vec<DataExport>>;

    /// Exports whose link has run out
    async fn fetch_expired_data_exports(&self) -> Result<Vec<DataExport>>;

    /// Save (insert or replace) a data export request
    async fn save_data_export(&self, export: &DataExport) -> Result<()>;

    /// Store the ZIP in pieces, returning how many
    async fn save_data_export_bytes(&self, user_id: &str, bytes: &[u8]) -> Result<i32>;

    /// Read the ZIP back
    async fn fetch_data_export_bytes(&self, user_id: &str) -> Result<Vec<u8>>;

    /// Remove the request and its file
    async fn remove_data_exports_for_user(&self, user_id: &str) -> Result<()>;

    /// Records that belong to a person, for their data export
    async fn export_documents(
        &self,
        user_id: &str,
        source: ExportSource,
    ) -> Result<Vec<serde_json::Value>>;
}
