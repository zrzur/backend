use revolt_result::Result;

use crate::{AppInstall, OAuthClient, OAuthCode, OAuthGrant, OAuthToken};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractOAuth: Sync + Send {
    async fn fetch_oauth_client(&self, application_id: &str) -> Result<Option<OAuthClient>>;
    async fn save_oauth_client(&self, client: &OAuthClient) -> Result<()>;

    async fn insert_oauth_code(&self, code: &OAuthCode) -> Result<()>;
    /// Fetch and delete in one step, so a code works once
    async fn take_oauth_code(&self, code_hash: &str) -> Result<Option<OAuthCode>>;

    async fn insert_oauth_token(&self, token: &OAuthToken) -> Result<()>;
    async fn fetch_oauth_token_by_access(&self, access_hash: &str) -> Result<Option<OAuthToken>>;
    async fn fetch_oauth_token_by_refresh(&self, refresh_hash: &str) -> Result<Option<OAuthToken>>;
    /// Revoke one pair; `false` if it was already revoked (lost a race)
    async fn revoke_oauth_token(&self, id: &str) -> Result<bool>;
    async fn revoke_oauth_token_family(&self, family: &str) -> Result<()>;
    async fn revoke_oauth_tokens(&self, user_id: &str, application_id: &str) -> Result<()>;

    async fn save_oauth_grant(&self, grant: &OAuthGrant) -> Result<()>;
    async fn fetch_oauth_grants(&self, user_id: &str) -> Result<Vec<OAuthGrant>>;
    async fn delete_oauth_grant(&self, user_id: &str, application_id: &str) -> Result<bool>;

    async fn save_app_install(&self, install: &AppInstall) -> Result<()>;
    async fn fetch_app_installs(&self, server_id: &str) -> Result<Vec<AppInstall>>;
    async fn delete_app_install(&self, server_id: &str, application_id: &str) -> Result<()>;
    /// When an application is deleted: its installs, grants, tokens and settings
    async fn delete_oauth_application(&self, application_id: &str) -> Result<()>;
}
