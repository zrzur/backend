use revolt_result::Result;

use crate::{NameRecord, NameScan, OfficialMarkRequest, OfficialMarkStatus, ReferenceDb, User};

use super::AbstractOfficialMarkRequest;

#[async_trait]
impl AbstractOfficialMarkRequest for ReferenceDb {
    async fn insert_official_mark_request(&self, request: &OfficialMarkRequest) -> Result<()> {
        self.official_mark_requests
            .lock()
            .await
            .insert(request.id.clone(), request.clone());
        Ok(())
    }

    async fn fetch_official_mark_request(&self, id: &str) -> Result<OfficialMarkRequest> {
        self.official_mark_requests
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_official_mark_requests(&self) -> Result<Vec<OfficialMarkRequest>> {
        let mut list: Vec<_> = self.official_mark_requests.lock().await.values().cloned().collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        list.truncate(200);
        Ok(list)
    }

    async fn decide_official_mark_request(
        &self,
        request: &OfficialMarkRequest,
        expected: OfficialMarkStatus,
    ) -> Result<bool> {
        let mut requests = self.official_mark_requests.lock().await;
        match requests.get_mut(&request.id) {
            Some(current) if current.status == expected => {
                *current = request.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn fetch_official_users(&self) -> Result<Vec<User>> {
        Ok(self.users.lock().await.values().filter(|user| user.official).cloned().collect())
    }

    async fn fetch_name_records(
        &self,
        scan: NameScan,
        offset: u64,
        limit: i64,
    ) -> Result<Vec<NameRecord>> {
        let mut records: Vec<NameRecord> = match scan {
            NameScan::Users => self
                .users
                .lock()
                .await
                .values()
                .flat_map(|user| {
                    std::iter::once(user.username.clone())
                        .chain(user.display_name.clone())
                        .map(|name| NameRecord {
                            kind: scan,
                            id: user.id.clone(),
                            name,
                            bot: user.bot.is_some(),
                            official: user.official || user.official_bot,
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            NameScan::Nicknames => self
                .server_members
                .lock()
                .await
                .values()
                .filter_map(|member| {
                    member.nickname.clone().map(|name| NameRecord {
                        kind: scan,
                        id: format!("{}/{}", member.id.server, member.id.user),
                        name,
                        bot: false,
                        official: false,
                    })
                })
                .collect(),
            NameScan::Webhooks => self
                .channel_webhooks
                .lock()
                .await
                .values()
                .map(|webhook| NameRecord {
                    kind: scan,
                    id: webhook.id.clone(),
                    name: webhook.name.clone(),
                    bot: false,
                    official: false,
                })
                .collect(),
        };
        records.sort_by(|a, b| (&a.id, &a.name).cmp(&(&b.id, &b.name)));
        Ok(records.into_iter().skip(offset as usize).take(limit as usize).collect())
    }
}
