use revolt_result::Result;

use crate::{
    ApplicationStatus, BadgeApplication, BadgeEvidence, BadgeProgram, BadgeReview,
    ServerActivityWeek,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

pub use reference::ProgramStore;

#[async_trait]
pub trait AbstractBadgePrograms: Sync + Send {
    async fn insert_badge_application(&self, application: &BadgeApplication) -> Result<()>;

    async fn fetch_badge_application(&self, id: &str) -> Result<BadgeApplication>;

    /// Replace an application if nobody changed it since `expected_updated_at`
    async fn save_badge_application(
        &self,
        application: &BadgeApplication,
        expected_updated_at: i64,
    ) -> Result<bool>;

    /// Every application of one person, newest first
    async fn fetch_badge_applications_by_applicant(&self, user_id: &str) -> Result<Vec<BadgeApplication>>;

    /// Every application for one server or bot, newest first
    async fn fetch_badge_applications_for_target(&self, target_id: &str) -> Result<Vec<BadgeApplication>>;

    /// The review queue: optionally one status and one program, newest first
    async fn fetch_badge_applications(
        &self,
        status: Option<ApplicationStatus>,
        program: Option<BadgeProgram>,
        limit: usize,
    ) -> Result<Vec<BadgeApplication>>;

    /// Approved applications whose recheck date has passed and is not
    /// flagged yet
    async fn fetch_badge_applications_due(&self, now: i64) -> Result<Vec<BadgeApplication>>;

    /// Applications whose evidence or whole record is past its retention,
    /// and drafts not touched since `draft_before`
    async fn fetch_badge_applications_expired(&self, now: i64, draft_before: i64) -> Result<Vec<BadgeApplication>>;

    /// Delete an application with its evidence and reviews
    async fn delete_badge_application(&self, id: &str) -> Result<()>;

    async fn insert_badge_evidence(&self, evidence: &BadgeEvidence) -> Result<()>;

    /// Evidence of one application, oldest first
    async fn fetch_badge_evidence(&self, application_id: &str) -> Result<Vec<BadgeEvidence>>;

    async fn delete_badge_evidence(&self, application_id: &str) -> Result<()>;

    async fn insert_badge_review(&self, review: &BadgeReview) -> Result<()>;

    /// Reviews of one application, oldest first
    async fn fetch_badge_reviews(&self, application_id: &str) -> Result<Vec<BadgeReview>>;

    /// Create or replace one week of a server
    async fn save_server_activity_week(&self, week: &ServerActivityWeek) -> Result<()>;

    /// Weeks of one server from `from_week` on, oldest first
    async fn fetch_server_activity_weeks(&self, server_id: &str, from_week: i64) -> Result<Vec<ServerActivityWeek>>;

    /// Delete every week before `week` (all servers)
    async fn delete_server_activity_weeks_before(&self, week: i64) -> Result<()>;

    /// Distinct authors of messages in these channels with ids in
    /// `[from, to)` (system messages and webhooks left out). Only ids come
    /// back; never content.
    async fn fetch_message_author_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>>;

    /// Distinct people whose read position in these channels is a message
    /// with an id in `[from, to)`
    async fn fetch_channel_reader_ids(&self, channels: &[String], from: &str, to: &str) -> Result<Vec<String>>;
}
