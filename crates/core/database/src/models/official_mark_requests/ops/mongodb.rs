use bson::Document;
use mongodb::options::FindOptions;
use revolt_result::Result;

use crate::{MongoDb, NameRecord, NameScan, OfficialMarkRequest, OfficialMarkStatus, User};

use super::AbstractOfficialMarkRequest;

static COL: &str = "official_mark_requests";

fn status_name(status: OfficialMarkStatus) -> &'static str {
    match status {
        OfficialMarkStatus::Pending => "Pending",
        OfficialMarkStatus::Confirmed => "Confirmed",
        OfficialMarkStatus::Refused => "Refused",
        OfficialMarkStatus::Withdrawn => "Withdrawn",
    }
}

#[async_trait]
impl AbstractOfficialMarkRequest for MongoDb {
    async fn insert_official_mark_request(&self, request: &OfficialMarkRequest) -> Result<()> {
        query!(self, insert_one, COL, &request).map(|_| ())
    }

    async fn fetch_official_mark_request(&self, id: &str) -> Result<OfficialMarkRequest> {
        query!(self, find_one_by_id, COL, id)?.ok_or_else(|| create_error!(NotFound))
    }

    async fn fetch_official_mark_requests(&self) -> Result<Vec<OfficialMarkRequest>> {
        self.find_with_options(
            COL,
            doc! {},
            FindOptions::builder().sort(doc! { "_id": -1 }).limit(200).build(),
        )
        .await
        .map_err(|_| create_database_error!("find", COL))
    }

    async fn decide_official_mark_request(
        &self,
        request: &OfficialMarkRequest,
        expected: OfficialMarkStatus,
    ) -> Result<bool> {
        // confirmed twice at the same moment: only the first counts
        self.col::<OfficialMarkRequest>(COL)
            .replace_one(doc! { "_id": &request.id, "status": status_name(expected) }, request)
            .await
            .map(|r| r.matched_count == 1)
            .map_err(|_| create_database_error!("replace_one", COL))
    }

    async fn fetch_official_users(&self) -> Result<Vec<User>> {
        query!(self, find, "users", doc! { "official": true })
    }

    async fn fetch_name_records(
        &self,
        scan: NameScan,
        offset: u64,
        limit: i64,
    ) -> Result<Vec<NameRecord>> {
        let (collection, filter, projection) = match scan {
            NameScan::Users => (
                "users",
                doc! {},
                doc! { "username": 1, "display_name": 1, "bot": 1, "official": 1, "official_bot": 1 },
            ),
            NameScan::Nicknames => (
                "server_members",
                doc! { "nickname": { "$exists": true } },
                doc! { "nickname": 1 },
            ),
            NameScan::Webhooks => ("channel_webhooks", doc! {}, doc! { "name": 1 }),
        };
        let documents: Vec<Document> = self
            .find_with_options(
                collection,
                filter,
                FindOptions::builder()
                    .projection(projection)
                    .sort(doc! { "_id": 1 })
                    .skip(offset)
                    .limit(limit)
                    .build(),
            )
            .await
            .map_err(|_| create_database_error!("find", collection))?;

        let mut records = vec![];
        for document in documents {
            let name_of = |field: &str| document.get_str(field).ok().map(str::to_string);
            let id = match scan {
                NameScan::Nicknames => document
                    .get_document("_id")
                    .map(|id| {
                        format!(
                            "{}/{}",
                            id.get_str("server").unwrap_or_default(),
                            id.get_str("user").unwrap_or_default()
                        )
                    })
                    .unwrap_or_default(),
                _ => document.get_str("_id").unwrap_or_default().to_string(),
            };
            let names = match scan {
                NameScan::Users => [name_of("username"), name_of("display_name")],
                NameScan::Nicknames => [name_of("nickname"), None],
                NameScan::Webhooks => [name_of("name"), None],
            };
            for name in names.into_iter().flatten() {
                records.push(NameRecord {
                    kind: scan,
                    id: id.clone(),
                    name,
                    bot: document.contains_key("bot"),
                    official: document.get_bool("official").unwrap_or_default()
                        || document.get_bool("official_bot").unwrap_or_default(),
                });
            }
        }
        Ok(records)
    }
}
