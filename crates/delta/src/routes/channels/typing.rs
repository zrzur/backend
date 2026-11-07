use revolt_database::{events::client::EventV1, util::reference::Reference, Database, User};
use revolt_result::Result;
use rocket::State;
use rocket_empty::EmptyResponse;

/// # Start Typing
///
/// Show that you are typing in a channel. Clients hide it again after a few
/// seconds or when your message arrives. For bots without a gateway
/// connection; connected clients send `BeginTyping` instead.
#[openapi(tag = "Messaging")]
#[post("/<target>/typing")]
pub async fn start_typing(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;

    revolt_database::util::threads::assert_can_type(db, &user, &channel).await?;

    EventV1::ChannelStartTyping {
        id: channel.id().to_string(),
        user: user.id,
    }
    .p(channel.id().to_string())
    .await;

    Ok(EmptyResponse)
}
