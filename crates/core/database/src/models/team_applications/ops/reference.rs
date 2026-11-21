use revolt_result::Result;

use crate::{ReferenceDb, TeamApplication, TeamApplicationStatus, TeamGrant};

use super::AbstractTeamApplications;

#[async_trait]
impl AbstractTeamApplications for ReferenceDb {
    async fn insert_team_application(&self, application: &TeamApplication) -> Result<()> {
        self.team_applications
            .lock()
            .await
            .insert(application.id.clone(), application.clone());
        Ok(())
    }

    async fn fetch_team_application(&self, id: &str) -> Result<TeamApplication> {
        self.team_applications
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_team_applications_of(&self, user_id: &str) -> Result<Vec<TeamApplication>> {
        let mut list: Vec<_> = self
            .team_applications
            .lock()
            .await
            .values()
            .filter(|a| a.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(list)
    }

    async fn fetch_open_team_applications(&self) -> Result<Vec<TeamApplication>> {
        let mut list: Vec<_> = self
            .team_applications
            .lock()
            .await
            .values()
            .filter(|a| a.is_open())
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn fetch_team_applications_pending_grant(&self) -> Result<Vec<TeamApplication>> {
        let mut list: Vec<_> = self
            .team_applications
            .lock()
            .await
            .values()
            .filter(|a| {
                a.status == TeamApplicationStatus::Accepted
                    && a.grant == Some(TeamGrant::PendingJoin)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn fetch_team_applications_accepted(&self) -> Result<Vec<TeamApplication>> {
        let mut list: Vec<_> = self
            .team_applications
            .lock()
            .await
            .values()
            .filter(|a| a.status == TeamApplicationStatus::Accepted)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn fetch_team_applications_acted(
        &self,
        from: i64,
        to: i64,
    ) -> Result<Vec<TeamApplication>> {
        let mut list: Vec<_> = self
            .team_applications
            .lock()
            .await
            .values()
            .filter(|a| a.acted_at.is_some_and(|at| at >= from && at <= to))
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn replace_team_application_if(
        &self,
        application: &TeamApplication,
        expected: TeamApplicationStatus,
    ) -> Result<bool> {
        let mut store = self.team_applications.lock().await;
        match store.get(&application.id) {
            Some(current) if current.status == expected => {
                store.insert(application.id.clone(), application.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn delete_team_application(&self, id: &str) -> Result<()> {
        self.team_applications.lock().await.remove(id);
        Ok(())
    }

    async fn delete_team_applications_due(&self, now: i64) -> Result<u64> {
        let mut store = self.team_applications.lock().await;
        let count = store.len();
        store.retain(|_, a| !a.purge_at.is_some_and(|at| at < now));
        Ok((count - store.len()) as u64)
    }

    async fn delete_team_applications_of(&self, user_id: &str) -> Result<u64> {
        let mut store = self.team_applications.lock().await;
        let count = store.len();
        store.retain(|_, a| a.user_id != user_id);
        Ok((count - store.len()) as u64)
    }
}
