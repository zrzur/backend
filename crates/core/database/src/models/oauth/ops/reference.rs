use revolt_result::Result;

use crate::{AppInstall, OAuthClient, OAuthCode, OAuthGrant, OAuthToken, ReferenceDb};

use super::AbstractOAuth;

#[async_trait]
impl AbstractOAuth for ReferenceDb {
    async fn fetch_oauth_client(&self, application_id: &str) -> Result<Option<OAuthClient>> {
        Ok(self.oauth.lock().await.clients.get(application_id).cloned())
    }

    async fn save_oauth_client(&self, client: &OAuthClient) -> Result<()> {
        self.oauth
            .lock()
            .await
            .clients
            .insert(client.id.clone(), client.clone());
        Ok(())
    }

    async fn insert_oauth_code(&self, code: &OAuthCode) -> Result<()> {
        self.oauth
            .lock()
            .await
            .codes
            .insert(code.code_hash.clone(), code.clone());
        Ok(())
    }

    async fn take_oauth_code(&self, code_hash: &str) -> Result<Option<OAuthCode>> {
        Ok(self.oauth.lock().await.codes.remove(code_hash))
    }

    async fn insert_oauth_token(&self, token: &OAuthToken) -> Result<()> {
        self.oauth
            .lock()
            .await
            .tokens
            .insert(token.id.clone(), token.clone());
        Ok(())
    }

    async fn fetch_oauth_token_by_access(&self, access_hash: &str) -> Result<Option<OAuthToken>> {
        Ok(self
            .oauth
            .lock()
            .await
            .tokens
            .values()
            .find(|t| t.access_hash == access_hash)
            .cloned())
    }

    async fn fetch_oauth_token_by_refresh(&self, refresh_hash: &str) -> Result<Option<OAuthToken>> {
        Ok(self
            .oauth
            .lock()
            .await
            .tokens
            .values()
            .find(|t| t.refresh_hash == refresh_hash)
            .cloned())
    }

    async fn revoke_oauth_token(&self, id: &str) -> Result<bool> {
        let mut oauth = self.oauth.lock().await;
        match oauth.tokens.get_mut(id) {
            Some(token) if !token.revoked => {
                token.revoked = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn revoke_oauth_token_family(&self, family: &str) -> Result<()> {
        for token in self.oauth.lock().await.tokens.values_mut() {
            if token.family == family {
                token.revoked = true;
            }
        }
        Ok(())
    }

    async fn revoke_oauth_tokens(&self, user_id: &str, application_id: &str) -> Result<()> {
        for token in self.oauth.lock().await.tokens.values_mut() {
            if token.user_id == user_id && token.application_id == application_id {
                token.revoked = true;
            }
        }
        Ok(())
    }

    async fn save_oauth_grant(&self, grant: &OAuthGrant) -> Result<()> {
        self.oauth
            .lock()
            .await
            .grants
            .insert(grant.id.clone(), grant.clone());
        Ok(())
    }

    async fn fetch_oauth_grants(&self, user_id: &str) -> Result<Vec<OAuthGrant>> {
        Ok(self
            .oauth
            .lock()
            .await
            .grants
            .values()
            .filter(|g| g.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn delete_oauth_grant(&self, user_id: &str, application_id: &str) -> Result<bool> {
        Ok(self
            .oauth
            .lock()
            .await
            .grants
            .remove(&OAuthGrant::key(user_id, application_id))
            .is_some())
    }

    async fn save_app_install(&self, install: &AppInstall) -> Result<()> {
        self.oauth
            .lock()
            .await
            .installs
            .insert(install.id.clone(), install.clone());
        Ok(())
    }

    async fn fetch_app_installs(&self, server_id: &str) -> Result<Vec<AppInstall>> {
        Ok(self
            .oauth
            .lock()
            .await
            .installs
            .values()
            .filter(|i| i.server_id == server_id)
            .cloned()
            .collect())
    }

    async fn delete_app_install(&self, server_id: &str, application_id: &str) -> Result<()> {
        self.oauth
            .lock()
            .await
            .installs
            .remove(&AppInstall::key(server_id, application_id));
        Ok(())
    }

    async fn delete_oauth_application(&self, application_id: &str) -> Result<()> {
        let mut oauth = self.oauth.lock().await;
        oauth.clients.remove(application_id);
        oauth.codes.retain(|_, c| c.application_id != application_id);
        oauth.tokens.retain(|_, t| t.application_id != application_id);
        oauth.grants.retain(|_, g| g.application_id != application_id);
        oauth.installs.retain(|_, i| i.application_id != application_id);
        Ok(())
    }
}
