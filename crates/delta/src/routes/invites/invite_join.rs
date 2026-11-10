use iso8601_timestamp::Timestamp;
use revolt_database::{
    util::reference::Reference, Channel, Database, Invite, JoinRequestStatus, Member,
    ServerJoinRequest, User, AMQP,
};
use revolt_models::v0::{self, InviteJoinResponse};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

/// # Join Invite
///
/// Join an invite by its ID
#[openapi(tag = "Invites")]
#[post("/<target>")]
pub async fn join(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::InviteJoinResponse>> {
    if user.bot.is_some() {
        return Err(create_error!(IsBot));
    }

    user.can_acquire_server(db).await?;

    let invite = target.as_invite(db).await?;
    revolt_database::check_vanity_open(db, &invite).await?;

    // Joining a discoverable server by its ID uses an unstored invite, so there's no use to consume
    let is_discoverable_server =
        matches!(&invite, Invite::Server { code, server, .. } if code == server);

    // A use is taken only after every check passed, so a refused join (ban,
    // age, already a member, access mode) never spends one.
    let stored = !is_discoverable_server;

    match &invite {
        Invite::Server { server, vanity, .. } => {
            let server = db.fetch_server(server).await?;

            // A security action: no invite, custom link or Discover join
            if server.invites_paused() {
                return Err(create_error!(InvitesPaused));
            }

            // Mature Servers are for declared adults only
            revolt_database::util::age::assert_can_join_server(db, &user, &server).await?;

            if db.fetch_ban(&server.id, &user.id).await.is_ok() {
                return Err(create_error!(Banned));
            }
            if db.fetch_member(&server.id, &user.id).await.is_ok() {
                return Err(create_error!(AlreadyInServer));
            }

            match server.access.unwrap_or_default() {
                v0::ServerAccess::Open => {}
                // a locked server takes people only through invites made by
                // someone allowed to invite; its custom link is just an address
                v0::ServerAccess::Locked => {
                    if *vanity || is_discoverable_server {
                        return Err(create_error!(InvalidOperation));
                    }
                }
                // an approval server never adds members from a link: it
                // records a request that a reviewer decides on
                // (a request is not a join, so it takes no use of the invite)
                v0::ServerAccess::Approval => {
                    let request = match db.fetch_join_request(&server.id, &user.id).await {
                        // asking again returns the same pending request
                        Ok(existing) if existing.status == JoinRequestStatus::Pending => existing,
                        // after a rejection, wait before asking again
                        Ok(existing) if !existing.cooldown_over(Timestamp::now_utc()) => {
                            return Err(create_error!(InvalidOperation));
                        }
                        _ => {
                            let request = ServerJoinRequest::pending(&server.id, &user.id);
                            db.save_join_request(&request).await?;
                            request
                        }
                    };
                    return Ok(Json(InviteJoinResponse::JoinRequest {
                        request: request.into(),
                    }));
                }
            }

            take_use(db, invite.code(), stored).await?;
            let source = if is_discoverable_server {
                revolt_database::InsightJoinSource::Discover
            } else if *vanity {
                revolt_database::InsightJoinSource::Vanity
            } else {
                revolt_database::InsightJoinSource::Invite
            };
            let (_, channels) = match Member::create_from(db, &server, &user, None, source).await {
                Ok(created) => created,
                Err(error) => {
                    give_back_use(db, invite.code(), stored).await;
                    return Err(error);
                }
            };

            // joins from Discover count for a server in the paid top row
            if is_discoverable_server {
                revolt_database::record_promotion_event(
                    db,
                    &server.id,
                    &user.id,
                    revolt_database::PromotionStat::Joins,
                )
                .await
                .ok();
            }

            Ok(Json(InviteJoinResponse::Server {
                channels: channels.into_iter().map(|c| c.into()).collect(),
                server: server.into(db).await,
            }))
        }
        Invite::Group {
            channel, creator, ..
        } => {
            let mut channel = db.fetch_channel(channel).await?;
            take_use(db, invite.code(), stored).await?;
            if let Err(error) = channel.add_user_to_group(db, amqp, &user, creator).await {
                give_back_use(db, invite.code(), stored).await;
                return Err(error);
            }
            if let Channel::Group { recipients, .. } = &channel {
                Ok(Json(InviteJoinResponse::Group {
                    users: User::fetch_many_ids_as_mutuals(db, &user, recipients).await?,
                    channel: channel.into(),
                }))
            } else {
                unreachable!()
            }
        }
    }
}

/// Take one use of a stored invite, atomically, so two people can't both get
/// the last one. The unstored invite of a Discover server has none to take.
async fn take_use(db: &Database, code: &str, stored: bool) -> Result<()> {
    if stored && db.consume_invite_use(code).await?.is_none() {
        return Err(create_error!(NotFound));
    }
    Ok(())
}

/// Give back a use taken by `take_use` when the join failed after it
async fn give_back_use(db: &Database, code: &str, stored: bool) {
    if stored {
        db.release_invite_use(code).await.ok();
    }
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::PartialServer;
    use revolt_models::v0::InviteJoinResponse;
    use rocket::http::Status;

    #[rocket::async_test]
    async fn success_join_discoverable_server_by_id() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;
        let (_, session, _) = harness.new_user().await;
        let (server, _) = harness.new_server(&owner).await;

        harness
            .db
            .update_server(
                &server.id,
                &PartialServer {
                    discoverable: Some(true),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("Failed to make server discoverable");

        let response = TestHarness::with_session(
            session,
            harness.client.post(format!("/invites/{}", server.id)),
        )
        .await;
        assert_eq!(response.status(), Status::Ok);
        let join: InviteJoinResponse = response.into_json().await.expect("`InviteJoinResponse`");
        match join {
            InviteJoinResponse::Server { server: joined, .. } => assert_eq!(joined.id, server.id),
            _ => unreachable!(),
        }
    }
}
