use revolt_database::FieldsUser;
use revolt_database::{
    util::reference::Reference, CommunityItemKind, CommunityItemStatus, Database, File,
    PartialUser, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// Stop a text the platform word list matches
async fn refuse_listed_words(db: &Database, text: &str) -> Result<()> {
    match revolt_database::check_platform_words(db, text).await? {
        Some(category) => Err(create_error!(ProfileTextFiltered {
            category: format!("{category:?}"),
        })),
        None => Ok(()),
    }
}

/// # Edit User
///
/// Edit currently authenticated user.
#[openapi(tag = "User Information")]
#[patch("/<target>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    mut user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditUser>,
) -> Result<Json<v0::User>> {
    let data = data.into_inner();
    data.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;

    // Profile look: only known fonts / decorations and #rrggbb colours
    if let Some(style) = &data.style {
        style.validate_style().map_err(|error| create_error!(FailedValidation { error: error.to_string() }))?;
    }

    // Filter out invalid edit fields
    if !user.privileged && (data.badges.is_some() || data.flags.is_some()) {
        return Err(create_error!(NotPrivileged));
    }

    // The platform word list reads what others see of a profile
    let public_text = [
        data.display_name.as_deref(),
        data.pronouns.as_deref(),
        data.status.as_ref().and_then(|status| status.text.as_deref()),
        data.profile.as_ref().and_then(|profile| profile.content.as_deref()),
    ];
    for text in public_text.into_iter().flatten() {
        refuse_listed_words(db, text).await?;
    }

    // If we want to edit a different user than self, ensure we have
    // permissions and subsequently replace the user in question
    if target.id != "@me" && target.id != user.id {
        let target_user = target.as_user(db).await?;
        let is_bot_owner = target_user
            .bot
            .as_ref()
            .map(|bot| bot.owner == user.id)
            .unwrap_or_default();

        if !is_bot_owner && !user.privileged {
            return Err(create_error!(NotPrivileged));
        }

        user = target_user;
    }

    // Badges are granted and revoked through /badges/admin (audited, no
    // self-grant); the raw bitfield can't change them
    if let Some(badges) = data.badges {
        let managed = revolt_database::managed_badge_bits();
        if badges as u32 & managed != user.badges.unwrap_or_default() as u32 & managed {
            return Err(create_error!(InvalidOperation));
        }
    }

    // nobody else may look like Zrzur; a name kept from before this rule
    // may be sent back unchanged
    if data.display_name.as_deref().is_some_and(|name| {
        revolt_database::passes_for_official(name)
            && !user.official
            && user.display_name.as_deref() != Some(name)
    }) {
        return Err(create_error!(InvalidUsername));
    }

    // Exit out early if nothing is changed
    if data.display_name.is_none()
        && data.pronouns.is_none()
        && data.status.is_none()
        && data.profile.is_none()
        && data.avatar.is_none()
        && data.badges.is_none()
        && data.flags.is_none()
        && data.style.is_none()
        && data.remove.is_empty()
    {
        return Ok(Json(user.into_self(false).await));
    }

    // 1. Remove fields from object
    if data.remove.contains(&v0::FieldsUser::Avatar) {
        if let Some(avatar) = &user.avatar {
            db.mark_attachment_as_deleted(&avatar.id).await?;
        }
    }

    if data.remove.contains(&v0::FieldsUser::ProfileBackground) {
        if let Some(profile) = &user.profile {
            if let Some(background) = &profile.background {
                db.mark_attachment_as_deleted(&background.id).await?;
            }
        }
    }

    for field in &data.remove {
        let field: FieldsUser = field.clone().into();
        user.remove_field(&field);
    }

    let mut partial: PartialUser = PartialUser {
        display_name: data.display_name,
        pronouns: data.pronouns,
        badges: data.badges,
        flags: data.flags,
        ..Default::default()
    };

    // An empty style is the same as removing it
    let mut remove = data.remove;
    if let Some(mut style) = data.style {
        // Image ids of community items are filled in here, never by clients
        style.decoration_file = None;
        style.nameplate_file = None;
        style.effect_file = None;
        for (value, file, kind) in [
            (
                style.decoration.clone(),
                &mut style.decoration_file,
                CommunityItemKind::Decoration,
            ),
            (
                style.nameplate.clone(),
                &mut style.nameplate_file,
                CommunityItemKind::Nameplate,
            ),
            (
                style.effect.clone(),
                &mut style.effect_file,
                CommunityItemKind::Effect,
            ),
        ] {
            if let Some(id) = value.as_deref().and_then(v0::community_item_ref) {
                let item = db.fetch_community_item(id).await?;
                if item.kind != kind {
                    return Err(create_error!(InvalidOperation));
                }
                if kind == CommunityItemKind::Effect
                    && !revolt_config::config().await.features.community.effects_enabled
                {
                    return Err(create_error!(InvalidOperation));
                }
                // what the user wears now, so saving other fields keeps it
                let (worn, worn_file) = match (&user.style, &kind) {
                    (Some(s), CommunityItemKind::Decoration) => {
                        (s.decoration.clone(), s.decoration_file.clone())
                    }
                    (Some(s), CommunityItemKind::Nameplate) => {
                        (s.nameplate.clone(), s.nameplate_file.clone())
                    }
                    (Some(s), CommunityItemKind::Effect) => {
                        (s.effect.clone(), s.effect_file.clone())
                    }
                    (None, _) => (None, None),
                };
                if item.status == CommunityItemStatus::Published
                    && !revolt_database::creator_is_suspended(db, &item.creator_id).await?
                {
                    if worn != value {
                        db.record_community_item_use(id, &user.id).await?;
                    }
                    *file = Some(item.file.id);
                } else if worn == value && worn_file.is_some() {
                    // only published designs can be put on
                    *file = worn_file;
                } else {
                    return Err(create_error!(InvalidOperation));
                }
            }
        }

        if style.is_empty() {
            if !remove.contains(&v0::FieldsUser::Style) {
                user.remove_field(&FieldsUser::Style);
                remove.push(v0::FieldsUser::Style);
            }
        } else {
            partial.style = Some(style);
        }
    }

    // 2. Apply new avatar
    if let Some(avatar) = data.avatar {
        partial.avatar = Some(File::use_user_avatar(db, &avatar, &user.id, &user.id).await?);
    }

    // 3. Apply new status
    if let Some(status) = data.status {
        let mut new_status = user.status.take().unwrap_or_default();
        if let Some(text) = status.text {
            new_status.text = Some(text);
        }

        if let Some(presence) = status.presence {
            new_status.presence = Some(presence.into());
        }

        partial.status = Some(new_status);
    }

    // 4. Apply new profile
    if let Some(profile) = data.profile {
        let mut new_profile = user.profile.take().unwrap_or_default();
        if let Some(content) = &profile.content {
            new_profile.content = Some(content.clone());
        }

        if let Some(background) = &profile.background {
            new_profile.background =
                Some(File::use_background(db, background, &user.id, &user.id).await?);
        }

        super::profile_fields::apply(&mut new_profile, &profile)?;
        if profile.links.is_some() {
            for link in &new_profile.links {
                refuse_listed_words(db, link).await?;
            }
        }

        partial.profile = Some(new_profile);
    }

    user.update(db, partial, remove.into_iter().map(Into::into).collect())
    .await?;

    Ok(Json(user.into_self(false).await))
}
