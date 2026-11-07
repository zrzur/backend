use iso8601_timestamp::Timestamp;
use revolt_database::{util::reference::Reference, Database, PartialMember, User};
use revolt_models::v0;
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Accept the rules
///
/// Accept the server's rules (rules screening). Members who joined after
/// screening was switched on can talk once they have accepted.
#[openapi(tag = "Server Members")]
#[post("/<target>/rules/accept")]
pub async fn accept(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Member>> {
    let server = target.as_server(db).await?;
    let mut member = db.fetch_member(&server.id, &user.id).await?;

    if member.accepted_rules_at.is_none() {
        member
            .update(
                db,
                PartialMember {
                    accepted_rules_at: Some(Timestamp::now_utc()),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
    }

    Ok(Json(member.into()))
}
