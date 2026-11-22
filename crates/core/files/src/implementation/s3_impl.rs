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
        use aws_sdk_s3::{error::ProvideErrorMetadata, operation::get_object::GetObjectError};
        let object = match self.client.get_object().bucket(bucket_id).key(path).send().await {
            Ok(object) => object,
            Err(error) => {
                let service = error.as_service_error();
                return Err(classify_get_failure(
                    matches!(service, Some(GetObjectError::NoSuchKey(_))),
                    service.and_then(|e| e.code()),
                    format!("{error:?}"),
                ));
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
        self.list_prefix_paged(bucket_id, prefix, 1000).await
    }

    /// The same, `page_size` keys per request
    pub async fn list_prefix_paged(
        &self,
        bucket_id: &str,
        prefix: &str,
        page_size: i32,
    ) -> anyhow::Result<Vec<String>> {
        collect_pages(|token| async move {
            let page = self
                .client
                .list_objects_v2()
                .bucket(bucket_id)
                .prefix(prefix)
                .max_keys(page_size)
                .set_continuation_token(token)
                .send()
                .await
                .with_context(|| format!("failed to list {prefix}* in {bucket_id}"))?;
            let keys = page
                .contents()
                .iter()
                .filter_map(|o| o.key().map(str::to_owned))
                .collect();
            let next = match page.next_continuation_token() {
                Some(next) if page.is_truncated() == Some(true) => Some(next.to_owned()),
                _ => None,
            };
            Ok((keys, next))
        })
        .await
    }
}

/// Only "no such key" means the object is missing. Any other failure, a 404
/// for a missing bucket or from a proxy included, is the storage's problem.
pub fn classify_get_failure(
    no_such_key: bool,
    code: Option<&str>,
    detail: String,
) -> crate::FetchFailure {
    if no_such_key || code == Some("NoSuchKey") {
        crate::FetchFailure::Missing
    } else {
        crate::FetchFailure::Transport(detail)
    }
}

/// Follow continuation tokens until a page says there is no next one
pub async fn collect_pages<F, Fut>(mut page: F) -> anyhow::Result<Vec<String>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<String>, Option<String>)>>,
{
    let mut keys = Vec::new();
    let mut token = None;
    loop {
        let (more, next) = page(token.take()).await?;
        keys.extend(more);
        match next {
            Some(next) => token = Some(next),
            None => return Ok(keys),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EncryptionKey, FetchFailure};

    #[test]
    fn only_no_such_key_is_missing() {
        assert_eq!(classify_get_failure(true, None, "x".into()), FetchFailure::Missing);
        assert_eq!(
            classify_get_failure(false, Some("NoSuchKey"), "x".into()),
            FetchFailure::Missing
        );
        // a missing bucket, a refusal, a proxy's 404: not the object's absence
        for code in [Some("NoSuchBucket"), Some("AccessDenied"), Some("NotFound"), None] {
            assert_eq!(
                classify_get_failure(false, code, "detail".into()),
                FetchFailure::Transport("detail".into())
            );
        }
    }

    #[tokio::test]
    async fn pages_are_followed_to_the_last_one() {
        let pages = [
            (vec!["a.1", "a.2"], Some("t1")),
            (vec!["a.3", "a.4"], Some("t2")),
            (vec!["a.5"], None),
        ];
        let mut asked = Vec::new();
        let keys = collect_pages(|token| {
            asked.push(token.clone());
            let index = match token.as_deref() {
                None => 0,
                Some("t1") => 1,
                Some("t2") => 2,
                Some(other) => panic!("unknown token {other}"),
            };
            let (keys, next) = &pages[index];
            let page = (
                keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
                next.map(str::to_owned),
            );
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(keys, vec!["a.1", "a.2", "a.3", "a.4", "a.5"]);
        assert_eq!(asked, vec![None, Some("t1".into()), Some("t2".into())]);
    }

    #[tokio::test]
    async fn a_failed_page_fails_the_list() {
        let result = collect_pages(|token| async move {
            match token {
                None => Ok((vec!["a.1".to_owned()], Some("t1".to_owned()))),
                Some(_) => Err(anyhow::anyhow!("storage gone")),
            }
        })
        .await;
        assert!(result.is_err());
    }

    /// Against the MinIO of the backend's compose.yml (127.0.0.1:14009,
    /// its public example credentials), when it is reachable
    fn minio() -> Option<S3Storage<EncryptionKey>> {
        if std::net::TcpStream::connect_timeout(
            &"127.0.0.1:14009".parse().unwrap(),
            std::time::Duration::from_millis(300),
        )
        .is_err()
        {
            eprintln!("SKIPPED: no MinIO on 127.0.0.1:14009 (backend compose.yml)");
            return None;
        }
        let key = EncryptionKey::new("XkbJ8gBzrouQ+15Ri23xCC81+aZE26Z6+gXzglFxOD4=".to_string());
        Some(S3Storage::new(
            key,
            FilesS3 {
                endpoint: "http://127.0.0.1:14009".into(),
                path_style_buckets: true,
                region: "minio".into(),
                access_key_id: "minioautumn".into(),
                secret_access_key: "minioautumn".into(),
                default_bucket: "revolt-uploads".into(),
            },
        ))
    }

    #[tokio::test]
    async fn minio_classifies_and_pages() {
        let Some(storage) = minio() else { return };
        let bucket = "revolt-uploads";
        let prefix = format!("selftest-{}.m4a.", std::process::id());
        let iv = storage
            .encrypt_and_upload_file(bucket, &format!("{prefix}0"), b"copy")
            .await
            .unwrap();
        for n in 1..5 {
            storage
                .encrypt_and_upload_file(bucket, &format!("{prefix}{n}"), b"copy")
                .await
                .unwrap();
        }
        assert_eq!(storage.fetch_checked(bucket, &format!("{prefix}0"), &iv).await.unwrap(), b"copy");
        assert_eq!(
            storage.fetch_checked(bucket, &format!("{prefix}none"), &iv).await,
            Err(FetchFailure::Missing)
        );
        let other_iv = storage
            .encrypt_and_upload_file(bucket, &format!("{prefix}x"), b"other")
            .await
            .unwrap();
        assert_ne!(other_iv, iv);
        assert_eq!(
            storage.fetch_checked(bucket, &format!("{prefix}0"), &other_iv).await,
            Err(FetchFailure::Undecryptable)
        );
        // a missing bucket is a 404 too, and not a missing object
        assert!(matches!(
            storage.fetch_checked("no-such-bucket-zrzur", "k", &iv).await,
            Err(FetchFailure::Transport(_))
        ));
        let mut listed = storage.list_prefix_paged(bucket, &prefix, 2).await.unwrap();
        listed.sort();
        assert_eq!(listed.len(), 6);
        for key in listed {
            storage.delete_file(bucket, &key).await.unwrap();
        }
        assert!(storage.list_prefix(bucket, &prefix).await.unwrap().is_empty());
    }
}
