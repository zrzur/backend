use revolt_result::Result;

use crate::ReferenceDb;
use crate::{FieldsMember, Member, MemberCompositeKey, PartialMember};

use super::{AbstractServerMembers, ChunkedServerMembersGenerator};

#[async_trait]
impl AbstractServerMembers for ReferenceDb {
    async fn add_member_role(&self, id: &MemberCompositeKey, role: &str) -> Result<Member> {
        let mut members = self.server_members.lock().await;
        let member = members.get_mut(id).ok_or_else(|| create_error!(NotFound))?;
        if !member.roles.iter().any(|r| r == role) { member.roles.push(role.to_string()); }
        Ok(member.clone())
    }

    async fn remove_member_role(&self, id: &MemberCompositeKey, role: &str) -> Result<Member> {
        let mut members = self.server_members.lock().await;
        let member = members.get_mut(id).ok_or_else(|| create_error!(NotFound))?;
        member.roles.retain(|r| r != role);
        Ok(member.clone())
    }

    /// Insert a new server member into the database
    async fn insert_or_merge_member(&self, member: &Member) -> Result<Option<Member>> {
        let mut server_members = self.server_members.lock().await;
        // a member who left while timed out comes back with the kept record
        // (its timeout) and the new join time, as on MongoDB
        if let Some(mut kept) = self.pending_member_deletions.lock().await.remove(&member.id) {
            kept.joined_at = member.joined_at;
            server_members.insert(kept.id.clone(), kept.clone());
            return Ok(Some(kept));
        }
        if server_members.contains_key(&member.id) {
            Err(create_database_error!("insert", "member"))
        } else {
            server_members.insert(member.id.clone(), member.clone());
            Ok(None)
        }
    }

    /// Fetch a server member by their id
    async fn fetch_member(&self, server_id: &str, user_id: &str) -> Result<Member> {
        let server_members = self.server_members.lock().await;
        server_members
            .get(&MemberCompositeKey {
                server: server_id.to_string(),
                user: user_id.to_string(),
            })
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch all members in a server
    async fn fetch_all_members(&self, server_id: &str) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;
        Ok(server_members
            .values()
            .filter(|member| member.id.server == server_id)
            .cloned()
            .collect())
    }

    async fn fetch_members_page(
        &self,
        server_id: &str,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;
        let mut members: Vec<Member> = server_members
            .values()
            .filter(|member| member.id.server == server_id)
            .filter(|member| after.is_none_or(|after| member.id.user.as_str() > after))
            .cloned()
            .collect();
        members.sort_by(|a, b| a.id.user.cmp(&b.id.user));
        members.truncate(limit.max(0) as usize);
        Ok(members)
    }

    async fn fetch_newest_members(&self, server_id: &str, limit: i64) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;
        let mut members: Vec<Member> = server_members
            .values()
            .filter(|member| member.id.server == server_id)
            .cloned()
            .collect();
        members.sort_by(|a, b| b.joined_at.cmp(&a.joined_at));
        members.truncate(limit.max(0) as usize);
        Ok(members)
    }

    /// Fetch all members in a server as an iterator
    async fn fetch_all_members_chunked(
        &self,
        server_id: &str,
    ) -> Result<ChunkedServerMembersGenerator> {
        let server_members = self.server_members.lock().await;

        let members = server_members
            .clone()
            .into_values()
            .filter(move |member| member.id.server == server_id)
            .collect();

        // inefficient, but acceptable for the reference database
        Ok(ChunkedServerMembersGenerator::new_reference(members))
    }

    /// Fetch all members that have any of the roles given
    async fn fetch_all_members_with_roles(
        &self,
        server_id: &str,
        roles: &[String],
    ) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;

        Ok(server_members
            .clone()
            .into_values()
            .filter(|member| {
                member.id.server == server_id
                    && !member
                        .roles
                        .iter()
                        .filter(|p| roles.contains(*p))
                        .collect::<Vec<&String>>()
                        .is_empty()
            })
            .collect())
    }

    async fn fetch_all_members_with_roles_chunked(
        &self,
        server_id: &str,
        roles: &[String],
    ) -> Result<ChunkedServerMembersGenerator> {
        let server_members = self.server_members.lock().await;

        let resp = server_members
            .clone()
            .into_values()
            .filter(|member| {
                member.id.server == server_id
                    && !member
                        .roles
                        .iter()
                        .filter(|p| roles.contains(*p))
                        .collect::<Vec<&String>>()
                        .is_empty()
            })
            .collect();

        return Ok(ChunkedServerMembersGenerator::new_reference(resp));
    }

    /// Fetch all memberships for a user
    async fn fetch_all_memberships(&self, user_id: &str) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;
        Ok(server_members
            .values()
            .filter(|member| member.id.user == user_id)
            .cloned()
            .collect())
    }

    /// Fetch multiple members by their ids
    async fn fetch_members(&self, server_id: &str, ids: &[String]) -> Result<Vec<Member>> {
        let server_members = self.server_members.lock().await;
        Ok(ids
            .iter()
            .filter_map(|id| {
                server_members
                    .get(&MemberCompositeKey {
                        server: server_id.to_string(),
                        user: id.to_string(),
                    })
                    .cloned()
            })
            .collect())
    }

    /// Fetch member count of a server
    async fn fetch_member_count(&self, server_id: &str) -> Result<usize> {
        let server_members = self.server_members.lock().await;
        Ok(server_members
            .values()
            .filter(|member| member.id.server == server_id)
            .count())
    }

    async fn fetch_server_member_totals(&self, min: usize) -> Result<Vec<(String, usize)>> {
        let server_members = self.server_members.lock().await;
        let mut totals: std::collections::HashMap<String, usize> = Default::default();
        for member in server_members.values() {
            *totals.entry(member.id.server.clone()).or_default() += 1;
        }
        let mut totals: Vec<(String, usize)> = totals.into_iter().filter(|(_, n)| *n >= min).collect();
        totals.sort();
        Ok(totals)
    }

    /// Fetch server count of a user
    async fn fetch_server_count(&self, user_id: &str) -> Result<usize> {
        let server_members = self.server_members.lock().await;
        Ok(server_members
            .values()
            .filter(|member| member.id.user == user_id)
            .count())
    }

    /// Update information for a server member
    async fn update_member(
        &self,
        id: &MemberCompositeKey,
        partial: &PartialMember,
        remove: Vec<FieldsMember>,
    ) -> Result<()> {
        let mut server_members = self.server_members.lock().await;
        if let Some(member) = server_members.get_mut(id) {
            for field in remove {
                #[allow(clippy::disallowed_methods)]
                member.remove_field(&field);
            }

            member.apply_options(partial.clone());
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Soft delete a member
    async fn soft_delete_member(&self, id: &MemberCompositeKey) -> Result<()> {
        let mut server_members = self.server_members.lock().await;

        let member = server_members.get_mut(id);
        if let Some(member) = member {
            if member.in_timeout() {
                // MongoDB marks the record `pending_deletion_at` and drops the
                // server-specific parts; every member query skips it
                let mut kept = server_members.remove(id).expect("member just found");
                kept.avatar = None;
                kept.nickname = None;
                kept.roles = vec![];
                self.pending_member_deletions.lock().await.insert(id.clone(), kept);
                Ok(())
            } else if server_members.remove(id).is_some() {
                Ok(())
            } else {
                Err(create_error!(NotFound))
            }
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Delete a server member by their id
    async fn force_delete_member(&self, id: &MemberCompositeKey) -> Result<()> {
        let mut server_members = self.server_members.lock().await;
        let pending = self.pending_member_deletions.lock().await.remove(id).is_some();
        if server_members.remove(id).is_some() || pending {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    async fn remove_dangling_members(&self) -> Result<()> {
        todo!()
    }

    /// Removes a user from every server they are in
    async fn clear_memberships(&self, user_id: &str) -> Result<()> {
        let mut server_members = self.server_members.lock().await;

        server_members.retain(|_, v| v.id.user != user_id);
        self.pending_member_deletions
            .lock()
            .await
            .retain(|_, v| v.id.user != user_id);

        Ok(())
    }
}
