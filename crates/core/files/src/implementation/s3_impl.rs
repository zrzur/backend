use std::io::Write;

use anyhow::Context;
use aws_sdk_s3::{
    config::{Credentials, Region},
    Client, Config,
};
use revolt_config::FilesS3;

use crate::{EncryptionRepository, FileStorageRepository};

pub struct S3Storage<ER: EncryptionRepository> {
    client: Client,
    encryption: ER,
}

impl<ER: EncryptionRepository> S3Storage<ER> {
    pub async fn from_config(encryption: ER) -> S3Storage<ER> {
        S3Storage::new(encryption, revolt_config::config().await.files.s3)
    }

    pub fn new(encryption: ER, s3_config: FilesS3) -> S3Storage<ER> {
        let provider_name = "my-creds";
        let creds = Credentials::new(
            s3_config.access_key_id,
            s3_config.secret_access_key,
            None,
            None,
            provider_name,
        );

        let config = Config::builder()
            .region(Region::new(s3_config.region))
            .endpoint_url(s3_config.endpoint)
            .force_path_style(s3_config.path_style_buckets)
            .credentials_provider(creds)
            .build();

        S3Storage {
            client: Client::from_conf(config),
            encryption,
        }
    }
}

impl<ER: EncryptionRepository> S3Storage<ER> {
    /// Fetch and decrypt, telling apart a missing object, bytes that don't
    /// decrypt with this key, and a failure to reach the storage
    pub async fn fetch_checked(
        &self,
        bucket_id: &str,
        path: &str,
        iv: &str,
    ) -> Result<Vec<u8>, crate::FetchFailure> {
        use aws_sdk_s3::operation::get_object::GetObjectError;
        let object = match self.client.get_object().bucket(bucket_id).key(path).send().await {
            Ok(object) => object,
            Err(error) => {
                let missing = matches!(
                    error.as_service_error(),
                    Some(GetObjectError::NoSuchKey(_))
                ) || error.raw_response().is_some_and(|raw| raw.status().as_u16() == 404);
                return Err(if missing {
                    crate::FetchFailure::Missing
                } else {
                    crate::FetchFailure::Transport(format!("{error:?}"))
                });
            }
        };
        let bytes = object
            .body
            .collect()
            .await
            .map_err(|error| crate::FetchFailure::Transport(format!("{error:?}")))?
            .into_bytes()
            .to_vec();
        if iv.is_empty() {
            return Ok(bytes);
        }
        self.encryption
            .decrypt_buffer(bytes, iv)
            .map_err(|_| crate::FetchFailure::Undecryptable)
    }

    /// Every object whose key starts with `prefix`
    pub async fn list_prefix(&self, bucket_id: &str, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .client
                .list_objects_v2()
                .bucket(bucket_id)
                .prefix(prefix)
                .set_continuation_token(token.take())
                .send()
                .await
                .with_context(|| format!("failed to list {prefix}* in {bucket_id}"))?;
            keys.extend(page.contents().iter().filter_map(|o| o.key().map(str::to_owned)));
            match page.next_continuation_token() {
                Some(next) if page.is_truncated() == Some(true) => token = Some(next.to_owned()),
                _ => break,
            }
        }
        Ok(keys)
    }
}

#[async_trait::async_trait]
impl<ER: EncryptionRepository> FileStorageRepository for S3Storage<ER> {
    #[tracing::instrument(skip(self), err)]
    async fn create_bucket(&self, bucket_id: &str) -> anyhow::Result<()> {
        self.client
            .create_bucket()
            .bucket(bucket_id)
            .send()
            .await
            .with_context(|| format!("failed to create bucket {bucket_id}"))?;

        Ok(())
    }

    #[tracing::instrument(skip(self, iv), err)]
    async fn fetch_and_decrypt_file(
        &self,
        bucket_id: &str,
        path: &str,
        iv: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut object = self
            .client
            .get_object()
            .bucket(bucket_id)
            .key(path)
            .send()
            .await
            .with_context(|| format!("failed to get object at {path} in {bucket_id}"))?;

        let mut buf = vec![];
        while let Some(bytes) = object.body.next().await {
            let data = bytes?;
            buf.write_all(&data)?;
        }

        if iv.is_empty() {
            Ok(buf)
        } else {
            self.encryption.decrypt_buffer(buf, iv)
        }
    }

    #[tracing::instrument(skip(self, buf), fields(size = buf.len()), err)]
    async fn encrypt_and_upload_file(
        &self,
        bucket_id: &str,
        path: &str,
        buf: &[u8],
    ) -> anyhow::Result<String> {
        let (buf, iv) = self.encryption.encrypt_buffer(buf)?;

        self.client
            .put_object()
            .bucket(bucket_id)
            .key(path)
            .body(buf.into())
            .send()
            .await
            .with_context(|| format!("failed to put object at {path} in {bucket_id}"))?;

        Ok(iv)
    }

    #[tracing::instrument(skip(self), err)]
    async fn delete_file(&self, bucket_id: &str, path: &str) -> anyhow::Result<()> {
        self.client
            .delete_object()
            .bucket(bucket_id)
            .key(path)
            .send()
            .await
            .with_context(|| format!("failed to delete object at {path} in {bucket_id}"))?;

        Ok(())
    }
}
