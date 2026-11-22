//! Making, storing, serving and forgetting the AAC copy of a voice message.
//!
//! The rules (see voice.rs for why there is a copy at all):
//! - nothing is done for a file that may not have one ([`may_have_copy`]) or
//!   that failed before (`refused`, one entry per original hash);
//! - one attempt per file at a time (`making`): the attempt runs as a task of
//!   its own, so a request that goes away part-way never leaves it half done;
//!   it ends stored, or refused and remembered;
//! - the bytes of each attempt go to a path of their own, and the record is
//!   written once, complete (path and key together): two servers racing keep
//!   one record whose key belongs to its bytes;
//! - a copy record's `processed_hash` is its own id (`<hash>.m4a`), which no
//!   upload's hash can ever be;
//! - a record that can't be read (no key, missing bytes, bytes that don't
//!   decrypt) is forgotten and made again once. Forgetting removes the record
//!   only while it still points at the same path, and touches the bytes only
//!   when the record was really removed. A storage that can't be reached is
//!   no reason to forget anything.

use std::{future::Future, sync::Arc};

use revolt_database::{Database, FileHash, Metadata, iso8601_timestamp::Timestamp};
use revolt_files::{AUTHENTICATION_TAG_SIZE_BYTES, FetchFailure, voice_rendition_id};
use sha2::Digest;

use crate::voice::{Made, RENDITION_CONTENT_TYPE, VoiceRefusal, may_have_copy};

/// Where the bytes live
pub trait Store: Send + Sync + 'static {
    fn read(
        &self,
        bucket: &str,
        path: &str,
        iv: &str,
    ) -> impl Future<Output = Result<Vec<u8>, FetchFailure>> + Send;
    /// Store encrypted; the key's nonce
    fn write(
        &self,
        bucket: &str,
        path: &str,
        data: &[u8],
    ) -> impl Future<Output = Result<String, String>> + Send;
    fn delete(&self, bucket: &str, path: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// What turns a recording into its copy (ffmpeg in voice.rs)
pub trait Convert: Send + Sync + 'static {
    fn make(
        &self,
        bytes: &[u8],
        content_type: &str,
    ) -> impl Future<Output = Result<Made, VoiceRefusal>> + Send;
}

/// Everything the copies need; cheap to clone (shared caches)
pub struct Copies<S, C> {
    pub db: Database,
    pub store: Arc<S>,
    pub convert: Arc<C>,
    /// Copies being made or just made, per original hash
    pub making: moka::future::Cache<String, FileHash>,
    /// Why a file has no copy, per original hash
    pub refused: moka::future::Cache<String, VoiceRefusal>,
}

impl<S, C> Clone for Copies<S, C> {
    fn clone(&self) -> Self {
        Copies {
            db: self.db.clone(),
            store: self.store.clone(),
            convert: self.convert.clone(),
            making: self.making.clone(),
            refused: self.refused.clone(),
        }
    }
}

impl<S: Store, C: Convert> Copies<S, C> {
    /// The record to serve as the copy of `hash`, made when there is none
    pub async fn copy(&self, hash: &FileHash) -> Result<FileHash, VoiceRefusal> {
        may_have_copy(&hash.content_type, hash.size)?;
        if let Some(refusal) = self.refused.get(&hash.id).await {
            return Err(refusal);
        }
        if let Ok(stored) = self.db.fetch_attachment_hash(&voice_rendition_id(&hash.id)).await {
            if !stored.iv.is_empty() {
                return Ok(stored);
            }
            // a record without a key can never be read: made again
            self.forget(hash, &stored).await;
        }
        let this = self.clone();
        let owned = hash.clone();
        let attempt = async move {
            // its own task: a caller that goes away does not stop it
            tokio::spawn(async move { this.make_once(owned).await })
                .await
                .unwrap_or(Err(VoiceRefusal::Unavailable))
        };
        self.making
            .try_get_with(hash.id.clone(), attempt)
            .await
            .map_err(|refusal| *refusal)
    }

    /// The copy and its bytes; a copy that can't be read is made again once
    pub async fn served(&self, hash: &FileHash) -> Result<(FileHash, Vec<u8>), VoiceRefusal> {
        let copy = self.copy(hash).await?;
        match self.store.read(&copy.bucket_id, &copy.path, &copy.iv).await {
            Ok(data) => return Ok((copy, data)),
            Err(FetchFailure::Transport(error)) => {
                tracing::warn!("AAC copy {} can't be reached now: {error}", copy.id);
                return Err(VoiceRefusal::Unavailable);
            }
            Err(failure) => {
                tracing::warn!("AAC copy {} is broken ({failure:?}): made again", copy.id);
                self.forget(hash, &copy).await;
            }
        }
        let copy = self.copy(hash).await?;
        let data = self
            .store
            .read(&copy.bucket_id, &copy.path, &copy.iv)
            .await
            .map_err(|failure| {
                tracing::error!("AAC copy {} broken again: {failure:?}", copy.id);
                VoiceRefusal::Unavailable
            })?;
        Ok((copy, data))
    }

    /// Take a broken copy away: the record only while it still points at the
    /// same bytes, and those bytes only when the record really went (and they
    /// are not the original's)
    pub async fn forget(&self, hash: &FileHash, copy: &FileHash) {
        self.making.invalidate(&hash.id).await;
        let removed = match self.db.delete_attachment_hash_at(&copy.id, &copy.path).await {
            Ok(removed) => removed,
            Err(error) => {
                tracing::warn!("AAC copy record {} not removed: {error:?}", copy.id);
                false
            }
        };
        if removed && copy.path != hash.path {
            if let Err(error) = self.store.delete(&copy.bucket_id, &copy.path).await {
                tracing::warn!("broken AAC copy {} stays in storage: {error}", copy.path);
            }
        }
    }

    /// One attempt, run to its end: stored, or refused and remembered
    async fn make_once(&self, hash: FileHash) -> Result<FileHash, VoiceRefusal> {
        let result = self.make_inner(&hash).await;
        if let Err(refusal) = result {
            self.refused.insert(hash.id.clone(), refusal).await;
        }
        result
    }

    async fn make_inner(&self, hash: &FileHash) -> Result<FileHash, VoiceRefusal> {
        let id = voice_rendition_id(&hash.id);
        let original = self
            .store
            .read(&hash.bucket_id, &hash.path, &hash.iv)
            .await
            .map_err(|failure| match failure {
                FetchFailure::Transport(_) => VoiceRefusal::Unavailable,
                FetchFailure::Missing | FetchFailure::Undecryptable => VoiceRefusal::Unreadable,
            })?;

        let copy = match self.convert.make(&original, &hash.content_type).await? {
            // AAC already: the record points at the original's own bytes
            Made::Original => FileHash {
                id: id.clone(),
                processed_hash: id.clone(),
                created_at: Timestamp::now_utc(),
                bucket_id: hash.bucket_id.clone(),
                path: hash.path.clone(),
                iv: hash.iv.clone(),
                metadata: Metadata::Audio { duration_ms: None },
                content_type: hash.content_type.clone(),
                size: hash.size,
            },
            Made::Copy(data) => {
                // only to log what was made
                let digest = format!("{:02x}", sha2::Sha256::digest(&data));
                let path = format!("{id}.{}", ulid::Ulid::new());
                let nonce = self
                    .store
                    .write(&hash.bucket_id, &path, &data)
                    .await
                    .map_err(|error| {
                        tracing::error!("AAC copy of {} not stored: {error}", hash.id);
                        VoiceRefusal::Unavailable
                    })?;
                tracing::info!("AAC copy {path} of {}: {} bytes, sha256 {digest}", hash.id, data.len());
                FileHash {
                    id: id.clone(),
                    processed_hash: id.clone(),
                    created_at: Timestamp::now_utc(),
                    bucket_id: hash.bucket_id.clone(),
                    path,
                    iv: nonce,
                    metadata: Metadata::Audio { duration_ms: None },
                    content_type: RENDITION_CONTENT_TYPE.to_owned(),
                    size: (data.len() + AUTHENTICATION_TAG_SIZE_BYTES) as isize,
                }
            }
        };

        if self.db.insert_attachment_hash(&copy).await.is_ok() {
            return Ok(copy);
        }
        // another server was first: keep its copy, drop ours
        if copy.path != hash.path {
            if let Err(error) = self.store.delete(&copy.bucket_id, &copy.path).await {
                tracing::warn!("spare AAC copy {} stays in storage: {error}", copy.path);
            }
        }
        match self.db.fetch_attachment_hash(&id).await {
            Ok(winner) if !winner.iv.is_empty() => Ok(winner),
            _ => Err(VoiceRefusal::Busy),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_database::DatabaseInfo;
    use std::{
        collections::HashMap,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    /// Bytes in memory; the "key" of an object is its nonce
    #[derive(Default)]
    struct Memory {
        objects: Mutex<HashMap<String, (Vec<u8>, String)>>,
        writes: AtomicUsize,
        /// paths that answer as unreachable
        unreachable: Mutex<Vec<String>>,
        /// how long a write takes
        slow_write: Mutex<Duration>,
    }

    impl Memory {
        fn put(&self, path: &str, data: &[u8], nonce: &str) {
            self.objects
                .lock()
                .unwrap()
                .insert(path.to_owned(), (data.to_vec(), nonce.to_owned()));
        }
        fn paths(&self) -> Vec<String> {
            let mut paths: Vec<String> = self.objects.lock().unwrap().keys().cloned().collect();
            paths.sort();
            paths
        }
    }

    impl Store for Memory {
        async fn read(&self, _: &str, path: &str, iv: &str) -> Result<Vec<u8>, FetchFailure> {
            if self.unreachable.lock().unwrap().iter().any(|p| p == path) {
                return Err(FetchFailure::Transport("down".into()));
            }
            match self.objects.lock().unwrap().get(path) {
                None => Err(FetchFailure::Missing),
                Some((_, nonce)) if nonce != iv => Err(FetchFailure::Undecryptable),
                Some((data, _)) => Ok(data.clone()),
            }
        }
        async fn write(&self, _: &str, path: &str, data: &[u8]) -> Result<String, String> {
            let delay = *self.slow_write.lock().unwrap();
            tokio::time::sleep(delay).await;
            let n = self.writes.fetch_add(1, Ordering::SeqCst);
            let nonce = format!("nonce-{n}");
            self.put(path, data, &nonce);
            Ok(nonce)
        }
        async fn delete(&self, _: &str, path: &str) -> Result<(), String> {
            self.objects.lock().unwrap().remove(path);
            Ok(())
        }
    }

    /// A converter that counts its calls; `answer` decides what it returns
    struct Fake {
        calls: AtomicUsize,
        answer: Mutex<Result<Made, VoiceRefusal>>,
        /// stored during the conversion: another server's copy record
        during: Mutex<Option<(Database, FileHash)>>,
        slow: Duration,
    }

    impl Fake {
        fn copying() -> Fake {
            Fake {
                calls: AtomicUsize::new(0),
                answer: Mutex::new(Ok(Made::Copy(b"aac bytes".to_vec()))),
                during: Mutex::new(None),
                slow: Duration::from_millis(20),
            }
        }
    }

    impl Convert for Fake {
        async fn make(&self, _: &[u8], _: &str) -> Result<Made, VoiceRefusal> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.slow).await;
            let during = self.during.lock().unwrap().take();
            if let Some((db, record)) = during {
                db.insert_attachment_hash(&record).await.unwrap();
            }
            match &*self.answer.lock().unwrap() {
                Ok(Made::Copy(data)) => Ok(Made::Copy(data.clone())),
                Ok(Made::Original) => Ok(Made::Original),
                Err(refusal) => Err(*refusal),
            }
        }
    }

    fn original() -> FileHash {
        FileHash {
            id: "h1".into(),
            processed_hash: "p1".into(),
            created_at: Timestamp::now_utc(),
            bucket_id: "bucket".into(),
            path: "h1".into(),
            iv: "nonce-original".into(),
            metadata: Metadata::Audio { duration_ms: None },
            content_type: "audio/opus".into(),
            size: 1000,
        }
    }

    async fn setup(fake: Fake) -> (Copies<Memory, Fake>, FileHash) {
        let db = DatabaseInfo::Reference.connect().await.unwrap();
        let hash = original();
        db.insert_attachment_hash(&hash).await.unwrap();
        let store = Memory::default();
        store.put("h1", b"opus bytes", "nonce-original");
        let copies = Copies {
            db,
            store: Arc::new(store),
            convert: Arc::new(fake),
            making: moka::future::Cache::new(100),
            refused: moka::future::Cache::new(100),
        };
        (copies, hash)
    }

    #[tokio::test]
    async fn one_attempt_stores_one_record_and_one_object() {
        let (copies, hash) = setup(Fake::copying()).await;
        let copy = copies.copy(&hash).await.unwrap();
        assert_eq!(copy.id, "h1.m4a");
        assert_eq!(copy.processed_hash, "h1.m4a");
        assert!(copy.path.starts_with("h1.m4a."));
        assert_eq!(copies.store.paths(), vec!["h1".to_owned(), copy.path.clone()]);
        let stored = copies.db.fetch_attachment_hash("h1.m4a").await.unwrap();
        assert_eq!((stored.path, stored.iv), (copy.path, copy.iv));
    }

    #[tokio::test]
    async fn a_caller_that_goes_away_leaves_one_finished_copy() {
        let (copies, hash) = setup(Fake::copying()).await;
        *copies.store.slow_write.lock().unwrap() = Duration::from_millis(100);
        // the request goes away while the copy is being stored
        let gone = tokio::time::timeout(Duration::from_millis(40), copies.copy(&hash)).await;
        assert!(gone.is_err());
        tokio::time::sleep(Duration::from_millis(300)).await;

        let stored = copies.db.fetch_attachment_hash("h1.m4a").await.unwrap();
        assert!(!stored.iv.is_empty());
        let copies_stored: Vec<_> =
            copies.store.paths().into_iter().filter(|p| p.starts_with("h1.m4a.")).collect();
        assert_eq!(copies_stored, vec![stored.path]);
        assert_eq!(copies.store.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn two_requests_at_once_convert_once() {
        let (copies, hash) = setup(Fake::copying()).await;
        let (a, b) = tokio::join!(copies.copy(&hash), copies.copy(&hash));
        assert_eq!(a.unwrap().path, b.unwrap().path);
        assert_eq!(copies.convert.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_lost_insert_keeps_the_winner_and_drops_our_bytes() {
        let (copies, hash) = setup(Fake::copying()).await;
        let db = copies.db.clone();
        let winner = FileHash {
            id: "h1.m4a".into(),
            path: "h1.m4a.WINNER".into(),
            iv: "nonce-winner".into(),
            processed_hash: "h1.m4a".into(),
            ..original()
        };
        // another server stores its copy while ours is converting
        *copies.convert.during.lock().unwrap() = Some((db, winner.clone()));
        let copy = copies.copy(&hash).await.unwrap();
        assert_eq!(copy.path, "h1.m4a.WINNER");
        // ours was written, then deleted
        assert_eq!(copies.store.writes.load(Ordering::SeqCst), 1);
        assert_eq!(copies.store.paths(), vec!["h1".to_owned()]);
    }

    #[tokio::test]
    async fn an_unreadable_file_is_remembered_and_not_converted_again() {
        let fake = Fake::copying();
        *fake.answer.lock().unwrap() = Err(VoiceRefusal::Unreadable);
        let (copies, hash) = setup(fake).await;
        assert_eq!(copies.copy(&hash).await, Err(VoiceRefusal::Unreadable));
        copies.making.invalidate(&hash.id).await;
        assert_eq!(copies.copy(&hash).await, Err(VoiceRefusal::Unreadable));
        assert_eq!(copies.convert.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_aac_original_points_at_its_own_bytes() {
        let fake = Fake::copying();
        *fake.answer.lock().unwrap() = Ok(Made::Original);
        let (copies, hash) = setup(fake).await;
        let copy = copies.copy(&hash).await.unwrap();
        assert_eq!((copy.path.as_str(), copy.iv.as_str()), ("h1", "nonce-original"));
        assert_eq!(copy.processed_hash, "h1.m4a");
        assert_eq!(copies.store.writes.load(Ordering::SeqCst), 0);
        // forgetting it never deletes the original's bytes
        copies.forget(&hash, &copy).await;
        assert_eq!(copies.store.paths(), vec!["h1".to_owned()]);
    }

    #[tokio::test]
    async fn a_record_without_a_key_is_made_again() {
        let (copies, hash) = setup(Fake::copying()).await;
        let broken = FileHash {
            id: "h1.m4a".into(),
            path: "h1.m4a.OLD".into(),
            iv: String::new(),
            processed_hash: "h1.m4a".into(),
            ..original()
        };
        copies.db.insert_attachment_hash(&broken).await.unwrap();
        copies.store.put("h1.m4a.OLD", b"half", "");
        let copy = copies.copy(&hash).await.unwrap();
        assert_ne!(copy.path, "h1.m4a.OLD");
        assert!(!copy.iv.is_empty());
        assert!(!copies.store.paths().contains(&"h1.m4a.OLD".to_owned()));
    }

    #[tokio::test]
    async fn forgetting_is_conditional_on_the_path() {
        let (copies, hash) = setup(Fake::copying()).await;
        let copy = copies.copy(&hash).await.unwrap();
        // meanwhile another server replaced the record with its own copy
        let newer = FileHash { path: "h1.m4a.NEWER".into(), ..copy.clone() };
        copies.db.delete_attachment_hash("h1.m4a").await.unwrap();
        copies.db.insert_attachment_hash(&newer).await.unwrap();
        copies.forget(&hash, &copy).await;
        // the newer record stays, and our old bytes were not touched either
        assert_eq!(copies.db.fetch_attachment_hash("h1.m4a").await.unwrap().path, "h1.m4a.NEWER");
        assert!(copies.store.paths().contains(&copy.path));
    }

    #[tokio::test]
    async fn served_makes_a_broken_copy_again_but_never_on_a_storage_outage() {
        let (copies, hash) = setup(Fake::copying()).await;
        let first = copies.copy(&hash).await.unwrap();

        // the storage can't be reached: 503, and the copy is kept
        copies.store.unreachable.lock().unwrap().push(first.path.clone());
        assert_eq!(copies.served(&hash).await.err(), Some(VoiceRefusal::Unavailable));
        assert_eq!(copies.db.fetch_attachment_hash("h1.m4a").await.unwrap().path, first.path);
        copies.store.unreachable.lock().unwrap().clear();

        // the bytes went missing: forgotten and made again
        copies.store.objects.lock().unwrap().remove(&first.path);
        let (second, data) = copies.served(&hash).await.unwrap();
        assert_ne!(second.path, first.path);
        assert_eq!(data, b"aac bytes");
        assert_eq!(copies.convert.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn nothing_is_done_for_a_file_that_may_not_have_a_copy() {
        let (copies, mut hash) = setup(Fake::copying()).await;
        hash.content_type = "image/png".into();
        assert_eq!(copies.copy(&hash).await, Err(VoiceRefusal::NotSound));
        assert_eq!(copies.convert.calls.load(Ordering::SeqCst), 0);
    }
}
