use revolt_database::{util::reference::Reference, Channel, Database, Invite};
use revolt_models::v0;
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Fetch Invite
///
/// Fetch an invite by its id.
#[openapi(tag = "Invites")]
#[get("/<target>")]
pub async fn fetch(db: &State<Database>, target: Reference<'_>) -> Result<Json<v0::InviteResponse>> {
    let invite = target.as_invite(db).await?;
    revolt_database::check_vanity_open(db, &invite).await?;

    Ok(Json(match invite {
        Invite::Server {
            channel,
            creator,
            vanity,
            ..
        } => {
            let channel = db.fetch_channel(&channel).await?;
            let user = db.fetch_user(&creator).await?;

            match channel {
                Channel::TextChannel {
                    id,
                    server,
                    name,
                    description,
                    ..
                } => {
                    let server = db.fetch_server(&server).await?;
                    let inside = if revolt_config::config().await.features.join_by_link_enabled {
                        inside_channels(db, &server).await
                    } else {
                        vec![]
                    };

                    v0::InviteResponse::Server {
                        server_description: server.description.clone(),
                        inside,
                        invites_paused: server.invites_paused(),
                        custom_link: vanity,
                        code: target.id.to_string(),
                        member_count: server.get_approximate_member_count(db).await as i64,
                        server_access: server.access,
                        server_id: server.id,
                        server_name: server.name,
                        server_icon: server.icon.map(|f| f.into()),
                        server_banner: server.banner.map(|f| f.into()),
                        server_flags: server.flags,
                        channel_id: id,
                        channel_name: name,
                        channel_description: description,
                        user_name: user.username,
                        user_avatar: user.avatar.map(|f| f.into()),
                    }
                }
                _ => unreachable!(),
            }
        }
        Invite::Group {
            channel, creator, ..
        } => {
            let channel = db.fetch_channel(&channel).await?;
            let user = db.fetch_user(&creator).await?;

            match channel {
                Channel::Group {
                    id,
                    name,
                    description,
                    ..
                } => v0::InviteResponse::Group {
                    code: target.id.to_string(),
                    channel_id: id,
                    channel_name: name,
                    channel_description: description,
                    user_name: user.username,
                    user_avatar: user.avatar.map(|f| f.into()),
                },
                _ => unreachable!(),
            }
        }
    }))
}

/// Up to four channels of a server that everyone in it can see, in the
/// server's own order, by name only. Mature channels and channels that
/// take the view permission away from everyone are left out; a server that
/// hides its channels from everyone by default shows none.
async fn inside_channels(
    db: &Database,
    server: &revolt_database::Server,
) -> Vec<v0::InviteInsideChannel> {
    const VIEW: i64 = revolt_permissions::ChannelPermission::ViewChannel as i64;

    if server.nsfw || server.default_permissions & VIEW == 0 {
        return vec![];
    }
    let Ok(channels) = db.fetch_channels(&server.channels).await else {
        return vec![];
    };

    // the database answers in any order: keep the server's
    server
        .channels
        .iter()
        .filter_map(|id| channels.iter().find(|channel| channel.id() == id))
        .filter_map(|channel| match channel {
            Channel::TextChannel {
                name,
                default_permissions,
                nsfw,
                voice,
                ..
            } if !nsfw
                && default_permissions
                    .as_ref()
                    .is_none_or(|field| field.d & VIEW == 0) =>
            {
                Some(v0::InviteInsideChannel {
                    name: name.clone(),
                    voice: voice.is_some(),
                })
            }
            _ => None,
        })
        .take(4)
        .collect()
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_database::{Channel, Server};
    use revolt_models::v0::{DataCreateGroup, DataCreateInvite, DataCreateServerChannel, Invite, InviteResponse, LegacyServerChannelType};
    use rocket::http::Status;

    #[rocket::async_test]
    async fn success_fetch_group_invite() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;

        let group = Channel::create_group(
            &harness.db,
            DataCreateGroup {
                ..Default::default()
            },
            user.id.clone(),
        )
        .await
        .expect("`Channel`");
        let create_response = TestHarness::with_session(
            session,
            harness
                .client
                .post(format!("/channels/{}/invites", group.id()))
                .json(&DataCreateInvite {
                    max_uses: None,
                    expires: None,
                    never_expires: false,
                }),
        )
        .await;
        assert_eq!(create_response.status(), Status::Ok);
        let invite_from_create: Invite = create_response.into_json().await.expect("`Invite`");
        let invite_code = match invite_from_create {
            Invite::Group { code, .. } => code,
            _ => unreachable!(),
        };
        let response = harness
            .client
            .get(format!("/invites/{}", invite_code))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let invite_response: InviteResponse = response.into_json().await.expect("`FetchInvite`");
        match invite_response {
            InviteResponse::Group {
                code,
                channel_id,
                user_name,
                ..
            } => {
                assert_eq!(code, invite_code);
                assert_eq!(channel_id, group.id());
                assert_eq!(user_name, user.username);
            }
            _ => unreachable!(),
        }
    }

    #[rocket::async_test]
    async fn fail_fetch_missing_invite() {
        let harness = TestHarness::new().await;
        let response = harness
            .client
            .get(format!("/invites/{}", TestHarness::rand_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);
    }

    #[rocket::async_test]
    async fn success_fetch_text_channel_invite() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (_, channels) = harness.new_server(&user).await;
        let channel = channels.first().expect("Server Channel");
        let create_response = TestHarness::with_session(
            session,
            harness
                .client
                .post(format!("/channels/{}/invites", channel.id()))
                .json(&DataCreateInvite {
                    max_uses: None,
                    expires: None,
                    never_expires: false,
                }),
        )
        .await;
        assert_eq!(create_response.status(), Status::Ok);
        let invite_from_create: Invite = create_response.into_json().await.expect("`Invite`");
        let invite_code = match invite_from_create {
            Invite::Server { code, .. } => code,
            _ => unreachable!(),
        };
        let response = harness
            .client
            .get(format!("/invites/{}", invite_code))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let invite_response: InviteResponse = response.into_json().await.expect("`FetchInvite`");
        match invite_response {
            InviteResponse::Server {
                code,
                channel_id,
                user_name,
                ..
            } => {
                assert_eq!(code, invite_code);
                assert_eq!(channel_id, channel.id());
                assert_eq!(user_name, user.username);
            }
            _ => unreachable!(),
        };
    }

    #[rocket::async_test]
    async fn success_fetch_voice_channel_invite() {
        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, _) = harness.new_server(&user).await;
        let server_mut: &mut Server = &mut server.clone();

        let channel = Channel::create_server_channel(
            &harness.db,
            server_mut,
            DataCreateServerChannel {
                channel_type: LegacyServerChannelType::Voice,
                name: "Voice Channel".to_string(),
                description: None,
                nsfw: Some(false),
                voice: None,
                announcement: None,
                category: None,
                private: None,
            },
            true,
        )
        .await
        .expect("Failed to make new channel");
        let create_response = TestHarness::with_session(
            session,
            harness
                .client
                .post(format!("/channels/{}/invites", channel.id()))
                .json(&DataCreateInvite {
                    max_uses: None,
                    expires: None,
                    never_expires: false,
                }),
        )
        .await;
        assert_eq!(create_response.status(), Status::Ok);
        let invite_from_create: Invite = create_response.into_json().await.expect("`Invite`");
        let invite_code = match invite_from_create {
            Invite::Server { code, .. } => code,
            _ => unreachable!(),
        };
        let response = harness
            .client
            .get(format!("/invites/{}", invite_code))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        let invite_response: InviteResponse = response.into_json().await.expect("`FetchInvite`");
        match invite_response {
            InviteResponse::Server {
                code,
                channel_id,
                user_name,
                ..
            } => {
                assert_eq!(code, invite_code);
                assert_eq!(channel_id, channel.id());
                assert_eq!(user_name, user.username);
            }
            _ => unreachable!(),
        };
    }

    /// A server with a text channel "racing", a private channel "staff" and
    /// a voice channel "Lounge" beside its first channel, and an invite
    async fn server_with_channels(harness: &TestHarness) -> (Server, String) {
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let mut server = server;
        for (name, channel_type, private) in [
            ("racing", LegacyServerChannelType::Text, None),
            ("staff", LegacyServerChannelType::Text, Some(true)),
            ("Lounge", LegacyServerChannelType::Voice, None),
        ] {
            Channel::create_server_channel(
                &harness.db,
                &mut server,
                DataCreateServerChannel {
                    channel_type,
                    name: name.to_string(),
                    description: None,
                    nsfw: Some(false),
                    voice: None,
                    announcement: None,
                    category: None,
                    private,
                },
                true,
            )
            .await
            .expect("`Channel`");
        }
        harness
            .db
            .update_server(
                &server.id,
                &revolt_database::PartialServer {
                    description: Some("Races and quiz nights".to_string()),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("description");

        let response = TestHarness::with_session(
            session,
            harness
                .client
                .post(format!("/channels/{}/invites", channels[0].id()))
                .json(&DataCreateInvite {
                    max_uses: None,
                    expires: None,
                    never_expires: false,
                }),
        )
        .await;
        let code = match response.into_json::<Invite>().await.expect("`Invite`") {
            Invite::Server { code, .. } => code,
            _ => unreachable!(),
        };
        (server, code)
    }

    #[rocket::async_test]
    async fn the_page_fields_are_public_and_name_no_private_channel() {
        revolt_config::overwrite_config(|config| config.features.join_by_link_enabled = true).await;
        let harness = TestHarness::new().await;
        let (_, code) = server_with_channels(&harness).await;

        let response = harness.client.get(format!("/invites/{code}")).dispatch().await;
        assert_eq!(response.status(), Status::Ok);
        match response.into_json::<InviteResponse>().await.expect("`InviteResponse`") {
            InviteResponse::Server {
                server_description,
                inside,
                invites_paused,
                custom_link,
                ..
            } => {
                assert_eq!(server_description.as_deref(), Some("Races and quiz nights"));
                let names: Vec<&str> = inside.iter().map(|c| c.name.as_str()).collect();
                assert_eq!(names.len(), 3, "{names:?}");
                assert!(names.contains(&"racing"));
                assert!(!names.contains(&"staff"));
                assert!(inside.iter().any(|c| c.name == "Lounge" && c.voice));
                assert!(inside.iter().all(|c| c.voice == (c.name == "Lounge")));
                assert!(!invites_paused);
                assert!(!custom_link);
            }
            _ => unreachable!(),
        }
    }

    #[rocket::async_test]
    async fn no_channel_is_named_while_the_short_path_is_off() {
        let harness = TestHarness::new().await;
        let (_, code) = server_with_channels(&harness).await;

        let response = harness.client.get(format!("/invites/{code}")).dispatch().await;
        match response.into_json::<InviteResponse>().await.expect("`InviteResponse`") {
            InviteResponse::Server { inside, server_description, .. } => {
                assert!(inside.is_empty());
                // the description is not a secret: Discover and the join dialog show it
                assert_eq!(server_description.as_deref(), Some("Races and quiz nights"));
            }
            _ => unreachable!(),
        }
    }

    #[rocket::async_test]
    async fn a_paused_server_says_so_on_its_invite() {
        let harness = TestHarness::new().await;
        let (server, code) = server_with_channels(&harness).await;
        harness
            .db
            .update_server(
                &server.id,
                &revolt_database::PartialServer {
                    invites_paused_until: Some(
                        iso8601_timestamp::Timestamp::now_utc()
                            + iso8601_timestamp::Duration::hours(1),
                    ),
                    ..Default::default()
                },
                vec![],
            )
            .await
            .expect("pause");

        let response = harness.client.get(format!("/invites/{code}")).dispatch().await;
        match response.into_json::<InviteResponse>().await.expect("`InviteResponse`") {
            InviteResponse::Server { invites_paused, .. } => assert!(invites_paused),
            _ => unreachable!(),
        }
    }
}
