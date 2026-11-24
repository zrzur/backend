use std::collections::{BTreeSet, HashMap};

use revolt_result::Result;

use crate::{
    ApplicationStatus, BadgeApplication, BadgeEvidence, BadgeProgram, BadgeReview, ReferenceDb,
    ServerActivityWeek,
};

use super::AbstractBadgePrograms;

/// In-memory badge program data of the reference database
#[derive(Default, Debug)]
pub struct ProgramStore {
    pub applications: HashMap<String, BadgeApplication>,
    pub evidence: Vec<BadgeEvidence>,
    pub reviews: Vec<BadgeReview>,
    pub weeks: HashMap<String, ServerActivityWeek>,
}

fn newest_first(mut list: Vec<BadgeApplication>) -> Vec<BadgeApplication> {
    list.sort_by(|a, b| b.id.cmp(&a.id));
    list
}

#[async_trait]
impl AbstractBadgePrograms for ReferenceDb {
    async fn insert_badge_application(&self, application: &BadgeApplication) -> Result<()> {
        let mut store = self.badge_programs.lock().await;
        if store.applications.contains_key(&application.id) {
            return Err(create_database_error!("insert_one", "badge_applications"));
        }
        store.applications.insert(application.id.clone(), application.clone());
        Ok(())
    }

    async fn fetch_badge_application(&self, id: &str) -> Result<BadgeApplication> {
        self.badge_programs
            .lock()
            .await
            .applications
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn save_badge_application(&self, application: &BadgeApplication, expected_updated_at: i64) -> Result<bool> {
        let mut store = self.badge_programs.lock().await;
        match store.applications.get(&application.id) {
            Some(current) if current.updated_at == expected_updated_at => {
                store.applications.insert(application.id.clone(), application.clone());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_badge_applications_by_applicant(&self, user_id: &str) -> Result<Vec<BadgeApplication>> {
        let store = self.badge_programs.lock().await;
        Ok(newest_first(
            store.applications.values().filter(|a| a.applicant_id == user_id).cloned().collect(),
        ))
    }

    async fn fetch_badge_applications_for_target(&self, target_id: &str) -> Result<Vec<BadgeApplication>> {
        let store = self.badge_programs.lock().await;
        Ok(newest_first(
            store.applications.values().filter(|a| a.target_id == target_id).cloned().collect(),
        ))
    }

    async fn fetch_badge_applications(
        &self,
        status: Option<ApplicationStatus>,
        program: Option<BadgeProgram>,
        limit: usize,
    ) -> Result<Vec<BadgeApplication>> {
        let store = self.badge_programs.lock().await;
        let mut list = newest_first(
            store
                .applications
                .values()
                .filter(|a| status.is_none_or(|s| a.status == s))
                .filter(|a| program.is_none_or(|p| a.program == p))
                .cloned()
                .collect(),
        );
        list.truncate(limit);
        Ok(list)
    }

    async fn fetch_badge_applications_due(&self, now: i64) -> Result<Vec<BadgeApplication>> {
        let store = self.badge_programs.lock().await;
        Ok(store
            .applications
            .values()
            .filter(|a| {
                a.status == ApplicationStatus::Approved
                    && !a.recheck_due
                    && a.next_review_at.is_some_and(|at| at <= now)
            })
            .cloned()
            .collect())
    }

    async fn fetch_badge_applications_expired(&self, now: i64, draft_before: i64) -> Result<Vec<BadgeApplication>> {
        let store = self.badge_programs.lock().await;
        Ok(store
            .applications
            .values()
            .filter(|a| {
                (!a.evidence_purged && a.evidence_purge_at.is_some_and(|at| at <= now))
                    || a.purge_at.is_some_and(|at| at <= now)
                    || (a.status == ApplicationStatus::Draft && a.updated_at < draft_before)
            })
            .cloned()
            .collect())
    }

    async fn delete_badge_application(&self, id: &str) -> Result<()> {
        let mut store = self.badge_programs.lock().await;
        store.applications.remove(id);
        store.evidence.retain(|e| e.application_id != id);
        store.reviews.retain(|r| r.application_id != id);
        Ok(())
    }

    async fn insert_badge_evidence(&self, evidence: &BadgeEvidence) -> Result<()> {
        self.badge_programs.lock().await.evidence.push(evidence.clone());
        Ok(())
    }

    async fn fetch_badge_evidence(&self, application_id: &str) -> Result<Vec<BadgeEvidence>> {
        let store = self.badge_programs.lock().await;
        let mut list: Vec<_> = store.evidence.iter().filter(|e| e.application_id == application_id).cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn delete_badge_evidence(&self, application_id: &str) -> Result<()> {
        self.badge_programs.lock().await.evidence.retain(|e| e.application_id != application_id);
        Ok(())
    }

    async fn insert_badge_review(&self, review: &BadgeReview) -> Result<()> {
        self.badge_programs.lock().await.reviews.push(review.clone());
        Ok(())
    }

    async fn fetch_badge_reviews(&self, application_id: &str) -> Result<Vec<BadgeReview>> {
        let store = self.badge_programs.lock().await;
        let mut list: Vec<_> = store.reviews.iter().filter(|r| r.application_id == application_id).cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn save_server_activity_week(&self, week: &ServerActivityWeek) -> Result<()> {
        self.badge_programs.lock().await.weeks.insert(week.id.clone(), week.clone());
        Ok(())
    }

    async fn fetch_server_activity_weeks(&self, server_id: &str, from_week: i64) -> Result<Vec<ServerActivityWeek>> {
        let store = self.badge_programs.lock().await;
        let mut list: Vec<_> = store
            .weeks
            .values()
            .filter(|w| w.server_id == server_id && w.week >= from_week)
            .cloned()
            .collect();
        list.sort_by_key(|w| w.week);
        Ok(list)
    }

    async fn delete_server_activity_weeks_before(&self, week: i64) -> Result<()> {
        self.badge_programs.lock().await.weeks.retain(|_, w| w.week >= week);
        Ok(())
    }

    async fn fetch_message_author_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>> {
        let messages = self.messages.lock().await;
        let authors: BTreeSet<String> = messages
            .values()
            .filter(|m| {
                channels.contains(&m.channel)
                    && m.id.as_str() >= from
                    && m.id.as_str() < to
                    && m.system.is_none()
                    && m.webhook.is_none()
            })
            .map(|m| m.author.clone())
            .collect();
        Ok(authors.into_iter().collect())
    }

    async fn fetch_channel_reader_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>> {
        let unreads = self.channel_unreads.lock().await;
        let readers: BTreeSet<String> = unreads
            .values()
            .filter(|u| {
                channels.contains(&u.id.channel)
                    && u.last_id.as_deref().is_some_and(|id| id >= from && id < to)
            })
            .map(|u| u.id.user.clone())
            .collect();
        Ok(readers.into_iter().collect())
    }
}
