use iso8601_timestamp::{Duration, Timestamp};
use revolt_models::v0::InteractionDataOption;

/// How long a pending desk action can be confirmed
pub const DESK_PENDING_LIFETIME_SECS: i64 = 10 * 60;

/// A changing desk command, stored until its Confirm button is pressed.
///
/// Everything the action needs is here, written by the server when the
/// command was used: the button press carries only this document's id.
/// Documents disappear on their own through the TTL index on `expires_at`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DeskPending {
    #[serde(rename = "_id")]
    pub id: String,
    /// The person who used the command; only they may confirm
    pub actor: String,
    /// Their desk session (`staff_sessions` id) when they used it; a
    /// confirmation from another desk session is refused
    pub session: String,
    /// The desk bot
    pub bot: String,
    /// Registry key of the action (`group.sub` or `sub`)
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The chosen sub-command's options, as the server checked and stored
    /// them on the command interaction
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<InteractionDataOption>,
    /// The command interaction whose private answer is the card
    pub card_interaction: String,
    /// The card's message, bound at the first press on it: later presses
    /// (after a step-up) must come from the same message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_message: Option<String>,
    /// The card's title and consequence lines, as the server wrote them;
    /// the card the person pressed on must still show them
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    /// Executed (once)
    #[serde(default)]
    pub used: bool,
}

impl DeskPending {
    /// The expiry of a pending action created now
    pub fn expiry_from_now() -> Timestamp {
        Timestamp::now_utc()
            .checked_add(Duration::seconds(DESK_PENDING_LIFETIME_SECS))
            .unwrap_or_else(Timestamp::now_utc)
    }

    pub fn is_expired(&self) -> bool {
        Timestamp::now_utc() >= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

    use super::*;
    #[allow(unused_imports)]
    use crate::AbstractDeskPending;

    fn pending(id: &str) -> DeskPending {
        DeskPending {
            id: id.to_string(),
            actor: "actor".into(),
            session: "session".into(),
            bot: "bot".into(),
            action: "test.change".into(),
            target_kind: None,
            target: None,
            options: vec![],
            card_interaction: "interaction".into(),
            card_message: None,
            title: "Title".into(),
            lines: vec!["Line".into()],
            created_at: Timestamp::now_utc(),
            expires_at: DeskPending::expiry_from_now(),
            used: false,
        }
    }

    /// The card binds once; a claim works once, again after a release
    #[tokio::test]
    async fn bind_claim_release() {
        database_test!(|db| async move {
            let id = ulid::Ulid::new().to_string();
            db.insert_desk_pending(&pending(&id)).await.unwrap();
            assert!(db.insert_desk_pending(&pending(&id)).await.is_err());
            let stored = db.fetch_desk_pending(&id).await.unwrap().unwrap();
            assert_eq!(stored, pending(&id).clone_with(&stored));
            assert!(!stored.is_expired());

            assert!(db.bind_desk_pending_card(&id, "m1").await.unwrap());
            assert!(db.bind_desk_pending_card(&id, "m1").await.unwrap());
            assert!(!db.bind_desk_pending_card(&id, "m2").await.unwrap());
            assert!(!db.bind_desk_pending_card("missing", "m1").await.unwrap());

            assert!(db.claim_desk_pending(&id).await.unwrap());
            assert!(!db.claim_desk_pending(&id).await.unwrap());
            db.release_desk_pending(&id).await.unwrap();
            assert!(db.claim_desk_pending(&id).await.unwrap());
            assert!(!db.claim_desk_pending("missing").await.unwrap());
            let stored = db.fetch_desk_pending(&id).await.unwrap().unwrap();
            assert!(stored.used);
            assert_eq!(stored.card_message.as_deref(), Some("m1"));
        });
    }

    impl DeskPending {
        /// This one with the stored times (they round-trip at millisecond precision)
        fn clone_with(mut self, stored: &DeskPending) -> DeskPending {
            self.created_at = stored.created_at;
            self.expires_at = stored.expires_at;
            self
        }
    }
}
