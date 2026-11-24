use revolt_models::v0::InteractionDataOption;

use crate::staff_now;

/// How long a pending desk action can be confirmed, in milliseconds
pub const DESK_PENDING_LIFETIME_MS: i64 = 10 * 60 * 1000;

/// A changing desk command, stored until its Confirm button is pressed.
///
/// Everything the action needs is here, written by the server when the
/// command was used: the button press carries only this document's id.
/// On MongoDB each document also carries `purge_at`, a BSON date copy of
/// `expires_at`, and disappears on its own through the TTL index on it.
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
    /// The command interaction whose first private answer is the card
    pub card_interaction: String,
    /// The card's message, bound at the first press on it: later presses
    /// (after a step-up) must come from the same message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_message: Option<String>,
    /// The card's title and consequence lines, as the server wrote them;
    /// the card the person pressed on must be exactly their rendering
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
    /// The language of the card and the answer (the command's `locale`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// Milliseconds since the epoch: an irreversible action needs a step-up
    /// made after this
    pub created_at: i64,
    /// Milliseconds since the epoch
    pub expires_at: i64,
    /// Executed (once)
    #[serde(default)]
    pub used: bool,
}

impl DeskPending {
    /// The expiry of a pending action created now
    pub fn expiry_from_now() -> i64 {
        staff_now() + DESK_PENDING_LIFETIME_MS
    }

    pub fn is_expired(&self) -> bool {
        staff_now() >= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};

    use super::*;
    #[allow(unused_imports)]
    use crate::AbstractDeskPending;

    fn pending(id: &str, expires_at: i64) -> DeskPending {
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
            locale: Some("ar".into()),
            created_at: 1,
            expires_at,
            used: false,
        }
    }

    /// The card binds once; a claim works once, again after a release, and
    /// never once expired
    #[tokio::test]
    async fn bind_claim_release() {
        database_test!(|db| async move {
            let id = ulid::Ulid::new().to_string();
            let expires_at = DeskPending::expiry_from_now();
            db.insert_desk_pending(&pending(&id, expires_at)).await.unwrap();
            assert!(db.insert_desk_pending(&pending(&id, expires_at)).await.is_err());
            let stored = db.fetch_desk_pending(&id).await.unwrap().unwrap();
            assert_eq!(stored, pending(&id, expires_at));
            assert!(!stored.is_expired());

            assert!(db.bind_desk_pending_card(&id, "m1").await.unwrap());
            assert!(db.bind_desk_pending_card(&id, "m1").await.unwrap());
            assert!(!db.bind_desk_pending_card(&id, "m2").await.unwrap());
            assert!(!db.bind_desk_pending_card("missing", "m1").await.unwrap());

            assert!(db.claim_desk_pending(&id, staff_now()).await.unwrap());
            assert!(!db.claim_desk_pending(&id, staff_now()).await.unwrap());
            db.release_desk_pending(&id).await.unwrap();
            assert!(db.claim_desk_pending(&id, staff_now()).await.unwrap());
            assert!(!db.claim_desk_pending("missing", staff_now()).await.unwrap());
            let stored = db.fetch_desk_pending(&id).await.unwrap().unwrap();
            assert!(stored.used);
            assert_eq!(stored.card_message.as_deref(), Some("m1"));

            // an expired one is never claimed, whatever its flag
            let old = ulid::Ulid::new().to_string();
            db.insert_desk_pending(&pending(&old, staff_now() - 60_000)).await.unwrap();
            assert!(!db.claim_desk_pending(&old, staff_now()).await.unwrap());
        });
    }
}
