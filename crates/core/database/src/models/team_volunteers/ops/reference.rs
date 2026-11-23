use revolt_result::Result;

use crate::{
    ReferenceDb, TeamActivityDay, TeamIdentity, TeamMember, TeamMemberFilter,
};

use super::AbstractTeamVolunteers;

#[async_trait]
impl AbstractTeamVolunteers for ReferenceDb {
    async fn upsert_team_identity(&self, identity: &TeamIdentity) -> Result<()> {
        self.team_identities
            .lock()
            .await
            .insert(identity.id.clone(), identity.clone());
        Ok(())
    }

    async fn fetch_team_identity(&self, user_id: &str) -> Result<Option<TeamIdentity>> {
        Ok(self.team_identities.lock().await.get(user_id).cloned())
    }

    async fn delete_team_identity(&self, user_id: &str) -> Result<()> {
        self.team_identities.lock().await.remove(user_id);
        Ok(())
    }

    async fn mark_team_identity_purge(&self, user_id: &str, purge_at: i64) -> Result<bool> {
        let mut store = self.team_identities.lock().await;
        match store.get_mut(user_id) {
            Some(identity) if identity.purge_at.is_none() => {
                identity.purge_at = Some(purge_at);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn clear_team_identity_purge(&self, user_id: &str) -> Result<bool> {
        match self.team_identities.lock().await.get_mut(user_id) {
            Some(identity) => {
                identity.purge_at = None;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn fetch_team_identities_kept(&self) -> Result<Vec<TeamIdentity>> {
        let mut kept: Vec<TeamIdentity> = self
            .team_identities
            .lock()
            .await
            .values()
            .filter(|i| i.purge_at.is_none())
            .cloned()
            .collect();
        kept.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(kept)
    }

    async fn delete_team_identities_due(&self, now: i64) -> Result<u64> {
        let mut store = self.team_identities.lock().await;
        let count = store.len();
        store.retain(|_, i| !i.purge_at.is_some_and(|at| at < now));
        Ok((count - store.len()) as u64)
    }

    async fn insert_team_member(&self, member: &TeamMember) -> Result<()> {
        let mut store = self.team_members.lock().await;
        if store.contains_key(&member.id)
            || store
                .values()
                .any(|m| m.user_id == member.user_id && m.role == member.role)
        {
            return Err(create_error!(InvalidOperation));
        }
        store.insert(member.id.clone(), member.clone());
        Ok(())
    }

    async fn fetch_team_member(&self, id: &str) -> Result<TeamMember> {
        self.team_members
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_team_members_of(&self, user_id: &str) -> Result<Vec<TeamMember>> {
        let mut list: Vec<_> = self
            .team_members
            .lock()
            .await
            .values()
            .filter(|m| m.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(list)
    }

    async fn fetch_team_members(&self, filter: &TeamMemberFilter) -> Result<Vec<TeamMember>> {
        let mut list: Vec<_> = self
            .team_members
            .lock()
            .await
            .values()
            .filter(|m| filter.role.as_ref().is_none_or(|r| &m.role == r))
            .filter(|m| filter.status.is_none_or(|s| m.status == s))
            .cloned()
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        if let Some(after) = &filter.after {
            list.retain(|m| m.id.as_str() > after.as_str());
        }
        if let Some(limit) = filter.limit {
            list.truncate(limit as usize);
        }
        Ok(list)
    }

    async fn update_team_member(&self, member: &TeamMember) -> Result<()> {
        let mut store = self.team_members.lock().await;
        match store.get_mut(&member.id) {
            Some(current) => {
                *current = member.clone();
                Ok(())
            }
            None => Err(create_error!(NotFound)),
        }
    }

    async fn delete_team_member_if_left(&self, id: &str, cutoff: i64) -> Result<u64> {
        let mut store = self.team_members.lock().await;
        let due = store.get(id).is_some_and(|m| {
            m.status == crate::TeamMemberStatus::Left && m.left_at.is_some_and(|at| at <= cutoff)
        });
        if due {
            store.remove(id);
        }
        Ok(u64::from(due))
    }

    async fn delete_team_members_of(&self, user_id: &str) -> Result<u64> {
        let mut store = self.team_members.lock().await;
        let count = store.len();
        store.retain(|_, m| m.user_id != user_id);
        Ok((count - store.len()) as u64)
    }

    async fn upsert_team_activity_day(&self, day: &TeamActivityDay) -> Result<()> {
        self.team_activity_days
            .lock()
            .await
            .insert(day.id.clone(), day.clone());
        Ok(())
    }

    async fn fetch_team_activity(
        &self,
        user_id: &str,
        from_day: i32,
        to_day: i32,
    ) -> Result<Vec<TeamActivityDay>> {
        let mut list: Vec<_> = self
            .team_activity_days
            .lock()
            .await
            .values()
            .filter(|d| d.user_id == user_id && d.day >= from_day && d.day <= to_day)
            .cloned()
            .collect();
        list.sort_by_key(|d| d.day);
        Ok(list)
    }

    async fn delete_team_activity_before(&self, day: i32) -> Result<u64> {
        let mut store = self.team_activity_days.lock().await;
        let count = store.len();
        store.retain(|_, d| d.day >= day);
        Ok((count - store.len()) as u64)
    }

    async fn delete_team_activity_of(&self, user_id: &str) -> Result<u64> {
        let mut store = self.team_activity_days.lock().await;
        let count = store.len();
        store.retain(|_, d| d.user_id != user_id);
        Ok((count - store.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ReferenceDb, TeamActivityCount, TeamActivityDay, TeamHistory, TeamIdentity, TeamMember,
        TeamMemberFilter, TeamMemberStatus,
    };

    use super::AbstractTeamVolunteers;

    fn identity(id: &str, purge_at: Option<i64>) -> TeamIdentity {
        TeamIdentity {
            id: id.into(),
            name: "Sara".into(),
            age: 24,
            country: "Austria".into(),
            rules_version: "1".into(),
            rules_at: 100,
            purge_at,
            reviewer: false,
        }
    }

    fn member(id: &str, user: &str, role: &str) -> TeamMember {
        TeamMember {
            id: id.into(),
            user_id: user.into(),
            role: role.into(),
            server: "server1".into(),
            status: TeamMemberStatus::Trial,
            joined_at: 1,
            trial_ends_at: 2,
            confirmed_at: None,
            away_until: None,
            left_at: None,
            low_months: 0,
            notes: Some("note".into()),
            invite: None,
            history: vec![TeamHistory {
                at: 1,
                actor: "staff".into(),
                action: "joined".into(),
            }],
        }
    }

    fn day(user: &str, day: i32) -> TeamActivityDay {
        TeamActivityDay {
            id: format!("{user}:{day}"),
            user_id: user.into(),
            day,
            counts: vec![TeamActivityCount {
                server: "server1".into(),
                messages: 3,
                replies: 1,
                moderation: 0,
                reviews: 0,
                tickets: 2,
            }],
        }
    }

    #[tokio::test]
    async fn ended_membership_is_deleted_only_while_ended_and_due() {
        let db = ReferenceDb::default();
        let mut left = member("m1", "u1", "support");
        left.status = TeamMemberStatus::Left;
        left.left_at = Some(100);
        db.insert_team_member(&left).await.unwrap();
        // not due yet
        assert_eq!(db.delete_team_member_if_left("m1", 99).await.unwrap(), 0);
        // reopened in between: never deleted
        let mut rejoined = left.clone();
        rejoined.status = TeamMemberStatus::Trial;
        rejoined.left_at = None;
        db.update_team_member(&rejoined).await.unwrap();
        assert_eq!(db.delete_team_member_if_left("m1", 1_000).await.unwrap(), 0);
        assert!(db.fetch_team_member("m1").await.is_ok());
        // ended without a date: not deleted by this rule
        let mut undated = left.clone();
        undated.left_at = None;
        db.update_team_member(&undated).await.unwrap();
        assert_eq!(db.delete_team_member_if_left("m1", 1_000).await.unwrap(), 0);
        // ended and due
        db.update_team_member(&left).await.unwrap();
        assert_eq!(db.delete_team_member_if_left("m1", 100).await.unwrap(), 1);
        assert!(db.fetch_team_member("m1").await.is_err());
        assert_eq!(db.delete_team_member_if_left("m1", 100).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn purge_marks_never_create_or_overwrite() {
        let db = ReferenceDb::default();
        // no record: nothing comes back
        assert!(!db.mark_team_identity_purge("u1", 5).await.unwrap());
        assert!(!db.clear_team_identity_purge("u1").await.unwrap());
        assert_eq!(db.fetch_team_identity("u1").await.unwrap(), None);
        // set only while none is set
        db.upsert_team_identity(&identity("u1", None)).await.unwrap();
        assert!(db.mark_team_identity_purge("u1", 5).await.unwrap());
        assert!(!db.mark_team_identity_purge("u1", 9).await.unwrap());
        assert_eq!(db.fetch_team_identity("u1").await.unwrap().unwrap().purge_at, Some(5));
        assert!(db.clear_team_identity_purge("u1").await.unwrap());
        assert_eq!(db.fetch_team_identity("u1").await.unwrap().unwrap().purge_at, None);
        // the rest of the record is never written
        assert_eq!(db.fetch_team_identity("u1").await.unwrap().unwrap().name, "Sara");
    }

    #[tokio::test]
    async fn identity_round_trip() {
        let db = ReferenceDb::default();
        let i = identity("u1", None);
        db.upsert_team_identity(&i).await.unwrap();
        assert_eq!(db.fetch_team_identity("u1").await.unwrap(), Some(i));
        assert_eq!(db.fetch_team_identity("u2").await.unwrap(), None);
        db.delete_team_identity("u1").await.unwrap();
        assert_eq!(db.fetch_team_identity("u1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn member_round_trip() {
        let db = ReferenceDb::default();
        let mut m = member("m1", "u1", "moderator");
        db.insert_team_member(&m).await.unwrap();
        assert_eq!(db.fetch_team_member("m1").await.unwrap(), m);
        assert_eq!(db.fetch_team_members_of("u1").await.unwrap(), vec![m.clone()]);
        m.status = TeamMemberStatus::Active;
        assert!(m.is_current());
        db.update_team_member(&m).await.unwrap();
        let active = TeamMemberFilter {
            role: Some("moderator".into()),
            status: Some(TeamMemberStatus::Active),
            ..Default::default()
        };
        assert_eq!(db.fetch_team_members(&active).await.unwrap(), vec![m.clone()]);
        let trial = TeamMemberFilter {
            role: None,
            status: Some(TeamMemberStatus::Trial),
            ..Default::default()
        };
        assert!(db.fetch_team_members(&trial).await.unwrap().is_empty());
        m.status = TeamMemberStatus::Left;
        assert!(!m.is_current());
    }

    #[tokio::test]
    async fn members_page_by_id() {
        let db = ReferenceDb::default();
        for (id, user) in [("m3", "u3"), ("m1", "u1"), ("m2", "u2"), ("m4", "u4")] {
            db.insert_team_member(&member(id, user, "moderator")).await.unwrap();
        }
        let ids = |list: Vec<TeamMember>| list.into_iter().map(|m| m.id).collect::<Vec<_>>();
        let first = TeamMemberFilter { limit: Some(2), ..Default::default() };
        assert_eq!(ids(db.fetch_team_members(&first).await.unwrap()), ["m1", "m2"]);
        let next = TeamMemberFilter {
            after: Some("m2".into()),
            limit: Some(5),
            ..Default::default()
        };
        assert_eq!(ids(db.fetch_team_members(&next).await.unwrap()), ["m3", "m4"]);
    }

    #[tokio::test]
    async fn one_member_per_person_and_role() {
        let db = ReferenceDb::default();
        db.insert_team_member(&member("m1", "u1", "moderator"))
            .await
            .unwrap();
        assert!(db
            .insert_team_member(&member("m2", "u1", "moderator"))
            .await
            .is_err());
        db.insert_team_member(&member("m3", "u1", "support"))
            .await
            .unwrap();
        db.insert_team_member(&member("m4", "u2", "moderator"))
            .await
            .unwrap();
        assert_eq!(db.delete_team_members_of("u1").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn identities_due_are_deleted() {
        let db = ReferenceDb::default();
        db.upsert_team_identity(&identity("due", Some(50))).await.unwrap();
        db.upsert_team_identity(&identity("later", Some(500))).await.unwrap();
        db.upsert_team_identity(&identity("kept", None)).await.unwrap();
        assert_eq!(db.delete_team_identities_due(100).await.unwrap(), 1);
        assert!(db.fetch_team_identity("due").await.unwrap().is_none());
        assert!(db.fetch_team_identity("later").await.unwrap().is_some());
        assert!(db.fetch_team_identity("kept").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn activity_range_is_inclusive() {
        let db = ReferenceDb::default();
        for d in [20261101, 20261105, 20261110, 20261111] {
            db.upsert_team_activity_day(&day("u1", d)).await.unwrap();
        }
        db.upsert_team_activity_day(&day("u2", 20261105)).await.unwrap();
        let found = db.fetch_team_activity("u1", 20261105, 20261110).await.unwrap();
        assert_eq!(
            found.iter().map(|d| d.day).collect::<Vec<_>>(),
            vec![20261105, 20261110]
        );
        assert_eq!(found[0], day("u1", 20261105));
        assert_eq!(db.delete_team_activity_before(20261105).await.unwrap(), 1);
        assert_eq!(db.delete_team_activity_of("u1").await.unwrap(), 3);
    }
}
