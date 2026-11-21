use std::sync::Arc;

use crate::utils::Consumer;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use base64::{
    engine::{self},
    Engine as _,
};
use lapin::{message::Delivery, Channel as AMQPChannel, Connection};
use revolt_database::{
    events::rabbit::*, util::format_display_name, Database, WebPushSubscription,
};
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, SubscriptionKeys, VapidSignatureBuilder,
    WebPushClient, WebPushError, WebPushMessageBuilder,
};

#[derive(Clone)]
#[allow(unused)]
pub struct VapidOutboundConsumer {
    db: Database,
    connection: Arc<Connection>,
    channel: Arc<AMQPChannel>,
    client: IsahcWebPushClient,
    pkey: Arc<Vec<u8>>,
}

#[async_trait]
impl Consumer for VapidOutboundConsumer {
    async fn create(
        db: Database,
        connection: Arc<Connection>,
        channel: Arc<AMQPChannel>,
    ) -> Self {
        let config = revolt_config::config().await;

        if config.pushd.vapid.private_key.is_empty() || config.pushd.vapid.public_key.is_empty() {
            panic!("no Vapid keys present");
        }

        let web_push_private_key = Arc::new(
            engine::general_purpose::URL_SAFE_NO_PAD
                .decode(config.pushd.vapid.private_key)
                .expect("valid `VAPID_PRIVATE_KEY`"),
        );

        Self {
            db,
            connection,
            channel,
            client: IsahcWebPushClient::new().unwrap(),
            pkey: web_push_private_key,
        }
    }

    fn channel(&self) -> &Arc<AMQPChannel> {
        &self.channel
    }

    async fn consume(&self, delivery: Delivery) -> Result<()> {
        let payload: PayloadToService = serde_json::from_slice(&delivery.data)?;

        let endpoint = payload
            .extras
            .get("endpoint")
            .ok_or_else(|| anyhow!("missing endpoint"))?
            .clone();

        // Saved before endpoints were checked: never send to it, and drop it
        if !WebPushSubscription::web_endpoint_is_allowed(&endpoint) {
            if let Err(err) = self
                .db
                .remove_push_subscription_by_session_id(&payload.session_id)
                .await
            {
                revolt_config::capture_error(&err);
            }
            return Ok(());
        }

        let subscription = SubscriptionInfo {
            endpoint,
            keys: SubscriptionKeys {
                auth: payload.token,
                p256dh: payload
                    .extras
                    .get("p256dh")
                    .ok_or_else(|| anyhow!("missing p256dh"))?
                    .clone(),
            },
        };

        let payload_body = match payload.notification {
            PayloadKind::FRReceived(alert) => {
                // before the name takes its display name out of the user
                let icon = avatar_url(&alert.from_user).await;
                let name = alert
                    .from_user
                    .display_name
                    .or(Some(revolt_models::v0::user_handle(&alert.from_user.username, &alert.from_user.discriminator)))
                    .clone()
                    .ok_or_else(|| anyhow!("missing name"))?;

                friend_push_payload(
                    &format!("{} sent you a friend request", name),
                    &alert.from_user.id,
                    Some(icon),
                )?
            }
            PayloadKind::FRAccepted(alert) => {
                // before the name takes its display name out of the user
                let icon = avatar_url(&alert.accepted_user).await;
                let name = alert
                    .accepted_user
                    .display_name
                    .or(Some(revolt_models::v0::user_handle(&alert.accepted_user.username, &alert.accepted_user.discriminator)))
                    .clone()
                    .ok_or_else(|| anyhow!("missing name"))?;

                friend_push_payload(
                    &format!("{} accepted your friend request", name),
                    &alert.accepted_user.id,
                    Some(icon),
                )?
            }
            PayloadKind::Generic(alert) => serde_json::to_string(&alert)?,
            PayloadKind::MessageNotification(alert) => serde_json::to_string(&alert)?,
            PayloadKind::DmCallStartEnd(alert) => {
                let channel = self.db.fetch_channel(&alert.channel_id).await?;
                let initiator_name =
                    format_display_name(&self.db, &alert.initiator_id, channel.server()).await?;
                let group = match &channel {
                    revolt_database::Channel::DirectMessage { .. } => None,
                    revolt_database::Channel::Group { name, .. } => Some(name.clone()),
                    _ => bail!("Invalid DmCallStart/End channel type"),
                };

                call_push_payload(
                    &initiator_name,
                    group.as_deref(),
                    &alert.channel_id,
                    alert.ended,
                    alert.reason,
                )?
            }
            PayloadKind::BadgeUpdate(_) => {
                bail!("Vapid cannot handle badge updates and they should not be sent here.");
            }
        };

        let signature = VapidSignatureBuilder::from_pem(
            std::io::Cursor::new(self.pkey.as_ref()),
            &subscription,
        )?
        .build()?;

        let mut builder = WebPushMessageBuilder::new(&subscription);
        builder.set_vapid_signature(signature);

        builder.set_payload(ContentEncoding::AesGcm, payload_body.as_bytes());

        let msg = builder.build()?;

        match self.client.send(msg).await {
            Err(WebPushError::Unauthorized) => {
                if let Err(err) = self
                    .db
                    .remove_push_subscription_by_session_id(&payload.session_id)
                    .await
                {
                    revolt_config::capture_error(&err);
                }
            }
            res => {
                res?;
            }
        };

        Ok(())
    }
}

/// The link a friend-request notification opens: the Friends page, which
/// opens this person's profile. Relative: the service worker resolves it
/// against its own origin and opens nothing outside it.
pub(crate) fn friend_profile_url(user_id: &str) -> String {
    format!("/friends?profile={user_id}")
}

/// The picture of a person in a push, as message pushes have it
async fn avatar_url(user: &revolt_database::User) -> String {
    let config = revolt_config::config().await;
    match &user.avatar {
        Some(avatar) => format!("{}/avatars/{}", config.hosts.autumn, avatar.id),
        None => format!("{}/users/{}/default_avatar", config.hosts.api, user.id),
    }
}

/// The web push for a friend request, received or accepted. It used to carry
/// only `body`, so a click opened the app's front page; now it opens the
/// person's profile. One notification per person: a newer one replaces it.
fn friend_push_payload(body: &str, user_id: &str, icon: Option<String>) -> Result<String> {
    let mut payload = serde_json::Map::new();
    payload.insert("title".into(), "Zrzur".into());
    payload.insert("body".into(), body.into());
    payload.insert("url".into(), friend_profile_url(user_id).into());
    payload.insert("tag".into(), format!("friend-{user_id}").into());
    if let Some(icon) = icon {
        payload.insert("icon".into(), icon.into());
    }
    Ok(serde_json::to_string(&payload)?)
}

/// The web push for a DM or group call. Ringing and its end share a tag, so
/// the end replaces the "is calling you" notification instead of adding a
/// second one (it used to repeat "is calling you" when the call ended).
fn call_push_payload(
    initiator_name: &str,
    group: Option<&str>,
    channel_id: &str,
    ended: bool,
    reason: Option<revolt_database::events::client::RingEndReason>,
) -> Result<String> {
    use revolt_database::events::client::RingEndReason;

    let body = match (ended, reason, group) {
        (false, _, None) => format!("{initiator_name} is calling you"),
        (false, _, Some(group)) => format!("{initiator_name} is calling your group, {group}"),
        (true, Some(RingEndReason::Missed), _) => format!("Missed call from {initiator_name}"),
        (true, Some(RingEndReason::Declined), _) => {
            format!("You declined the call from {initiator_name}")
        }
        (true, Some(RingEndReason::Answered), _) => {
            format!("Call with {initiator_name} answered on another device")
        }
        (true, _, _) => format!("The call from {initiator_name} ended"),
    };

    let mut payload = serde_json::Map::new();
    payload.insert("title".into(), group.unwrap_or(initiator_name).into());
    payload.insert("body".into(), body.into());
    payload.insert("tag".into(), format!("call-{channel_id}").into());
    payload.insert("url".into(), format!("/channel/{channel_id}").into());
    payload.insert(
        "call".into(),
        if ended { "ended" } else { "ringing" }.into(),
    );
    Ok(serde_json::to_string(&payload)?)
}

#[cfg(test)]
mod friend_tests {
    use super::friend_push_payload;

    const ID: &str = "01HZX5Q2N7V3K8M4P6R9T0W1YA";

    fn payload(icon: Option<&str>) -> serde_json::Value {
        serde_json::from_str(
            &friend_push_payload("Lina sent you a friend request", ID, icon.map(String::from))
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_friend_request_opens_the_profile() {
        let push = payload(None);
        assert_eq!(push["url"], format!("/friends?profile={ID}"));
        // relative: the service worker keeps it on its own origin
        assert!(push["url"].as_str().unwrap().starts_with('/'));
    }

    #[test]
    fn the_old_body_stays_and_a_title_and_tag_come_with_it() {
        let push = payload(None);
        assert_eq!(push["body"], "Lina sent you a friend request");
        assert_eq!(push["title"], "Zrzur");
        assert_eq!(push["tag"], format!("friend-{ID}"));
        assert!(push.get("icon").is_none());
    }

    #[test]
    fn the_icon_is_the_persons_picture_when_given() {
        let push = payload(Some("https://cdn.example/avatars/A1"));
        assert_eq!(push["icon"], "https://cdn.example/avatars/A1");
    }
}

#[cfg(test)]
mod tests {
    use super::call_push_payload;
    use revolt_database::events::client::RingEndReason;

    fn payload(ended: bool, reason: Option<RingEndReason>, group: Option<&str>) -> serde_json::Value {
        serde_json::from_str(&call_push_payload("Lina", group, "C1", ended, reason).unwrap())
            .unwrap()
    }

    #[test]
    fn ringing_and_its_end_share_one_notification() {
        let ringing = payload(false, None, None);
        assert_eq!(ringing["body"], "Lina is calling you");
        assert_eq!(ringing["call"], "ringing");
        let ended = payload(true, Some(RingEndReason::Missed), None);
        assert_eq!(ended["tag"], ringing["tag"]);
        assert_eq!(ended["url"], "/channel/C1");
        assert_eq!(ended["call"], "ended");
    }

    #[test]
    fn the_end_never_says_calling_again() {
        for reason in [
            None,
            Some(RingEndReason::Missed),
            Some(RingEndReason::Declined),
            Some(RingEndReason::Answered),
            Some(RingEndReason::Stopped),
            Some(RingEndReason::Ended),
        ] {
            let body = payload(true, reason, Some("Team"))["body"].to_string();
            assert!(!body.contains("calling"), "{reason:?}: {body}");
        }
        assert_eq!(payload(true, Some(RingEndReason::Missed), None)["body"], "Missed call from Lina");
    }

    #[test]
    fn a_group_call_names_the_group() {
        let ringing = payload(false, None, Some("Team"));
        assert_eq!(ringing["title"], "Team");
        assert_eq!(ringing["body"], "Lina is calling your group, Team");
    }
}
