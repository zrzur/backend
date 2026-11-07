use revolt_database::{Database, DiscoverRequestStatus, DiscoverRequestType, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use super::listing::set_discoverable;

/// # Decide Discover request
///
/// Approves, denies, removes or marks a Discover request as under review,
/// listing or unlisting the server/bot accordingly, and can feature an
/// approved listing. Only privileged users may use this endpoint.
#[openapi(tag = "Discover")]
#[post("/requests/<kind>/<id>", data = "<data>")]
pub async fn decide(
    db: &State<Database>,
    user: User,
    kind: &str,
    id: &str,
    data: Json<v0::DataDiscoverDecision>,
) -> Result<EmptyResponse> {
    if !user.privileged {
        return Err(create_error!(NotPrivileged));
    }
    decide_request(db, kind, id, data.into_inner()).await?;
    Ok(EmptyResponse)
}

/// Apply a moderation decision to a request (callers check who may)
pub(crate) async fn decide_request(
    db: &Database,
    kind: &str,
    id: &str,
    data: v0::DataDiscoverDecision,
) -> Result<()> {
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    let request_type = match kind {
        "server" => DiscoverRequestType::Server,
        "bot" => DiscoverRequestType::Bot,
        _ => return Err(create_error!(InvalidOperation)),
    };

    let mut request = db
        .fetch_discover_request_by_item_id(request_type.clone(), id)
        .await?;

    let decided = data.decision.is_some();
    if let Some(decision) = data.decision {
        let (status, discoverable) = match decision {
            v0::DiscoverDecision::Review => (DiscoverRequestStatus::UnderReview, None),
            v0::DiscoverDecision::Approve => {
                (DiscoverRequestStatus::Approved(data.reason), Some(true))
            }
            v0::DiscoverDecision::Deny => (DiscoverRequestStatus::Denied(data.reason), Some(false)),
            v0::DiscoverDecision::Remove => {
                (DiscoverRequestStatus::Removed(data.reason), Some(false))
            }
        };

        if let Some(discoverable) = discoverable {
            set_discoverable(db, request_type.clone(), id, discoverable).await?;
        }

        request.status = status;
    }

    if let Some(featured) = data.featured {
        request.featured = featured;
    }

    // Only listed items stay featured
    if !matches!(request.status, DiscoverRequestStatus::Approved(_)) {
        request.featured = false;
    }

    db.save_discover_request(&request).await?;

    // tell the owner in their private messages (not for "under review")
    let outcome = match &request.status {
        DiscoverRequestStatus::Approved(note) if decided => Some(("discover.approved", note.clone())),
        DiscoverRequestStatus::Denied(note) if decided => Some(("discover.denied", note.clone())),
        DiscoverRequestStatus::Removed(note) if decided => Some(("discover.removed", note.clone())),
        _ => None,
    };
    if let Some((key, note)) = outcome {
        let owner = match request_type {
            DiscoverRequestType::Server => db.fetch_server(id).await.ok().map(|s| (s.owner, s.name)),
            DiscoverRequestType::Bot => match db.fetch_bot(id).await {
                Ok(bot) => Some((bot.owner, db.fetch_user(id).await.map(|u| u.username).unwrap_or_default())),
                Err(_) => None,
            },
        };
        if let Some((owner_id, name)) = owner {
            crate::util::official::discover_decided(db, &owner_id, key, &name, note.as_deref()).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{DiscoverRequestStatus, DiscoverRequestType};
    use rocket::http::{ContentType, Status};

    #[rocket::async_test]
    async fn approve_lists_server_and_requires_privilege() {
        let harness = TestHarness::new().await;
        let (_, owner_session, owner) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        let response = TestHarness::with_session(
            owner_session.clone(),
            harness
                .client
                .put(format!("/servers/{}/discover", server.id))
                .header(ContentType::JSON)
                .body(r##"{"tags":["Gaming","#arabic"],"category":"gaming","language":"ar"}"##),
        )
        .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        let body = r#"{"decision":"Approve","reason":"Welcome!","featured":true}"#;

        // The owner is not privileged and cannot approve their own request
        let response = TestHarness::with_session(
            owner_session,
            harness
                .client
                .post(format!("/discover/requests/server/{}", server.id))
                .header(ContentType::JSON)
                .body(body),
        )
        .await;
        assert_eq!(response.status(), Status::Forbidden);
        drop(response);

        let (_, mod_session, moderator) = harness.new_user().await;
        harness
            .db
            .update_user(
                &moderator.id,
                &revolt_database::PartialUser {
                    privileged: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("Failed to make user privileged");

        let response = TestHarness::with_session(
            mod_session,
            harness
                .client
                .post(format!("/discover/requests/server/{}", server.id))
                .header(ContentType::JSON)
                .body(body),
        )
        .await;
        assert_eq!(response.status(), Status::NoContent);
        drop(response);

        let server = harness.db.fetch_server(&server.id).await.unwrap();
        assert!(server.discoverable);

        let request = harness
            .db
            .fetch_discover_request_by_item_id(DiscoverRequestType::Server, &server.id)
            .await
            .unwrap();
        assert_eq!(
            request.status,
            DiscoverRequestStatus::Approved(Some("Welcome!".to_string()))
        );
        assert!(request.featured);
        assert_eq!(request.tags, vec!["gaming".to_string(), "arabic".to_string()]);
    }
}
