use revolt_database::{
    util::{reference::Reference, signed_request::derive_secret},
    Database, FieldsBot, PartialBot, User, ValidatedTicket,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::util::interaction_endpoint::{signing_key, verify_endpoint, PURPOSE};

/// The bot, if this person owns it
async fn owned(db: &Database, user: &User, target: Reference<'_>) -> Result<revolt_database::Bot> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    let bot = target.as_bot(db).await?;
    if bot.owner != user.id {
        return Err(create_error!(NotFound));
    }
    // a saved endpoint stays for reinstatement, but nothing changes meanwhile
    if !revolt_database::bot_may_receive(db, &bot).await {
        return Err(create_error!(InvalidOperation));
    }
    Ok(bot)
}

/// # New Interactions Signing Secret
///
/// Makes the secret your server uses to check `X-Zrzur-Signature`; the
/// previous one stops working at once. It is in this response only.
///
/// Requires a validated MFA ticket (`X-MFA-Ticket`) created by the bot owner.
#[openapi(tag = "Bots")]
#[post("/<target>/interactions/secret")]
pub async fn new_secret(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    target: Reference<'_>,
) -> Result<Json<v0::InteractionsSecret>> {
    let mut bot = owned(db, &user, target).await?;
    if validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }
    let key = signing_key().await?;

    let version = bot.interactions_secret_version + 1;
    bot.update(
        db,
        PartialBot {
            interactions_secret_version: Some(version),
            ..Default::default()
        },
        vec![],
    )
    .await?;

    Ok(Json(v0::InteractionsSecret {
        secret: derive_secret(&key, PURPOSE, &bot.id, version),
        version,
    }))
}

/// # Set Interactions Endpoint
///
/// Interactions go to this HTTPS address from now on, instead of the
/// gateway. Make a signing secret first. The address gets two requests before
/// it is saved: one with a wrong signature, which it must refuse, and a
/// signed `Ping`, which it must answer with `{"type":"Pong"}`, in either order.
///
/// Requires a validated MFA ticket (`X-MFA-Ticket`) created by the bot owner:
/// this address receives what people enter into the bot.
#[openapi(tag = "Bots")]
#[put("/<target>/interactions/endpoint", data = "<data>")]
pub async fn set_endpoint(
    db: &State<Database>,
    user: User,
    validated_ticket: Option<ValidatedTicket>,
    target: Reference<'_>,
    data: Json<v0::DataInteractionsEndpoint>,
) -> Result<Json<v0::Bot>> {
    let mut bot = owned(db, &user, target).await?;
    if validated_ticket.is_none() {
        return Err(create_error!(InvalidCredentials));
    }
    if bot.interactions_secret_version == 0 {
        return Err(create_error!(InvalidOperation));
    }

    let url = verify_endpoint(&data.url, &bot).await?;
    bot.update(
        db,
        PartialBot {
            interactions_endpoint: Some(url.to_string()),
            ..Default::default()
        },
        vec![],
    )
    .await?;
    Ok(Json(bot.into()))
}

/// # Remove Interactions Endpoint
///
/// Interactions go back to the gateway.
#[openapi(tag = "Bots")]
#[delete("/<target>/interactions/endpoint")]
pub async fn remove_endpoint(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::Bot>> {
    let mut bot = owned(db, &user, target).await?;
    bot.update(db, PartialBot::default(), vec![FieldsBot::InteractionsEndpoint])
        .await?;
    Ok(Json(bot.into()))
}
