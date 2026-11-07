use iso8601_timestamp::Timestamp;
use revolt_database::{
    active_promotions, epoch_ms, note_discover_browser, record_promotion_event, Database,
    DiscoverRequestType, PromotionStat, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;
use validator::Validate;

use super::listing::{bot_card, ranked_bots};

/// # List Discover bots
///
/// Lists public bots approved for Discover, boosted ones at or above their
/// guaranteed place. Public: also used by the standalone Discover website,
/// so no session is required.
#[openapi(tag = "Discover")]
#[get("/bots?<options..>")]
pub async fn list_bots(
    db: &State<Database>,
    user: Option<User>,
    options: v0::OptionsDiscoverQuery,
) -> Result<Json<Vec<v0::DiscoverBot>>> {
    options.validate().map_err(|error| revolt_database::util::validation::validation_failed(&error))?;
    if let Some(user) = user.filter(|user| user.bot.is_none()) {
        note_discover_browser(&user.id).await;
    }

    Ok(Json(
        ranked_bots(db, Some(&options))
            .await?
            .into_iter()
            .skip(options.offset.unwrap_or(0) as usize)
            .take(options.limit.unwrap_or(50) as usize)
            .collect(),
    ))
}

/// # Fetch Discover bot
///
/// Fetches a single bot listed on Discover.
#[openapi(tag = "Discover")]
#[get("/bots/<id>")]
pub async fn fetch_bot(
    db: &State<Database>,
    _user: Option<User>,
    id: &str,
) -> Result<Json<v0::DiscoverBot>> {
    let bot = db.fetch_bot(id).await?;
    if !bot.discoverable || !bot.public {
        return Err(create_error!(NotFound));
    }

    let user = db.fetch_user(id).await?;
    let request = db
        .fetch_discover_request_by_item_id(DiscoverRequestType::Bot, id)
        .await
        .ok();
    let mut card = bot_card(db, &bot, user, request.as_ref()).await;
    if let Some(promotion) = active_promotions(db).await?.remove(&card.id) {
        card.boost = promotion.boost.map(|(kind, _)| kind);
        card.promoted = promotion.promoted.is_some();
    }
    Ok(Json(card))
}

/// # Discover top row for bots
///
/// Bots in the paid top row, oldest placement first, and how many places
/// are free. Bots that aren't listed any more don't show.
#[openapi(tag = "Discover")]
#[get("/promoted/bots")]
pub async fn promoted_bots(
    db: &State<Database>,
    _user: Option<User>,
) -> Result<Json<v0::DiscoverPromotedBots>> {
    let billing = revolt_config::config().await.features.billing;
    let now = epoch_ms(Timestamp::now_utc());
    let promotions = active_promotions(db).await?;

    let mut placed: Vec<_> = promotions
        .values()
        .filter_map(|promotion| promotion.promoted.clone())
        .filter(|item| item.ends_at > now && item.target_kind == "bot")
        .collect();
    placed.sort_by_key(|item| item.started_at);
    let taken = placed.len();

    let mut bots = Vec::new();
    for item in placed {
        let (Ok(bot), Ok(user)) = (
            db.fetch_bot(&item.server_id).await,
            db.fetch_user(&item.server_id).await,
        ) else {
            continue;
        };
        if !bot.discoverable || !bot.public {
            continue;
        }
        let request = db
            .fetch_discover_request_by_item_id(DiscoverRequestType::Bot, &bot.id)
            .await
            .ok();
        let mut card = bot_card(db, &bot, user, request.as_ref()).await;
        card.promoted = true;
        card.boost = promotions
            .get(&card.id)
            .and_then(|promotion| promotion.boost.as_ref().map(|(kind, _)| kind.clone()));
        bots.push(card);
    }

    let slots = if billing.available() {
        billing.discover.promoted_slots
    } else {
        0
    };
    Ok(Json(v0::DiscoverPromotedBots {
        slots: slots as u32,
        open: slots.saturating_sub(taken) as u32,
        bots,
    }))
}

/// # Discover bot seen or opened
///
/// Counts a view of a top-row bot or a click on it, once a day per
/// person. Other bots are ignored.
#[openapi(tag = "Discover")]
#[post("/bots/<id>/events", data = "<data>")]
pub async fn bot_event(
    db: &State<Database>,
    user: User,
    id: &str,
    data: Json<v0::DataDiscoverEvent>,
) -> Result<EmptyResponse> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let stat = match data.kind.as_str() {
        "view" => PromotionStat::Views,
        "click" => PromotionStat::Clicks,
        _ => return Err(create_error!(InvalidOperation)),
    };
    record_promotion_event(db, id, &user.id, stat).await?;
    Ok(EmptyResponse)
}

