use futures::StreamExt;
use mongodb::options::ReplaceOptions;
use revolt_result::Result;

use crate::{AppInstall, MongoDb, OAuthClient, OAuthCode, OAuthGrant, OAuthToken};

use super::AbstractOAuth;

static CLIENTS: &str = "oauth_clients";
static CODES: &str = "oauth_codes";
static TOKENS: &str = "oauth_tokens";
static GRANTS: &str = "oauth_grants";
static INSTALLS: &str = "app_installs";

impl MongoDb {
    async fn upsert<T: serde::Serialize + Send + Sync>(
        &self,
        collection: &'static str,
        id: &str,
        value: &T,
    ) -> Result<()> {
        self.col::<T>(collection)
            .replace_one(doc! { "_id": id }, value)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("replace_one", collection))
    }

    async fn find_all<T>(&self, collection: &'static str, filter: bson::Document) -> Result<Vec<T>>
    where
        T: serde::de::DeserializeOwned + Send + Sync,
    {
        Ok(self
            .col::<T>(collection)
            .find(filter)
            .await
            .map_err(|_| create_database_error!("find", collection))?
            .filter_map(|s| async { s.ok() })
            .collect()
            .await)
    }
}

#[async_trait]
impl AbstractOAuth for MongoDb {
    async fn fetch_oauth_client(&self, application_id: &str) -> Result<Option<OAuthClient>> {
        self.col::<OAuthClient>(CLIENTS)
            .find_one(doc! { "_id": application_id })
            .await
            .map_err(|_| create_database_error!("find_one", CLIENTS))
    }

    async fn save_oauth_client(&self, client: &OAuthClient) -> Result<()> {
        self.upsert(CLIENTS, &client.id, client).await
    }

    async fn insert_oauth_code(&self, code: &OAuthCode) -> Result<()> {
        self.col::<OAuthCode>(CODES)
            .insert_one(code)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", CODES))
    }

    async fn take_oauth_code(&self, code_hash: &str) -> Result<Option<OAuthCode>> {
        self.col::<OAuthCode>(CODES)
            .find_one_and_delete(doc! { "_id": code_hash })
            .await
            .map_err(|_| create_database_error!("find_one_and_delete", CODES))
    }

    async fn insert_oauth_token(&self, token: &OAuthToken) -> Result<()> {
        self.col::<OAuthToken>(TOKENS)
            .insert_one(token)
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("insert_one", TOKENS))
    }

    async fn fetch_oauth_token_by_access(&self, access_hash: &str) -> Result<Option<OAuthToken>> {
        self.col::<OAuthToken>(TOKENS)
            .find_one(doc! { "access_hash": access_hash })
            .await
            .map_err(|_| create_database_error!("find_one", TOKENS))
    }

    async fn fetch_oauth_token_by_refresh(&self, refresh_hash: &str) -> Result<Option<OAuthToken>> {
        self.col::<OAuthToken>(TOKENS)
            .find_one(doc! { "refresh_hash": refresh_hash })
            .await
            .map_err(|_| create_database_error!("find_one", TOKENS))
    }

    async fn revoke_oauth_token(&self, id: &str) -> Result<bool> {
        let result = self
            .col::<OAuthToken>(TOKENS)
            .update_one(
                doc! { "_id": id, "revoked": false },
                doc! { "$set": { "revoked": true } },
            )
            .await
            .map_err(|_| create_database_error!("update_one", TOKENS))?;
        Ok(result.modified_count == 1)
    }

    async fn revoke_oauth_token_family(&self, family: &str) -> Result<()> {
        self.col::<OAuthToken>(TOKENS)
            .update_many(doc! { "family": family }, doc! { "$set": { "revoked": true } })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", TOKENS))
    }

    async fn revoke_oauth_tokens(&self, user_id: &str, application_id: &str) -> Result<()> {
        self.col::<OAuthToken>(TOKENS)
            .update_many(
                doc! { "user_id": user_id, "application_id": application_id },
                doc! { "$set": { "revoked": true } },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_many", TOKENS))
    }

    async fn save_oauth_grant(&self, grant: &OAuthGrant) -> Result<()> {
        self.upsert(GRANTS, &grant.id, grant).await
    }

    async fn fetch_oauth_grants(&self, user_id: &str) -> Result<Vec<OAuthGrant>> {
        self.find_all(GRANTS, doc! { "user_id": user_id }).await
    }

    async fn delete_oauth_grant(&self, user_id: &str, application_id: &str) -> Result<bool> {
        let result = self
            .col::<OAuthGrant>(GRANTS)
            .delete_one(doc! { "_id": OAuthGrant::key(user_id, application_id) })
            .await
            .map_err(|_| create_database_error!("delete_one", GRANTS))?;
        Ok(result.deleted_count == 1)
    }

    async fn save_app_install(&self, install: &AppInstall) -> Result<()> {
        self.upsert(INSTALLS, &install.id, install).await
    }

    async fn fetch_app_installs(&self, server_id: &str) -> Result<Vec<AppInstall>> {
        self.find_all(INSTALLS, doc! { "server_id": server_id }).await
    }

    async fn delete_app_install(&self, server_id: &str, application_id: &str) -> Result<()> {
        self.col::<AppInstall>(INSTALLS)
            .delete_one(doc! { "_id": AppInstall::key(server_id, application_id) })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", INSTALLS))
    }

    async fn delete_oauth_application(&self, application_id: &str) -> Result<()> {
        for collection in [CODES, TOKENS, GRANTS, INSTALLS] {
            self.col::<bson::Document>(collection)
                .delete_many(doc! { "application_id": application_id })
                .await
                .map_err(|_| create_database_error!("delete_many", collection))?;
        }
        self.col::<bson::Document>(CLIENTS)
            .delete_one(doc! { "_id": application_id })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_one", CLIENTS))
    }
}
