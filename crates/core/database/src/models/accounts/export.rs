//! "Download my data": a ZIP of JSON files the person can ask for from
//! Settings, built in the background by crond.
//!
//! - One request per 24 hours.
//! - The e-mail points to Settings > My data. Only the signed-in owner can
//!   download (no token in any URL). The file is kept 7 days, then removed.
//! - Only what the person can already see or that belongs to them goes in:
//!   no password hash, MFA secrets, tokens, staff notes or other people's
//!   private data (other people appear as ids only).
//!
//! The ZIP is kept in the database in chunks, so no new file service is
//! needed.

use iso8601_timestamp::{Duration, Timestamp};
use revolt_config::config;
use revolt_result::Result;
use serde_json::{json, Value};

use crate::{
    util::email::{email_templates, send_email},
    Account, Database, User, ACCOUNT_CONTACT_EMAIL,
};

/// Minimum time between two requests
pub const DATA_EXPORT_COOLDOWN_HOURS: i64 = 24;
/// How long the download link works
pub const DATA_EXPORT_VALID_DAYS: i64 = 7;
/// Most messages written into one export (the newest first)
pub const DATA_EXPORT_MAX_MESSAGES: i64 = 50_000;
/// Size of one stored piece of a ZIP (MongoDB documents stop at 16 MB)
pub const DATA_EXPORT_CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// Largest ZIP built (it is built in memory); a bigger one counts as failed
/// and is answered by hand within one month
pub const DATA_EXPORT_MAX_BYTES: usize = 256 * 1024 * 1024;

auto_derived!(
    /// State of a data export request (one per person, `_id` is the user id)
    pub struct DataExport {
        /// Unique Id (the id of the person)
        #[serde(rename = "_id")]
        pub id: String,
        /// Where the request is
        pub status: DataExportStatus,
        /// When it was asked for
        pub requested_at: Timestamp,
        /// When the file was ready
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub ready_at: Option<Timestamp>,
        /// When the link stops working and the file is removed
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub expires_at: Option<Timestamp>,
        /// Size of the ZIP in bytes
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pub size: Option<i64>,
        /// How many pieces the ZIP is stored in
        #[serde(default)]
        pub chunks: i32,
    }

    /// Progress of a data export
    pub enum DataExportStatus {
        /// Waiting for crond
        Pending,
        /// The file can be downloaded
        Ready,
        /// Building it failed; the person may ask again
        Failed,
    }

    /// What `export_documents` reads
    pub enum ExportSource {
        /// Messages the person sent (newest first, capped)
        Messages,
        /// Files the person uploaded
        Files,
        /// Servers the person is a member of (their own membership)
        Memberships,
        /// Reports the person filed
        Reports,
        /// Support tickets the person opened (staff-only notes removed later)
        SupportTickets,
        /// The person's own settings
        Settings,
    }
);

impl DataExport {
    /// Seconds until a new request is allowed (0 when it is)
    pub fn seconds_until_next_request(&self) -> u64 {
        let next = self
            .requested_at
            .checked_add(Duration::hours(DATA_EXPORT_COOLDOWN_HOURS))
            .unwrap_or(self.requested_at);
        let now = Timestamp::now_utc();
        if next <= now {
            0
        } else {
            let ms = next.duration_since(now).whole_milliseconds().max(0) as u64;
            ms.div_ceil(1000)
        }
    }

    /// Ask for a copy of the data. The caller has already checked that the
    /// person is who they say (fresh login, password or MFA).
    pub async fn request(db: &Database, account: &Account) -> Result<DataExport> {
        if let Some(existing) = db.fetch_data_export(&account.id).await? {
            let wait = existing.seconds_until_next_request();
            if wait > 0 && existing.status != DataExportStatus::Failed {
                return Err(create_error!(InSlowmode { retry_after: wait }));
            }
            // The earlier file goes away as soon as a new one is asked for
            db.remove_data_exports_for_user(&account.id).await?;
        }

        let export = DataExport {
            id: account.id.clone(),
            status: DataExportStatus::Pending,
            requested_at: Timestamp::now_utc(),
            ready_at: None,
            expires_at: None,
            size: None,
            chunks: 0,
        };
        db.save_data_export(&export).await?;
        Ok(export)
    }

    /// Build the ZIP, store it and e-mail the link (called by crond)
    pub async fn process(&mut self, db: &Database) -> Result<()> {
        let result = async {
            let account = db.fetch_account(&self.id).await?;
            let user = db.fetch_user(&self.id).await?;
            let zip = build_export_zip(db, &account, &user).await?;
            if zip.len() > DATA_EXPORT_MAX_BYTES {
                return Err(create_error!(PayloadTooLarge));
            }
            self.size = Some(zip.len() as i64);
            self.chunks = db.save_data_export_bytes(&self.id, &zip).await?;
            Result::Ok(account)
        }
        .await;

        match result {
            Ok(account) => {
                let now = Timestamp::now_utc();
                self.status = DataExportStatus::Ready;
                self.ready_at = Some(now);
                self.expires_at = now.checked_add(Duration::days(DATA_EXPORT_VALID_DAYS));
                db.save_data_export(self).await?;
                self.send_email(&account).await.ok();
                Ok(())
            }
            Err(error) => {
                self.status = DataExportStatus::Failed;
                db.save_data_export(self).await?;
                Err(error)
            }
        }
    }

    async fn send_email(&self, account: &Account) -> Result<()> {
        let config = config().await;
        if config.api.smtp.host.is_empty() {
            return Ok(());
        }
        let templates = email_templates().await;
        let days = DATA_EXPORT_VALID_DAYS;
        send_email(
            &config.api.smtp,
            account.email.clone(),
            &templates.notice,
            json!({
                "title_en": "Your copy of your data is ready",
                "body_en": format!("You asked for a copy of your Zrzur data. It is ready. Sign in and open Settings > My data to download it. It is kept for {days} days, then deleted. Problems? Write to {ACCOUNT_CONTACT_EMAIL}."),
                "title_ar": "نسخة بياناتك جاهزة",
                "body_ar": format!("طلبتَ نسخة من بياناتك في زرزور وقد أصبحت جاهزة. سجّل الدخول وافتح الإعدادات ← بياناتي لتنزيلها. تبقى النسخة {days} أيام ثم تُحذف. هل واجهتك مشكلة؟ راسلنا على {ACCOUNT_CONTACT_EMAIL}."),
                "title_de": "Deine Datenkopie ist bereit",
                "body_de": format!("Du hast eine Kopie deiner Zrzur-Daten angefordert. Sie ist fertig. Melde dich an und öffne Einstellungen > Meine Daten, um sie herunterzuladen. Sie bleibt {days} Tage erhalten und wird dann gelöscht. Probleme? Schreib an {ACCOUNT_CONTACT_EMAIL}."),
                "link": config.hosts.app.clone(),
            }),
        )
    }
}

/// Build the whole ZIP for one person
pub async fn build_export_zip(db: &Database, account: &Account, user: &User) -> Result<Vec<u8>> {
    let config = config().await;
    let autumn = config.hosts.autumn.clone();

    // Account, without anything that lets someone sign in
    let account_json = json!({
        "id": account.id,
        "email": account.email,
        "email_verified": matches!(account.verification, crate::EmailVerification::Verified),
        "two_factor_enabled": account.mfa.is_active(),
        "birth_month": account.age.as_ref().map(|a| a.birth_month.clone()),
        "age_declared_at": account.age.as_ref().map(|a| a.declared_at),
        "age_group_changes": account.age.as_ref().map(|a| a.changes.clone()),
        "created_at": ulid::Ulid::from_string(&account.id).ok().map(|u| u.timestamp_ms()),
    });

    // Profile: the User document (people in `relations` are ids only)
    let profile = serde_json::to_value(user).unwrap_or(Value::Null);

    // Designs they made, with what they promised about each. Who reported or
    // reviewed one is not theirs to have.
    let creations: Vec<Value> = db
        .fetch_community_items(crate::CommunityItemQuery {
            creator_id: Some(&user.id),
            limit: 1000,
            ..Default::default()
        })
        .await?
        .into_iter()
        .map(|item| {
            let mut item = serde_json::to_value(item).unwrap_or(Value::Null);
            if let Some(o) = item.as_object_mut() {
                o.remove("reports");
                o.remove("reviewed_by");
            }
            item
        })
        .collect();

    let messages = db.export_documents(&user.id, ExportSource::Messages).await?;
    let truncated = messages.len() as i64 >= DATA_EXPORT_MAX_MESSAGES;

    let files: Vec<Value> = db
        .export_documents(&user.id, ExportSource::Files)
        .await?
        .into_iter()
        .map(|mut file| {
            if let Some(o) = file.as_object_mut() {
                o.remove("reported");
                o.remove("hash");
            }
            if let (Some(tag), Some(id), Some(name)) = (
                file.get("tag").and_then(Value::as_str).map(str::to_owned),
                file.get("_id").and_then(Value::as_str).map(str::to_owned),
                file.get("filename").and_then(Value::as_str).map(str::to_owned),
            ) {
                let encoded: String = name
                    .bytes()
                    .map(|b| {
                        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                            (b as char).to_string()
                        } else {
                            format!("%{b:02X}")
                        }
                    })
                    .collect();
                file["link"] = json!(format!("{autumn}/{tag}/{id}/{encoded}"));
            }
            file
        })
        .collect();

    // Servers: the person's own membership, with the name of the server
    let memberships = db
        .export_documents(&user.id, ExportSource::Memberships)
        .await?;
    let server_ids: Vec<String> = db
        .fetch_all_memberships(&user.id)
        .await?
        .into_iter()
        .map(|m| m.id.server)
        .collect();
    let servers: Vec<Value> = db
        .fetch_servers(&server_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| json!({ "id": s.id, "name": s.name, "owner": s.owner == user.id }))
        .collect();

    // Purchases and subscriptions
    let orders = db.fetch_orders_by_user(&user.id, 10_000).await?;
    let supercharges = db.fetch_supercharges_by_user(&user.id).await?;
    let purchases = json!({
        "feather": user.plus,
        "orders": orders.iter().map(|o| json!({
            "id": o.id, "product": o.product, "kind": o.kind, "amount": o.amount,
            "currency": o.currency, "status": o.status, "provider": o.provider,
            "created_at": o.created_at, "paid_at": o.paid_at, "quantity": o.quantity,
        })).collect::<Vec<_>>(),
        "wings": serde_json::to_value(&supercharges).unwrap_or(Value::Null),
    });

    // Reports the person filed (staff notes stay out)
    let reports: Vec<Value> = db
        .export_documents(&user.id, ExportSource::Reports)
        .await?
        .into_iter()
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                o.remove("notes");
            }
            r
        })
        .collect();

    let settings = db.export_documents(&user.id, ExportSource::Settings).await?;

    // Support tickets (staff-only notes are not part of the copy) and bots
    // (without their token)
    let support: Vec<Value> = db
        .export_documents(&user.id, ExportSource::SupportTickets)
        .await?
        .into_iter()
        .map(|mut t| {
            if let Some(messages) = t.get_mut("messages").and_then(Value::as_array_mut) {
                messages.retain(|m| !m.get("internal").and_then(Value::as_bool).unwrap_or(false));
            }
            if let Some(o) = t.as_object_mut() {
                o.remove("assigned_to");
            }
            t
        })
        .collect();
    let bots: Vec<Value> = db
        .fetch_bots_by_user(&user.id)
        .await?
        .into_iter()
        .filter_map(|b| serde_json::to_value(b).ok())
        .map(|mut b| {
            if let Some(o) = b.as_object_mut() {
                o.remove("token");
                o.remove("token_hash");
            }
            b
        })
        .collect();

    let cap_note = if truncated {
        format!(
            "messages.json holds the newest {DATA_EXPORT_MAX_MESSAGES} messages you sent. For the rest, write to {ACCOUNT_CONTACT_EMAIL}; we answer within one month.
"
        )
    } else {
        String::new()
    };
    let readme = format!(
        "Zrzur data export
Created: {}

This ZIP holds your own data as JSON files: account.json (no password or secrets), profile.json, messages.json (messages you sent, up to {DATA_EXPORT_MAX_MESSAGES}), files.json (files you uploaded, with links that work while the file exists), servers.json, purchases.json, reports.json (reports you filed), support.json (your support tickets), bots.json (bots you own, without tokens), creations.json (designs you made, with your rights confirmations) and settings.json. Other people appear as ids only.
{cap_note}
Why we use this data, who receives it, how long we keep it, your rights and how to complain to the Austrian data protection authority: https://zrzur.com/legal/privacy . Questions: {ACCOUNT_CONTACT_EMAIL}.
",
        Timestamp::now_utc().format(),
    );

    // compact JSON keeps big exports small in memory
    let compact = |v: Value| serde_json::to_vec(&v).unwrap_or_default();
    Ok(zip_stored(vec![
        ("README.txt".into(), readme.into_bytes()),
        ("account.json".into(), compact(account_json)),
        ("profile.json".into(), compact(profile)),
        ("messages.json".into(), compact(Value::Array(messages))),
        ("files.json".into(), compact(Value::Array(files))),
        (
            "servers.json".into(),
            compact(json!({ "memberships": memberships, "servers": servers })),
        ),
        ("purchases.json".into(), compact(purchases)),
        ("reports.json".into(), compact(Value::Array(reports))),
        ("support.json".into(), compact(Value::Array(support))),
        ("bots.json".into(), compact(Value::Array(bots))),
        ("creations.json".into(), compact(Value::Array(creations))),
        ("settings.json".into(), compact(Value::Array(settings))),
    ]))
}

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// A ZIP without compression (JSON text; no extra library needed)
pub fn zip_stored(files: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in &files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name_bytes = name.as_bytes();
        // local file header: UTF-8 names (bit 11), stored, 1980-01-01
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x0021u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_has_signatures_and_every_file() {
        let zip = zip_stored(vec![
            ("a.json".into(), b"{}".to_vec()),
            ("b.json".into(), b"[]".to_vec()),
        ]);
        assert_eq!(&zip[0..4], &[0x50, 0x4b, 0x03, 0x04]);
        let end = &zip[zip.len() - 22..];
        assert_eq!(&end[0..4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([end[10], end[11]]), 2);
        assert!(zip.windows(6).any(|w| w == b"a.json"));
    }
}
