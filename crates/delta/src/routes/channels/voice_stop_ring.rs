use revolt_database::{
    events::client::RingEndReason,
    util::reference::Reference,
    voice::{announce_ring_end, get_voice_state, stop_ringing, UserVoiceChannel, VoiceClient},
    Channel, Database, User, AMQP,
};
use revolt_result::{create_error, Result};

use rocket::State;
use rocket_empty::EmptyResponse;

/// # Stop Ring
/// Stops ringing a specific user in a DM or group call.
///
/// The person being rung may stop their own ring (decline) without being in
/// the call. Stopping anyone else's ring needs you to be in the call
/// (NotConnected otherwise). Only valid in DM/Group channels, returns
/// NoEffect in servers. Returns NotFound if the user is not in the DM/group.
/// Everyone in the channel gets `VoiceRingEnd`; the person's call
/// notification is replaced.
#[openapi(tag = "Voice")]
#[put("/<target>/end_ring/<target_user>")]
pub async fn stop_ring(
    db: &State<Database>,
    amqp: &State<AMQP>,
    voice: &State<VoiceClient>,
    user: User,
    target: Reference<'_>,
    target_user: Reference<'_>,
) -> Result<EmptyResponse> {
    if !voice.is_enabled() {
        return Err(create_error!(LiveKitUnavailable));
    }

    let channel = target.as_channel(db).await?;
    if channel.server().is_some() {
        return Err(create_error!(NoEffect));
    }

    let members = match channel {
        Channel::DirectMessage { ref recipients, .. } | Channel::Group { ref recipients, .. } => {
            recipients
        }
        _ => return Err(create_error!(NoEffect)),
    };

    if !members.iter().any(|m| m == &user.id) || !members.iter().any(|m| target_user.id == m) {
        return Err(create_error!(NotFound));
    }

    let declining = target_user.id == user.id;
    if !declining
        && get_voice_state(
            &UserVoiceChannel {
                id: channel.id().to_string(),
                server_id: None,
            },
            &user.id,
        )
        .await?
        .is_none()
    {
        return Err(create_error!(NotConnected));
    }

    // only someone still being rung has a ring to end; a second press (or a
    // decline after the call ended) changes nothing
    if stop_ringing(channel.id(), target_user.id).await? {
        announce_ring_end(
            Some(amqp.inner()),
            channel.id(),
            None,
            vec![target_user.id.to_string()],
            if declining {
                RingEndReason::Declined
            } else {
                RingEndReason::Stopped
            },
        )
        .await;
    }

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{
        rocket,
        util::test::{PubSubTestHelper, TestHarness},
    };
    use revolt_database::{
        events::client::{EventV1, RingEndReason},
        voice::{
            is_current_ring, mark_call_answered, start_ringing, stop_ringing,
            take_call_answered, take_ringing,
        },
        Channel, Session, User,
    };
    use rocket::http::{Header, Status};

    struct Dm {
        harness: TestHarness,
        caller: (Session, User),
        callee: (Session, User),
        channel: Channel,
    }

    async fn dm() -> Dm {
        let harness = TestHarness::new().await;
        let (_, caller_session, caller) = harness.new_user().await;
        let (_, callee_session, callee) = harness.new_user().await;
        let channel = Channel::DirectMessage {
            id: ulid::Ulid::new().to_string(),
            active: true,
            recipients: vec![caller.id.clone(), callee.id.clone()],
            last_message_id: None,
            started_by: vec![],
            stopped_by: vec![],
        };
        harness.db.insert_channel(&channel).await.unwrap();
        Dm {
            harness,
            caller: (caller_session, caller),
            callee: (callee_session, callee),
            channel,
        }
    }

    async fn end_ring(dm: &Dm, session: &Session, user: &str) -> (Status, String) {
        let response = dm
            .harness
            .client
            .put(format!("/channels/{}/end_ring/{user}", dm.channel.id()))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        let status = response.status();
        (status, response.into_string().await.unwrap_or_default())
    }

    #[rocket::async_test]
    async fn the_person_rung_can_decline_and_the_caller_hears_it() {
        let dm = dm().await;
        let callee = dm.callee.1.id.clone();
        start_ringing(dm.channel.id(), "R1", &dm.caller.1.id, &[callee.clone()])
            .await
            .unwrap();
        let mut events = PubSubTestHelper::new(dm.channel.id()).await;

        // the callee is not in the call, and may still end their own ring
        let (status, body) = end_ring(&dm, &dm.callee.0, &callee).await;
        assert_eq!(status, Status::NoContent, "{body}");

        let event = events
            .wait_for_event(|event| matches!(event, EventV1::VoiceRingEnd { .. }))
            .await;
        let EventV1::VoiceRingEnd { user, reason, channel_id } = event else {
            unreachable!()
        };
        assert_eq!(user, callee);
        assert_eq!(reason, RingEndReason::Declined);
        assert_eq!(channel_id, dm.channel.id());
        // no longer rung
        assert!(!stop_ringing(dm.channel.id(), &callee).await.unwrap());
    }

    #[rocket::async_test]
    async fn declining_again_changes_nothing() {
        let dm = dm().await;
        let callee = dm.callee.1.id.clone();
        let (status, body) = end_ring(&dm, &dm.callee.0, &callee).await;
        assert_eq!(status, Status::NoContent, "{body}");
    }

    #[rocket::async_test]
    async fn someone_else_needs_to_be_in_the_call_to_stop_a_ring() {
        let dm = dm().await;
        let callee = dm.callee.1.id.clone();
        start_ringing(dm.channel.id(), "R2", &dm.caller.1.id, &[callee.clone()])
            .await
            .unwrap();
        let (status, body) = end_ring(&dm, &dm.caller.0, &callee).await;
        assert_ne!(status, Status::NoContent);
        assert!(body.contains("NotConnected"), "{body}");
        // still rung
        assert!(stop_ringing(dm.channel.id(), &callee).await.unwrap());
    }

    #[rocket::async_test]
    async fn people_outside_the_dm_cannot_end_its_rings() {
        let dm = dm().await;
        let (_, stranger_session, _) = dm.harness.new_user().await;
        let callee = dm.callee.1.id.clone();
        let (status, body) = end_ring(&dm, &stranger_session, &callee).await;
        assert_eq!(status, Status::NotFound, "{body}");
    }

    #[rocket::async_test]
    async fn ring_state_follows_the_call() {
        let channel = ulid::Ulid::new().to_string();
        let users = vec!["A".to_string(), "B".to_string()];
        start_ringing(&channel, "R3", "C", &users).await.unwrap();
        assert!(is_current_ring(&channel, "R3").await.unwrap());
        assert!(stop_ringing(&channel, "A").await.unwrap());
        assert_eq!(take_ringing(&channel).await.unwrap(), vec!["B".to_string()]);
        assert!(take_ringing(&channel).await.unwrap().is_empty());

        // a newer call in the same channel replaces the ring and its timer
        start_ringing(&channel, "R4", "C", &users).await.unwrap();
        assert!(!is_current_ring(&channel, "R3").await.unwrap());

        // answered or not
        assert!(!take_call_answered(&channel).await.unwrap());
        mark_call_answered(&channel).await.unwrap();
        assert!(take_call_answered(&channel).await.unwrap());
        assert!(!take_call_answered(&channel).await.unwrap());
    }
}
