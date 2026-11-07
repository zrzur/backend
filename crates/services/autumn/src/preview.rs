//! Previews cost a full image decode, and anyone can ask for one. A finished
//! preview is kept, asking for the same one at once renders it only once, and
//! only a few render at a time.

use std::{future::Future, sync::Arc, time::Duration};

use lazy_static::lazy_static;
use moka::future::Cache;
use revolt_result::{Error, Result, create_error};
use tokio::{sync::Semaphore, time::timeout};

/// Previews being rendered at once
const MAX_RENDERS: usize = 4;
/// How long a request waits for its turn before giving up
const QUEUE_WAIT: Duration = Duration::from_secs(5);
/// Memory kept for finished previews
const CACHE_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_TIME: Duration = Duration::from_secs(60 * 60);

lazy_static! {
    pub static ref PREVIEWS: Previews = Previews::new(MAX_RENDERS, CACHE_BYTES, QUEUE_WAIT);
}

pub struct Previews {
    cache: Cache<String, Arc<Vec<u8>>>,
    gate: Arc<Semaphore>,
    wait: Duration,
}

impl Previews {
    fn new(renders: usize, bytes: u64, wait: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .weigher(|key: &String, data: &Arc<Vec<u8>>| -> u32 {
                    (key.len() + data.len()).try_into().unwrap_or(u32::MAX)
                })
                .max_capacity(bytes)
                .time_to_live(CACHE_TIME)
                .build(),
            gate: Arc::new(Semaphore::new(renders)),
            wait,
        }
    }

    /// The preview stored under `key`, rendered now if there is none. A
    /// failed render is not kept, and a full queue answers an error rather
    /// than piling up more work.
    pub async fn get_or_render<F, Fut>(&self, key: String, render: F) -> Result<Arc<Vec<u8>>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<u8>>>,
    {
        let gate = self.gate.clone();
        let wait = self.wait;

        self.cache
            .try_get_with(key, async move {
                let _permit = timeout(wait, gate.acquire_owned())
                    .await
                    .map_err(|_| create_error!(InternalError))?
                    .map_err(|_| create_error!(InternalError))?;
                render().await.map(Arc::new)
            })
            .await
            .map_err(|error: Arc<Error>| (*error).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MIB: u64 = 1024 * 1024;

    #[tokio::test]
    async fn a_second_request_does_not_render_again() {
        let previews = Previews::new(2, MIB, QUEUE_WAIT);
        let renders = AtomicUsize::new(0);
        for _ in 0..3 {
            let data = previews
                .get_or_render("attachments:hash".to_owned(), || async {
                    renders.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![1, 2, 3])
                })
                .await
                .unwrap();
            assert_eq!(*data, vec![1, 2, 3]);
        }
        assert_eq!(renders.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_same_preview_asked_for_at_once_renders_once() {
        let previews = Arc::new(Previews::new(4, MIB, QUEUE_WAIT));
        let renders = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..20 {
            let (previews, renders) = (previews.clone(), renders.clone());
            tasks.push(tokio::spawn(async move {
                previews
                    .get_or_render("avatars:hash".to_owned(), || async move {
                        renders.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        Ok(vec![9])
                    })
                    .await
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        assert_eq!(renders.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn only_a_few_render_at_once() {
        let previews = Arc::new(Previews::new(2, MIB, Duration::from_secs(30)));
        let running = Arc::new(AtomicUsize::new(0));
        let highest = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for n in 0..10 {
            let (previews, running, highest) = (previews.clone(), running.clone(), highest.clone());
            tasks.push(tokio::spawn(async move {
                previews
                    .get_or_render(format!("attachments:{n}"), || async move {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        highest.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(vec![0])
                    })
                    .await
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        assert_eq!(highest.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_full_queue_refuses_instead_of_waiting_forever() {
        let previews = Arc::new(Previews::new(1, MIB, Duration::from_millis(20)));
        let busy = previews.clone();
        let first = tokio::spawn(async move {
            busy.get_or_render("attachments:slow".to_owned(), || async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(vec![0])
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let refused = previews
            .get_or_render("attachments:other".to_owned(), || async { Ok(vec![0]) })
            .await;
        assert!(refused.is_err());
        assert!(first.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_failed_render_is_tried_again() {
        let previews = Previews::new(1, MIB, QUEUE_WAIT);
        let failed = previews
            .get_or_render("attachments:bad".to_owned(), || async {
                Err(create_error!(InternalError))
            })
            .await;
        assert!(failed.is_err());
        let retried = previews
            .get_or_render("attachments:bad".to_owned(), || async { Ok(vec![1]) })
            .await;
        assert_eq!(*retried.unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn each_tag_has_its_own_preview() {
        let previews = Previews::new(1, MIB, QUEUE_WAIT);
        let a = previews
            .get_or_render("avatars:hash".to_owned(), || async { Ok(vec![1]) })
            .await;
        let b = previews
            .get_or_render("icons:hash".to_owned(), || async { Ok(vec![2]) })
            .await;
        assert_ne!(*a.unwrap(), *b.unwrap());
    }
}
