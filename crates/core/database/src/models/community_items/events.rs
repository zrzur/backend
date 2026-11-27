//! Live updates for the public Designs gallery. Designs are public to
//! everyone signed in, so every (non-bot) session follows one shared topic;
//! only changes the gallery can see are sent, which keeps traffic small.

use crate::{events::client::EventV1, CommunityItem, CommunityItemStatus, Database};

/// The topic every signed-in person's session follows for design changes
pub const DESIGNS_TOPIC: &str = "designs";

fn is_public(item: Option<&CommunityItem>) -> bool {
    item.is_some_and(|item| item.status == CommunityItemStatus::Published)
}

/// Whether a public design changed in a way the gallery shows
fn shows_differently(before: &CommunityItem, after: &CommunityItem) -> bool {
    before.name != after.name
        || before.description != after.description
        || before.tags != after.tags
        || before.file.id != after.file.id
        || before.revision != after.revision
        || before.published_at != after.published_at
        || before.popularity != after.popularity
        || before.kind != after.kind
        || before.creator_id != after.creator_id
}

/// What an open gallery has to hear about a design going from `before` to
/// `after` (`None` for "did not exist"), if anything. Only the public view
/// of a design travels: no reports, notes, pending revisions or rights.
pub fn community_change_event(
    before: Option<&CommunityItem>,
    after: Option<&CommunityItem>,
) -> Option<EventV1> {
    match (is_public(before), is_public(after)) {
        (false, true) => Some(EventV1::CommunityItemCreate(
            after?.clone().into_v0(false, false),
        )),
        (true, true) => {
            let (before, after) = (before?, after?);
            shows_differently(before, after)
                .then(|| EventV1::CommunityItemUpdate(after.clone().into_v0(false, false)))
        }
        (true, false) => Some(EventV1::CommunityItemDelete {
            id: before?.id.clone(),
        }),
        (false, false) => None,
    }
}

/// Tell every open gallery about a design change. A suspended creator's
/// designs are hidden from the gallery, so they never arrive; leaving it is
/// still sent. A failure here never undoes the change.
pub async fn announce_community_change(
    db: &Database,
    before: Option<&CommunityItem>,
    after: Option<&CommunityItem>,
) {
    let Some(event) = community_change_event(before, after) else {
        return;
    };
    if let EventV1::CommunityItemCreate(item) | EventV1::CommunityItemUpdate(item) = &event {
        match crate::creator_is_suspended(db, &item.creator_id).await {
            Ok(false) => {}
            Ok(true) => return,
            Err(error) => {
                log::error!("design event for {}: {error:?}", item.id);
                return;
            }
        }
    }
    event.p(DESIGNS_TOPIC.to_string()).await;
}

/// Save a changed design if nobody changed it since `before` was read, and
/// tell open galleries. `false` when someone else changed it first.
pub async fn save_community_item(
    db: &Database,
    before: &CommunityItem,
    after: &CommunityItem,
    clear_reports: bool,
) -> revolt_result::Result<bool> {
    if !db
        .update_community_item(after, before.version, clear_reports)
        .await?
    {
        return Ok(false);
    }
    announce_community_change(db, Some(before), Some(after)).await;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommunityItemKind, File, Metadata};

    fn item(status: CommunityItemStatus) -> CommunityItem {
        CommunityItem {
            id: "01DESIGN".to_string(),
            kind: CommunityItemKind::Decoration,
            name: "Crown".to_string(),
            creator_id: "01CREATOR".to_string(),
            file: File {
                id: "file1".to_string(),
                tag: "decorations".to_string(),
                filename: "crown.png".to_string(),
                hash: None,
                uploaded_at: None,
                uploader_id: Some("01CREATOR".to_string()),
                used_for: None,
                deleted: None,
                reported: None,
                purged: None,
                metadata: Metadata::Image {
                    width: 128,
                    height: 128,
                    thumbhash: None,
                    animated: None,
                },
                content_type: "image/png".to_string(),
                size: 1024,
                message_id: None,
                user_id: None,
                server_id: None,
                object_id: None,
                forwarded_from: None,
            },
            status,
            description: Some("shiny".to_string()),
            tags: vec!["gold".to_string()],
            revision: 1,
            published_at: Some(1_700_000_000_000),
            reviewed_by: Some("01STAFF".to_string()),
            review_note: Some("private note".to_string()),
            pending: None,
            popularity: 3,
            version: 7,
            reports: vec!["01REPORTER".to_string()],
            rights: None,
        }
    }

    #[test]
    fn publishing_creates_with_the_public_view_only() {
        let before = item(CommunityItemStatus::PendingReview);
        let after = item(CommunityItemStatus::Published);
        let event = community_change_event(Some(&before), Some(&after)).unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "CommunityItemCreate");
        assert_eq!(json["_id"], "01DESIGN");
        assert_eq!(json["name"], "Crown");
        assert_eq!(json["status"], "Published");
        assert_eq!(json["popularity"], 3);
        // nothing only the creator and reviewers may see
        for key in ["review_note", "reports", "version", "rights", "pending", "favourite"] {
            assert!(json.get(key).is_none(), "{key} leaked: {json}");
        }
        // and it reads back as the same event
        let back: EventV1 = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventV1::CommunityItemCreate(i) if i.id == "01DESIGN"));
    }

    #[test]
    fn a_new_published_design_is_created() {
        let after = item(CommunityItemStatus::Published);
        assert!(matches!(
            community_change_event(None, Some(&after)),
            Some(EventV1::CommunityItemCreate(_))
        ));
        let draft = item(CommunityItemStatus::Draft);
        assert!(community_change_event(None, Some(&draft)).is_none());
    }

    #[test]
    fn leaving_the_gallery_deletes() {
        let before = item(CommunityItemStatus::Published);
        for status in [
            CommunityItemStatus::Suspended,
            CommunityItemStatus::Archived,
            CommunityItemStatus::PendingReview,
        ] {
            let after = item(status);
            let json = serde_json::to_value(
                community_change_event(Some(&before), Some(&after)).unwrap(),
            )
            .unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "type": "CommunityItemDelete", "id": "01DESIGN" })
            );
        }
        assert!(matches!(
            community_change_event(Some(&before), None),
            Some(EventV1::CommunityItemDelete { .. })
        ));
    }

    #[test]
    fn only_visible_changes_update() {
        let before = item(CommunityItemStatus::Published);
        let mut after = before.clone();
        // a version bump, a new note or a report is nothing the gallery shows
        after.version += 1;
        after.review_note = None;
        after.reports.clear();
        assert!(community_change_event(Some(&before), Some(&after)).is_none());

        after.name = "Big crown".to_string();
        let json = serde_json::to_value(
            community_change_event(Some(&before), Some(&after)).unwrap(),
        )
        .unwrap();
        assert_eq!(json["type"], "CommunityItemUpdate");
        assert_eq!(json["name"], "Big crown");
    }

    #[test]
    fn private_designs_say_nothing() {
        let before = item(CommunityItemStatus::Draft);
        let mut after = item(CommunityItemStatus::PendingReview);
        after.name = "Other".to_string();
        assert!(community_change_event(Some(&before), Some(&after)).is_none());
        assert!(community_change_event(Some(&before), None).is_none());
    }

    #[test]
    fn popularity_serialises() {
        let json = serde_json::to_value(EventV1::CommunityItemPopularity {
            id: "01DESIGN".to_string(),
            popularity: 4,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "CommunityItemPopularity", "id": "01DESIGN", "popularity": 4 })
        );
    }
}
