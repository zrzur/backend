use revolt_database::{Database, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Discover moderation queue
///
/// Lists Discover requests that are pending or under review, requests
/// from Plus members first. Only privileged users may use this endpoint.
#[openapi(tag = "Discover")]
#[get("/requests")]
pub async fn queue(db: &State<Database>, user: User) -> Result<Json<Vec<v0::DiscoverQueueEntry>>> {
    if !user.privileged {
        return Err(create_error!(NotPrivileged));
    }
    queue_entries(db).await.map(Json)
}

/// Open requests with their server/bot details, Plus members first
pub(crate) async fn queue_entries(db: &Database) -> Result<Vec<v0::DiscoverQueueEntry>> {
    let mut entries = Vec::new();
    for request in db.fetch_open_discover_requests().await? {
        // Items deleted since the request was filed are skipped
        let entry = match request.request_type {
            DiscoverRequestType::Server => {
                let Ok(server) = db.fetch_server(&request.request_id).await else {
                    continue;
                };

                v0::DiscoverQueueEntry {
                    count: server.get_approximate_member_count(db).await as i64,
                    name: server.name,
                    description: server.description,
                    icon: server.icon.map(Into::into),
                    owner: server.owner,
                    request: request.into(),
                }
            }
            DiscoverRequestType::Bot => {
                let (Ok(bot), Ok(bot_user)) = (
                    db.fetch_bot(&request.request_id).await,
                    db.fetch_user(&request.request_id).await,
                ) else {
                    continue;
                };

                v0::DiscoverQueueEntry {
                    count: db.fetch_server_count(&bot.id).await.unwrap_or(0) as i64,
                    name: bot_user.username,
                    description: bot_user
                        .profile
                        .as_ref()
                        .and_then(|profile| profile.public_content())
                        .map(str::to_string),
                    icon: bot_user.avatar.map(Into::into),
                    owner: bot.owner,
                    request: request.into(),
                }
            }
        };

        // Plus members' requests are reviewed first
        let priority = match db.fetch_user(&entry.owner).await {
            Ok(owner) => owner.has_plus().await,
            Err(_) => false,
        };
        entries.push((priority, entry));
    }

    // stable: keeps the queue order within each group
    entries.sort_by_key(|(priority, _)| !priority);
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}
