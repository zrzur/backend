use revolt_database::{
    events::client::EventV1,
    util::reference::Reference,
    voice::{get_voice_state, set_self_muted, update_voice_state, UserVoiceChannel},
    Database, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Result};

use rocket::{serde::json::Json, State};
use rocket_empty::EmptyResponse;

/// # Update Own Voice State
///
/// Tell the others in the voice channel that you muted or unmuted your
/// microphone, or deafened or undeafened yourself. The voice server does not
/// report a mute (only a track that stops), so people looking at the channel
/// from outside the call would otherwise never see it.
///
/// This is how you are shown, nothing more: whether you can be heard is
/// still decided by your permissions on the voice server. You must be in the
/// call (NotConnected otherwise).
#[openapi(tag = "Voice")]
#[patch("/<target>/voice_state", data = "<data>")]
pub async fn update(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataUpdateOwnVoiceState>,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;
    let voice_channel = UserVoiceChannel::from_channel(&channel);
    if get_voice_state(&voice_channel, &user.id).await?.is_none() {
        return Err(create_error!(NotConnected));
    }

    let v0::DataUpdateOwnVoiceState { muted, deafened } = data.into_inner();
    let partial = v0::PartialUserVoiceState {
        is_publishing: muted.map(|muted| !muted),
        is_receiving: deafened.map(|deafened| !deafened),
        ..Default::default()
    };
    if partial.is_publishing.is_none() && partial.is_receiving.is_none() {
        return Ok(EmptyResponse);
    }

    // remembered, so a late "microphone published" from the voice server
    // does not show the person unmuted again
    if let Some(muted) = muted {
        set_self_muted(&voice_channel, &user.id, muted).await?;
    }
    update_voice_state(&voice_channel, &user.id, &partial).await?;
    EventV1::UserVoiceStateUpdate {
        id: user.id.clone(),
        channel_id: channel.id().to_string(),
        data: partial,
    }
    .p(channel.id().to_string())
    .await;

    Ok(EmptyResponse)
}

#[cfg(test)]
mod test {
    use crate::{
        rocket,
        util::test::{PubSubTestHelper, TestHarness},
    };
    use revolt_database::{
        events::client::EventV1,
        iso8601_timestamp::Timestamp,
        voice::{create_voice_state, get_voice_state, update_voice_state_tracks, UserVoiceChannel},
        Channel, Member,
    };
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn muting_yourself_is_shown_to_the_channel() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (mut server, _) = harness.new_server(&user).await;
        let channel = Channel::create_server_channel(
            &harness.db,
            &mut server,
            v0::DataCreateServerChannel {
                channel_type: v0::LegacyServerChannelType::Voice,
                name: "Voice".to_string(),
                description: None,
                nsfw: Some(false),
                voice: Some(v0::VoiceInformation {
                    max_users: None,
                    stage: false,
                ..Default::default()
                }),
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .unwrap();
        Member::create(&harness.db, &server, &user, None).await.unwrap();
        let vc = UserVoiceChannel::from_channel(&channel);
        create_voice_state(&vc, &user.id, Timestamp::now_utc())
            .await
            .unwrap();
        let mut events = PubSubTestHelper::new(channel.id()).await;

        let response = harness
            .client
            .patch(format!("/channels/{}/voice_state", channel.id()))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(json!({ "muted": true, "deafened": true }).to_string())
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NoContent);

        let event = events
            .wait_for_event(|event| matches!(event, EventV1::UserVoiceStateUpdate { .. }))
            .await;
        let EventV1::UserVoiceStateUpdate { id, data, .. } = event else {
            unreachable!()
        };
        assert_eq!(id, user.id);
        assert_eq!(data.is_publishing, Some(false));
        assert_eq!(data.is_receiving, Some(false));
        let state = get_voice_state(&vc, &user.id).await.unwrap().unwrap();
        assert!(!state.is_publishing && !state.is_receiving);

        // the voice server's late "microphone published" keeps the mute
        let partial = update_voice_state_tracks(&vc, &user.id, true, 2).await.unwrap();
        assert_eq!(partial.is_publishing, Some(false));
    }

    #[rocket::async_test]
    async fn only_people_in_the_call_have_a_voice_state_to_change() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (_, channels) = harness.new_server(&user).await;
        let response = harness
            .client
            .patch(format!("/channels/{}/voice_state", channels[0].id()))
            .header(ContentType::JSON)
            .header(Header::new("x-session-token", session.token.to_string()))
            .body(json!({ "muted": true }).to_string())
            .dispatch()
            .await;
        assert_ne!(response.status(), Status::NoContent);
        assert!(response.into_string().await.unwrap_or_default().contains("NotConnected"));
    }
}
