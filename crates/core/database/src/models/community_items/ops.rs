use revolt_result::Result;

use crate::{
    CommunityItem, CommunityItemKind, CommunityItemStatus, CommunityReview, CreatorPermissionLog,
    PlatformRole, User,
};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

/// Order of a community list
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommunitySort {
    /// Newest first
    #[default]
    New,
    /// Most unique users who applied it first
    Popular,
}

/// Filters for listing community items
#[derive(Default, Clone)]
pub struct CommunityItemQuery<'a> {
    pub kind: Option<CommunityItemKind>,
    /// Case-insensitive match on the name or a tag, or any of
    /// `search_creators` made it
    pub search: Option<&'a str>,
    pub search_creators: Vec<String>,
    pub creator_id: Option<&'a str>,
    /// Leave out what these creators made (suspended creators)
    pub hidden_creators: Vec<String>,
    /// Only these ids (favourites)
    pub ids: Option<Vec<String>>,
    pub statuses: Vec<CommunityItemStatus>,
    /// Only items that have at least one report
    pub reported: bool,
    /// Only items with a revision waiting for review
    pub pending_revision: bool,
    pub sort: CommunitySort,
    pub limit: usize,
    pub offset: usize,
}

#[async_trait]
pub trait AbstractCommunityItems: Sync + Send {
    /// Save a new community item
    async fn insert_community_item(&self, item: &CommunityItem) -> Result<()>;

    /// Fetch one item by id
    async fn fetch_community_item(&self, id: &str) -> Result<CommunityItem>;

    /// List items
    async fn fetch_community_items(
        &self,
        query: CommunityItemQuery<'_>,
    ) -> Result<Vec<CommunityItem>>;

    /// Count a creator's items in the given statuses (all kinds)
    async fn count_community_items_by_creator(
        &self,
        creator_id: &str,
        statuses: &[CommunityItemStatus],
    ) -> Result<usize>;

    /// Record a report (once per user); returns the number of reports
    async fn report_community_item(&self, id: &str, user_id: &str) -> Result<usize>;

    /// Save the review fields of `item` (status, file, name, description,
    /// tags, revision, published_at, reviewed_by, review_note, pending,
    /// version) only if the stored version is still `expected_version`.
    /// Returns false if someone else changed it first.
    async fn update_community_item(
        &self,
        item: &CommunityItem,
        expected_version: i64,
        clear_reports: bool,
    ) -> Result<bool>;

    /// Remove an item for good, with its favourites, use counts and review
    /// log. Files and wearers are the caller's (see `erase_community_items_of`).
    async fn delete_community_item(&self, id: &str) -> Result<()>;

    /// Take an item off every user that wears it
    async fn unset_community_item_from_users(&self, id: &str) -> Result<()>;

    /// Point everyone wearing an item at its newly approved image
    /// Who has this item on, at most `limit` people
    async fn community_item_wearers(&self, id: &str, limit: usize) -> Result<Vec<String>>;

    async fn set_community_item_file_on_users(&self, id: &str, file_id: &str) -> Result<()>;

    /// Count a user applying an item (once per user ever)
    async fn record_community_item_use(&self, id: &str, user_id: &str) -> Result<()>;

    /// Append to the review log
    async fn insert_community_review(&self, review: &CommunityReview) -> Result<()>;

    /// Review log of one item, newest first
    async fn fetch_community_reviews(&self, item_id: &str) -> Result<Vec<CommunityReview>>;

    /// Save an item to a user's favourites (no-op if already saved)
    async fn add_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()>;

    /// Remove an item from a user's favourites
    async fn remove_community_favourite(&self, user_id: &str, item_id: &str) -> Result<()>;

    /// Ids of a user's favourite items, newest first
    async fn fetch_community_favourites(&self, user_id: &str) -> Result<Vec<String>>;

    /// Platform permissions of one user
    async fn fetch_platform_role(&self, user_id: &str) -> Result<Option<PlatformRole>>;

    /// Platform permission records of several users
    async fn fetch_platform_roles(&self, user_ids: &[String]) -> Result<Vec<PlatformRole>>;

    /// Creators who are suspended right now
    async fn fetch_suspended_creators(&self) -> Result<Vec<String>>;

    /// Everyone who is or was a creator, reviewer or admin
    async fn fetch_all_platform_roles(&self) -> Result<Vec<PlatformRole>>;

    /// Create or replace a platform permission record
    async fn save_platform_role(&self, role: &PlatformRole) -> Result<()>;

    /// Append to the creator log
    async fn insert_creator_log(&self, log: &CreatorPermissionLog) -> Result<()>;

    /// Creator log of one user, newest first
    async fn fetch_creator_logs(&self, user_id: &str) -> Result<Vec<CreatorPermissionLog>>;

    /// Find users by username or display name (staff tools, creator search)
    async fn search_users_by_name(&self, query: &str, limit: usize) -> Result<Vec<User>>;
}
