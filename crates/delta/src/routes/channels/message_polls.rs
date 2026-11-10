//! Polls: vote, take a vote back, list who chose an answer, end early.
//!
//! Voting needs what reacting needs (it is the same kind of light answer to
//! a message): View Channel, Read Message History and React, in a channel
//! the user may write in (verification level, read-only threads) and may see
//! (mature channels). Reading voters needs only to be able to read the
//! message. The author ends their own poll; others need Manage Messages.
use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Channel, Database, Message, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// Channel, permissions and the poll message, after the checks every poll
/// route shares: the user can read the channel and its history, and (mature
/// channels) is old enough
async fn poll_message(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
    msg: &Reference<'_>,
) -> Result<(Channel, PermissionValue, Message)> {
    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;
    revolt_database::util::age::assert_can_view_channel(db, user, &channel).await?;

    let message = msg.as_message_in_channel(db, channel.id()).await?;
    if message.poll.is_none() {
        return Err(create_error!(NotFound));
    }
    Ok((channel, permissions, message))
}

/// Checks before a vote changes: people only, React, a channel the user may
/// write in, an open poll and an answer it has
async fn check_vote(
    db: &Database,
    user: &User,
    channel: &Channel,
    permissions: &PermissionValue,
    message: &Message,
    answer: u32,
) -> Result<()> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }
    permissions.throw_if_lacking_channel_permission(ChannelPermission::React)?;
    revolt_database::util::threads::assert_not_read_only(db, channel).await?;
    revolt_database::util::verification::assert_can_talk(db, user, channel).await?;

    let poll = message.poll.as_ref().ok_or_else(|| create_error!(NotFound))?;
    if poll.is_closed_at(Timestamp::now_utc()) {
        // past its end but not yet closed by crond: close it now
        if !poll.ended {
            message.end_poll(db).await?;
        }
        return Err(create_error!(PollEnded));
    }
    if poll.answer(answer).is_none() {
        return Err(create_error!(NotFound));
    }
    Ok(())
}

/// # Vote on a Poll
///
/// Choose an answer of the poll in this message. On a single-choice poll it
/// replaces your earlier choice; on a multiple-choice poll it is added to
/// it. Needs React. Returns the poll with the new counts and your choice.
#[openapi(tag = "Interactions")]
#[put("/<target>/messages/<msg>/polls/answers/<answer>/votes/@me")]
pub async fn vote(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    answer: u32,
) -> Result<Json<v0::Poll>> {
    let (channel, permissions, message) = poll_message(db, &user, &target, &msg).await?;
    check_vote(db, &user, &channel, &permissions, &message, answer).await?;

    let poll = message.poll.as_ref().expect("checked above");
    let choice = poll.choice_with(&user.id, answer);
    if choice == poll.choices_of(&user.id) {
        return Ok(Json(message.poll_for(&user.id).expect("checked above")));
    }

    let updated = message
        .set_poll_choice(db, &user.id, choice, answer, true)
        .await?;
    updated
        .poll_for(&user.id)
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}

/// # Remove a Poll Vote
///
/// Take back your choice of an answer of the poll in this message. Returns
/// the poll with the new counts and your choice.
#[openapi(tag = "Interactions")]
#[delete("/<target>/messages/<msg>/polls/answers/<answer>/votes/@me")]
pub async fn unvote(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    answer: u32,
) -> Result<Json<v0::Poll>> {
    let (channel, permissions, message) = poll_message(db, &user, &target, &msg).await?;
    check_vote(db, &user, &channel, &permissions, &message, answer).await?;

    let poll = message.poll.as_ref().expect("checked above");
    let before = poll.choices_of(&user.id);
    if !before.contains(&answer) {
        return Ok(Json(message.poll_for(&user.id).expect("checked above")));
    }

    let choice = poll.choice_without(&user.id, answer);
    let updated = message
        .set_poll_choice(db, &user.id, choice, answer, false)
        .await?;
    updated
        .poll_for(&user.id)
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}

/// # Fetch Poll Voters
///
/// People who chose an answer, oldest vote first, a page at a time (up to
/// 100, default 50; pass the last id as `after` for the next page). Anyone
/// who can read the message can see them.
#[openapi(tag = "Interactions")]
#[get("/<target>/messages/<msg>/polls/answers/<answer>/voters?<options..>")]
pub async fn voters(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    answer: u32,
    options: v0::OptionsPollVoters,
) -> Result<Json<v0::PollVoters>> {
    let (channel, _, message) = poll_message(db, &user, &target, &msg).await?;
    let poll = message.poll.as_ref().expect("checked above");
    let answer = poll.answer(answer).ok_or_else(|| create_error!(NotFound))?;

    let limit = options.limit.unwrap_or(50).clamp(1, 100) as usize;
    let start = match &options.after {
        Some(after) => answer
            .voters
            .get_index_of(after)
            .map(|index| index + 1)
            .ok_or_else(|| create_error!(NotFound))?,
        None => 0,
    };
    let ids: Vec<String> = answer
        .voters
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect();

    let users = User::fetch_many_ids_as_mutuals(db, &user, &ids).await?;
    let members = match channel.server() {
        Some(server) => Some(
            db.fetch_members(server, &ids)
                .await?
                .into_iter()
                .map(Into::into)
                .collect(),
        ),
        None => None,
    };

    Ok(Json(v0::PollVoters {
        ids,
        users,
        members,
        count: answer.voters.len() as u32,
    }))
}

/// # End a Poll
///
/// Close the poll in this message now: votes stop, the results are final
/// and a "poll ended" reply is posted. The author can end their own poll;
/// anyone else needs Manage Messages.
#[openapi(tag = "Interactions")]
#[post("/<target>/messages/<msg>/polls/end")]
pub async fn end(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
) -> Result<Json<v0::Poll>> {
    let (_, permissions, message) = poll_message(db, &user, &target, &msg).await?;
    if message.author != user.id {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageMessages)?;
    }

    let poll = message.poll.as_ref().expect("checked above");
    if poll.ended {
        return Err(create_error!(PollEnded));
    }
    message.end_poll(db).await?;

    let updated = db.fetch_message(&message.id).await?;
    updated
        .poll_for(&user.id)
        .map(Json)
        .ok_or_else(|| create_error!(NotFound))
}
