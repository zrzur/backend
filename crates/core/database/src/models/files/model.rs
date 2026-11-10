use crate::{Database, FileHash, Metadata};

use iso8601_timestamp::Timestamp;
use revolt_result::Result;

auto_derived_partial!(
    /// File
    pub struct File {
        /// Unique Id
        #[serde(rename = "_id")]
        pub id: String,
        /// Tag / bucket this file was uploaded to
        pub tag: String,
        /// Original filename
        pub filename: String,
        /// Hash of this file
        pub hash: Option<String>, // these are Option<>s to not break file uploads on legacy Autumn

        /// When this file was uploaded
        pub uploaded_at: Option<Timestamp>, // these are Option<>s to not break file uploads on legacy Autumn
        /// ID of user who uploaded this file
        #[serde(skip_serializing_if = "Option::is_none")]
        pub uploader_id: Option<String>, // these are Option<>s to not break file uploads on legacy Autumn

        /// What the file was used for
        #[serde(skip_serializing_if = "Option::is_none")]
        pub used_for: Option<FileUsedFor>,

        /// Whether this file was deleted
        #[serde(skip_serializing_if = "Option::is_none")]
        pub deleted: Option<bool>,
        /// Whether this file was reported
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reported: Option<bool>,
        /// Whether a reported, deleted file left the CDN. Such files are kept
        /// as evidence, so crond can't tell them apart by their record going.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub purged: Option<bool>,

        // !!! DEPRECATED:
        /// Parsed metadata of this file
        pub metadata: Metadata,
        /// Raw content type of this file
        pub content_type: String,
        /// Size of this file (in bytes)
        pub size: isize,

        // TODO: migrate this mess to having:
        // - author_id
        // - parent: Parent { Message(id), User(id), etc }
        #[serde(skip_serializing_if = "Option::is_none")]
        pub message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub server_id: Option<String>,

        /// Id of the object this file is associated with
        #[serde(skip_serializing_if = "Option::is_none")]
        pub object_id: Option<String>,

        /// A forwarded copy: id of the first record of this file (the one
        /// first uploaded). Safety actions and deleting the original reach
        /// every copy through it.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub forwarded_from: Option<String>,
    },
    "PartialFile"
);

auto_derived!(
    /// Type of object file was used for
    pub enum FileUsedForType {
        Message,
        ServerBanner,
        Emoji,
        UserAvatar,
        WebhookAvatar,
        UserProfileBackground,
        LegacyGroupIcon,
        ChannelIcon,
        ServerIcon,
        RoleIcon,
        CommunityItem,
        ServerEventCover,
    }

    /// Information about what the file was used for
    pub struct FileUsedFor {
        /// Type of the object
        #[serde(rename = "type")]
        pub object_type: FileUsedForType,
        /// ID of the object
        pub id: String,
    }
);

impl File {
    /// A second record for the same stored file, used by nothing yet, so a
    /// message can claim it like a fresh upload (forwarding). The original
    /// record stays with its own message; the stored file is removed only
    /// once no record points at its hash. Files without a hash (legacy
    /// uploads) can't be shared this way.
    pub async fn copy_unused(db: &Database, file: &File, uploader_id: &str) -> Result<File> {
        if file.hash.is_none() {
            return Err(revolt_result::create_error!(InvalidOperation));
        }

        let copy = File {
            id: ulid::Ulid::new().to_string(),
            uploaded_at: Some(Timestamp::now_utc()),
            uploader_id: Some(uploader_id.to_owned()),
            used_for: None,
            deleted: None,
            reported: None,
            purged: None,
            message_id: None,
            user_id: None,
            server_id: None,
            object_id: None,
            forwarded_from: Some(file.root_id().to_string()),
            ..file.clone()
        };
        db.insert_attachment(&copy).await?;
        Ok(copy)
    }

    /// Id of the first record of this file: itself, unless it is a
    /// forwarded copy
    pub fn root_id(&self) -> &str {
        self.forwarded_from.as_deref().unwrap_or(&self.id)
    }

    /// Every record of these files: the originals and every forwarded copy
    /// of them, the given ones included
    pub async fn with_copies(db: &Database, files: &[File]) -> Result<Vec<File>> {
        let mut all: Vec<File> = Vec::new();
        for file in files {
            let family = match &file.hash {
                Some(hash) => db.fetch_file_family(hash, file.root_id()).await?,
                None => vec![],
            };
            for member in family.into_iter().chain(std::iter::once(file.clone())) {
                if !all.iter().any(|known| known.id == member.id) {
                    all.push(member);
                }
            }
        }
        Ok(all)
    }

    /// Copies forwarded from these files, when they are originals (a copy
    /// going takes nothing else with it)
    pub async fn forwarded_copies(db: &Database, files: &[File]) -> Result<Vec<File>> {
        let originals: Vec<File> = files
            .iter()
            .filter(|file| file.forwarded_from.is_none())
            .cloned()
            .collect();
        Ok(File::with_copies(db, &originals)
            .await?
            .into_iter()
            .filter(|file| file.forwarded_from.is_some())
            .collect())
    }

    /// Safety: these files, the originals they were forwarded from and every
    /// copy become unavailable at once and leave CDN caches
    pub async fn remove_with_copies(db: &Database, files: &[File]) -> Result<()> {
        let all = File::with_copies(db, files).await?;
        if all.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = all.iter().map(|file| file.id.clone()).collect();
        db.mark_attachments_as_deleted(&ids).await?;
        crate::util::cdn_purge::purge_files_later(all);
        Ok(())
    }

    /// Safety: keep this file and every record of the same forwarded file as
    /// evidence (when deleted later, they stay stored for the case)
    pub async fn mark_reported_with_copies(db: &Database, tag: &str, id: &str) -> Result<()> {
        db.mark_attachment_as_reported(id).await?;
        if let Ok(file) = db.fetch_attachment(tag, id).await {
            for other in File::with_copies(db, &[file]).await? {
                if other.id != id {
                    db.mark_attachment_as_reported(&other.id).await?;
                }
            }
        }
        Ok(())
    }

    /// Get the hash entry for this file
    pub async fn as_hash(&self, db: &Database) -> Result<FileHash> {
        db.fetch_attachment_hash(self.hash.as_ref().unwrap()).await
    }

    /// Use a file for a message attachment
    pub async fn use_attachment(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "attachments",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::Message,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a user profile background
    pub async fn use_background(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "backgrounds",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::UserProfileBackground,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a user avatar
    pub async fn use_user_avatar(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "avatars",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::UserAvatar,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a webhook avatar
    pub async fn use_webhook_avatar(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "avatars",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::WebhookAvatar,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a server icon
    pub async fn use_server_icon(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "icons",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::ServerIcon,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a channel icon
    pub async fn use_channel_icon(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "icons",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::ChannelIcon,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a server banner
    pub async fn use_server_banner(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "banners",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::ServerBanner,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for the cover of a scheduled server event (same rules as
    /// a server banner)
    pub async fn use_server_event_cover(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "banners",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::ServerEventCover,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for an emoji
    pub async fn use_emoji(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "emojis",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::Emoji,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a role icon
    pub async fn use_role_icon(
        db: &Database,
        id: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            "icons",
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::RoleIcon,
            },
            uploader_id.to_owned(),
        )
        .await
    }

    /// Use a file for a community decoration, nameplate or effect
    pub async fn use_community_item(
        db: &Database,
        id: &str,
        tag: &str,
        parent: &str,
        uploader_id: &str,
    ) -> Result<File> {
        db.find_and_use_attachment(
            id,
            tag,
            FileUsedFor {
                id: parent.to_owned(),
                object_type: FileUsedForType::CommunityItem,
            },
            uploader_id.to_owned(),
        )
        .await
    }
}
