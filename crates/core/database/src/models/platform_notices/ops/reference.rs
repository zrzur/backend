use revolt_result::Result;

use crate::{PlatformNotice, ReferenceDb};

use super::AbstractPlatformNotice;

#[async_trait]
impl AbstractPlatformNotice for ReferenceDb {
    async fn insert_platform_notice(&self, notice: &PlatformNotice) -> Result<()> {
        self.platform_notices
            .lock()
            .await
            .insert(notice.id.clone(), notice.clone());
        Ok(())
    }

    async fn fetch_platform_notice(&self, id: &str) -> Result<PlatformNotice> {
        self.platform_notices
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_platform_notices(&self, user_id: Option<&str>, limit: usize) -> Result<Vec<PlatformNotice>> {
        let mut list: Vec<_> = self
            .platform_notices
            .lock()
            .await
            .values()
            .filter(|n| user_id.is_none_or(|u| n.user_id.as_deref() == Some(u)))
            .cloned()
            .collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        list.truncate(limit);
        Ok(list)
    }

    async fn fetch_platform_notices_for(&self, user_id: &str, now: i64) -> Result<Vec<PlatformNotice>> {
        let dismissed = self.notice_dismissals.lock().await;
        let mut list: Vec<_> = self
            .platform_notices
            .lock()
            .await
            .values()
            .filter(|n| n.user_id.as_deref().is_none_or(|u| u == user_id))
            .filter(|n| n.is_live(now))
            .filter(|n| !dismissed.contains(&(n.id.clone(), user_id.to_string())))
            .cloned()
            .collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        list.truncate(20);
        Ok(list)
    }

    async fn withdraw_platform_notice(&self, id: &str, at: i64) -> Result<()> {
        match self.platform_notices.lock().await.get_mut(id) {
            Some(notice) => {
                notice.withdrawn_at = Some(at);
                Ok(())
            }
            None => Err(create_error!(NotFound)),
        }
    }

    async fn set_platform_notice_delivery(&self, id: &str, emailed: bool, pushed: bool) -> Result<()> {
        if let Some(notice) = self.platform_notices.lock().await.get_mut(id) {
            notice.emailed = emailed;
            notice.pushed = pushed;
        }
        Ok(())
    }

    async fn dismiss_platform_notice(&self, id: &str, user_id: &str) -> Result<()> {
        self.notice_dismissals
            .lock()
            .await
            .insert((id.to_string(), user_id.to_string()));
        Ok(())
    }
}
