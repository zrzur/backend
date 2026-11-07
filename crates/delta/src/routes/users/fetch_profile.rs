use revolt_database::{
    creator_is_active,
    util::{
        age::{may_see_personal_sections, owner_band},
        bridge::v0::ProfileViewer,
        permissions::DatabasePermissionQuery,
        reference::Reference,
    },
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_user_permissions, UserPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Fetch User Profile
///
/// Retrieve a user's profile data.
///
/// Will fail if you do not have permission to access the other user's profile.
#[openapi(tag = "User Information")]
#[get("/<target>/profile")]
pub async fn profile(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::UserProfile>> {
    if user.id == target.id {
        let creator = creator_is_active(db, &user.id).await?;
        return Ok(Json(v0::UserProfile {
            creator,
            ..user
                .profile
                .unwrap_or_default()
                .for_viewer(ProfileViewer::Owner)
        }));
    }

    let target = target.as_user(db).await?;

    let mut query = DatabasePermissionQuery::new(db, &user).user(&target);
    calculate_user_permissions(&mut query)
        .await
        .throw_if_lacking_user_permission(UserPermission::ViewProfile)?;

    let band = owner_band(db, &target).await;
    let personal = may_see_personal_sections(band, false, target.is_friends_with(&user.id));
    // what a minor made or wears goes no further than their links do
    let creator = personal
        && target.bot.is_none()
        && target.shows_section("creations")
        && creator_is_active(db, &target.id).await?;
    Ok(Json(v0::UserProfile {
        creator,
        ..target
            .profile
            .unwrap_or_default()
            .for_viewer(ProfileViewer::Other { personal })
    }))
}
