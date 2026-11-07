use crate::{
    AbstractAccounts, Account, DataExport, DataExportStatus, DeletionInfo, EmailVerification,
    ExportSource, Lockout, ReferenceDb, DATA_EXPORT_CHUNK_BYTES,
};
use iso8601_timestamp::{Duration, Timestamp};
use revolt_result::Result;

#[async_trait]
impl AbstractAccounts for ReferenceDb {
    /// Find account by id
    async fn fetch_account(&self, id: &str) -> Result<Account> {
        let accounts = self.accounts.lock().await;
        accounts
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(UnknownUser))
    }

    /// Find account by normalised email
    async fn fetch_account_by_normalised_email(
        &self,
        normalised_email: &str,
    ) -> Result<Option<Account>> {
        let accounts = self.accounts.lock().await;
        Ok(accounts
            .values()
            .find(|account| account.email_normalised == normalised_email)
            .cloned())
    }

    /// Find account with active pending email verification
    async fn fetch_account_with_email_verification(&self, token_to_match: &str) -> Result<Account> {
        let accounts = self.accounts.lock().await;
        accounts
            .values()
            .find(|account| match &account.verification {
                EmailVerification::Pending { token, .. }
                | EmailVerification::Moving { token, .. } => token == token_to_match,
                _ => false,
            })
            .cloned()
            .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find account with active password reset
    async fn fetch_account_with_password_reset(&self, token: &str) -> Result<Account> {
        let accounts = self.accounts.lock().await;
        accounts
            .values()
            .find(|account| {
                if let Some(reset) = &account.password_reset {
                    reset.token == token
                } else {
                    false
                }
            })
            .cloned()
            .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find account with active deletion token
    async fn fetch_account_with_deletion_token(&self, token_to_match: &str) -> Result<Account> {
        let accounts = self.accounts.lock().await;
        accounts
            .values()
            .find(|account| {
                if let Some(DeletionInfo::WaitingForVerification { token, .. }) = &account.deletion
                {
                    token == token_to_match
                } else {
                    false
                }
            })
            .cloned()
            .ok_or_else(|| create_error!(InvalidToken))
    }

    /// Find accounts which are due to be deleted
    async fn fetch_accounts_due_for_deletion(&self) -> Result<Vec<Account>> {
        let now = Timestamp::now_utc();
        let accounts = self.accounts.lock().await;

        Ok(accounts
            .values()
            .filter(|account| {
                if let Some(DeletionInfo::Scheduled { after }) = &account.deletion {
                    after <= &now
                } else {
                    false
                }
            })
            .cloned()
            .collect())
    }

    // Save account
    async fn save_account(&self, account: &Account) -> Result<()> {
        let mut accounts = self.accounts.lock().await;
        accounts.insert(account.id.to_string(), account.clone());
        Ok(())
    }

    async fn user_has_content(&self, user_id: &str) -> Result<bool> {
        Ok(self.servers.lock().await.values().any(|s| s.owner == user_id)
            || self.bots.lock().await.values().any(|b| b.owner == user_id)
            || self.messages.lock().await.values().any(|m| m.author == user_id))
    }

    async fn remove_account(&self, id: &str) -> Result<()> {
        self.accounts.lock().await.remove(id);
        Ok(())
    }

    async fn scrub_account(&self, id: &str) -> Result<()> {
        if let Some(account) = self.accounts.lock().await.get_mut(id) {
            account.password = String::new();
            account.mfa = Default::default();
            account.age = None;
            account.password_reset = None;
            account.lockout = None;
        }
        Ok(())
    }

    async fn fetch_data_export(&self, user_id: &str) -> Result<Option<DataExport>> {
        Ok(self.data_exports.lock().await.get(user_id).cloned())
    }

    async fn scrub_user_fields(&self, user_id: &str) -> Result<()> {
        if let Some(user) = self.users.lock().await.get_mut(user_id) {
            user.locale = None;
            user.privacy = None;
            user.profile = None;
            user.plus = None;
            user.badges = None;
        }
        Ok(())
    }

    async fn fetch_pending_data_exports(&self) -> Result<Vec<DataExport>> {
        Ok(self.data_exports
            .lock()
            .await
            .values()
            .filter(|e| e.status == DataExportStatus::Pending)
            .cloned()
            .collect())
    }

    async fn fetch_expired_data_exports(&self) -> Result<Vec<DataExport>> {
        let now = Timestamp::now_utc();
        Ok(self.data_exports
            .lock()
            .await
            .values()
            .filter(|e| {
                e.status == DataExportStatus::Ready && e.expires_at.is_some_and(|at| at <= now)
            })
            .cloned()
            .collect())
    }

    async fn save_data_export(&self, export: &DataExport) -> Result<()> {
        self.data_exports
            .lock()
            .await
            .insert(export.id.clone(), export.clone());
        Ok(())
    }

    async fn save_data_export_bytes(&self, user_id: &str, bytes: &[u8]) -> Result<i32> {
        self.data_export_bytes
            .lock()
            .await
            .insert(user_id.to_string(), bytes.to_vec());
        Ok(bytes.chunks(DATA_EXPORT_CHUNK_BYTES).count() as i32)
    }

    async fn fetch_data_export_bytes(&self, user_id: &str) -> Result<Vec<u8>> {
        Ok(self.data_export_bytes
            .lock()
            .await
            .get(user_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn remove_data_exports_for_user(&self, user_id: &str) -> Result<()> {
        self.data_exports.lock().await.remove(user_id);
        self.data_export_bytes.lock().await.remove(user_id);
        Ok(())
    }

    async fn export_documents(
        &self,
        user_id: &str,
        source: ExportSource,
    ) -> Result<Vec<serde_json::Value>> {
        Ok(match source {
            ExportSource::Messages => self
                .messages
                .lock()
                .await
                .values()
                .filter(|m| m.author == user_id)
                .filter_map(|m| serde_json::to_value(m).ok())
                .collect(),
            ExportSource::Files => self
                .files
                .lock()
                .await
                .values()
                .filter(|f| f.uploader_id.as_deref() == Some(user_id) && f.deleted != Some(true))
                .filter_map(|f| serde_json::to_value(f).ok())
                .collect(),
            ExportSource::Memberships => self
                .server_members
                .lock()
                .await
                .values()
                .filter(|m| m.id.user == user_id)
                .filter_map(|m| serde_json::to_value(m).ok())
                .collect(),
            ExportSource::Reports => self
                .safety_reports
                .lock()
                .await
                .values()
                .filter(|r| r.author_id == user_id)
                .filter_map(|r| serde_json::to_value(r).ok())
                .collect(),
            ExportSource::SupportTickets => self
                .support_tickets
                .lock()
                .await
                .values()
                .filter(|t| t.user_id == user_id)
                .filter_map(|t| serde_json::to_value(t).ok())
                .collect(),
            ExportSource::Settings => self
                .user_settings
                .lock()
                .await
                .get(user_id)
                .and_then(|s| serde_json::to_value(s).ok())
                .into_iter()
                .collect(),
        })
    }

    async fn bump_lockout_count(&self, id: &str) -> Result<Account> {
        let mut accounts = self.accounts.lock().await;
        let account = accounts
            .get_mut(id)
            .ok_or_else(|| create_error!(UnknownUser))?;

        account.lockout = Some(Lockout::after_failure(
            account.lockout.as_ref(),
            Timestamp::now_utc(),
        ));

        Ok(account.clone())
    }
}
