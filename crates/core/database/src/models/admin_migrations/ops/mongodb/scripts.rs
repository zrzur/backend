use std::{
    collections::{HashMap, HashSet},
    ops::BitXor,
    time::Duration,
};

use crate::{
    mongodb::{
        bson::{doc, from_bson, from_document, to_document, Bson, DateTime, Document},
        options::FindOptions,
    },
    AbstractServers, Invite, MongoDb, User, DISCRIMINATOR_SEARCH_SPACE,
};
use bson::{oid::ObjectId, to_bson};
use futures::StreamExt;
use iso8601_timestamp::Timestamp;
use rand::seq::SliceRandom;
use revolt_permissions::{ChannelPermission, DEFAULT_WEBHOOK_PERMISSIONS, THREAD_PERMISSIONS};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Serialize, Deserialize)]
struct MigrationInfo {
    _id: i32,
    revision: i32,
}

pub const LATEST_REVISION: i32 = 93; // MUST BE +1 to last migration
// Number given only in the release after the one above, once no old delta runs:
// a session an old delta wrote during the rollout has no date and never expires.
const PENDING_SESSION_EXPIRY_FILL: i32 = -1;
// Only indexes: the accounts
// themselves are seeded on every start of delta, no data moves.
const PENDING_OFFICIAL_INDEXES: i32 = 75;
const PENDING_LIST_INDEXES: i32 = 76;
const NO_OP_REVISION_77: i32 = 77;
const PENDING_PLATFORM_WORD_SEED: i32 = 78;
const PENDING_PLATFORM_WORD_SEED_2: i32 = 79;
// Number given by the coordinator at merge (stream botsapp)
const PENDING_COMMAND_PERMISSIONS_INDEX: i32 = 80;
// Index of polls crond still has to close and answer
const PENDING_POLL_INDEX: i32 = 81;
// Number given by the coordinator at merge (stream events)
const PENDING_SERVER_EVENT_INDEXES: i32 = 82;
// Number given by the coordinator at merge (stream profile-mutuals-status)
const PENDING_STATUS_MUTUAL_INDEXES: i32 = 83;
// Number given by the coordinator at merge (stream insights)
const PENDING_SERVER_INSIGHTS: i32 = 85;
// Number given by the coordinator at merge (stream voice-extras)
const PENDING_SOUNDBOARD: i32 = 86;
const PENDING_SESSION_HASHES: i32 = 87;
// Sessions are looked up by token hash; the old unique index on `token` made
// every session after the first a duplicate (all have no `token`)
const PENDING_SESSION_TOKEN_INDEX: i32 = 88;
// Number given by the coordinator at merge (stream steam-connection)
const PENDING_LINKED_ACCOUNT_INDEXES: i32 = 89;
// Stream saved-reminders: the coordinator gives the final number at merge
const PENDING_SAVED_REMINDER_INDEXES: i32 = 90;
// Number given by the coordinator at merge (stream events-v5)
const PENDING_SERVER_EVENT_ANSWERS: i32 = 91;
// Stream team-volunteers: the coordinator gives the number at merge; -1 never runs
const PENDING_TEAM_VOLUNTEER_INDEXES: i32 = 92;
// Stream rich-presence: the coordinator gives the number at merge; -1 never runs
const PENDING_ACTIVITY_BLOCKS_INDEX: i32 = -1;
// Stream badge-phase1: the coordinator gives the number at merge; -1 never runs
const PENDING_OFFICIAL_BOT_FLAGS: i32 = -1;
// Stream badge-phase0: the coordinator gives the number at merge; -1 never runs
const PENDING_BADGE_GATE_INDEXES: i32 = -1;

/// The earliest day a session from before expiry can expire. At least 30
/// days out, also with expiry switched off (0), for the day it is switched on.
pub fn session_grace_until(now: Timestamp, idle_days: u32) -> Timestamp {
    now + iso8601_timestamp::Duration::days(idle_days.max(30).into())
}

/// Sessions made before expiry may look unused, because HTTP use was not
/// recorded. Each one gets `until` as the earliest day it can expire, and
/// `last_seen` stays true, so eviction and "last used" are not misled. Until a
/// session has this date it does not expire. One that has a date keeps it, and
/// a new session always has one, so this is safe to run twice.
pub async fn give_sessions_expiry_grace(db: &mongodb::Database, until: Timestamp) {
    db.collection::<Document>("sessions")
        .update_many(
            doc! { "idle_grace_until": { "$exists": false } },
            doc! { "$set": { "idle_grace_until": to_bson(&until).expect("Timestamp") } },
        )
        .await
        .expect("Failed to give sessions an expiry grace");
    create_session_expiry_index(db).await;
}

/// The daily sweep reads sessions by `last_seen`
pub async fn create_session_expiry_index(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "sessions",
        "indexes": [{ "key": { "last_seen": 1_i32 }, "name": "last_seen" }]
    })
    .await
    .expect("Failed to create the session expiry index");
}

/// Marks a server, channel or webhook this migration has handled. Whatever
/// carries it is skipped from then on, so a second run, a run after a crash or
/// a second delta starting at the same time can't undo what an owner set since.
const THREADS_DONE: &str = "thread_permissions_done";
/// How many times a server is read again when its categories keep changing
const THREADS_TRIES: usize = 5;

/// A saved permission number: old documents may hold it as an i32
fn stored_bits(field: &Document, key: &str) -> Option<i64> {
    field
        .get_i64(key)
        .ok()
        .or_else(|| field.get_i32(key).ok().map(i64::from))
}

/// One item of a cursor. A read error stops the migration: ending the loop
/// instead would save the revision with the rest of the documents left out.
fn read_or_stop<T, E: std::fmt::Display>(item: Result<T, E>, what: &str) -> T {
    item.unwrap_or_else(|error| panic!("Failed to read {what}: {error}"))
}

/// A thread permission in a saved deny meant nothing before these bits were
/// taken, and must not start to mean something now (say, a deny of every bit)
fn has_thread_deny(permission: &Document) -> bool {
    stored_bits(permission, "d").unwrap_or_default() & *THREAD_PERMISSIONS as i64 != 0
}

/// Whoever manages channels gets the thread permissions outright, so closing
/// them for everyone leaves moderators in. This is true of a role, and of a
/// role or member given Manage Channel by an override.
fn is_moderator_without_bits(permission: &Document) -> bool {
    let allow = stored_bits(permission, "a").unwrap_or_default();
    allow & ChannelPermission::ManageChannel as i64 != 0
        && allow & *THREAD_PERMISSIONS as i64 != *THREAD_PERMISSIONS as i64
}

/// Clear the saved denies at `path`, in place
fn clear_denies(bit: &mut Document, path: &str, permission: &Document) {
    if has_thread_deny(permission) {
        bit.insert(format!("{path}.d"), doc! { "and": !(*THREAD_PERMISSIONS as i64) });
    }
}

/// `clear_denies`, and the bits for a moderator at `path`
fn fix_grant(bit: &mut Document, path: &str, permission: &Document) {
    clear_denies(bit, path, permission);
    if is_moderator_without_bits(permission) {
        bit.insert(format!("{path}.a"), doc! { "or": *THREAD_PERMISSIONS as i64 });
    }
}

/// Clear the saved denies on a category's copy, which goes back whole
fn clear_category_denies(permission: &mut Document) -> bool {
    if !has_thread_deny(permission) {
        return false;
    }
    let deny = stored_bits(permission, "d").unwrap_or_default();
    permission.insert("d", deny & !(*THREAD_PERMISSIONS as i64));
    true
}

/// `clear_category_denies`, and the bits for a moderator
fn fix_category_grant(permission: &mut Document) -> bool {
    let mut changed = clear_category_denies(permission);
    if is_moderator_without_bits(permission) {
        let allow = stored_bits(permission, "a").unwrap_or_default();
        permission.insert("a", allow | *THREAD_PERMISSIONS as i64);
        changed = true;
    }
    changed
}

/// The update for one server: everyone's default gets the bits, and only the
/// numbers that change are written, so an edit to another role survives.
/// Categories are an array, so they go back whole, and only under a guard
/// (`migrate_server`).
fn thread_permissions_on_server(server: &Document) -> Document {
    let mut bit = doc! { "default_permissions": { "or": *THREAD_PERMISSIONS as i64 } };
    let mut set = doc! { THREADS_DONE: true };

    if let Ok(roles) = server.get_document("roles") {
        for (id, role) in roles {
            if let Some(permission) = role
                .as_document()
                .and_then(|role| role.get_document("permissions").ok())
            {
                fix_grant(&mut bit, &format!("roles.{id}.permissions"), permission);
            }
        }
    }

    if let Ok(categories) = server.get_array("categories") {
        let mut categories = categories.clone();
        let mut changed = false;
        for category in categories.iter_mut().filter_map(Bson::as_document_mut) {
            if let Ok(permission) = category.get_document_mut("default_permissions") {
                changed |= clear_category_denies(permission);
            }
            if let Ok(map) = category.get_document_mut("role_permissions") {
                for (_, permission) in map.iter_mut() {
                    if let Bson::Document(permission) = permission {
                        changed |= fix_category_grant(permission);
                    }
                }
            }
        }
        if changed {
            set.insert("categories", categories);
        }
    }

    doc! { "$bit": bit, "$set": set }
}

/// The update for one channel: only the numbers that change
fn thread_permissions_on_channel(channel: &Document) -> Document {
    let mut bit = Document::new();
    if let Ok(permission) = channel.get_document("default_permissions") {
        clear_denies(&mut bit, "default_permissions", permission);
    }
    for field in ["role_permissions", "member_permissions"] {
        if let Ok(map) = channel.get_document(field) {
            for (id, permission) in map {
                if let Some(permission) = permission.as_document() {
                    fix_grant(&mut bit, &format!("{field}.{id}"), permission);
                }
            }
        }
    }

    let mut update = doc! { "$set": { THREADS_DONE: true } };
    if !bit.is_empty() {
        update.insert("$bit", bit);
    }
    update
}

/// Write one server, in one step with its mark. When nothing matches, another
/// delta got there first or the categories changed since they were read: look
/// again rather than write an old copy over them.
async fn migrate_server(servers: &mongodb::Collection<Document>, mut server: Document) {
    let id = server.get_str("_id").expect("A server has an id").to_string();
    let not_done = doc! { "_id": &id, THREADS_DONE: { "$ne": true } };
    for _ in 0..THREADS_TRIES {
        let update = thread_permissions_on_server(&server);
        let mut guard = not_done.clone();
        let rewrites_categories = update
            .get_document("$set")
            .is_ok_and(|set| set.contains_key("categories"));
        if let (true, Some(read)) = (rewrites_categories, server.get("categories")) {
            guard.insert("categories", read.clone());
        }

        let result = servers
            .update_one(guard, update)
            .await
            .expect("Failed to update a server");
        if result.matched_count == 1 {
            return;
        }

        match servers
            .find_one(not_done.clone())
            .projection(doc! { "roles": 1_i32, "categories": 1_i32 })
            .await
            .expect("Failed to read a server again")
        {
            Some(fresh) => server = fresh,
            None => return,
        }
    }
    panic!("Server {id} kept changing during the thread permissions migration");
}

/// Everything older than this is this migration's to handle; what was made
/// since was made with the new defaults. The first delta to get here sets it
/// and every other run reads the same one.
async fn thread_permissions_claim(db: &mongodb::Database) -> String {
    let claims = db.collection::<Document>("migration_claims");
    // a second run fails here on purpose: the _id is taken
    let _ = claims
        .insert_one(doc! { "_id": THREADS_DONE, "before": Ulid::new().to_string() })
        .await;
    claims
        .find_one(doc! { "_id": THREADS_DONE })
        .await
        .expect("Failed to read the thread permissions claim")
        .and_then(|claim| claim.get_str("before").ok().map(str::to_string))
        .expect("The thread permissions claim was not saved")
}

/// Threads and forums get permissions of their own without anyone losing what
/// they can do: until now Send Messages was enough, and it still has to be
/// held, so everyone gets the new bits from the server's default permissions.
/// That covers members and bots: a bot has no role of its own and takes its
/// permissions from the server like anyone else. Webhooks carry their own
/// value, so those that can send get the bits too.
///
/// Safe to run twice, also by two deltas at once: each document is written in
/// one step with its mark, and what is marked or newer than the claim is
/// skipped. Stop every old delta first: a server one of them makes after the
/// claim has the old defaults and is not picked up.
pub async fn grant_thread_permissions(db: &mongodb::Database) {
    let before = thread_permissions_claim(db).await;
    let to_do = doc! { "_id": { "$lt": &before }, THREADS_DONE: { "$ne": true } };

    let servers = db.collection::<Document>("servers");
    let mut cursor = servers
        .find(to_do.clone())
        .projection(doc! { "roles": 1_i32, "categories": 1_i32 })
        .await
        .expect("Failed to read servers");
    while let Some(server) = cursor.next().await {
        migrate_server(&servers, read_or_stop(server, "servers")).await;
    }

    let channels = db.collection::<Document>("channels");
    let mut text_channels = to_do.clone();
    text_channels.insert("channel_type", "TextChannel");
    let mut cursor = channels
        .find(text_channels)
        .projection(doc! { "default_permissions": 1_i32, "role_permissions": 1_i32, "member_permissions": 1_i32 })
        .await
        .expect("Failed to read channels");
    while let Some(channel) = cursor.next().await {
        let channel = read_or_stop(channel, "channels");
        let id = channel.get_str("_id").expect("A channel has an id");
        channels
            .update_one(
                doc! { "_id": id, THREADS_DONE: { "$ne": true } },
                thread_permissions_on_channel(&channel),
            )
            .await
            .expect("Failed to update a channel's overrides");
    }

    // a webhook has never needed CreateThreads: it only starts forum posts
    let mut hooks = to_do;
    hooks.insert(
        "permissions",
        doc! { "$bitsAllSet": ChannelPermission::SendMessage as i64 },
    );
    db.collection::<Document>("channel_webhooks")
        .update_many(
            hooks,
            doc! {
                "$bit": { "permissions": { "or": (ChannelPermission::SendMessageInThreads + ChannelPermission::CreateForumPosts) as i64 } },
                "$set": { THREADS_DONE: true },
            },
        )
        .await
        .expect("Failed to update webhook permissions");
}

/// Webhooks keep working with the token they have: it is hashed in place and
/// the plain one removed. One that already has a hash keeps it (never hash a
/// hash), so this is safe to run twice.
pub async fn hash_webhook_tokens(db: &mongodb::Database) {
    let webhooks = db.collection::<Document>("channel_webhooks");
    let mut cursor = webhooks
        .find(doc! { "token": { "$exists": true } })
        .await
        .expect("Failed to read webhooks");
    while let Some(Ok(webhook)) = cursor.next().await {
        let Ok(id) = webhook.get_str("_id") else {
            continue;
        };
        let update = match webhook.get_str("token") {
            Ok(token) if !webhook.get_str("token_hash").is_ok_and(|hash| !hash.is_empty()) => doc! {
                "$set": { "token_hash": crate::hash_token(token) },
                "$unset": { "token": 1_i32 }
            },
            _ => doc! { "$unset": { "token": 1_i32 } },
        };
        webhooks
            .update_one(doc! { "_id": id }, update)
            .await
            .expect("Failed to hash a webhook token");
    }
    create_webhook_indexes(db).await;
}

/// Webhooks are listed by channel
pub async fn create_webhook_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "channel_webhooks",
        "indexes": [{ "key": { "channel_id": 1_i32 }, "name": "channel_id" }]
    })
    .await
    .expect("Failed to create the webhook index");
}

/// Starter entries of the platform word list, from
/// `assets/platform_words_seed.txt`. The list belongs to staff from then on:
/// the first finished seed leaves a mark and no later run writes anything, so
/// an entry staff removed never comes back. A run cut short goes on where it
/// stopped: ids come from the entry, and an entry staff wrote stays theirs.
/// The mark is not in `migrations`: that collection holds one document.
pub async fn seed_platform_words(db: &mongodb::Database) {
    let seeds = db.collection::<Document>("seed_state");
    let done = doc! { "_id": "platform_words" };
    if seeds.find_one(done.clone()).await.expect("Failed to read the seed state").is_some() {
        return;
    }
    let entries = match crate::platform_word_seed(crate::safety_now()) {
        Ok(entries) => entries,
        Err(error) => {
            error!("The starter word list can't be read, nothing was added: {error}");
            return;
        }
    };

    let words = db.collection::<Document>("platform_words");
    let mut existing = HashSet::new();
    let mut cursor = words.find(doc! {}).await.expect("Failed to read the word list");
    while let Some(word) = cursor.next().await {
        let word = word.expect("Failed to read a word");
        if let Ok(pattern) = word.get_str("pattern") {
            existing.insert((pattern.to_string(), word.get_bool("allow").unwrap_or_default()));
        }
    }
    for entry in entries {
        if existing.contains(&(entry.pattern.clone(), entry.allow)) {
            continue;
        }
        let document = bson::to_document(&entry).expect("Failed to serialise a word");
        words
            .update_one(doc! { "_id": &entry.id }, doc! { "$setOnInsert": document })
            .upsert(true)
            .await
            .expect("Failed to add a word");
    }
    seeds
        .update_one(done, doc! { "$set": { "done": true, "version": WORD_SEED_VERSION } })
        .upsert(true)
        .await
        .expect("Failed to save the seed state");
}

const WORD_SEED_VERSION: i32 = 2;

/// Bring a database seeded with the first starter list to the second: only
/// what that version changed. It removes the entries the seed wrote and leaves
/// any entry staff wrote; what staff removed stays removed. Its version in the
/// mark makes a second run do nothing.
pub async fn update_platform_word_seed(db: &mongodb::Database) {
    let seeds = db.collection::<Document>("seed_state");
    let done = doc! { "_id": "platform_words" };
    let state = seeds.find_one(done.clone()).await.expect("Failed to read the seed state");
    if state.is_some_and(|state| state.get_i32("version").unwrap_or(1) >= WORD_SEED_VERSION) {
        return;
    }
    let words = db.collection::<Document>("platform_words");
    words
        .delete_many(doc! {
            "added_by": "system",
            "pattern": { "$in": crate::WORD_SEED_REMOVED_V2 },
        })
        .await
        .expect("Failed to remove words");
    let entries = match crate::platform_word_seed(crate::safety_now()) {
        Ok(entries) => entries,
        Err(error) => {
            error!("The starter word list can't be read, nothing was added: {error}");
            return;
        }
    };
    for entry in entries {
        if !crate::WORD_SEED_ADDED_V2.contains(&entry.pattern.as_str()) {
            continue;
        }
        let exists = doc! { "pattern": &entry.pattern, "allow": entry.allow };
        if words.find_one(exists).await.expect("Failed to read a word").is_some() {
            continue;
        }
        let document = bson::to_document(&entry).expect("Failed to serialise a word");
        words
            .update_one(doc! { "_id": &entry.id }, doc! { "$setOnInsert": document })
            .upsert(true)
            .await
            .expect("Failed to add a word");
    }
    seeds
        .update_one(done, doc! { "$set": { "done": true, "version": WORD_SEED_VERSION } })
        .upsert(true)
        .await
        .expect("Failed to save the seed state");
}

/// Indexes for platform staff: session lookup by token hash, a unique,
/// ordered audit chain, and the audit filters of the admin console.
pub async fn create_platform_staff_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        (
            "staff_sessions",
            doc! { "key": { "token_hash": 1_i32 }, "name": "token_hash", "unique": true },
        ),
        (
            "staff_sessions",
            doc! { "key": { "user_id": 1_i32, "created_at": -1_i32 }, "name": "user_created" },
        ),
        (
            "platform_audit",
            doc! { "key": { "seq": 1_i32 }, "name": "seq", "unique": true },
        ),
        (
            "platform_audit",
            doc! { "key": { "actor_id": 1_i32, "seq": -1_i32 }, "name": "actor_seq" },
        ),
        (
            "platform_audit",
            doc! { "key": { "target_id": 1_i32, "seq": -1_i32 }, "name": "target_seq", "sparse": true },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// Indexes behind the Trust & Safety queue, dedupe, standing and appeals.
/// Every reviewer filter and every per-report lookup uses one of these, so
/// filing or triaging a report never scans a collection.
pub async fn create_trust_safety_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        (
            "safety_cases",
            doc! { "key": { "reference": 1_i32 }, "name": "reference", "unique": true },
        ),
        (
            "safety_cases",
            doc! { "key": { "status": 1_i32, "_id": -1_i32 }, "name": "status_id" },
        ),
        (
            "safety_cases",
            doc! { "key": { "route": 1_i32, "status": 1_i32, "_id": -1_i32 }, "name": "route_status_id" },
        ),
        (
            // dedupe of a reporter's open report and related reports
            "safety_cases",
            doc! { "key": { "target.kind": 1_i32, "target.id": 1_i32, "status": 1_i32 }, "name": "target_status" },
        ),
        (
            "safety_cases",
            doc! { "key": { "reporter_id": 1_i32, "created_at": -1_i32 }, "name": "reporter_created" },
        ),
        (
            "safety_cases",
            doc! { "key": { "assignee_id": 1_i32, "status": 1_i32 }, "name": "assignee_status", "sparse": true },
        ),
        (
            "safety_cases",
            doc! { "key": { "target_owner_id": 1_i32 }, "name": "target_owner", "sparse": true },
        ),
        (
            "safety_evidence",
            doc! { "key": { "case_id": 1_i32 }, "name": "case_id" },
        ),
        (
            "safety_evidence",
            doc! { "key": { "expires_at": 1_i32 }, "name": "expires_at", "sparse": true },
        ),
        (
            "safety_evidence",
            doc! { "key": { "files.id": 1_i32 }, "name": "files_id", "sparse": true },
        ),
        (
            "safety_case_log",
            doc! { "key": { "case_id": 1_i32, "_id": 1_i32 }, "name": "case_id" },
        ),
        (
            "safety_actions",
            doc! { "key": { "subject_user_id": 1_i32, "created_at": -1_i32 }, "name": "subject_created", "sparse": true },
        ),
        (
            "safety_actions",
            doc! { "key": { "case_id": 1_i32 }, "name": "case_id", "sparse": true },
        ),
        (
            "safety_actions",
            doc! { "key": { "kind": 1_i32, "expires_at": 1_i32 }, "name": "kind_expires", "sparse": true },
        ),
        (
            // one appeal per action
            "safety_appeals",
            doc! { "key": { "action_id": 1_i32 }, "name": "action_id", "unique": true },
        ),
        (
            "safety_appeals",
            doc! { "key": { "status": 1_i32, "_id": -1_i32 }, "name": "status_id" },
        ),
        (
            "safety_appeals",
            doc! { "key": { "user_id": 1_i32 }, "name": "user_id" },
        ),
        (
            "safety_appeals",
            doc! { "key": { "case_id": 1_i32 }, "name": "case_id", "sparse": true },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// New review fields for a community item made before review existed:
/// Active → Published (stays public), Hidden → Suspended (was hidden after
/// reports, a reviewer decides), Removed → Archived. Returns `None` if the
/// document is already migrated. The creator and file are never touched.
pub fn community_item_review_fields(item: &Document, created_ms: i64, wearers: i64) -> Option<Document> {
    let status = match item.get_str("status").ok()? {
        "Active" => "Published",
        "Hidden" => "Suspended",
        "Removed" => "Archived",
        _ => return None,
    };
    let mut set = doc! {
        "status": status,
        "revision": 1_i64,
        "version": 0_i64,
        "popularity": wearers,
    };
    if status != "Archived" {
        set.insert("published_at", created_ms);
    }
    Some(set)
}

/// Migration 55: community designs move to creator-only publishing with
/// review. Existing items keep their creator, image and wearers.
async fn migrate_community_review(db: &MongoDb) {
    let items = db.col::<Document>("community_items");
    let users = db.col::<Document>("users");
    let uses = db.col::<Document>("community_uses");

    let mut cursor = items
        .find(doc! { "status": { "$in": ["Active", "Hidden", "Removed"] } })
        .await
        .expect("Failed to read community_items");
    while let Some(Ok(item)) = cursor.next().await {
        let Ok(id) = item.get_str("_id").map(str::to_string) else {
            continue;
        };
        let created_ms = Ulid::from_string(&id)
            .map(|ulid| ulid.timestamp_ms() as i64)
            .unwrap_or_default();

        // Everyone wearing it counts as having applied it once
        let custom = format!("c:{id}");
        let mut wearers = users
            .find(doc! { "$or": [ { "style.decoration": &custom }, { "style.nameplate": &custom } ] })
            .projection(doc! { "_id": 1 })
            .await
            .expect("Failed to read users");
        let mut count = 0_i64;
        while let Some(Ok(user)) = wearers.next().await {
            if let Ok(user_id) = user.get_str("_id") {
                // ignore duplicates if the migration is re-run
                let _ = uses
                    .insert_one(doc! { "_id": format!("{id}:{user_id}"), "item": &id, "user": user_id })
                    .await;
                count += 1;
            }
        }

        if let Some(set) = community_item_review_fields(&item, created_ms, count) {
            items
                .update_one(doc! { "_id": &id }, doc! { "$set": set })
                .await
                .expect("Failed to migrate community item");
        }
    }

    for (collection, indexes) in [
        (
            "community_items",
            vec![
                doc! { "key": { "kind": 1_i32, "status": 1_i32, "_id": -1_i32 }, "name": "kind_status" },
                doc! { "key": { "kind": 1_i32, "status": 1_i32, "popularity": -1_i32 }, "name": "kind_popular" },
                doc! { "key": { "creator_id": 1_i32 }, "name": "creator" },
                doc! { "key": { "pending.status": 1_i32 }, "name": "pending_status" },
            ],
        ),
        (
            "community_reviews",
            vec![doc! { "key": { "item_id": 1_i32 }, "name": "item" }],
        ),
        (
            "community_favourites",
            vec![doc! { "key": { "user_id": 1_i32, "created_at": -1_i32 }, "name": "user" }],
        ),
        (
            "creator_permissions",
            vec![doc! { "key": { "user_id": 1_i32 }, "name": "user" }],
        ),
    ] {
        db.db()
            .run_command(doc! { "createIndexes": collection, "indexes": indexes })
            .await
            .expect("Failed to create community indexes");
    }
}

/// Secondary indexes for lookups that otherwise scan the whole collection:
/// direct messages on every Ready, server emoji, bot token authentication,
/// and the billing / supercharge queries (per request and in crond).
/// Adding an index never changes query results, and creating an existing
/// index with the same definition is a no-op, so this is safe to re-run.
pub async fn create_lookup_indexes(db: &crate::mongodb::Database) {
    for (collection, indexes) in [
        (
            "channels",
            vec![
                doc! { "key": { "recipients": 1_i32 }, "name": "recipients" },
                doc! { "key": { "user": 1_i32 }, "name": "user", "sparse": true },
            ],
        ),
        (
            "emojis",
            vec![doc! { "key": { "parent.id": 1_i32 }, "name": "parent_id" }],
        ),
        (
            "bots",
            vec![
                doc! { "key": { "token": 1_i32 }, "name": "token" },
                doc! { "key": { "owner": 1_i32 }, "name": "owner" },
            ],
        ),
        (
            "servers",
            vec![doc! { "key": { "supercharge.count": 1_i32 }, "name": "supercharge_count", "sparse": true }],
        ),
        (
            "billing_orders",
            vec![
                doc! { "key": { "user_id": 1_i32, "created_at": -1_i32 }, "name": "user_created" },
                doc! { "key": { "status": 1_i32, "created_at": 1_i32 }, "name": "status_created" },
            ],
        ),
        (
            "plus_subscriptions",
            vec![
                doc! { "key": { "user_id": 1_i32, "ended": 1_i32 }, "name": "user_ended" },
                doc! { "key": { "ended": 1_i32, "current_period_end": 1_i32 }, "name": "ended_period_end" },
            ],
        ),
        (
            "supercharges",
            vec![
                doc! { "key": { "user_id": 1_i32 }, "name": "user" },
                doc! { "key": { "server_id": 1_i32 }, "name": "server", "sparse": true },
                doc! { "key": { "source": 1_i32, "current_period_end": 1_i32 }, "name": "source_period_end" },
            ],
        ),
    ] {
        // One at a time, and only a speed-up: if a deployment already has an
        // equivalent index under another name, skip just that one and keep
        // going rather than refuse to start.
        for index in indexes {
            if let Err(error) = db
                .run_command(doc! { "createIndexes": collection, "indexes": [index] })
                .await
            {
                warn!("Could not create a lookup index on {collection}: {error}");
            }
        }
    }
}

/// Indexes of the developer platform (application commands, interactions)
/// Indexes for provider payments: lookups by provider ids, the webhook
/// queue, the audit log and reconciliation (all additive)
pub async fn create_payment_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        ("billing_orders", doc! { "key": { "provider_checkout_id": 1_i32 }, "name": "provider_checkout", "unique": true, "sparse": true }),
        ("billing_orders", doc! { "key": { "provider_payment_id": 1_i32 }, "name": "provider_payment", "sparse": true }),
        ("billing_orders", doc! { "key": { "provider_subscription_id": 1_i32, "created_at": 1_i32 }, "name": "provider_subscription", "sparse": true }),
        ("billing_orders", doc! { "key": { "provider_invoice_id": 1_i32 }, "name": "provider_invoice", "sparse": true }),
        ("billing_orders", doc! { "key": { "status": 1_i32, "fulfilment": 1_i32, "fulfilment_lease": 1_i32 }, "name": "fulfilment_queue" }),
        ("billing_webhook_events", doc! { "key": { "status": 1_i32, "next_attempt_at": 1_i32 }, "name": "event_queue" }),
        ("billing_webhook_events", doc! { "key": { "received_at": -1_i32 }, "name": "received_at" }),
        ("billing_customers", doc! { "key": { "user_id": 1_i32 }, "name": "user_id" }),
        ("billing_audit", doc! { "key": { "order_id": 1_i32, "created_at": -1_i32 }, "name": "order_created" }),
        ("billing_audit", doc! { "key": { "user_id": 1_i32, "created_at": -1_i32 }, "name": "user_created" }),
        ("billing_audit", doc! { "key": { "created_at": -1_i32 }, "name": "created_at" }),
        ("billing_reconciliation", doc! { "key": { "resolved": 1_i32, "found_at": -1_i32 }, "name": "resolved_found" }),
        ("plus_subscriptions", doc! { "key": { "provider_subscription_id": 1_i32 }, "name": "provider_subscription", "sparse": true }),
        ("supercharges", doc! { "key": { "order_id": 1_i32 }, "name": "order_id", "sparse": true }),
        ("supercharges", doc! { "key": { "provider_subscription_id": 1_i32 }, "name": "provider_subscription", "sparse": true }),
        ("discover_promotions", doc! { "key": { "orders": 1_i32 }, "name": "orders" }),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

pub async fn create_developer_platform_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [(
        "application_commands",
        // one command per type and name in a scope; a global command has no server_id
        doc! {
            "key": { "application_id": 1_i32, "server_id": 1_i32, "type": 1_i32, "name": 1_i32 },
            "name": "application_scope_type_name",
            "unique": true
        },
    ), (
        "interactions",
        // a retried request (same client nonce) returns the first interaction
        doc! { "key": { "user_id": 1_i32, "nonce": 1_i32 }, "name": "user_nonce", "unique": true },
    ), (
        "interactions",
        // tokens and private answers disappear once the interaction expires
        doc! { "key": { "expires_at": 1_i32 }, "name": "expires_at_ttl", "expireAfterSeconds": 0_i32 },
    ), (
        "interactions",
        // components on an ephemeral answer are looked up by its message id
        doc! { "key": { "ephemeral_message._id": 1_i32 }, "name": "ephemeral_message", "sparse": true },
    )] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// Indexes of OAuth2 (codes, tokens, grants) and app installs
pub async fn create_oauth_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        ("oauth_codes", doc! { "key": { "expires_at": 1_i32 }, "name": "expires_at_ttl", "expireAfterSeconds": 0_i32 }),
        ("oauth_tokens", doc! { "key": { "access_hash": 1_i32 }, "name": "access_hash", "unique": true }),
        ("oauth_tokens", doc! { "key": { "refresh_hash": 1_i32 }, "name": "refresh_hash", "unique": true }),
        ("oauth_tokens", doc! { "key": { "user_id": 1_i32, "application_id": 1_i32 }, "name": "user_application" }),
        ("oauth_tokens", doc! { "key": { "family": 1_i32 }, "name": "family" }),
        ("oauth_tokens", doc! { "key": { "application_id": 1_i32 }, "name": "application" }),
        // a refresh token that ran out also takes its access token with it
        ("oauth_tokens", doc! { "key": { "expires_at": 1_i32 }, "name": "expires_at_ttl", "expireAfterSeconds": 0_i32 }),
        ("oauth_grants", doc! { "key": { "user_id": 1_i32 }, "name": "user" }),
        ("oauth_grants", doc! { "key": { "application_id": 1_i32 }, "name": "application" }),
        ("app_installs", doc! { "key": { "server_id": 1_i32 }, "name": "server" }),
        ("app_installs", doc! { "key": { "application_id": 1_i32 }, "name": "application" }),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

pub async fn migrate_database(db: &MongoDb) {
    let migrations = db.col::<Document>("migrations");
    let data = migrations
        .find_one(doc! {})
        .await
        .expect("Failed to fetch migration data.");

    if let Some(doc) = data {
        let info: MigrationInfo =
            from_document(doc).expect("Failed to read migration information.");

        let revision = run_migrations(db, info.revision).await;

        migrations
            .update_one(
                doc! {
                    "_id": info._id
                },
                doc! {
                    "$set": {
                        "revision": revision
                    }
                },
            )
            .await
            .expect("Failed to commit migration information.");

        info!("Migration complete. Currently at revision {}.", revision);
    } else {
        panic!("Database was configured incorrectly, possibly because initalization failed.")
    }
}

pub async fn run_migrations(db: &MongoDb, revision: i32) -> i32 {
    info!("Starting database migration.");

    if revision <= 0 {
        info!("Running migration [revision 0]: Test migration system.");
    }

    if revision <= 1 {
        info!("Running migration [revision 1 / 2021-04-24]: Migrate to Autumn v1.0.0.");

        let messages = db.col::<Document>("messages");
        let attachments = db.col::<Document>("attachments");

        messages
            .update_many(
                doc! { "attachment": { "$exists": 1_i32 } },
                doc! { "$set": { "attachment.tag": "attachments", "attachment.size": 0_i32 } },
            )
            .await
            .expect("Failed to update messages.");

        attachments
            .update_many(
                doc! {},
                doc! { "$set": { "tag": "attachments", "size": 0_i32 } },
            )
            .await
            .expect("Failed to update attachments.");
    }

    if revision <= 2 {
        info!("Running migration [revision 2 / 2021-05-08]: Add servers collection.");

        db.db()
            .create_collection("servers")
            .await
            .expect("Failed to create servers collection.");
    }

    if revision <= 3 {
        info!("Running migration [revision 3 / 2021-05-25]: Support multiple file uploads, add channel_unreads and user_settings.");

        let messages = db.col::<Document>("messages");
        let mut cursor = messages
            .find(doc! {
                "attachment": {
                    "$exists": 1_i32
                }
            })
            .with_options(
                FindOptions::builder()
                    .projection(doc! {
                        "_id": 1_i32,
                        "attachments": [ "$attachment" ]
                    })
                    .build(),
            )
            .await
            .expect("Failed to fetch messages.");

        while let Some(result) = cursor.next().await {
            let doc = result.unwrap();
            let id = doc.get_str("_id").unwrap();
            let attachments = doc.get_array("attachments").unwrap();

            messages
                .update_one(
                    doc! { "_id": id },
                    doc! { "$unset": { "attachment": 1_i32 }, "$set": { "attachments": attachments } },
                )
                .await
                .unwrap();
        }

        db.db()
            .create_collection("channel_unreads")
            .await
            .expect("Failed to create channel_unreads collection.");

        db.db()
            .create_collection("user_settings")
            .await
            .expect("Failed to create user_settings collection.");
    }

    if revision <= 4 {
        info!("Running migration [revision 4 / 2021-06-01]: Add more server collections.");

        db.db()
            .create_collection("server_members")
            .await
            .expect("Failed to create server_members collection.");

        db.db()
            .create_collection("server_bans")
            .await
            .expect("Failed to create server_bans collection.");

        db.db()
            .create_collection("channel_invites")
            .await
            .expect("Failed to create channel_invites collection.");
    }

    if revision <= 5 {
        info!("Running migration [revision 5 / 2021-06-26]: Add permissions.");

        #[derive(Serialize)]
        struct Server {
            pub default_permissions: (i32, i32),
        }

        let server = Server {
            default_permissions: (0_i32, 0_i32),
        };

        db.col::<Document>("servers")
            .update_many(
                doc! {},
                doc! {
                    "$set": to_document(&server).unwrap()
                },
            )
            .await
            .expect("Failed to migrate servers.");
    }

    if revision <= 6 {
        info!("Running migration [revision 6 / 2021-07-09]: Add message text index.");

        db.db()
            .run_command(doc! {
                "createIndexes": "messages",
                "indexes": [
                    {
                        "key": {
                            "content": "text"
                        },
                        "name": "content"
                    }
                ]
            })
            .await
            .expect("Failed to create message index.");
    }

    if revision <= 7 {
        info!("Running migration [revision 7 / 2021-08-11]: Add message text index.");

        db.db()
            .create_collection("bots")
            .await
            .expect("Failed to create bots collection.");
    }

    if revision <= 8 {
        info!("Running migration [revision 8 / 2021-09-10]: Update to Authifier version 1.");

        db.db()
            .run_command(doc! {
                "dropIndexes": "accounts",
                "index": ["email", "email_normalised"]
            })
            .await
            .expect("Failed to delete legacy account indexes.");

        let col = db.col::<Document>("sessions");
        let mut cursor = db.col::<Document>("accounts").find(doc! {}).await.unwrap();

        while let Some(doc) = cursor.next().await {
            if let Ok(account) = doc {
                let id = account.get_str("_id").unwrap();
                if let Some(sessions) = account.get("sessions") {
                    #[derive(Deserialize)]
                    struct Session {
                        id: String,
                        token: String,
                        friendly_name: String,
                        subscription: Option<Document>,
                    }

                    let sessions = from_bson::<Vec<Session>>(sessions.clone()).unwrap();
                    for session in sessions {
                        info!("Converting session {} to new format.", &session.id);

                        let mut doc = doc! {
                            "_id": session.id,
                            "token": session.token,
                            "user_id": id,
                            "name": session.friendly_name,
                        };

                        if let Some(sub) = session.subscription {
                            doc.insert("subscription", sub);
                        }

                        col.insert_one(doc).await.ok();
                    }
                } else {
                    info!("Account doesn't have any sessions!");
                }
            }
        }

        db.col::<Document>("accounts")
            .update_many(
                doc! {},
                doc! {
                    "$unset": {
                        "sessions": 1_i32,
                    },
                    "$set": {
                        "mfa": {
                            "recovery_codes": []
                        }
                    }
                },
            )
            .await
            .unwrap();
    }

    if revision <= 9 {
        info!("Running migration [revision 9 / 2021-09-14]: Switch from last_message to last_message_id.");

        let mut cursor = db.col::<Document>("channels").find(doc! {}).await.unwrap();

        while let Some(doc) = cursor.next().await {
            if let Ok(channel) = doc {
                let channel_id = channel.get_str("_id").unwrap();
                if let Some(last_message) = channel.get("last_message") {
                    #[derive(Serialize, Deserialize, Debug, Clone)]
                    pub struct Obj {
                        #[serde(rename = "_id")]
                        id: String,
                    }

                    #[derive(Serialize, Deserialize, Debug, Clone)]
                    #[serde(untagged)]
                    pub enum LastMessage {
                        Obj(Obj),
                        Id(String),
                    }

                    let lm = from_bson::<LastMessage>(last_message.clone()).unwrap();
                    let id = match lm {
                        LastMessage::Obj(Obj { id }) => id,
                        LastMessage::Id(id) => id,
                    };

                    info!("Converting session {} to new format.", &channel_id);
                    db.col::<Document>("channels")
                        .update_one(
                            doc! {
                                "_id": channel_id
                            },
                            doc! {
                                "$set": {
                                    "last_message_id": id
                                },
                                "$unset": {
                                    "last_message": 1_i32,
                                }
                            },
                        )
                        .await
                        .unwrap();
                } else {
                    info!("{} has no last_message.", &channel_id);
                }
            }
        }
    }

    if revision <= 10 {
        info!("Running migration [revision 10 / 2021-11-01]: Remove nonce values on channels and servers.");

        db.col::<Document>("servers")
            .update_many(
                doc! {},
                doc! {
                    "$unset": {
                        "nonce": 1_i32,
                    }
                },
            )
            .await
            .unwrap();

        db.col::<Document>("channels")
            .update_many(
                doc! {},
                doc! {
                    "$unset": {
                        "nonce": 1_i32,
                    }
                },
            )
            .await
            .unwrap();
    }

    if revision <= 11 {
        info!("Running migration [revision 11 / 2021-11-14]: Add indexes to database.");

        db.db()
            .run_command(doc! {
                "createIndexes": "messages",
                "indexes": [
                    {
                        "key": {
                            "channel": 1_i32
                        },
                        "name": "channel"
                    }
                ]
            })
            .await
            .expect("Failed to create message index.");

        db.db()
            .run_command(doc! {
                "createIndexes": "channel_unreads",
                "indexes": [
                    {
                        "key": {
                            "_id.channel": 1_i32,
                            "_id.user": 1_i32,
                        },
                        "name": "compound_id"
                    },
                    {
                        "key": {
                            "_id.user": 1_i32,
                        },
                        "name": "user_id"
                    }
                ]
            })
            .await
            .expect("Failed to create channel_unreads index.");

        db.db()
            .run_command(doc! {
                "createIndexes": "server_members",
                "indexes": [
                    {
                        "key": {
                            "_id.server": 1_i32,
                            "_id.user": 1_i32,
                        },
                        "name": "compound_id"
                    },
                    {
                        "key": {
                            "_id.user": 1_i32,
                        },
                        "name": "user_id"
                    }
                ]
            })
            .await
            .expect("Failed to create server_members index.");
    }

    if revision <= 12 {
        info!("Running migration [revision 12 / 2021-11-21]: Add indexes to database.");

        db.db()
            .run_command(doc! {
                "createIndexes": "messages",
                "indexes": [
                    {
                        "key": {
                            "channel": 1_i32,
                            "_id": 1_i32
                        },
                        "name": "channel_id_compound"
                    }
                ]
            })
            .await
            .expect("Failed to create message index.");
    }

    if revision <= 13 {
        info!("Running migration [revision 13 / 22-02-2022]: Wipe legacy permission values.");

        warn!("This is a destructive operation and will wipe existing permission data (excl. defaults for SendMessage).");
        warn!("Taking a backup is advised.");
        warn!("Continuing in 10 seconds...");
        tokio::time::sleep(Duration::from_secs(10)).await;

        let servers = db.col::<Document>("servers");
        let mut cursor = servers.find(doc! {}).await.unwrap();

        while let Some(Ok(mut document)) = cursor.next().await {
            let id = document.get_str("_id").unwrap().to_string();
            info!("Updating server {id}");

            let mut update = doc! {};

            // Try to pluck channel permission SendMessage (0x2)
            // Structure of default_permissions used to be [server, channel]
            let has_send = document
                .get_array("default_permissions")
                .map(|x| {
                    x.get(1)
                        .map(|x| x.as_i32().map(|x| (x as u32 & 0x2) == 0x2))
                })
                .ok()
                .flatten()
                .flatten()
                .unwrap_or_default();

            update.insert(
                "default_permissions",
                // Remove Send Message permission if it wasn't originally granted
                (4000323584).bitxor(if has_send { 0 } else { (1 << 22) as u64 }) as i64,
            );

            if let Some(Bson::Document(mut roles)) = document.remove("roles") {
                for role in roles.keys().cloned().collect::<Vec<String>>() {
                    if let Some(Bson::Document(role)) = roles.get_mut(role) {
                        role.insert(
                            "permissions",
                            doc! {
                                "a": 0_i64,
                                "d": 0_i64,
                            },
                        );
                    }
                }

                update.insert("roles", roles);
            }

            servers
                .update_one(doc! { "_id": id }, doc! { "$set": update })
                .await
                .unwrap();
        }

        let channels = db.col::<Document>("channels");
        let mut cursor = channels.find(doc! {}).await.unwrap();

        while let Some(Ok(document)) = cursor.next().await {
            let id = document.get_str("_id").unwrap().to_string();
            info!("Updating channel {id}");

            let mut unset = doc! {
                "permissions": 1_i32,
                "role_permissions": 1_i32,
            };

            // Try to pluck channel permission SendMessage (0x2)
            let has_send = document
                .get_i32("default_permissions")
                .map(|x| (x as u32 & 0x2) == 0x2)
                .unwrap_or(true);

            if has_send {
                // Let parent permissions fall through.
                unset.insert("default_permissions", 1_i32);
            }

            let mut update = doc! {
                "$unset": unset
            };

            if !has_send {
                // Block send message permission.
                update.insert(
                    "$set",
                    doc! {
                        "default_permissions": {
                            "a": 0_i64,
                            "d": (1 << 22) as i64
                        }
                    },
                );
            }

            channels
                .update_one(doc! { "_id": id }, update)
                .await
                .unwrap();
        }
    }

    if revision <= 14 {
        info!("Running migration [revision 14 / 21-04-2022]: Split content into content and system fields.");

        db.col::<Document>("messages")
            .update_many(
                doc! {
                    "content": {
                        "$type": "object"
                    }
                },
                doc! {
                    "$rename": {
                        "content": "system"
                    }
                },
            )
            .await
            .unwrap();
    }

    if revision <= 15 {
        info!("Running migration [revision 15 / 04-06-2022]: Migrate Authifier to latest version.");

        if !db
            .db()
            .collection::<Document>("mfa_tickets")
            .list_index_names()
            .await
            .unwrap_or_default()
            .contains(&"token".to_owned())
        {
            // Make sure all collections exist
            let list = db.db().list_collection_names().await.unwrap();
            let collections = ["accounts", "sessions", "invites", "mfa_tickets"];

            for name in collections {
                if !list.contains(&name.to_string()) {
                    db.db().create_collection(name).await.unwrap();
                }
            }

            // Setup index for `accounts`
            let col = db.db().collection::<Document>("accounts");
            col.drop_indexes().await.unwrap();

            db.db()
                .run_command(doc! {
                    "createIndexes": "accounts",
                    "indexes": [
                        {
                            "key": {
                                "email": 1
                            },
                            "name": "email",
                            "unique": true,
                            "collation": {
                                "locale": "en",
                                "strength": 2
                            }
                        },
                        {
                            "key": {
                                "email_normalised": 1
                            },
                            "name": "email_normalised",
                            "unique": true,
                            "collation": {
                                "locale": "en",
                                "strength": 2
                            }
                        },
                        {
                            "key": {
                                "verification.token": 1
                            },
                            "name": "email_verification"
                        },
                        {
                            "key": {
                                "password_reset.token": 1
                            },
                            "name": "password_reset"
                        }
                    ]
                })
                .await
                .unwrap();

            // Setup index for `sessions`
            let col = db.db().collection::<Document>("sessions");
            col.drop_indexes().await.unwrap();

            db.db()
                .run_command(doc! {
                    "createIndexes": "sessions",
                    "indexes": [
                        {
                            "key": {
                                "token": 1
                            },
                            "name": "token",
                            "unique": true
                        },
                        {
                            "key": {
                                "user_id": 1
                            },
                            "name": "user_id"
                        }
                    ]
                })
                .await
                .unwrap();

            // Setup index for `mfa_tickets`
            let col = db.db().collection::<Document>("mfa_tickets");
            col.drop_indexes().await.unwrap();

            db.db()
                .run_command(doc! {
                    "createIndexes": "mfa_tickets",
                    "indexes": [
                        {
                            "key": {
                                "token": 1
                            },
                            "name": "token",
                            "unique": true
                        }
                    ]
                })
                .await
                .unwrap();
        }
    }

    if revision <= 16 {
        info!("Running migration [revision 16 / 07-07-2022]: Add `emojis` collection and Authifier migration.");

        if !db
            .db()
            .collection::<Document>("accounts")
            .list_index_names()
            .await
            .expect("list of index names")
            .contains(&"account_deletion".to_owned())
        {
            db.db()
                .run_command(doc! {
                    "createIndexes": "accounts",
                    "indexes": [
                        {
                            "key": {
                                "deletion.token": 1
                            },
                            "name": "account_deletion"
                        }
                    ]
                })
                .await
                .unwrap();
        }

        db.db()
            .create_collection("emojis")
            .await
            .expect("Failed to create emojis collection.");

        db.db()
            .run_command(doc! {
                "createIndexes": "emojis",
                "indexes": [
                    {
                        "key": {
                            "parent.id": 1_i32,
                        },
                        "name": "parent_id"
                    }
                ]
            })
            .await
            .expect("Failed to create emoji parent index.");
    }

    if revision <= 17 {
        info!("Running migration [revision 17 / 15-07-2022]: Initialise `joined_at` property on server members.");

        db.col::<Document>("server_members")
            .update_many(
                doc! {},
                doc! {
                    "$set": {
                        "joined_at": DateTime::now().try_to_rfc3339_string().expect("Failed to convert the date to rfc3339")
                    }
                },
            )
            .await
            .expect("Failed to update server members.");
    }

    if revision <= 18 {
        info!("Running migration [revision 18 / 27-02-2022]: Create author index on messages. Drop plain channel index if exists.");

        if db
            .db()
            .run_command(doc! {
                "dropIndexes": "messages",
                "index": ["channel"]
            })
            .await
            .is_err()
        {
            info!("Failed to drop `messages.channel` index but this is ok since that means it's probably gone.");
        }

        db.db()
            .run_command(doc! {
                "createIndexes": "messages",
                "indexes": [
                    {
                        "key": {
                            "author": 1_i32,
                        },
                        "name": "author"
                    }
                ]
            })
            .await
            .expect("Failed to create messages author index.");
    }

    if revision <= 19 {
        info!(
            "Running migration [revision 19 / 27-02-2023]: Create report / snapshot collections."
        );

        db.db().create_collection("safety_reports").await.unwrap();

        db.db().create_collection("safety_snapshots").await.unwrap();
    }

    if revision <= 20 {
        info!("Running migration [revision 20 / 28-02-2023]: Add index `snapshot.report_id`.");

        db.db()
            .run_command(doc! {
                "createIndexes": "safety_snapshots",
                "indexes": [
                    {
                        "key": {
                            "report_id": 1_i32
                        },
                        "name": "report_id"
                    }
                ]
            })
            .await
            .expect("Failed to create safety snapshot index.");
    }

    if revision <= 21 {
        info!("Running migration [revision 21 / 31-05-2023]: Add collection `safety_strikes`.");

        db.db().create_collection("safety_strikes").await.unwrap();
    }

    if revision <= 22 {
        info!("Running migration [revision 22 / 31-05-2023]: Add moderator_id to account strikes.");

        db.col::<Document>("safety_strikes")
            .update_many(
                doc! {},
                doc! {
                    "$set": {
                        "moderator_id": "01EX2NCWQ0CHS3QJF0FEQS1GR4"
                    }
                },
            )
            .await
            .expect("Failed to update server members.");
    }

    if revision <= 23 {
        info!("Running migration [revision 23 / 10-06-2023]: Generate discriminators for users.");

        db.db()
            .run_command(doc! {
                "dropIndexes": "users",
                "index": "username"
            })
            .await
            .expect("Failed to drop existing username index.");

        #[derive(Serialize, Deserialize)]
        struct UserInformation {
            #[serde(rename = "_id")]
            id: String,
            username: String,
        }

        let re_username = regex::Regex::new(r"^(\p{L}|[\d_.-])+$").unwrap();

        let users: Vec<UserInformation> = db
            .col::<UserInformation>("users")
            .find(doc! {})
            .await
            .unwrap()
            .map(|doc| doc.expect("id and username"))
            .collect()
            .await;

        let search_space: Vec<String> = DISCRIMINATOR_SEARCH_SPACE.iter().cloned().collect();
        let mut claimed: HashSet<String> = HashSet::new();

        for i in 0..users.len() {
            let info = &users[i];
            let mut discriminator = {
                let mut rng = rand::thread_rng();
                search_space.choose(&mut rng).unwrap()
            };

            if re_username.is_match(&info.username) {
                while claimed.contains(&format!("{}#{}", info.username, discriminator)) {
                    let new_discriminator = {
                        let mut rng = rand::thread_rng();
                        search_space.choose(&mut rng).unwrap()
                    };

                    info!(
                        "Re-rolled {} to {new_discriminator} from {discriminator}",
                        info.username
                    );

                    discriminator = new_discriminator;
                }

                claimed.insert(format!("{}#{}", info.username, discriminator));

                info!(
                    "({}/{}) Migrating user \"{}\" to #{} - compliant",
                    i + 1,
                    users.len(),
                    info.username,
                    discriminator
                );

                db.col::<UserInformation>("users")
                    .update_one(
                        doc! {
                            "_id": &info.id
                        },
                        doc! {
                            "$set": {
                                "discriminator": discriminator
                            }
                        },
                    )
                    .await
                    .unwrap();
            } else {
                let mut sanitised = info
                    .username
                    .graphemes(true)
                    .filter(|s| re_username.is_match(s))
                    .collect::<String>();

                while sanitised.len() < 2 {
                    sanitised += "_";
                }

                while claimed.contains(&format!("{}#{}", sanitised, discriminator)) {
                    let new_discriminator = {
                        let mut rng = rand::thread_rng();
                        search_space.choose(&mut rng).unwrap()
                    };

                    info!("Re-rolled {sanitised} to {new_discriminator} from {discriminator}");
                    discriminator = new_discriminator;
                }

                claimed.insert(format!("{}#{}", sanitised, discriminator));

                info!(
                    "({}/{}) Migrating user \"{}\" to #{} - sanitised: \"{}\"",
                    i + 1,
                    users.len(),
                    info.username,
                    discriminator,
                    sanitised
                );

                db.col::<UserInformation>("users")
                    .update_one(
                        doc! {
                            "_id": &info.id
                        },
                        doc! {
                            "$set": {
                                "username": sanitised,
                                "discriminator": discriminator,
                                "display_name": &info.username
                            }
                        },
                    )
                    .await
                    .unwrap();
            }
        }
    }

    if revision <= 24 {
        info!("Running migration [revision 24 / 09-06-2023]: Add collection `channel_webhooks` if not exists, update users index.");

        db.db().create_collection("channel_webhooks").await.ok();

        db.db()
            .run_command(doc! {
                "createIndexes": "users",
                "indexes": [
                    {
                        "key": {
                            "username": 1_i32
                        },
                        "name": "username",
                        "unique": false,
                        "collation": {
                            "locale": "en",
                            "strength": 2_i32
                        }
                    },
                    {
                        "key": {
                            "username": 1_i32,
                            "discriminator": 1_i32
                        },
                        "name": "username_discriminator",
                        "unique": true,
                        "collation": {
                            "locale": "en",
                            "strength": 2_i32
                        }
                    }
                ]
            })
            .await
            .expect("Failed to create username index.");
    };

    if revision <= 25 {
        info!("Running migration [revision 25 / 11-06-2023]: Add permissions to webhooks.");

        db.col::<Document>("webhooks")
            .update_many(
                doc! {},
                doc! {
                    "$set": {
                        "permissions": *DEFAULT_WEBHOOK_PERMISSIONS as i64
                    }
                },
            )
            .await
            .expect("Failed to update webhooks.");
    }

    if revision <= 25 {
        info!("Running migration [revision 25 / 15-06-2023]: Add collection `ratelimit_events` with index.");

        db.db().create_collection("ratelimit_events").await.ok();

        db.db()
            .run_command(doc! {
                "createIndexes": "ratelimit_events",
                "indexes": [
                    {
                        "key": {
                            "_id": 1_i32,
                            "target_id": 1_i32,
                            "event_type": 1_i32,
                        },
                        "name": "compound_key"
                    }
                ]
            })
            .await
            .expect("Failed to create ratelimit_events index.");
    }

    if revision <= 26 {
        // Need to migrate fields on attachments, change `user_id`, `object_id`, etc to `parent`.
        info!("Running migration [revision 26 / 15-05-2024]: fix invites being incorrectly serialized with wrong enum tagging.");

        auto_derived!(
            pub enum OldInvite {
                Server {
                    #[serde(rename = "_id")]
                    code: String,
                    server: String,
                    creator: String,
                    channel: String,
                },
                Group {
                    #[serde(rename = "_id")]
                    code: String,
                    creator: String,
                    channel: String,
                },
            }
        );

        #[derive(serde::Serialize, serde::Deserialize)]
        struct Outer {
            _id: ObjectId,
            #[serde(flatten)]
            invite: OldInvite,
        }

        let invites = db
            .db()
            .collection::<Outer>("channel_invites")
            .find(doc! {
                "type": { "$exists": false }
            })
            .await
            .expect("failed to find invites")
            .filter_map(|s| async { s.ok() })
            .collect::<Vec<Outer>>()
            .await
            .into_iter()
            .map(|invite| match invite.invite {
                OldInvite::Server {
                    code,
                    server,
                    creator,
                    channel,
                } => Invite::Server {
                    code,
                    server,
                    creator,
                    channel,
                    max_uses: None,
                    uses: 0,
                    expires: None,
                    vanity: false,
                },
                OldInvite::Group {
                    code,
                    creator,
                    channel,
                } => Invite::Group {
                    code,
                    creator,
                    channel,
                    max_uses: None,
                    uses: 0,
                    expires: None,
                },
            })
            .collect::<Vec<Invite>>();

        if !invites.is_empty() {
            db.db()
                .collection("channel_invites")
                .insert_many(invites)
                .await
                .expect("failed to insert corrected invite");

            db.db()
                .collection::<Outer>("channel_invites")
                .delete_many(doc! {
                    "type": { "$exists": false }
                })
                .await
                .expect("failed to find invites");
        }
    }

    if revision <= 27 {
        info!("Running migration [revision 27 / 21-07-2024]: create message pinned index.");

        db.db()
            .run_command(doc! {
                "createIndexes": "messages",
                "indexes": [
                    {
                        "key": {
                            "channel": 1_i32,
                            "pinned": 1_i32
                        },
                        "name": "channel_pinned_compound"
                    }
                ]
            })
            .await
            .expect("Failed to create message index.");
    }

    if revision <= 28 {
        info!("Running migration [revision 28 / 10-09-2024]: Add support for new Autumn.");

        db.db().create_collection("attachment_hashes").await.ok();

        db.db()
            .run_command(doc! {
                "createIndexes": "attachments",
                "indexes": [
                    {
                        "key": {
                            "hash": 1_i32
                        },
                        "name": "hash"
                    }
                ]
            })
            .await
            .expect("Failed to create attachments index.");

        db.db()
            .run_command(doc! {
                "createIndexes": "attachment_hashes",
                "indexes": [
                    {
                        "key": {
                            "processed_hash": 1_i32
                        },
                        "name": "processed_hash"
                    }
                ]
            })
            .await
            .expect("Failed to create attachment_hashes index.");
    }

    // Revision 29 omitted due to bug.

    if revision <= 30 {
        info!("Running migration [revision 30 / 29-09-2024]: Add index for used_for.id to attachments.");

        db.db()
            .run_command(doc! {
                "createIndexes": "attachments",
                "indexes": [
                    {
                        "key": {
                            "used_for.id": 1_i32
                        },
                        "name": "used_for_id"
                    }
                ]
            })
            .await
            .expect("Failed to create attachments index.");
    }

    if revision <= 31 {
        info!("Running migration [revision 31 / 31-10-2024]: Add creator_id to webhooks and delete those whose channels don't exist.");

        #[derive(serde::Serialize, serde::Deserialize)]
        struct WebhookShell {
            _id: String,
            channel_id: String,
        }

        #[allow(clippy::enum_variant_names)]
        #[derive(serde::Serialize, serde::Deserialize)]
        enum Channel {
            Group { owner: String },
            TextChannel { server: String },
            VoiceChannel { server: String },
        }

        let webhooks = db
            .db()
            .collection::<WebhookShell>("channel_webhooks")
            .find(doc! {})
            .await
            .expect("webhooks")
            .filter_map(|s| async { s.ok() })
            .collect::<Vec<WebhookShell>>()
            .await;

        for webhook in webhooks {
            match db
                .col::<Channel>("channels")
                .find_one(doc! { "_id": &webhook.channel_id })
                .await
                .unwrap()
            {
                Some(channel) => {
                    let creator_id = match channel {
                        Channel::Group { owner, .. } => owner,
                        Channel::TextChannel { server, .. }
                        | Channel::VoiceChannel { server, .. } => {
                            let server = db.fetch_server(&server).await.expect("server");
                            server.owner
                        }
                    };

                    db.db()
                        .collection::<Document>("channel_webhooks")
                        .update_one(
                            doc! {
                                "_id": webhook._id,
                            },
                            doc! {
                                "$set" : {
                                    "creator_id": creator_id
                                }
                            },
                        )
                        .await
                        .expect("update webhook");
                }
                None => {
                    db.db()
                        .collection::<WebhookShell>("channel_webhooks")
                        .delete_one(doc! { "_id": webhook._id })
                        .await
                        .expect("failed to delete invalid webhook");
                }
            }
        }
    }

    if revision <= 32 {
        info!(
            "Running migration [revision 32 / 12-05-2025]: (Authifier) Add last_seen to sessions."
        );

        loop {
            #[derive(Deserialize)]
            struct SessionId {
                _id: String,
            }

            let sessions: Vec<SessionId> = db
                .db()
                .collection("sessions")
                .find(doc! {
                    "$or": [
                        { "last_seen": { "$exists": false } },
                        { "last_seen": "1970-01-01T00:00:00.000Z" }
                    ]
                })
                .limit(50_000) // about 400 batches for 2 million
                .await
                .expect("Failed to create cursor for sessions!")
                .map(|doc| doc.expect("id and username"))
                .collect()
                .await;

            if sessions.is_empty() {
                break;
            }

            for session in sessions {
                let timestamp = iso8601_timestamp::Timestamp::from(
                    Ulid::from_string(&session._id).unwrap().datetime(),
                );

                db.db()
                    .collection::<Document>("sessions")
                    .update_one(
                        doc! {
                            "_id": &session._id.to_string(),
                        },
                        doc! {
                            "$set": {
                                "last_seen": timestamp.format().to_string()
                            }
                        },
                    )
                    .await
                    .expect("Failed to update a session.");
            }
        }
    }

    if revision <= 40 {
        info!(
            "Running migration [revision |> 40 / 30-05-2025]: Set last policy acknowlegement date to now and create policy changes collection."
        );

        db.db()
            .create_collection("policy_changes")
            .await
            .expect("Failed to create policy_changes collection.");

        db.db()
            .collection::<User>("users")
            .update_many(
                doc! {},
                doc! {
                    "$set": {
                        "last_acknowledged_policy_change": to_bson(&Timestamp::now_utc())
                            .expect("failed to serialise timestamp")
                    }
                },
            )
            .await
            .expect("failed to update users");
    }

    if revision <= 43 {
        info!(
            "Running migration [revision 43 / 05-06-2025]: convert role ranks to uniform numbers."
        );

        #[derive(Serialize, Deserialize, Clone)]
        struct Role {
            pub rank: i64,
        }

        #[derive(Serialize, Deserialize, Clone)]
        struct Server {
            #[serde(rename = "_id")]
            pub id: String,
            #[serde(default = "HashMap::<String, Role>::new")]
            pub roles: HashMap<String, Role>,
        }

        let mut servers = db
            .db()
            .collection::<Server>("servers")
            .find(doc! {
                "roles": {
                    "$exists": true,
                    "$ne": []
                }
            })
            .await
            .unwrap()
            .filter_map(|s| async { s.ok() })
            .boxed();

        while let Some(server) = servers.next().await {
            let mut ordered_roles = server.roles.clone().into_iter().collect::<Vec<_>>();
            ordered_roles.sort_by(|(_, role_a), (_, role_b)| role_a.rank.cmp(&role_b.rank));
            let ordered_roles = ordered_roles
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();

            let mut doc = doc! {};

            for id in server.roles.keys() {
                doc.insert(
                    format!("roles.{id}.rank"),
                    ordered_roles.iter().position(|x| id == x).unwrap() as i64,
                );
            }

            db.db()
                .collection::<Server>("servers")
                .update_one(doc! { "_id": &server.id }, doc! { "$set": doc })
                .await
                .unwrap();
        }
    }

    if revision <= 46 {
        info!("Running migration [revision 46 / 29-04-2025]: Convert all `VoiceChannel`'s into `TextChannel`");

        db.col::<Document>("channels")
            .update_many(
                doc! { "channel_type": "VoiceChannel" },
                doc! {
                    "$set": {
                        "channel_type": "TextChannel",
                        "voice": {}
                    }
                },
            )
            .await
            .expect("Failed to update voice channels");
    };

    if revision <= 48 {
        info!("Running migration [revision 48 / 22-10-2025]: Add Video + Listen to default permissions");

        db.col::<Document>("servers")
            .update_many(
                doc! { },
                doc! {
                    "$bit": {
                        "default_permissions": {
                            "or": (ChannelPermission::Video + ChannelPermission::Speak + ChannelPermission::Listen) as i64
                        },
                    }
                }
            )
            .await
            .expect("Failed to update default_permissions");
    };

    if revision <= 49 {
        info!("Running migration [revision 49 / 12-12-2025]: Add _id key to roles");

        #[derive(Serialize, Deserialize, Clone)]
        struct Server {
            #[serde(rename = "_id")]
            pub id: String,
            #[serde(default = "HashMap::<String, Document>::new")]
            pub roles: HashMap<String, Document>,
        }

        let mut servers = db
            .db()
            .collection::<Server>("servers")
            .find(doc! {
                "roles": {
                    "$exists": true,
                    "$ne": {}
                }
            })
            .await
            .unwrap()
            .map(|res| res.expect("Failed to decode Server { id, roles }"));

        while let Some(server) = servers.next().await {
            let mut doc = doc! {};

            for id in server.roles.keys() {
                doc.insert(format!("roles.{id}._id"), id);
            }

            db.db()
                .collection::<Server>("servers")
                .update_one(doc! { "_id": &server.id }, doc! { "$set": doc })
                .await
                .unwrap();
        }
    };

    if revision <= 50 {
        info!("Running migration [revision 50 / 13-04-2026]: Rename invites collection to account_invites");

        let result = db
            .db()
            .client()
            .database("admin")
            .run_command(doc! {
                "renameCollection": "revolt.invites",
                "to": "revolt.account_invites",
                "dropTarget": true
            })
            .await;

        if let Err(e) = result {
            // NamespaceNotFound (26) = source collection doesn't exist, safe to ignore
            if !matches!(e.kind.as_ref(), mongodb::error::ErrorKind::Command(ce) if ce.code == 26) {
                panic!("Failed to rename invites collection: {e}");
            }
        }
    }

    if revision <= 51 {
        info!("Running migration [revision 51 / 28-11-2025]: Add audit logs collection");

        db.db()
            .create_collection("audit_logs")
            .await
            .expect("Failed to create audit_logs collection");

        db.db()
            .run_command(doc! {
                "createIndexes": "audit_logs",
                "indexes": [
                    {
                        "key": {
                            "expires_at": 1_i32,
                        },
                        "name": "expires_at_ttl",
                        "expireAfterSeconds": 0
                    },
                    {
                        "key": {
                            "server": 1_i32,
                            "user": 1_i32,
                            "action.type": 1_i32,
                        },
                        "name": "audit_log_filters",
                    },
                ]
            })
            .await
            .expect("Failed to create audit_logs index");
    };

    if revision <= 52 {
        let config = revolt_config::config().await;
        if config.production {
            info!("Running migration [revision 52 / 20-08-2026]: Discover endpoints");
            db.db()
                .create_collection("discover_requests")
                .await
                .expect("Failed to create discover_requests collection");

            db.db()
                .run_command(doc! {
                    "createIndexes": "discover_requests",
                    "indexes": [
                        {
                            "key": {
                                "request_type": 1,
                                "request_id": 1
                            },
                            "name": "request_type_id"
                        }
                    ]
                })
                .await
                .expect("Failed to create index");
        } else {
            info!("Skipping migration [revision 52 / 20-08-2026]: Discover endpoints");
        }
    }

    if revision <= 53 {
        info!(
            "Running migration [revision 53 / 30-08-2026]: [FIX] clean bad last_channel_id values"
        );
        db.db()
            .collection::<Document>("channels")
            .update_many(
                doc! {"last_message_id": {"$regex": "\""}},
                vec![
                    doc! {"$set": {"last_message_id": {"$trim": {"input": "$last_message_id", "chars": "\""}}}},
                ],
            )
            .await
            .expect("Failed to clean up channels");
    }

    if revision <= 54 {
        info!("Running migration [revision 54 / 14-09-2026]: Add UseExternalEmojis to default permissions");

        db.col::<Document>("servers")
            .update_many(
                doc! {},
                doc! {
                "$bit": {
                    "default_permissions": {
                        "or": ChannelPermission::UseExternalEmojis as i64
                    },
                },
            },
            )
            .await
            .expect("Failed to update default_permissions");
    };

    if revision <= 55 {
        info!("Running migration [revision 55 / 26-10-2026]: Community designs review");
        migrate_community_review(db).await;
    }

    if revision <= 56 {
        info!("Running migration [revision 56 / 26-10-2026]: Lookup indexes for channels, emojis, bots and billing");
        create_lookup_indexes(&db.db()).await;
    }

    if revision <= 57 {
        info!("Running migration [revision 57 / 26-10-2026]: Developer platform indexes");
        create_developer_platform_indexes(&db.db()).await;
    }

    if revision <= 58 {
        info!("Running migration [revision 58 / 26-10-2026]: Trust & Safety indexes");
        create_trust_safety_indexes(&db.db()).await;
    }

    if revision <= 59 {
        info!("Running migration [revision 59 / 26-10-2026]: Payment provider indexes");
        create_payment_indexes(&db.db()).await;
    }

    if revision <= 60 {
        info!("Running migration [revision 60 / 27-10-2026]: OAuth2 and app install indexes");
        create_oauth_indexes(&db.db()).await;
    }

    if revision <= 61 {
        info!("Running migration [revision 61 / 01-11-2026]: Platform staff indexes");
        create_platform_staff_indexes(&db.db()).await;
    }

    if revision <= 62 {
        info!("Running migration [revisions 62-65 / 02-11-2026]: Bot verification, account change, notice and support ticket indexes");
        create_staff_feature_indexes(&db.db()).await;
    }

    // The bot migrations follow the platform staff migrations (61-65).
    // Each bot migration is safe to run again.

    if revision <= 66 {
        info!("Running migration [revision 66 / 02-11-2026]: Bots keep the privileged gateway intents");
        // Bots made before gateway intents could read message content, the
        // member list and presence; keep that, so none of them breaks. New
        // bots start with none and their owner switches them on.
        db.col::<Document>("bots")
            .update_many(
                doc! { "privileged_intents": { "$exists": false } },
                doc! { "$set": { "privileged_intents": ((1_i64 << 10) | (1_i64 << 11) | (1_i64 << 12)) } },
            )
            .await
            .expect("Failed to update bots.");
    }

    if revision <= 67 {
        info!("Running migration [revision 67 / 02-11-2026]: Store bot tokens only as hashes");
        // Every bot keeps working with the token it has: the token is hashed
        // in place and the plain one is removed from the database. A bot that
        // already has a hash keeps it (never hash a hash); only its plain
        // token is removed.
        let bots = db.col::<Document>("bots");
        let mut cursor = bots
            .find(doc! { "token": { "$exists": true } })
            .await
            .expect("Failed to read bots");
        while let Some(Ok(bot)) = cursor.next().await {
            let (Ok(id), Ok(token)) = (bot.get_str("_id"), bot.get_str("token")) else {
                continue;
            };
            let update = if bot.get_str("token_hash").is_ok_and(|hash| !hash.is_empty()) {
                doc! { "$unset": { "token": 1_i32 } }
            } else {
                doc! {
                    "$set": { "token_hash": crate::hash_token(token) },
                    "$unset": { "token": 1_i32 }
                }
            };
            bots.update_one(doc! { "_id": id }, update)
                .await
                .expect("Failed to hash a bot token");
        }
        create_bot_token_index(&db.db()).await;
    }

    if revision <= 68 {
        info!("Running migration [revision 68 / 02-11-2026]: Bots past 100 servers keep their intents");
        // Past 100 servers only privileged intents approved by Trust & Safety
        // stay on. Bots already that big keep what they have today; a bot
        // that already has approved intents is left alone.
        let members = db.col::<Document>("server_members");
        let bots = db.col::<Document>("bots");
        let mut cursor = bots
            .find(doc! { "approved_intents": { "$exists": false } })
            .await
            .expect("Failed to read bots");
        while let Some(Ok(bot)) = cursor.next().await {
            let Ok(id) = bot.get_str("_id") else {
                continue;
            };
            let servers = members
                .count_documents(doc! { "_id.user": id })
                .await
                .unwrap_or(0);
            if servers > crate::UNVERIFIED_BOT_SERVER_LIMIT as u64 {
                let intents = bot
                    .get_i64("privileged_intents")
                    .or_else(|_| bot.get_i32("privileged_intents").map(i64::from))
                    .unwrap_or(0);
                bots.update_one(
                    doc! { "_id": id },
                    doc! { "$set": { "approved_intents": intents } },
                )
                .await
                .expect("Failed to approve a bot's intents");
            }
        }
    }

    if revision <= 69 {
        info!("Running migration [revision 69 / 03-11-2026]: Thread lookups by parent channel");
        create_thread_indexes(&db.db()).await;
    }

    if revision <= 70 {
        info!("Running migration [revision 70 / 04-11-2026]: Data export indexes");
        create_data_export_indexes(&db.db()).await;
    }

    if revision <= 71 {
        info!("Running migration [revision 71 / 04-11-2026]: Statements of reasons on moderation actions");
        fill_action_statements(&db.db()).await;
    }

    if revision <= 72 {
        info!("Running migration [revision 72 / 04-11-2026]: Store webhook tokens only as hashes");
        hash_webhook_tokens(&db.db()).await;
    }

    if revision <= 73 {
        info!("Running migration [revision 73 / 05-11-2026]: Sessions from before expiry get a full idle period");
        let idle_days = revolt_config::config().await.api.sessions.idle_days;
        let until = session_grace_until(Timestamp::now_utc(), idle_days);
        give_sessions_expiry_grace(&db.db(), until).await;
    }

    if revision <= 74 {
        info!("Running migration [revision 74 / 06-11-2026]: Thread and forum permissions");
        grant_thread_permissions(&db.db()).await;
    }

    if revision <= PENDING_SESSION_EXPIRY_FILL {
        info!("Running migration [revision {PENDING_SESSION_EXPIRY_FILL} / 05-11-2026]: Sessions written during the expiry rollout get their date");
        let idle_days = revolt_config::config().await.api.sessions.idle_days;
        let until = session_grace_until(Timestamp::now_utc(), idle_days);
        give_sessions_expiry_grace(&db.db(), until).await;
    }

    if revision <= PENDING_OFFICIAL_INDEXES {
        info!("Running migration [revision {PENDING_OFFICIAL_INDEXES} / 06-11-2026]: Indexes for official accounts");
        create_official_indexes(&db.db()).await;
    }

    if revision <= PENDING_LIST_INDEXES {
        info!("Running migration [revision {PENDING_LIST_INDEXES} / 06-11-2026]: Indexes for Designs, Discover and file deletion");
        create_list_indexes(&db.db()).await;
    }

    if revision <= NO_OP_REVISION_77 {
        // Held indexes of profile comments, removed before any production database ran it
        info!("Running migration [revision {NO_OP_REVISION_77} / 06-11-2026]: No-op");
    }

    if revision <= PENDING_PLATFORM_WORD_SEED {
        info!("Running migration [revision {PENDING_PLATFORM_WORD_SEED} / 06-11-2026]: Starter entries of the platform word list");
        seed_platform_words(&db.db()).await;
    }

    if revision <= PENDING_PLATFORM_WORD_SEED_2 {
        info!("Running migration [revision {PENDING_PLATFORM_WORD_SEED_2} / 06-11-2026]: Second version of the starter word list");
        update_platform_word_seed(&db.db()).await;
    }

    if revision <= PENDING_COMMAND_PERMISSIONS_INDEX {
        info!("Running migration [revision {PENDING_COMMAND_PERMISSIONS_INDEX} / 10-11-2026]: Index for command permission rules");
        create_command_permission_indexes(&db.db()).await;
    }

    if revision <= PENDING_POLL_INDEX {
        info!("Running migration [revision {PENDING_POLL_INDEX} / 10-11-2026]: Index of open polls");
        create_poll_index(&db.db()).await;
    }

    if revision <= PENDING_SERVER_EVENT_INDEXES {
        info!("Running migration [revision {PENDING_SERVER_EVENT_INDEXES} / 10-11-2026]: Indexes for scheduled server events");
        create_server_event_indexes(&db.db()).await;
    }

    if revision <= PENDING_STATUS_MUTUAL_INDEXES {
        info!("Running migration [revision {PENDING_STATUS_MUTUAL_INDEXES} / 10-11-2026]: Indexes for status expiry and mutual friends");
        create_status_mutual_indexes(&db.db()).await;
    }

    if revision <= PENDING_SERVER_INSIGHTS {
        info!("Running migration [revision {PENDING_SERVER_INSIGHTS} / 10-11-2026]: Indexes for server insights");
        create_server_insight_indexes(&db.db()).await;
    }

    if revision <= PENDING_SOUNDBOARD {
        info!("Running migration [revision {PENDING_SOUNDBOARD} / 10-11-2026]: Soundboard index, and every server's members may use the soundboard");
        create_soundboard(&db.db()).await;
    }

    if revision <= PENDING_SESSION_HASHES {
        info!("Running migration [revision {PENDING_SESSION_HASHES} / 11-11-2026]: sessions keep only a hash of their token; the old sessions end");
        end_sessions_without_hash(&db.db()).await;
    }

    if revision <= PENDING_SESSION_TOKEN_INDEX {
        info!("Running migration [revision {PENDING_SESSION_TOKEN_INDEX} / 12-11-2026]: sessions are unique by token hash, not by the token they no longer keep");
        index_sessions_by_token_hash(&db.db()).await;
    }

    if revision <= PENDING_LINKED_ACCOUNT_INDEXES {
        info!("Running migration [revision {PENDING_LINKED_ACCOUNT_INDEXES} / 14-11-2026]: Indexes for linked accounts (Steam)");
        create_linked_account_indexes(&db.db()).await;
    }

    if revision <= PENDING_SAVED_REMINDER_INDEXES {
        info!("Running migration [revision {PENDING_SAVED_REMINDER_INDEXES} / 19-11-2026]: Indexes for saved messages and message reminders");
        create_saved_reminder_indexes(&db.db()).await;
    }

    if revision <= PENDING_SERVER_EVENT_ANSWERS {
        info!("Running migration [revision {PENDING_SERVER_EVENT_ANSWERS} / 18-11-2026]: Event answers: every Interested mark becomes maybe, counts are set again from the answers");
        let report = migrate_server_event_answers(&db.db()).await;
        info!("Event answers: {report:?}");
    }

    if revision <= PENDING_TEAM_VOLUNTEER_INDEXES {
        info!("Running migration [revision {PENDING_TEAM_VOLUNTEER_INDEXES} / 20-11-2026]: Indexes for team volunteers");
        create_team_volunteer_indexes(&db.db()).await;
    }

    if revision <= PENDING_ACTIVITY_BLOCKS_INDEX {
        info!("Running migration [revision {PENDING_ACTIVITY_BLOCKS_INDEX} / 23-11-2026]: Indexes for activity blocks");
        create_activity_block_indexes(&db.db()).await;
    }

    if revision <= PENDING_OFFICIAL_BOT_FLAGS {
        info!("Running migration [revision {PENDING_OFFICIAL_BOT_FLAGS} / 24-11-2026]: The official bot mark moves to the bot's Official flag");
        fill_official_bot_flags(&db.db()).await;
    }

    if revision <= PENDING_BADGE_GATE_INDEXES {
        info!("Running migration [revision {PENDING_BADGE_GATE_INDEXES} / 24-11-2026]: Indexes for badge holders and badge change requests");
        create_badge_gate_indexes(&db.db()).await;
    }

    // Reminder to update LATEST_REVISION when adding new migrations.
    LATEST_REVISION.max(revision)
}

/// Team volunteers: one membership per person and role, activity days by
/// person and day. Only indexes, no data moves. Safe to run twice: creating
/// an index that exists is a no-op.
pub async fn create_team_volunteer_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "team_members",
        "indexes": [
            { "key": { "user_id": 1_i32, "role": 1_i32 }, "name": "user_role", "unique": true }
        ]
    })
    .await
    .expect("Failed to create the team member index");
    db.run_command(doc! {
        "createIndexes": "team_activity_days",
        "indexes": [
            { "key": { "user_id": 1_i32, "day": 1_i32 }, "name": "user_day" },
            { "key": { "day": 1_i32 }, "name": "day" }
        ]
    })
    .await
    .expect("Failed to create the team activity indexes");
    db.run_command(doc! {
        "createIndexes": "team_identities",
        "indexes": [
            { "key": { "purge_at": 1_i32 }, "name": "purge_at", "sparse": true }
        ]
    })
    .await
    .expect("Failed to create the team identity index");
}

/// `BotFlags::Official` becomes the one source of the official bot mark.
/// Until now `zrzur-staff official-bot` wrote only the account's
/// `official_bot`; every bot whose account has it gets the flag (Discover,
/// deletion protection and bot edits read the flag). Other flags stay. Safe
/// to run twice: OR-ing a set bit changes nothing.
pub async fn fill_official_bot_flags(db: &mongodb::Database) {
    let official = revolt_models::v0::BotFlags::Official as i32;
    let mut marked = db
        .collection::<Document>("users")
        .find(doc! { "official_bot": true, "bot": { "$exists": true } })
        .projection(doc! { "_id": 1_i32 })
        .await
        .expect("Failed to read official bot accounts");
    let mut ids = vec![];
    while let Some(user) = marked.next().await {
        let user = user.expect("Failed to read an official bot account");
        if let Ok(id) = user.get_str("_id") {
            ids.push(id.to_string());
        }
    }
    let bots = db.collection::<Document>("bots");
    for id in ids {
        bots.update_one(doc! { "_id": &id }, doc! { "$bit": { "flags": { "or": official } } })
            .await
            .expect("Failed to set the Official flag");
    }
}

/// Badge gate: every holder of one badge (appointment gaps) and the open
/// request for one change (two-person dedupe). Only indexes, no data moves.
/// Safe to run twice: creating an index that exists is a no-op.
pub async fn create_badge_gate_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "badge_grants",
        "indexes": [
            { "key": { "badge": 1_i32, "revoked_at": 1_i32 }, "name": "badge_held" }
        ]
    })
    .await
    .expect("Failed to create the badge holder index");
    db.run_command(doc! {
        "createIndexes": "badge_change_requests",
        "indexes": [
            { "key": { "status": 1_i32, "user_id": 1_i32, "badge": 1_i32, "grant": 1_i32 }, "name": "open_change" }
        ]
    })
    .await
    .expect("Failed to create the badge request index");
}

/// Activity blocks (Trust & Safety) are looked up by value on every
/// `SetActivity` and by case when an action is reversed. Only indexes, no
/// data moves. Safe to run twice: creating an index that exists is a no-op.
pub async fn create_activity_block_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "activity_blocks",
        "indexes": [
            { "key": { "value": 1_i32, "kind": 1_i32 }, "name": "value_kind" },
            { "key": { "case_id": 1_i32 }, "name": "case_id", "sparse": true }
        ]
    })
    .await
    .expect("Failed to create the activity block indexes");
}

/// Saved messages are listed by person, newest first. Reminders are listed
/// by person, and crond finds the waiting ones that are due. Only indexes,
/// no data moves. Safe to run twice: creating an index that exists is a
/// no-op.
pub async fn create_saved_reminder_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "saved_messages",
        "indexes": [
            { "key": { "user_id": 1_i32, "saved_at": -1_i32 }, "name": "user_newest" }
        ]
    })
    .await
    .expect("Failed to create the saved message index");
    db.run_command(doc! {
        "createIndexes": "message_reminders",
        "indexes": [
            { "key": { "user_id": 1_i32, "due_at": 1_i32 }, "name": "user_due" },
            { "key": { "delivered_at": 1_i32, "due_at": 1_i32 }, "name": "waiting_due" }
        ]
    })
    .await
    .expect("Failed to create the message reminder indexes");
}

/// Indexes for answers to events: the attendee list of one answer (paged by
/// user), a server's events by start (calendar and "my events"), events by
/// creator (the copy of a person's data) and seat lists by server (deleted
/// with it). Creating an index that exists does nothing.
pub async fn create_server_event_answer_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        (
            "server_event_interests",
            doc! {
                "key": { "_id.event": 1_i32, "answer": 1_i32, "_id.user": 1_i32 },
                "name": "event_answer_user"
            },
        ),
        (
            "server_event_interests",
            doc! { "key": { "server": 1_i32 }, "name": "server" },
        ),
        (
            "server_events",
            doc! { "key": { "server": 1_i32, "starts_at": 1_i32 }, "name": "server_start" },
        ),
        (
            "server_events",
            doc! { "key": { "creator": 1_i32 }, "name": "creator" },
        ),
        (
            "server_event_rosters",
            doc! { "key": { "server": 1_i32 }, "name": "server" },
        ),
        (
            "server_events",
            doc! { "key": { "invited": 1_i32 }, "name": "invited", "sparse": true },
        ),
        (
            "server_events",
            doc! { "key": { "status": 1_i32, "ended_at": 1_i32 }, "name": "status_ended" },
        ),
        (
            "server_events",
            doc! { "key": { "co_organisers": 1_i32 }, "name": "co_organisers", "sparse": true },
        ),
        // a share link code belongs to one event
        (
            "server_events",
            doc! {
                "key": { "link.code": 1_i32 },
                "name": "link_code",
                "unique": true,
                "partialFilterExpression": { "link.code": { "$exists": true } }
            },
        ),
        (
            "server_events",
            doc! {
                "key": { "invitations.code": 1_i32 },
                "name": "invitation_code",
                "unique": true,
                "partialFilterExpression": { "invitations.code": { "$exists": true } }
            },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// What the releases before left behind: events (with their marks and seat
/// lists) of servers that were deleted, and marks of people who are no
/// longer members of the server (they left through a deleted account).
///
/// With `remove: false` it only counts them. With `remove: true` it deletes
/// them; nothing of a server that exists and of a person who is a member
/// there is touched. Returns (events, marks) found.
///
/// Nothing calls this with `remove: true`: the migration keeps every old
/// event and mark (owner, 2026-11-18). It is here for a deliberate run once
/// a retention period has passed or a deletion is due.
pub async fn remove_orphaned_server_event_data(
    db: &crate::mongodb::Database,
    remove: bool,
) -> (u64, u64) {
    let events = db.collection::<Document>("server_events");
    let rows = db.collection::<Document>("server_event_interests");
    let rosters = db.collection::<Document>("server_event_rosters");
    let servers = db.collection::<Document>("servers");
    let members = db.collection::<Document>("server_members");
    let (mut gone_events, mut gone_rows) = (0, 0);

    let mut known: std::collections::HashMap<String, bool> = Default::default();
    let mut cursor = events
        .find(doc! {})
        .projection(doc! { "_id": 1_i32, "server": 1_i32 })
        .await
        .expect("Failed to read the events");
    let mut orphans = Vec::new();
    while let Some(event) = cursor.next().await {
        let event = event.expect("Failed to read an event");
        let (Ok(id), Ok(server)) = (event.get_str("_id"), event.get_str("server")) else {
            continue;
        };
        let exists = match known.get(server) {
            Some(exists) => *exists,
            None => {
                let exists = servers
                    .count_documents(doc! { "_id": server })
                    .await
                    .expect("Failed to look for a server")
                    > 0;
                known.insert(server.to_string(), exists);
                exists
            }
        };
        if !exists {
            orphans.push(id.to_string());
        }
    }
    let orphaned: std::collections::HashSet<String> = orphans.iter().cloned().collect();
    for id in orphans {
        if !remove {
            gone_events += 1;
            gone_rows += rows
                .count_documents(doc! { "_id.event": &id })
                .await
                .expect("Failed to count the marks of an orphaned event");
            continue;
        }
        gone_rows += rows
            .delete_many(doc! { "_id.event": &id })
            .await
            .expect("Failed to remove the marks of an orphaned event")
            .deleted_count;
        rosters
            .delete_one(doc! { "_id": &id })
            .await
            .expect("Failed to remove the seat list of an orphaned event");
        gone_events += events
            .delete_one(doc! { "_id": &id })
            .await
            .expect("Failed to remove an orphaned event")
            .deleted_count;
    }

    let mut cursor = rows
        .find(doc! {})
        .projection(doc! { "_id": 1_i32, "server": 1_i32 })
        .await
        .expect("Failed to read the marks");
    let mut left = Vec::new();
    while let Some(row) = cursor.next().await {
        let row = row.expect("Failed to read a mark");
        let (Ok(key), Ok(server)) = (row.get_document("_id"), row.get_str("server")) else {
            continue;
        };
        let Ok(user) = key.get_str("user") else { continue };
        // counted with its event above
        if key.get_str("event").is_ok_and(|event| orphaned.contains(event)) {
            continue;
        }
        let member = members
            .count_documents(doc! { "_id.server": server, "_id.user": user })
            .await
            .expect("Failed to look for a member")
            > 0;
        if !member {
            left.push((key.clone(), user.to_string()));
        }
    }
    for (key, user) in left {
        if !remove {
            gone_rows += 1;
            continue;
        }
        gone_rows += rows
            .delete_one(doc! { "_id": key.clone() })
            .await
            .expect("Failed to remove the mark of someone who left")
            .deleted_count;
        if let Ok(event) = key.get_str("event") {
            rosters
                .update_one(
                    doc! { "_id": event },
                    doc! { "$pull": { "going": &user, "waiting": &user } },
                )
                .await
                .expect("Failed to take someone who left off a seat list");
        }
    }
    (gone_events, gone_rows)
}

/// What one run of `migrate_server_event_answers` changed
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ServerEventAnswerMigration {
    /// "Interested" marks that became "maybe" answers
    pub marks_turned_into_maybe: u64,
    /// Events whose counts were written differently than they stood
    pub events_recounted: u64,
    /// Events that exist and were looked at
    pub events_kept: u64,
    /// Events still scheduled, running or in draft whose server no longer
    /// exists: called off (not deleted), so that they fall under the
    /// retention time
    pub events_of_deleted_servers_called_off: u64,
    /// How often the counts had to be written until the rows stood still
    /// (1 when nobody else wrote meanwhile)
    pub count_passes: u32,
    /// Events of servers that no longer exist: found and kept
    pub orphaned_events_found: u64,
    /// Marks of such events, and of people who are no longer members of
    /// the server: found and kept
    pub orphaned_marks_found: u64,
}

/// Events get answers (going, maybe, not going) in place of the single
/// "Interested" mark. Every mark is kept and becomes "maybe", and each
/// event's counts are set again from its rows (and its seat list, when it
/// has one). Nothing is deleted: what earlier releases left behind (events
/// of deleted servers, marks of deleted accounts) is counted and reported,
/// and stays until a deliberate clean-up. A second run finds nothing left
/// to change and reports zero changes.
pub async fn migrate_server_event_answers(
    db: &crate::mongodb::Database,
) -> ServerEventAnswerMigration {
    create_server_event_answer_indexes(db).await;

    let mut report = ServerEventAnswerMigration::default();
    // counted only: nothing is removed here
    let (events, marks) = remove_orphaned_server_event_data(db, false).await;
    report.orphaned_events_found = events;
    report.orphaned_marks_found = marks;

    let rows = db.collection::<Document>("server_event_interests");
    report.marks_turned_into_maybe = rows
        .update_many(
            doc! { "answer": { "$exists": false } },
            doc! { "$set": { "answer": "Maybe", "waiting": false } },
        )
        .await
        .expect("Failed to turn Interested marks into answers")
        .modified_count;

    report.events_of_deleted_servers_called_off = call_off_events_of_deleted_servers(db).await;

    // The counts are written from a snapshot of the rows. If the rows
    // changed meanwhile (an API process is taking answers), the counts
    // would be wrong for good: counted again, a few times, and then the
    // migration stops with a clear word instead of leaving them wrong.
    loop {
        report.count_passes += 1;
        let wanted = server_event_counts_snapshot(db).await;
        let (kept, changed) = write_server_event_counts(db, &wanted).await;
        report.events_kept = kept;
        report.events_recounted += changed;
        if server_event_counts_snapshot(db).await == wanted {
            break;
        }
        if report.count_passes >= MAX_COUNT_PASSES {
            panic!(
                "Event answers changed {MAX_COUNT_PASSES} times while the migration counted them: \
                 an API process is still taking answers. Stop every API process, then start again; \
                 the migration is safe to run again."
            );
        }
    }
    report
}

/// How often the migration counts again before it gives up
const MAX_COUNT_PASSES: u32 = 3;

/// Events still to come, running or in draft whose server no longer exists
/// are called off at this moment. Nothing is deleted: they and their answers
/// then fall under the retention time like any other ended event. Events of
/// servers that exist are not touched. Returns how many were called off.
pub async fn call_off_events_of_deleted_servers(db: &crate::mongodb::Database) -> u64 {
    let events = db.collection::<Document>("server_events");
    let servers = db.collection::<Document>("servers");
    let mut known: std::collections::HashMap<String, bool> = Default::default();
    let mut cursor = events
        .find(doc! { "status": { "$in": ["Scheduled", "Active", "Draft"] } })
        .projection(doc! { "_id": 1_i32, "server": 1_i32 })
        .await
        .expect("Failed to read the events");
    let mut orphans = Vec::new();
    while let Some(event) = cursor.next().await {
        let event = event.expect("Failed to read an event");
        let (Ok(id), Ok(server)) = (event.get_str("_id"), event.get_str("server")) else {
            continue;
        };
        let exists = match known.get(server) {
            Some(exists) => *exists,
            None => {
                let exists = servers
                    .count_documents(doc! { "_id": server })
                    .await
                    .expect("Failed to look for a server")
                    > 0;
                known.insert(server.to_string(), exists);
                exists
            }
        };
        if !exists {
            orphans.push(id.to_string());
        }
    }
    let now = to_bson(&Timestamp::now_utc()).expect("a time");
    let mut called_off = 0;
    for id in orphans {
        called_off += events
            .update_one(
                doc! { "_id": &id, "status": { "$in": ["Scheduled", "Active", "Draft"] } },
                doc! { "$set": { "status": "Cancelled", "ended_at": now.clone() } },
            )
            .await
            .expect("Failed to call off an event of a deleted server")
            .modified_count;
    }
    called_off
}

/// What every event's four counts should be, from its rows (and its seat
/// list, when it has one): going, maybe, not going, waiting
pub async fn server_event_counts_snapshot(
    db: &crate::mongodb::Database,
) -> std::collections::BTreeMap<String, [i64; 4]> {
    let rows = db.collection::<Document>("server_event_interests");
    let mut counts: std::collections::BTreeMap<String, [i64; 4]> = Default::default();
    let mut cursor = rows
        .aggregate(vec![doc! { "$group": {
            "_id": { "event": "$_id.event", "answer": "$answer", "waiting": "$waiting" },
            "n": { "$sum": 1_i32 }
        } }])
        .await
        .expect("Failed to count event answers");
    while let Some(group) = cursor.next().await {
        let group = group.expect("Failed to read an event answer count");
        let Ok(id) = group.get_document("_id") else { continue };
        let Ok(event) = id.get_str("event") else { continue };
        let n = group
            .get_i64("n")
            .or_else(|_| group.get_i32("n").map(i64::from))
            .unwrap_or(0);
        let slot = match (id.get_str("answer"), id.get_bool("waiting").unwrap_or(false)) {
            (Ok("Going"), false) => 0,
            (Ok("Going"), true) => 3,
            (Ok("NotGoing"), _) => 2,
            _ => 1,
        };
        counts.entry(event.to_string()).or_default()[slot] += n;
    }
    // an event that has a seat list: it decides the seats
    let mut cursor = db
        .collection::<Document>("server_event_rosters")
        .find(doc! {})
        .await
        .expect("Failed to read the seat lists");
    while let Some(roster) = cursor.next().await {
        let roster = roster.expect("Failed to read a seat list");
        let Ok(id) = roster.get_str("_id") else { continue };
        let size = |field: &str| roster.get_array(field).map(|a| a.len() as i64).unwrap_or(0);
        let entry = counts.entry(id.to_string()).or_default();
        entry[0] = size("going");
        entry[3] = size("waiting");
    }
    counts
}

/// Write the counts onto every event (an event without rows gets zeros).
/// Returns (events looked at, events whose counts changed).
async fn write_server_event_counts(
    db: &crate::mongodb::Database,
    counts: &std::collections::BTreeMap<String, [i64; 4]>,
) -> (u64, u64) {
    let events = db.collection::<Document>("server_events");
    let mut ids = Vec::new();
    let mut cursor = events
        .find(doc! {})
        .projection(doc! { "_id": 1_i32 })
        .await
        .expect("Failed to read the events");
    while let Some(event) = cursor.next().await {
        if let Ok(id) = event.expect("Failed to read an event").get_str("_id") {
            ids.push(id.to_string());
        }
    }
    let (mut kept, mut changed) = (0, 0);
    for id in ids {
        let [going, maybe, not_going, waiting] = counts.get(&id).copied().unwrap_or_default();
        kept += 1;
        changed += events
            .update_one(
                doc! { "_id": &id },
                doc! { "$set": {
                    "interested_count": maybe,
                    "going_count": going,
                    "not_going_count": not_going,
                    "waitlist_count": waiting,
                } },
            )
            .await
            .expect("Failed to set the counts of an event")
            .modified_count;
    }
    (kept, changed)
}


/// Linked accounts: found by person, and one service account links to one
/// Zrzur account only. Safe to run twice.
pub async fn create_linked_account_indexes(db: &mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "linked_accounts",
        "indexes": [
            { "key": { "user_id": 1_i32 }, "name": "user_id" },
            {
                "key": { "service": 1_i32, "external_id": 1_i32 },
                "name": "service_external_id",
                "unique": true
            }
        ]
    })
    .await
    .expect("Failed to create the linked account indexes");
}

/// Sessions keep only a hash of the token (`Session::token_hash`). The old
/// sessions hold the token itself, so they are removed and their owners sign
/// in again. Safe to run twice: nothing is left to remove the second time.
pub async fn end_sessions_without_hash(db: &mongodb::Database) {
    db.collection::<Document>("sessions")
        .delete_many(doc! { "token_hash": { "$exists": false } })
        .await
        .expect("Failed to end sessions without a token hash");
}

/// Sessions keep only `token_hash`, so the unique index on `token` saw every
/// new session as `token: null` and refused all but the first. Drops that
/// index and makes `token_hash` unique. Safe to run twice: dropping an index
/// that is gone is ignored, and creating one that exists is a no-op.
pub async fn index_sessions_by_token_hash(db: &mongodb::Database) {
    // an error here only means the old index is already gone
    let _ = db
        .run_command(doc! { "dropIndexes": "sessions", "index": "token" })
        .await;
    db.run_command(doc! {
        "createIndexes": "sessions",
        "indexes": [{ "key": { "token_hash": 1_i32 }, "name": "token_hash", "unique": true }]
    })
    .await
    .expect("Failed to index sessions by token hash");
}

/// Indexes of "Download my data" and of the lookups it makes. Safe to run
/// twice (creating an index that exists is a no-op).
pub async fn create_data_export_indexes(db: &mongodb::Database) {
    for (collection, index) in [
        (
            "data_exports",
            doc! { "key": { "status": 1_i32, "expires_at": 1_i32 }, "name": "status_expires" },
        ),
        (
            "data_export_chunks",
            doc! { "key": { "user_id": 1_i32, "n": 1_i32 }, "name": "user_n" },
        ),
        (
            "attachments",
            doc! { "key": { "uploader_id": 1_i32 }, "name": "uploader", "sparse": true },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// What an old action followed, from its case. Unknown stays unset: the
/// notice and the app then show no such line rather than guess.
fn action_basis(case: &Document) -> Option<&'static str> {
    if case.get_str("subcategory") == Ok("staff") {
        Some("OwnInitiative")
    } else if case.contains_key("public_reporter") {
        Some("Notice")
    } else if case.contains_key("reporter_id") {
        Some("Report")
    } else {
        None
    }
}

/// Give moderation actions taken before statements of reasons existed the
/// parts DSA Art. 17 asks for: nothing automated, the rule of their category
/// (shown by its translated name), the reason shown to the person as the
/// facts, worldwide, and what they followed where the case says. Only fills
/// what is missing, so running it twice changes nothing.
pub async fn fill_action_statements(db: &crate::mongodb::Database) {
    let actions = db.collection::<Document>("safety_actions");
    for (field, value) in [
        ("automated_detection", Bson::Boolean(false)),
        ("automated_decision", Bson::Boolean(false)),
        ("territorial_scope", Bson::String("worldwide".to_string())),
    ] {
        actions
            .update_many(doc! { field: { "$exists": false } }, doc! { "$set": { field: value } })
            .await
            .expect("Failed to fill action statements");
    }
    // facts start as the reason the person was already shown
    actions
        .update_many(
            doc! { "facts": { "$exists": false } },
            vec![doc! { "$set": { "facts": "$public_reason" } }],
        )
        .await
        .expect("Failed to fill action facts");
    actions
        .update_many(
            doc! { "ground": { "$exists": false }, "category": { "$type": "string" } },
            vec![doc! { "$set": { "ground": { "type": "Terms", "rule": { "$concat": ["rule:", "$category"] } } } }],
        )
        .await
        .expect("Failed to fill action grounds");
    actions
        .update_many(
            doc! { "ground": { "$exists": false } },
            doc! { "$set": { "ground": { "type": "Terms", "rule": "Terms of Service" } } },
        )
        .await
        .expect("Failed to fill action grounds");

    let cases = db.collection::<Document>("safety_cases");
    let without_basis: Vec<Document> = actions
        .find(doc! { "basis": { "$exists": false }, "case_id": { "$type": "string" } })
        .projection(doc! { "case_id": 1 })
        .await
        .expect("Failed to read actions")
        .filter_map(|document| async move { document.ok() })
        .collect()
        .await;
    for action in without_basis {
        let (Ok(id), Ok(case_id)) = (action.get_str("_id"), action.get_str("case_id")) else {
            continue;
        };
        let case = cases.find_one(doc! { "_id": case_id }).await.expect("Failed to read case");
        if let Some(basis) = case.as_ref().and_then(action_basis) {
            actions
                .update_one(doc! { "_id": id }, doc! { "$set": { "basis": basis } })
                .await
                .expect("Failed to fill action basis");
        }
    }
}

/// Finding official accounts without reading every user. `createIndexes` skips
/// an index that already exists, so this is safe to run twice.
pub async fn create_official_indexes(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "users",
        "indexes": [{ "key": { "official": 1_i32 }, "name": "official", "sparse": true }]
    })
    .await
    .expect("Failed to create the official index.");
}

/// Command permission rules are read per server and app (every command
/// used, every command list). Creating an index that exists does nothing, so
/// this is safe to run twice.
pub async fn create_command_permission_indexes(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "command_permissions",
        "indexes": [{
            "key": { "server_id": 1_i32, "application_id": 1_i32 },
            "name": "server_application",
        }]
    })
    .await
    .expect("Failed to create the command permissions index.");
}

/// Scheduled server events: a server's upcoming list, crond's due and
/// active queries, and the interested marks by event (paged by user) and by
/// user. Only indexes, no data moves; creating an index that exists does
/// nothing, so this is safe to run twice.
pub async fn create_server_event_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        (
            "server_events",
            doc! { "key": { "server": 1_i32, "status": 1_i32, "starts_at": 1_i32 }, "name": "server_status_start" },
        ),
        (
            "server_events",
            doc! { "key": { "status": 1_i32, "starts_at": 1_i32 }, "name": "status_start" },
        ),
        (
            "server_event_interests",
            doc! { "key": { "_id.event": 1_i32, "_id.user": 1_i32 }, "name": "event_user" },
        ),
        (
            "server_event_interests",
            doc! { "key": { "_id.user": 1_i32 }, "name": "user" },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// Server insights: a server's days for its page, the days crond deletes,
/// and the few open days crond closes (a partial index: closed rows are not
/// in it). Only indexes (the collection is made by its first write);
/// creating an index that exists does nothing, so this is safe to run twice.
pub async fn create_server_insight_indexes(db: &crate::mongodb::Database) {
    for index in [
        doc! { "key": { "server": 1_i32, "day": 1_i32 }, "name": "server_day" },
        doc! { "key": { "day": 1_i32 }, "name": "day" },
        doc! {
            "key": { "open": 1_i32, "day": 1_i32 },
            "name": "open_day",
            "partialFilterExpression": { "open": true },
        },
    ] {
        db.run_command(doc! { "createIndexes": "server_insights", "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on server_insights: {error}"));
    }
}

/// Indexes of the bot verification queue, account change requests, notices
/// and support tickets (revisions 62-65)
pub async fn create_staff_feature_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        ("bot_verifications", doc! { "key": { "bot_id": 1_i32, "_id": -1_i32 }, "name": "bot_newest" }),
        ("bot_verifications", doc! { "key": { "status": 1_i32, "_id": 1_i32 }, "name": "status_oldest" }),
        ("account_change_requests", doc! { "key": { "user_id": 1_i32, "_id": -1_i32 }, "name": "user_newest" }),
        ("account_change_requests", doc! { "key": { "token_hash": 1_i32 }, "name": "token_hash", "unique": true, "sparse": true }),
        ("platform_notices", doc! { "key": { "user_id": 1_i32, "_id": -1_i32 }, "name": "user_newest" }),
        ("notice_dismissals", doc! { "key": { "user_id": 1_i32 }, "name": "user" }),
        ("support_tickets", doc! { "key": { "status": 1_i32, "updated_at": -1_i32 }, "name": "status_recent" }),
        ("support_tickets", doc! { "key": { "user_id": 1_i32, "updated_at": -1_i32 }, "name": "user_recent" }),
        ("support_tickets", doc! { "key": { "assigned_to": 1_i32, "updated_at": -1_i32 }, "name": "assigned_recent" }),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .unwrap_or_else(|error| panic!("Failed to create an index on {collection}: {error}"));
    }
}

/// Users by the time their custom status runs out (only those with one),
/// for crond's minute sweep; and users by friend, for the mutual friends
/// on a profile, which read the whole collection without it. Safe to run
/// twice (creating an index that exists is a no-op).
pub async fn create_status_mutual_indexes(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "users",
        "indexes": [
            {
                "key": { "status.expires_at": 1_i32 },
                "name": "status_expires_at",
                "partialFilterExpression": { "status.expires_at": { "$exists": true } },
            },
            {
                "key": { "relations._id": 1_i32, "relations.status": 1_i32 },
                "name": "relations_id_status",
            },
        ]
    })
    .await
    .expect("Failed to create the status and mutual friends indexes");
}

/// Polls by the time they end, for crond to close them and post their
/// result. Only polls whose reply is not posted yet are in it (open ones,
/// and closed ones crond still has to finish). Safe to run twice.
pub async fn create_poll_index(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "messages",
        "indexes": [{
            "key": { "poll.expires_at": 1_i32 },
            "name": "polls_to_close",
            "partialFilterExpression": { "poll.replied": false },
        }]
    })
    .await
    .expect("Failed to create the poll index");
}

/// What the public lists and crond read most. Without `_id` in the popular
/// index every page of popular designs sorts all of them in memory; without
/// the others Discover and file deletion read the whole collection. Safe to
/// run twice (creating an index that exists is a no-op).
pub async fn create_list_indexes(db: &crate::mongodb::Database) {
    for (collection, index) in [
        (
            "community_items",
            doc! { "key": { "kind": 1_i32, "status": 1_i32, "popularity": -1_i32, "_id": -1_i32 }, "name": "kind_popular_id" },
        ),
        (
            "servers",
            doc! {
                "key": { "discoverable": 1_i32, "_id": -1_i32 },
                "name": "discoverable",
                "partialFilterExpression": { "discoverable": true },
            },
        ),
        (
            "bots",
            doc! {
                "key": { "discoverable": 1_i32, "public": 1_i32, "_id": -1_i32 },
                "name": "discoverable",
                "partialFilterExpression": { "discoverable": true },
            },
        ),
        (
            "attachments",
            doc! {
                "key": { "deleted": 1_i32 },
                "name": "deleted",
                "partialFilterExpression": { "deleted": true },
            },
        ),
    ] {
        db.run_command(doc! { "createIndexes": collection, "indexes": [index] })
            .await
            .expect("Failed to create a list index");
    }
}

/// Threads are listed by their parent channel
pub async fn create_thread_indexes(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "channels",
        "indexes": [ { "key": { "parent": 1_i32, "_id": -1_i32 }, "name": "parent_newest", "sparse": true } ]
    })
    .await
    .expect("Failed to create the thread index");
}

/// Bots are found by the hash of their token
pub async fn create_bot_token_index(db: &crate::mongodb::Database) {
    // an index on token_hash may already exist under another name
    // (made by hand on the test database); creating a second one fails
    let existing = db
        .run_command(doc! { "listIndexes": "bots" })
        .await
        .ok()
        .and_then(|reply| reply.get_document("cursor").ok().cloned())
        .and_then(|cursor| cursor.get_array("firstBatch").ok().cloned())
        .unwrap_or_default();
    let has_index = existing.iter().any(|index| {
        index
            .as_document()
            .and_then(|index| index.get_document("key").ok())
            .is_some_and(|key| key.len() == 1 && key.contains_key("token_hash"))
    });
    if has_index {
        return;
    }
    db.run_command(doc! {
        "createIndexes": "bots",
        "indexes": [
            { "key": { "token_hash": 1_i32 }, "name": "token_hash", "unique": true }
        ]
    })
    .await
    .expect("Failed to create the bot token index");
}

#[cfg(test)]
mod thread_permission_migration_tests {
    use super::{
        migrate_server, read_or_stop, thread_permissions_on_channel, thread_permissions_on_server,
        THREADS_DONE,
    };
    use crate::mongodb::bson::{doc, Document};
    use revolt_permissions::{ChannelPermission, THREAD_PERMISSIONS};

    const THREADS: i64 = (1 << 43) | (1 << 44) | (1 << 45);
    const SEND: i64 = ChannelPermission::SendMessage as i64;
    const MANAGE_CHANNEL: i64 = ChannelPermission::ManageChannel as i64;

    #[test]
    fn the_bits_are_the_ones_clients_know() {
        assert_eq!(*THREAD_PERMISSIONS as i64, THREADS);
    }

    #[test]
    #[should_panic(expected = "Failed to read servers")]
    fn a_cursor_error_stops_the_migration() {
        let item: Result<Document, &str> = Err("connection lost");
        read_or_stop(item, "servers");
    }

    #[test]
    fn only_the_numbers_that_change_are_written() {
        let server = doc! {
            "_id": "s",
            "roles": {
                "moderator": { "name": "m", "permissions": { "a": MANAGE_CHANNEL, "d": 0_i64 } },
                "talker": { "name": "t", "permissions": { "a": SEND, "d": 0_i64 } },
                "wide-deny": { "name": "w", "permissions": { "a": 0_i64, "d": THREADS | SEND } },
            },
            "categories": [{
                "id": "c", "title": "c", "channels": [],
                "default_permissions": { "a": 0_i64, "d": THREADS },
                "role_permissions": {
                    "talker": { "a": 0_i64, "d": THREADS | MANAGE_CHANNEL },
                    "lead": { "a": MANAGE_CHANNEL, "d": 0_i64 },
                },
            }],
        };
        let update = thread_permissions_on_server(&server);
        let bit = update.get_document("$bit").unwrap();
        assert_eq!(bit.get_document("default_permissions").unwrap().get_i64("or").unwrap(), THREADS);
        assert_eq!(bit.get_document("roles.moderator.permissions.a").unwrap().get_i64("or").unwrap(), THREADS);
        assert_eq!(bit.get_document("roles.wide-deny.permissions.d").unwrap().get_i64("and").unwrap(), !THREADS);
        // a role that only talks is untouched: it takes them from the default
        assert!(!bit.keys().any(|path| path.contains("talker")));

        let set = update.get_document("$set").unwrap();
        assert!(set.get_bool(THREADS_DONE).unwrap());
        assert!(!set.contains_key("roles"));
        let categories = set.get_array("categories").unwrap();
        let category = categories[0].as_document().unwrap();
        assert_eq!(category.get_document("default_permissions").unwrap().get_i64("d").unwrap(), 0);
        let overrides = category.get_document("role_permissions").unwrap();
        assert_eq!(overrides.get_document("talker").unwrap().get_i64("d").unwrap(), MANAGE_CHANNEL);
        assert_eq!(overrides.get_document("lead").unwrap().get_i64("a").unwrap(), MANAGE_CHANNEL | THREADS);

        // nothing to clean in the categories: no guarded write
        let plain = thread_permissions_on_server(&doc! { "_id": "s", "categories": [] });
        assert!(!plain.get_document("$set").unwrap().contains_key("categories"));
    }

    #[test]
    fn channel_overrides_keep_everything_but_the_new_denies() {
        let channel = doc! {
            "_id": "c",
            "default_permissions": { "a": 0_i64, "d": SEND | THREADS },
            "role_permissions": {
                "r": { "a": SEND, "d": 1_i64 << 44 },
                "lead": { "a": MANAGE_CHANNEL, "d": 0_i64 },
            },
            "member_permissions": { "u": { "a": 0_i64, "d": 1_i64 << 43 } },
        };
        let update = thread_permissions_on_channel(&channel);
        let bit = update.get_document("$bit").unwrap();
        for path in ["default_permissions.d", "role_permissions.r.d", "member_permissions.u.d"] {
            assert_eq!(bit.get_document(path).unwrap().get_i64("and").unwrap(), !THREADS, "{path}");
        }
        // Manage Channel by an override is a moderator too
        assert_eq!(bit.get_document("role_permissions.lead.a").unwrap().get_i64("or").unwrap(), THREADS);
        assert_eq!(bit.len(), 4);

        // nothing to clean: only the mark, as an empty $bit would be an error
        let clean = doc! { "_id": "c", "default_permissions": { "a": 0_i64, "d": SEND } };
        let update = thread_permissions_on_channel(&clean);
        assert!(!update.contains_key("$bit"));
        assert!(update.get_document("$set").unwrap().get_bool(THREADS_DONE).unwrap());
    }

    /// MongoDB only: a saved server, channel and webhooks, run twice, then
    /// what owners do after it
    #[tokio::test]
    async fn existing_servers_and_webhooks_keep_what_they_could_do() {
        let db = crate::DatabaseInfo::Test("thread_permission_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        db.drop_database().await;
        let servers = mongo.col::<Document>("servers");
        servers
            .insert_many([
                doc! { "_id": "00wide", "default_permissions": 1_i32 << 22 },
                // a server that talks through a role: the bits are in the
                // default, Send Messages stays what the role gives
                doc! { "_id": "00gated", "default_permissions": 1_i64 << 20, "roles": {
                    "mod": { "name": "m", "permissions": { "a": MANAGE_CHANNEL, "d": THREADS } },
                } },
            ])
            .await
            .unwrap();
        let channels = mongo.col::<Document>("channels");
        channels
            .insert_one(doc! {
                "_id": "00c", "channel_type": "TextChannel", "server": "00wide", "name": "c",
                "default_permissions": { "a": 0_i64, "d": THREADS },
            })
            .await
            .unwrap();
        let hooks = mongo.col::<Document>("channel_webhooks");
        hooks
            .insert_many([
                doc! { "_id": "00sends", "permissions": SEND },
                doc! { "_id": "00muted", "permissions": ChannelPermission::React as i64 },
            ])
            .await
            .unwrap();

        for _ in 0..2 {
            super::grant_thread_permissions(&mongo.db()).await;
        }

        let server = |id: &'static str| {
            let servers = servers.clone();
            async move { servers.find_one(doc! { "_id": id }).await.unwrap().unwrap() }
        };
        assert_eq!(server("00wide").await.get_i64("default_permissions").unwrap(), (1 << 22) | THREADS);
        let gated = server("00gated").await;
        assert_eq!(gated.get_i64("default_permissions").unwrap(), (1 << 20) | THREADS);
        let role = gated.get_document("roles").unwrap().get_document("mod").unwrap();
        let role = role.get_document("permissions").unwrap();
        assert_eq!((role.get_i64("a").unwrap(), role.get_i64("d").unwrap()), (MANAGE_CHANNEL | THREADS, 0));
        let channel = channels.find_one(doc! { "_id": "00c" }).await.unwrap().unwrap();
        assert_eq!(channel.get_document("default_permissions").unwrap().get_i64("d").unwrap(), 0);
        let hook = hooks.find_one(doc! { "_id": "00sends" }).await.unwrap().unwrap();
        assert_eq!(hook.get_i64("permissions").unwrap(), SEND | (1 << 44) | (1 << 45));
        let hook = hooks.find_one(doc! { "_id": "00muted" }).await.unwrap().unwrap();
        assert_eq!(hook.get_i64("permissions").unwrap(), ChannelPermission::React as i64);

        // an owner closes things; a late run, or a second delta, leaves that
        servers
            .update_one(doc! { "_id": "00wide" }, doc! { "$set": { "default_permissions": SEND } })
            .await
            .unwrap();
        channels
            .update_one(doc! { "_id": "00c" }, doc! { "$set": { "default_permissions.d": THREADS } })
            .await
            .unwrap();
        hooks
            .update_one(doc! { "_id": "00sends" }, doc! { "$set": { "permissions": SEND } })
            .await
            .unwrap();
        // made after the claim, with the new defaults, and closed by its owner
        servers
            .insert_one(doc! { "_id": "zz-new", "default_permissions": SEND })
            .await
            .unwrap();

        super::grant_thread_permissions(&mongo.db()).await;

        assert_eq!(server("00wide").await.get_i64("default_permissions").unwrap(), SEND);
        assert_eq!(server("zz-new").await.get_i64("default_permissions").unwrap(), SEND);
        let channel = channels.find_one(doc! { "_id": "00c" }).await.unwrap().unwrap();
        assert_eq!(channel.get_document("default_permissions").unwrap().get_i64("d").unwrap(), THREADS);
        let hook = hooks.find_one(doc! { "_id": "00sends" }).await.unwrap().unwrap();
        assert_eq!(hook.get_i64("permissions").unwrap(), SEND);
        db.drop_database().await;
    }

    /// MongoDB only: only the numbers change on a role, and categories edited
    /// after they were read are read again, not written over
    #[tokio::test]
    async fn an_edit_during_the_migration_survives() {
        let db = crate::DatabaseInfo::Test("thread_permission_concurrent".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        db.drop_database().await;
        let servers = mongo.col::<Document>("servers");
        let category = |title: &str| {
            doc! {
                "id": "c", "title": title, "channels": [],
                "default_permissions": { "a": 0_i64, "d": THREADS },
            }
        };
        let read = doc! {
            "_id": "00s",
            "default_permissions": SEND,
            "roles": { "mod": { "name": "m", "permissions": { "a": MANAGE_CHANNEL, "d": 0_i64 } } },
            "categories": [category("old")],
        };
        servers.insert_one(read.clone()).await.unwrap();

        // after `read` was taken: the role's name, and the category's title
        servers
            .update_one(
                doc! { "_id": "00s" },
                doc! { "$set": {
                    "roles.mod.name": "renamed",
                    "roles.mod.permissions.a": MANAGE_CHANNEL | SEND,
                    "categories": [category("edited")],
                } },
            )
            .await
            .unwrap();

        migrate_server(&servers, read).await;

        let saved = servers.find_one(doc! { "_id": "00s" }).await.unwrap().unwrap();
        let role = saved.get_document("roles").unwrap().get_document("mod").unwrap();
        assert_eq!(role.get_str("name").unwrap(), "renamed");
        assert_eq!(
            role.get_document("permissions").unwrap().get_i64("a").unwrap(),
            MANAGE_CHANNEL | SEND | THREADS
        );
        let categories = saved.get_array("categories").unwrap();
        let category = categories[0].as_document().unwrap();
        assert_eq!(category.get_str("title").unwrap(), "edited");
        assert_eq!(category.get_document("default_permissions").unwrap().get_i64("d").unwrap(), 0);
        assert!(saved.get_bool(THREADS_DONE).unwrap());

        db.drop_database().await;
    }
}

#[cfg(test)]
mod community_migration_tests {
    use super::community_item_review_fields;
    use crate::mongodb::bson::doc;

    #[test]
    fn keeps_existing_items_public_and_attributed() {
        let item = doc! {
            "_id": "01JZZZZZZZZZZZZZZZZZZZZZZZ",
            "kind": "Decoration",
            "name": "Old frame",
            "creator_id": "uploader",
            "file": { "_id": "file1", "tag": "decorations" },
            "status": "Active",
        };
        let set = community_item_review_fields(&item, 1_700_000_000_000, 3).unwrap();
        assert_eq!(set.get_str("status").unwrap(), "Published");
        assert_eq!(set.get_i64("popularity").unwrap(), 3);
        assert_eq!(set.get_i64("published_at").unwrap(), 1_700_000_000_000);
        // creator, file and name are left alone
        assert!(set.get("creator_id").is_none());
        assert!(set.get("file").is_none());
        assert!(set.get("name").is_none());
    }

    #[test]
    fn maps_hidden_and_removed_and_skips_migrated() {
        let status = |s: &str| {
            community_item_review_fields(&doc! { "_id": "x", "status": s }, 0, 0)
                .map(|set| set.get_str("status").unwrap().to_string())
        };
        assert_eq!(status("Hidden").as_deref(), Some("Suspended"));
        assert_eq!(status("Removed").as_deref(), Some("Archived"));
        assert_eq!(status("Published"), None);
        assert_eq!(status("PendingReview"), None);
    }
}

#[cfg(test)]
mod statement_migration_tests {
    use crate::mongodb::bson::{doc, Document};

    /// MongoDB only: the team volunteer indexes can be made twice
    #[tokio::test]
    async fn team_volunteer_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("team_volunteer_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_team_volunteer_indexes(&mongo.db()).await;
        }
        for (collection, wanted) in [
            ("team_members", 2),
            ("team_activity_days", 3),
            ("team_identities", 2),
        ] {
            let names = mongo
                .db()
                .collection::<Document>(collection)
                .list_index_names()
                .await
                .unwrap();
            assert_eq!(names.len(), wanted, "{collection}: {names:?}");
        }
    }

    /// MongoDB only: every bot whose account has `official_bot` gets the
    /// Official flag, other flags stay, other bots are left alone, twice
    #[tokio::test]
    async fn official_bot_flags_fill_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test(format!("official_bot_flags_{}", ulid::Ulid::new()))
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        let users = mongo.db().collection::<Document>("users");
        let bots = mongo.db().collection::<Document>("bots");
        users
            .insert_many([
                doc! { "_id": "A", "username": "a", "official_bot": true, "bot": { "owner": "O" } },
                doc! { "_id": "B", "username": "b", "official_bot": true, "bot": { "owner": "O" } },
                doc! { "_id": "C", "username": "c", "bot": { "owner": "O" } },
                doc! { "_id": "P", "username": "p", "official_bot": true },
            ])
            .await
            .unwrap();
        bots.insert_many([
            doc! { "_id": "A", "owner": "O" },
            doc! { "_id": "B", "owner": "O", "flags": 1_i32 },
            doc! { "_id": "C", "owner": "O", "flags": 1_i32 },
        ])
        .await
        .unwrap();
        for _ in 0..2 {
            super::fill_official_bot_flags(&mongo.db()).await;
        }
        let flags = |bot: Document| bot.get_i32("flags").ok();
        let find = |id: &'static str| bots.find_one(doc! { "_id": id });
        assert_eq!(flags(find("A").await.unwrap().unwrap()), Some(2));
        assert_eq!(flags(find("B").await.unwrap().unwrap()), Some(3));
        assert_eq!(flags(find("C").await.unwrap().unwrap()), Some(1));
        assert!(find("P").await.unwrap().is_none());
    }

    /// MongoDB only: the badge gate indexes can be made twice
    #[tokio::test]
    async fn badge_gate_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("badge_gate_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_badge_gate_indexes(&mongo.db()).await;
        }
        for (collection, name) in [("badge_grants", "badge_held"), ("badge_change_requests", "open_change")] {
            let names = mongo
                .db()
                .collection::<Document>(collection)
                .list_index_names()
                .await
                .unwrap();
            assert!(names.contains(&name.to_string()), "{collection}: {names:?}");
        }
    }

    /// MongoDB only: the activity block indexes can be made twice
    #[tokio::test]
    async fn activity_block_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("activity_block_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_activity_block_indexes(&mongo.db()).await;
        }
        let names = mongo
            .db()
            .collection::<Document>("activity_blocks")
            .list_index_names()
            .await
            .unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
    }

    /// MongoDB only: the saved message and reminder indexes can be made twice
    #[tokio::test]
    async fn saved_reminder_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("saved_reminder_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_saved_reminder_indexes(&mongo.db()).await;
        }
        for (collection, wanted) in [("saved_messages", 2), ("message_reminders", 3)] {
            let names = mongo
                .db()
                .collection::<Document>(collection)
                .list_index_names()
                .await
                .unwrap();
            assert_eq!(names.len(), wanted, "{collection}: {names:?}");
        }
        db.drop_database().await;
    }

    /// MongoDB only: the poll index can be made twice
    #[tokio::test]
    async fn poll_index_runs_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("poll_index_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_poll_index(&mongo.db()).await;
        }
        db.drop_database().await;
    }

    /// MongoDB only: the status and mutual friends indexes can be made twice
    #[tokio::test]
    async fn status_mutual_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("status_mutual_index_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_status_mutual_indexes(&mongo.db()).await;
        }
        db.drop_database().await;
    }

    /// MongoDB only: running it twice is fine
    #[tokio::test]
    async fn list_indexes_run_twice_on_mongodb() {
        let db = crate::DatabaseInfo::Test("list_indexes_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        for _ in 0..2 {
            super::create_list_indexes(&mongo.db()).await;
        }
        db.drop_database().await;
    }

    #[test]
    fn the_grace_is_at_least_thirty_days() {
        use iso8601_timestamp::{Duration, Timestamp};

        let now = Timestamp::now_utc();
        for (idle_days, days) in [(0, 30), (7, 30), (30, 30), (45, 45)] {
            assert_eq!(
                super::session_grace_until(now, idle_days),
                now + Duration::days(days),
                "{idle_days}"
            );
        }
    }

    /// MongoDB only: every session gets the grace date, its `last_seen` stays
    /// as it was, a second run changes nothing, and the sweep and the auth
    /// path both honour the date.
    #[tokio::test]
    async fn old_sessions_get_a_grace_and_keep_their_last_seen_on_mongodb() {
        use crate::Session;
        use iso8601_timestamp::{Duration, Timestamp};

        let db = crate::DatabaseInfo::Test("session_grace_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        db.drop_database().await;

        let ms = |time: Timestamp| time.duration_since(Timestamp::UNIX_EPOCH).whole_milliseconds();
        let now = Timestamp::now_utc();
        let long_ago = now + Duration::days(-400);
        let old = Session {
            id: "old".to_string(),
            user_id: "u".to_string(),
            token: "old-token".to_string(),
            // as every session since C3: found by the hash of its token
            token_hash: crate::models::hash_secret("old-token"),
            name: "old".to_string(),
            last_seen: long_ago,
            idle_grace_until: None,
            origin: None,
            subscription: None,
        };
        db.save_session(&old).await.unwrap();

        let until = now + Duration::days(30);
        super::give_sessions_expiry_grace(&mongo.db(), until).await;

        // written by an old delta during the rollout: no date yet
        let late_write = Session {
            id: "late".to_string(),
            token: "late-token".to_string(),
            token_hash: crate::models::hash_secret("late-token"),
            ..old.clone()
        };
        db.save_session(&late_write).await.unwrap();

        // the second fill: a date for that one, the first date moves nothing,
        // and the index already exists
        super::give_sessions_expiry_grace(&mongo.db(), until + Duration::days(9)).await;
        let late = db.fetch_session("late").await.unwrap();
        assert_eq!(
            late.idle_grace_until.map(ms),
            Some(ms(until + Duration::days(9)))
        );
        db.delete_session("late").await.unwrap();

        let stored = db.fetch_session("old").await.unwrap();
        assert_eq!(ms(stored.last_seen), ms(long_ago));
        assert_eq!(stored.idle_grace_until.map(ms), Some(ms(until)));

        // not swept while the date is ahead
        assert_eq!(Session::delete_idle(&db).await.unwrap(), 0);

        // the sweep's query finds it the day after
        let late = until + Duration::days(1);
        let found = db
            .fetch_idle_sessions(late + Duration::days(-30), late, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);

        // and until then it works, and using it counts
        let used = Session::from_token(&db, "old-token").await.unwrap();
        assert!(used.last_seen > long_ago);
        db.drop_database().await;
    }

    #[tokio::test]
    async fn webhook_tokens_are_hashed_once_and_keep_working() {
        let db = crate::DatabaseInfo::Test("webhook_token_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        db.drop_database().await;
        let hooks = mongo.col::<Document>("channel_webhooks");
        hooks
            .insert_many([
                doc! { "_id": "old", "name": "a", "creator_id": "u", "channel_id": "c", "permissions": 0_i64, "token": "plain" },
                doc! { "_id": "both", "name": "b", "creator_id": "u", "channel_id": "c", "permissions": 0_i64, "token": "plain", "token_hash": "kept" },
                doc! { "_id": "new", "name": "c", "creator_id": "u", "channel_id": "c", "permissions": 0_i64, "token_hash": "kept" },
                doc! { "_id": "none", "name": "d", "creator_id": "u", "channel_id": "c", "permissions": 0_i64, "token": bson::Bson::Null },
            ])
            .await
            .unwrap();

        // twice: safe to run again
        for _ in 0..2 {
            super::hash_webhook_tokens(&mongo.db()).await;
        }

        let old = db.fetch_webhook("old").await.unwrap();
        assert_eq!(old.token, None);
        assert_eq!(old.token_hash, Some(crate::hash_token("plain")));
        assert!(old.assert_token("plain").is_ok());
        let stored = hooks.find_one(doc! { "_id": "old" }).await.unwrap().unwrap();
        assert!(!stored.contains_key("token"));
        assert_eq!(db.fetch_webhook("both").await.unwrap().token_hash.as_deref(), Some("kept"));
        assert_eq!(db.fetch_webhook("new").await.unwrap().token_hash.as_deref(), Some("kept"));
        let none = hooks.find_one(doc! { "_id": "none" }).await.unwrap().unwrap();
        assert!(!none.contains_key("token"));
        assert!(db.fetch_webhook("none").await.unwrap().assert_token("").is_err());
        db.drop_database().await;
    }

    /// MongoDB only: actions saved before statements of reasons existed get
    /// the defaults, the reason becomes the facts, and a second run changes nothing.
    #[tokio::test]
    async fn old_actions_get_statement_defaults_on_mongodb() {
        let db = crate::DatabaseInfo::Test("stmt_migration".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let crate::Database::MongoDb(mongo) = &db else {
            return;
        };
        db.drop_database().await;
        let actions = mongo.col::<Document>("safety_actions");
        actions
            .insert_one(doc! {
                "_id": "old", "actor_id": "r", "kind": "Warning",
                "target": { "kind": "Message", "id": "m" },
                "category": "Spam", "public_reason": "Posted ads", "created_at": 1_i64,
                "appealable": true,
            })
            .await
            .unwrap();
        actions
            .insert_one(doc! {
                "_id": "new", "public_reason": "x", "facts": "Kept as written",
                "ground": { "type": "Law", "law": "StGB", "provision": "130" },
                "automated_detection": true, "automated_decision": false,
                "territorial_scope": "Germany",
            })
            .await
            .unwrap();
        let cases = mongo.col::<Document>("safety_cases");
        cases
            .insert_many([
                doc! { "_id": "staff-case", "reporter_id": "r", "subcategory": "staff" },
                doc! { "_id": "public-case", "reporter_id": "public", "public_reporter": { "location": "x" } },
                doc! { "_id": "user-case", "reporter_id": "u" },
            ])
            .await
            .unwrap();
        for (id, case) in [("on-staff", "staff-case"), ("on-public", "public-case"), ("on-user", "user-case")] {
            actions
                .insert_one(doc! { "_id": id, "case_id": case, "public_reason": "x", "category": "Harassment" })
                .await
                .unwrap();
        }
        actions
            .insert_one(doc! { "_id": "no-category", "case_id": "gone", "public_reason": "x" })
            .await
            .unwrap();
        for _ in 0..2 {
            super::fill_action_statements(&mongo.db()).await;
        }
        let action = |id: &'static str| {
            let actions = actions.clone();
            async move { actions.find_one(doc! { "_id": id }).await.unwrap().unwrap() }
        };
        assert_eq!(action("on-staff").await.get_str("basis").unwrap(), "OwnInitiative");
        assert_eq!(action("on-public").await.get_str("basis").unwrap(), "Notice");
        assert_eq!(action("on-user").await.get_str("basis").unwrap(), "Report");
        // no case to ask: nothing is claimed
        assert!(!action("old").await.contains_key("basis"));
        assert!(!action("no-category").await.contains_key("basis"));
        // the ground names the rule of the category, shown by its translated name
        assert_eq!(action("old").await.get_document("ground").unwrap().get_str("rule").unwrap(), "rule:Spam");
        assert_eq!(action("no-category").await.get_document("ground").unwrap().get_str("rule").unwrap(), "Terms of Service");
        let old = actions.find_one(doc! { "_id": "old" }).await.unwrap().unwrap();
        assert_eq!(old.get_str("facts").unwrap(), "Posted ads");
        assert_eq!(old.get_str("territorial_scope").unwrap(), "worldwide");
        assert!(!old.get_bool("automated_detection").unwrap());
        assert_eq!(old.get_document("ground").unwrap().get_str("type").unwrap(), "Terms");
        let new = actions.find_one(doc! { "_id": "new" }).await.unwrap().unwrap();
        assert_eq!(new.get_str("facts").unwrap(), "Kept as written");
        assert_eq!(new.get_str("territorial_scope").unwrap(), "Germany");
        assert!(new.get_bool("automated_detection").unwrap());
        assert_eq!(new.get_document("ground").unwrap().get_str("type").unwrap(), "Law");
        db.drop_database().await;
    }
}

#[cfg(test)]
mod official_index_tests {
    use crate::mongodb::bson::{doc, Document};
    use crate::{AbstractMigrations, Database, DatabaseInfo};
    use futures::TryStreamExt;

    #[tokio::test]
    async fn the_index_is_made_once_however_often_it_runs() {
        // not `database_test!`: it names the database after this long path,
        // and MongoDB refuses a name that long
        let db = DatabaseInfo::Test("official_index".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        let handle = mongo.db();
        // the collection exists on a real database
        handle
            .collection::<Document>("users")
            .insert_one(doc! { "_id": "u", "username": "u", "discriminator": "0001" })
            .await
            .unwrap();
        super::create_official_indexes(&handle).await;
        super::create_official_indexes(&handle).await;
        let indexes: Vec<_> = handle
            .collection::<Document>("users")
            .list_indexes()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let official: Vec<_> = indexes
            .iter()
            .filter(|index| index.options.as_ref().and_then(|o| o.name.as_deref()) == Some("official"))
            .collect();
        assert_eq!(official.len(), 1);
        assert_eq!(official[0].options.as_ref().and_then(|o| o.sparse), Some(true));
        db.drop_database().await;
    }
}

#[cfg(test)]
mod platform_word_seed_tests {
    use super::*;
    use crate::{Database, DatabaseInfo};

    #[tokio::test]
    async fn the_seed_never_undoes_what_staff_did() {
        let db = DatabaseInfo::Test("platform_word_seed_twice".to_string())
            .connect()
            .await
            .expect("database");
        let Database::MongoDb(mongo) = &db else { return };
        let database = mongo.db();
        for collection in ["platform_words", "seed_state"] {
            database.collection::<Document>(collection).drop().await.expect("clean start");
        }
        let words = database.collection::<Document>("platform_words");

        // staff wrote an entry of the file by hand before the seed ran
        words
            .insert_one(doc! { "_id": "by-staff", "pattern": "kys", "allow": false })
            .await
            .expect("insert");
        seed_platform_words(&database).await;
        let total = crate::platform_word_seed(0).expect("the starter file reads").len() as u64;
        assert_eq!(words.count_documents(doc! {}).await.expect("count"), total);

        // staff removed one and changed another
        words.delete_one(doc! { "pattern": "free nitro" }).await.expect("delete");
        words
            .update_one(doc! { "pattern": "retard" }, doc! { "$set": { "note": "by staff" } })
            .await
            .expect("update");
        seed_platform_words(&database).await;
        assert_eq!(words.count_documents(doc! {}).await.expect("count"), total - 1);
        assert_eq!(words.count_documents(doc! { "pattern": "free nitro" }).await.expect("count"), 0);
        assert_eq!(words.count_documents(doc! { "note": "by staff" }).await.expect("count"), 1);

        // the second version changes a database seeded with the first
        words
            .insert_many([
                doc! { "_id": "old-1", "pattern": "عاهرة", "allow": false, "added_by": "system" },
                doc! { "_id": "old-2", "pattern": "انتحر", "allow": false, "added_by": "system" },
                doc! { "_id": "staff-1", "pattern": "يا كلب", "allow": false, "added_by": "01STAFF" },
            ])
            .await
            .expect("insert");
        database
            .collection::<Document>("seed_state")
            .update_one(doc! { "_id": "platform_words" }, doc! { "$unset": { "version": "" } })
            .await
            .expect("first version");
        words.delete_one(doc! { "pattern": "steam gift" }).await.expect("delete");
        update_platform_word_seed(&database).await;
        for gone in ["عاهرة", "انتحر"] {
            assert_eq!(words.count_documents(doc! { "pattern": gone }).await.expect("count"), 0);
        }
        assert_eq!(words.count_documents(doc! { "pattern": "يا كلب" }).await.expect("count"), 1, "staff's own entry");
        assert_eq!(words.count_documents(doc! { "pattern": "steam gift" }).await.expect("count"), 1);
        assert_eq!(words.count_documents(doc! { "pattern": "free nitro" }).await.expect("count"), 0, "staff removed it");
        // run twice: what staff did since stays
        words.delete_one(doc! { "pattern": "steam gift" }).await.expect("delete");
        update_platform_word_seed(&database).await;
        assert_eq!(words.count_documents(doc! { "pattern": "steam gift" }).await.expect("count"), 0);

        // the mark stays out of the one document `migrate_database` reads
        assert_eq!(
            database.collection::<Document>("migrations").count_documents(doc! { "_id": "platform_words" }).await.expect("count"),
            0
        );
    }
}

#[cfg(test)]
mod server_event_index_tests {
    use crate::mongodb::bson::Document;
    use crate::{Database, DatabaseInfo};
    use futures::TryStreamExt;

    #[tokio::test]
    async fn event_indexes_are_made_once_however_often_it_runs() {
        let db = DatabaseInfo::Test("server_event_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        let handle = mongo.db();
        super::create_server_event_indexes(&handle).await;
        super::create_server_event_indexes(&handle).await;
        for (collection, names) in [
            ("server_events", vec!["server_status_start", "status_start"]),
            ("server_event_interests", vec!["event_user", "user"]),
        ] {
            let indexes: Vec<_> = handle
                .collection::<Document>(collection)
                .list_indexes()
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            for name in names {
                let count = indexes
                    .iter()
                    .filter(|index| index.options.as_ref().and_then(|o| o.name.as_deref()) == Some(name))
                    .count();
                assert_eq!(count, 1, "{collection}.{name}");
            }
        }
        db.drop_database().await;
    }
}

#[cfg(test)]
mod server_insight_index_tests {
    use crate::mongodb::bson::Document;
    use crate::{Database, DatabaseInfo};
    use futures::TryStreamExt;

    #[tokio::test]
    async fn insight_indexes_are_made_once_however_often_it_runs() {
        let db = DatabaseInfo::Test("server_insight_indexes".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        let handle = mongo.db();
        super::create_server_insight_indexes(&handle).await;
        super::create_server_insight_indexes(&handle).await;
        let indexes: Vec<_> = handle
            .collection::<Document>("server_insights")
            .list_indexes()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        for name in ["server_day", "day", "open_day"] {
            let count = indexes
                .iter()
                .filter(|index| index.options.as_ref().and_then(|o| o.name.as_deref()) == Some(name))
                .count();
            assert_eq!(count, 1, "server_insights.{name}");
        }
        db.drop_database().await;
    }
}

#[cfg(test)]
mod command_permission_index_tests {
    use crate::mongodb::bson::{doc, Document};
    use crate::{AbstractApplicationCommands, Database, DatabaseInfo};
    use futures::TryStreamExt;

    #[tokio::test]
    async fn the_rules_index_is_made_once_however_often_it_runs() {
        let db = DatabaseInfo::Test("command_rules_index".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        let handle = mongo.db();
        super::create_command_permission_indexes(&handle).await;
        super::create_command_permission_indexes(&handle).await;
        let indexes: Vec<_> = handle
            .collection::<Document>("command_permissions")
            .list_indexes()
            .await
            .unwrap()
            .try_collect()
            .await
            .unwrap();
        let ours: Vec<_> = indexes
            .iter()
            .filter(|index| {
                index.options.as_ref().and_then(|o| o.name.as_deref()) == Some("server_application")
            })
            .collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].keys, doc! { "server_id": 1_i32, "application_id": 1_i32 });
        db.drop_database().await;
    }

    /// A stored rule that does not read is an error, never skipped
    #[tokio::test]
    async fn a_rule_that_does_not_read_is_an_error() {
        let db = DatabaseInfo::Test("command_rules_decode".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        mongo
            .db()
            .collection::<Document>("command_permissions")
            .insert_one(doc! {
                "_id": "s:a:a",
                "server_id": "s",
                "application_id": "a",
                "permissions": [{ "id": "r", "type": "Nonsense", "permission": false }],
            })
            .await
            .unwrap();
        assert!(db.fetch_command_permissions("s", Some("a")).await.is_err());
        assert!(db.fetch_command_rules("s", "a", "c").await.is_err());
        db.drop_database().await;
    }
}

/// Marks a server whose default permissions got Use Soundboard (and names
/// the claim with the cutoff), so running this again never gives back a
/// right an owner took away since
const SOUNDBOARD_DONE: &str = "soundboard_default_done";

/// The soundboard index (sounds of a server, oldest first), and Use
/// Soundboard in the default permissions of every existing server, as new
/// servers get it. Safe to run twice.
pub async fn create_soundboard(db: &crate::mongodb::Database) {
    db.run_command(doc! {
        "createIndexes": "soundboard_sounds",
        "indexes": [{ "key": { "server": 1_i32, "_id": 1_i32 }, "name": "server_id" }]
    })
    .await
    .expect("Failed to create the soundboard index");

    // servers made after the first run already have it in their defaults:
    // the cutoff is saved once, so a second run never touches them (an owner
    // may have taken it away since)
    let claims = db.collection::<Document>("migration_claims");
    let _ = claims
        .insert_one(doc! { "_id": SOUNDBOARD_DONE, "before": Ulid::new().to_string() })
        .await;
    let before = claims
        .find_one(doc! { "_id": SOUNDBOARD_DONE })
        .await
        .expect("Failed to read the soundboard claim")
        .and_then(|claim| claim.get_str("before").ok().map(str::to_string))
        .expect("The soundboard claim was not saved");

    db.collection::<Document>("servers")
        .update_many(
            doc! { "_id": { "$lt": &before }, SOUNDBOARD_DONE: { "$ne": true } },
            doc! {
                "$bit": { "default_permissions": { "or": ChannelPermission::UseSoundboard as i64 } },
                "$set": { SOUNDBOARD_DONE: true },
            },
        )
        .await
        .expect("Failed to give servers the soundboard");
}

#[cfg(test)]
mod server_event_answer_migration_tests {
    use crate::mongodb::bson::{doc, to_document, Document};
    use crate::{Database, DatabaseInfo, ServerEvent};
    use futures::TryStreamExt;
    use iso8601_timestamp::Timestamp;
    use revolt_models::v0;

    /// An event as the release before answers stored it
    fn old_event(id: &str, interested: i64) -> Document {
        let mut event = ServerEvent::new(
            "SERVER",
            "CREATOR",
            "Old event".into(),
            None,
            Timestamp::now_utc(),
            None,
            v0::ServerEventPlace::External {
                location: "Park".into(),
            },
            None,
        );
        event.id = id.to_string();
        let mut document = to_document(&event).unwrap();
        for new in [
            "going_count",
            "not_going_count",
            "waitlist_count",
            "roster_version",
            "waitlist",
            "visibility",
            "attendee_list",
        ] {
            document.remove(new);
        }
        // a count that drifted from its rows
        document.insert("interested_count", interested);
        document
    }

    fn old_mark(event: &str, user: &str) -> Document {
        doc! {
            "_id": { "event": event, "user": user },
            "server": "SERVER",
            "created_at": crate::mongodb::bson::to_bson(&Timestamp::now_utc()).unwrap(),
        }
    }

    #[tokio::test]
    async fn old_marks_become_maybe_none_is_lost_and_twice_is_the_same() {
        let db = DatabaseInfo::Test("server_event_answers".to_string())
            .connect()
            .await
            .expect("Database connection failed.");
        let Database::MongoDb(mongo) = &db else { return };
        db.drop_database().await;
        let handle = mongo.db();
        let events = handle.collection::<Document>("server_events");
        let rows = handle.collection::<Document>("server_event_interests");
        // the server exists and three people are members of it
        handle
            .collection::<Document>("servers")
            .insert_one(doc! { "_id": "SERVER" })
            .await
            .unwrap();
        handle
            .collection::<Document>("server_members")
            .insert_many(["USER_1", "USER_2", "USER_3"].map(|user| {
                doc! { "_id": { "server": "SERVER", "user": user } }
            }))
            .await
            .unwrap();
        // left behind by earlier releases: an event of a deleted server
        // with a mark, and the mark of a deleted account
        let mut orphan = old_event("EVENT_GONE", 1);
        orphan.insert("server", "SERVER_GONE");
        let mut orphan_mark = old_mark("EVENT_GONE", "USER_1");
        orphan_mark.insert("server", "SERVER_GONE");
        events
            .insert_many([
                old_event("EVENT_A", 9),
                old_event("EVENT_B", 0),
                old_event("EVENT_C", 4),
                orphan,
            ])
            .await
            .unwrap();
        rows.insert_many([
            old_mark("EVENT_A", "USER_1"),
            old_mark("EVENT_A", "USER_2"),
            old_mark("EVENT_B", "USER_1"),
            old_mark("EVENT_A", "USER_DELETED"),
            orphan_mark,
        ])
        .await
        .unwrap();

        for run in 0..2 {
            let report = super::migrate_server_event_answers(&handle).await;
            // the first run says what it changed, the second changes nothing
            let expected = if run == 0 {
                super::ServerEventAnswerMigration {
                    marks_turned_into_maybe: 5,
                    events_recounted: 4,
                    events_kept: 4,
                    // the scheduled event of the deleted server is called
                    // off, not deleted
                    events_of_deleted_servers_called_off: 1,
                    count_passes: 1,
                    orphaned_events_found: 1,
                    orphaned_marks_found: 2,
                }
            } else {
                super::ServerEventAnswerMigration {
                    events_kept: 4,
                    count_passes: 1,
                    orphaned_events_found: 1,
                    orphaned_marks_found: 2,
                    ..Default::default()
                }
            };
            assert_eq!(report, expected, "run {run}");

            // nothing is deleted: the event of the deleted server and the
            // mark of the deleted account are still there
            let gone = db.fetch_server_event("EVENT_GONE").await.unwrap();
            assert_eq!(gone.interested_count, 1, "run {run}");
            // called off at the migration, so that its retention time runs
            assert_eq!(gone.status, v0::ServerEventStatus::Cancelled, "run {run}");
            assert!(gone.ended_at.is_some(), "run {run}");
            // an event of a server that exists is not called off
            assert_eq!(
                db.fetch_server_event("EVENT_A").await.unwrap().status,
                v0::ServerEventStatus::Scheduled,
                "run {run}"
            );
            assert_eq!(
                rows.count_documents(doc! { "_id.user": "USER_DELETED" }).await.unwrap(),
                1,
                "run {run}"
            );
            // every mark is still there, as "maybe"
            let all: Vec<Document> = rows.find(doc! {}).await.unwrap().try_collect().await.unwrap();
            assert_eq!(all.len(), 5, "run {run}");
            assert!(all.iter().all(|row| row.get_str("answer") == Ok("Maybe")), "run {run}");
            for (event, maybe) in [("EVENT_A", 3), ("EVENT_B", 1), ("EVENT_C", 0)] {
                let stored = db.fetch_server_event(event).await.unwrap();
                assert_eq!(stored.interested_count, maybe, "{event}, run {run}");
                assert_eq!(
                    (stored.going_count, stored.not_going_count, stored.waitlist_count),
                    (0, 0, 0),
                    "{event}, run {run}"
                );
                // what an old event means with the new settings: as before
                assert_eq!(stored.visibility, v0::ServerEventVisibility::Place);
                assert_eq!(stored.attendee_list, v0::ServerEventAttendeeList::Everyone);
                assert!(stored.waitlist);
                let shown: v0::ServerEvent = stored.into();
                assert_eq!(shown.interested_count, maybe, "{event}, run {run}");
                assert_eq!(shown.counts.maybe, maybe, "{event}, run {run}");
            }
            let marked = db.fetch_server_event_interested("EVENT_A", None, 10).await.unwrap();
            assert_eq!(
                marked,
                vec!["USER_1".to_string(), "USER_2".to_string(), "USER_DELETED".to_string()],
                "run {run}"
            );
        }

        // answers given after the migration survive another run
        let mut event = db.fetch_server_event("EVENT_A").await.unwrap();
        event
            .answer(&db, "USER_1", v0::ServerEventAnswer::Going)
            .await
            .unwrap();
        event
            .answer(&db, "USER_3", v0::ServerEventAnswer::NotGoing)
            .await
            .unwrap();
        super::migrate_server_event_answers(&handle).await;
        let stored = db.fetch_server_event("EVENT_A").await.unwrap();
        assert_eq!(
            (stored.going_count, stored.interested_count, stored.not_going_count),
            (1, 2, 1)
        );
        assert_eq!(rows.count_documents(doc! {}).await.unwrap(), 6);

        // the new indexes exist once
        for (collection, name) in [
            ("server_event_interests", "event_answer_user"),
            ("server_events", "server_start"),
            ("server_event_rosters", "server"),
            ("server_events", "invited"),
            ("server_events", "co_organisers"),
        ] {
            let indexes: Vec<_> = handle
                .collection::<Document>(collection)
                .list_indexes()
                .await
                .unwrap()
                .try_collect()
                .await
                .unwrap();
            let count = indexes
                .iter()
                .filter(|index| index.options.as_ref().and_then(|o| o.name.as_deref()) == Some(name))
                .count();
            assert_eq!(count, 1, "{collection}.{name}");
        }

        // what the migration checks itself with: the counts the rows give.
        // A row written behind its back changes that picture, which is how
        // it notices that someone else is still taking answers
        let before = super::server_event_counts_snapshot(&handle).await;
        assert_eq!(before.get("EVENT_A"), Some(&[1, 2, 1, 0]));
        rows.insert_one(old_mark("EVENT_B", "USER_2")).await.unwrap();
        let after = super::server_event_counts_snapshot(&handle).await;
        assert_ne!(after, before);
        assert_eq!(after.get("EVENT_B"), Some(&[0, 2, 0, 0]));
        rows.delete_one(doc! { "_id": { "event": "EVENT_B", "user": "USER_2" } })
            .await
            .unwrap();

        // the deliberate clean-up, when someone decides to run it: only the
        // leftovers go, nothing of a member of an existing server
        assert_eq!(super::remove_orphaned_server_event_data(&handle, false).await, (1, 2));
        assert_eq!(rows.count_documents(doc! {}).await.unwrap(), 6, "counting removes nothing");
        assert_eq!(super::remove_orphaned_server_event_data(&handle, true).await, (1, 2));
        assert!(db.fetch_server_event("EVENT_GONE").await.is_err());
        assert_eq!(rows.count_documents(doc! { "_id.user": "USER_DELETED" }).await.unwrap(), 0);
        assert_eq!(rows.count_documents(doc! {}).await.unwrap(), 4);
        assert_eq!(super::remove_orphaned_server_event_data(&handle, true).await, (0, 0));
        db.drop_database().await;
    }
}
